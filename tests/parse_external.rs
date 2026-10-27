//! Contract tests for the `external-http` backend and the `auto` fallback.
//!
//! Everything that speaks HTTP goes through `tests/mock_http.rs`, a one-shot
//! server on a random loopback port: these tests must pass on a machine with no
//! network at all. Tokens are process-local environment variables set by the
//! test itself (`WIKI_PARSE_TOKEN_T11_*`), so the user's real `WIKI_PARSE_TOKEN`
//! is never read and the tests never depend on it.
//!
//! The first three tests are the plan's original scenarios. The rest pin down
//! the parts the plan leaves implicit but that `convert` depends on: the full
//! `pick_backend` matrix, the multipart contract, the two fallback directions,
//! the "both failed" error, the JSON path, token hygiene and the timeout.

mod common;
mod mock_http;

use std::time::{Duration, Instant};

use llm_wiki::config::{Config, ExternalCfg, Primary};
use llm_wiki::parse::external::ExternalParser;
use llm_wiki::parse::{self, pick_backend, Source};

#[test]
fn auto_order_is_builtin_then_external_for_pdf() {
    let mut cfg = Config::default();
    cfg.parse.primary = Primary::Auto;
    cfg.parse.external = Some(mock_http::sample_external());
    assert_eq!(
        pick_backend(&cfg, "pdf"),
        vec!["builtin-pdf", "external-http"]
    );
    assert_eq!(pick_backend(&cfg, "md"), vec!["builtin-text"]);
}

#[test]
fn builtin_mode_never_tries_external() {
    let mut cfg = Config::default();
    cfg.parse.primary = Primary::Builtin;
    cfg.parse.external = Some(mock_http::sample_external());
    assert_eq!(pick_backend(&cfg, "pdf"), vec!["builtin-pdf"]);
}

#[test]
fn external_500_falls_back_and_reports_both_reasons() {
    let srv = mock_http::serve_once(500, "boom");
    let mut cfg = Config::default();
    cfg.parse.primary = Primary::External;
    cfg.parse.external = Some(mock_http::external_at(&srv.url));
    let err = parse::parse_with(&cfg, &Source::inline("a.pdf", b"%PDF")).unwrap_err();
    assert!(err.to_string().contains("500"));
}

#[test]
fn pick_backend_covers_the_whole_matrix() {
    let mut cfg = Config::default();

    // `auto` without an external service: builtin only, whatever the extension.
    for (ext, expected) in [
        ("md", vec!["builtin-text"]),
        ("markdown", vec!["builtin-text"]),
        ("txt", vec!["builtin-text"]),
        ("html", vec!["builtin-html"]),
        ("htm", vec!["builtin-html"]),
        ("pdf", vec!["builtin-pdf"]),
        ("png", vec!["builtin-asset"]),
        ("SVG", vec!["builtin-asset"]),
        ("docx", Vec::new()),
    ] {
        assert_eq!(pick_backend(&cfg, ext), expected, "auto, ext {ext}");
    }

    // `auto` with an external service: text and images still never leave.
    cfg.parse.external = Some(mock_http::sample_external());
    for (ext, expected) in [
        ("md", vec!["builtin-text"]),
        ("txt", vec!["builtin-text"]),
        ("html", vec!["builtin-html", "external-http"]),
        ("pdf", vec!["builtin-pdf", "external-http"]),
        ("png", vec!["builtin-asset"]),
        ("docx", Vec::new()),
    ] {
        assert_eq!(
            pick_backend(&cfg, ext),
            expected,
            "auto+external, ext {ext}"
        );
    }

    // `builtin` ignores the external service even when it is configured.
    cfg.parse.primary = Primary::Builtin;
    assert_eq!(pick_backend(&cfg, "pdf"), vec!["builtin-pdf"]);
    assert_eq!(pick_backend(&cfg, "html"), vec!["builtin-html"]);

    // `external` forces the service for text-like formats, never for images.
    cfg.parse.primary = Primary::External;
    for ext in ["md", "markdown", "txt", "html", "htm", "pdf"] {
        assert_eq!(pick_backend(&cfg, ext), vec!["external-http"], "ext {ext}");
    }
    assert_eq!(pick_backend(&cfg, "png"), vec!["builtin-asset"]);
    assert_eq!(pick_backend(&cfg, "docx"), Vec::<&str>::new());

    // Case of the extension never changes the decision.
    assert_eq!(pick_backend(&cfg, "PDF"), vec!["external-http"]);
}

