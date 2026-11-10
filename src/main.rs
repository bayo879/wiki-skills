//! `llm-wiki` entry point: parse arguments, dispatch, report.
//!
//! No business logic lives here; each command handler ends in
//! `report::emit` or `report::fail`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::Serialize;

use llm_wiki::chunk;
use llm_wiki::cli::{Cli, Command, DoctorOnly};
use llm_wiki::config::Config;
use llm_wiki::doctor::{self, Check, Level};
use llm_wiki::graph;
use llm_wiki::hash;
use llm_wiki::ignore;
use llm_wiki::lock;
use llm_wiki::parse::{self, Warning};
use llm_wiki::paths;
use llm_wiki::precondition;
use llm_wiki::query::{self, Scope as QueryScope};
use llm_wiki::report::{self, Exit};
use llm_wiki::store::Store;
use llm_wiki::sync;
use llm_wiki::worklist::{self, WorkError};

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
            dry_run,
            key,
            chunk,
            range,
            force_large,
            force,
        } => run_ingest(keys, dry_run, key, chunk, range, force_large, force),
        Command::Done { key, hash } => run_done(key, hash),
        Command::Ignore { add, remove, check } => run_ignore(&add, &remove, check.as_deref()),
        Command::Query {
            text,
            limit,
            no_archive,
            graph,
        } => run_query(text, limit, no_archive, graph),
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

