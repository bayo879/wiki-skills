//! Read one file inside the wiki engine by an internal reference.
//!
//! The command exists so an author never has to know the engine's layout. A
//! reference is either an alias (`schema`), a typed key (`page:concepts/x.md`,
//! `raw:notes/a.md`), or an engine-relative path (`pages/concepts/x.md`,
//! `.wiki/raw/notes/a.md`). Resolution always stays under the configured engine
//! directory; absolute paths and `..` components are rejected rather than
//! normalised away.

use std::path::{Path, PathBuf};

#[cfg(test)]
use std::path::Component;

use crate::config::Config;
use crate::paths;

/// Why a reference could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The reference is not a path inside the wiki engine.
    #[error("reference {0:?} is not an internal wiki file")]
    BadRef(String),
    /// The file does not exist, or is not a regular file.
    #[error("no wiki file at {0}")]
    NotFound(String),
    /// The bytes are not UTF-8 text, so they cannot be printed as a page.
    #[error("{0} is not UTF-8 text")]
    NotText(String),
    /// The filesystem refused the read.
    #[error("cannot read {0}: {1}")]
    Io(String, std::io::Error),
}

impl ReadError {
    /// Stable code for the JSON envelope.
    pub fn code(&self) -> &'static str {
        match self {
            ReadError::BadRef(_) => "E_BAD_REF",
            ReadError::NotFound(_) => "E_NO_FILE",
            ReadError::NotText(_) => "E_NOT_TEXT",
            ReadError::Io(_, _) => "E_IO",
        }
    }
}

/// Result of a read.
pub type Result<T> = std::result::Result<T, ReadError>;

/// Resolves `reference` to a file under the engine directory.
///
/// The returned path is only used internally; error messages carry the
/// reference, not the host path.
pub fn resolve(root: &Path, cfg: &Config, reference: &str) -> Result<PathBuf> {
    let reference = reference.trim().trim_start_matches("./");
    if reference.is_empty() {
        return Err(ReadError::BadRef(reference.to_string()));
    }
    if Path::new(reference).is_absolute() {
        return Err(ReadError::BadRef(reference.to_string()));
    }
    if reference
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(ReadError::BadRef(reference.to_string()));
    }

    let engine = cfg.paths.engine.trim_end_matches('/');
    let candidates = candidates(cfg, reference);
    let engine_root = root.join(engine);

    for relative in candidates {
        // Every candidate is already engine-relative. `join_key` is the only
        // joiner used elsewhere; keep the same Unicode/NFC discipline here.
        let path = paths::join_key(root, &relative);
        if !path.starts_with(&engine_root) {
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let canonical = match std::fs::canonicalize(&path) {
            Ok(path) => path,
            Err(error) => {
                return Err(ReadError::Io(reference.to_string(), error));
            }
        };
        let canonical_engine = match std::fs::canonicalize(&engine_root) {
            Ok(path) => path,
            Err(error) => return Err(ReadError::Io(reference.to_string(), error)),
        };
        if !canonical.starts_with(&canonical_engine) {
            return Err(ReadError::BadRef(reference.to_string()));
        }
        return Ok(canonical);
    }

    Err(ReadError::NotFound(reference.to_string()))
}

/// Candidate engine-relative paths, most specific first.
fn candidates(cfg: &Config, reference: &str) -> Vec<String> {
    let engine = cfg.paths.engine.trim_end_matches('/');
    let wiki = cfg.paths.wiki.trim_end_matches('/');
    let raw = cfg.paths.raw.trim_end_matches('/');

    if reference == "schema" {
        return vec![format!("{engine}/SCHEMA.md")];
    }
    if reference == "index" {
        return vec![format!("{wiki}/index.md")];
    }
    if let Some(key) = reference.strip_prefix("page:") {
        return vec![format!("{wiki}/{key}")];
    }
    if let Some(key) = reference.strip_prefix("raw:") {
        return vec![format!("{raw}/{key}")];
    }
    if reference.starts_with(&format!("{engine}/")) {
        return vec![reference.to_string()];
    }
    if let Some(key) = reference.strip_prefix("pages/") {
        return vec![format!("{wiki}/{key}")];
    }
    if let Some(key) = reference.strip_prefix("raw/") {
        return vec![format!("{raw}/{key}")];
    }
    // A bare key is accepted for convenience, but only as a page or raw; the
    // engine's own files must be named with the `schema` alias.
    vec![format!("{wiki}/{reference}"), format!("{raw}/{reference}")]
}

/// Reads one wiki file and returns its text.
pub fn read_text(root: &Path, cfg: &Config, reference: &str) -> Result<String> {
    let path = resolve(root, cfg, reference)?;
    let bytes =
        std::fs::read(&path).map_err(|error| ReadError::Io(reference.to_string(), error))?;
    String::from_utf8(bytes).map_err(|_| ReadError::NotText(reference.to_string()))
}

