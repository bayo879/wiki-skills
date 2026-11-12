//! Strict reading of a PDF before `pdf-extract` is allowed to render it.
//!
//! `pdf-extract` is lenient in exactly the places this pipeline cannot afford to
//! be: it stops at the first page it cannot read and still returns what it has,
//! it follows Form XObjects without noticing cycles, and it panics on font
//! encodings it does not know. A text layer that silently disappears is worse
//! than a refused document, so the document is read twice: once strictly here,
//! where every problem is reported with its page, and once through the
//! extractor, whose output is only used when this pass found nothing to report.
//!
//! Three things are checked:
//!
//! - The page tree is walked by hand from the catalog, so a broken `/Kids`
//!   reference is an error instead of a silently smaller document.
//! - Every content stream a page actually uses is decoded strictly, and the
//!   operators in it are tokenised, so text is found wherever it is drawn —
//!   including inside Form XObjects, which are followed with a recursion stack.
//! - Every font the content sets text in is checked: the pipeline can only turn
//!   codes into text through `ToUnicode` (or a simple font's `/Encoding`), so a
//!   Type0 font without a usable map is an error rather than empty output.
//!
//! Unicode CMaps (`UniGB-UCS2-H` and friends) are the one translation applied:
//! their codes *are* UTF-16BE, so rewriting the encoding to `Identity-H` and
//! adding an identity `ToUnicode` map preserves the text exactly. Any other
//! named CMap is refused: guessing GBK codes through the wrong table would
//! produce plausible nonsense.

use std::collections::HashSet;
use std::io::Read;

use flate2::read::ZlibDecoder;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

/// Deepest page-tree nesting accepted before the document is called broken.
const MAX_PAGE_DEPTH: usize = 64;

/// Deepest Form XObject nesting accepted.
const MAX_FORM_DEPTH: usize = 32;

/// Operands kept for the tokeniser; longer stacks are dropped, not grown.
const MAX_OPERANDS: usize = 64;

/// The `ToUnicode` map added when a Unicode CMap is rewritten.
///
/// The codes of a Unicode CMap are UTF-16BE, so the identity range is exact:
/// `adobe-cmap-parser` reads it and `pdf-extract` then maps each code to itself.
const IDENTITY_TO_UNICODE: &[u8] = b"1 beginbfrange\n<0000> <FFFF> <0000>\nendbfrange\n";

/// Why a document could not be read far enough to be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The file is not a PDF `lopdf` can parse at all.
    Load,
    /// Encrypted, and the empty password did not open it.
    PasswordRequired,
    /// The page tree is malformed: missing `Kids`, a cycle, a non-dictionary.
    InvalidPageTree,
    /// A reference the content needs does not resolve.
    InvalidReference,
    /// A stream declares a filter this pipeline cannot decode strictly.
    StreamDecode,
    /// A content stream ends inside a string, an array or a dictionary.
    ContentDecode,
    /// A Form XObject draws itself, directly or through another form.
    FormCycle,
    /// Nesting exceeded the accepted depth.
    RecursionLimit,
    /// A font's encoding cannot be turned into text without guessing.
    FontEncoding,
}

impl FailureKind {
    /// The stable prefix of the message, safe to match on in logs and tests.
    pub fn code(self) -> &'static str {
        match self {
            FailureKind::Load => "pdf_load",
            FailureKind::PasswordRequired => "pdf_password_required",
            FailureKind::InvalidPageTree => "pdf_invalid_page_tree",
            FailureKind::InvalidReference => "pdf_invalid_reference",
            FailureKind::StreamDecode => "pdf_stream_decode",
            FailureKind::ContentDecode => "pdf_content_decode",
            FailureKind::FormCycle => "pdf_form_cycle",
            FailureKind::RecursionLimit => "pdf_recursion_limit",
            FailureKind::FontEncoding => "pdf_font_encoding",
        }
    }
}

/// One refusal, with the page it belongs to when a page is to blame.
#[derive(Debug, Clone)]
pub struct Failure {
    /// What kind of problem was found.
    pub kind: FailureKind,
    /// One-based page number, when the problem is page-local.
    pub page: Option<u32>,
    /// Human-readable detail; the page prefix is added by [`Failure::message`].
    pub detail: String,
}

impl Failure {
    fn new(kind: FailureKind, detail: impl Into<String>) -> Failure {
        Failure {
            kind,
            page: None,
            detail: detail.into(),
        }
    }

    /// Attaches `page` unless a page is already recorded.
    fn on_page(mut self, page: u32) -> Failure {
        if self.page.is_none() {
            self.page = Some(page);
        }
        self
    }

