//! `source/` → `raw/`: attach traceability metadata to parsed markdown.
//!
//! The raw layer is where the two source layers meet. [`convert_one`] reads one
//! source key, runs it through the parsing registry, and writes
//! `raw/<key without extension>.md` with a fixed-order frontmatter that records
//! where the text came from and which backend produced it. The body comes from
//! the backend unchanged; this module only wraps it.
//!
//! Three rules keep the layer trustworthy:
//!
//! - the file is written through a temporary sibling and renamed into place, so
//!   a crash cannot leave a half-written raw file that `ingest` would accept;
//! - an existing raw file is only overwritten when its `source` field names the
//!   same source, so `a.md` and `a.txt` cannot silently fight over `raw/a.md`;
//! - the ledger is updated only after the file is in place, and only its
//!   `raw_key` / `converted_at` columns change.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{SecondsFormat, Utc};
use serde::Serialize;

use crate::config::Config;
use crate::hash;
use crate::parse::{self, Extracted, Source, Warning};
use crate::paths;
use crate::store::{SourceRow, Store, StoreError};

/// What one conversion produced, as reported by `convert` and `sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConvertOutcome {
    /// Repository-relative path of the raw file, such as `raw/说明.md`.
    pub raw_path: String,
    /// Character count of the markdown body, excluding the frontmatter.
    pub chars: usize,
    /// Id of the backend that produced the markdown.
    pub backend: String,
    /// Wall-clock time spent on the whole conversion, in milliseconds.
    pub duration_ms: u64,
    /// Non-fatal problems the backend reported.
    pub warnings: Vec<Warning>,
}

/// Why a source could not be converted.
#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    /// The source could not be read, or the raw file could not be written.
    #[error("cannot access {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Another source already owns the raw path this conversion would write.
    #[error(
        "{raw_path} already holds the output of {existing}, not {key}; \
         refusing to overwrite it"
    )]
    RawCollision {
        raw_path: String,
        existing: String,
        key: String,
    },
    /// No backend claimed the extension.
    #[error(transparent)]
    Parse(#[from] parse::ParseError),
    /// The source could not be digested for a new ledger row.
    #[error(transparent)]
    Hash(#[from] hash::HashError),
    /// The ledger could not be read or written.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl ConvertError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            ConvertError::Io { .. } => "E_IO",
            ConvertError::RawCollision { .. } => "E_RAW_COLLISION",
            ConvertError::Parse(err) => err.code(),
            ConvertError::Hash(_) => "E_HASH",
            ConvertError::Store(_) => "E_DB",
        }
    }
}

/// Result of the conversion helpers.
pub type Result<T> = std::result::Result<T, ConvertError>;

/// Converts the source `key` (relative to `source/`) into `raw/*.md`.
///
/// `cfg.paths.source` and `cfg.paths.raw` decide the two layers, so a repository
/// with renamed directories works unchanged. The ledger is read for the rest of
/// the row and written back with a new `raw_key` and `converted_at`; a source
/// the ledger does not know yet (a direct `convert --key`, or a conversion that
/// runs before a scan) gets a row built from the file itself.
pub fn convert_one(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    key: &str,
) -> Result<ConvertOutcome> {
    let started = Instant::now();

    let source_path = paths::join_key(&root.join(&cfg.paths.source), key);
    let bytes = std::fs::read(&source_path).map_err(|source| ConvertError::Io {
        path: source_path.clone(),
        source,
    })?;

    let raw_rel = raw_key_for(key);
    let raw_key = slash_join(&cfg.paths.raw, &raw_rel);
    let raw_path = paths::join_key(root, &raw_key);
    let source_ref = slash_join(&cfg.paths.source, key);

    ensure_no_other_owner(&raw_path, &raw_key, &source_ref)?;

    let (extracted, backend) = parse::parse_with(cfg, &Source::inline(key, &bytes))?;
    let converted_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let document = render_raw(
        &source_ref,
        &hash::hash_bytes(&bytes),
        &backend,
        &converted_at,
        &extracted,
    );
    write_atomic(&raw_path, document.as_bytes())?;

    record_conversion(store, cfg, key, &source_path, &raw_key, &converted_at)?;

    Ok(ConvertOutcome {
        raw_path: raw_key,
        chars: extracted.markdown.chars().count(),
        backend,
        duration_ms: started.elapsed().as_millis() as u64,
        warnings: extracted.warnings,
    })
}

/// The raw file name for `key`: the same relative path with a `.md` extension.
fn raw_key_for(key: &str) -> String {
    paths::to_slash(&Path::new(key).with_extension("md"))
}

