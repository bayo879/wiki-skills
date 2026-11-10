//! The command-line surface: global flags and the subcommand table.
//!
//! Later tasks add their subcommands to [`Command`]; nothing here does work.

use clap::{Parser, Subcommand, ValueEnum};

/// Top-level invocation: `llm-wiki [--json] <command>`.
#[derive(Debug, Parser)]
#[command(
    name = "llm-wiki",
    about = "Maintain a Karpathy-style LLM wiki",
    version,
    // `help` is clap's subcommand; the engine keeps no verb of its own for it.
    disable_help_subcommand = false
)]
pub struct Cli {
    /// Write one JSON document to stdout instead of human-readable text.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

/// The public commands. Everything else in the engine is internal.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check the repository's health; `--fix` repairs the unambiguous defects.
    ///
    /// Without `--fix` it only reads. The exit code is 1 when a check fails.
    /// `--fix` bootstraps an uninitialised repository, then runs `lint --fix`
    /// and rebuilds the search index before reporting.
    ///
    /// Examples: `llm-wiki doctor`, `llm-wiki doctor --fix`.
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
    /// Convert sources and return unfinished work for direct writing to live pages.
    ///
    /// `--dry-run` previews changes; `--key` previews parsed source content.
    Ingest {
        /// Repo-relative source keys; empty means every pending change.
        keys: Vec<String>,
        /// Run one default round for the `wiki自动摄入` trigger. Manual options
        /// cannot be combined with it.
        #[arg(long)]
        auto: bool,
        /// Report what a round would do and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Convert and ingest exactly this source, converted before or not.
        #[arg(long)]
        key: Option<String>,
        /// Preview one chunk of the source named by `--key`.
        #[arg(long, conflicts_with = "range")]
        chunk: Option<usize>,
        /// Print one slice of the source named by `--key`: `0` the head, `N`
        /// the N-th following chunk.
        #[arg(long, value_name = "INDEX", conflicts_with = "chunk")]
        range: Option<String>,
        /// Legacy preview flag; never bypasses `parse.max_source_bytes`.
        #[arg(long)]
        force_large: bool,
        /// Convert now, even if a source was modified within `[ingest].settle_seconds`.
        #[arg(long)]
        force: bool,
    },
    /// Print one wiki file through an internal reference.
    ///
    /// References are engine-relative (`pages/...`, `raw/...`, `.wiki/...`) or
    /// aliases (`schema`, `index`, `page:<key>`, `raw:<key>`). Long files are
    /// paginated: `--offset` is the 1-based first line, `--limit` defaults to
    /// 2000 lines and is capped there.
    ///
    /// Examples: `llm-wiki read schema`, `llm-wiki read .wiki/pages/x.md`,
    /// `llm-wiki read .wiki/raw/a.md --offset 201`.
    Read {
        /// Internal reference returned by ingest, or `schema`/`index`.
        reference: String,
        /// 1-based first line to print.
        #[arg(long, default_value_t = 1)]
        offset: usize,
        /// Maximum lines to print; capped at 2000.
        #[arg(long, default_value_t = 2000)]
        limit: usize,
    },
    /// Work the ingest task tree.
    ///
    /// `done` closes the task you were handed and prints the next one;
    /// `status` shows the tree without advancing it; `retry` is the human-only
    /// exit for a failed or blocked task.
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },
    /// Manage the ignore list that keeps sources out of the wiki.
    ///
    /// With no action it lists the patterns. `--add`/`--remove` rewrite the
    /// file; `--check` explains one source key without changing anything.
    /// Examples: `llm-wiki ignore`, `llm-wiki ignore --add drafts/**`,
    /// `llm-wiki ignore --check drafts/a.md`.
    Ignore {
        /// Add these glob patterns.
        #[arg(long, value_name = "PATTERN", num_args = 1.., conflicts_with_all = ["remove", "check"])]
        add: Vec<String>,
        /// Remove these glob patterns.
        #[arg(long, value_name = "PATTERN", num_args = 1.., conflicts_with = "check")]
        remove: Vec<String>,
        /// Explain whether one source key is ignored, and by which pattern.
        #[arg(long, value_name = "KEY")]
        check: Option<String>,
    },
    /// Search the wiki.
    ///
    /// `--graph` prints the `[[wikilink]]` structure instead of searching;
    /// `--no-archive` hides pages distilled from sources under
    /// `[ingest] session_dir`. With no `TEXT`, `--graph` is the only useful
    /// form. Examples: `llm-wiki query 订单超时`,
    /// `llm-wiki query 订单超时 --no-archive`, `llm-wiki query --graph`.
    Query {
        /// Words to search for; omit together with `--graph`.
        text: Option<String>,
        /// Return at most this many hits.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Hide pages distilled from sources under `[ingest] session_dir`.
        #[arg(long)]
        no_archive: bool,
        /// Print the `[[wikilink]]` structure: degrees, edges, orphans, broken links.
        #[arg(long)]
        graph: bool,
    },
}

/// Actions on the ingest task tree.
#[derive(Debug, Subcommand)]
pub enum TaskAction {
    /// Close one task and print the next one.
    ///
    /// A `page` task needs `--payload <file>` with the page content; a
    /// `breakdown` task needs `--leaves <file>` with the leaf list.
    Done {
        /// Task id from `next_task` or `task status`.
        id: i64,
        /// Page content JSON for a `page` task.
        #[arg(long, value_name = "FILE")]
        payload: Option<std::path::PathBuf>,
        /// Leaf list JSON for a `breakdown` task.
        #[arg(long, value_name = "FILE")]
        leaves: Option<std::path::PathBuf>,
    },
    /// Show the tree without advancing it.
    Status,
    /// Reset a failed or blocked task to pending (human-only).
    Retry {
        /// Task id to reset.
        id: i64,
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
