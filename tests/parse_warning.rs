//! `below_min_chars` is emitted only when a backend had to interpret a format.
//!
//! The warning exists so an agent can tell "this HTML/PDF extraction came back
//! almost empty" apart from "this note really is that short". `builtin-text`
//! copies bytes and assets are links by design, so neither is warned about.

use llm_wiki::config::Config;
use llm_wiki::parse::{parse_with, Source};

fn codes(cfg: &Config, key: &str, bytes: &[u8]) -> Vec<String> {
    let (extracted, _) = parse_with(cfg, &Source::inline(key, bytes)).expect("parses");
    extracted
        .warnings
        .into_iter()
        .map(|warning| warning.code)
        .collect()
}

#[test]
fn short_html_warns_below_min_chars() {
    let cfg = Config::default();
    let got = codes(&cfg, "x.html", b"<article><p>hi</p></article>");
    assert!(
        got.iter().any(|code| code == "below_min_chars"),
        "short html extraction should be flagged: {got:?}"
    );
}

#[test]
fn long_html_does_not_warn() {
    let cfg = Config::default();
    let body = "正文".repeat(cfg.parse.min_chars);
    let html = format!("<article><p>{body}</p></article>");
    let got = codes(&cfg, "x.html", html.as_bytes());
    assert!(
        !got.iter().any(|code| code == "below_min_chars"),
        "a full extraction is not thin: {got:?}"
    );
}

#[test]
fn short_markdown_is_never_warned_about() {
    let cfg = Config::default();
    let got = codes(&cfg, "note.md", "短\n".as_bytes());
    assert!(
        !got.iter().any(|code| code == "below_min_chars"),
        "a short note is short on purpose: {got:?}"
    );
}

#[test]
fn an_asset_is_refused_instead_of_warned_about() {
    let cfg = Config::default();
    let err = parse_with(&cfg, &Source::inline("a.png", b"\x89PNG\r\n\x1a\n")).unwrap_err();
    assert_eq!(err.code(), "E_BAD_SOURCE");
    assert!(
        err.to_string().contains("text_unavailable"),
        "an image is a link, never a source: {err}"
    );
}
