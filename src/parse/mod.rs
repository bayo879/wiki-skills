//! The parsing layer: every backend turns one source file into markdown.
//!
//! A [`Parser`] never knows about `raw/`: it sees the source bytes and returns an
//! [`Extracted`] value. `convert` is what attaches the traceability frontmatter
//! and writes the result; `parse` runs the same backends without touching disk.
//!
//! Backends are registered per config in [`registry`], in the order they should
//! be tried, and [`parse_with`] picks the first one that claims the extension.
//! Backend ids are stable strings because they end up in the raw frontmatter and
//! in command output.

pub mod asset;
pub mod external;
pub mod html;
pub mod pdf;
pub mod text;

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::{Config, Primary};

/// One source handed to a backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Path identity key relative to `source/`, `/`-separated and NFC.
    pub key: String,
    /// Lowercase extension without the dot; empty when the name has none.
    pub ext: String,
    /// The raw file content, so a backend never needs the filesystem.
    pub bytes: Vec<u8>,
}

impl Source {
    /// Builds a source from an in-memory buffer, deriving `ext` from `key`.
    ///
    /// The extension is lowercased so backends can compare it without repeating
    /// the case folding; `key` itself keeps its original spelling because it is
    /// what the raw frontmatter records.
    pub fn inline(key: &str, bytes: &[u8]) -> Source {
        Source {
            key: String::from(key),
            ext: extension_of(key),
            bytes: bytes.to_vec(),
        }
    }

    /// Reads `path` as one source, naming it after the file itself.
    ///
    /// The key is the file name, not the path from the working directory: a
    /// source is identified by what it is called, and the directory it happened
    /// to sit in is not part of its identity. The extension is derived from that
    /// same name, so `Image.PNG` is still claimed by the asset backend.
    pub fn path(path: &Path) -> Result<Source> {
        let bytes = std::fs::read(path).map_err(|err| ParseError::Io {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
        let key = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Source {
            ext: extension_of(&key),
            key,
            bytes,
        })
    }
}

/// The extension of `key` without its dot, lowercased.
fn extension_of(key: &str) -> String {
    Path::new(key)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_default()
}

/// A non-fatal problem met while parsing.
///
/// Warnings travel with the result rather than failing it: an empty body or a
/// lossy UTF-8 decode still produces usable markdown, and the caller reports the
/// degradation instead of hiding it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Warning {
    /// Stable machine-readable code, such as `empty` or `bad_utf8`.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
}

impl Warning {
    pub fn new(code: &str, message: impl Into<String>) -> Warning {
        Warning {
            code: String::from(code),
            message: message.into(),
        }
    }
}

/// What a backend produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Extracted {
    /// Markdown body, without any frontmatter.
    pub markdown: String,
    /// Degradations that did not stop the parse.
    pub warnings: Vec<Warning>,
    /// The result is a resource link, not a knowledge source.
    pub asset: bool,
    /// The file had no extractable text layer (a scanned document).
    pub no_text_layer: bool,
}

impl Extracted {
    /// A plain markdown result with no warnings and no degradation flags.
    pub fn markdown(markdown: impl Into<String>) -> Extracted {
        Extracted {
            markdown: markdown.into(),
            warnings: Vec::new(),
            asset: false,
            no_text_layer: false,
        }
    }
}

/// One parsing backend.
pub trait Parser {
    /// Stable id recorded in the raw frontmatter, such as `builtin-text`.
    fn id(&self) -> &'static str;
    /// Whether this backend handles `ext` (without the dot, any case).
    fn can_handle(&self, ext: &str) -> bool;
    /// Turns the source into markdown.
    fn parse(&self, input: &Source) -> Result<Extracted>;
}

/// Why a source could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// No registered backend claims the extension.
    #[error("no parser handles the extension {ext:?} of source {key:?}")]
    NoParser { key: String, ext: String },
    /// The file behind a source could not be read.
    #[error("cannot read {}: {message}", path.display())]
    Io { path: PathBuf, message: String },
    /// A backend claimed the source and then could not decode it.
    #[error("cannot parse source {key:?}: {message}")]
    Bad { key: String, message: String },
    /// The external service is configured incompletely.
    #[error("external parser configuration is incomplete: {message}")]
    ExternalConfig { message: String },
    /// The environment variable named by `auth_env` is not set.
    #[error("external parser token variable {name} is not set")]
    NoAuthEnv { name: String },
    /// The external service could not be reached or returned an unusable answer.
    #[error("external parser failed: {message}")]
    External { message: String },
    /// The external service answered with an unexpected HTTP status.
    #[error("external parser returned HTTP {status}: {body}")]
    ExternalStatus { status: u16, body: String },
    /// In `auto` mode both the builtin and the external backend failed.
    #[error("builtin backend failed ({builtin}); external backend failed ({external})")]
    BothFailed { builtin: String, external: String },
    /// The source is above `parse.max_source_bytes` and is refused.
    #[error("source is {size} bytes, above parse.max_source_bytes ({limit} bytes)")]
    Oversize { size: u64, limit: u64 },
}

