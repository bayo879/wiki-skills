//! The `builtin-html` backend: web pages reduced to article markdown.
//!
//! HTML is not markdown, so this backend is a lossy projection rather than a
//! pass-through: pick the article container, drop page chrome, and map the
//! surviving structure onto the handful of markdown constructs the wiki uses.
//!
//! Two rules shape the output. First, structure wins over fidelity: a page is
//! reduced to blocks (headings, paragraphs, lists, fenced code), and blocks are
//! joined by exactly one blank line, so the same page always yields the same
//! text and no source whitespace survives. Second, nothing here fails: the body
//! may be invalid UTF-8 (`bad_utf8`) or empty (`empty`), and both still return
//! markdown so the raw layer stays complete.

use scraper::{ElementRef, Html, Node, Selector};

use super::{Extracted, Parser, Result, Source, Warning};

/// Backend id recorded in the raw frontmatter and in command output.
pub const ID: &str = "builtin-html";

/// Elements that are page chrome, not content: they are dropped with their
/// whole subtree, including any text inside them.
const DROPPED: &[&str] = &[
    "script", "style", "noscript", "nav", "header", "footer", "aside",
];

/// The `builtin-html` backend.
pub struct HtmlParser;

impl HtmlParser {
    /// A stateless backend; construction exists so the registry reads uniformly.
    pub fn new() -> HtmlParser {
        HtmlParser
    }
}

impl Default for HtmlParser {
    fn default() -> Self {
        HtmlParser::new()
    }
}

impl Parser for HtmlParser {
    fn id(&self) -> &'static str {
        ID
    }

    /// `html` and `htm`, in any case.
    fn can_handle(&self, ext: &str) -> bool {
        matches!(ext.to_ascii_lowercase().as_str(), "html" | "htm")
    }

    fn parse(&self, input: &Source) -> Result<Extracted> {
        let mut warnings = Vec::new();
        let text = match std::str::from_utf8(&input.bytes) {
            Ok(text) => text.to_string(),
            Err(err) => {
                warnings.push(Warning::new(
                    "bad_utf8",
                    format!(
                        "{} is not valid UTF-8 after byte {}; invalid bytes were replaced with U+FFFD",
                        input.key,
                        err.valid_up_to()
                    ),
                ));
                String::from_utf8_lossy(&input.bytes).into_owned()
            }
        };

        let markdown = to_markdown(&text);
        if markdown.trim().is_empty() {
            warnings.push(Warning::new(
                "empty",
                format!("{} has no extractable article text", input.key),
            ));
        }

        Ok(Extracted {
            markdown,
            warnings,
            asset: false,
            no_text_layer: false,
        })
    }
}

/// Maps one HTML document to markdown, with no I/O and no configuration.
///
/// This is the whole backend as a pure function: `parse` only adds the UTF-8
/// decode and the warnings around it.
pub fn to_markdown(html: &str) -> String {
    let document = Html::parse_document(html);
    let root = container(&document);
    let mut out = String::new();
    render_children(root, &mut out);
    out.trim().to_string()
}

/// The element whose children are the article: `article`, else `main`, else
/// `body`.
///
/// First match wins rather than deepest match: a page that marks its article
/// explicitly is trusted over a page that only has a body, which is what keeps
/// sidebars and comment threads out of the markdown.
fn container(document: &Html) -> ElementRef<'_> {
    for tag in ["article", "main", "body"] {
        if let Some(element) = document.select(&selector(tag)).next() {
            return element;
        }
    }
    document.root_element()
}

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("static selector is valid")
}

fn is_dropped(name: &str) -> bool {
    DROPPED.contains(&name)
}

/// Appends one block, separated from the previous one by a single blank line.
///
/// Blank blocks are ignored, so an empty `<div>` or a whitespace-only text node
/// costs nothing and cannot create the consecutive blank lines that make
/// markdown ambiguous.
fn push_block(out: &mut String, block: &str) {
    let block = block.trim();
    if block.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(block);
}

// ---------------------------------------------------------------- block level

fn render_children(parent: ElementRef<'_>, out: &mut String) {
    for child in parent.children() {
        match child.value() {
            Node::Text(text) => push_block(out, collapse(&text.text).trim()),
            Node::Element(_) => {
                render_element(ElementRef::wrap(child).expect("element nodes wrap"), out)
            }
            _ => {}
        }
    }
}

fn render_element(element: ElementRef<'_>, out: &mut String) {
    let name = element.value().name();
    if is_dropped(name) {
        return;
    }

    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = name[1..].parse::<usize>().unwrap_or(1);
            let text = inline_of(element);
            if !text.is_empty() {
                push_block(out, &format!("{} {}", "#".repeat(level), text));
            }
        }
        "p" => push_block(out, &inline_of(element)),
        "pre" => push_block(out, &code_block(element)),
        "ul" => push_list(element, false, out),
        "ol" => push_list(element, true, out),
        "li" => push_block(out, &inline_of(element)),
        "blockquote" => push_block(out, &quote_of(element)),
        "hr" => push_block(out, "---"),
        // Inline constructs that can appear directly under a block container:
        // they keep their own markdown syntax instead of losing it as a bare
        // text block.
        "a" | "img" | "code" | "strong" | "b" | "em" | "i" | "span" | "small" => {
            let mut text = String::new();
            inline_element(element, &mut text);
            push_block(out, &text);
        }
        // Everything else is only a container: recurse without adding a block,
        // so a chain of `<div>`s does not add any whitespace of its own.
        _ => render_children(element, out),
    }
}