    /// The message handed to [`crate::parse::ParseError::Bad`].
    pub fn message(&self) -> String {
        match self.page {
            Some(page) => format!("{} (page {page}): {}", self.kind.code(), self.detail),
            None => format!("{}: {}", self.kind.code(), self.detail),
        }
    }
}

/// A document that passed every check, plus what the walk learned about it.
pub struct Validated {
    /// The decrypted document, with Unicode CMaps normalised.
    pub document: Document,
    /// Whether any page draws text.
    pub has_text: bool,
    /// Every page, in document order, with the resources it inherits.
    pub pages: Vec<PageInfo>,
}

/// One checked page, enough to hand it to the extractor on its own.
pub struct PageInfo {
    /// The page object; the extractor sees a document whose page tree holds
    /// only this page, so a page it silently dropped cannot hide inside a
    /// combined result.
    pub id: ObjectId,
    /// The `/Resources` the page effectively uses, already resolved through the
    /// page tree; `None` only when the document genuinely has none.
    pub resources: Option<Dictionary>,
    /// Whether the strict walk found text drawing on this page.
    pub has_text: bool,
    /// Whether any of that text draws a visible character, not just empty or
    /// blank strings. A page whose visible text the extractor cannot produce
    /// fails the document; a page that only draws blanks may legitimately
    /// extract to nothing.
    pub has_nonblank_text: bool,
}

/// Reads `bytes` strictly; the returned document is safe to hand to `pdf-extract`.
pub fn load_and_validate(bytes: &[u8]) -> std::result::Result<Validated, Failure> {
    let mut document = load(bytes)?;
    rewrite_unicode_fonts(&mut document)?;

    let pages = collect_pages(&document)?;
    if pages.is_empty() {
        return Err(Failure::new(
            FailureKind::InvalidPageTree,
            "the page tree reaches no pages",
        ));
    }

    let checker = Checker { doc: &document };
    let mut infos = Vec::with_capacity(pages.len());
    for (index, page) in pages.into_iter().enumerate() {
        let (has_text, has_nonblank_text) = checker.page_has_text(&page, index as u32 + 1)?;
        infos.push(PageInfo {
            id: page.id,
            resources: page.resources,
            has_text,
            has_nonblank_text,
        });
    }
    let has_text = infos.iter().any(|page| page.has_text);

    Ok(Validated {
        document,
        has_text,
        pages: infos,
    })
}

/// Loads and — when the file uses an empty user password — decrypts `bytes`.
fn load(bytes: &[u8]) -> std::result::Result<Document, Failure> {
    let document = Document::load_mem(bytes).map_err(|error| {
        // `lopdf` reports an encryption revision it does not implement and a
        // password it cannot check through the same variant, so the message is
        // the only distinction left; both mean "we cannot read this file".
        Failure::new(
            FailureKind::Load,
            format!("the document could not be read: {error}"),
        )
    })?;

    // With no password supplied, `lopdf` leaves a locked document encrypted and
    // unloaded instead of failing the load. Treating that as "no text" would
    // publish an empty page for a document somebody simply protected.
    if document.is_encrypted() {
        return Err(Failure::new(
            FailureKind::PasswordRequired,
            "the document is encrypted and does not open with an empty password",
        ));
    }

    Ok(document)
}

// ------------------------------------------------------------ font rewrite

/// A font rewrite queued before the object table is mutated.
struct FontRewrite {
    id: ObjectId,
    /// Whether an identity `ToUnicode` map has to be added.
    add_to_unicode: bool,
}

/// Rewrites the encodings `pdf-extract` cannot read into the one it can.
///
/// Nothing else is touched: a font with an unknown CMap stays as it is, and the
/// per-page check refuses it later with the page number attached.
fn rewrite_unicode_fonts(document: &mut Document) -> std::result::Result<(), Failure> {
    let mut rewrites = Vec::new();
    for (id, object) in &document.objects {
        let Object::Dictionary(font) = object else {
            continue;
        };
        if name_of(font, b"Subtype") != Some(b"Type0") {
            continue;
        }
        let Some(encoding) = name_of(font, b"Encoding") else {
            continue;
        };
        if encoding == b"Identity-H" {
            continue;
        }
        let is_identity_v = encoding == b"Identity-V";
        if !is_identity_v && !is_unicode_cmap(encoding) {
            continue;
        }
        rewrites.push(FontRewrite {
            id: *id,
            add_to_unicode: !is_identity_v && font.get(b"ToUnicode").is_err(),
        });
    }

    for rewrite in rewrites {
        let to_unicode = rewrite.add_to_unicode.then(|| {
            document.add_object(Stream::new(Dictionary::new(), IDENTITY_TO_UNICODE.to_vec()))
        });
        if let Some(Object::Dictionary(font)) = document.objects.get_mut(&rewrite.id) {
            font.set("Encoding", Object::Name(b"Identity-H".to_vec()));
            if let Some(id) = to_unicode {
                font.set("ToUnicode", id);
            }
        }
    }
    Ok(())
}

