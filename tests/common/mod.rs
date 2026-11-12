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

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use lopdf::content::{Content, Operation};
use lopdf::encryption::crypt_filters::{CryptFilter, Rc4CryptFilter};
use lopdf::{
    dictionary, Document, EncryptionState, EncryptionVersion, Object, ObjectId, Permissions, Stream,
};

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

/// Builds the document for `pages`, with the page tree, one font and a catalog.
fn document(pages: &[Page]) -> Document {
    let mut doc = Document::with_version("1.5");

    let pages_id = doc.new_object_id();
    let font_id = helvetica(&mut doc);
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
    doc
}

/// Saves `doc` to a byte vector, as every fixture eventually does.
fn save_bytes(mut doc: Document) -> Vec<u8> {
    let mut out = Vec::new();
    doc.save_to(&mut out).expect("the fixture is a valid PDF");
    out
}

/// Builds the PDF bytes for `pages`, compressing the content streams.
fn build(pages: &[Page]) -> Vec<u8> {
    let mut doc = document(pages);
    // Compress the content streams so the probe has to decompress them: an
    // uncompressed stream would let a broken probe pass by accident.
    doc.compress();
    save_bytes(doc)
}

/// The Helvetica font object every text fixture draws with.
fn helvetica(doc: &mut Document) -> ObjectId {
    doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    })
}

/// Adds a page with `resources` and, when given, a `/Contents` reference.
fn page(
    doc: &mut Document,
    pages_id: ObjectId,
    resources: Object,
    content: Option<Object>,
) -> ObjectId {
    let mut dict = dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Resources" => resources,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    };
    if let Some(content) = content {
        dict.set("Contents", content);
    }
    doc.add_object(dict)
}

/// Closes `doc` with the page tree and catalog, then serialises it.
fn finish(mut doc: Document, pages_id: ObjectId, kids: Vec<ObjectId>, compress: bool) -> Vec<u8> {
    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids.into_iter().map(Object::Reference).collect::<Vec<_>>(),
            "Count" => count,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    if compress {
        doc.compress();
    }
    save_bytes(doc)
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

// ------------------------------------------------------- encrypted fixtures

/// The PDF bytes of an RC4-128 (V4/R4) document with `user_password`.
///
/// An empty `user_password` is the shape the real corpus has: the file opens
/// without a prompt but every stream is still encrypted. A non-empty one models
/// a document the pipeline must refuse instead of guessing.
pub fn make_encrypted_pdf_bytes(body: &str, pages: usize, user_password: &str) -> Vec<u8> {
    let pages: Vec<Page> = (0..pages).map(|_| Page::Text(String::from(body))).collect();
    let mut doc = document(&pages);
    doc.compress();
    // R4 derives its key from the file identifier, which every real encrypted
    // document carries; without one the fixture cannot be encrypted at all.
    doc.trailer.set(
        "ID",
        vec![
            Object::string_literal(b"fixture-file-id".to_vec()),
            Object::string_literal(b"fixture-file-id".to_vec()),
        ],
    );

    let state = {
        let filter: Arc<dyn CryptFilter> = Arc::new(Rc4CryptFilter);
        let version = EncryptionVersion::V4 {
            document: &doc,
            encrypt_metadata: false,
            crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
            stream_filter: b"StdCF".to_vec(),
            string_filter: b"StdCF".to_vec(),
            owner_password: "owner-secret",
            user_password,
            permissions: Permissions::all(),
        };
        EncryptionState::try_from(version).expect("V4/R4 is supported")
    };
    doc.encrypt(&state).expect("the fixture encrypts");
    save_bytes(doc)
}

/// Writes an RC4 document the pipeline is expected to open or refuse.
pub fn make_encrypted_pdf(path: &Path, body: &str, pages: usize, user_password: &str) {
    write(path, &make_encrypted_pdf_bytes(body, pages, user_password));
}

// ------------------------------------------------------------ strict layer

/// The PDF bytes of twelve pages whose text sits on page 4 only.
///
/// Page 4 is outside the six-page sample (first two, middle two, last two), so a
/// probe that only reads sampled pages calls this document a scan.
pub fn make_pdf_with_text_on_fourth_page_bytes() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = helvetica(&mut doc);
    let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });

    let mut kids = Vec::new();
    for index in 0..12 {
        let bytes = if index == 3 {
            text_content("text on the fourth page")
        } else {
            GRAPHICS.to_vec()
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, bytes));
        kids.push(page(
            &mut doc,
            pages_id,
            Object::Reference(resources),
            Some(Object::Reference(content_id)),
        ));
    }
    finish(doc, pages_id, kids, true)
}

