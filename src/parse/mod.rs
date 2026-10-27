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

pub mod html;
pub mod text;

use std::path::Path;

use serde::Serialize;

use crate::config::Config;

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
}

impl ParseError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            ParseError::NoParser { .. } => "E_NO_PARSER",
        }
    }
}

/// Result of the parsing helpers.
pub type Result<T> = std::result::Result<T, ParseError>;

/// The backends available for `cfg`, in the order they are tried.
///
/// Later tasks append the html, pdf, asset and external backends here; `cfg`
/// decides which ones are enabled, which is why it is part of the signature even
/// though only the text backend exists today.
pub fn registry(_cfg: &Config) -> Vec<Box<dyn Parser>> {
    vec![Box::new(text::TextParser::new())]
}

/// Parses `src` with the first backend that claims its extension.
///
/// The second value is the id of the backend that ran, so the caller can record
/// it in the raw frontmatter without asking the parser again.
pub fn parse_with(cfg: &Config, src: &Source) -> Result<(Extracted, String)> {
    for backend in registry(cfg) {
        if !backend.can_handle(&src.ext) {
            continue;
        }
        let id = backend.id();
        return Ok((backend.parse(src)?, String::from(id)));
    }
    Err(ParseError::NoParser {
        key: src.key.clone(),
        ext: src.ext.clone(),
    })
}
