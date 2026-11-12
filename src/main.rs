//! `llm-wiki` entry point: parse arguments, dispatch, report.
//!
//! No business logic lives here; each command handler ends in
//! `report::emit` or `report::fail`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::Serialize;

use llm_wiki::chunk;
use llm_wiki::cli::{Cli, Command, DoctorOnly, QueryArgs, RelationFlag, TaskAction};
use llm_wiki::config::{Config, ConfigError, CONFIG_FILE};
use llm_wiki::doctor::{self, Check, Level};
use llm_wiki::graph;
use llm_wiki::hash;
use llm_wiki::ignore;
use llm_wiki::init;
use llm_wiki::lock;
use llm_wiki::parse::{self, Warning};
use llm_wiki::paths;
use llm_wiki::precondition;
use llm_wiki::query::{self, Scope as QueryScope};
use llm_wiki::read::{self, MAX_LINES};
use llm_wiki::report::{self, Exit};
use llm_wiki::store::{Store, StoreError};
use llm_wiki::sync;
use llm_wiki::task;

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
        Command::Doctor { fix, only, key } => run_doctor(fix, only, key),
        Command::Ingest {
            keys,
            auto,
            dry_run,
            key,
            chunk,
            range,
            force_large,
            force,
        } => run_ingest(IngestOptions {
            keys,
            auto,
            dry_run,
            key,
            chunk,
            range,
            force_large,
            force,
        }),
        Command::Read {
            reference,
            offset,
            limit,
        } => run_read(reference, offset, limit),
        Command::Task { action } => run_task(action),
        Command::Ignore { add, remove, check } => run_ignore(&add, &remove, check.as_deref()),
        Command::Query(args) => run_query(args),
    }
}

/// The repository root is the current directory, for every command.
///
/// Failing to read it is an environment error (exit 2), never a finding.
fn wiki_root(cmd: &str) -> PathBuf {
    match std::env::current_dir() {
        Ok(cwd) => {
            let executable = std::env::current_exe().unwrap_or_else(|err| {
                report::fail(
                    cmd,
                    Exit::Usage,
                    "E_IO",
                    &format!("cannot locate executable: {err}"),
                )
            });
            let root = llm_wiki::runtime_root::resolve_root(&executable, &cwd);
            // Relative source, leaves and payload paths share the same root.
            if let Err(err) = std::env::set_current_dir(&root) {
                report::fail(
                    cmd,
                    Exit::Usage,
                    "E_IO",
                    &format!("cannot enter distribution root: {err}"),
                );
            }
            root
        }
        Err(err) => report::fail(
            cmd,
            Exit::Usage,
            "E_IO",
            &format!("cannot determine the current directory: {err}"),
        ),
    }
}

fn fresh_install(cmd: &str, root: &Path) -> bool {
    precondition::is_fresh_install(root).unwrap_or_else(|err| {
        report::fail(
            cmd,
            Exit::Usage,
            "E_IO",
            &format!("cannot inspect engine: {err}"),
        )
    })
}

/// Opens the repository for a command: initialises a fresh directory, provisions
/// the derived directories, then loads the contract.
///
/// No `.wiki/`, or only installation resources, is a first run: the default
/// contract is created without printing anything and the command continues.
/// A damaged repository (`.wiki/` exists but a contract or the page tree is
/// missing) still refuses with `E_NOT_INITIALISED`, so `doctor` can diagnose it
/// instead of silently recreating the product.
fn open_repo(cmd: &str, root: &Path) -> Config {
    if let Err(missing) = precondition::check(root) {
        if !fresh_install(cmd, root) {
            report::fail_detail(
                cmd,
                Exit::Usage,
                precondition::CODE,
                &format!(
                    "the repository is not initialised: {} `{}` is missing",
                    missing.kind(),
                    missing.path()
                ),
                Some(serde_json::json!({
                    "missing": missing.path(),
                    "kind": missing.kind(),
                    "hint": precondition::HINT,
                })),
            );
        }
        if let Err(err) = init::run(root, false) {
            report::fail(
                cmd,
                Exit::Usage,
                "E_IO",
                &format!("cannot create the default engine: {err}"),
            );
        }
    }
    if let Err(err) = precondition::provision(root) {
        report::fail(
            cmd,
            Exit::Usage,
            "E_IO",
            &format!("cannot create the engine directories: {err}"),
        );
    }
    match Config::load(root) {
        Ok(cfg) => cfg,
        Err(err) => report::fail(cmd, Exit::Usage, err.code(), &config_error_message(&err)),
    }
}

/// The contract error without the host path.
///
/// `ConfigError`'s own `Display` names the absolute config path; the stable
/// reason is enough to act on.
fn config_error_message(err: &ConfigError) -> String {
    match err {
        ConfigError::NoConfig { .. } => {
            format!("no {CONFIG_FILE} in the repository; run `llm-wiki doctor --fix` to create it")
        }
        ConfigError::Io { source, .. } => format!("cannot read {CONFIG_FILE}: {source}"),
        ConfigError::Bad { message } | ConfigError::TokenInConfig { message } => message.clone(),
    }
}

/// Drops the host path from a message that may have come from an internal
/// error type. Command output names repository-relative files only.
fn strip_root(root: &Path, message: &str) -> String {
    message.replace(&root.display().to_string(), ".")
}

