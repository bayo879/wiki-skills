//! Bookkeeping rendering: the generated `wiki/index.md` and the append-only
//! `wiki/log.md`.
//!
//! Both files sit beside the pages and are maintained by the CLI, never by the
//! agent, so their rules are deliberately mechanical:
//!
//! - `index.md` is regenerated from the pages on disk. It is a pure function of
//!   the page set and the configured type order, which is why [`render_index`]
//!   takes plain [`PageMeta`] values and no paths: the same pages always produce
//!   the same bytes, and a rewritten index carries no drift from the previous
//!   one. Draft pages stay in the catalogue but are marked, so a reader can tell
//!   a conclusion from a note that is still being written.
//! - `log.md` is append-only. Every byte already in the file is preserved
//!   verbatim and the new entry is spliced onto the end; rewriting it would let
//!   a bookkeeping pass silently drop history. The log therefore has no
//!   validator — a human or agent may hand-write entries — and the only escaping
//!   it does is on the separator itself, so a title cannot be mistaken for the
//!   `action | title` split.
//!
//! Both writers go through [`write_atomic`] (temporary sibling plus rename), so
//! a reader never observes a half-written catalogue, and a crash leaves either
//! the old file or the new one.

use std::path::{Component, Path, PathBuf};

use chrono::NaiveDate;
use walkdir::WalkDir;

use crate::archive;
use crate::config::Config;
use crate::page;
use crate::paths;

/// First line of every `index.md`.
const INDEX_TITLE: &str = "# 索引";

/// Lines longer than this in the catalogue are cut: the index is a scannable
/// table of contents, not the page body.
const SUMMARY_MAX: usize = 80;

/// Marker appended to a page whose `status` is `draft`.
const DRAFT_MARK: &str = "（草稿）";

/// Header written when `log.md` does not exist yet.
const LOG_HEADER: &str = "# 日志\n\n";

/// Failure while rendering the bookkeeping files.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// A file or directory could not be read or written.
    #[error("cannot access {}: {source}", path.display())]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// A page could not be read, so it cannot be catalogued.
    #[error(transparent)]
    Page(#[from] page::PageError),
}

impl RenderError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            RenderError::Io { .. } => "E_IO",
            RenderError::Page(err) => err.code(),
        }
    }
}

/// Result of the render helpers.
pub type Result<T> = std::result::Result<T, RenderError>;

/// One page's contribution to `index.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageMeta {
    /// Page id, used in the link target.
    pub id: String,
    /// Human-readable title, used as the link text.
    pub title: String,
    /// Page type: the section the page appears under.
    pub typ: String,
    /// One-line summary, truncated when long.
    pub summary: String,
    /// Lifecycle state as written in frontmatter (`draft` adds the marker).
    pub status: String,
}

/// One entry of `log.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    /// Date of the action.
    pub date: NaiveDate,
    /// Short verb, e.g. `ingest` or `lint`.
    pub action: String,
    /// What was acted on.
    pub title: String,
    /// Optional body, written as its own block below the heading.
    pub detail: Option<String>,
}

impl LogEntry {
    /// Renders the heading line: `## [YYYY-MM-DD] <action> | <title>`.
    ///
    /// A ` | ` inside the title becomes the full-width `｜`, so the first
    /// separator is always the action/title split and a parser can rely on it.
    pub fn to_line(&self) -> String {
        let mut out = String::with_capacity(self.title.len() + self.action.len() + 20);
        out.push_str("## [");
        out.push_str(&self.date.format("%Y-%m-%d").to_string());
        out.push_str("] ");
        out.push_str(&self.action);
        out.push_str(" | ");
        out.push_str(&escape_pipe(&self.title));
        out
    }
}

/// Replaces the log separator inside free text with its full-width twin.
fn escape_pipe(text: &str) -> String {
    text.replace(" | ", " ｜ ")
}

