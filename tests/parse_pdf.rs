//! Signature tests for the `builtin-pdf` backend and the text-layer probe.
//!
//! The first three tests are the plan's original scenarios. The rest cover what
//! the plan leaves implicit but that ingestion depends on: a broken PDF must be
//! an error rather than a panic, the probe must tokenise content streams instead
//! of matching bytes, sampling must not assume six pages exist, a text-free PDF
//! must degrade to the same link the asset backend emits, and the parser must
//! work from the bytes it was handed rather than from the filesystem.
//!
//! Fixtures are built with `lopdf` (see `tests/common/mod.rs`), so every
//! assertion runs against a real PDF document.

mod common;

use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};

use llm_wiki::config::Config;
use llm_wiki::parse::pdf::{has_text_layer, PdfParser};
use llm_wiki::parse::{self, Parser, Source};
use tempfile::tempdir;

#[test]
fn text_pdf_has_text_layer() {
    let d = tempdir().unwrap();
    let p = d.path().join("t.pdf");
    common::make_pdf(&p, "hello world", 4);
    assert!(has_text_layer(&p).unwrap());
    let out = PdfParser::new().parse(&Source::path(&p).unwrap()).unwrap();
    assert!(out.markdown.contains("hello world"));
    assert!(!out.asset);
}

#[test]
fn scanned_pdf_degrades_to_asset_link() {
    let d = tempdir().unwrap();
    let p = d.path().join("scan.pdf");
    common::make_scanned_pdf(&p, 6);
    assert!(!has_text_layer(&p).unwrap());
    let out = PdfParser::new().parse(&Source::path(&p).unwrap()).unwrap();
    assert!(out.asset);
    assert!(out.no_text_layer);
    assert!(
        out.markdown.contains("](../source/scan.pdf)"),
        "key 只有一层，前缀是一个 ../: {:?}",
        out.markdown
    );
}

#[test]
fn cover_scan_with_text_body_is_not_degraded() {
    let d = tempdir().unwrap();
    let p = d.path().join("mixed.pdf");
    common::make_mixed_pdf(&p);
    assert!(has_text_layer(&p).unwrap());
}

#[test]
fn declares_id_and_claims_pdf_only() {
    let parser = PdfParser::new();
    assert_eq!(parser.id(), "builtin-pdf");
    for ext in ["pdf", "PDF", "Pdf"] {
        assert!(parser.can_handle(ext), "{ext} must be claimed");
    }
    for ext in ["md", "txt", "html", "png", ""] {
        assert!(!parser.can_handle(ext), "{ext} must not be claimed");
    }
}

#[test]
fn registry_binds_the_pdf_backend() {
    let cfg = Config::default();
    let bytes = common::make_pdf_bytes("registered text", 1);
    let (extracted, id) = parse::parse_with(&cfg, &parse::Source::inline("a.pdf", &bytes)).unwrap();
    assert_eq!(id, "builtin-pdf");
    assert!(extracted.markdown.contains("registered text"));
    assert!(!extracted.asset);
}

#[test]
fn broken_pdf_is_an_error_not_a_crash() {
    // The bytes are not a PDF at all: loading fails, so the probe says "assume
    // there is text" and the extractor is the one that reports the real error.
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        PdfParser::new().parse(&Source::inline(
            "bad.pdf",
            b"%PDF-1.4\n\xe5\x9e\x83\xe5\x9c\xbe",
        ))
    }))
    .expect("a bad pdf must not let a panic escape");
    assert!(outcome.is_err(), "a bad pdf must be an error: {outcome:?}");
}

#[test]
fn extraction_panic_is_an_error_not_a_crash() {
    // This one loads fine and does have a text layer, but `pdf-extract` panics
    // on the missing `MediaBox`. `catch_unwind` is what turns that into `Err`.
    let bytes = common::make_pdf_missing_media_box_bytes();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        PdfParser::new().parse(&Source::inline("no-media-box.pdf", &bytes))
    }))
    .expect("an extractor panic must not escape the backend");
    assert!(
        outcome.is_err(),
        "a document the extractor panics on must be an error: {outcome:?}"
    );
}

#[test]
fn strings_are_not_mistaken_for_text_operators() {
    let d = tempdir().unwrap();

    // No text-showing operator, but `(Tj)` and `<544A>` as operands.
    let probe = d.path().join("strings.pdf");
    common::make_scanned_pdf_with_text_in_strings(&probe);
    assert!(
        !has_text_layer(&probe).unwrap(),
        "operands spelling Tj must not count as a text layer"
    );

    // A real `Tj` next to a body that spells the operator names.
    let text = d.path().join("body.pdf");
    common::make_pdf(&text, "Tj TJ", 2);
    assert!(has_text_layer(&text).unwrap());
}

