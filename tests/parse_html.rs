//! Tests for the `builtin-html` backend.
//!
//! The backend is a pure HTML → Markdown mapping, so every test is an in-memory
//! `Source`: no filesystem, no fixtures. Assertions are on whole lines where the
//! exact formatting matters, because a block that merely *contains* "# a" would
//! also accept "## a".

use llm_wiki::parse::html::HtmlParser;
use llm_wiki::parse::{Extracted, Parser, Source};

fn parse(html: &str) -> Extracted {
    HtmlParser::new()
        .parse(&Source::inline("x.html", html.as_bytes()))
        .expect("html parsing never fails")
}

/// Whether `markdown` has a line exactly equal to `expected`.
fn has_line(markdown: &str, expected: &str) -> bool {
    markdown.lines().any(|line| line == expected)
}

#[test]
fn drops_navigation_and_keeps_article() {
    let html = r#"<html><body><nav>导航</nav>
        <article><h1>订单</h1><p>正文段落</p><a href="/x">链接</a></article>
        <script>var a=1</script></body></html>"#;
    let out = HtmlParser::new()
        .parse(&Source::inline("x.html", html.as_bytes()))
        .unwrap();
    assert!(out.markdown.contains("# 订单"));
    assert!(out.markdown.contains("正文段落"));
    assert!(out.markdown.contains("[链接](/x)"));
    assert!(!out.markdown.contains("导航"));
    assert!(!out.markdown.contains("var a=1"));
}

#[test]
fn declares_id_and_claimed_extensions() {
    let parser = HtmlParser::new();
    assert_eq!(parser.id(), "builtin-html");
    for ext in ["html", "HTML", "htm", "Htm"] {
        assert!(parser.can_handle(ext), "{ext} must be claimed");
    }
    for ext in ["md", "txt", "pdf", "png", ""] {
        assert!(!parser.can_handle(ext), "{ext} must not be claimed");
    }
}

#[test]
fn maps_every_heading_level() {
    let out = parse("<h1>a</h1><h2>b</h2><h3>c</h3><h4>d</h4><h5>e</h5><h6>f</h6>");
    for expected in ["# a", "## b", "### c", "#### d", "##### e", "###### f"] {
        assert!(
            has_line(&out.markdown, expected),
            "missing {expected:?} in {:?}",
            out.markdown
        );
    }
}

#[test]
fn maps_images_with_and_without_alt() {
    let out = parse(r#"<p><img src="a.png" alt="图"><img src="b.png"></p>"#);
    assert!(
        out.markdown.contains("![图](a.png)"),
        "missing image with alt in {:?}",
        out.markdown
    );
    assert!(
        out.markdown.contains("![](b.png)"),
        "missing image without alt in {:?}",
        out.markdown
    );
}

#[test]
fn drops_chrome_tags() {
    let out = parse(
        "<header>页眉</header><aside>侧边</aside><style>.a{color:red}</style>\
         <noscript>无脚本</noscript><p>正文</p>",
    );
    assert!(out.markdown.contains("正文"), "{:?}", out.markdown);
    for gone in ["页眉", "侧边", "color:red", "无脚本"] {
        assert!(!out.markdown.contains(gone), "{gone:?} leaked");
    }
}

#[test]
fn maps_pre_code_to_fenced_block() {
    let out = parse(r#"<pre><code class="language-rust">fn a() {}</code></pre>"#);
    let markdown = &out.markdown;
    assert!(
        has_line(markdown, "```rust"),
        "missing fenced opener in {markdown:?}"
    );
    assert!(
        markdown.contains("fn a() {}"),
        "code body lost in {markdown:?}"
    );
    assert!(
        markdown.trim_end().ends_with("```"),
        "missing fenced closer in {markdown:?}"
    );
}

#[test]
fn maps_inline_code() {
    let out = parse("<p>run <code>cargo test</code> now</p>");
    assert!(
        out.markdown.contains("`cargo test`"),
        "missing inline code in {:?}",
        out.markdown
    );
}

#[test]
fn maps_unordered_and_ordered_lists() {
    let markdown = parse("<ul><li>a</li><li>b</li></ul><ol><li>x</li><li>y</li></ol>").markdown;
    for expected in ["- a", "- b", "1. x", "2. y"] {
        assert!(
            has_line(&markdown, expected),
            "missing {expected:?} in {markdown:?}"
        );
    }
}

#[test]
fn nested_divs_do_not_add_blank_lines() {
    let markdown = parse("<div><div><p>a</p></div><div><p>b</p></div></div>").markdown;
    assert_eq!(markdown, "a\n\nb");
}

#[test]
fn empty_document_warns_empty() {
    let out = parse("<html><body></body></html>");
    assert!(out.markdown.trim().is_empty(), "{:?}", out.markdown);
    assert!(
        out.warnings.iter().any(|warning| warning.code == "empty"),
        "missing `empty` warning in {:?}",
        out.warnings
    );
}

#[test]
fn prefers_article_over_body_noise() {
    let markdown = parse(
        r#"<body><div>无关内容</div><article><p>正文</p></article><div>更多无关</div></body>"#,
    )
    .markdown;
    assert!(markdown.contains("正文"), "{markdown:?}");
    assert!(!markdown.contains("无关"), "{markdown:?}");
}

#[test]
fn invalid_utf8_warns_but_does_not_panic() {
    let out = HtmlParser::new()
        .parse(&Source::inline("x.html", &[0xff, 0xfe]))
        .unwrap();
    assert!(
        out.warnings
            .iter()
            .any(|warning| warning.code == "bad_utf8"),
        "missing `bad_utf8` warning in {:?}",
        out.warnings
    );
}