impl ParseError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            ParseError::NoParser { .. } => "E_NO_PARSER",
            ParseError::Io { .. } => "E_IO",
            ParseError::Bad { .. } => "E_BAD_SOURCE",
            ParseError::ExternalConfig { .. } => "E_BAD_CONFIG",
            ParseError::NoAuthEnv { .. } => "E_NO_AUTH_ENV",
            ParseError::External { .. } => "E_EXTERNAL",
            ParseError::ExternalStatus { .. } => "E_EXTERNAL",
            ParseError::BothFailed { .. } => "E_BOTH_FAILED",
            ParseError::Oversize { .. } => "E_OVERSIZE",
        }
    }
}

/// Result of the parsing helpers.
pub type Result<T> = std::result::Result<T, ParseError>;

/// How a source of one size is delivered by `parse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Small enough to return in one piece.
    Whole,
    /// Large enough that the text must be chunked; `target` is the requested
    /// chunk size in bytes.
    Chunked { target: usize },
}

/// Classifies a source size against `parse.chunk_threshold` and
/// `parse.max_source_bytes`.
///
/// A source at or below the chunk threshold keeps the whole-document behaviour.
/// Above it the text is chunked, and above `max_source_bytes` the source is
/// refused rather than silently truncated.
pub fn delivery(cfg: &Config, size: u64) -> Result<Delivery> {
    delivery_with(cfg, size, false)
}

/// [`delivery`] with an explicit override for the size ceiling.
///
/// `force_large` exists only for `parse`, the debugging entry point: a source
/// above `parse.max_source_bytes` never enters the ledger, so being able to look
/// at what a backend extracts from it is the difference between "refused" and
/// "refused and undiagnosable". The pipeline itself (`convert`, `sync`) always
/// passes `false`.
pub fn delivery_with(cfg: &Config, size: u64, force_large: bool) -> Result<Delivery> {
    if size > cfg.parse.max_source_bytes && !force_large {
        return Err(ParseError::Oversize {
            size,
            limit: cfg.parse.max_source_bytes,
        });
    }
    if size <= cfg.parse.chunk_threshold {
        Ok(Delivery::Whole)
    } else {
        Ok(Delivery::Chunked {
            target: cfg.parse.chunk_target as usize,
        })
    }
}

/// The builtin backends available to `cfg`, in the order they are tried.
///
/// Order is specificity, not preference: text and html each claim a distinct set
/// of extensions, asset only claims image extensions, and pdf only claims `pdf`,
/// so no two backends compete for the same file. External backends are not part
/// of this list: they are network calls, so [`parse_with`] builds one only when
/// [`pick_backend`] says it should run.
pub fn registry(_cfg: &Config) -> Vec<Box<dyn Parser>> {
    vec![
        Box::new(text::TextParser::new()),
        Box::new(html::HtmlParser::new()),
        Box::new(asset::AssetParser::new()),
        Box::new(pdf::PdfParser::new()),
    ]
}

/// The id a builtin backend would claim `ext` under, if any.
fn builtin_id(ext: &str) -> Option<&'static str> {
    if text::TextParser::new().can_handle(ext) {
        return Some(text::ID);
    }
    if html::HtmlParser::new().can_handle(ext) {
        return Some(html::ID);
    }
    if asset::AssetParser::new().can_handle(ext) {
        return Some(asset::ID);
    }
    if pdf::PdfParser::new().can_handle(ext) {
        return Some(pdf::ID);
    }
    None
}

/// Backend ids to try for `ext`, in order, under `cfg.parse.primary`.
///
/// This is the whole policy in one place, so the tests can assert it without a
/// network and `parse_with` never invents a second copy of the rules:
///
/// - `auto`: the builtin backend first; the external service only for documents
///   that can be too thin (`html` and `pdf`), and only when it is configured.
/// - `builtin`: the builtin backend alone, even if an external service exists.
/// - `external`: the external service for text-like formats. Images stay local
///   because a parser service cannot produce a useful markdown body for them.
pub fn pick_backend(cfg: &Config, ext: &str) -> Vec<&'static str> {
    let ext = ext.to_ascii_lowercase();
    let builtin = builtin_id(&ext);
    let has_external = cfg.parse.external.is_some();
    match cfg.parse.primary {
        Primary::Auto => {
            let Some(builtin) = builtin else {
                return Vec::new();
            };
            let mut order = vec![builtin];
            if has_external && matches!(ext.as_str(), "html" | "htm" | "pdf") {
                order.push(external::ID);
            }
            order
        }
        Primary::Builtin => builtin.into_iter().collect(),
        Primary::External => {
            if matches!(
                ext.as_str(),
                "md" | "markdown" | "txt" | "html" | "htm" | "pdf"
            ) {
                vec![external::ID]
            } else {
                builtin.into_iter().collect()
            }
        }
    }
}

