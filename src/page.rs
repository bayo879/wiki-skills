//! Page frontmatter: parse the small YAML subset pages use, and rewrite single
//! scalars in place.
//!
//! Pages live under `wiki/<type>/<id>.md` and start with a `---` frontmatter
//! block. `read` parses that block into [`FrontMatter`] plus the body; the two
//! writers change one scalar each. The writers exist because every other field
//! in a page — including keys this build does not know, comments and quoting —
//! belongs to the human or agent that wrote it. Re-rendering the whole block
//! would silently normalise those bytes, so both writers locate the target line,
//! replace only the scalar after its colon, and leave every other byte alone.
//! That is why this module works on `&[u8]` rather than `String`: CRLF endings
//! and a leading BOM survive a rewrite unchanged.
//!
//! The parser is deliberately small: top-level `key: value` lines, inline
//! arrays (`key: [a, b]`) and block sequences (`key:` followed by `- item`
//! lines). Quoted scalars are unescaped (`\"` in double quotes, `''` in single
//! quotes), and `#` only starts a comment when it is outside quotes and at the
//! start of a line or after whitespace, so `title: "a #b"` keeps its hash.
//! Anything outside the subset is reported as [`PageError::BadFrontmatter`]
//! with the line it appeared on rather than guessed at.

use std::path::{Path, PathBuf};

use chrono::NaiveDate;

/// UTF-8 byte order mark, preserved by the in-place writers.
const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Frontmatter key of the last-write date.
const UPDATED: &str = "updated";

/// Frontmatter key of the lifecycle state.
const STATUS: &str = "status";

/// Page lifecycle state, written as `draft` / `stable` / `stale`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Written but not yet approved; not citable as a conclusion.
    Draft,
    /// Approved and current.
    Stable,
    /// Out of date, waiting for review.
    Stale,
}

impl Status {
    /// Stable lowercase name used in frontmatter.
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Draft => "draft",
            Status::Stable => "stable",
            Status::Stale => "stale",
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The parsed frontmatter of one page.
///
/// Known keys map to their own fields; every other key lands in [`extra`] in
/// file order, so a caller can report or preserve it without the reader having
/// to understand it.
///
/// [`extra`]: FrontMatter::extra
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontMatter {
    /// Page id: kebab-case, equal to the file stem.
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Page type, taken from the `type:` key (`typ` avoids the Rust keyword).
    pub typ: String,
    /// One-line summary for `index.md`.
    pub summary: String,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// `raw/*.md` references, including any `#p=..` fragment, kept verbatim.
    pub sources: Vec<String>,
    /// Lifecycle state.
    pub status: Status,
    /// Last date written by the CLI.
    pub updated: NaiveDate,
    /// Unknown keys and their scalars, in file order.
    pub extra: Vec<(String, String)>,
}

/// Why a page could not be read or rewritten.
#[derive(Debug, thiserror::Error)]
pub enum PageError {
    /// The page could not be read or written.
    #[error("cannot access {}: {source}", path.display())]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// `write_updated` found no `updated:` line and refuses to invent one.
    #[error("no `updated:` field in {}", path.display())]
    NoUpdated {
        /// Page that lacks the field.
        path: PathBuf,
    },
    /// The frontmatter could not be understood; the line number locates it.
    #[error("bad frontmatter at line {line}: {message}")]
    BadFrontmatter {
        /// 1-based line number inside the page.
        line: usize,
        /// What was wrong.
        message: String,
    },
}

impl PageError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            PageError::Io { .. } => "E_IO",
            PageError::NoUpdated { .. } => "E_NO_UPDATED",
            PageError::BadFrontmatter { .. } => "E_BAD_FRONTMATTER",
        }
    }
}

/// Result of the page helpers.
pub type Result<T> = std::result::Result<T, PageError>;

/// Reads `p` into its frontmatter and body.
///
/// The body is every byte after the closing `---` line and its newline, kept
/// verbatim (a blank line after the frontmatter stays in the body). `status`
/// defaults to [`Status::Draft`] when absent; `id`, `title`, `type` and `summary`
/// default to empty strings; `tags` and `sources` default to empty vectors.
///
/// `updated` is required and must be `YYYY-MM-DD`, and an unterminated or
/// missing frontmatter block is an error — a page without a date cannot be
/// audited, so it is rejected rather than silently dated today.
pub fn read(p: &Path) -> Result<(FrontMatter, String)> {
    parse(&read_bytes(p)?)
}