/// Whether `name` is a CMap whose codes are UTF-16BE (`UniGB-UTF16-H`, …).
fn is_unicode_cmap(name: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(name) else {
        return false;
    };
    let Some(rest) = text.strip_prefix("Uni") else {
        return false;
    };
    let stem = rest
        .strip_suffix("-H")
        .or_else(|| rest.strip_suffix("-V"))
        .unwrap_or(rest);
    stem.ends_with("-UCS2") || stem.ends_with("-UTF16")
}

// -------------------------------------------------------------- page tree

/// One page, with the resources it inherits from the page-tree nodes above it.
struct PageEntry {
    id: ObjectId,
    resources: Option<Dictionary>,
}

/// Walks the page tree from the catalog, refusing anything malformed.
fn collect_pages(document: &Document) -> std::result::Result<Vec<PageEntry>, Failure> {
    let root = document
        .trailer
        .get(b"Root")
        .map_err(|_| Failure::new(FailureKind::InvalidPageTree, "the trailer has no /Root"))?
        .as_reference()
        .map_err(|_| Failure::new(FailureKind::InvalidPageTree, "/Root is not a reference"))?;
    let catalog = document
        .get_object(root)
        .map_err(|_| Failure::new(FailureKind::InvalidReference, "/Root does not resolve"))?
        .as_dict()
        .map_err(|_| Failure::new(FailureKind::InvalidPageTree, "/Root is not a dictionary"))?;
    let pages = catalog
        .get(b"Pages")
        .map_err(|_| Failure::new(FailureKind::InvalidPageTree, "the catalog has no /Pages"))?
        .as_reference()
        .map_err(|_| Failure::new(FailureKind::InvalidPageTree, "/Pages is not a reference"))?;

    let mut out = Vec::new();
    let mut active = HashSet::new();
    collect_node(document, pages, None, 0, &mut active, &mut out)?;
    Ok(out)
}

fn collect_node(
    document: &Document,
    id: ObjectId,
    inherited: Option<&Dictionary>,
    depth: usize,
    active: &mut HashSet<ObjectId>,
    out: &mut Vec<PageEntry>,
) -> std::result::Result<(), Failure> {
    if depth > MAX_PAGE_DEPTH {
        return Err(Failure::new(
            FailureKind::RecursionLimit,
            "the page tree nests deeper than a document plausibly can",
        ));
    }
    if !active.insert(id) {
        return Err(Failure::new(
            FailureKind::InvalidPageTree,
            format!("the page tree reaches object {id:?} twice on one path"),
        ));
    }

    let dict = document
        .get_object(id)
        .map_err(|_| {
            Failure::new(
                FailureKind::InvalidReference,
                format!("page-tree object {id:?} does not exist"),
            )
        })?
        .as_dict()
        .map_err(|_| {
            Failure::new(
                FailureKind::InvalidPageTree,
                format!("page-tree object {id:?} is not a dictionary"),
            )
        })?;

    let resources = dict
        .get(b"Resources")
        .ok()
        .map(|object| deref(document, object))
        .and_then(|object| object.as_dict().ok())
        .cloned()
        .or_else(|| inherited.cloned());

    match dict
        .get(b"Type")
        .ok()
        .and_then(|object| object.as_name().ok())
    {
        Some(b"Pages") => {
            let kids = dict
                .get(b"Kids")
                .map_err(|_| {
                    Failure::new(
                        FailureKind::InvalidPageTree,
                        format!("page-tree node {id:?} has no /Kids"),
                    )
                })?
                .as_array()
                .map_err(|_| {
                    Failure::new(
                        FailureKind::InvalidPageTree,
                        format!("page-tree node {id:?} has a non-array /Kids"),
                    )
                })?;
            for kid in kids {
                let kid = kid.as_reference().map_err(|_| {
                    Failure::new(
                        FailureKind::InvalidPageTree,
                        format!("page-tree node {id:?} has a /Kids entry that is not a reference"),
                    )
                })?;
                collect_node(document, kid, resources.as_ref(), depth + 1, active, out)?;
            }
        }
        Some(b"Page") => out.push(PageEntry { id, resources }),
        _ => {
            return Err(Failure::new(
                FailureKind::InvalidPageTree,
                format!("object {id:?} is neither a page nor a page-tree node"),
            ))
        }
    }

    active.remove(&id);
    Ok(())
}

// ------------------------------------------------------------------ checks

/// What one page's content says about text, gathered across its streams and forms.
#[derive(Default)]
struct TextFound {
    any: bool,
    nonblank: bool,
}

struct Checker<'a> {
    doc: &'a Document,
}

