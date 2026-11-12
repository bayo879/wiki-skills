//! The usability gate every backend result passes before it is published.
//!
//! A backend can return a link, an empty body or a document it only partly read.
//! None of those is a source: publishing one turns "we could not read this" into
//! "this document has no text", which is the failure the PDF issue started from.
//! These tests pin the gate itself (at `parse_with` and at `convert`), the
//! fallback order around it, and the extraction risks this round deliberately
//! does *not* fix, so a later rewrite of the HTML algorithm has a baseline.

mod common;
mod mock_http;

use std::fs;

use llm_wiki::config::{Config, Primary};
use llm_wiki::parse::{self, ParseError, Source, UnavailableReason};
use llm_wiki::store::Store;
use llm_wiki::{convert, init};

/// A fresh wiki with an initialised ledger, for the `convert` cases.
fn setup() -> (tempfile::TempDir, Store, Config) {
    let d = tempfile::tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut store = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    store.migrate().unwrap();
    (d, store, cfg)
}

/// The refusal reason and page of `error`.
fn unavailable(error: ParseError) -> (UnavailableReason, Option<u32>) {
    match error {
        ParseError::Unavailable { reason, page, .. } => (reason, page),
        other => panic!("expected an unavailable source, got {other:?}"),
    }
}

#[test]
fn a_scan_is_refused_instead_of_published_as_a_link() {
    let bytes = common::make_scanned_pdf_bytes(2);
    let error = parse::parse_with(&Config::default(), &Source::inline("scan.pdf", &bytes))
        .expect_err("a scan has no text to publish");
    assert_eq!(
        unavailable(error),
        (UnavailableReason::TextUnavailable, None)
    );
}

#[test]
fn a_body_of_whitespace_is_refused() {
    let error = parse::parse_with(
        &Config::default(),
        &Source::inline("a.md", b"---\na: 1\n---\n"),
    )
    .expect_err("frontmatter alone is not a body");
    assert_eq!(
        unavailable(error),
        (UnavailableReason::TextUnavailable, None)
    );
}

#[test]
fn an_external_answer_without_a_body_is_refused() {
    let srv = mock_http::serve_once(200, &serde_json::json!({ "data": "" }).to_string());
    std::env::set_var("WIKI_PARSE_TOKEN", "test-token-abc");
    let mut cfg = Config::default();
    cfg.parse.primary = Primary::External;
    cfg.parse.external = Some(mock_http::external_at(&srv.url));

    let error = parse::parse_with(&cfg, &Source::inline("a.md", b"body")).expect_err("empty");
    assert_eq!(
        unavailable(error),
        (UnavailableReason::TextUnavailable, None)
    );
}

// The external protocol carries text and nothing else, so a service that
// answers with a bare attachment link is indistinguishable from a document that
// contains one. Only an empty body can be refused; the gap is recorded in the
// task report rather than papered over with a heuristic.

#[test]
fn a_fallback_that_returns_nothing_keeps_both_reasons() {
    let srv = mock_http::serve_once(200, &serde_json::json!({ "data": "  " }).to_string());
    std::env::set_var("WIKI_PARSE_TOKEN", "test-token-abc");
    let mut cfg = Config::default();
    cfg.parse.primary = Primary::Auto;
    cfg.parse.external = Some(mock_http::external_at(&srv.url));

    let bytes = common::make_scanned_pdf_bytes(2);
    let error =
        parse::parse_with(&cfg, &Source::inline("scan.pdf", &bytes)).expect_err("nothing usable");
    match error {
        ParseError::BothFailed { builtin, external } => {
            assert!(builtin.contains("text_unavailable"), "{builtin}");
            assert!(external.contains("text_unavailable"), "{external}");
        }
        other => panic!("expected both reasons, got {other:?}"),
    }
}

#[test]
fn a_scan_is_refused_before_it_reaches_the_raw_directory() {
    let (d, mut store, cfg) = setup();
    let bytes = common::make_scanned_pdf_bytes(2);
    fs::write(d.path().join("scan.pdf"), &bytes).unwrap();

    let error = convert::convert_one(d.path(), &cfg, &mut store, "scan.pdf")
        .expect_err("a scan must fail, not publish");
    assert_eq!(error.code(), "E_BAD_SOURCE");
    assert!(
        !d.path().join(".wiki/raw/scan.pdf").exists(),
        "no raw may exist for a source that was never read"
    );
}

// ------------------------------------------------------- pinned extraction risks
//
// The spec keeps the HTML algorithm unchanged this round: these cases are
// recorded so the loss is visible instead of silent. They assert what happens
// today, and each will change when the multi-container rewrite lands.

#[test]
fn only_the_first_article_is_published() {
    let html = b"<article><p>first body</p></article><article><p>second body</p></article>";
    let (extracted, _) = parse::parse_with(&Config::default(), &Source::inline("a.html", html))
        .expect("the first article is a real body");
    assert!(extracted.markdown.contains("first body"), "{extracted:?}");
    assert!(
        !extracted.markdown.contains("second body"),
        "the second article is dropped today; the rewrite must add it back"
    );
}

#[test]
fn table_cells_survive_without_their_structure() {
    let html = b"<article><table><tr><td>left</td><td>right</td></tr></table></article>";
    let (extracted, _) = parse::parse_with(&Config::default(), &Source::inline("a.html", html))
        .expect("cell text is a real body");
    assert!(extracted.markdown.contains("left"), "{extracted:?}");
    assert!(extracted.markdown.contains("right"), "{extracted:?}");
    assert!(
        !extracted.markdown.contains('|'),
        "cells are paragraphs today, not a markdown table"
    );
}

#[test]
fn invalid_utf8_in_a_body_keeps_the_readable_part() {
    let mut bytes = Vec::from(&b"good text "[..]);
    bytes.push(0xff);
    let (extracted, _) = parse::parse_with(&Config::default(), &Source::inline("a.txt", &bytes))
        .expect("the readable part is a real body");
    assert!(extracted.markdown.contains("good text"), "{extracted:?}");
    assert!(
        extracted
            .warnings
            .iter()
            .any(|warning| warning.code == "bad_utf8"),
        "the loss must be reported: {:?}",
        extracted.warnings
    );
}
