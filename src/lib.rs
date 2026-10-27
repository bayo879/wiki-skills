//! `llm-wiki` library interface.
//!
//! Everything except argument parsing lives here so that both the binary and the
//! integration tests exercise the same code. `main.rs` keeps only the clap
//! wiring and dispatch.

pub mod archive;
pub mod chunk;
pub mod cli;
pub mod config;
pub mod convert;
pub mod doctor;
pub mod graph;
pub mod hash;
pub mod ingest;
pub mod init;
pub mod lint;
pub mod lock;
pub mod page;
pub mod parse;
pub mod paths;
pub mod query;
pub mod render;
pub mod report;
pub mod scan;
pub mod staging;
pub mod store;
pub mod sync;
pub mod version;
