//! The command-line surface: global flags and the subcommand table.
//!
//! Later tasks add their subcommands to [`Command`]; nothing here does work.

use clap::{Parser, Subcommand, ValueEnum};

/// Top-level invocation: `llm-wiki [--json] <command>`.
#[derive(Debug, Parser)]
#[command(
    name = "llm-wiki",
    about = "Maintain a Karpathy-style LLM wiki",
    // `version` must be the only subcommand, so do not let clap add `help`.
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Write one JSON document to stdout instead of human-readable text.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

/// One variant per subcommand, in the order the plan adds them.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the version of this binary.
    Version,
    /// Check the repository's health without changing anything.
    ///
    /// Every check runs; the exit code is 1 when one of them fails.
    Doctor,
    /// Write the contract files and create the engine directories.
    Init {
        /// Rewrite `wiki.toml` and `SCHEMA.md` even if they already exist.
        #[arg(long)]
        force: bool,
    },
    /// Compare `source/` with the ledger and report what changed.
    ///
    /// Both layers are scanned: `source/` says what `convert` should pick up,
    /// and the raw layer says what was edited by hand after conversion. Each
    /// change names its layer.
    Scan {
        /// Migrate the renames this scan reports: move the ledger rows and
        /// rewrite every page that cites the old name. Nothing else changes.
        #[arg(long)]
        apply_moves: bool,
    },
    /// Convert unconverted sources into the raw layer.
    Convert {
        /// Convert exactly this source key, converted before or not.
        #[arg(long)]
        key: Option<String>,
        /// Convert at most this many pending sources.
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Show what a source parses to, without writing anything.
    Parse {
        /// Source key, relative to `source/`.
        #[arg(long)]
        key: String,
        /// Return one chunk by number (1-based); materialized on demand.
        #[arg(long, conflicts_with = "range")]
        chunk: Option<usize>,
        /// Return chunks `A-B` (1-based, inclusive), joined.
        #[arg(long, conflicts_with = "chunk")]
        range: Option<String>,
        /// Extract a source above `parse.max_source_bytes` anyway, for diagnosis.
        /// Nothing is written: an oversize source never enters the ledger.
        #[arg(long)]
        force_large: bool,
    },
    /// Drive one ingest round: open it, close it, or drop it.
    Ingest {
        #[command(subcommand)]
        command: IngestCommand,
    },
    /// Audit the wiki against the lint rules.
    Lint {
        /// Fail on warnings as well as errors.
        #[arg(long)]
        strict: bool,
        /// Repair the defects that need no judgement, in place.
        #[arg(long)]
        fix: bool,
    },
    /// Snapshot the approved wiki as a new version.
    Checkpoint {
        /// Why the snapshot is taken; recorded in the version record.
        #[arg(long)]
        label: String,
    },
    /// Publish the staging candidates, or report that a human must confirm.
    Approve {
        /// Merge without asking; without it, a ready merge is only reported.
        #[arg(long)]
        confirm: bool,
    },
    /// Discard the staging candidates and record why.
    Reject {
        /// Why the candidates were rejected.
        #[arg(long)]
        reason: String,
    },
    /// Leave free-text guidance for the next agent session.
    Instruct {
        /// The instruction, written verbatim to `.wiki/next-instruction.md`.
        #[arg(long)]
        text: String,
    },
    /// Keep the queue for later without changing any state.
    Defer,
    /// Inspect, seal or clear the conversation summaries.
    Archive {
        #[command(subcommand)]
        command: ArchiveCommand,
    },
    /// Search the sealed conversation summaries.
    Recall {
        /// Case-insensitive substring of the title, summary or body.
        query: String,
        /// Return each whole summary instead of its opening excerpt.
        #[arg(long)]
        full: bool,
    },
    /// Search the wiki with the full-text index.
    Query {
        /// Words to search for; omit only together with `--reindex`.
        text: Option<String>,
        /// Return at most this many hits.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Inline the summary of each linked neighbour; one hop, five maximum.
        #[arg(long, default_value_t = 0)]
        expand: usize,
        /// Search live pages, archived summaries, or both.
        #[arg(long, value_enum, default_value = "all")]
        scope: ScopeArg,
        /// Rebuild the whole index from `wiki/` before searching.
        #[arg(long)]
        reindex: bool,
    },
    /// Print the `[[wikilink]]` structure of `wiki/`: degrees, edges, orphans, broken links.
    Graph,
    /// Run one unattended round: scan, convert, gate and audit.
    Sync {
        /// Convert at most this many sources; defaults to `sync.max_per_run`.
        #[arg(long)]
        limit: Option<usize>,
        /// Report what the round would do and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Ignore the settle window and the stable-streak gate.
        #[arg(long)]
        force: bool,
    },
}

/// The half of the index `query` may return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScopeArg {
    /// Live pages and archived summaries.
    All,
    /// Live pages only.
    Wiki,
    /// Archived summaries only.
    Archive,
}

/// The three verbs of the archive layer.
#[derive(Debug, Subcommand)]
pub enum ArchiveCommand {
    /// Report the four numbers that say whether the layering keeps up.
    Status,
    /// Record a summary the agent already wrote, and re-hang the pages that
    /// cite the sealed sources.
    Seal {
        /// Ledger keys of the sources being sealed; repeatable.
        #[arg(long)]
        sources: Vec<String>,
        /// Summary id; defaults to the `YYYY-MM` of `--period-end`.
        #[arg(long)]
        id: Option<String>,
        /// Summary title, for the human rendering only.
        #[arg(long)]
        title: Option<String>,
        /// First day the summary covers; defaults to the first of the month.
        #[arg(long)]
        period_start: Option<chrono::NaiveDate>,
        /// Last day the summary covers; defaults to today.
        #[arg(long)]
        period_end: Option<chrono::NaiveDate>,
        /// Who sealed it: `human` or `agent`.
        #[arg(long, default_value = "agent")]
        by: String,
    },
    /// Clear the summaries that are over capacity and past their deadline.
    Purge {
        /// Report the candidates and delete nothing.
        #[arg(long)]
        dry_run: bool,
        /// The human said yes; without it a purge only reports what it needs.
        #[arg(long)]
        yes: bool,
    },
}

/// The three verbs of an ingest round.
#[derive(Debug, Subcommand)]
pub enum IngestCommand {
    /// Open a round for these sources and report the pages they affect.
    Begin {
        /// Source keys the round reads; each must be in the ledger.
        #[arg(long)]
        sources: Vec<String>,
        /// Pages already known to be affected, merged into the scan.
        #[arg(long)]
        pages: Vec<String>,
        /// Report the round without writing its pending list.
        #[arg(long)]
        dry_run: bool,
    },
    /// Close the open round: refresh the candidates and the staging index.
    Commit {
        /// Candidate page ids to publish into staging, in the order given.
        #[arg(long)]
        pages: Vec<String>,
    },
    /// Drop the open round, keeping the candidates already written.
    Abort,
}
