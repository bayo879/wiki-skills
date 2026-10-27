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
}
