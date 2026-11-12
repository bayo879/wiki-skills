//! The `builtin-pdf` backend: a text layer becomes markdown, a scan becomes a link.
//!
//! Everything follows from one question: does the document carry text? The
//! answer comes from [`validate`], which reads the document strictly before
//! [`pdf_extract`] is allowed to render it: encryption, the page tree, every
//! content stream a page uses (including Form XObjects) and every font the text
//! is set in are checked, page by page. When the walk finds text the document is
//! re-saved decrypted and handed to the extractor; when it genuinely finds none,
//! the backend does not guess: a scan degrades to the same link the asset backend
//! emits and is flagged `no_text_layer`, because there is no OCR in this pipeline
//! and plausible nonsense is worse than an honest link.
//!
//! The old probe sampled the first two, middle two and last two pages and gave up
//! on the rest; it also treated any load or decode error as "no text". Both were
//! wrong in the same direction — a document whose body sat outside the sample, or
//! whose encryption this build could not open, was published as an attachment
//! link with the text silently gone. Sampling is therefore gone: every page is
//! read, and a page that cannot be read fails the document instead of shrinking
//! it.
//!
//! [`pdf_extract`] still panics on some documents `lopdf` loads happily (a page
//! without a `MediaBox` is enough), so extraction runs inside `catch_unwind` and
//! a panic is reported as an error. The panic hook is muted around the call so a
//! recoverable document does not print a backtrace.

mod validate;

use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Mutex;

use lopdf::Document;

use self::validate::FailureKind;
use super::asset;
use super::{Extracted, ParseError, Parser, Result, Source, UnavailableReason, Warning};

/// Backend id recorded in the raw frontmatter and in command output.
pub const ID: &str = "builtin-pdf";

/// The one extension this backend claims.
const EXTENSION: &str = "pdf";

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
        let validate::Validated {
            document, has_text, ..
        } = validate::load_and_validate(&input.bytes)
            .map_err(|failure| unavailable(&input.key, failure))?;

        if !has_text {
            return Ok(Extracted {
                markdown: asset::link_for(&input.key),
                warnings: vec![Warning::new(
                    "no_text_layer",
                    format!(
                        "{} has no text layer on any page; it is linked, not extracted (there is no OCR)",
                        input.key
                    ),
                )],
                asset: true,
                no_text_layer: true,
            });
        }

        let bytes = serialise(document, &input.key)?;
        let text = extract(&bytes, &input.key)?;
        Ok(into_markdown(&text, &input.key))
    }
}

/// Turns one strict-walk failure into the error the caller sees.
///
/// A document that cannot be opened or needs a password is a plain parse
/// failure. Everything else means the walk got far enough to know text is
/// missing: a stream or page it could not read is a partial extraction, and a
/// font it cannot map back to Unicode would silently drop characters, so it is
/// reported as lossy decoding. Naming the kind and the page is what lets the
/// caller report "page 3, undecodable stream" instead of guessing "no text".
fn unavailable(key: &str, failure: validate::Failure) -> ParseError {
    let key = String::from(key);
    match failure.kind {
        FailureKind::Load | FailureKind::PasswordRequired => ParseError::Bad {
            key,
            message: failure.message(),
        },
        FailureKind::FontEncoding => ParseError::Unavailable {
            key: key.clone(),
            backend: String::from(ID),
            reason: UnavailableReason::LossyDecode,
            page: failure.page,
        },
        _ => ParseError::Unavailable {
            key,
            backend: String::from(ID),
            reason: UnavailableReason::PartialExtraction,
            page: failure.page,
        },
    }
}

/// Whether the document at `path` has a text layer on any page.
///
/// Only reading the file can fail: a document the strict walk refuses is
/// reported as having text, so the extractor gets to report the real error
/// instead of the probe guessing that the file is a scan.
pub fn has_text_layer(path: &Path) -> Result<bool> {
    let bytes = std::fs::read(path).map_err(|err| ParseError::Io {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;
    Ok(validate::load_and_validate(&bytes)
        .map(|validated| validated.has_text)
        .unwrap_or(true))
}

// -------------------------------------------------------------- extraction

/// Writes the validated document back out, unencrypted, for the extractor.
///
/// `pdf-extract` only accepts bytes, and a document it opens itself would still
/// be encrypted and still carry the unreadable CMap, so the normalised copy is
/// what it gets.
fn serialise(mut document: Document, key: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    document.save_to(&mut out).map_err(|err| ParseError::Bad {
        key: String::from(key),
        message: format!("pdf_serialise: the validated document could not be written back: {err}"),
    })?;
    Ok(out)
}

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
        Ok(Err(err)) => Err(format!("pdf_extract: could not decode the document: {err}")),
        Err(payload) => Err(format!(
            "pdf_extract: panicked on this document: {}",
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