/// The PDF bytes of a page set in a Type0 font with a Unicode CMap encoding.
///
/// `encoding` names the CMap (`UniGB-UTF16-H` is the real-world case); the drawn
/// codes are UTF-16BE, which is what that CMap means.
pub fn make_pdf_with_unicode_cmap_bytes(encoding: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let descriptor = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "SimSun",
        "Flags" => 4i64,
        "FontBBox" => vec![0.into(), 0.into(), 1000.into(), 1000.into()],
        "ItalicAngle" => 0i64,
        "Ascent" => 880i64,
        "Descent" => -120i64,
        "CapHeight" => 880i64,
        "StemV" => 80i64,
    });
    let descendant = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "SimSun",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0i64,
        },
        "FontDescriptor" => descriptor,
        "DW" => 1000i64,
    });
    let font = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "SimSun",
        "Encoding" => encoding,
        "DescendantFonts" => vec![Object::Reference(descendant)],
    });
    let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });

    // 你好, drawn as the UTF-16BE codes the Unicode CMap expects.
    let content = b"BT\n/F1 12 Tf\n100 700 Td\n<4F60597D> Tj\nET\n".to_vec();
    let content_id = doc.add_object(Stream::new(dictionary! {}, content));
    let page_id = page(
        &mut doc,
        pages_id,
        Object::Reference(resources),
        Some(Object::Reference(content_id)),
    );
    finish(doc, pages_id, vec![page_id], false)
}

/// The PDF bytes of two text pages, the second with an undecodable stream.
pub fn make_pdf_with_corrupt_second_page_bytes() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = helvetica(&mut doc);
    let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });

    let first = doc.add_object(Stream::new(
        dictionary! {},
        text_content("first page is fine"),
    ));
    let broken = doc.add_object(Stream::new(
        dictionary! { "Filter" => "FlateDecode" },
        b"this is not a deflate stream".to_vec(),
    ));
    let kids = vec![
        page(
            &mut doc,
            pages_id,
            Object::Reference(resources),
            Some(Object::Reference(first)),
        ),
        page(
            &mut doc,
            pages_id,
            Object::Reference(resources),
            Some(Object::Reference(broken)),
        ),
    ];
    finish(doc, pages_id, kids, false)
}

/// The PDF bytes of a page whose only text lives inside a Form XObject.
pub fn make_pdf_with_form_text_bytes() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = helvetica(&mut doc);

    let form_id = doc.new_object_id();
    let form = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
        },
        text_content("text inside a form"),
    );
    doc.objects.insert(form_id, Object::Stream(form));

    let resources = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
        "XObject" => dictionary! { "Fm1" => form_id },
    });
    let content = doc.add_object(Stream::new(dictionary! {}, b"q\n/Fm1 Do\nQ\n".to_vec()));
    let page_id = page(
        &mut doc,
        pages_id,
        Object::Reference(resources),
        Some(Object::Reference(content)),
    );
    finish(doc, pages_id, vec![page_id], true)
}

/// The PDF bytes of a form that draws itself: a cycle no extractor may follow.
pub fn make_pdf_with_form_cycle_bytes() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = helvetica(&mut doc);

    let form_id = doc.new_object_id();
    let form = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
                "XObject" => dictionary! { "Fm1" => form_id },
            },
        },
        b"q\n/Fm1 Do\nQ\n".to_vec(),
    );
    doc.objects.insert(form_id, Object::Stream(form));

    let resources = doc.add_object(dictionary! { "XObject" => dictionary! { "Fm1" => form_id } });
    let content = doc.add_object(Stream::new(dictionary! {}, b"q\n/Fm1 Do\nQ\n".to_vec()));
    let page_id = page(
        &mut doc,
        pages_id,
        Object::Reference(resources),
        Some(Object::Reference(content)),
    );
    finish(doc, pages_id, vec![page_id], false)
}

/// The PDF bytes of a page that draws an XObject the resources do not contain.
pub fn make_pdf_with_missing_form_bytes() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let resources = doc.add_object(dictionary! { "XObject" => dictionary! {} });
    let content = doc.add_object(Stream::new(dictionary! {}, b"q\n/Fm9 Do\nQ\n".to_vec()));
    let page_id = page(
        &mut doc,
        pages_id,
        Object::Reference(resources),
        Some(Object::Reference(content)),
    );
    finish(doc, pages_id, vec![page_id], false)
}

/// The PDF bytes of three text pages, the middle one carrying no content at all.
pub fn make_pdf_with_blank_middle_page_bytes() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = helvetica(&mut doc);
    let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });

    let first = doc.add_object(Stream::new(
        dictionary! {},
        text_content("before the blank page"),
    ));
    let last = doc.add_object(Stream::new(
        dictionary! {},
        text_content("after the blank page"),
    ));
    let kids = vec![
        page(
            &mut doc,
            pages_id,
            Object::Reference(resources),
            Some(Object::Reference(first)),
        ),
        page(&mut doc, pages_id, Object::Reference(resources), None),
        page(
            &mut doc,
            pages_id,
            Object::Reference(resources),
            Some(Object::Reference(last)),
        ),
    ];
    finish(doc, pages_id, kids, true)
}