/// Renders the whole `index.md` document.
///
/// Sections follow `types` in the order given; a type that has pages but is not
/// in `types` is appended afterwards in name order, and a type with no pages
/// gets no section at all. Pages inside a section are sorted by id, the file
/// stem: codepoint order on titles is neither alphabetical nor by stroke count
/// for CJK text (`乙` U+4E59 sorts before `甲` U+7532), and locale-aware
/// collation would make the file differ between machines. The id is ASCII
/// kebab-case by schema, so the order is stable everywhere and reproducible
/// from the page set alone.
pub fn render_index(pages: &[PageMeta], types: &[String]) -> String {
    let mut groups: std::collections::BTreeMap<&str, Vec<&PageMeta>> =
        std::collections::BTreeMap::new();
    for page in pages {
        groups.entry(page.typ.as_str()).or_default().push(page);
    }

    // Configured order first, then the remaining types in name order. `groups`
    // is a BTreeMap, so its key order is already the name order.
    let mut order: Vec<&str> = Vec::with_capacity(groups.len());
    for typ in types {
        let typ = typ.as_str();
        if groups.contains_key(typ) && !order.contains(&typ) {
            order.push(typ);
        }
    }
    for typ in groups.keys() {
        if !order.contains(typ) {
            order.push(typ);
        }
    }

    let mut out = String::from(INDEX_TITLE);
    out.push('\n');
    for typ in order {
        let mut section = groups.remove(typ).expect("type came from groups");
        section.sort_by(|a, b| a.id.cmp(&b.id));
        out.push('\n');
        out.push_str("## ");
        out.push_str(typ);
        out.push('\n');
        for page in section {
            out.push_str("- [");
            push_escaped(&mut out, &page.title);
            out.push_str("](");
            push_link_target(&mut out, &format!("{}/{}.md", page.typ, page.id));
            out.push_str(") — ");
            push_truncated(&mut out, &page.summary);
            if page.status == "draft" {
                out.push_str(DRAFT_MARK);
            }
            out.push('\n');
        }
    }
    out
}

/// Appends a link destination, quoting it when the path would end the link early.
///
/// The target is relative to the directory the index lives in, so the same
/// document works for `wiki/index.md` and `.wiki/staging/index.md`.
fn push_link_target(out: &mut String, target: &str) {
    if paths::needs_angle(target) {
        out.push('<');
        out.push_str(target);
        out.push('>');
    } else {
        out.push_str(target);
    }
}

/// Appends `text` with Markdown link brackets escaped, so a title cannot close
/// the link text early.
fn push_escaped(out: &mut String, text: &str) {
    for c in text.chars() {
        if c == '[' || c == ']' {
            out.push('\\');
        }
        out.push(c);
    }
}

/// Appends the summary, cut at [`SUMMARY_MAX`] characters with a `…`.
fn push_truncated(out: &mut String, text: &str) {
    for (index, c) in text.chars().enumerate() {
        if index == SUMMARY_MAX {
            out.push('…');
            return;
        }
        out.push(c);
    }
}

/// Appends one entry to `log.md`, preserving every existing byte.
///
/// A missing file is created with the `# 日志` header; an existing file is
/// treated as opaque history, so an empty one gets no header, and a file whose
/// last line has no newline gets exactly one inserted before the entry. The
/// `detail` block, when present, is surrounded by blank lines.
pub fn append_log(path: &Path, e: &LogEntry) -> Result<()> {
    let mut body = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::from(LOG_HEADER),
        Err(source) => {
            return Err(RenderError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };

    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&e.to_line());
    body.push('\n');
    if let Some(detail) = &e.detail {
        body.push('\n');
        body.push_str(detail);
        if !detail.ends_with('\n') {
            body.push('\n');
        }
        body.push('\n');
    }
    write_atomic(path, body.as_bytes())
}

/// Regenerates `wiki/index.md` from the pages on disk and returns the count.
pub fn write_index(root: &Path, cfg: &Config) -> Result<usize> {
    write_index_in(&root.join(&cfg.paths.wiki), &cfg.schema.types)
}

/// Regenerates `index.md` inside `wiki_dir` from the pages on disk.
///
/// Every `.md` page under the directory is read for its frontmatter, so an
/// empty or missing directory still produces a valid, header-only index. The
/// catalogue is built from the pages on disk rather than from the ledger, so no
/// store handle is needed, and the directory is a parameter rather than a
/// config lookup so the staging directory can be indexed the same way the wiki
/// directory is.
pub fn write_index_in(wiki_dir: &Path, types: &[String]) -> Result<usize> {
    let pages = index_pages(wiki_dir)?;
    let index = render_index(&pages, types);
    write_atomic(&wiki_dir.join("index.md"), index.as_bytes())?;
    Ok(pages.len())
}