/// Opens and migrates the ledger.
///
/// `Store::open` does not migrate, so every command entry point must do it
/// explicitly before touching the schema.
fn open_store(cmd: &str, root: &Path, cfg: &Config) -> Store {
    let db = root.join(&cfg.paths.engine).join("state.db");
    let mut store = match Store::open(&db) {
        Ok(store) => store,
        Err(err) => report::fail(cmd, Exit::Usage, "E_DB", &store_error_message(&err)),
    };
    if let Err(err) = store.migrate() {
        report::fail(cmd, Exit::Usage, "E_DB", &err.to_string());
    }
    store
}

/// The ledger error without the host path.
///
/// `StoreError`'s own `Display` names the absolute database file, which the
/// command output must not expose; the stable reason is enough to act on.
fn store_error_message(err: &StoreError) -> String {
    match err {
        StoreError::Sqlite(err) => format!("sqlite: {err}"),
        StoreError::Io { source, .. } => format!("cannot prepare the ledger directory: {source}"),
        StoreError::WalUnavailable { mode, .. } => {
            format!("cannot enable WAL on the ledger: sqlite kept journal_mode={mode}")
        }
        StoreError::SchemaTooNew {
            found, supported, ..
        } => format!(
            "the ledger was written by a newer build (schema {found}, this build reads up to {supported})"
        ),
    }
}

/// `init` writes the contract files and creates the engine directories. A
/// failure to read the working directory, or to write the contract, is an
/// environment error (exit 2) rather than a finding.
///
/// `doctor` is also safe as the first command: a directory with no `.wiki/`
/// gets the default engine before the checks run, so a new repository reports a
/// healthy result instead of asking for a separate initialisation step.
fn run_doctor(fix: bool, only: Option<DoctorOnly>, key: Option<String>) -> ! {
    let root = wiki_root("doctor");
    if fresh_install("doctor", &root) {
        if let Err(err) = init::run(&root, false) {
            report::fail(
                "doctor",
                Exit::Usage,
                "E_IO",
                &format!("cannot create the default engine: {err}"),
            );
        }
    }
    let cfg = Config::load(&root).ok();
    let opts = doctor::DoctorOptions { fix, only, key };
    let checks = doctor::run(&root, cfg.as_ref(), &opts);
    let failed = checks.iter().any(|check| check.level == Level::Fail);
    let exit = if failed { Exit::Error } else { Exit::Ok };
    report::emit_with("doctor", DoctorData { checks }, exit, human_doctor)
}