/// Parses page bytes already in memory.
///
/// Split out of [`read`] so a caller holding candidate content — the staging
/// writer, which must validate before it stores anything — can use the same
/// parser without a temporary file.
pub fn parse(bytes: &[u8]) -> Result<(FrontMatter, String)> {
    let block = find_block(bytes)?;
    let header = std::str::from_utf8(&bytes[block.content_start..block.content_end])
        .map_err(|_| bad(1, "frontmatter is not valid UTF-8"))?;
    let body = std::str::from_utf8(&bytes[block.body_start..])
        .map_err(|_| bad(block.close_line, "page body is not valid UTF-8"))?;
    Ok((parse_header(header, block.close_line)?, String::from(body)))
}

/// Replaces the scalar of the `updated:` line with `d`, touching nothing else.
///
/// A missing `updated:` line is [`PageError::NoUpdated`]: the field is required
/// by the page schema, so a page without it is a defect to report, not a slot
/// to fill in.
pub fn write_updated(p: &Path, d: NaiveDate) -> Result<()> {
    let bytes = read_bytes(p)?;
    let block = find_block(&bytes)?;
    let Some((start, end, line)) = find_field_line(&bytes, &block, UPDATED) else {
        return Err(PageError::NoUpdated {
            path: p.to_path_buf(),
        });
    };
    let (value_start, value_end) = scalar_span(&bytes[start..end], UPDATED.len(), line)?;
    let value = d.format("%Y-%m-%d").to_string();
    write_bytes(
        p,
        splice(
            &bytes,
            start + value_start,
            start + value_end,
            value.as_bytes(),
        ),
    )
}

/// Replaces the scalar of the `status:` line with `s`, touching nothing else.
///
/// `status` is optional in the schema (absent means [`Status::Draft`]), so a
/// page without the line gets one inserted directly after the opening `---`,
/// using the file's own line ending.
pub fn set_status(p: &Path, s: Status) -> Result<()> {
    let bytes = read_bytes(p)?;
    let block = find_block(&bytes)?;
    match find_field_line(&bytes, &block, STATUS) {
        Some((start, end, line)) => {
            let (value_start, value_end) = scalar_span(&bytes[start..end], STATUS.len(), line)?;
            write_bytes(
                p,
                splice(
                    &bytes,
                    start + value_start,
                    start + value_end,
                    s.as_str().as_bytes(),
                ),
            )
        }
        None => {
            let eol: &[u8] = if bytes[..block.content_start].ends_with(b"\r\n") {
                b"\r\n"
            } else {
                b"\n"
            };
            let mut inserted = Vec::new();
            inserted.extend_from_slice(STATUS.as_bytes());
            inserted.extend_from_slice(b": ");
            inserted.extend_from_slice(s.as_str().as_bytes());
            inserted.extend_from_slice(eol);
            write_bytes(
                p,
                splice(&bytes, block.content_start, block.content_start, &inserted),
            )
        }
    }
}

/// Reads the whole page, treating every OS error as [`PageError::Io`].
fn read_bytes(p: &Path) -> Result<Vec<u8>> {
    std::fs::read(p).map_err(|source| PageError::Io {
        path: p.to_path_buf(),
        source,
    })
}

/// Writes the whole page back, treating every OS error as [`PageError::Io`].
fn write_bytes(p: &Path, bytes: Vec<u8>) -> Result<()> {
    std::fs::write(p, bytes).map_err(|source| PageError::Io {
        path: p.to_path_buf(),
        source,
    })
}

/// Byte range of the first frontmatter block of a page.
struct Block {
    /// Offset of the first byte after the opening `---` line.
    content_start: usize,
    /// Offset of the closing `---` line.
    content_end: usize,
    /// Offset of the first byte after the closing `---` line.
    body_start: usize,
    /// 1-based line number of the closing `---`.
    close_line: usize,
}

/// One line of bytes: `(end before '\n', start of the next line)`.
///
/// `end` still includes a trailing `\r`; callers that need the content without
/// it use [`trim_line_end`].
fn line_at(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    if start >= bytes.len() {
        return None;
    }
    match bytes[start..].iter().position(|&b| b == b'\n') {
        Some(offset) => {
            let end = start + offset;
            Some((end, end + 1))
        }
        None => Some((bytes.len(), bytes.len())),
    }
}

