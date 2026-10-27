//! The `builtin-pdf` backend: a text layer becomes markdown, a scan becomes a link.
//!
//! Everything follows from one question: does the document carry text? If it
//! does, [`pdf_extract`] renders the pages and they are joined into markdown. If
//! it does not, the backend does not guess: a scan degrades to the same link the
//! asset backend emits and is flagged `no_text_layer`, because there is no OCR in
//! this pipeline and plausible nonsense is worse than an honest link.
//!
//! Two hazards shape the code:
//!
//! - [`pdf_extract`] panics on documents `lopdf` loads happily (a page without a
//!   `MediaBox` is enough to make it `.expect("MediaBox")`), so extraction runs
//!   inside `catch_unwind` and a panic is reported as an error. The panic hook is
//!   muted around the call so a recoverable document does not print a backtrace.
//! - The probe cannot look for the bytes `Tj`: a scanned page may spell that in a
//!   caption string, or inside a hex string. Content streams are therefore
//!   tokenised, strings and names are skipped whole, and only real operators
//!   count.
//!
//! The probe samples the first two, the middle two and the last two pages rather
//! than reading the whole document twice: a body with a scanned cover still has
//! text in it, and a document whose text starts on page 3 is still a text
//! document.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Mutex;

use lopdf::{Document, ObjectId};

use super::asset;
use super::{Extracted, ParseError, Parser, Result, Source, Warning};

/// Backend id recorded in the raw frontmatter and in command output.
pub const ID: &str = "builtin-pdf";

/// The one extension this backend claims.
const EXTENSION: &str = "pdf";

/// Operators that show text: `Tj`, `TJ`, the next-line variants `'` and `"`.
const TEXT_OPERATORS: [&str; 4] = ["Tj", "TJ", "'", "\""];

/// How many pages are sampled at the head and at the tail of the document.
const EDGE_SAMPLE: usize = 2;

/// The form feed `pdf_extract` uses as a page separator, when it emits one.
const PAGE_BREAK: char = '\u{0c}';

/// Serialises the panic-hook swap.
///
/// The hook is process-global, so two concurrent parses could otherwise take and
/// restore each other's hook and lose the default one for the rest of the run.
static PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

/// The `builtin-pdf` backend.
pub struct PdfParser;

impl PdfParser {
    /// A stateless backend; construction exists so the registry reads uniformly.
    pub fn new() -> PdfParser {
        PdfParser
    }
}

impl Default for PdfParser {
    fn default() -> Self {
        PdfParser::new()
    }
}

impl Parser for PdfParser {
    fn id(&self) -> &'static str {
        ID
    }

    /// `pdf`, in any case.
    fn can_handle(&self, ext: &str) -> bool {
        ext.eq_ignore_ascii_case(EXTENSION)
    }

    fn parse(&self, input: &Source) -> Result<Extracted> {
        // The bytes come from `Source`, never from the filesystem: a big
        // document must not be read twice just to answer the probe.
        if !has_text_layer_in(&input.bytes) {
            return Ok(Extracted {
                markdown: asset::link_for(&input.key),
                warnings: vec![Warning::new(
                    "no_text_layer",
                    format!(
                        "{} has no text layer on any sampled page; it is linked, not extracted (there is no OCR)",
                        input.key
                    ),
                )],
                asset: true,
                no_text_layer: true,
            });
        }

        let text = extract(&input.bytes, &input.key)?;
        Ok(into_markdown(&text, &input.key))
    }
}