/// Human rendering of the checks: the result when everything passed, otherwise
/// only the checks that need a decision.
///
/// The per-check table is still in `--json`; a healthy repository should not
/// spend a screen explaining that nothing is wrong.
fn human_doctor(data: &DoctorData) -> String {
    let issues: Vec<&Check> = data
        .checks
        .iter()
        .filter(|check| check.level != Level::Ok)
        .collect();
    if issues.is_empty() {
        return if data.checks.iter().any(|check| check.name == "init") {
            String::from("初始化完成")
        } else {
            String::from("健康检查通过")
        };
    }
    issues
        .iter()
        .map(|check| {
            format!(
                "{:<4}  {:<16}  {}",
                check.level.label(),
                check.name,
                check.detail
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The source keys `convert` should hand to [`convert::convert_one`].
///
/// `--key` names one source directly, so it works before the first scan. Without
/// it the pending set comes from a scan: the ledger alone cannot answer the
/// question, because a new source only gains a row once something converts it.
/// `ingest --dry-run --key` shows what one source parses to.
///
/// It is the safe counterpart of `convert`: the same backend and the same
/// warnings, but nothing is written except the chunk cache of a large source.
/// Sources at or below `parse.chunk_threshold` keep the whole-document shape; a
/// larger source is materialized as chunk files under `.wiki/cache/<hash>/` and
/// reported by `chunks_dir`, so later reads can pick one chunk (`--chunk`) or a
/// window of chunks (`--range`) instead of loading the whole text again.
fn parse_preview(
    cmd: &str,
    root: &Path,
    cfg: &Config,
    key: String,
    chunk_wanted: Option<usize>,
    range: Option<String>,
    force_large: bool,
) -> ! {
    let source_path = paths::join_key(&root.join(&cfg.paths.source), &key);
    let input = match hash::read_source(&source_path, cfg.parse.max_source_bytes) {
        Ok(input) => input,
        Err(err) => {
            let code = match &err {
                hash::HashError::Oversize { .. } => "E_OVERSIZE",
                hash::HashError::Unstable { .. } => "E_SOURCE_DRIFT",
                hash::HashError::Io { .. } | hash::HashError::NotRegular { .. } => "E_IO",
            };
            let exit = if matches!(err, hash::HashError::Oversize { .. }) {
                Exit::Usage
            } else {
                Exit::Error
            };
            report::fail(cmd, exit, code, &err.to_string())
        }
    };
    let bytes = input.bytes;
    // Kept for argument compatibility, never used to exceed the input budget.
    let _ = force_large;
    let delivery = match parse::delivery_with(cfg, bytes.len() as u64, false) {
        Ok(delivery) => delivery,
        Err(err) => report::fail(cmd, Exit::Usage, err.code(), &err.to_string()),
    };

    let (extracted, backend) = match parse::parse_with(cfg, &parse::Source::inline(&key, &bytes)) {
        Ok(result) => result,
        Err(err) => report::fail(cmd, Exit::Usage, err.code(), &err.to_string()),
    };

    if delivery == parse::Delivery::Whole && chunk_wanted.is_none() && range.is_none() {
        report::emit(
            cmd,
            ParseData::Full {
                backend,
                chars: extracted.markdown.chars().count(),
                warnings: extracted.warnings,
                markdown_head: extracted.markdown.chars().take(HEAD_CHARS).collect(),
            },
        );
    }

    let target = match delivery {
        parse::Delivery::Chunked { target } => target,
        parse::Delivery::Whole => cfg.parse.chunk_target as usize,
    };
    let digest = hash::hash_bytes(extracted.markdown.as_bytes());
    let hash = digest.strip_prefix("blake3:").unwrap_or(&digest);
    let cache_root = root.join(&cfg.paths.engine).join("cache");
    let chunks = match chunk::materialize(&cache_root, hash, &extracted.markdown, target) {
        Ok(chunks) => chunks,
        Err(err) => report::fail(cmd, Exit::Usage, err.code(), &err.to_string()),
    };
    let count = chunks.len();
    let chunks_rel = paths::to_slash(&Path::new(&cfg.paths.engine).join("cache").join(hash));

    if let Some(number) = chunk_wanted {
        let text = match chunk::read_range(&cache_root, hash, number, number) {
            Ok(text) => text,
            Err(err) => report::fail(cmd, Exit::Usage, err.code(), &err.to_string()),
        };
        report::emit(
            cmd,
            ParseData::Chunk {
                backend,
                chars: text.chars().count(),
                warnings: extracted.warnings,
                chunks: count,
                chunk: number,
                markdown: text,
            },
        );
    }

    if let Some(spec) = range {
        let (from, to) = match parse_chunk_range(&spec) {
            Ok(range) => range,
            Err(message) => report::fail(cmd, Exit::Usage, "E_BAD_ARGS", &message),
        };
        let text = match chunk::read_range(&cache_root, hash, from, to) {
            Ok(text) => text,
            Err(err) => report::fail(cmd, Exit::Usage, err.code(), &err.to_string()),
        };
        report::emit(
            cmd,
            ParseData::Range {
                backend,
                chars: text.chars().count(),
                warnings: extracted.warnings,
                chunks: count,
                from,
                to,
                markdown: text,
            },
        );
    }

    report::emit(
        cmd,
        ParseData::Chunked {
            backend,
            chars: extracted.markdown.chars().count(),
            warnings: extracted.warnings,
            chunks: count,
            chunks_dir: chunks_rel,
        },
    );
}

/// Parses a `A-B` chunk range into 1-based inclusive bounds.
fn parse_chunk_range(spec: &str) -> std::result::Result<(usize, usize), String> {
    let (from, to) = spec
        .split_once('-')
        .ok_or_else(|| format!("chunk range {spec:?} must be written as A-B"))?;
    let from: usize = from
        .parse()
        .map_err(|_| format!("chunk range {spec:?} must start with a number"))?;
    let to: usize = to
        .parse()
        .map_err(|_| format!("chunk range {spec:?} must end with a number"))?;
    if from == 0 || to < from {
        return Err(format!("chunk range {spec:?} must be 1-based with A <= B"));
    }
    Ok((from, to))
}

/// Convert sources and hand unfinished work to the author of live pages.
struct IngestOptions {
    keys: Vec<String>,
    auto: bool,
    dry_run: bool,
    key: Option<String>,
    chunk: Option<usize>,
    range: Option<String>,
    force_large: bool,
    force: bool,
}

fn run_ingest(options: IngestOptions) -> ! {
    let IngestOptions {
        keys,
        auto,
        dry_run,
        key,
        chunk,
        range,
        force_large,
        force,
    } = options;
    let cmd = "ingest";
    if auto
        && (dry_run
            || !keys.is_empty()
            || key.is_some()
            || chunk.is_some()
            || range.is_some()
            || force_large
            || force)
    {
        report::fail(
            cmd,
            Exit::Usage,
            "E_USAGE",
            "--auto runs one default round and cannot be combined with manual ingest options",
        );
    }
    let root = wiki_root(cmd);
    let cfg = open_repo(cmd, &root);
    // `--dry-run --key` is the parse preview that used to be its own command:
    // same inputs, same output shape, one fewer verb.
    if let Some(key) = key {
        parse_preview(cmd, &root, &cfg, key, chunk, range, force_large);
    }
    let mut store = open_store(cmd, &root, &cfg);
    // A named key limits the round to that source; no key means every new or
    // changed source. Whether a source is ready is decided by the file itself:
    // it must have been quiet for `[ingest].settle_seconds`. `--force` is the
    // explicit override for the moment a human knows better.
    let args = sync::SyncArgs {
        limit: cfg.ingest.max_per_run,
        dry_run,
        force,
        only: keys,
    };
    match sync::run(&root, &cfg, &mut store, &args) {
        Ok(report) => {
            let task_view = match report
                .next_task
                .as_ref()
                .map(|row| task::view(&root, &cfg, &store, row))
                .transpose()
            {
                Ok(view) => view,
                Err(err) => report::fail(cmd, Exit::Error, err.code(), &err.to_string()),
            };
            let counts = remaining(&store, report.waiting.len());
            let exit = if report.failures > 0 {
                Exit::Error
            } else {
                Exit::Ok
            };
            let data = IngestData::from_report(report, &cfg, counts, task_view);
            report::emit_with(cmd, data, exit, human_ingest)
        }
        Err(err) => {
            let message = match &err {
                sync::SyncError::Lock(lock_err) => lock_err.hint().to_string(),
                _ => strip_root(&root, &err.to_string()),
            };
            let exit = if err.code() == "E_LOCKED" {
                Exit::Busy
            } else {
                Exit::Usage
            };
            report::fail(cmd, exit, err.code(), &message)
        }
    }
}

/// What one operation actually changed.
#[derive(Default, Serialize)]
struct Effects {
    merged_pages: Vec<String>,
    confirmed_sources: Vec<String>,
    removed_pages: Vec<String>,
}

/// Work the tool still knows about, counted rather than dumped.
#[derive(Serialize)]
struct Remaining {
    actionable: usize,
    waiting: usize,
    failed: usize,
    blocked: usize,
}

fn remaining(store: &Store, waiting: usize) -> Remaining {
    let mut out = Remaining {
        actionable: 0,
        waiting,
        failed: 0,
        blocked: 0,
    };
    for task in store.tasks().unwrap_or_default() {
        match task.status.as_str() {
            "pending" | "active"
                if matches!(task.kind.as_str(), "page" | "route" | "reconcile") =>
            {
                out.actionable += 1
            }
            "failed" => out.failed += 1,
            "blocked" => out.blocked += 1,
            _ => {}
        }
    }
    out
}

/// The single completion rule shared by ingest and task done.
fn outcome(
    has_task: bool,
    effects: &Effects,
    remaining: &Remaining,
    issues: &[String],
) -> &'static str {
    if !effects.merged_pages.is_empty()
        && (remaining.failed > 0 || remaining.blocked > 0 || !issues.is_empty())
    {
        "partial"
    } else if remaining.failed > 0 || remaining.blocked > 0 || !issues.is_empty() {
        "needs_attention"
    } else if has_task {
        "in_progress"
    } else if remaining.waiting > 0 {
        "waiting"
    } else {
        "complete"
    }
}

/// Source conversion results and the direct-page authoring contract.
#[derive(Serialize)]
struct IngestData {
    run_id: String,
    outcome: String,
    changes: Vec<llm_wiki::scan::Change>,
    converted: usize,
    ingested: Vec<String>,
    queued: usize,
    waiting: Vec<String>,
    events: Vec<String>,
    warnings: Vec<String>,
    failures: usize,
    effects: Effects,
    remaining: Remaining,
    issues: Vec<String>,
    next_task: Option<llm_wiki::task::TaskView>,
    write_to: String,
    types: Vec<String>,
    schema: String,
}

impl IngestData {
    fn from_report(
        report: sync::SyncReport,
        cfg: &Config,
        remaining: Remaining,
        task: Option<llm_wiki::task::TaskView>,
    ) -> Self {
        let effects = Effects {
            merged_pages: report.merged_pages,
            confirmed_sources: report.confirmed_sources,
            removed_pages: report.removed_pages,
        };
        let outcome = outcome(task.is_some(), &effects, &remaining, &report.issues).to_string();
        Self {
            run_id: report.run_id,
            outcome,
            changes: report.changes,
            converted: report.converted,
            ingested: report.ingested,
            queued: report.queued,
            waiting: report.waiting,
            events: report.events,
            warnings: report.warnings,
            failures: report.failures,
            effects,
            remaining,
            issues: report.issues,
            next_task: task,
            write_to: cfg.paths.wiki.clone(),
            types: cfg.schema.types.clone(),
            schema: format!("{}/SCHEMA.md", cfg.paths.engine.trim_end_matches('/')),
        }
    }
}

fn human_ingest(data: &IngestData) -> String {
    let mut out = format!(
        "run_id: {}\noutcome: {}\nconverted: {}\nqueued: {}\nactionable: {}\nwaiting: {}\nfailed: {}\nblocked: {}\nwrite_to: {}\nschema: {}",
        data.run_id,
        data.outcome,
        data.converted,
        data.queued,
        data.remaining.actionable,
        data.remaining.waiting,
        data.remaining.failed,
        data.remaining.blocked,
        data.write_to,
        data.schema
    );
    out.push_str(&human_effects(&data.effects));
    out.push_str(&human_task_view(data.next_task.as_ref()));
    for key in &data.waiting {
        out.push_str(&format!("\nwaiting: {key}"));
    }
    for event in &data.events {
        out.push_str(&format!("\nevent: {event}"));
    }
    for warning in &data.warnings {
        out.push_str(&format!("\nwarning: {warning}"));
    }
    for issue in &data.issues {
        out.push_str(&format!("\nissue: {issue}"));
    }
    out
}

fn human_effects(effects: &Effects) -> String {
    let mut out = String::new();
    if !effects.merged_pages.is_empty() {
        out.push_str(&format!(
            "\nmerged_pages: {}",
            effects.merged_pages.join(", ")
        ));
    }
    if !effects.confirmed_sources.is_empty() {
        out.push_str(&format!(
            "\nconfirmed_sources: {}",
            effects.confirmed_sources.join(", ")
        ));
    }
    if !effects.removed_pages.is_empty() {
        out.push_str(&format!(
            "\nremoved_pages: {}",
            effects.removed_pages.join(", ")
        ));
    }
    out
}

fn human_task_view(task: Option<&llm_wiki::task::TaskView>) -> String {
    let Some(task) = task else {
        return String::from("\ntask: none");
    };
    let mut out = format!("\ntask: {} {} ({})", task.kind, task.id, task.status);
    out.push_str(&human_task_details(task));
    out
}

/// Everything the view explains besides its identity, shared by `ingest`,
/// `task done` and the current task in `task status`.
fn human_task_details(task: &llm_wiki::task::TaskView) -> String {
    let mut out = String::new();
    if let Some(stage) = &task.stage {
        out.push_str(&format!("\nstage: {stage}"));
    }
    if let Some(source) = &task.source {
        out.push_str(&format!(
            "\nsource: {} read {}",
            source.key, source.read_ref
        ));
    }
    if let Some(target) = &task.target {
        out.push_str(&format!(
            "\ntarget: {} (exists: {})",
            target.reference, target.exists
        ));
        if let Some(title) = &target.title {
            out.push_str(&format!("\ntarget_title: {title}"));
        }
    }
    for page in &task.affected_pages {
        out.push_str(&format!("\naffected_page: {page}"));
    }
    out.push_str(&format!("\nguide: {}", task.guide.goal));
    for forbidden in &task.guide.forbidden {
        out.push_str(&format!("\nforbidden: {forbidden}"));
    }
    out.push_str(&format!(
        "\ncontract: {} fields [{}]",
        task.contract.fields.len(),
        task.contract.schema
    ));
    if let Some(example) = task.contract.examples.first() {
        out.push_str(&format!("\nexample: {}", example.title));
    }
    out.push_str(&format!(
        "\nsubmit: {} [{}]",
        task.submit.command, task.submit.schema
    ));
    out
}

/// Prints one wiki file, windowed so a large page cannot flood the caller.
///
/// `--offset` is 1-based; `--limit` is capped at [`MAX_LINES`] rather than
/// rejected, so a caller asking for "the whole file" still gets a bounded read.
fn run_read(reference: String, offset: usize, limit: usize) -> ! {
    let cmd = "read";
    let root = wiki_root(cmd);
    let cfg = open_repo(cmd, &root);
    if offset == 0 || limit == 0 {
        report::fail(
            cmd,
            Exit::Usage,
            "E_USAGE",
            "--offset and --limit must be at least 1",
        );
    }
    let limit = limit.min(MAX_LINES);
    let text = match read::read_text(&root, &cfg, &reference) {
        Ok(text) => text,
        Err(err) => {
            let exit = if matches!(err, read::ReadError::BadRef(_)) {
                Exit::Usage
            } else {
                Exit::Error
            };
            report::fail(cmd, exit, err.code(), &err.to_string())
        }
    };
    let page = read::page(&text, offset, limit);
    let data = ReadData {
        reference,
        content: page.content,
        line_start: page.line_start,
        line_end: page.line_end,
        total_lines: page.total_lines,
        truncated: page.truncated,
    };
    report::emit_with(cmd, data, Exit::Ok, human_read)
}

#[derive(Serialize)]
struct ReadData {
    reference: String,
    content: String,
    line_start: usize,
    line_end: usize,
    total_lines: usize,
    truncated: bool,
}

fn human_read(data: &ReadData) -> String {
    let mut out = format!(
        "reference: {}\nlines: {}-{} of {}",
        data.reference, data.line_start, data.line_end, data.total_lines
    );
    if data.truncated {
        out.push_str(&format!(" (next --offset {})", data.line_end + 1));
    }
    out.push('\n');
    out.push_str(&data.content);
    out
}

fn run_ignore(add: &[String], remove: &[String], check: Option<&str>) -> ! {
    let cmd = "ignore";
    let root = wiki_root(cmd);
    open_repo(cmd, &root);
    if let Some(key) = check {
        let list = match ignore::IgnoreList::load(&root) {
            Ok(list) => list,
            Err(err) => fail_ignore(cmd, &err),
        };
        let key = key.trim_start_matches("./");
        let data = IgnoreCheckData {
            key: key.to_string(),
            ignored: list.is_ignored(key, false),
            pattern: list.deciding_pattern(key, false),
        };
        report::emit_with(cmd, data, Exit::Ok, human_ignore_check);
    }
    if !add.is_empty() || !remove.is_empty() {
        let (added, removed) = if !add.is_empty() {
            match ignore::add(&root, add) {
                Ok(added) => (added, Vec::new()),
                Err(err) => fail_ignore(cmd, &err),
            }
        } else {
            match ignore::remove(&root, remove) {
                Ok(removed) => (Vec::new(), removed),
                Err(err) => fail_ignore(cmd, &err),
            }
        };
        let patterns = match ignore::IgnoreList::load(&root) {
            Ok(list) => list.patterns(),
            Err(err) => fail_ignore(cmd, &err),
        };
        let data = IgnoreChangeData {
            added,
            removed,
            count: patterns.len(),
            patterns,
        };
        report::emit_with(cmd, data, Exit::Ok, human_ignore_change);
    }
    let patterns = match ignore::IgnoreList::load(&root) {
        Ok(list) => list.patterns(),
        Err(err) => fail_ignore(cmd, &err),
    };
    let data = IgnoreListData {
        count: patterns.len(),
        patterns,
    };
    report::emit_with(cmd, data, Exit::Ok, human_ignore_list)
}

fn fail_ignore(cmd: &str, err: &ignore::IgnoreError) -> ! {
    report::fail(cmd, Exit::Error, err.code(), &err.to_string())
}

/// Completion checks and clears work while holding the same lock as ingest.
fn run_task(action: TaskAction) -> ! {
    let cmd = "task";
    let root = wiki_root(cmd);
    let cfg = open_repo(cmd, &root);
    let mut store = open_store(cmd, &root, &cfg);
    match action {
        TaskAction::Done {
            id,
            payload,
            leaves,
            accept,
            regenerate,
        } => {
            let guard = match lock::acquire(&root) {
                Ok(guard) => guard,
                Err(err) => report::fail(
                    cmd,
                    if err.code() == "E_LOCKED" {
                        Exit::Busy
                    } else {
                        Exit::Usage
                    },
                    err.code(),
                    &err.hint(),
                ),
            };
            let verdict = match (accept, regenerate) {
                (true, false) => Some(task::Verdict::Accept),
                (false, true) => Some(task::Verdict::Regenerate),
                _ => None,
            };
            let result = task::close(
                &root,
                &cfg,
                &mut store,
                id,
                payload.as_deref(),
                leaves.as_deref(),
                verdict,
            );
            drop(guard);
            match result {
                Ok(settled) => {
                    let task_view = match settled
                        .task
                        .as_ref()
                        .map(|row| task::view(&root, &cfg, &store, row))
                        .transpose()
                    {
                        Ok(view) => view,
                        Err(err) => report::fail(cmd, Exit::Error, err.code(), &err.to_string()),
                    };
                    let effects = Effects {
                        merged_pages: settled.merged_pages,
                        confirmed_sources: settled.confirmed_sources,
                        removed_pages: Vec::new(),
                    };
                    let counts = remaining(&store, 0);
                    let outcome = outcome(task_view.is_some(), &effects, &counts, &settled.issues)
                        .to_string();
                    report::emit_with(
                        cmd,
                        TaskDoneData {
                            closed: id,
                            outcome,
                            effects,
                            remaining: counts,
                            issues: settled.issues,
                            next_task: task_view,
                        },
                        Exit::Ok,
                        human_task_done,
                    )
                }
                Err(err) => report::fail(cmd, Exit::Error, err.code(), &err.to_string()),
            }
        }
        TaskAction::Status => match task::status(&root, &cfg, &mut store) {
            Ok(status) => report::emit_with(cmd, status, Exit::Ok, human_task_status),
            Err(err) => report::fail(cmd, Exit::Error, err.code(), &err.to_string()),
        },
        TaskAction::Retry { id, replan } => {
            if replan {
                let guard = match lock::acquire(&root) {
                    Ok(guard) => guard,
                    Err(err) => report::fail(
                        cmd,
                        if err.code() == "E_LOCKED" {
                            Exit::Busy
                        } else {
                            Exit::Usage
                        },
                        err.code(),
                        &err.hint(),
                    ),
                };
                let result = task::replan(&root, &cfg, &mut store, id);
                drop(guard);
                match result {
                    Ok(()) => match store.task(id) {
                        Ok(Some(task)) => report::emit_with(cmd, task, Exit::Ok, human_task_replan),
                        Ok(None) => report::fail(
                            cmd,
                            Exit::Error,
                            "E_NO_TASK",
                            &format!("no open task with id {id}"),
                        ),
                        Err(err) => report::fail(cmd, Exit::Error, "E_DB", &err.to_string()),
                    },
                    Err(err) => report::fail(cmd, Exit::Error, err.code(), &err.to_string()),
                }
            } else {
                match task::retry(&mut store, id) {
                    Ok(task) => report::emit_with(cmd, task, Exit::Ok, human_task_retry),
                    Err(err) => report::fail(cmd, Exit::Error, err.code(), &err.to_string()),
                }
            }
        }
    }
}

#[derive(Serialize)]
struct TaskDoneData {
    closed: i64,
    outcome: String,
    effects: Effects,
    remaining: Remaining,
    issues: Vec<String>,
    next_task: Option<task::TaskView>,
}

fn human_task_done(data: &TaskDoneData) -> String {
    let mut out = format!(
        "closed: {}\noutcome: {}\nactionable: {}\nwaiting: {}\nfailed: {}\nblocked: {}",
        data.closed,
        data.outcome,
        data.remaining.actionable,
        data.remaining.waiting,
        data.remaining.failed,
        data.remaining.blocked
    );
    out.push_str(&human_effects(&data.effects));
    out.push_str(&human_task_view(data.next_task.as_ref()));
    for issue in &data.issues {
        out.push_str(&format!("\nissue: {issue}"));
    }
    out
}

fn human_task_status(status: &task::TaskStatus) -> String {
    let mut out = String::new();
    for root in &status.roots {
        out.push_str(&format!(
            "\nroot {} {} [{}{}] stage {} open {}",
            root.id,
            root.source_key,
            root.status,
            if root.attempts > 0 {
                format!(", attempts {}", root.attempts)
            } else {
                String::new()
            },
            root.stage.as_deref().unwrap_or("-"),
            root.open_children
        ));
        for leaf in &root.leaves {
            out.push_str(&format!(
                "\n  leaf {} {} ({})",
                leaf.id,
                leaf.target_path.as_deref().unwrap_or("-"),
                leaf.status
            ));
        }
    }
    match &status.current {
        Some(task) => {
            out.push_str(&format!(
                "\ncurrent: {} {} ({})",
                task.kind, task.id, task.status
            ));
            out.push_str(&human_task_details(task));
        }
        None => out.push_str("\ncurrent: none"),
    }
    for task in &status.failed {
        out.push_str(&format!(
            "\nfailed: {} {} {}",
            task.kind,
            task.id,
            task.last_error.as_deref().unwrap_or("")
        ));
    }
    for task in &status.blocked {
        out.push_str(&format!(
            "\nblocked: {} {} {}",
            task.kind,
            task.id,
            task.last_error.as_deref().unwrap_or("")
        ));
    }
    out
}

fn human_task_retry(task: &llm_wiki::store::TaskRow) -> String {
    format!("retried: {} {} ({})", task.kind, task.id, task.status)
}

fn human_task_replan(task: &llm_wiki::store::TaskRow) -> String {
    format!("replanned: {} {} ({})", task.kind, task.id, task.status)
}

/// `query` is the only command that decides when to pay for an index update: a
/// plain search catches up incrementally. A full rebuild is `doctor --fix`,
/// which nobody asks for by accident.
fn run_query(args: QueryArgs) -> ! {
    let cmd = "query";
    let root = wiki_root(cmd);
    let cfg = open_repo(cmd, &root);

    if args.relation.is_some() && args.page.is_none() && args.source.is_none() {
        report::fail(
            cmd,
            Exit::Usage,
            "E_USAGE",
            "--relation is only valid with --page or --source",
        )
    }

    if args.graph {
        let mut store = open_store(cmd, &root, &cfg);
        if let Err(err) = query::refresh(&root, &cfg, &mut store) {
            fail_query(cmd, &err)
        }
        match graph::build(&store) {
            Ok(data) => report::emit_with(cmd, data, Exit::Ok, human_graph),
            Err(err) => report::fail(cmd, Exit::Usage, err.code(), &err.to_string()),
        }
    }

    if let Some(reference) = args.page.as_deref() {
        let kind = match args.relation {
            Some(RelationFlag::Outgoing) => query::RelationKind::Outgoing,
            Some(RelationFlag::Incoming) => query::RelationKind::Incoming,
            Some(RelationFlag::Sources) => query::RelationKind::Sources,
            None => query::RelationKind::All,
            Some(RelationFlag::Citing) => report::fail(
                cmd,
                Exit::Usage,
                "E_USAGE",
                "--relation citing is only valid with --source",
            ),
        };
        let mut store = open_store(cmd, &root, &cfg);
        if let Err(err) = query::refresh(&root, &cfg, &mut store) {
            fail_query(cmd, &err)
        }
        match query::exact_page(
            &root,
            &cfg,
            &store,
            reference,
            kind,
            args.offset,
            args.limit.unwrap_or(0),
        ) {
            Ok(item) => report::emit_with(cmd, item, Exit::Ok, human_recall),
            Err(err) => fail_query(cmd, &err),
        }
    }

    if let Some(reference) = args.source.as_deref() {
        match args.relation {
            Some(RelationFlag::Citing) | None => {}
            _ => report::fail(
                cmd,
                Exit::Usage,
                "E_USAGE",
                "--relation with --source only accepts citing",
            ),
        }
        let mut store = open_store(cmd, &root, &cfg);
        if let Err(err) = query::refresh(&root, &cfg, &mut store) {
            fail_query(cmd, &err)
        }
        match query::exact_source(
            &root,
            &cfg,
            &store,
            reference,
            args.offset,
            args.limit.unwrap_or(0),
        ) {
            Ok(view) => report::emit_with(cmd, view, Exit::Ok, human_source),
            Err(err) => fail_query(cmd, &err),
        }
    }

    let mut store = open_store(cmd, &root, &cfg);
    let text = args.text.unwrap_or_default();
    if text.trim().is_empty() {
        report::fail(
            cmd,
            Exit::Usage,
            "E_EMPTY_QUERY",
            "no query text; pass a word to search for, --page, or --graph",
        )
    }
    if let Err(err) = query::refresh(&root, &cfg, &mut store) {
        fail_query(cmd, &err)
    }

    let search = query::SearchQuery {
        text,
        limit: args.limit.unwrap_or(10),
        expand: 0,
        scope: if args.no_archive {
            QueryScope::ExcludingSessions
        } else {
            QueryScope::All
        },
    };
    match query::search(&root, &cfg, &store, &search) {
        Ok(hits) => {
            let data = QueryHitsData {
                count: hits.len(),
                hits,
            };
            report::emit_with(cmd, data, Exit::Ok, human_query)
        }
        Err(err) => fail_query(cmd, &err),
    }
}

/// One block per requested relation group, then the sources.
fn human_recall(item: &query::RecallItem) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}  {}", item.page, item.title));
    if !item.status.is_empty() {
        out.push_str(&format!("  [{}]", item.status));
    }
    out.push('\n');
    if !item.summary.is_empty() {
        out.push_str(&format!("  {}\n", item.summary));
    }
    if item.incomplete {
        out.push_str("  (index incomplete)\n");
    }
    recall_group(&mut out, "outgoing", &item.outgoing);
    recall_group(&mut out, "incoming", &item.incoming);
    if item.sources.total > 0 {
        out.push_str(&format!("\nsources ({})\n", item.sources.total));
        for source in &item.sources.items {
            let availability = if source.raw_available {
                "available"
            } else {
                "missing"
            };
            out.push_str(&format!("  {}  {}\n", source.raw, availability));
        }
    }
    out
}

/// A source read: the raw's availability, then the pages citing it.
fn human_source(view: &query::SourceView) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{}  {}\n",
        view.raw_key,
        if view.available {
            "available"
        } else {
            "missing"
        }
    ));
    if view.citing.total == 0 {
        out.push_str("\nno page cites this source\n");
        return out;
    }
    let citing = &view.citing;
    let first = if citing.items.is_empty() {
        0
    } else {
        citing.offset + 1
    };
    out.push_str(&format!(
        "\nciting ({}-{} of {})\n",
        first,
        citing.offset + citing.items.len(),
        citing.total
    ));
    for page in &citing.items {
        out.push_str(&format!(
            "  {}  {}  {}\n",
            page.page, page.title, page.summary
        ));
    }
    if let Some(next) = citing.next_offset {
        out.push_str(&format!("  next: --offset {next}\n"));
    }
    out
}