/// Joins two `/`-separated repository paths, tolerating an empty base.
fn slash_join(base: &str, rel: &str) -> String {
    let base = base.trim_matches('/');
    if base.is_empty() {
        String::from(rel)
    } else {
        format!("{base}/{rel}")
    }
}

/// Refuses to write `raw_path` when it already belongs to a different source.
///
/// The owner is whatever the existing file's `source` field says, not the
/// ledger: a raw file is the authority on where it came from, and a file with no
/// readable `source` cannot prove it is ours, so it is treated as a collision
/// rather than being overwritten.
fn ensure_no_other_owner(raw_path: &Path, raw_key: &str, source_ref: &str) -> Result<()> {
    if !raw_path.exists() {
        return Ok(());
    }
    let owner = read_field(raw_path, SOURCE_FIELD)?;
    match owner {
        Some(ref found) if found == source_ref => Ok(()),
        Some(found) => Err(ConvertError::RawCollision {
            raw_path: String::from(raw_key),
            existing: found,
            key: String::from(source_ref),
        }),
        None => Err(ConvertError::RawCollision {
            raw_path: String::from(raw_key),
            existing: String::from("an unknown source (no `source` field)"),
            key: String::from(source_ref),
        }),
    }
}

/// Frontmatter key that records the source path.
const SOURCE_FIELD: &str = "source";

/// Writes `body` to `path` through a temporary sibling plus a rename.
///
/// A rename is atomic on the same filesystem, so readers (and `ingest`) never
/// observe a partially written document. The temporary name is dot-prefixed and
/// process-tagged so a leftover from a killed run is invisible to the walkers.
fn write_atomic(path: &Path, body: &[u8]) -> Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent).map_err(|source| ConvertError::Io {
        path: parent.clone(),
        source,
    })?;

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("raw.md"));
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));

    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(body)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(source) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(ConvertError::Io { path: tmp, source });
    }
    if let Err(source) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ConvertError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

/// Renders the raw document: fixed-order frontmatter, then the markdown body.
///
/// `asset` and `no_text_layer` appear only when true, because their absence is
/// the normal case and `pin: false` is the one boolean that is always written —
/// it tells a human editor which flag to flip before hand-editing the body.
fn render_raw(
    source: &str,
    source_hash: &str,
    parser: &str,
    converted_at: &str,
    extracted: &Extracted,
) -> String {
    let mut out = String::from("---\n");
    let _ = writeln!(out, "source: {}", yaml_scalar(source));
    let _ = writeln!(out, "source_hash: {}", yaml_scalar(source_hash));
    let _ = writeln!(out, "parser: {}", yaml_scalar(parser));
    let _ = writeln!(out, "converted_at: {}", yaml_scalar(converted_at));
    if extracted.asset {
        out.push_str("asset: true\n");
    }
    if extracted.no_text_layer {
        out.push_str("no_text_layer: true\n");
    }
    out.push_str("pin: false\n---\n");
    out.push_str(&extracted.markdown);
    out
}