impl Checker<'_> {
    /// Whether this page draws text, after checking everything it draws with.
    fn page_has_text(
        &self,
        page: &PageEntry,
        number: u32,
    ) -> std::result::Result<(bool, bool), Failure> {
        let dict = self
            .doc
            .get_object(page.id)
            .map_err(|_| unreachable_page(page.id, number))?
            .as_dict()
            .map_err(|_| unreachable_page(page.id, number))?;

        let contents = content_ids(dict, number)?;
        let resources = page.resources.as_ref();
        let mut found = TextFound::default();
        let mut active_forms = HashSet::new();
        for id in contents {
            let stream = self.stream(id, number, "content stream")?;
            let data = decode_stream(stream).map_err(|failure| failure.on_page(number))?;
            let scan = scan_content(&data, number)?;
            found.any |= scan.has_text;
            found.nonblank |= scan.has_nonblank_text;
            self.check_fonts(&scan.fonts, resources, number)?;
            self.follow_draws(
                &scan.draws,
                resources,
                number,
                &mut active_forms,
                0,
                &mut found,
            )?;
        }
        Ok((found.any, found.nonblank))
    }

    /// Follows the `Do` operators of one content stream into the forms it draws.
    fn follow_draws(
        &self,
        draws: &[Vec<u8>],
        resources: Option<&Dictionary>,
        number: u32,
        active: &mut HashSet<ObjectId>,
        depth: usize,
        found: &mut TextFound,
    ) -> std::result::Result<(), Failure> {
        if draws.is_empty() {
            return Ok(());
        }
        if depth > MAX_FORM_DEPTH {
            return Err(Failure::new(
                FailureKind::RecursionLimit,
                "form XObjects nest deeper than a document plausibly can",
            )
            .on_page(number));
        }

        let xobjects = resources
            .and_then(|resources| resources.get(b"XObject").ok())
            .map(|object| deref(self.doc, object))
            .and_then(|object| object.as_dict().ok())
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::InvalidReference,
                    format!(
                        "the page draws {}, but its resources have no /XObject dictionary",
                        show(&draws[0])
                    ),
                )
                .on_page(number)
            })?;

        for name in draws {
            let object = xobjects.get(name.as_slice()).map_err(|_| {
                Failure::new(
                    FailureKind::InvalidReference,
                    format!(
                        "the page draws {}, which the resources do not define",
                        show(name)
                    ),
                )
                .on_page(number)
            })?;
            let id = object.as_reference().ok();
            let stream = deref(self.doc, object).as_stream().map_err(|_| {
                Failure::new(
                    FailureKind::InvalidReference,
                    format!("XObject {} is not a stream", show(name)),
                )
                .on_page(number)
            })?;

            // Only forms carry content operators; an image is not text and its
            // pixel data is not decoded here.
            if name_of(&stream.dict, b"Subtype") != Some(b"Form") {
                continue;
            }

            if let Some(id) = id {
                if !active.insert(id) {
                    return Err(Failure::new(
                        FailureKind::FormCycle,
                        format!("form {} draws itself", show(name)),
                    )
                    .on_page(number));
                }
            }

            let data = decode_stream(stream).map_err(|failure| failure.on_page(number))?;
            let scan = scan_content(&data, number)?;
            let form_resources = stream
                .dict
                .get(b"Resources")
                .ok()
                .map(|object| deref(self.doc, object))
                .and_then(|object| object.as_dict().ok())
                .or(resources);
            found.any |= scan.has_text;
            found.nonblank |= scan.has_nonblank_text;
            self.check_fonts(&scan.fonts, form_resources, number)?;
            self.follow_draws(
                &scan.draws,
                form_resources,
                number,
                active,
                depth + 1,
                found,
            )?;

            if let Some(id) = id {
                active.remove(&id);
            }
        }
        Ok(())
    }

    /// Every font the content sets text in has to be turnable into characters.
    fn check_fonts(
        &self,
        names: &[Vec<u8>],
        resources: Option<&Dictionary>,
        number: u32,
    ) -> std::result::Result<(), Failure> {
        if names.is_empty() {
            return Ok(());
        }
        let fonts = resources
            .and_then(|resources| resources.get(b"Font").ok())
            .map(|object| deref(self.doc, object))
            .and_then(|object| object.as_dict().ok())
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::InvalidReference,
                    format!(
                        "the content sets text in {}, but the resources have no /Font dictionary",
                        show(&names[0])
                    ),
                )
                .on_page(number)
            })?;

        for name in names {
            let object = fonts.get(name.as_slice()).map_err(|_| {
                Failure::new(
                    FailureKind::InvalidReference,
                    format!(
                        "the content sets text in {}, which the resources do not define",
                        show(name)
                    ),
                )
                .on_page(number)
            })?;
            let font = deref(self.doc, object).as_dict().map_err(|_| {
                Failure::new(
                    FailureKind::InvalidReference,
                    format!("font {} is not a dictionary", show(name)),
                )
                .on_page(number)
            })?;
            self.check_font(name, font, number)?;
        }
        Ok(())
    }

    fn check_font(
        &self,
        name: &[u8],
        font: &Dictionary,
        number: u32,
    ) -> std::result::Result<(), Failure> {
        let to_unicode = font.get(b"ToUnicode").ok().map(|object| {
            let stream = deref(self.doc, object).as_stream().map_err(|_| {
                Failure::new(
                    FailureKind::FontEncoding,
                    format!("font {} has a /ToUnicode that is not a stream", show(name)),
                )
                .on_page(number)
            })?;
            decode_stream(stream)
                .map(|_| ())
                .map_err(|failure| failure.on_page(number))
        });
        if let Some(result) = to_unicode {
            result?;
        }

        if name_of(font, b"Subtype") != Some(b"Type0") {
            // A simple font is read through its built-in or /Encoding table.
            return Ok(());
        }

        match name_of(font, b"Encoding") {
            Some(b"Identity-H") | Some(b"Identity-V") => {}
            Some(encoding) => {
                return Err(Failure::new(
                    FailureKind::FontEncoding,
                    format!(
                        "font {} uses the named CMap {}, which the pipeline cannot decode",
                        show(name),
                        show(encoding)
                    ),
                )
                .on_page(number))
            }
            None => {
                return Err(Failure::new(
                    FailureKind::FontEncoding,
                    format!("Type0 font {} has no usable /Encoding", show(name)),
                )
                .on_page(number))
            }
        }

        if font.get(b"ToUnicode").is_err() {
            return Err(Failure::new(
                FailureKind::FontEncoding,
                format!(
                    "Type0 font {} has no /ToUnicode map, so its codes would become empty text",
                    show(name)
                ),
            )
            .on_page(number));
        }
        Ok(())
    }

    /// Resolves one stream reference, reporting the reference rather than a page.
    fn stream(
        &self,
        id: ObjectId,
        number: u32,
        what: &str,
    ) -> std::result::Result<&Stream, Failure> {
        self.doc
            .get_object(id)
            .map_err(|_| {
                Failure::new(
                    FailureKind::InvalidReference,
                    format!("{what} {id:?} does not exist"),
                )
                .on_page(number)
            })?
            .as_stream()
            .map_err(|_| {
                Failure::new(
                    FailureKind::InvalidReference,
                    format!("{what} {id:?} is not a stream"),
                )
                .on_page(number)
            })
    }
}