/// End offset of a line with its trailing `\r` removed.
fn trim_line_end(bytes: &[u8], start: usize, end: usize) -> usize {
    if end > start && bytes[end - 1] == b'\r' {
        end - 1
    } else {
        end
    }
}

/// Whether a line is a frontmatter delimiter, allowing trailing whitespace.
fn is_delimiter(line: &[u8]) -> bool {
    line.starts_with(b"---") && line[3..].iter().all(|&b| b == b' ' || b == b'\t')
}

/// Locates the first frontmatter block, skipping a leading BOM.
fn find_block(bytes: &[u8]) -> Result<Block> {
    let base = if bytes.starts_with(BOM) { BOM.len() } else { 0 };
    let Some((open_end, open_next)) = line_at(bytes, base) else {
        return Err(bad(1, "file is empty, expected `---`"));
    };
    let open_end = trim_line_end(bytes, base, open_end);
    if !is_delimiter(&bytes[base..open_end]) {
        return Err(bad(1, "file must start with `---`"));
    }

    let mut line = 2;
    let mut pos = open_next;
    while pos < bytes.len() {
        let Some((end, next)) = line_at(bytes, pos) else {
            break;
        };
        let end = trim_line_end(bytes, pos, end);
        if is_delimiter(&bytes[pos..end]) {
            return Ok(Block {
                content_start: open_next,
                content_end: pos,
                body_start: next,
                close_line: line,
            });
        }
        pos = next;
        line += 1;
    }
    Err(bad(
        line,
        "unterminated frontmatter, expected a closing `---`",
    ))
}

/// Finds a top-level `key:` line inside the block.
///
/// Returns `(line start, line content end, 1-based line number)`. Only column
/// zero matches, so an indented `updated:` inside a nested map is not touched.
fn find_field_line(bytes: &[u8], block: &Block, key: &str) -> Option<(usize, usize, usize)> {
    let mut prefix = String::with_capacity(key.len() + 1);
    prefix.push_str(key);
    prefix.push(':');
    let prefix = prefix.as_bytes();

    let mut line = 2;
    let mut pos = block.content_start;
    while pos < block.content_end {
        let (end, next) = line_at(bytes, pos)?;
        let end = trim_line_end(bytes, pos, end);
        if bytes[pos..end].starts_with(prefix) {
            return Some((pos, end, line));
        }
        pos = next;
        line += 1;
    }
    None
}

/// The byte range of the scalar after `colon` within one line.
///
/// Skipped leading whitespace is not part of the range, so it survives the
/// splice. A quoted scalar returns the range *inside* the quotes so the quoting
/// style survives; a plain scalar ends at whitespace, which leaves a trailing
/// comment untouched.
fn scalar_span(line: &[u8], colon: usize, line_no: usize) -> Result<(usize, usize)> {
    let mut start = colon + 1;
    while start < line.len() && (line[start] == b' ' || line[start] == b'\t') {
        start += 1;
    }
    let Some(&first) = line.get(start) else {
        return Ok((start, start));
    };
    if first == b'"' || first == b'\'' {
        return match closing_quote(line, start, first) {
            Some(close) => Ok((start + 1, close)),
            None => Err(bad(line_no, "unterminated quoted scalar")),
        };
    }
    if first == b'#' {
        return Ok((start, start));
    }
    let mut end = start;
    while end < line.len() && line[end] != b' ' && line[end] != b'\t' {
        end += 1;
    }
    Ok((start, end))
}

