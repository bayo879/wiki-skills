//! `llm-wiki` entry point: parse arguments, dispatch, report.
//!
//! No business logic lives here; each command handler ends in
//! `report::emit` or `report::fail`.

use std::ffi::OsStr;

use clap::Parser;
use serde::Serialize;

use llm_wiki::cli::{Cli, Command};
use llm_wiki::config::Config;
use llm_wiki::init;
use llm_wiki::report::{self, Exit};
use llm_wiki::scan;
use llm_wiki::store::Store;

fn main() {
    // Tell the reporter the format before parsing, so that usage errors are
    // still emitted as JSON when `--json` was asked for.
    report::init(flag_present("--json"));

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => exit_on_parse_error(err),
    };
    // Once the arguments parse, the parsed flag is authoritative.
    report::init(cli.json);
    dispatch(cli);
}

/// Whether any raw argument is exactly `flag`. `--json` may appear before or
/// after the subcommand, so this cannot depend on clap's parse result (which is
/// exactly what is unavailable when parsing fails).
fn flag_present(flag: &str) -> bool {
    std::env::args_os().any(|arg| arg.as_os_str() == OsStr::new(flag))
}

fn dispatch(cli: Cli) -> ! {
    match cli.command {
        Command::Version => report::emit(
            "version",
            VersionData {
                version: env!("CARGO_PKG_VERSION"),
            },
        ),
        Command::Init { force } => run_init(force),
        Command::Scan => run_scan(),
    }
}

/// `init` acts on the current directory, which is the repository root by
/// convention. A failure to read it, or to write the contract, is an
/// environment error (exit 2) rather than a finding.
fn run_init(force: bool) -> ! {
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(err) => report::fail(
            "init",
            Exit::Usage,
            "E_IO",
            &format!("cannot determine the current directory: {err}"),
        ),
    };
    match init::run(&root, force) {
        Ok(outcome) => report::emit("init", outcome),
        Err(err) => report::fail("init", Exit::Usage, "E_IO", &err.to_string()),
    }
}

/// `scan` reads `wiki.toml` and the ledger under the current directory, then
/// reports the classified changes. A missing contract, database, or source tree
/// is an environment error (exit 2), not a finding.
fn run_scan() -> ! {
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(err) => report::fail(
            "scan",
            Exit::Usage,
            "E_IO",
            &format!("cannot determine the current directory: {err}"),
        ),
    };
    let cfg = match Config::load(&root) {
        Ok(cfg) => cfg,
        Err(err) => report::fail("scan", Exit::Usage, err.code(), &err.to_string()),
    };
    // `Store::open` does not migrate, so every command entry point must do it
    // explicitly before touching the schema.
    let db = root.join(&cfg.paths.engine).join("state.db");
    let mut store = match Store::open(&db) {
        Ok(store) => store,
        Err(err) => report::fail("scan", Exit::Usage, "E_DB", &err.to_string()),
    };
    if let Err(err) = store.migrate() {
        report::fail("scan", Exit::Usage, "E_DB", &err.to_string());
    }
    match scan::scan_source(&root, &cfg, &store) {
        Ok(outcome) => report::emit("scan", outcome),
        Err(err) => report::fail("scan", Exit::Usage, "E_IO", &err.to_string()),
    }
}

/// Payload of the `version` success envelope.
#[derive(Serialize)]
struct VersionData {
    version: &'static str,
}

/// Maps a clap failure onto the CLI contract: help stays plain text on stdout
/// with exit 0, every real failure becomes `E_USAGE` with exit 2 on stdout.
fn exit_on_parse_error(err: clap::Error) -> ! {
    use clap::error::ErrorKind;

    match err.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            print!("{}", err.render());
            std::process::exit(Exit::Ok.code());
        }
        _ => {
            // clap prefixes its diagnostics with `error: `; the envelope already
            // carries the code, so keep only the content.
            let rendered = err.render().to_string();
            let message = rendered.trim_end();
            let message = message.strip_prefix("error: ").unwrap_or(message);
            report::fail(&command_hint(), Exit::Usage, "E_USAGE", message)
        }
    }
}

/// Best-effort name of the command the caller tried to run, used as `cmd` in a
/// usage-error envelope. Falls back to the binary name when there is none.
fn command_hint() -> String {
    std::env::args_os()
        .skip(1)
        .filter_map(|arg| arg.into_string().ok())
        .find(|arg| !arg.starts_with('-'))
        .unwrap_or_else(|| String::from("llm-wiki"))
}
