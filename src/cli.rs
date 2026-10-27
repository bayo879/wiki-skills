//! The command-line surface: global flags and the subcommand table.
//!
//! Later tasks add their subcommands to [`Command`]; nothing here does work.

use clap::{Parser, Subcommand};

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
    /// Write the contract files and create the engine directories.
    Init {
        /// Rewrite `wiki.toml` and `SCHEMA.md` even if they already exist.
        #[arg(long)]
        force: bool,
    },
    /// Compare `source/` with the ledger and report what changed.
    Scan,
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
    },
    /// Drive one ingest round: open it, close it, or drop it.
    Ingest {
        #[command(subcommand)]
        command: IngestCommand,
    },
    /// Audit the wiki against the twelve lint rules.
    Lint {
        /// Fail on warnings as well as errors.
        #[arg(long)]
        strict: bool,
        /// Repair the defects that need no judgement, in place.
        #[arg(long)]
        fix: bool,
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