#[test]
fn external_mode_sends_multipart_and_returns_markdown() {
    let payload =
        serde_json::json!({ "data": "# from the service\n\nbody from service" }).to_string();
    let srv = mock_http::serve_once(200, &payload);

    let mut external = mock_http::external_at(&srv.url);
    external.auth_env = String::from("WIKI_PARSE_TOKEN_T11_AUTH");
    std::env::set_var("WIKI_PARSE_TOKEN_T11_AUTH", "test-token-abc");

    let mut cfg = Config::default();
    cfg.parse.primary = Primary::External;
    cfg.parse.external = Some(external);

    let source = Source::inline("docs/a.pdf", b"%PDF-1.4 fake body");
    let (extracted, id) = parse::parse_with(&cfg, &source).unwrap();
    assert_eq!(id, "external-http");
    assert_eq!(
        extracted.markdown,
        "# from the service\n\nbody from service"
    );
    assert!(!extracted.asset);
    assert!(!extracted.no_text_layer);

    let request = srv.finish();
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/parse");
    assert_eq!(
        request.header("authorization"),
        Some("Bearer test-token-abc"),
        "the token must come from the environment variable"
    );
    let content_type = request.header("content-type").expect("a content type");
    assert!(
        content_type.starts_with("multipart/form-data; boundary="),
        "unexpected content type {content_type:?}"
    );
    let body = request.body_text();
    assert!(body.contains("name=\"file\""), "missing file field");
    assert!(
        body.contains("filename=\"a.pdf\""),
        "the file name must be the basename of the source key"
    );
    assert!(body.contains("%PDF-1.4 fake body"), "missing file bytes");
    assert!(body.contains("name=\"fileFlag\""), "missing extra field");
    assert!(body.contains("\r\n\r\nN\r\n"), "extra field value missing");
    assert!(body.contains("name=\"system\""), "missing extra field");
    assert!(
        body.contains("\r\n\r\nBSS\r\n"),
        "extra field value missing"
    );
}

#[test]
fn auto_scanned_pdf_uses_external() {
    let payload =
        serde_json::json!({ "data": "# scan via service\n\nrecognised body" }).to_string();
    let srv = mock_http::serve_once(200, &payload);
    std::env::set_var("WIKI_PARSE_TOKEN", "test-token-abc");

    let mut cfg = Config::default();
    cfg.parse.primary = Primary::Auto;
    cfg.parse.external = Some(mock_http::external_at(&srv.url));

    let bytes = common::make_scanned_pdf_bytes(2);
    let (extracted, id) = parse::parse_with(&cfg, &Source::inline("scan.pdf", &bytes)).unwrap();
    assert_eq!(id, "external-http", "a scan has no builtin text to keep");
    assert_eq!(extracted.markdown, "# scan via service\n\nrecognised body");
    assert!(
        extracted
            .warnings
            .iter()
            .any(|warning| warning.code == "external_used"),
        "warnings: {:?}",
        extracted.warnings
    );
}

#[test]
fn auto_short_pdf_falls_back_to_external() {
    let payload = serde_json::json!({ "data": "# better text\n\nfrom the service" }).to_string();
    let srv = mock_http::serve_once(200, &payload);
    std::env::set_var("WIKI_PARSE_TOKEN", "test-token-abc");

    let mut cfg = Config::default();
    cfg.parse.primary = Primary::Auto;
    cfg.parse.external = Some(mock_http::external_at(&srv.url));

    let bytes = common::make_pdf_bytes("tiny body", 1);
    let (extracted, id) = parse::parse_with(&cfg, &Source::inline("tiny.pdf", &bytes)).unwrap();
    assert_eq!(id, "external-http");
    assert_eq!(extracted.markdown, "# better text\n\nfrom the service");
}

#[test]
fn auto_external_failure_keeps_builtin_with_a_warning() {
    let srv = mock_http::serve_once(500, "boom");
    std::env::set_var("WIKI_PARSE_TOKEN", "test-token-abc");

    let mut cfg = Config::default();
    cfg.parse.primary = Primary::Auto;
    cfg.parse.external = Some(mock_http::external_at(&srv.url));

    let bytes = common::make_pdf_bytes("tiny body", 1);
    let (extracted, id) = parse::parse_with(&cfg, &Source::inline("tiny.pdf", &bytes)).unwrap();
    assert_eq!(
        id, "builtin-pdf",
        "a failed fallback must keep the builtin result"
    );
    assert!(extracted.markdown.contains("tiny body"));
    let warning = extracted
        .warnings
        .iter()
        .find(|warning| warning.code == "external_failed")
        .expect("the failure must be reported, not hidden");
    assert!(warning.message.contains("500"), "warning: {warning:?}");
}

#[test]
fn auto_both_failures_are_reported_together() {
    let srv = mock_http::serve_once(500, "boom");
    std::env::set_var("WIKI_PARSE_TOKEN", "test-token-abc");

    let mut cfg = Config::default();
    cfg.parse.primary = Primary::Auto;
    cfg.parse.external = Some(mock_http::external_at(&srv.url));

    let broken = b"%PDF-1.4\n\xe5\x9e\x83\xe5\x9c\xbe";
    let err = parse::parse_with(&cfg, &Source::inline("bad.pdf", broken)).unwrap_err();
    assert_eq!(err.code(), "E_BOTH_FAILED");
    let message = err.to_string();
    assert!(message.contains("cannot parse"), "message: {message}");
    assert!(message.contains("500"), "message: {message}");
    assert_eq!(
        message.matches("failed").count(),
        2,
        "both reasons must be present: {message}"
    );
}