fn unreachable_page(id: ObjectId, number: u32) -> Failure {
    Failure::new(
        FailureKind::InvalidPageTree,
        format!("page object {id:?} stopped being a dictionary"),
    )
    .on_page(number)
}

/// The content streams of one page, in order.
fn content_ids(page: &Dictionary, number: u32) -> std::result::Result<Vec<ObjectId>, Failure> {
    match page.get(b"Contents") {
        Ok(Object::Reference(id)) => Ok(vec![*id]),
        Ok(Object::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_reference().map_err(|_| {
                    Failure::new(
                        FailureKind::InvalidReference,
                        "a /Contents array holds an entry that is not a reference",
                    )
                    .on_page(number)
                })
            })
            .collect(),
        // A page without content is a legitimate blank page.
        Ok(Object::Null) | Err(_) => Ok(Vec::new()),
        Ok(_) => Err(Failure::new(
            FailureKind::InvalidReference,
            "/Contents is neither a reference nor an array",
        )
        .on_page(number)),
    }
}

// ---------------------------------------------------------- stream decoding

/// Decodes `stream` through every filter it declares, refusing anything unknown.
fn decode_stream(stream: &Stream) -> std::result::Result<Vec<u8>, Failure> {
    let filters = filters_of(&stream.dict)?;
    let mut data = stream.content.clone();
    for filter in &filters {
        data = match filter.as_slice() {
            b"FlateDecode" | b"Fl" => inflate(&data)?,
            b"ASCIIHexDecode" | b"AHx" => ascii_hex(&data)?,
            b"ASCII85Decode" | b"A85" => ascii85(&data)?,
            other => {
                return Err(Failure::new(
                    FailureKind::StreamDecode,
                    format!("the stream declares the unsupported filter {}", show(other)),
                ))
            }
        };
    }
    check_predictor(&stream.dict)?;
    Ok(data)
}