fn recall_group(out: &mut String, label: &str, group: &query::PageGroup) {
    if group.total == 0 && group.unresolved.is_empty() {
        return;
    }
    let first = if group.items.is_empty() {
        0
    } else {
        group.offset + 1
    };
    out.push_str(&format!(
        "\n{label} ({}-{} of {})\n",
        first,
        group.offset + group.items.len(),
        group.total
    ));
    for page in &group.items {
        out.push_str(&format!(
            "  {}  {}  {}\n",
            page.page, page.title, page.summary
        ));
    }
    for unresolved in &group.unresolved {
        out.push_str(&format!(
            "  [[{}]]  {}\n",
            unresolved.literal, unresolved.resolution
        ));
    }
    if let Some(next) = group.next_offset {
        out.push_str(&format!("  next: --offset {next}\n"));
    }
}

/// One line per page with its degrees, then the orphans and the broken links.
fn human_graph(data: &graph::Graph) -> String {
    let mut out = String::new();
    for node in &data.nodes {
        out.push_str(&format!(
            "{:>3} {:>3}  {}\n",
            node.in_degree, node.out_degree, node.page
        ));
    }
    if !data.orphans.is_empty() {
        out.push_str(&format!("\n孤立页 ({})\n", data.orphans.len()));
        for page in &data.orphans {
            out.push_str(&format!("  {page}\n"));
        }
    }
    if !data.broken.is_empty() {
        out.push_str(&format!("\n断链 ({})\n", data.broken.len()));
        for target in &data.broken {
            out.push_str(&format!("  [[{target}]]\n"));
        }
    }
    out
}

