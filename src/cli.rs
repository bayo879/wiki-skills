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
    /// Check the repository's health; `--fix` repairs the unambiguous defects.
    ///
    /// Without `--fix` it only reads. The exit code is 1 when a check fails.
    /// `--fix` bootstraps an uninitialised repository, then runs `lint --fix`
    /// and rebuilds the search index before reporting.
    Doctor {
        /// Repair what has one obvious correction; never rewrites content.
        #[arg(long)]
        fix: bool,
        /// Run only one family of checks.
        #[arg(long, value_enum)]
        only: Option<DoctorOnly>,
        /// Restrict the `convert` and `session` families to one source key.
        #[arg(long)]
        key: Option<String>,
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
    /// One write-line round: scan, convert, and open it.
    Ingest {
        /// Sources to read; empty means every new or changed source.
        keys: Vec<String>,
        /// Report what the round would do and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Preview one source's parse output; requires --dry-run.
        #[arg(long, requires = "dry_run")]
        key: Option<String>,
        /// Return chunk `N` (1-based, materialized on demand).
        #[arg(long, conflicts_with = "range")]
        chunk: Option<usize>,
        /// Return chunks `A-B` (1-based, inclusive), joined.
        #[arg(long, conflicts_with = "chunk")]
        range: Option<String>,
        /// Extract a source above `parse.max_source_bytes` anyway, for diagnosis.
        #[arg(long)]
        force_large: bool,
    },
    /// Close the open round: refresh its candidates, then publish them.
    Submit {
        /// Publish even a candidate the brakes would hold.
        #[arg(long, conflicts_with = "auto")]
        confirm: bool,
        /// Unattended: skip a braked candidate and report it instead of asking.
        #[arg(long, conflicts_with = "confirm")]
        auto: bool,
    },
    /// Drop the open round, keeping the candidates already written.
    Abort,
    /// Snapshot the approved wiki as a new version.
    Checkpoint {
        /// Why the snapshot is taken; recorded in the version record.
        #[arg(long)]
        label: String,
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
    /// List, add, remove or explain entries of `.wiki/ignore`.
    Ignore {
        #[command(subcommand)]
        command: IgnoreCommand,
    },
    /// Restore the pages and raws of a recorded version into the working tree.
    Rollback {
        /// Version name such as `v003`.
        version: String,
    },
    /// List the recorded versions, newest first.
    History,
    /// Show what one version adds, removes or changes against the working tree.
    Diff {
        /// Version name such as `v003`.
        version: String,
    },
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
        /// Convert only these repo-relative sources; repeatable.
        #[arg(long = "only", value_name = "PATH")]
        only: Vec<String>,
    },
}

/// The four verbs of the project ignore list.
#[derive(Debug, Subcommand)]
pub enum IgnoreCommand {
    /// Print every pattern, in file order.
    List,
    /// Append patterns; a duplicate is not added twice.
    Add {
        /// `.gitignore`-style patterns.
        patterns: Vec<String>,
    },
    /// Remove patterns by exact text.
    Remove {
        /// Patterns to drop.
        patterns: Vec<String>,
    },
    /// Say whether one path is ignored, and which pattern decided it.
    Check {
        /// Path relative to the repository root.
        path: String,
    },
}

/// The one family of `doctor` checks `--only` may select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DoctorOnly {
    /// Paths, legacy layout, lock and staging.
    Scan,
    /// Whether each pending source parses.
    Convert,
    /// Whether each distilled session record became a page.
    Session,
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