fn filters_of(dict: &Dictionary) -> std::result::Result<Vec<Vec<u8>>, Failure> {
    match dict.get(b"Filter") {
        Err(_) => Ok(Vec::new()),
        Ok(Object::Name(name)) => Ok(vec![name.clone()]),
        Ok(Object::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_name().map(Vec::from).map_err(|_| {
                    Failure::new(
                        FailureKind::StreamDecode,
                        "a /Filter array holds an entry that is not a name",
                    )
                })
            })
            .collect(),
        Ok(_) => Err(Failure::new(
            FailureKind::StreamDecode,
            "/Filter is neither a name nor an array",
        )),
    }
}

/// Refuses a stream whose predictor this module cannot reverse.
///
/// Content streams do not use predictors; the streams that do (cross-reference
/// tables) are never decoded here, so refusing one is a safety net, not a loss.
fn check_predictor(dict: &Dictionary) -> std::result::Result<(), Failure> {
    let Ok(parms) = dict.get(b"DecodeParms") else {
        return Ok(());
    };
    let dicts: Vec<&Object> = match parms {
        Object::Dictionary(_) => vec![parms],
        Object::Array(items) => items.iter().collect(),
        _ => return Ok(()),
    };
    for parm in dicts {
        let Ok(parm) = parm.as_dict() else {
            continue;
        };
        let predictor = parm
            .get(b"Predictor")
            .ok()
            .and_then(|object| object.as_i64().ok())
            .unwrap_or(1);
        if predictor > 1 {
            return Err(Failure::new(
                FailureKind::StreamDecode,
                format!(
                    "the stream uses predictor {predictor}, which this pipeline does not decode"
                ),
            ));
        }
    }
    Ok(())
}

fn inflate(data: &[u8]) -> std::result::Result<Vec<u8>, Failure> {
    let mut out = Vec::new();
    ZlibDecoder::new(data)
        .read_to_end(&mut out)
        .map_err(|error| {
            Failure::new(
                FailureKind::StreamDecode,
                format!("a Flate stream does not decompress: {error}"),
            )
        })?;
    Ok(out)
}

fn ascii_hex(data: &[u8]) -> std::result::Result<Vec<u8>, Failure> {
    let mut out = Vec::new();
    let mut high: Option<u8> = None;
    for &byte in data {
        match byte {
            b'>' => break,
            byte if byte.is_ascii_whitespace() => continue,
            byte if byte.is_ascii_hexdigit() => {
                let value = (byte as char).to_digit(16).unwrap_or(0) as u8;
                match high.take() {
                    Some(previous) => out.push(previous << 4 | value),
                    None => high = Some(value),
                }
            }
            _ => {
                return Err(Failure::new(
                    FailureKind::StreamDecode,
                    "an ASCIIHex stream holds a non-hex byte",
                ))
            }
        }
    }
    if let Some(previous) = high {
        out.push(previous << 4);
    }
    Ok(out)
}

fn ascii85(data: &[u8]) -> std::result::Result<Vec<u8>, Failure> {
    let mut out = Vec::new();
    let mut group = [0u8; 5];
    let mut count = 0usize;
    for &byte in data {
        match byte {
            b'~' => break,
            byte if byte.is_ascii_whitespace() => continue,
            b'z' if count == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'!'..=b'u' => {
                group[count] = byte - b'!';
                count += 1;
                if count == 5 {
                    let value = group.iter().fold(0u32, |acc, digit| {
                        acc.wrapping_mul(85).wrapping_add(u32::from(*digit))
                    });
                    out.extend_from_slice(&value.to_be_bytes());
                    count = 0;
                }
            }
            _ => {
                return Err(Failure::new(
                    FailureKind::StreamDecode,
                    "an ASCII85 stream holds a byte outside the alphabet",
                ))
            }
        }
    }
    if count > 0 {
        let mut value = 0u32;
        for (index, digit) in group.iter().enumerate() {
            // The encoder pads a short tail with the highest digit, `u`.
            let digit = if index < count { *digit } else { 84 };
            value = value.wrapping_mul(85).wrapping_add(u32::from(digit));
        }
        out.extend_from_slice(&value.to_be_bytes()[..count - 1]);
    }
    Ok(out)
}

// --------------------------------------------------------- content scanner

/// What one content stream (or form) says about text.
#[derive(Default)]
struct Scan {
    has_text: bool,
    has_nonblank_text: bool,
    draws: Vec<Vec<u8>>,
    fonts: Vec<Vec<u8>>,
}

/// One operand the tokeniser kept: only names matter to the checks above.
enum Operand {
    Name(Vec<u8>),
    /// A string operand; `solid` is false when it draws only blanks.
    Text {
        solid: bool,
    },
    Other,
}