/// Renders `value` as a YAML scalar that parses back to the same string.
///
/// The conservative choice is for `convert` to quote anything that could be read
/// as structure — `#` starts a comment and `:` separates a mapping — and to
/// escape the two characters that are special inside a double-quoted scalar.
/// A plain value such as `source/说明.md` or `builtin-text` stays readable.
fn yaml_scalar(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value.trim() != value
        || value
            .chars()
            .any(|c| c.is_control() || "#:\"'\\,[]{}&*!|>%@`".contains(c))
        || value.starts_with('-')
        || value.starts_with('?');

    if !needs_quotes {
        return String::from(value);
    }

    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Reads one leading-frontmatter field from a file, undoing [`yaml_scalar`].
///
/// The bytes are decoded lossily: an unreadable *encoding* must not turn into an
/// I/O error here, because the caller's response to an unrecognised owner is the
/// same either way — refuse to overwrite.
fn read_field(path: &Path, name: &str) -> Result<Option<String>> {
    let bytes = std::fs::read(path).map_err(|source| ConvertError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(frontmatter_field(&String::from_utf8_lossy(&bytes), name))
}

/// The value of `name` in the first frontmatter block of `text`.
///
/// Only top-level `key: value` lines are understood, which is all this module
/// writes; anything else (indented children, block scalars, single quotes) is
/// reported as absent, and the caller treats that as "owner unknown".
fn frontmatter_field(text: &str, name: &str) -> Option<String> {
    let body = text.strip_prefix("---\n")?;
    for line in body.split_inclusive('\n') {
        let line = line.trim_end_matches('\n');
        if line.trim_end() == "---" {
            return None;
        }
        let Some(rest) = line
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix(':'))
        else {
            continue;
        };
        return Some(unquote(rest.trim()));
    }
    None
}

/// Undoes the double-quote escaping of [`yaml_scalar`].
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
        return String::from(value);
    }
    let inner = &value[1..value.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Writes `raw_key` and `converted_at` into the ledger, keeping every other
/// column as recorded.
///
/// A key the ledger has never seen gets a row derived from the file on disk, so
/// `convert --key` works before the first scan. The digest is chosen exactly the
/// way `scan` chooses it (full up to `parse.chunk_threshold`, sampled above), so
/// the next scan sees an unchanged source instead of a spurious `changed`.
fn record_conversion(
    store: &mut Store,
    cfg: &Config,
    key: &str,
    source_path: &Path,
    raw_key: &str,
    converted_at: &str,
) -> Result<()> {
    let mut row = match store.get_source(key)? {
        Some(row) => row,
        None => {
            let fingerprint = hash::fingerprint(source_path)?;
            let digest = if fingerprint.size <= cfg.parse.chunk_threshold {
                hash::hash_file(source_path, true)?
            } else {
                fingerprint.sample.clone()
            };
            SourceRow {
                key: String::from(key),
                hash: digest,
                size: fingerprint.size as i64,
                mtime: fingerprint.mtime,
                raw_key: None,
                converted_at: None,
            }
        }
    };
    row.raw_key = Some(String::from(raw_key));
    row.converted_at = Some(String::from(converted_at));
    store.upsert_source(&row)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extracted(asset: bool, no_text_layer: bool) -> Extracted {
        Extracted {
            markdown: String::from("正文\n"),
            warnings: Vec::new(),
            asset,
            no_text_layer,
        }
    }

    fn keys(document: &str) -> Vec<&str> {
        document
            .lines()
            .skip(1)
            .take_while(|line| *line != "---")
            .map(|line| line.split(':').next().unwrap())
            .collect()
    }

    #[test]
    fn the_full_key_order_is_fixed() {
        let document = render_raw(
            "source/a.pdf",
            "blake3:abc",
            "builtin-pdf",
            "2026-10-27T10:00:00Z",
            &extracted(true, true),
        );
        assert_eq!(
            keys(&document),
            [
                "source",
                "source_hash",
                "parser",
                "converted_at",
                "asset",
                "no_text_layer",
                "pin"
            ]
        );
    }

    #[test]
    fn degradation_flags_appear_only_when_true() {
        let plain = render_raw(
            "source/a.md",
            "blake3:abc",
            "builtin-text",
            "t",
            &extracted(false, false),
        );
        assert_eq!(
            keys(&plain),
            ["source", "source_hash", "parser", "converted_at", "pin"]
        );
        assert!(plain.contains("pin: false\n---\n正文\n"));
    }

    #[test]
    fn scalars_round_trip_through_quoting() {
        for value in [
            "source/说明.md",
            "source/说明#1.md",
            "source/a b\"c:d.md",
            "a\\b",
            " lead",
            "trail ",
            "",
            "- dash",
            "? question",
        ] {
            let rendered = yaml_scalar(value);
            assert_eq!(unquote(&rendered), value, "rendered as {rendered:?}");
        }
    }

    #[test]
    fn every_key_is_discoverable_in_the_rendered_document() {
        let document = render_raw(
            "source/a b.md",
            "blake3:abc",
            "builtin-text",
            "2026-10-27T10:00:00Z",
            &extracted(false, false),
        );
        assert_eq!(
            frontmatter_field(&document, "source").as_deref(),
            Some("source/a b.md")
        );
        assert_eq!(
            frontmatter_field(&document, "source_hash").as_deref(),
            Some("blake3:abc")
        );
        assert_eq!(
            frontmatter_field(&document, "pin").as_deref(),
            Some("false")
        );
        assert_eq!(frontmatter_field(&document, "asset"), None);
    }

    #[test]
    fn raw_keys_drop_the_extension_and_keep_the_directory() {
        assert_eq!(raw_key_for("a.md"), "a.md");
        assert_eq!(raw_key_for("a.txt"), "a.md");
        assert_eq!(raw_key_for("a.markdown"), "a.md");
        assert_eq!(raw_key_for("子目录/说明.md"), "子目录/说明.md");
        assert_eq!(raw_key_for("d/a.tar.gz"), "d/a.tar.md");
        assert_eq!(raw_key_for("noext"), "noext.md");
    }
}