/// Opens the repository for a command: proves it was initialised, provisions
/// the derived directories, then loads the contract.
///
/// The order matters. `init` is the only way to create `wiki.toml`, `SCHEMA.md`
/// and `wiki/`, so a command that finds one of them missing refuses with
/// `E_NOT_INITIALISED` and a hint instead of failing later on a symptom (a
/// missing database, an empty scan, a staging area with nothing to compare).
/// Everything else in the layout is derived, so it is created here rather than
/// demanded from the caller.
fn open_repo(cmd: &str, root: &Path) -> Config {
    if let Err(missing) = precondition::check(root) {
        report::fail_detail(
            cmd,
            Exit::Usage,
            precondition::CODE,
            &format!(
                "the repository at {} is not initialised: {} `{}` is missing",
                root.display(),
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
///
/// The payload ends with a fixed follow-up prompt aimed at the agent: it names
/// two steps the CLI cannot take itself (a per-project instruction and a
/// scheduler). The human form prints it verbatim instead of folding it onto one
/// `next: …` line.
fn run_doctor(fix: bool, only: Option<DoctorOnly>, key: Option<String>) -> ! {
    let root = wiki_root("doctor");
    let cfg = Config::load(&root).ok();
    let opts = doctor::DoctorOptions { fix, only, key };
    let checks = doctor::run(&root, cfg.as_ref(), &opts);
    let failed = checks.iter().any(|check| check.level == Level::Fail);
    let exit = if failed { Exit::Error } else { Exit::Ok };
    report::emit_with("doctor", DoctorData { checks }, exit, human_doctor)
}

/// Human rendering of the checks: one aligned row each, no summary line (the
/// exit code is the summary).
fn human_doctor(data: &DoctorData) -> String {
    data.checks
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
fn run_ingest(
    keys: Vec<String>,
    dry_run: bool,
    key: Option<String>,
    chunk: Option<usize>,
    range: Option<String>,
    force_large: bool,
    force: bool,
) -> ! {
    let cmd = "ingest";
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
            let exit = if report.failures > 0 {
                Exit::Error
            } else {
                Exit::Ok
            };
            // The author writes live pages directly and owns every source citation.
            let data = IngestData::from_report(report, &cfg);
            report::emit_with(cmd, data, exit, human_ingest)
        }
        Err(err) => {
            let message = match &err {
                sync::SyncError::Lock(lock_err) => lock_err.hint(),
                _ => err.to_string(),
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

/// Source conversion results and the direct-page authoring contract.
#[derive(Serialize)]
struct IngestData {
    run_id: String,
    changes: Vec<llm_wiki::scan::Change>,
    stale_pages: Vec<String>,
    converted: usize,
    ingested: Vec<String>,
    queued: usize,
    waiting: Vec<String>,
    warnings: Vec<String>,
    failures: usize,
    pending: Vec<llm_wiki::store::WorkItem>,
    affected_pages: Vec<String>,
    write_to: String,
    types: Vec<String>,
    schema: String,
    instructions: String,
}

impl IngestData {
    fn from_report(report: sync::SyncReport, cfg: &Config) -> Self {
        Self {
            run_id: report.run_id,
            changes: report.changes,
            stale_pages: report.stale_pages,
            converted: report.converted,
            ingested: report.ingested,
            queued: report.queued,
            waiting: report.waiting,
            warnings: report.warnings,
            failures: report.failures,
            pending: report.work_items,
            affected_pages: report.affected_pages,
            write_to: cfg.paths.wiki.clone(),
            types: cfg.schema.types.clone(),
            schema: format!("{}/SCHEMA.md", cfg.paths.engine.trim_end_matches('/')),
            instructions: String::from("Write or update live pages directly in write_to, following schema and types. The author must explicitly fill truthful sources and updated; the tool never adds sources. Writes take effect immediately. After checking pages, acknowledge each source with done using the raw hash actually read; done does not publish, validate prose, or roll back pages."),
        }
    }
}

fn human_ingest(data: &IngestData) -> String {
    let mut out = format!("run_id: {}\nconverted: {}\nqueued: {}\npending_total: {}\nfailures: {}\nwrite_to: {}\ntypes: {}\nschema: {}\n{}", data.run_id, data.converted, data.queued, data.pending.len(), data.failures, data.write_to, data.types.join(", "), data.schema, data.instructions);
    for item in &data.pending {
        out.push_str(&format!(
            "\npending: {} -> {} ({})",
            item.source_key, item.raw_key, item.raw_hash
        ));
    }
    for key in &data.waiting {
        out.push_str(&format!("\nwaiting: {key}"));
    }
    for page in &data.affected_pages {
        out.push_str(&format!("\naffected_page: {page}"));
    }
    for warning in &data.warnings {
        out.push_str(&format!("\nwarning: {warning}"));
    }
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
fn run_done(key: String, hash: String) -> ! {
    let cmd = "done";
    let root = wiki_root(cmd);
    let cfg = open_repo(cmd, &root);
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
    let mut store = open_store(cmd, &root, &cfg);
    let result = worklist::complete(&root, &cfg, &mut store, &key, &hash);
    drop(guard);
    match result {
        Ok(completed) => report::emit_with(
            cmd,
            DoneData {
                key,
                hash,
                completed,
            },
            Exit::Ok,
            human_done,
        ),
        Err(err) => {
            let code = match &err {
                WorkError::Drift(_) => "E_SOURCE_DRIFT",
                WorkError::UnknownSource(_) => "E_NO_SOURCE",
                WorkError::MissingRaw(_) => "E_NO_RAW",
                WorkError::Store(_) => "E_DB",
                WorkError::Hash(_) => "E_IO",
            };
            report::fail(cmd, Exit::Error, code, &err.to_string())
        }
    }
}

#[derive(Serialize)]
struct DoneData {
    key: String,
    hash: String,
    completed: bool,
}

fn human_done(data: &DoneData) -> String {
    format!(
        "{}: {}",
        data.key,
        if data.completed {
            "completed"
        } else {
            "already complete"
        }
    )
}

/// `query` is the only command that decides when to pay for an index update: a
/// plain search catches up incrementally. A full rebuild is `doctor --fix`,
/// which nobody asks for by accident.
fn run_query(text: Option<String>, limit: usize, no_archive: bool, graph: bool) -> ! {
    let cmd = "query";
    let root = wiki_root(cmd);
    let cfg = open_repo(cmd, &root);

    if graph {
        match graph::build(&root, &cfg) {
            Ok(data) => report::emit_with(cmd, data, Exit::Ok, human_graph),
            Err(err) => report::fail(cmd, Exit::Usage, err.code(), &err.to_string()),
        }
    }

    let mut store = open_store(cmd, &root, &cfg);
    let text = text.unwrap_or_default();
    if text.trim().is_empty() {
        report::fail(
            cmd,
            Exit::Usage,
            "E_EMPTY_QUERY",
            "no query text; pass a word to search for, or --graph",
        )
    }
    if let Err(err) = query::refresh(&root, &cfg, &mut store) {
        fail_query(cmd, &err)
    }

    let search = query::SearchQuery {
        text,
        limit,
        expand: 0,
        scope: if no_archive {
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
        let links = if hit.links.is_empty() {
            String::from("-")
        } else {
            hit.links
                .iter()
                .map(|link| link.title.clone())
                .collect::<Vec<_>>()
                .join(" | ")
        };
        let sources = if hit.sources.is_empty() {
            String::from("-")
        } else {
            hit.sources
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
            "{}. {} [{}] {}\n   {}\n   → links: {} | sources: {} | backlinks {}",
            index + 1,
            hit.title,
            hit.status,
            hit.page,
            excerpt,
            links,
            sources,
            hit.backlinks
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
