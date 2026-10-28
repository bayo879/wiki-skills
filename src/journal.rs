//! The engine's journal: one JSON object per line in `.wiki/journal.jsonl`.
//!
//! The journal is an audit trail, not wiki content. It is never versioned, so
//! `rollback` cannot rewrite what a human decided; it is never indexed, so it
//! cannot compete with pages in retrieval; and because a line is JSON, a
//! multi-line reason survives intact instead of breaking a heading format.
//!
//! One writer is assumed, like the rest of the engine: `approve`, `reject`,
//! `defer`, `instruct` and `rollback` are the only callers, and they are serial.

use chrono::{Local, SecondsFormat};
use serde::Serialize;
use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// File name, relative to the engine directory.
pub const JOURNAL_FILE: &str = "journal.jsonl";

/// One line of the journal.
#[derive(Debug, Serialize)]
pub struct Entry<'a> {
    /// When the decision was taken, RFC 3339 with the local offset.
    pub at: String,
    /// Short verb: `approve`, `reject`, `defer`, `instruct`, `rollback`.
    pub action: &'a str,
    /// Version the action produced, when it produced one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<&'a str>,
    /// Why, or what. Free text, newlines included.
    pub detail: &'a str,
}

impl Entry<'_> {
    /// Renders the line without its trailing newline.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| String::from("{}"))
    }
}

/// Failure while appending to the journal.
#[derive(Debug)]
pub enum JournalError {
    /// The file or its directory could not be written.
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying OS error.
        source: std::io::Error,
    },
}

impl JournalError {
    /// Stable error code for the CLI envelope.
    pub fn code(&self) -> &'static str {
        "E_IO"
    }
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalError::Io { path, source } => {
                write!(f, "cannot append to {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for JournalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JournalError::Io { source, .. } => Some(source),
        }
    }
}

/// The journal path for a repository root.
pub fn path(root: &Path) -> PathBuf {
    root.join(crate::config::ENGINE_DIR).join(JOURNAL_FILE)
}

/// Appends one decision to the journal.
pub fn append(
    root: &Path,
    action: &str,
    version: Option<&str>,
    detail: &str,
) -> Result<(), JournalError> {
    let path = path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| JournalError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let entry = Entry {
        at: Local::now().to_rfc3339_opts(SecondsFormat::Secs, false),
        action,
        version,
        detail,
    };
    let mut line = entry.to_line();
    line.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| JournalError::Io {
            path: path.clone(),
            source,
        })?;
    file.write_all(line.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|source| JournalError::Io { path, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn a_multi_line_detail_survives_as_one_line_of_json() {
        let d = tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join(crate::config::ENGINE_DIR)).unwrap();
        append(root, "reject", None, "第一行\n第二行").unwrap();
        let text = std::fs::read_to_string(path(root)).unwrap();
        assert_eq!(text.lines().count(), 1, "one decision is one line: {text}");
        let json: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(json["action"], "reject");
        assert_eq!(json["detail"], "第一行\n第二行");
        assert!(json.get("version").is_none(), "no version, no key");
    }

    #[test]
    fn entries_append_in_order() {
        let d = tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join(crate::config::ENGINE_DIR)).unwrap();
        append(root, "approve", Some("v001"), "").unwrap();
        append(root, "defer", None, "later").unwrap();
        let text = std::fs::read_to_string(path(root)).unwrap();
        let actions: Vec<String> = text
            .lines()
            .map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                value["action"].as_str().unwrap().to_string()
            })
            .collect();
        assert_eq!(actions, vec!["approve", "defer"]);
    }
}