/// Tokenises a content stream strictly, collecting text, draws and fonts.
fn scan_content(content: &[u8], page: u32) -> std::result::Result<Scan, Failure> {
    let mut scan = Scan::default();
    let mut operands: Vec<Operand> = Vec::new();
    let mut array_depth = 0usize;
    let mut dict_depth = 0usize;
    let mut index = 0;

    while index < content.len() {
        match content[index] {
            b'%' => index = skip_comment(content, index + 1),
            b'(' => {
                let end = literal_string_end(content, index + 1).ok_or_else(|| {
                    Failure::new(
                        FailureKind::ContentDecode,
                        "a content stream ends inside a literal string",
                    )
                    .on_page(page)
                })?;
                let solid = literal_is_solid(&content[index + 1..end - 1]);
                operands.push(Operand::Text { solid });
                index = end;
            }
            b'<' if content.get(index + 1) == Some(&b'<') => {
                dict_depth += 1;
                index += 2;
            }
            b'>' if content.get(index + 1) == Some(&b'>') => {
                if dict_depth == 0 {
                    return Err(Failure::new(
                        FailureKind::ContentDecode,
                        "a content stream closes a dictionary it never opened",
                    )
                    .on_page(page));
                }
                dict_depth -= 1;
                index += 2;
            }
            b'<' => {
                let end = hex_string_end(content, index + 1).ok_or_else(|| {
                    Failure::new(
                        FailureKind::ContentDecode,
                        "a content stream ends inside a hex string",
                    )
                    .on_page(page)
                })?;
                let solid = hex_is_solid(&content[index + 1..end - 1]);
                operands.push(Operand::Text { solid });
                index = end;
            }
            b'>' => {
                return Err(Failure::new(
                    FailureKind::ContentDecode,
                    "a content stream holds a stray '>'",
                )
                .on_page(page))
            }
            b'[' => {
                array_depth += 1;
                index += 1;
            }
            b']' => {
                if array_depth == 0 {
                    return Err(Failure::new(
                        FailureKind::ContentDecode,
                        "a content stream closes an array it never opened",
                    )
                    .on_page(page));
                }
                array_depth -= 1;
                index += 1;
            }
            b')' | b'{' | b'}' => index += 1,
            b'/' => {
                let (end, name) = read_name(content, index + 1);
                index = end;
                operands.push(Operand::Name(name));
            }
            byte if is_space(byte) => index += 1,
            _ => {
                let start = index;
                while index < content.len() && !is_token_boundary(content[index]) {
                    index += 1;
                }
                if start == index {
                    index += 1;
                    continue;
                }
                let token = &content[start..index];
                if is_operator(token) {
                    if token == b"BI" {
                        index = skip_inline_image(content, index, page)?;
                    } else {
                        record_operator(token, &operands, &mut scan);
                    }
                    operands.clear();
                } else {
                    operands.push(Operand::Other);
                }
            }
        }
        if operands.len() > MAX_OPERANDS {
            operands.clear();
        }
    }

    if dict_depth > 0 {
        return Err(Failure::new(
            FailureKind::ContentDecode,
            "a content stream ends inside a dictionary",
        )
        .on_page(page));
    }
    if array_depth > 0 {
        return Err(Failure::new(
            FailureKind::ContentDecode,
            "a content stream ends inside an array",
        )
        .on_page(page));
    }
    Ok(scan)
}

/// Whether a literal string's body draws a non-blank character.
fn literal_is_solid(raw: &[u8]) -> bool {
    let mut index = 0;
    while index < raw.len() {
        let byte = raw[index];
        if byte == b'\\' {
            let Some(&next) = raw.get(index + 1) else {
                break;
            };
            match next {
                b'n' | b'r' | b't' | b'b' | b'f' => index += 2,
                b'0'..=b'7' => {
                    let mut value = 0u32;
                    let mut used = 0;
                    while used < 3 {
                        match raw.get(index + 1 + used) {
                            Some(digit @ b'0'..=b'7') => {
                                value = value * 8 + u32::from(digit - b'0');
                                used += 1;
                            }
                            _ => break,
                        }
                    }
                    if value != 0 && value != 0x20 {
                        return true;
                    }
                    index += 1 + used;
                }
                b'\n' | b'\r' => index += 2,
                _ => return next != b' ',
            }
            continue;
        }
        if !is_space(byte) {
            return true;
        }
        index += 1;
    }
    false
}

/// Whether a hex string draws a non-blank character.
fn hex_is_solid(raw: &[u8]) -> bool {
    let mut pending: Option<u8> = None;
    for &byte in raw {
        let Some(digit) = (byte as char).to_digit(16).map(|digit| digit as u8) else {
            continue;
        };
        match pending.take() {
            None => pending = Some(digit),
            Some(high) => {
                let value = (high << 4) | digit;
                if value != 0 && value != 0x20 {
                    return true;
                }
            }
        }
    }
    false
}

