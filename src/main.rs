//! `llm-wiki` entry point: parse arguments, dispatch, report.
//!
//! No business logic lives here; each command handler ends in
//! `report::emit` or `report::fail`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::Serialize;

use llm_wiki::cli::{Cli, Command};
use llm_wiki::config::Config;
use llm_wiki::convert;
use llm_wiki::init;
use llm_wiki::parse::{self, Warning};
use llm_wiki::paths;
use llm_wiki::report::{self, Exit};
use llm_wiki::scan;
use llm_wiki::store::{self, Store};

/// Characters of parsed markdown echoed by `parse`.
const HEAD_CHARS: usize = 200;

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
        Command::Convert { key, limit } => run_convert(key, limit),
        Command::Parse { key } => run_parse(key),
    }
}

/// The repository root is the current directory, for every command.
///
/// Failing to read it is an environment error (exit 2), never a finding.
fn wiki_root(cmd: &str) -> PathBuf {
    match std::env::current_dir() {
        Ok(root) => root,
        Err(err) => report::fail(
            cmd,
            Exit::Usage,
            "E_IO",
            &format!("cannot determine the current directory: {err}"),
        ),
    }
}

/// Reads `wiki.toml` under `root`; a missing or invalid contract is a usage
/// error carrying the configuration's own code.
fn load_config(cmd: &str, root: &Path) -> Config {
    match Config::load(root) {
        Ok(cfg) => cfg,
        Err(err) => report::fail(cmd, Exit::Usage, err.code(), &err.to_string()),
    }
}

/// Opens and migrates the ledger.
///
/// `Store::open` does not migrate, so every command entry point must do it
/// explicitly before touching the schema.
fn open_store(cmd: &str, root: &Path, cfg: &Config) -> Store {
    let db = root.join(&cfg.paths.engine).join("state.db");
    let mut store = match Store::open(&db) {
        Ok(store) => store,
        Err(err) => report::fail(cmd, Exit::Usage, "E_DB", &err.to_string()),
    };
    if let Err(err) = store.migrate() {
        report::fail(cmd, Exit::Usage, "E_DB", &err.to_string());
    }
    store
}

/// `init` writes the contract files and creates the engine directories. A
/// failure to read the working directory, or to write the contract, is an
/// environment error (exit 2) rather than a finding.
fn run_init(force: bool) -> ! {
    let root = wiki_root("init");
    match init::run(&root, force) {
        Ok(outcome) => report::emit("init", outcome),
        Err(err) => report::fail("init", Exit::Usage, "E_IO", &err.to_string()),
    }
}

/// `scan` compares `source/` with the ledger and reports the classified changes.
fn run_scan() -> ! {
    let root = wiki_root("scan");
    let cfg = load_config("scan", &root);
    let store = open_store("scan", &root, &cfg);
    match scan::scan_source(&root, &cfg, &store) {
        Ok(outcome) => report::emit("scan", outcome),
        Err(err) => report::fail("scan", Exit::Usage, "E_IO", &err.to_string()),
    }
}

/// `convert` turns pending sources into the raw layer.
///
/// With `--key` the named source is converted whether or not the ledger has seen
/// it; without it, the first `--limit` sources lacking a `raw_key` are taken in
/// ledger order. A failed conversion is a finding (exit 1), not an environment
/// error, so that one bad source does not look like a broken invocation.
fn run_convert(key: Option<String>, limit: usize) -> ! {
    let root = wiki_root("convert");
    let cfg = load_config("convert", &root);
    let mut store = open_store("convert", &root, &cfg);

    let keys = match conversion_targets(&store, key.as_deref(), limit) {
        Ok(keys) => keys,
        Err(err) => report::fail("convert", Exit::Usage, "E_DB", &err.to_string()),
    };

    let mut converted = Vec::with_capacity(keys.len());
    for key in keys {
        match convert::convert_one(&root, &cfg, &mut store, &key) {
            Ok(outcome) => converted.push(outcome),
            Err(err) => report::fail("convert", Exit::Error, err.code(), &err.to_string()),
        }
    }
    report::emit("convert", ConvertData { converted });
}

/// The source keys `convert` should hand to [`convert::convert_one`].
fn conversion_targets(
    store: &Store,
    key: Option<&str>,
    limit: usize,
) -> store::Result<Vec<String>> {
    if let Some(key) = key {
        return Ok(vec![String::from(key)]);
    }
    let pending = store
        .all_sources()?
        .into_iter()
        .filter(|row| row.raw_key.is_none())
        .map(|row| row.key)
        .take(limit);
    Ok(pending.collect())
}

/// `parse` shows what one source parses to.
///
/// It is the safe counterpart of `convert`: the same backend and the same
/// warnings, but nothing is written and the ledger is never opened.
fn run_parse(key: String) -> ! {
    let root = wiki_root("parse");
    let cfg = load_config("parse", &root);
    let source_path = paths::join_key(&root.join(&cfg.paths.source), &key);
    let bytes = match std::fs::read(&source_path) {
        Ok(bytes) => bytes,
        Err(err) => report::fail(
            "parse",
            Exit::Usage,
            "E_IO",
            &format!("cannot read {}: {err}", source_path.display()),
        ),
    };

    let (extracted, backend) = match parse::parse_with(&cfg, &parse::Source::inline(&key, &bytes)) {
        Ok(result) => result,
        Err(err) => report::fail("parse", Exit::Usage, err.code(), &err.to_string()),
    };
    report::emit(
        "parse",
        ParseData {
            markdown_head: extracted.markdown.chars().take(HEAD_CHARS).collect(),
            chars: extracted.markdown.chars().count(),
            warnings: extracted.warnings,
            backend,
        },
    );
}

/// Payload of the `version` success envelope.
#[derive(Serialize)]
struct VersionData {
    version: &'static str,
}

/// Payload of the `convert` success envelope.
#[derive(Serialize)]
struct ConvertData {
    converted: Vec<convert::ConvertOutcome>,
}

/// Payload of the `parse` success envelope.
#[derive(Serialize)]
struct ParseData {
    backend: String,
    chars: usize,
    warnings: Vec<Warning>,
    markdown_head: String,
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
