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

/// The six subcommands. Everything else in the engine is internal.
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
    /// One write-line round: select sources, convert, open the round, write
    /// candidates, publish them.
    ///
    /// With no `KEYS` it takes every pending change in `source/`. `--dry-run`
    /// and `--key` preview one source. `--chunk N` and `--range N` pick one
    /// slice of a large source. Every round is recorded as a version before
    /// the candidates are written. Examples: `llm-wiki ingest --dry-run`,
    /// `llm-wiki ingest`, `llm-wiki ingest notes/a.md`.
    Ingest {
        /// Repo-relative source keys; empty means every pending change.
        keys: Vec<String>,
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
        /// Read a source above `parse.max_source_bytes` anyway.
        #[arg(long)]
        force_large: bool,
    },
    /// Close the open round: refresh its candidates, then publish them.
    ///
    /// `--confirm` publishes everything and ignores the brakes. `--auto` is the
    /// unattended form: a braked candidate is skipped with a warning and the
    /// run still exits 0. Exit 3 is reserved for the attended form when every
    /// candidate was held back. Examples: `llm-wiki submit`,
    /// `llm-wiki submit --confirm`, `llm-wiki submit --auto`.
    Submit {
        /// Publish every candidate, including the ones the brakes hold back.
        #[arg(long)]
        confirm: bool,
        /// Unattended: skip a braked candidate and report it instead of asking.
        #[arg(long, conflicts_with = "confirm")]
        auto: bool,
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
    /// Inspect the versions recorded automatically, and restore one.
    ///
    /// One snapshot is taken per `ingest` round and one per `submit`; no
    /// command creates one by hand. `restore` is the only way back from a bad
    /// publish, and it needs `--confirm` because it replaces the live tree.
    /// Examples: `llm-wiki version list`, `llm-wiki version diff`,
    /// `llm-wiki version restore v003 --confirm`.
    Version {
        #[command(subcommand)]
        command: VersionCommand,
    },
}

/// What a person does with versions: read the list, read one difference,
/// restore one.
#[derive(Debug, Subcommand)]
pub enum VersionCommand {
    /// Every snapshot, newest first.
    List,
    /// What a snapshot held, compared with the working tree now.
    Diff {
        /// Version name; the newest one when omitted.
        tag: Option<String>,
    },
    /// Put `pages/` and `raw/` back to what `tag` held.
    ///
    /// A tag that does not exist is rejected without touching the tree, and
    /// the restore is itself a version, so it can be undone the same way.
    Restore {
        /// Version name to restore.
        tag: String,
        /// Required: a restore replaces the live tree.
        #[arg(long)]
        confirm: bool,
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
