//! The `builtin-text` backend: markdown and plain text.
//!
//! Markdown is the output format, so it is also the cheapest input: strip the
//! source's own frontmatter (it belongs to the source, not to the raw copy),
//! normalise line endings, and keep every structural marker untouched.
//!
//! Nothing here fails. A file that is not valid UTF-8 is decoded lossily and a
//! `bad_utf8` warning records the damage, and a body with no usable text gets an
//! `empty` warning; both still produce markdown, which keeps the raw layer
//! complete and lets `lint` decide whether the degradation matters.

use super::{Extracted, Parser, Result, Source, Warning};

/// Backend id recorded in the raw frontmatter and in command output.
pub const ID: &str = "builtin-text";

/// A run of source bytes with no structure to interpret.
pub struct TextParser;

impl TextParser {
    /// A stateless backend; construction exists so the registry reads uniformly.
    pub fn new() -> TextParser {
        TextParser
    }
}

impl Default for TextParser {
    fn default() -> Self {
        TextParser::new()
    }
}

impl Parser for TextParser {
    fn id(&self) -> &'static str {
        ID
    }

    /// `md`, `markdown` and `txt`, in any case.
    fn can_handle(&self, ext: &str) -> bool {
        matches!(ext.to_ascii_lowercase().as_str(), "md" | "markdown" | "txt")
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

        let markdown = normalise(&text);
        if markdown.trim().is_empty() {
            warnings.push(Warning::new(
                "empty",
                format!("{} has no text outside its frontmatter", input.key),
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

/// Drops a leading BOM, turns CRLF into LF, then removes the first frontmatter
/// block.
///
/// Newlines are normalised first on purpose: a frontmatter block written on a
/// Windows machine ends its delimiters with `\r\n`, and detecting it before
/// normalisation would silently keep the source's metadata in the body.
fn normalise(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let text = if text.contains('\r') {
        text.replace("\r\n", "\n")
    } else {
        text.to_string()
    };
    match frontmatter_end(&text) {
        Some(end) => text[end..].to_string(),
        None => text,
    }
}

/// Byte offset just past the closing `---` line of the first frontmatter block.
///
/// Only a leading block counts, and only when both delimiters are present: an
/// unterminated `---` at the top is ordinary body text, not a half frontmatter
/// that swallows the rest of the file.
fn frontmatter_end(text: &str) -> Option<usize> {
    let body = text.strip_prefix("---\n")?;
    let opening = text.len() - body.len();
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        offset += line.len();
        let content = line.trim_end_matches('\n').trim_end();
        if content == "---" || content == "..." {
            return Some(opening + offset);
        }
    }
    None
}
