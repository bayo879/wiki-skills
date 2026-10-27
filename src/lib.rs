//! `llm-wiki` library interface.
//!
//! Everything except argument parsing lives here so that both the binary and the
//! integration tests exercise the same code. `main.rs` keeps only the clap
//! wiring and dispatch.

pub mod cli;
pub mod config;
pub mod hash;
pub mod init;
pub mod paths;
pub mod report;
pub mod scan;
pub mod store;