/// Index of the quote closing the one at `open`, honouring `\"` and `''`.
fn closing_quote(line: &[u8], open: usize, quote: u8) -> Option<usize> {
    let mut i = open + 1;
    while i < line.len() {
        let b = line[i];
        if quote == b'"' && b == b'\\' {
            i += 2;
            continue;
        }
        if b == quote {
            if quote == b'\'' && i + 1 < line.len() && line[i + 1] == b'\'' {
                i += 2;
                continue;
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Replaces `bytes[start..end]` with `value`, keeping everything around it.
fn splice(bytes: &[u8], start: usize, end: usize, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() - (end - start) + value.len());
    out.extend_from_slice(&bytes[..start]);
    out.extend_from_slice(value);
    out.extend_from_slice(&bytes[end..]);
    out
}

/// A `key:` line whose block-sequence items are still being collected.
struct PendingList {
    /// The key the items belong to.
    key: String,
    /// Items seen so far, in order.
    items: Vec<String>,
}

/// A parsed YAML value in the supported subset.
enum Value {
    /// A single scalar, already unquoted and comment-stripped.
    Scalar(String),
    /// An inline `[a, b]` array.
    List(Vec<String>),
}

/// Parses the frontmatter block, which starts at line 2 of the page.
fn parse_header(text: &str, close_line: usize) -> Result<FrontMatter> {
    let mut id = String::new();
    let mut title = String::new();
    let mut typ = String::new();
    let mut summary = String::new();
    let mut tags = Vec::new();
    let mut sources = Vec::new();
    let mut status = Status::Draft;
    let mut updated: Option<NaiveDate> = None;
    let mut extra: Vec<(String, String)> = Vec::new();
    let mut pending: Option<PendingList> = None;

    for (index, raw) in text.split_inclusive('\n').enumerate() {
        let line_no = index + 2;
        let line = trim_line(raw);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(item) = list_item(trimmed) {
            let Some(list) = pending.as_mut() else {
                return Err(bad(line_no, "list item outside a list field"));
            };
            list.items
                .push(parse_scalar(strip_comment(item).trim(), line_no)?);
            continue;
        }
        if line.len() != line.trim_start().len() {
            return Err(bad(
                line_no,
                "unexpected indented line; nested mappings are not supported",
            ));
        }

        flush_pending(&mut pending, &mut tags, &mut sources, &mut extra);

        let colon = line
            .find(':')
            .ok_or_else(|| bad(line_no, "expected `key: value`"))?;
        let key = line[..colon].trim();
        if key.is_empty() {
            return Err(bad(line_no, "empty key before `:`"));
        }
        let value = parse_value(&line[colon + 1..], line_no)?;
        match key {
            "id" => id = scalar(value, key, line_no)?,
            "title" => title = scalar(value, key, line_no)?,
            "type" => typ = scalar(value, key, line_no)?,
            "summary" => summary = scalar(value, key, line_no)?,
            "status" => status = parse_status(&scalar(value, key, line_no)?, line_no)?,
            "updated" => {
                updated = Some(parse_date_field(&scalar(value, key, line_no)?, line_no)?);
            }
            "tags" => assign_list(value, &mut tags, &mut pending, "tags"),
            "sources" => assign_list(value, &mut sources, &mut pending, "sources"),
            other => match value {
                Value::Scalar(s) if s.is_empty() => {
                    pending = Some(PendingList {
                        key: String::from(other),
                        items: Vec::new(),
                    });
                }
                Value::Scalar(s) => extra.push((String::from(other), s)),
                Value::List(items) => extra.push((String::from(other), items.join(", "))),
            },
        }
    }

    flush_pending(&mut pending, &mut tags, &mut sources, &mut extra);

    let Some(updated) = updated else {
        return Err(bad(close_line, "missing required `updated:` field"));
    };
    Ok(FrontMatter {
        id,
        title,
        typ,
        summary,
        tags,
        sources,
        status,
        updated,
        extra,
    })
}

/// Stores a list value, or starts collecting a block sequence for it.
fn assign_list(
    value: Value,
    field: &mut Vec<String>,
    pending: &mut Option<PendingList>,
    key: &str,
) {
    match value {
        Value::List(items) => *field = items,
        Value::Scalar(s) if s.is_empty() => {
            *pending = Some(PendingList {
                key: String::from(key),
                items: Vec::new(),
            });
        }
        Value::Scalar(s) => *field = vec![s],
    }
}

/// Finishes the block sequence being collected, if any.
///
/// Known list keys land in their fields; an unknown key keeps its items joined
/// by `, ` so the information is not lost.
fn flush_pending(
    pending: &mut Option<PendingList>,
    tags: &mut Vec<String>,
    sources: &mut Vec<String>,
    extra: &mut Vec<(String, String)>,
) {
    let Some(list) = pending.take() else {
        return;
    };
    match list.key.as_str() {
        "tags" => *tags = list.items,
        "sources" => *sources = list.items,
        _ => extra.push((list.key.clone(), list.items.join(", "))),
    }
}

/// The item text of a block-sequence line, if the line is one.
fn list_item(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('-')?;
    if rest.is_empty() {
        return Some(rest);
    }
    if rest.starts_with(' ') || rest.starts_with('\t') {
        return Some(rest.trim_start());
    }
    None
}

/// Strips one trailing `\n` and one trailing `\r` from a raw line.
fn trim_line(raw: &str) -> &str {
    let raw = raw.strip_suffix('\n').unwrap_or(raw);
    raw.strip_suffix('\r').unwrap_or(raw)
}

/// Parses a value: an inline array or a single scalar.
fn parse_value(raw: &str, line_no: usize) -> Result<Value> {
    let raw = strip_comment(raw).trim();
    if let Some(inner) = raw.strip_prefix('[') {
        let Some(inner) = inner.strip_suffix(']') else {
            return Err(bad(line_no, "unterminated inline array"));
        };
        let mut items = Vec::new();
        for item in split_items(inner) {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            items.push(parse_scalar(item, line_no)?);
        }
        return Ok(Value::List(items));
    }
    Ok(Value::Scalar(parse_scalar(raw, line_no)?))
}

/// Parses one scalar, unescaping a matching pair of quotes.
fn parse_scalar(raw: &str, line_no: usize) -> Result<String> {
    let bytes = raw.as_bytes();
    match bytes.first() {
        Some(b'"') => {
            if bytes.len() >= 2 && bytes[bytes.len() - 1] == b'"' {
                Ok(unescape_double(&raw[1..raw.len() - 1]))
            } else {
                Err(bad(line_no, "unterminated double-quoted scalar"))
            }
        }
        Some(b'\'') => {
            if bytes.len() >= 2 && bytes[bytes.len() - 1] == b'\'' {
                Ok(String::from(&raw[1..raw.len() - 1]).replace("''", "'"))
            } else {
                Err(bad(line_no, "unterminated single-quoted scalar"))
            }
        }
        _ => Ok(String::from(raw)),
    }
}

/// Undoes the double-quote escaping the raw writer produces.
fn unescape_double(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Cuts a line at the first `#` that is outside quotes and after whitespace.
fn strip_comment(raw: &str) -> &str {
    let bytes = raw.as_bytes();
    let mut i = 0;
    let mut single = false;
    let mut double = false;
    while i < bytes.len() {
        let b = bytes[i];
        if double {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'"' {
                double = false;
            }
            i += 1;
            continue;
        }
        if single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                single = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => double = true,
            b'\'' => single = true,
            b'#' if i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t' => return &raw[..i],
            _ => {}
        }
        i += 1;
    }
    raw
}

/// Splits an inline array body on commas that are outside quotes.
fn split_items(inner: &str) -> Vec<&str> {
    let bytes = inner.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let mut single = false;
    let mut double = false;
    while i < bytes.len() {
        let b = bytes[i];
        if double {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'"' {
                double = false;
            }
            i += 1;
            continue;
        }
        if single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                single = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => double = true,
            b'\'' => single = true,
            b',' => {
                parts.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&inner[start..]);
    parts
}

/// Parses a `status` scalar, rejecting values outside the three states.
fn parse_status(value: &str, line_no: usize) -> Result<Status> {
    match value {
        "draft" => Ok(Status::Draft),
        "stable" => Ok(Status::Stable),
        "stale" => Ok(Status::Stale),
        other => Err(bad(
            line_no,
            format!("`status` must be draft, stable or stale, got `{other}`"),
        )),
    }
}

/// Parses the required `updated` scalar, reporting the line when it is bad.
fn parse_date_field(value: &str, line_no: usize) -> Result<NaiveDate> {
    parse_iso_date(value).ok_or_else(|| {
        bad(
            line_no,
            format!("`updated` must be YYYY-MM-DD, got `{value}`"),
        )
    })
}

/// Strict `YYYY-MM-DD` parse: exactly ten bytes, two dashes, the rest digits.
fn parse_iso_date(value: &str) -> Option<NaiveDate> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if bytes
        .iter()
        .enumerate()
        .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
    {
        return None;
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

/// Turns a scalar value into a `String`, rejecting an array where one is needed.
fn scalar(value: Value, key: &str, line_no: usize) -> Result<String> {
    match value {
        Value::Scalar(s) => Ok(s),
        Value::List(_) => Err(bad(
            line_no,
            format!("`{key}` expects a scalar, not an array"),
        )),
    }
}

/// Builds a bad-frontmatter error located at `line`.
fn bad(line: usize, message: impl Into<String>) -> PageError {
    PageError::BadFrontmatter {
        line,
        message: message.into(),
    }
}
