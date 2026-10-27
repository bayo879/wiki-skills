//! `llm-wiki` entry point: parse arguments, dispatch, report.
//!
//! No business logic lives here; each command handler ends in
//! `report::emit` or `report::fail`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::Serialize;
use serde_json::json;

use llm_wiki::archive;
use llm_wiki::chunk;
use llm_wiki::cli::{ArchiveCommand, Cli, Command, IngestCommand, ScopeArg};
use llm_wiki::config::Config;
use llm_wiki::convert;
use llm_wiki::hash;
use llm_wiki::ingest;
use llm_wiki::init;
use llm_wiki::lint;
use llm_wiki::parse::{self, Warning};
use llm_wiki::paths;
use llm_wiki::query::{self, Scope as QueryScope};
use llm_wiki::report::{self, Exit};
use llm_wiki::scan::{self, ChangeKind};
use llm_wiki::store::Store;
use llm_wiki::sync;
use llm_wiki::version::{self, ApproveOutcome};

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
        Command::Parse { key, chunk, range } => run_parse(key, chunk, range),
        Command::Ingest { command } => run_ingest(command),
        Command::Lint { strict, fix } => run_lint(strict, fix),
        Command::Checkpoint { label } => run_checkpoint(label),
        Command::Approve { confirm } => run_approve(confirm),
        Command::Reject { reason } => run_reject(reason),
        Command::Instruct { text } => run_instruct(text),
        Command::Defer => run_defer(),
        Command::Archive { command } => run_archive(command),
        Command::Recall { query, full } => run_recall(query, full),
        Command::Query {
            text,
            limit,
            expand,
            scope,
            reindex,
        } => run_query(text, limit, expand, scope, reindex),
        Command::Sync {
            limit,
            dry_run,
            force,
        } => run_sync(limit, dry_run, force),
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

/// `sync` runs one unattended round under the repository lock.
///
/// A round that converted some sources and failed others still ran: the
/// failures are findings (exit 1), and the caller can read them off the report.
/// A held lock is exit 4, so a scheduler can tell "someone else is working" from
/// "this round found problems". Anything else is an environment error (exit 2).
fn run_sync(limit: Option<usize>, dry_run: bool, force: bool) -> ! {
    let root = wiki_root("sync");
    let cfg = load_config("sync", &root);
    let mut store = open_store("sync", &root, &cfg);
    let args = sync::SyncArgs {
        limit: limit.unwrap_or(cfg.sync.max_per_run),
        dry_run,
        force,
    };
    match sync::run(&root, &cfg, &mut store, &args) {
        Ok(report) => {
            let exit = if report.failures > 0 {
                Exit::Error
            } else {
                Exit::Ok
            };
            report::emit_with("sync", report, exit, human_sync)
        }
        Err(err) => {
            // The envelope carries the code, so the message is the hint alone:
            // the library's `Display` prefixes the code for log lines.
            let message = match &err {
                sync::SyncError::Lock(lock_err) => lock_err.hint(),
                _ => err.to_string(),
            };
            let exit = if err.code() == "E_LOCKED" {
                Exit::Busy
            } else {
                Exit::Usage
            };
            report::fail("sync", exit, err.code(), &message)
        }
    }
}

