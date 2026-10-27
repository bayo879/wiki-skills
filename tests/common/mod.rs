//! Shared fixtures: real PDF files built with `lopdf`.
//!
//! The PDF tests need actual documents, not byte blobs: the probe reads
//! decompressed content streams and the extractor resolves fonts, so a fixture
//! that only *looks* like a PDF would prove nothing. Every builder here writes a
//! document whose pages carry either text-showing operators or pure graphics,
//! which is exactly the distinction the probe has to make.
//!
//! Pages that need no text are written as raw content streams so a test can put
//! arbitrary operators and strings into them — including the literal `(Tj)` and
//! the hex string `<544A>`, which are operands, not text operators.

#![allow(dead_code)]

use std::path::Path;

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};

/// One page's content stream.
enum Page {
    /// A text page: `BT /F1 12 Tf … (body) Tj ET`.
    Text(String),
    /// A page with raw content-stream bytes and no text operators.
    Raw(&'static [u8]),
}

/// Graphics only: a filled rectangle, in and out of the graphics state.
const GRAPHICS: &[u8] = b"q\n0.8 g\n100 700 200 120 re f\nQ\n";

/// Graphical page whose *operands* contain the letter sequences `Tj` and `TJ`.
///
/// `/Artifact` is a name, `(Tj)` a literal string and `<544A>` a hex string: a
/// probe that looks for the bare bytes would call this a text page, while a
/// tokenising probe sees no text-showing operator at all.
const TEXT_IN_STRINGS: &[u8] =
    b"q\n/Artifact << /ActualText (Tj) /Alt <544A> >> BDC\n100 700 200 120 re f\nEMC\nQ\n";

/// Builds the PDF bytes for `pages`, with the page tree, one font and a catalog.
fn build(pages: &[Page]) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");

    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let mut kids = Vec::new();
    for page in pages {
        let bytes = match page {
            Page::Text(body) => text_content(body),
            Page::Raw(raw) => raw.to_vec(),
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, bytes));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        kids.push(page_id);
    }

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids
                .iter()
                .map(|id| Object::Reference(*id))
                .collect::<Vec<_>>(),
            "Count" => kids.len() as i64,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    // Compress the content streams so the probe has to decompress them: an
    // uncompressed stream would let a broken probe pass by accident.
    doc.compress();

    let mut out = Vec::new();
    doc.save_to(&mut out).expect("the fixture is a valid PDF");
    out
}

/// The content stream of a one-line text page.
fn text_content(body: &str) -> Vec<u8> {
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("Td", vec![100.into(), 700.into()]),
            Operation::new("Tj", vec![Object::string_literal(body)]),
            Operation::new("ET", vec![]),
        ],
    };
    content.encode().expect("the fixture content encodes")
}

/// The PDF bytes of a document whose `pages` pages all show `body`.
pub fn make_pdf_bytes(body: &str, pages: usize) -> Vec<u8> {
    let pages: Vec<Page> = (0..pages).map(|_| Page::Text(String::from(body))).collect();
    build(&pages)
}

/// The PDF bytes of a document with no text layer at all.
pub fn make_scanned_pdf_bytes(pages: usize) -> Vec<u8> {
    let pages: Vec<Page> = (0..pages).map(|_| Page::Raw(GRAPHICS)).collect();
    build(&pages)
}

/// The PDF bytes of a text-free page that still spells `Tj` and `TJ` in strings.
pub fn make_scanned_pdf_with_text_in_strings_bytes() -> Vec<u8> {
    build(&[Page::Raw(TEXT_IN_STRINGS)])
}

/// The PDF bytes of a six-page document whose fifth page is the only text page.
pub fn make_mixed_pdf_bytes() -> Vec<u8> {
    let mut pages: Vec<Page> = Vec::new();
    for index in 0..6 {
        if index == 4 {
            pages.push(Page::Text(String::from("fifth page body")));
        } else {
            pages.push(Page::Raw(GRAPHICS));
        }
    }
    build(&pages)
}

/// The PDF bytes of a text page with no `MediaBox` anywhere.
///
/// `lopdf` loads it (the xref is fine) but `pdf-extract` panics while rendering
/// it, which is the case `catch_unwind` in the backend exists for.
pub fn make_pdf_missing_media_box_bytes() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");

    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let content_id = doc.add_object(Stream::new(dictionary! {}, text_content("no media box")));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => resources_id,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1i64,
            "Resources" => resources_id,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    doc.compress();

    let mut out = Vec::new();
    doc.save_to(&mut out).expect("the fixture is a valid PDF");
    out
}

/// Writes `bytes` to `path`, creating the parent directory if needed.
pub fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("the fixture directory is writable");
    }
    std::fs::write(path, bytes).expect("the fixture file is writable");
}

/// Writes a `pages`-page document whose every page shows `body`.
pub fn make_pdf(path: &Path, body: &str, pages: usize) {
    write(path, &make_pdf_bytes(body, pages));
}

/// Writes a `pages`-page document with no text layer.
pub fn make_scanned_pdf(path: &Path, pages: usize) {
    write(path, &make_scanned_pdf_bytes(pages));
}

/// Writes the six-page document whose fifth page is the only text page.
pub fn make_mixed_pdf(path: &Path) {
    write(path, &make_mixed_pdf_bytes());
}

/// Writes a text-free page whose operands spell `Tj` and `TJ`.
pub fn make_scanned_pdf_with_text_in_strings(path: &Path) {
    write(path, &make_scanned_pdf_with_text_in_strings_bytes());
}

/// Writes a text page that `pdf-extract` panics on.
pub fn make_pdf_missing_media_box(path: &Path) {
    write(path, &make_pdf_missing_media_box_bytes());
}