/// Whether the document at `path` has a text layer on any sampled page.
///
/// Only reading the file can fail: a document `lopdf` cannot open is reported as
/// having text, so the extractor gets to report the real error instead of the
/// probe guessing that the file is a scan.
pub fn has_text_layer(path: &Path) -> Result<bool> {
    let bytes = std::fs::read(path).map_err(|err| ParseError::Io {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;
    Ok(has_text_layer_in(&bytes))
}

/// The shared probe body, over bytes instead of a path.
fn has_text_layer_in(bytes: &[u8]) -> bool {
    let document = match Document::load_mem(bytes) {
        Ok(document) => document,
        Err(_) => return true,
    };

    let pages = document.get_pages();
    sample_page_ids(&pages)
        .into_iter()
        .any(|page| match document.get_page_content(page) {
            // Content is decompressed here, so the operators are visible even
            // when the fixture compressed the stream.
            Ok(content) => has_text_operator(&content),
            Err(_) => false,
        })
}

/// The page ids to probe: first two, middle two, last two, without repeats.
///
/// Documents shorter than six pages are probed whole; longer ones skip the body
/// between the middle and the tail, which keeps the cost flat for a fat manual.
fn sample_page_ids(pages: &BTreeMap<u32, ObjectId>) -> Vec<ObjectId> {
    let ids: Vec<ObjectId> = pages.values().copied().collect();
    let len = ids.len();
    if len == 0 {
        return Vec::new();
    }

    let middle = len / 2;
    let candidates = [
        0,
        1,
        middle.saturating_sub(EDGE_SAMPLE / 2),
        middle,
        len.saturating_sub(EDGE_SAMPLE),
        len - 1,
    ];
    let mut picked: Vec<usize> = candidates
        .into_iter()
        .filter(|index| *index < len)
        .collect();
    picked.sort_unstable();
    picked.dedup();
    picked.into_iter().map(|index| ids[index]).collect()
}

// ------------------------------------------------------------------- probe

/// Whether a content stream shows any text at all.
///
/// The scan is a small PDF tokeniser, not a substring search: `(...)` literal
/// strings, `<...>` hex strings, `<<...>>` dictionaries, `/Names` and `%`
/// comments are skipped as whole units, and only the surviving token is compared
/// against [`TEXT_OPERATORS`]. A scanned page whose caption says `Tj` therefore
/// stays a scan.
///
/// Inline image bodies are not skipped (their binary payload can, in principle,
/// spell an operator); recognising `BI`/`ID`/`EI` is left out deliberately,
/// because a false positive only costs an extraction attempt whose empty result
/// is reported as an `empty` warning, while a false negative would throw away a
/// real text layer.
fn has_text_operator(content: &[u8]) -> bool {
    let mut index = 0;
    while index < content.len() {
        match content[index] {
            b'%' => index = skip_comment(content, index + 1),
            b'(' => index = skip_literal_string(content, index + 1),
            b'<' if content.get(index + 1) == Some(&b'<') => index += 2,
            b'<' => index = skip_hex_string(content, index + 1),
            b'>' => {
                index += 1;
                if content.get(index) == Some(&b'>') {
                    index += 1;
                }
            }
            b')' | b'[' | b']' | b'{' | b'}' => index += 1,
            b'/' => index = skip_name(content, index + 1),
            byte if is_space(byte) => index += 1,
            _ => {
                let start = index;
                while index < content.len() && !is_token_boundary(content[index]) {
                    index += 1;
                }
                if is_text_operator(&content[start..index]) {
                    return true;
                }
            }
        }
    }
    false
}

fn is_text_operator(token: &[u8]) -> bool {
    TEXT_OPERATORS
        .iter()
        .any(|operator| operator.as_bytes() == token)
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'\0')
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Whether a byte ends a token: PDF separates tokens by whitespace or delimiter.
fn is_token_boundary(byte: u8) -> bool {
    is_space(byte) || is_delimiter(byte)
}

/// Skips a `%` comment, returning the index of the byte after its line.
fn skip_comment(content: &[u8], mut index: usize) -> usize {
    while index < content.len() && content[index] != b'\n' && content[index] != b'\r' {
        index += 1;
    }
    index
}

/// Skips a `/Name` operand, returning the index of the byte after it.
fn skip_name(content: &[u8], mut index: usize) -> usize {
    while index < content.len() && !is_space(content[index]) && !is_delimiter(content[index]) {
        index += 1;
    }
    index
}

/// Skips a `(...)` literal string, honouring escapes and nested parentheses.
fn skip_literal_string(content: &[u8], mut index: usize) -> usize {
    let mut depth = 1usize;
    while index < content.len() {
        match content[index] {
            // A backslash escapes the next byte, whatever it is; a trailing one
            // simply runs out of stream.
            b'\\' => index = (index + 2).min(content.len()),
            b'(' => {
                depth += 1;
                index += 1;
            }
            b')' => {
                depth -= 1;
                index += 1;
                if depth == 0 {
                    return index;
                }
            }
            _ => index += 1,
        }
    }
    content.len()
}

/// Skips a `<...>` hex string, returning the index of the byte after it.
fn skip_hex_string(content: &[u8], mut index: usize) -> usize {
    while index < content.len() {
        if content[index] == b'>' {
            return index + 1;
        }
        index += 1;
    }
    content.len()
}

// -------------------------------------------------------------- extraction

/// Renders the document at `bytes` to text, turning any failure into [`ParseError`].
fn extract(bytes: &[u8], key: &str) -> Result<String> {
    match guarded_extract(bytes) {
        Ok(text) => Ok(text),
        Err(message) => Err(ParseError::Bad {
            key: String::from(key),
            message,
        }),
    }
}

/// Runs `pdf-extract` with panics caught and their default output muted.
fn guarded_extract(bytes: &[u8]) -> std::result::Result<String, String> {
    let _guard = PANIC_HOOK_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem(bytes)
    }));
    std::panic::set_hook(previous);

    match outcome {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(err)) => Err(format!("pdf-extract could not decode the document: {err}")),
        Err(payload) => Err(format!(
            "pdf-extract panicked on this document: {}",
            panic_message(payload.as_ref())
        )),
    }
}

/// The message inside a caught panic payload, when it has one.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return String::from(*text);
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    String::from("unknown panic payload")
}

/// Turns extracted text into markdown, reporting how much it trusted the pages.
///
/// `pdf-extract` 0.7 does not emit page separators, so a multi-page document
/// arrives as one block; the `page_split_unknown` warning is what keeps that
/// silent loss of structure visible. When a separator is present the pages are
/// trimmed and joined by one blank line.
fn into_markdown(text: &str, key: &str) -> Extracted {
    let mut warnings = Vec::new();
    let trimmed = text.trim();
    let markdown = if text.contains(PAGE_BREAK) {
        text.split(PAGE_BREAK)
            .map(str::trim)
            .filter(|page| !page.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    } else {
        String::from(trimmed)
    };

    if !text.contains(PAGE_BREAK) && !trimmed.is_empty() {
        warnings.push(Warning::new(
            "page_split_unknown",
            format!("{key} was extracted as one block: the extractor reported no page boundaries"),
        ));
    }
    if markdown.trim().is_empty() {
        warnings.push(Warning::new(
            "empty",
            format!("{key} has a text layer but no extractable text"),
        ));
    }

    Extracted {
        markdown,
        warnings,
        asset: false,
        no_text_layer: false,
    }
}