/// Most lines `read` prints in one call.
pub const MAX_LINES: usize = 2000;

/// One window of a text file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The requested lines, with their original line terminators.
    pub content: String,
    /// 1-based first line, or 0 for an empty file.
    pub line_start: usize,
    /// 1-based last line, or 0 for an empty file.
    pub line_end: usize,
    /// Number of lines in the whole file.
    pub total_lines: usize,
    /// Whether lines after `line_end` exist.
    pub truncated: bool,
}

/// Selects `[offset, offset + limit)` (1-based) from `text`.
///
/// An empty window — empty file, or `offset` past the last line — reports
/// `line_start == line_end == 0` rather than an inverted range; the caller
/// carries the requested offset in its payload. The limit is clamped by the
/// caller, not here, so this stays a pure slice function.
pub fn page(text: &str, offset: usize, limit: usize) -> Page {
    let lines: Vec<&str> = if text.is_empty() {
        Vec::new()
    } else {
        text.split_inclusive('\n').collect()
    };
    let total_lines = lines.len();
    let start = offset.saturating_sub(1).min(total_lines);
    let end = start.saturating_add(limit).min(total_lines);
    let content = lines[start..end].concat();
    // An empty window (empty file, or an offset past the last line) reports
    // `0-0` rather than an inverted range; the requested offset lives in the
    // CLI payload, so no information is lost.
    let empty = start >= total_lines;
    Page {
        content,
        line_start: if empty { 0 } else { start + 1 },
        line_end: if empty { 0 } else { end },
        total_lines,
        truncated: end < total_lines,
    }
}

/// Whether every component is a plain name, used by tests to pin the rule.
#[cfg(test)]
fn is_plain(reference: &str) -> bool {
    Path::new(reference)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn repo() -> (TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        crate::init::run(dir.path(), false).unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        (dir, cfg)
    }

    #[test]
    fn typed_and_bare_references_reach_pages_and_raw() {
        let (dir, cfg) = repo();
        std::fs::create_dir_all(dir.path().join(".wiki/pages/concept")).unwrap();
        std::fs::write(dir.path().join(".wiki/pages/concept/a.md"), "page\n").unwrap();
        std::fs::create_dir_all(dir.path().join(".wiki/raw")).unwrap();
        std::fs::write(dir.path().join(".wiki/raw/a.md"), "raw\n").unwrap();

        assert_eq!(
            read_text(dir.path(), &cfg, "page:concept/a.md").unwrap(),
            "page\n"
        );
        assert_eq!(read_text(dir.path(), &cfg, "raw:a.md").unwrap(), "raw\n");
        assert_eq!(
            read_text(dir.path(), &cfg, "concept/a.md").unwrap(),
            "page\n"
        );
        assert_eq!(
            read_text(dir.path(), &cfg, ".wiki/raw/a.md").unwrap(),
            "raw\n"
        );
    }

    #[test]
    fn schema_alias_reads_the_contract() {
        let (dir, cfg) = repo();
        let text = read_text(dir.path(), &cfg, "schema").unwrap();
        assert!(!text.trim().is_empty(), "{text}");
    }

    #[test]
    fn absolute_parent_and_empty_references_are_refused() {
        let (dir, cfg) = repo();
        for reference in ["", "/etc/passwd", "../a.md", "pages/../a.md", "."] {
            assert!(
                matches!(
                    resolve(dir.path(), &cfg, reference),
                    Err(ReadError::BadRef(_))
                ),
                "{reference}"
            );
        }
    }

    #[test]
    fn a_missing_file_is_not_found_and_names_the_reference() {
        let (dir, cfg) = repo();
        let err = resolve(dir.path(), &cfg, "page:nope.md").unwrap_err();
        assert!(matches!(err, ReadError::NotFound(ref name) if name == "page:nope.md"));
    }

    #[test]
    fn only_plain_components_pass_the_guard() {
        assert!(is_plain("pages/a.md"));
        assert!(!is_plain("pages/../a.md"));
    }

    #[test]
    fn a_page_is_a_window_with_original_line_endings() {
        let text = "one\ntwo\nthree\nfour";
        let first = page(text, 1, 2);
        assert_eq!(first.content, "one\ntwo\n");
        assert_eq!(
            (first.line_start, first.line_end, first.total_lines),
            (1, 2, 4)
        );
        assert!(first.truncated);

        let tail = page(text, 3, 10);
        assert_eq!(tail.content, "three\nfour");
        assert_eq!((tail.line_start, tail.line_end), (3, 4));
        assert!(!tail.truncated);

        let past = page(text, 9, 10);
        assert_eq!(past.content, "");
        assert_eq!((past.line_start, past.line_end), (0, 0));
        assert!(!past.truncated);

        let empty = page("", 1, 10);
        assert_eq!(empty.content, "");
        assert_eq!(
            (empty.line_start, empty.line_end, empty.total_lines),
            (0, 0, 0)
        );
    }
}