/// Human rendering of a sync round: the counters first, then the warnings that
/// explain them.
fn human_sync(report: &sync::SyncReport) -> String {
    let mut out = format!("run_id: {}\n", report.run_id);
    out.push_str(&format!("converted: {}\n", report.converted));
    out.push_str(&format!(
        "ingested: {}\n",
        if report.ingested.is_empty() {
            String::from("(none)")
        } else {
            report.ingested.join(", ")
        }
    ));
    out.push_str(&format!("queued: {}\n", report.queued));
    out.push_str(&format!("failures: {}\n", report.failures));
    for warning in &report.warnings {
        out.push_str(&format!("warning: {warning}\n"));
    }
    out.trim_end().to_string()
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
/// it; without it, a scan decides what is pending — sources classified as new or
/// changed — and the first `--limit` of them, in key order, are converted. A
/// failed conversion is a finding (exit 1), not an environment error, so that one
/// bad source does not look like a broken invocation.
fn run_convert(key: Option<String>, limit: usize) -> ! {
    let root = wiki_root("convert");
    let cfg = load_config("convert", &root);
    let mut store = open_store("convert", &root, &cfg);

    let keys = match conversion_targets(&root, &cfg, &store, key.as_deref(), limit) {
        Ok(keys) => keys,
        Err(err) => report::fail("convert", Exit::Usage, "E_IO", &err.to_string()),
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
///
/// `--key` names one source directly, so it works before the first scan. Without
/// it the pending set comes from a scan: the ledger alone cannot answer the
/// question, because a new source only gains a row once something converts it.
fn conversion_targets(
    root: &Path,
    cfg: &Config,
    store: &Store,
    key: Option<&str>,
    limit: usize,
) -> std::result::Result<Vec<String>, scan::ScanError> {
    if let Some(key) = key {
        return Ok(vec![String::from(key)]);
    }
    let scan_report = scan::scan_source(root, cfg, store)?;
    let mut keys: Vec<String> = scan_report
        .changes
        .into_iter()
        .filter(|change| matches!(change.kind, ChangeKind::New | ChangeKind::Changed))
        .map(|change| change.key)
        .collect();
    keys.sort();
    keys.truncate(limit);
    Ok(keys)
}

/// `parse` shows what one source parses to.
///
/// It is the safe counterpart of `convert`: the same backend and the same
/// warnings, but nothing is written except the chunk cache of a large source.
/// Sources at or below `parse.chunk_threshold` keep the whole-document shape; a
/// larger source is materialized as chunk files under `.wiki/cache/<hash>/` and
/// reported by `chunks_dir`, so later reads can pick one chunk (`--chunk`) or a
/// window of chunks (`--range`) instead of loading the whole text again.
fn run_parse(key: String, chunk_wanted: Option<usize>, range: Option<String>) -> ! {
    let root = wiki_root("parse");
    let cfg = load_config("parse", &root);
    let source_path = paths::join_key(&root.join(&cfg.paths.source), &key);
    let bytes = match std::fs::read(&source_path) {
        Ok(bytes) => bytes,
        Err(err) => report::fail(
            "parse",
            Exit::Error,
            "E_IO",
            &format!("cannot read {}: {err}", source_path.display()),
        ),
    };

    let delivery = match parse::delivery(&cfg, bytes.len() as u64) {
        Ok(delivery) => delivery,
        Err(err) => report::fail("parse", Exit::Usage, err.code(), &err.to_string()),
    };

    let (extracted, backend) = match parse::parse_with(&cfg, &parse::Source::inline(&key, &bytes)) {
        Ok(result) => result,
        Err(err) => report::fail("parse", Exit::Usage, err.code(), &err.to_string()),
    };

    if delivery == parse::Delivery::Whole && chunk_wanted.is_none() && range.is_none() {
        report::emit(
            "parse",
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
        Err(err) => report::fail("parse", Exit::Usage, err.code(), &err.to_string()),
    };
    let count = chunks.len();
    let chunks_rel = paths::to_slash(&Path::new(&cfg.paths.engine).join("cache").join(hash));

    if let Some(number) = chunk_wanted {
        let text = match chunk::read_range(&cache_root, hash, number, number) {
            Ok(text) => text,
            Err(err) => report::fail("parse", Exit::Usage, err.code(), &err.to_string()),
        };
        report::emit(
            "parse",
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
            Err(message) => report::fail("parse", Exit::Usage, "E_BAD_ARGS", &message),
        };
        let text = match chunk::read_range(&cache_root, hash, from, to) {
            Ok(text) => text,
            Err(err) => report::fail("parse", Exit::Usage, err.code(), &err.to_string()),
        };
        report::emit(
            "parse",
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
        "parse",
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

/// `ingest` drives one round: `begin` opens it, `commit`/`abort` close it.
///
/// A round never publishes: `commit` refreshes the candidates under staging and
/// regenerates the staging index, and approval is what moves pages into `wiki/`.
fn run_ingest(command: IngestCommand) -> ! {
    let cmd = match &command {
        IngestCommand::Begin { .. } => "ingest.begin",
        IngestCommand::Commit { .. } => "ingest.commit",
        IngestCommand::Abort => "ingest.abort",
    };
    let root = wiki_root(cmd);
    let cfg = load_config(cmd, &root);
    let mut store = open_store(cmd, &root, &cfg);

    match command {
        IngestCommand::Begin {
            sources,
            pages,
            dry_run,
        } => {
            let input = ingest::BeginInput {
                sources,
                pages,
                dry_run,
            };
            match ingest::begin(&root, &cfg, &mut store, &input) {
                Ok(outcome) => report::emit(cmd, outcome),
                Err(err) => fail_ingest(cmd, &err),
            }
        }
        IngestCommand::Commit { pages } => match ingest::commit(&root, &cfg, &mut store, &pages) {
            Ok(outcome) => report::emit(cmd, outcome),
            Err(err) => fail_ingest(cmd, &err),
        },
        IngestCommand::Abort => match ingest::abort(&root, &cfg, &mut store) {
            Ok(outcome) => report::emit(cmd, outcome),
            Err(err) => fail_ingest(cmd, &err),
        },
    }
}

/// A named-but-missing source, page or round is the caller's mistake (exit 2);
/// a damaged staging directory or ledger is a finding (exit 1).
fn fail_ingest(cmd: &str, err: &ingest::IngestError) -> ! {
    let exit = if err.code().starts_with("E_NO_") {
        Exit::Usage
    } else {
        Exit::Error
    };
    report::fail(cmd, exit, err.code(), &err.to_string())
}

/// `checkpoint` snapshots the approved wiki as a new version.
fn run_checkpoint(label: String) -> ! {
    let root = wiki_root("checkpoint");
    let cfg = load_config("checkpoint", &root);
    let mut store = open_store("checkpoint", &root, &cfg);
    match version::checkpoint(&root, &cfg, &mut store, &label) {
        Ok(version) => report::emit("checkpoint", CheckpointData { version }),
        Err(err) => fail_version("checkpoint", &err),
    }
}

/// `approve` publishes the staged candidates, but only when a human confirmed.
///
/// Without `--confirm`, a ready merge is reported as `NEEDS_CONFIRMATION` with
/// exit 3: exiting 0 would read as "published" and exiting 1 as "failed", while
/// the human decision is a third outcome with its own code.
fn run_approve(confirm: bool) -> ! {
    let root = wiki_root("approve");
    let cfg = load_config("approve", &root);
    let mut store = open_store("approve", &root, &cfg);
    match version::approve(&root, &cfg, &mut store, confirm) {
        Ok(ApproveOutcome::Applied { version }) => report::emit("approve", ApproveData { version }),
        Ok(ApproveOutcome::NeedsConfirmation { reason, detail }) => report::fail_detail(
            "approve",
            Exit::NeedsConfirm,
            "NEEDS_CONFIRMATION",
            &format!("{reason}: {detail}"),
            Some(json!({ "reason": reason, "detail": detail })),
        ),
        Err(err) => fail_version("approve", &err),
    }
}

/// `reject` discards the candidates and keeps the reason in the log.
fn run_reject(reason: String) -> ! {
    let root = wiki_root("reject");
    let cfg = load_config("reject", &root);
    let mut store = open_store("reject", &root, &cfg);
    match version::reject(&root, &cfg, &mut store, &reason) {
        Ok(()) => report::emit("reject", RejectData { reason }),
        Err(err) => fail_version("reject", &err),
    }
}

/// `instruct` writes guidance for the next agent session.
fn run_instruct(text: String) -> ! {
    let root = wiki_root("instruct");
    let cfg = load_config("instruct", &root);
    let mut store = open_store("instruct", &root, &cfg);
    match version::instruct(&root, &cfg, &mut store, &text) {
        Ok(()) => report::emit("instruct", InstructData { text }),
        Err(err) => fail_version("instruct", &err),
    }
}

/// `defer` keeps the queue for later and changes nothing else.
fn run_defer() -> ! {
    let root = wiki_root("defer");
    let cfg = load_config("defer", &root);
    let mut store = open_store("defer", &root, &cfg);
    match version::defer(&root, &cfg, &mut store) {
        Ok(()) => report::emit("defer", json!({})),
        Err(err) => fail_version("defer", &err),
    }
}

/// A drift or a stale source is a finding (exit 1); everything else — a missing
/// candidate set, a bad reason, an unwritable path — is the caller's or the
/// environment's mistake (exit 2).
fn fail_version(cmd: &str, err: &version::VersionError) -> ! {
    let exit = match err {
        version::VersionError::Drift { .. } | version::VersionError::Stale { .. } => Exit::Error,
        _ => Exit::Usage,
    };
    report::fail(cmd, exit, err.code(), &err.to_string())
}

/// `archive` inspects, seals or clears the conversation summaries.
///
/// The summary body is never written here: the agent writes
/// `wiki/archive/<id>.md` first, and `seal` only records it and re-hangs the
/// pages that cited the sealed sources.
fn run_archive(command: ArchiveCommand) -> ! {
    let cmd = "archive";
    let root = wiki_root(cmd);
    let cfg = load_config(cmd, &root);
    let mut store = open_store(cmd, &root, &cfg);

    match command {
        ArchiveCommand::Status => match archive::status(&root, &cfg, &store) {
            Ok(outcome) => report::emit_with(
                cmd,
                ArchiveStatusData {
                    action: "status",
                    archives: outcome.archives,
                    live_sessions: outcome.live_sessions,
                    purge_candidates: outcome.purge_candidates,
                    still_referenced: outcome.still_referenced,
                    note: outcome.note,
                },
                Exit::Ok,
                human_archive_status,
            ),
            Err(err) => fail_archive(cmd, &err),
        },
        ArchiveCommand::Seal {
            sources,
            id,
            title,
            period_start,
            period_end,
            by,
        } => {
            // `sealed_by` is a closed vocabulary; a third value would make the
            // record unfalsifiable.
            if by != "human" && by != "agent" {
                report::fail(
                    cmd,
                    Exit::Usage,
                    "E_USAGE",
                    &format!("`--by` is `human` or `agent`, not `{by}`"),
                );
            }
            let req = archive::SealRequest {
                sources,
                id,
                title,
                period_start,
                period_end,
                by,
            };
            match archive::seal(&root, &cfg, &mut store, &req) {
                Ok(outcome) => report::emit_with(
                    cmd,
                    ArchiveSealData {
                        action: "seal",
                        key: outcome.key,
                        sources: outcome.sources,
                        pages: outcome.pages,
                        updated_pages: outcome.updated_pages,
                    },
                    Exit::Ok,
                    human_archive_seal,
                ),
                Err(err) => fail_archive(cmd, &err),
            }
        }
        ArchiveCommand::Purge { dry_run, yes } => {
            let req = archive::PurgeRequest { dry_run, yes };
            match archive::purge(&root, &cfg, &mut store, &req) {
                Ok(outcome) => report::emit_with(
                    cmd,
                    ArchivePurgeData {
                        action: "purge",
                        candidates: outcome.candidates,
                        sources: outcome.sources,
                        deleted: outcome.deleted,
                        dry_run: outcome.dry_run,
                    },
                    Exit::Ok,
                    human_archive_purge,
                ),
                Err(err) => fail_archive(cmd, &err),
            }
        }
    }
}

/// `recall` searches the sealed summaries.
///
/// Configuration is loaded only to find the ledger: the search itself depends on
/// nothing the config could change.
fn run_recall(query: String, full: bool) -> ! {
    let cmd = "recall";
    let root = wiki_root(cmd);
    let cfg = load_config(cmd, &root);
    let mut store = open_store(cmd, &root, &cfg);
    let req = archive::RecallRequest { query, full };
    match archive::recall(&root, &mut store, &req) {
        Ok(outcome) => report::emit_with(
            cmd,
            RecallData {
                hits: outcome.hits.into_iter().map(RecallHitData::from).collect(),
                missing: outcome.missing,
            },
            Exit::Ok,
            human_recall,
        ),
        Err(err) => fail_archive(cmd, &err),
    }
}

/// `query` is the only command that decides when to pay for an index update:
/// `--reindex` rebuilds, a plain search catches up incrementally, and with no
/// text only `--reindex` has something to do (report the page count).
fn run_query(
    text: Option<String>,
    limit: usize,
    expand: usize,
    scope: ScopeArg,
    reindex: bool,
) -> ! {
    let cmd = "query";
    let root = wiki_root(cmd);
    let cfg = load_config(cmd, &root);
    let mut store = open_store(cmd, &root, &cfg);
    let text = text.unwrap_or_default();
    let empty = text.trim().is_empty();

    if reindex {
        match query::reindex(&root, &cfg, &mut store) {
            Ok(indexed) if empty => report::emit(cmd, QueryReindexData { indexed }),
            Ok(_) => {}
            Err(err) => fail_query(cmd, &err),
        }
    } else if empty {
        // Empty text without a rebuild is the caller's mistake, and it is
        // caught before any write: `query` must not silently rebuild.
        report::fail(
            cmd,
            Exit::Usage,
            "E_EMPTY_QUERY",
            "no query text; pass a word to search for, or --reindex",
        )
    } else if let Err(err) = query::refresh(&root, &cfg, &mut store) {
        fail_query(cmd, &err)
    }

    let search = query::SearchQuery {
        text,
        limit,
        expand,
        scope: match scope {
            ScopeArg::All => QueryScope::All,
            ScopeArg::Wiki => QueryScope::Wiki,
            ScopeArg::Archive => QueryScope::Archive,
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

/// A query that cannot run is the caller's mistake: empty text, a broken
/// ledger, a page path that cannot be named. No hits is not a failure — it is
/// an empty result with exit 0.
fn fail_query(cmd: &str, err: &query::QueryError) -> ! {
    report::fail(cmd, Exit::Usage, err.code(), &err.to_string())
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
        if let Some(recall) = &hit.recall {
            block.push_str(&format!(" | recall: {recall}"));
        }
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

/// A purge that needs a human is a third outcome (exit 3), a recall that matched
/// nothing is a finding (exit 1), and every other refusal — an unledgered key, an
/// already sealed one, a missing summary, a bad query, a broken ledger — is the
/// caller's or the environment's mistake (exit 2).
fn fail_archive(cmd: &str, err: &archive::ArchiveError) -> ! {
    let exit = match err {
        archive::ArchiveError::NeedsConfirmation => Exit::NeedsConfirm,
        archive::ArchiveError::NoHit => Exit::Error,
        _ => Exit::Usage,
    };
    report::fail(cmd, exit, err.code(), &err.to_string())
}

/// The four numbers, one per line, so a diff between two runs reads as a diff.
fn human_archive_status(data: &ArchiveStatusData) -> String {
    [
        format!("summaries: {}", data.archives),
        format!("live sessions: {}", data.live_sessions),
        format!("purge candidates: {}", data.purge_candidates),
        format!("still referenced: {}", data.still_referenced),
    ]
    .join("\n")
}

/// One line: what was sealed, and how many pages moved to cite it.
fn human_archive_seal(data: &ArchiveSealData) -> String {
    format!("sealed {} ({} pages re-hung)", data.key, data.updated_pages)
}

/// The candidates first, then the counts that explain them.
fn human_archive_purge(data: &ArchivePurgeData) -> String {
    let mut lines: Vec<String> = data
        .candidates
        .iter()
        .map(|key| format!("candidate: {key}"))
        .collect();
    lines.push(format!("deleted: {}", data.deleted));
    lines.push(format!("sources: {}", data.sources));
    lines.push(format!("dry run: {}", data.dry_run));
    lines.join("\n")
}

/// Each hit as `title [period] key` with its body indented beneath, so a human
/// can tell the hit from the summary it quotes.
fn human_recall(data: &RecallData) -> String {
    let mut lines = Vec::new();
    for hit in &data.hits {
        lines.push(format!(
            "{} [{}..{}] {}",
            hit.title, hit.period_start, hit.period_end, hit.key
        ));
        for line in hit.body.lines() {
            if line.is_empty() {
                lines.push(String::new());
            } else {
                lines.push(format!("    {line}"));
            }
        }
    }
    if data.missing > 0 {
        lines.push(format!("missing summaries: {}", data.missing));
    }
    lines.join("\n")
}

/// `lint` audits the wiki; with `--fix` it repairs the mechanical defects
/// first, then reports what is left.
///
/// The exit code is decided by the findings, not by the command: an error (or
/// any finding under `--strict`) is exit 1 even though the command succeeded.
fn run_lint(strict: bool, fix: bool) -> ! {
    let root = wiki_root("lint");
    let cfg = load_config("lint", &root);
    let mut store = open_store("lint", &root, &cfg);

    if fix {
        match lint::fix(&root, &cfg, &mut store) {
            Ok(outcome) => {
                let exit = if lint::fails(&outcome.remaining, strict) {
                    Exit::Error
                } else {
                    Exit::Ok
                };
                report::emit_with("lint", outcome, exit, human_fix)
            }
            Err(err) => report::fail("lint", Exit::Usage, err.code(), &err.to_string()),
        }
    } else {
        match lint::lint(&root, &cfg, &store) {
            Ok(findings) => {
                let exit = if lint::fails(&findings, strict) {
                    Exit::Error
                } else {
                    Exit::Ok
                };
                let payload = LintData { findings };
                report::emit_with("lint", payload, exit, human_lint)
            }
            Err(err) => report::fail("lint", Exit::Usage, err.code(), &err.to_string()),
        }
    }
}

/// One finding per line, or a single `clean` line when there is nothing to
/// report.
fn human_lint(data: &LintData) -> String {
    if data.findings.is_empty() {
        return String::from("lint: clean");
    }
    data.findings
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// What `--fix` changed, then the findings it could not repair.
fn human_fix(outcome: &lint::FixReport) -> String {
    let mut lines = vec![format!("fixed: {}", outcome.fixed)];
    lines.extend(outcome.remaining.iter().map(ToString::to_string));
    lines.join("\n")
}

/// Payload of the `lint` success envelope.
#[derive(Serialize)]
struct LintData {
    findings: Vec<lint::Finding>,
}

/// Payload of the `version` success envelope.
#[derive(Serialize)]
struct VersionData {
    version: &'static str,
}

/// Payload of the `checkpoint` success envelope.
#[derive(Serialize)]
struct CheckpointData {
    version: String,
}

/// Payload of the `approve` success envelope.
#[derive(Serialize)]
struct ApproveData {
    version: String,
}

/// Payload of the `reject` success envelope.
#[derive(Serialize)]
struct RejectData {
    reason: String,
}

/// Payload of the `instruct` success envelope.
#[derive(Serialize)]
struct InstructData {
    text: String,
}

/// Payload of the `convert` success envelope.
#[derive(Serialize)]
struct ConvertData {
    converted: Vec<convert::ConvertOutcome>,
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

/// Payload of the `archive status` success envelope.
#[derive(Serialize)]
struct ArchiveStatusData {
    action: &'static str,
    archives: usize,
    live_sessions: usize,
    purge_candidates: usize,
    still_referenced: usize,
    note: String,
}

/// Payload of the `archive seal` success envelope.
#[derive(Serialize)]
struct ArchiveSealData {
    action: &'static str,
    key: String,
    sources: Vec<String>,
    pages: Vec<String>,
    updated_pages: usize,
}

/// Payload of the `archive purge` success envelope.
#[derive(Serialize)]
struct ArchivePurgeData {
    action: &'static str,
    candidates: Vec<String>,
    sources: usize,
    deleted: usize,
    dry_run: bool,
}

/// Payload of the `recall` success envelope.
#[derive(Serialize)]
struct RecallData {
    hits: Vec<RecallHitData>,
    missing: usize,
}

/// One hit: the summary's identity, its period and the text to read.
#[derive(Serialize)]
struct RecallHitData {
    key: String,
    title: String,
    period_start: String,
    period_end: String,
    body: String,
}

impl From<archive::RecallHit> for RecallHitData {
    fn from(hit: archive::RecallHit) -> Self {
        Self {
            key: hit.key,
            title: hit.title,
            period_start: hit.period_start,
            period_end: hit.period_end,
            body: hit.body,
        }
    }
}

/// Payload of the `query` success envelope.
#[derive(Serialize)]
struct QueryHitsData {
    count: usize,
    hits: Vec<query::Hit>,
}

/// Payload of `query --reindex` with no text: what a rebuild indexed.
#[derive(Serialize)]
struct QueryReindexData {
    indexed: usize,
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