fn push_list(element: ElementRef<'_>, ordered: bool, out: &mut String) {
    let mut lines = Vec::new();
    list_lines(element, ordered, 0, &mut lines);
    push_block(out, &lines.join("\n"));
}

fn list_lines(list: ElementRef<'_>, ordered: bool, depth: usize, lines: &mut Vec<String>) {
    let indent = "  ".repeat(depth);
    let mut index = 0;
    for child in list.children() {
        let name = match child.value() {
            Node::Element(element) => element.name(),
            _ => continue,
        };
        if name != "li" {
            continue;
        }
        index += 1;
        let item = ElementRef::wrap(child).expect("element nodes wrap");

        let mut content = String::new();
        let mut nested = Vec::new();
        for grandchild in item.children() {
            let grandchild_name = match grandchild.value() {
                Node::Element(element) => element.name(),
                _ => "",
            };
            if grandchild_name == "ul" || grandchild_name == "ol" {
                let nested_list = ElementRef::wrap(grandchild).expect("element nodes wrap");
                let nested_depth = depth + 1;
                list_lines(
                    nested_list,
                    grandchild_name == "ol",
                    nested_depth,
                    &mut nested,
                );
            } else {
                match grandchild.value() {
                    Node::Text(text) => content.push_str(&collapse(&text.text)),
                    Node::Element(_) => inline_element(
                        ElementRef::wrap(grandchild).expect("element nodes wrap"),
                        &mut content,
                    ),
                    _ => {}
                }
            }
        }

        let marker = if ordered {
            format!("{index}. ")
        } else {
            String::from("- ")
        };
        lines.push(format!("{indent}{marker}{}", content.trim()));
        lines.extend(nested);
    }
}

fn quote_of(element: ElementRef<'_>) -> String {
    let mut inner = String::new();
    render_children(element, &mut inner);
    inner
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::from(">")
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A fenced code block, keeping the code body as written.
///
/// The language comes from the `language-*` (or `lang-*`) class on `<code>` or,
/// failing that, on `<pre>`; without one the fence has no annotation.
fn code_block(pre: ElementRef<'_>) -> String {
    let code = pre.select(&selector("code")).next();
    let language = code
        .and_then(|element| element.value().attr("class"))
        .or_else(|| pre.value().attr("class"))
        .and_then(language_of)
        .unwrap_or("");
    let body = raw_text(pre);
    let body = body.trim_matches('\n');
    if body.trim().is_empty() {
        return String::new();
    }
    format!("```{language}\n{body}\n```")
}

fn language_of(class: &str) -> Option<&str> {
    class.split_whitespace().find_map(|token| {
        token
            .strip_prefix("language-")
            .or_else(|| token.strip_prefix("lang-"))
    })
}

// --------------------------------------------------------------- inline level

/// Collects the inline markdown of an element's children, trimmed.
fn inline_of(element: ElementRef<'_>) -> String {
    let mut out = String::new();
    inline_children(element, &mut out);
    out.trim().to_string()
}

fn inline_children(parent: ElementRef<'_>, out: &mut String) {
    for child in parent.children() {
        match child.value() {
            Node::Text(text) => out.push_str(&collapse(&text.text)),
            Node::Element(_) => {
                inline_element(ElementRef::wrap(child).expect("element nodes wrap"), out)
            }
            _ => {}
        }
    }
}

fn inline_element(element: ElementRef<'_>, out: &mut String) {
    let name = element.value().name();
    if is_dropped(name) {
        return;
    }

    match name {
        "br" => out.push(' '),
        "img" => {
            let alt = element.value().attr("alt").unwrap_or("");
            let src = element.value().attr("src").unwrap_or("");
            if !src.is_empty() {
                out.push_str(&format!("![{alt}]({src})"));
            } else if !alt.is_empty() {
                out.push_str(alt);
            }
        }
        "a" => {
            let text = inline_of(element);
            let href = element.value().attr("href").unwrap_or("");
            if href.is_empty() || text.is_empty() {
                out.push_str(&text);
            } else {
                out.push_str(&format!("[{text}]({href})"));
            }
        }
        "code" => {
            let text = raw_text(element);
            let code = text.trim();
            if !code.is_empty() {
                out.push('`');
                out.push_str(code);
                out.push('`');
            }
        }
        "strong" | "b" => emphasized(element, "**", out),
        "em" | "i" => emphasized(element, "*", out),
        _ => inline_children(element, out),
    }
}

fn emphasized(element: ElementRef<'_>, marker: &str, out: &mut String) {
    let inner = inline_of(element);
    if !inner.is_empty() {
        out.push_str(marker);
        out.push_str(&inner);
        out.push_str(marker);
    }
}

/// Collapses every whitespace run in `text` to a single space, without trimming.
///
/// Per text node rather than on the finished inline string, so whitespace inside
/// inline `<code>` and inside fenced blocks is left alone.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out
}

/// Concatenates every text descendant, keeping whitespace exactly.
fn raw_text(element: ElementRef<'_>) -> String {
    let mut out = String::new();
    raw_children(element, &mut out);
    out
}

fn raw_children(parent: ElementRef<'_>, out: &mut String) {
    for child in parent.children() {
        match child.value() {
            Node::Text(text) => out.push_str(&text.text),
            Node::Element(element) if !is_dropped(element.name()) => {
                raw_children(ElementRef::wrap(child).expect("element nodes wrap"), out);
            }
            _ => {}
        }
    }
}