#[test]
fn probe_handles_fewer_than_six_pages() {
    let d = tempdir().unwrap();
    for pages in [1usize, 2, 3, 5] {
        let p = d.path().join(format!("{pages}p.pdf"));
        common::make_pdf(&p, "short", pages);
        assert!(
            has_text_layer(&p).is_ok(),
            "{pages}-page documents must not make the probe fail"
        );
    }
}

#[test]
fn single_page_pdf_extracts() {
    let d = tempdir().unwrap();
    let p = d.path().join("one.pdf");
    common::make_pdf(&p, "only page", 1);
    let out = PdfParser::new().parse(&Source::path(&p).unwrap()).unwrap();
    assert!(out.markdown.contains("only page"), "{:?}", out.markdown);
    assert!(!out.asset);
    assert!(!out.no_text_layer);
}

#[test]
fn probe_leaves_the_file_and_its_directory_alone() {
    let d = tempdir().unwrap();
    let p = d.path().join("scan.pdf");
    common::make_scanned_pdf(&p, 6);
    let before = fs::read(&p).unwrap();
    let stamp = fs::metadata(&p).unwrap().modified().unwrap();

    assert!(!has_text_layer(&p).unwrap());

    assert_eq!(fs::read(&p).unwrap(), before, "the file was rewritten");
    assert_eq!(
        fs::metadata(&p).unwrap().modified().unwrap(),
        stamp,
        "the file was touched"
    );
    assert_eq!(
        fs::read_dir(d.path()).unwrap().count(),
        1,
        "the probe left files behind"
    );
}

#[test]
fn source_path_uses_the_file_name_as_key() {
    let d = tempdir().unwrap();
    let p = d.path().join("x/scan.pdf");
    common::write(&p, b"%PDF-1.4\n");

    let src = Source::path(&p).unwrap();
    assert_eq!(src.key, "scan.pdf");
    assert_eq!(src.ext, "pdf");
    assert_eq!(src.bytes, b"%PDF-1.4\n");
}

#[test]
fn source_path_reports_a_missing_file() {
    let d = tempdir().unwrap();
    let err = Source::path(&d.path().join("gone.pdf")).unwrap_err();
    assert_eq!(err.code(), "E_IO");
}

#[test]
fn parser_works_from_the_bytes_it_was_handed() {
    let d = tempdir().unwrap();
    let p = d.path().join("ghost.pdf");
    let bytes = common::make_pdf_bytes("in memory only", 2);
    common::write(&p, &bytes);
    // Remove the file: a backend that re-reads the path for the probe would now
    // fail, while one that uses `Source.bytes` keeps working.
    fs::remove_file(&p).unwrap();
    assert!(!p.exists());

    let out = PdfParser::new()
        .parse(&Source::inline("ghost.pdf", &bytes))
        .unwrap();
    assert!(
        out.markdown.contains("in memory only"),
        "{:?}",
        out.markdown
    );
}

#[test]
fn text_pdf_reports_that_the_page_split_is_unknown() {
    let d = tempdir().unwrap();
    let p = d.path().join("pages.pdf");
    common::make_pdf(&p, "multi page", 3);
    let out = PdfParser::new().parse(&Source::path(&p).unwrap()).unwrap();
    assert!(
        out.warnings.iter().any(|w| w.code == "page_split_unknown"),
        "expected a `page_split_unknown` warning, got {:?}",
        out.warnings
    );
}

#[test]
fn pdf_with_no_extractable_text_warns_empty() {
    let d = tempdir().unwrap();
    let p = d.path().join("blank.pdf");
    // The `Tj` operator is there, so this is not a scanned document; it just
    // shows an empty string.
    common::make_pdf(&p, "", 2);
    assert!(has_text_layer(&p).unwrap());
    let out = PdfParser::new().parse(&Source::path(&p).unwrap()).unwrap();
    assert!(!out.asset);
    assert!(!out.no_text_layer);
    assert!(
        out.warnings.iter().any(|w| w.code == "empty"),
        "expected an `empty` warning, got {:?}",
        out.warnings
    );
}

#[test]
fn scanned_pdf_is_not_run_through_the_extractor() {
    let d = tempdir().unwrap();
    let p = d.path().join("scan.pdf");
    common::make_scanned_pdf(&p, 6);
    let out = PdfParser::new().parse(&Source::path(&p).unwrap()).unwrap();
    assert!(out.warnings.iter().any(|w| w.code == "no_text_layer"));
    // A scanned page has no text to be empty about.
    assert!(!out.warnings.iter().any(|w| w.code == "empty"));
}

#[test]
fn mixed_pdf_extracts_the_one_text_page() {
    let d = tempdir().unwrap();
    let p = d.path().join("mixed.pdf");
    common::make_mixed_pdf(&p);
    let out = PdfParser::new().parse(&Source::path(&p).unwrap()).unwrap();
    assert!(
        out.markdown.contains("fifth page body"),
        "{:?}",
        out.markdown
    );
    assert!(!out.asset);
    assert!(!out.no_text_layer);
}