/// A query that cannot run is the caller's mistake: empty text, a broken
/// ledger, a page path that cannot be named. No hits is not a failure — it is
/// an empty result with exit 0.
fn fail_query(cmd: &str, err: &query::QueryError) -> ! {
    report::fail(cmd, Exit::Usage, err.code(), &err.to_string())
}

/// Payload of `ignore` with no action.
#[derive(Serialize)]
struct IgnoreListData {
    count: usize,
    patterns: Vec<String>,
}

fn human_ignore_list(data: &IgnoreListData) -> String {
    if data.patterns.is_empty() {
        return String::from("no patterns");
    }
    data.patterns.join("\n")
}

/// Payload of `ignore --add` / `ignore --remove`.
#[derive(Serialize)]
struct IgnoreChangeData {
    added: Vec<String>,
    removed: Vec<String>,
    count: usize,
    patterns: Vec<String>,
}

fn human_ignore_change(data: &IgnoreChangeData) -> String {
    let mut out = String::new();
    for pattern in &data.added {
        out.push_str(&format!("added   {pattern}\n"));
    }
    for pattern in &data.removed {
        out.push_str(&format!("removed {pattern}\n"));
    }
    out.push_str(&format!("{} pattern(s) now", data.count));
    out
}

/// Payload of `ignore --check`.
#[derive(Serialize)]
struct IgnoreCheckData {
    key: String,
    ignored: bool,
    pattern: Option<String>,
}