/// Records what one operator says about text, draws and fonts.
fn record_operator(token: &[u8], operands: &[Operand], scan: &mut Scan) {
    match token {
        b"Tj" | b"TJ" | b"'" | b"\"" => {
            scan.has_text = true;
            // Every string in a `TJ` array is drawn, so any solid one makes
            // the page carry visible text; looking only at the last string let
            // a trailing blank hide the visible one before it.
            scan.has_nonblank_text |= operands
                .iter()
                .any(|operand| matches!(operand, Operand::Text { solid: true }));
        }
        b"Do" => {
            if let Some(Operand::Name(name)) = operands.last() {
                scan.draws.push(name.clone());
            }
        }
        b"Tf" => {
            let length = operands.len();
            if length >= 2 {
                if let Operand::Name(name) = &operands[length - 2] {
                    scan.fonts.push(name.clone());
                }
            }
        }
        _ => {}
    }
}

/// Skips `BI … ID <binary> EI`, returning the index after `EI`.
fn skip_inline_image(
    content: &[u8],
    mut index: usize,
    page: u32,
) -> std::result::Result<usize, Failure> {
    while index < content.len() {
        if content[index..].starts_with(b"ID")
            && index > 0
            && is_token_boundary(content[index - 1])
            && content
                .get(index + 2)
                .is_none_or(|byte| is_token_boundary(*byte))
        {
            index += 2;
            // The spec requires exactly one whitespace byte after `ID`.
            if content.get(index).is_some_and(|byte| is_space(*byte)) {
                index += 1;
            }
            while index < content.len() {
                if content[index] == b'E'
                    && content.get(index + 1) == Some(&b'I')
                    && index > 0
                    && is_space(content[index - 1])
                {
                    return Ok(index + 2);
                }
                index += 1;
            }
            return Err(Failure::new(
                FailureKind::ContentDecode,
                "an inline image has no EI terminator",
            )
            .on_page(page));
        }
        index += 1;
    }
    Err(Failure::new(
        FailureKind::ContentDecode,
        "a content stream opens an inline image without its ID",
    )
    .on_page(page))
}

// ------------------------------------------------------------- token helpers

fn is_operator(token: &[u8]) -> bool {
    let Some(first) = token.first() else {
        return false;
    };
    first.is_ascii_alphabetic() || *first == b'\'' || *first == b'"'
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

fn is_token_boundary(byte: u8) -> bool {
    is_space(byte) || is_delimiter(byte)
}

fn skip_comment(content: &[u8], mut index: usize) -> usize {
    while index < content.len() && content[index] != b'\n' && content[index] != b'\r' {
        index += 1;
    }
    index
}

/// Reads a `/Name`, decoding `#xx` escapes; returns the end and the bytes.
fn read_name(content: &[u8], mut index: usize) -> (usize, Vec<u8>) {
    let mut name = Vec::new();
    while index < content.len() && !is_space(content[index]) && !is_delimiter(content[index]) {
        if content[index] == b'#'
            && index + 2 < content.len()
            && content[index + 1].is_ascii_hexdigit()
            && content[index + 2].is_ascii_hexdigit()
        {
            let high = (content[index + 1] as char).to_digit(16).unwrap_or(0) as u8;
            let low = (content[index + 2] as char).to_digit(16).unwrap_or(0) as u8;
            name.push(high << 4 | low);
            index += 3;
            continue;
        }
        name.push(content[index]);
        index += 1;
    }
    (index, name)
}

/// The index after a `)` closing the string opened before `index`.
fn literal_string_end(content: &[u8], mut index: usize) -> Option<usize> {
    let mut depth = 1usize;
    while index < content.len() {
        match content[index] {
            b'\\' => index = (index + 2).min(content.len()),
            b'(' => {
                depth += 1;
                index += 1;
            }
            b')' => {
                depth -= 1;
                index += 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => index += 1,
        }
    }
    None
}

/// The index after the `>` closing the hex string opened before `index`.
fn hex_string_end(content: &[u8], mut index: usize) -> Option<usize> {
    while index < content.len() {
        if content[index] == b'>' {
            return Some(index + 1);
        }
        index += 1;
    }
    None
}

// ---------------------------------------------------------------- utilities

/// Resolves a reference one level; anything else is returned as it is.
fn deref<'a>(document: &'a Document, object: &'a Object) -> &'a Object {
    match object {
        Object::Reference(id) => match document.get_object(*id) {
            Ok(object) => object,
            Err(_) => object,
        },
        other => other,
    }
}

/// The bytes of a `/Name` entry in `dict`, when it has one.
fn name_of<'a>(dict: &'a Dictionary, key: &[u8]) -> Option<&'a [u8]> {
    dict.get(key).ok().and_then(|object| object.as_name().ok())
}

/// Renders a name or filter for a message without trusting it to be UTF-8.
fn show(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