/// Parses `src` with the backends [`pick_backend`] names, applying the fallback.
///
/// The second value is the id of the backend that produced the result, so the
/// caller can record it in the raw frontmatter without asking the parser again.
///
/// In `auto` mode the builtin result is kept unless it is thin (fewer than
/// `[parse].min_chars` characters) or it has no text layer at all; only then is
/// the external service tried. A failed external attempt never loses the builtin
/// result: it is returned with an `external_failed` warning instead. Only when
/// both backends fail is there no result, and then both reasons travel together
/// in [`ParseError::BothFailed`].
pub fn parse_with(cfg: &Config, src: &Source) -> Result<(Extracted, String)> {
    let (mut extracted, backend) = parse_with_backend(cfg, src)?;
    append_below_min_chars(cfg, &mut extracted, &backend);
    Ok((extracted, backend))
}

/// Warns when a backend that had to interpret a format produced almost nothing.
///
/// A thin result from `html`, `pdf` or the external service usually means the
/// extraction failed rather than that the source is short, so it is worth
/// surfacing. `builtin-text` is exempt: it copies the bytes, so a short note is
/// short on purpose. Assets are exempt too: they are links by design.
fn append_below_min_chars(cfg: &Config, extracted: &mut Extracted, backend: &str) {
    if extracted.asset || backend == text::ID {
        return;
    }
    let chars = extracted.markdown.chars().count();
    if chars < cfg.parse.min_chars {
        extracted.warnings.push(Warning::new(
            "below_min_chars",
            format!(
                "the {backend} backend produced only {chars} characters (min_chars {})",
                cfg.parse.min_chars
            ),
        ));
    }
}

fn parse_with_backend(cfg: &Config, src: &Source) -> Result<(Extracted, String)> {
    let order = pick_backend(cfg, &src.ext);
    let Some(&first) = order.first() else {
        return Err(ParseError::NoParser {
            key: src.key.clone(),
            ext: src.ext.clone(),
        });
    };

    if first == external::ID {
        let parser = external::ExternalParser::from_config(&cfg.parse.external)?;
        let extracted = parser.parse(src)?;
        return Ok((extracted, String::from(external::ID)));
    }

    let builtin = registry(cfg)
        .into_iter()
        .find(|backend| backend.id() == first)
        .expect("pick_backend only names registered builtin backends");
    if !order.contains(&external::ID) {
        return Ok((builtin.parse(src)?, String::from(first)));
    }
    let external_parser = external::ExternalParser::from_config(&cfg.parse.external)?;

    let extracted = match builtin.parse(src) {
        Ok(extracted) => extracted,
        Err(builtin_error) => {
            return match external_parser.parse(src) {
                Ok(extracted) => Ok((
                    with_warning(
                        extracted,
                        "external_used",
                        format!(
                            "the builtin backend failed ({builtin_error}), used the external one"
                        ),
                    ),
                    String::from(external::ID),
                )),
                Err(external_error) => Err(ParseError::BothFailed {
                    builtin: builtin_error.to_string(),
                    external: external_error.to_string(),
                }),
            };
        }
    };

    let chars = extracted.markdown.chars().count();
    let too_thin = !extracted.asset && chars < cfg.parse.min_chars;
    if !extracted.asset && !too_thin {
        return Ok((extracted, String::from(first)));
    }

    match external_parser.parse(src) {
        Ok(extracted) => Ok((
            with_warning(
                extracted,
                "external_used",
                if chars < cfg.parse.min_chars {
                    format!(
                        "the builtin backend produced only {chars} characters (min_chars {}), used the external one",
                        cfg.parse.min_chars
                    )
                } else {
                    String::from("the source has no text layer, used the external one")
                },
            ),
            String::from(external::ID),
        )),
        Err(error) => Ok((
            with_warning(extracted, "external_failed", error.to_string()),
            String::from(first),
        )),
    }
}

/// Appends one warning to a result without disturbing the rest of it.
fn with_warning(mut extracted: Extracted, code: &str, message: impl Into<String>) -> Extracted {
    extracted.warnings.push(Warning::new(code, message));
    extracted
}