fn human_ignore_check(data: &IgnoreCheckData) -> String {
    match &data.pattern {
        Some(pattern) => format!("{} is ignored by `{pattern}`", data.key),
        None => format!("{} is not ignored", data.key),
    }
}

/// One block per hit: the heading, the snippet, then the three next-step clues
/// on one arrow line. Expanded neighbours hang off it, one line each.
fn human_query(data: &QueryHitsData) -> String {
    if data.hits.is_empty() {
        return String::from("no hits");
    }
    let mut blocks = Vec::new();
    for (index, hit) in data.hits.iter().enumerate() {
        let excerpt = if hit.snippet.is_empty() {
            hit.summary.as_str()
        } else {
            hit.snippet.as_str()
        };
        // A body excerpt may span lines; a block line must not.
        let excerpt = excerpt
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let links = if hit.outgoing.total == 0 {
            String::from("-")
        } else {
            hit.outgoing
                .items
                .iter()
                .map(|link| link.title.clone())
                .collect::<Vec<_>>()
                .join(" | ")
        };
        let sources = if hit.sources.total == 0 {
            String::from("-")
        } else {
            hit.sources
                .items
                .iter()
                .map(|source| {
                    format!(
                        "{} ({})",
                        source.raw,
                        if source.raw_available {
                            "ok"
                        } else {
                            "missing"
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(" | ")
        };
        let mut block = format!(
            "{}. {} [{}] {}\n   {}\n   → out: {} | in: {} | sources: {}",
            index + 1,
            hit.title,
            hit.status,
            hit.page,
            excerpt,
            links,
            hit.incoming.total,
            sources
        );
        for expanded in &hit.expanded {
            block.push_str(&format!(
                "\n   ↳ {} ({}): {}",
                expanded.title, expanded.page, expanded.summary
            ));
        }
        blocks.push(block);
    }
    blocks.join("\n\n")
}

/// Payload of the `parse` success envelope, tagged by delivery mode.
#[derive(Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum ParseData {
    /// A source small enough to return whole.
    Full {
        backend: String,
        chars: usize,
        warnings: Vec<Warning>,
        markdown_head: String,
    },
    /// A chunked source: where the chunks live, one file per range.
    Chunked {
        backend: String,
        chars: usize,
        warnings: Vec<Warning>,
        chunks: usize,
        chunks_dir: String,
    },
    /// One chunk requested with `--chunk`.
    Chunk {
        backend: String,
        chars: usize,
        warnings: Vec<Warning>,
        chunks: usize,
        chunk: usize,
        markdown: String,
    },
    /// A window of chunks requested with `--range`.
    Range {
        backend: String,
        chars: usize,
        warnings: Vec<Warning>,
        chunks: usize,
        from: usize,
        to: usize,
        markdown: String,
    },
}

/// Payload of the `query` success envelope.
#[derive(Serialize)]
struct QueryHitsData {
    count: usize,
    hits: Vec<query::Hit>,
}

/// Payload of the `doctor` envelope: every check, in report order.
#[derive(Serialize)]
struct DoctorData {
    checks: Vec<Check>,
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