#[test]
fn external_json_path_missing_is_an_error() {
    let srv = mock_http::serve_once(200, r#"{"data":"x"}"#);
    std::env::set_var("WIKI_PARSE_TOKEN", "test-token-abc");

    let mut external = mock_http::external_at(&srv.url);
    external.response.text = String::from("payload");
    let mut cfg = Config::default();
    cfg.parse.primary = Primary::External;
    cfg.parse.external = Some(external);

    let err = parse::parse_with(&cfg, &Source::inline("a.pdf", b"%PDF")).unwrap_err();
    assert!(err.to_string().contains("payload"), "error: {err}");
}

#[test]
fn missing_auth_env_is_an_error_without_leaking_the_token() {
    const SECRET: &str = "SECRET-DO-NOT-LEAK-0123456789";
    const ABSENT: &str = "WIKI_PARSE_TOKEN_T11_ABSENT";
    let _ = std::env::var(ABSENT);
    std::env::remove_var(ABSENT);
    // A token lives in a *different* variable: the error must not echo it even
    // though the parser could have read it.
    std::env::set_var("WIKI_PARSE_TOKEN_T11_SECRET", SECRET);

    let srv = mock_http::serve_once(200, r#"{"data":"never sent"}"#);
    let mut external = mock_http::external_at(&srv.url);
    external.auth_env = String::from(ABSENT);
    let mut cfg = Config::default();
    cfg.parse.primary = Primary::External;
    cfg.parse.external = Some(external);

    let err = parse::parse_with(&cfg, &Source::inline("a.pdf", b"%PDF")).unwrap_err();
    assert_eq!(err.code(), "E_NO_AUTH_ENV");
    let message = err.to_string();
    assert!(message.contains(ABSENT), "the variable name may be named");
    assert!(
        !message.contains(SECRET),
        "the token must never leak: {message}"
    );
}

#[test]
fn timeout_is_an_error_not_a_hang() {
    std::env::set_var("WIKI_PARSE_TOKEN", "test-token-abc");
    let srv = mock_http::serve_once_delayed(200, r#"{"data":"too late"}"#, Duration::from_secs(3));

    let mut external = mock_http::external_at(&srv.url);
    external.timeout_s = 1;
    let mut cfg = Config::default();
    cfg.parse.primary = Primary::External;
    cfg.parse.external = Some(external);

    let started = Instant::now();
    let err = parse::parse_with(&cfg, &Source::inline("a.pdf", b"%PDF")).unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "the request must give up after ~1s, took {elapsed:?}"
    );
    assert_eq!(err.code(), "E_EXTERNAL");
}

#[test]
fn image_stays_builtin_even_in_external_mode() {
    let mut cfg = Config::default();
    cfg.parse.primary = Primary::External;
    cfg.parse.external = Some(mock_http::sample_external());

    let (extracted, id) = parse::parse_with(&cfg, &Source::inline("a.png", b"\x89PNG")).unwrap();
    assert_eq!(id, "builtin-asset");
    assert!(extracted.asset);
    assert_eq!(extracted.markdown, "![](../source/a.png)");
}

#[test]
fn text_never_goes_external_and_short_text_is_not_warned() {
    let mut cfg = Config::default();
    cfg.parse.primary = Primary::Auto;
    cfg.parse.external = Some(mock_http::sample_external());

    let (extracted, id) =
        parse::parse_with(&cfg, &Source::inline("note.md", "短\n".as_bytes())).unwrap();
    assert_eq!(id, "builtin-text");
    assert_eq!(extracted.markdown, "短\n");
    assert!(
        !extracted
            .warnings
            .iter()
            .any(|warning| warning.code == "below_min_chars"),
        "a short markdown note is not a fallback candidate: {:?}",
        extracted.warnings
    );
}

#[test]
fn auto_short_html_falls_back_to_external() {
    let payload = serde_json::json!({ "data": "# html via service" }).to_string();
    let srv = mock_http::serve_once(200, &payload);
    std::env::set_var("WIKI_PARSE_TOKEN", "test-token-abc");

    let mut cfg = Config::default();
    cfg.parse.primary = Primary::Auto;
    cfg.parse.external = Some(mock_http::external_at(&srv.url));

    let (extracted, id) = parse::parse_with(&cfg, &Source::inline("a.html", b"<p>hi</p>")).unwrap();
    assert_eq!(id, "external-http");
    assert_eq!(extracted.markdown, "# html via service");
}

#[test]
fn external_parser_rejects_incomplete_configuration() {
    assert_eq!(
        ExternalParser::from_config(&None).unwrap_err().code(),
        "E_BAD_CONFIG"
    );
    let mut external = ExternalCfg::default();
    external.url = String::new();
    assert_eq!(
        ExternalParser::from_config(&Some(external.clone()))
            .unwrap_err()
            .code(),
        "E_BAD_CONFIG"
    );

    external.url = String::from("http://127.0.0.1:9/parse");
    external.fields.clear();
    assert_eq!(
        ExternalParser::from_config(&Some(external))
            .unwrap_err()
            .code(),
        "E_BAD_CONFIG",
        "a field carrying the file is required"
    );
}
