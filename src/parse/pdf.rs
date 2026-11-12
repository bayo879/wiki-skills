//! The `builtin-pdf` backend: a text layer becomes markdown, a scan becomes a link.
//!
//! Everything follows from one question: does the document carry text? The
//! answer comes from [`validate`], which reads the document strictly before
//! [`pdf_extract`] is allowed to render it: encryption, the page tree, every
//! content stream a page uses (including Form XObjects) and every font the text
//! is set in are checked, page by page. When the walk finds text the document is
//! re-saved decrypted and handed to the extractor. When the walk genuinely finds
//! no text the backend returns the same flagged placeholder the asset backend
//! emits (`asset`, `no_text_layer`); the shared gate in `parse::validate_extracted`
//! then refuses it, because there is no OCR in this pipeline and a link must not
//! be registered as a converted document.
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

use lopdf::{Document, Object};

use self::validate::FailureKind;
use super::asset;
use super::{Extracted, ParseError, Parser, Result, Source, UnavailableReason, Warning};

/// Backend id recorded in the raw frontmatter and in command output.
pub const ID: &str = "builtin-pdf";

/// The one extension this backend claims.
const EXTENSION: &str = "pdf";

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
            document,
            has_text,
            pages,
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

        // One page per extraction: a document-level call lets the extractor stop
        // at the first page it cannot read and still return the pages before it,
        // which would publish a silently truncated body. Page numbers come from
        // this loop, not from separators the extractor may or may not emit.
        let mut texts = Vec::with_capacity(pages.len());
        let mut missing = Vec::new();
        for (index, page) in pages.iter().enumerate() {
            let number = index as u32 + 1;
            let bytes = serialise_page(&document, page, &input.key)?;
            let text = extract(&bytes, &input.key)?;
            // The walk saw a text operator and the extractor returned nothing.
            // It may be a page that only draws blank strings, so this is a
            // named nonfatal warning rather than a refusal — but it travels in
            // the raw frontmatter, so a dropped page cannot go unnoticed.
            if page.has_text && !text.chars().any(|c| !c.is_whitespace()) {
                missing.push(number);
            }
            texts.push(text);
        }
        Ok(into_markdown(&texts, &missing, &input.key))
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
/// Test-facing only: ingestion goes through [`Parser::parse`], which turns the
/// strict walk's failures into `ParseError` themselves. Only reading the file can
/// fail here; a document the walk refuses is reported as having text, so a caller
/// that does use this gets the parser's real error instead of a false "scan".
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

/// Writes a one-page copy of the validated document out, unencrypted.
///
/// `pdf-extract` only accepts bytes, and a document it opens itself would still
/// be encrypted and still carry the unreadable CMap, so the normalised copy is
/// what it gets. Narrowing the page tree to one page is what makes an extraction
/// failure attributable to a page number.
fn serialise_page(document: &Document, page: &validate::PageInfo, key: &str) -> Result<Vec<u8>> {
    let invalid = |detail: &str| ParseError::Bad {
        key: String::from(key),
        message: format!("pdf_serialise: {detail}"),
    };

    let mut copy = document.clone();
    let root = copy
        .trailer
        .get(b"Root")
        .map_err(|_| invalid("the trailer lost its /Root"))?
        .as_reference()
        .map_err(|_| invalid("/Root stopped being a reference"))?;
    let catalog = copy
        .get_object(root)
        .map_err(|_| invalid("/Root stopped resolving"))?
        .as_dict()
        .map_err(|_| invalid("/Root stopped being a dictionary"))?
        .clone();
    let pages_id = catalog
        .get(b"Pages")
        .map_err(|_| invalid("the catalog has no /Pages"))?
        .as_reference()
        .map_err(|_| invalid("/Pages stopped being a reference"))?;

    // The page may inherit `/Resources`; the copy has no ancestors to inherit
    // from, so the resolved dictionary is written onto the page itself.
    let mut page_dict = copy
        .get_object(page.id)
        .map_err(|_| invalid("the page stopped resolving"))?
        .as_dict()
        .map_err(|_| invalid("the page stopped being a dictionary"))?
        .clone();
    if !page_dict.has(b"Resources") {
        if let Some(resources) = &page.resources {
            page_dict.set("Resources", Object::Dictionary(resources.clone()));
        }
    }
    copy.objects.insert(page.id, Object::Dictionary(page_dict));

    let mut pages_dict = copy
        .get_object(pages_id)
        .map_err(|_| invalid("/Pages stopped resolving"))?
        .as_dict()
        .map_err(|_| invalid("/Pages stopped being a dictionary"))?
        .clone();
    pages_dict.set("Kids", vec![Object::Reference(page.id)]);
    pages_dict.set("Count", 1_i64);
    copy.objects
        .insert(pages_id, Object::Dictionary(pages_dict));

    let mut out = Vec::new();
    copy.save_to(&mut out).map_err(|err| {
        invalid(&format!(
            "the validated document could not be written back: {err}"
        ))
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

/// Turns the per-page texts into markdown.
///
/// The page boundaries are exact — they come from the loop that extracted one
/// page at a time — so pages are joined by one blank line and there is no
/// "page split unknown" warning any more. A page the walk saw text on but the
/// extractor returned nothing for is named by number; a document whose pages are
/// all blank still gets `empty`.
fn into_markdown(texts: &[String], missing: &[u32], key: &str) -> Extracted {
    let mut warnings = Vec::new();
    if !missing.is_empty() {
        let pages = missing
            .iter()
            .map(|page| page.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        warnings.push(Warning::new(
            "page_text_missing",
            format!(
                "{key}: page(s) {pages} draw text but the extractor returned none; the body may be incomplete"
            ),
        ));
    }
    let markdown = texts
        .iter()
        .map(|page| page.trim())
        .filter(|page| !page.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

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