/// The index [`write_index`] would write for `root`, as text.
///
/// `lint --fix` compares this with the file on disk to decide whether the
/// index is stale, so the two paths must agree byte for byte: both build the
/// catalogue with [`index_pages`] and render it with [`render_index`].
pub fn index_text(root: &Path, cfg: &Config) -> Result<String> {
    let pages = index_pages(&root.join(&cfg.paths.wiki))?;
    Ok(render_index(&pages, &cfg.schema.types))
}

/// Reads every page under `wiki_dir` into the catalogue rows `index.md` shows.
fn index_pages(wiki_dir: &Path) -> Result<Vec<PageMeta>> {
    let mut pages = Vec::new();
    for path in page_files(wiki_dir)? {
        // A summary is what the wiki remembers, not what it knows: `index.md`
        // catalogues the pages a reader can be sent to, and a distillation of
        // past sessions is read through `recall`.
        if is_summary(wiki_dir, &path) {
            continue;
        }
        let (front, _body) = page::read(&path)?;
        pages.push(PageMeta {
            id: if front.id.is_empty() {
                file_stem(&path)
            } else {
                front.id
            },
            title: front.title,
            typ: if front.typ.is_empty() {
                parent_name(&path)
            } else {
                front.typ
            },
            summary: front.summary,
            status: front.status.to_string(),
        });
    }
    Ok(pages)
}

/// Whether `path` lives under the wiki's archive directory.
fn is_summary(wiki_dir: &Path, path: &Path) -> bool {
    path.strip_prefix(wiki_dir)
        .ok()
        .and_then(|rel| rel.components().next())
        .is_some_and(|first| first.as_os_str() == archive::DIR)
}

/// Every page file under `dir`, sorted by path.
///
/// A missing directory has no pages: a wiki that has not been populated yet
/// still produces a valid, header-only index. `index.md` and `log.md` at the
/// root are the bookkeeping files themselves and are skipped, and so is
/// everything under `audit/`: audit reports are not catalogue entries.
pub fn page_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !dir.is_dir() {
        return Ok(files);
    }
    for entry in WalkDir::new(dir).follow_links(false) {
        let entry = entry.map_err(|err| {
            let path = err
                .path()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| dir.to_path_buf());
            let source = err.io_error().map_or_else(
                || std::io::Error::other("cannot walk the directory"),
                |io| std::io::Error::new(io.kind(), io.to_string()),
            );
            RenderError::Io { path, source }
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let rel = path.strip_prefix(dir).unwrap_or(path);
        if is_bookkeeping(rel) {
            continue;
        }
        files.push(path.to_path_buf());
    }
    files.sort();
    Ok(files)
}

/// Whether a path relative to the wiki root is a bookkeeping file or audit
/// output rather than a page.
///
/// `index.md` and `log.md` are only bookkeeping at the root; a page that happens
/// to be named `index.md` inside a type directory is still a page.
fn is_bookkeeping(rel: &Path) -> bool {
    let mut components = rel.components();
    match components.next() {
        Some(Component::Normal(first)) if first == "audit" => return true,
        _ => {}
    }
    rel.components().count() == 1
        && matches!(
            rel.file_name().and_then(|name| name.to_str()),
            Some("index.md" | "log.md")
        )
}

/// The file stem of `path`, or an empty string when it is not UTF-8.
fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map_or_else(String::new, String::from)
}

/// The type-directory name of `path`, or an empty string when it has none.
fn parent_name(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map_or_else(String::new, String::from)
}

/// Writes `body` to `path` through a temporary sibling plus a rename.
///
/// A rename is atomic on the same filesystem, so a crash leaves the previous
/// file intact instead of a truncated catalogue. The temporary name is
/// dot-prefixed and process-tagged so a leftover from a killed run is invisible
/// to the page walker.
fn write_atomic(path: &Path, body: &[u8]) -> Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent).map_err(|source| RenderError::Io {
        path: parent.clone(),
        source,
    })?;

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("index.md"));
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));

    let written = (|| -> std::io::Result<()> {
        std::fs::write(&tmp, body)?;
        let file = std::fs::File::open(&tmp)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(source) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(RenderError::Io { path: tmp, source });
    }
    if let Err(source) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(RenderError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}
