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
//! - the ledgers are updated only after the file is in place: the `sources` row
//!   only gains `raw_key` / `converted_at`, and the `raws` row records the digest
//!   of the bytes actually written, so `scan_raw` does not report the conversion
//!   as pending work.
//!
//! Those three rules leave one window: the file is renamed into place before the
//! ledger writes, so a database failure would leave the two disagreeing. The
//! commit half therefore re-checks the source, the raw file and the ledger rows
//! right before writing, and puts the previous raw back when registration fails.
//! See `protection.rs` for the checks and `commit.rs` for the compensation.

mod commit;
mod protection;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{SecondsFormat, Utc};
use serde::Serialize;

use crate::config::Config;
use crate::hash;
use crate::parse::{self, Extracted, Source, Warning};
use crate::paths;
use crate::store::{RawRow, SourceRow, Store, StoreError};

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
    /// Whether the raw file was deliberately left untouched.
    pub skipped: bool,
    /// Why the conversion was skipped, when it was.
    pub skip_reason: Option<String>,
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
    /// The baseline moved between preparing and committing the conversion.
    #[error("{raw_path}: {reason}; nothing was written")]
    RawConflict { raw_path: String, reason: String },
    /// The ledger refused the conversion and the previous raw could not be put back.
    #[error(
        "{raw_path}: the conversion was not recorded ({cause}) and the previous file \
         could not be restored ({rollback}); {material}"
    )]
    RollbackFailed {
        raw_path: String,
        cause: String,
        rollback: String,
        material: String,
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
            ConvertError::RawConflict { .. } => "E_RAW_COLLISION",
            ConvertError::Parse(err) => err.code(),
            ConvertError::Hash(_) => "E_HASH",
            ConvertError::RollbackFailed { .. } => "E_IO",
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
///
/// A raw file whose frontmatter says `pin: true` is hand-maintained: it is
/// reported as skipped and both the file and the ledger are left alone. A raw
/// file that belongs to another source is an error, not a skip.
pub fn convert_one(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    key: &str,
) -> Result<ConvertOutcome> {
    let prepared = prepare(root, cfg, key)?;
    commit(store, prepared)
}

/// Everything a conversion needs to write, computed without touching the disk
/// or the database. `sync` runs this half in parallel.
#[derive(Debug)]
pub struct Prepared {
    /// What the caller reports once the write is committed.
    pub outcome: ConvertOutcome,
    /// The bytes and rows to persist; `None` for a pinned (skipped) raw file.
    pub write: Option<Ready>,
}

/// The write half of a conversion: the rendered document and its ledger rows.
///
/// Everything the commit needs to re-check is captured here, because `prepare`
/// and `commit` can run minutes apart and on different threads: the source file
/// re-reads must use the same size limit, and the raw comparison must use the
/// bytes seen at prepare, not whatever is on disk now.
#[derive(Debug)]
pub struct Ready {
    raw_path: PathBuf,
    document: String,
    source_row: SourceRow,
    source_path: PathBuf,
    source_ref: String,
    raw_row: RawRow,
    mtime_ns: Option<i64>,
    max_source_bytes: u64,
    baseline: protection::Baseline,
}

/// Reads, parses and renders one source. No file is written and no ledger row
/// changes here, so several of these can run at once.
pub fn prepare(root: &Path, cfg: &Config, key: &str) -> Result<Prepared> {
    let started = Instant::now();

    let source_path = paths::join_key(&root.join(&cfg.paths.source), key);
    let input = hash::read_source(&source_path, cfg.parse.max_source_bytes)?;
    let bytes = &input.bytes;

    let raw_rel = raw_key_for(key);
    let raw_key = slash_join(&cfg.paths.raw, &raw_rel);
    let raw_path = paths::join_key(root, &raw_key);
    let source_ref = slash_join(&cfg.paths.source, key);

    // The raw file is read once and kept: the commit compares against these
    // bytes, so a file edited in between is a conflict instead of a lost edit.
    let baseline = protection::inspect(&raw_path, &source_ref)?;
    if baseline.is_pinned() {
        let backend = baseline
            .pinned_parser()
            .unwrap_or_else(|| String::from("pinned"));
        return Ok(Prepared {
            outcome: ConvertOutcome {
                raw_path: raw_key,
                chars: 0,
                backend,
                duration_ms: started.elapsed().as_millis() as u64,
                warnings: Vec::new(),
                skipped: true,
                skip_reason: Some(String::from("pinned")),
            },
            write: None,
        });
    }
    if let Some(existing) = baseline.foreign_source() {
        return Err(ConvertError::RawCollision {
            raw_path: raw_key,
            existing: String::from(existing),
            key: source_ref,
        });
    }

    let (extracted, backend) = parse::parse_with(cfg, &Source::inline(key, bytes))?;
    let converted_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let document = render_raw(
        &source_ref,
        &hash::hash_bytes(bytes),
        &backend,
        &converted_at,
        &extracted,
    );
    // Digest the rendered document, not the source: `scan_raw` hashes the bytes
    // on disk, frontmatter included, and the two must agree.
    let raw_hash = hash::hash_bytes(document.as_bytes());

    let source_row = SourceRow {
        key: key.to_owned(),
        hash: input.hash,
        size: input.size,
        mtime: input.mtime,
        raw_key: Some(raw_key.clone()),
        converted_at: Some(converted_at.clone()),
    };
    // The `raws` key space is relative to `raw/`, like `scan_raw` reports it.
    let raw_row = RawRow {
        key: raw_rel,
        hash: raw_hash,
        source_key: String::from(key),
        converted_at: converted_at.clone(),
    };

    Ok(Prepared {
        outcome: ConvertOutcome {
            raw_path: raw_key,
            chars: extracted.markdown.chars().count(),
            backend,
            duration_ms: started.elapsed().as_millis() as u64,
            warnings: extracted.warnings,
            skipped: false,
            skip_reason: None,
        },
        write: Some(Ready {
            raw_path,
            document,
            source_row,
            source_path,
            source_ref,
            raw_row,
            mtime_ns: input.mtime_ns,
            max_source_bytes: cfg.parse.max_source_bytes,
            baseline,
        }),
    })
}

/// Writes the prepared document and its ledger rows. Database work stays on the
/// calling thread: `Store` is not `Sync`.
pub fn commit(store: &mut Store, prepared: Prepared) -> Result<ConvertOutcome> {
    let Some(ready) = prepared.write else {
        return Ok(prepared.outcome);
    };
    protection::recheck(store, &ready)?;
    commit::apply(store, &ready)?;
    Ok(prepared.outcome)
}

/// Reports whether a conversion of `key` would be refused, without doing it.
///
/// Dry runs need the answer a real run would give for a pinned, foreign or stale
/// raw file, but they must not parse, fetch or write: this reads the source to
/// digest it, reads the raw frontmatter, and compares both with the ledger. A
/// pinned raw is not an error — the real run reports it as a skip — while a
/// foreign raw is, because no run would overwrite it.
pub fn preflight(root: &Path, cfg: &Config, store: &Store, key: &str) -> Result<Option<String>> {
    protection::preflight(root, cfg, store, key)
}

/// The raw file name for `key`: the same relative path with a `.md` extension.
pub(crate) fn raw_key_for(key: &str) -> String {
    paths::to_slash(&Path::new(key).with_extension("md"))
}

/// Joins two `/`-separated repository paths, tolerating an empty base.
pub(crate) fn slash_join(base: &str, rel: &str) -> String {
    let base = base.trim_matches('/');
    if base.is_empty() {
        String::from(rel)
    } else {
        format!("{base}/{rel}")
    }
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
    render_warnings(&mut out, &extracted.warnings);
    out.push_str("pin: false\n---\n");
    out.push_str(&extracted.markdown);
    out
}

/// Renders `warnings` as a block sequence under a `warnings:` key.
///
/// An empty list is written as `[]` rather than omitted: a reader can then tell
/// "the backend reported nothing" from "this raw predates the field", and both
/// parse. The list is a sequence of mappings rather than flow style so a message
/// containing a comma or a brace changes nothing.
fn render_warnings(out: &mut String, warnings: &[Warning]) {
    if warnings.is_empty() {
        out.push_str("warnings: []\n");
        return;
    }
    out.push_str("warnings:\n");
    for warning in warnings {
        let _ = writeln!(out, "  - code: {}", yaml_scalar(&warning.code));
        let _ = writeln!(out, "    message: {}", yaml_scalar(&warning.message));
    }
}

/// Renders `value` as a YAML scalar that parses back to the same string.
///
/// The conservative choice is for `convert` to quote anything that could be read
/// as structure — `#` starts a comment and `:` separates a mapping — and to
/// escape the two characters that are special inside a double-quoted scalar.
/// A plain value such as `source/说明.md` or `builtin-text` stays readable.
pub(crate) fn yaml_scalar(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value.trim() != value
        || value
            .chars()
            .any(|c| c.is_control() || "#:\"'\\,[]{}&*!|>%@`".contains(c))
        || value.starts_with('-')
        || value.starts_with('?')
        // A bare `true` or `500` would come back as a boolean or a number, so
        // a value that looks like another YAML type is quoted as well.
        || looks_like_another_type(value);

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

/// Whether YAML would read `value` as something other than a string.
fn looks_like_another_type(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~" | ".nan" | ".inf" | "-.inf"
    ) {
        return true;
    }
    value.parse::<f64>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::protection as protect;
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
                "warnings",
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
            [
                "source",
                "source_hash",
                "parser",
                "converted_at",
                "warnings",
                "pin"
            ]
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
            assert_eq!(
                protect::unquote(&rendered),
                value,
                "rendered as {rendered:?}"
            );
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
            protect::frontmatter_field(&document, "source").as_deref(),
            Some("source/a b.md")
        );
        assert_eq!(
            protect::frontmatter_field(&document, "source_hash").as_deref(),
            Some("blake3:abc")
        );
        assert_eq!(
            protect::frontmatter_field(&document, "pin").as_deref(),
            Some("false")
        );
        assert_eq!(protect::frontmatter_field(&document, "asset"), None);
    }

    fn frontmatter(document: &str) -> serde_yaml::Value {
        let body = document.strip_prefix("---\n").expect("frontmatter opens");
        let end = body.find("\n---\n").expect("frontmatter closes");
        serde_yaml::from_str(&body[..end]).expect("frontmatter is YAML")
    }

    #[test]
    fn empty_warnings_render_as_an_empty_sequence() {
        let document = render_raw(
            "source/a.md",
            "blake3:abc",
            "builtin-text",
            "t",
            &extracted(false, false),
        );
        let value = frontmatter(&document);
        assert_eq!(
            value["warnings"].as_sequence().map(Vec::len),
            Some(0),
            "an empty list must be explicit, not missing"
        );
    }

    #[test]
    fn warnings_survive_the_yaml_round_trip() {
        let extracted = Extracted {
            markdown: String::from("正文\n"),
            warnings: vec![
                Warning::new("bad_utf8", "invalid byte at offset 3, replaced"),
                Warning::new("odd", "quote \" colon: dash - [x] #hash {}"),
                Warning::new("numeric", "500"),
                Warning::new("spaced", " leading and trailing "),
            ],
            asset: false,
            no_text_layer: false,
        };
        let document = render_raw("source/a.md", "blake3:abc", "builtin-html", "t", &extracted);
        let value = frontmatter(&document);
        let warnings = value["warnings"].as_sequence().expect("a sequence");
        let pairs: Vec<(&str, &str)> = warnings
            .iter()
            .map(|warning| {
                (
                    warning["code"].as_str().expect("code is a string"),
                    warning["message"].as_str().expect("message is a string"),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            [
                ("bad_utf8", "invalid byte at offset 3, replaced"),
                ("odd", "quote \" colon: dash - [x] #hash {}"),
                ("numeric", "500"),
                ("spaced", " leading and trailing "),
            ],
            "400 and 500 stay strings; punctuation changes nothing"
        );
    }

    #[test]
    fn a_raw_predating_the_warnings_field_still_reads() {
        let old = "---\nsource: source/a.md\nsource_hash: blake3:abc\nparser: builtin-text\nconverted_at: t\npin: false\n---\n正文\n";
        assert_eq!(
            protect::frontmatter_field(old, "source").as_deref(),
            Some("source/a.md")
        );
        assert_eq!(
            protect::frontmatter_field(old, "pin").as_deref(),
            Some("false")
        );
        assert_eq!(protect::frontmatter_field(old, "warnings"), None);
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
