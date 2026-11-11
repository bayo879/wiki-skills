//! `lint`: the eleven audit rules over `wiki/`, and the mechanical `--fix`.
//!
//! `lint` reads the wiki directory and the ledger and returns findings sorted by
//! severity, rule and page, so the output is a stable value that can be
//! snapshotted. `fix` repairs the defects that need no judgement — a missing or
//! malformed `updated`, an `updated` that did not move when the body changed,
//! and a stale `index.md` — then re-runs `lint` and reports what is left.
//!
//! The list below is the contract: `tests/lint.rs` holds one test per rule:
//!
//! - `L1` error: a page is missing `id`, `title` or `status`.
//! - `L2` error: a `[[wikilink]]` names no page.
//! - `L3` error: one id is used by two type directories.
//! - `L4` error: `status` is not `draft`, `stable` or `stale`.
//! - `L5` warning: `updated` is missing or not a `YYYY-MM-DD` date.
//! - `L6` error: a page's `sources` names a file that does not exist.
//! - `L7` warning: a file under `raw/` is not in the ledger.
//! - `L8` warning: the body is shorter than `[audit].min_page_chars` characters.
//! - `L9` warning: the body changed but `updated` did not.
//! - `L10` info: one raw file is cited by more than three pages.
//! - `L11` info: an asset raw file is cited by no page.
//! - `L12` warning: a page cites no source at all.
//! - `L13` warning: the body still carries an unfinished marker (`TODO`,
//!   `FIXME`, `待补`, `占位`).
//!
//! Two of the rules are easy to get wrong, so they are called out here:
//!
//! `L9` only fires when the ledger holds a record for the page. `state.db` is
//! not in git, so a fresh clone has no records at all; reporting every page as
//! "changed but not dated" would make a clean checkout fail, so a missing record
//! is silent. A record whose `updated` no longer matches the page is silent too:
//! the page was re-dated, which is exactly the fix.
//!
//! `L8` counts characters, not bytes, so a short Chinese page is still short.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use chrono::{Local, NaiveDate};
use serde::Serialize;

use crate::config::Config;
use crate::hash;
use crate::page::{self, PageError};
use crate::paths;
use crate::relation::{PageIdentity, Resolution, Resolver};
use crate::render;
use crate::scan::{self, RawLedger};
use crate::source_refs;
use crate::store::{PageMetaRow, Store, StoreError};

/// Severity of one finding.
///
/// The declaration order is the severity order: `Info < Warning < Error`, so a
/// descending sort puts errors first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Worth knowing; never affects the exit code.
    Info,
    /// A defect that does not stop the wiki from working.
    Warning,
    /// A defect that makes a page or the wiki wrong.
    Error,
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Level::Info => "info",
            Level::Warning => "warning",
            Level::Error => "error",
        })
    }
}

/// One lint finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Rule identifier, `L1` through `L11`.
    pub rule: &'static str,
    /// Severity.
    pub level: Level,
    /// Repository-relative page path, when the finding is about one page.
    pub page: Option<String>,
    /// Human-readable description.
    pub msg: String,
}

impl Finding {
    fn new(rule: &'static str, level: Level, page: Option<String>, msg: String) -> Finding {
        Finding {
            rule,
            level,
            page,
            msg,
        }
    }
}

impl std::fmt::Display for Finding {
    /// One line: `<level> <rule> <page>: <msg>`, `-` when no page is named.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} {}: {}",
            self.level,
            self.rule,
            self.page.as_deref().unwrap_or("-"),
            self.msg
        )
    }
}

/// What `fix` repaired and what is still wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixReport {
    /// Number of items actually changed on disk.
    pub fixed: usize,
    /// Findings left after the repairs.
    pub remaining: Vec<Finding>,
}

/// Why lint or fix could not finish.
#[derive(Debug, thiserror::Error)]
pub enum LintError {
    /// The wiki directory could not be walked or its index written.
    #[error(transparent)]
    Render(#[from] render::RenderError),
    /// The ledger could not be read or written.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A page could not be digested.
    #[error(transparent)]
    Hash(#[from] hash::HashError),
    /// A page could not be read or rewritten.
    #[error(transparent)]
    Page(#[from] PageError),
    /// `raw/` could not be walked for `L7`.
    #[error(transparent)]
    Scan(#[from] scan::ScanError),
}

impl LintError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            LintError::Render(err) => err.code(),
            LintError::Store(_) => "E_DB",
            LintError::Hash(_) => "E_IO",
            LintError::Page(err) => err.code(),
            LintError::Scan(_) => "E_IO",
        }
    }
}

/// Result of the lint helpers.
pub type Result<T> = std::result::Result<T, LintError>;

/// One page as `lint` sees it: frontmatter, body and where it lives.
struct PageInfo {
    /// Repository-relative, `/`-separated path (`wiki/concept/a.md`).
    key: String,
    /// Wiki-relative path without the extension (`concept/a`).
    rel: String,
    /// Frontmatter `id`; empty when the field is missing.
    id: String,
    /// Parsed frontmatter.
    front: page::FrontMatter,
    /// Body text the rules count and scan.
    body: String,
}

/// Audits `wiki/` and returns findings in a stable order.
///
/// The order is severity (error, warning, info), then rule, then page, so two
/// runs over the same tree produce the same document.
pub fn lint(root: &Path, cfg: &Config, store: &Store) -> Result<Vec<Finding>> {
    let mut found: Vec<Finding> = Vec::new();
    let pages = load_pages(root, cfg, &mut found)?;

    check_dead_links(cfg, &pages, &mut found);
    check_duplicate_ids(&pages, &mut found);
    check_missing_sources(root, cfg, &pages, &mut found);
    check_short_pages(cfg, &pages, &mut found);
    for page in &pages {
        found.extend(page_quality(&page.key, &page.front, &page.body));
    }
    check_unledgered_raws(root, cfg, store, &mut found)?;

    let cited = cited_raws(cfg, &pages);
    check_over_shared_raws(&cited, &mut found);
    check_page_meta(root, store, &pages, &mut found)?;
    check_unreferenced_assets(store, &cited, &mut found)?;

    found.sort_by(|a, b| {
        b.level
            .cmp(&a.level)
            .then_with(|| a.rule.cmp(b.rule))
            .then_with(|| a.page.cmp(&b.page))
    });
    Ok(found)
}

/// Repairs the mechanical defects and re-runs [`lint`].
///
/// Three repairs are mechanical and safe: a missing or malformed `updated` is
/// set to the local date, a changed body with an unchanged date gets the date
/// bumped and the ledger record refreshed, and a stale `index.md` is rewritten.
/// Everything that needs a judgement — a dead link, a malformed status, a
/// duplicate id — is left in `remaining` for a human or an agent to fix.
pub fn fix(root: &Path, cfg: &Config, store: &mut Store) -> Result<FixReport> {
    let today = Local::now().date_naive();
    let found = lint(root, cfg, store)?;
    let mut fixed = 0;

    for finding in found.iter().filter(|f| f.rule == "L5") {
        if let Some(key) = &finding.page {
            // A page whose frontmatter cannot be repaired at all is already
            // reported as unreadable; fixing the other pages is still useful,
            // so a refusal here leaves the finding in `remaining`.
            if page::write_updated_lenient(&root.join(key), today).is_ok() {
                fixed += 1;
            }
        }
    }

    for finding in found.iter().filter(|f| f.rule == "L9") {
        if let Some(key) = &finding.page {
            let path = root.join(key);
            page::write_updated_lenient(&path, today).map_err(LintError::Page)?;
            store.upsert_page_meta(&PageMetaRow {
                page_key: key.clone(),
                hash: hash::hash_file(&path, true)?,
                updated: today.to_string(),
            })?;
            fixed += 1;
        }
    }

    // A page the reader refuses also stops the catalogue from being built; the
    // L4/L5 finding already names it, so the index repair is skipped rather
    // than turning the whole run into an error.
    if let Ok(expected) = render::index_text(root, cfg) {
        let current = fs::read_to_string(root.join(&cfg.paths.wiki).join("index.md")).ok();
        if current.as_deref() != Some(expected.as_str()) {
            render::write_index(root, cfg)?;
            fixed += 1;
        }
    }

    let remaining = lint(root, cfg, store)?;
    Ok(FixReport { fixed, remaining })
}

/// Whether `findings` should make the process exit 1.
///
/// An error always fails; a warning fails only under `--strict`; an info never
/// does.
pub fn fails(findings: &[Finding], strict: bool) -> bool {
    findings.iter().any(|f| match f.level {
        Level::Error => true,
        Level::Warning => strict,
        Level::Info => false,
    })
}

/// Walks `wiki/`, checks the per-page rules, and returns the pages it could
/// read.
///
/// A page whose frontmatter does not parse is still worth a finding, but it
/// carries no body to inspect: `L4`/`L5` are emitted here and the page is
/// dropped from the list. The probe below re-reads it leniently so the other
/// rules can still look at it when only `updated` or `status` was bad.
fn load_pages(root: &Path, cfg: &Config, found: &mut Vec<Finding>) -> Result<Vec<PageInfo>> {
    let wiki_dir = root.join(&cfg.paths.wiki);
    let mut pages = Vec::new();
    for path in render::page_files(&wiki_dir)? {
        let key = repo_key(root, &path);
        let raw = fs::read(&path).map_err(|source| PageError::Io {
            path: path.clone(),
            source,
        })?;
        let text = String::from_utf8_lossy(&raw).into_owned();

        let mut reported_l5 = false;
        let (front, body) = match page::parse(&raw) {
            Ok(pair) => pair,
            Err(err) => {
                let PageError::BadFrontmatter { line, message } = &err else {
                    return Err(LintError::Page(err));
                };
                let offending = text
                    .lines()
                    .nth(line.saturating_sub(1))
                    .unwrap_or("")
                    .trim_start()
                    .to_string();
                if offending.starts_with("status:") {
                    let value = offending.split_once(':').map_or("", |(_, v)| v.trim());
                    found.push(Finding::new(
                        "L4",
                        Level::Error,
                        Some(key.clone()),
                        format!("`status` value `{value}` is not draft, stable or stale"),
                    ));
                } else {
                    found.push(Finding::new(
                        "L5",
                        Level::Warning,
                        Some(key.clone()),
                        format!("`updated`: {message}"),
                    ));
                    reported_l5 = true;
                }
                match probe(&text) {
                    Some(pair) => pair,
                    None => continue,
                }
            }
        };

        if !reported_l5 && front.updated == epoch() {
            found.push(Finding::new(
                "L5",
                Level::Warning,
                Some(key.clone()),
                String::from("`updated` is missing or not a `YYYY-MM-DD` date"),
            ));
        }

        check_required_fields(&text, &front, &key, found);
        pages.push(PageInfo {
            rel: rel_page(&key, &cfg.paths.wiki),
            key,
            id: front.id.clone(),
            front,
            body,
        });
    }
    Ok(pages)
}

/// `L1`: every page needs an `id`, a `title` and a `status`.
///
/// `status` is optional in the reader (absent means `draft`), so its presence
/// is checked textually; an empty `id` or `title` is a missing field.
fn check_required_fields(
    text: &str,
    front: &page::FrontMatter,
    key: &str,
    found: &mut Vec<Finding>,
) {
    let mut missing: Vec<&str> = Vec::new();
    if front.id.trim().is_empty() {
        missing.push("id");
    }
    if front.title.trim().is_empty() {
        missing.push("title");
    }
    if !has_field(text, "status") {
        missing.push("status");
    }
    for field in missing {
        found.push(Finding::new(
            "L1",
            Level::Error,
            Some(String::from(key)),
            format!("missing required `{field}` field"),
        ));
    }
}

/// `L2`: every `[[wikilink]]` outside a code fence must name a page.
fn check_dead_links(cfg: &Config, pages: &[PageInfo], found: &mut Vec<Finding>) {
    let identities: Vec<PageIdentity> = pages
        .iter()
        .map(|info| PageIdentity {
            key: info.key.clone(),
            id: info.id.clone(),
            wiki_rel: info.rel.clone(),
        })
        .collect();
    let resolver = Resolver::new(&identities);

    for info in pages {
        for target in wikilinks(&info.body) {
            let (resolution, _) = resolver.resolve(&target, &cfg.paths.wiki);
            let message = match resolution {
                Resolution::Resolved => continue,
                Resolution::Missing => format!("link `[[{target}]]` names no page"),
                Resolution::Ambiguous => {
                    format!("link `[[{target}]]` is ambiguous; use the type-qualified name")
                }
            };
            found.push(Finding::new(
                "L2",
                Level::Error,
                Some(info.key.clone()),
                message,
            ));
        }
    }
}

/// `L3`: one id must live in one type directory.
fn check_duplicate_ids(pages: &[PageInfo], found: &mut Vec<Finding>) {
    let mut by_id: BTreeMap<&str, Vec<&PageInfo>> = BTreeMap::new();
    for info in pages {
        if !info.id.is_empty() {
            by_id.entry(info.id.as_str()).or_default().push(info);
        }
    }
    for (id, group) in by_id {
        if group.len() < 2 {
            continue;
        }
        let paths: Vec<&str> = group.iter().map(|info| info.rel.as_str()).collect();
        for info in group {
            found.push(Finding::new(
                "L3",
                Level::Error,
                Some(info.key.clone()),
                format!(
                    "id `{id}` is used by {} pages: {}",
                    paths.len(),
                    paths.join(", ")
                ),
            ));
        }
    }
}

/// `L6`: a page's `sources` must name a file that exists.
///
/// A summary reference is exempt: after a seal re-hangs `sources` onto
/// `wiki/archive/<id>.md`, the summary is read through `recall` and its file may
/// legitimately leave the tree with a purge, so requiring it to exist would make
/// a clean archive look broken.
fn check_missing_sources(root: &Path, cfg: &Config, pages: &[PageInfo], found: &mut Vec<Finding>) {
    let raw_prefix = cfg.paths.raw.trim_start_matches("./").trim_end_matches('/');
    for info in pages {
        for source in &info.front.sources {
            let base = source
                .split_once('#')
                .map_or(source.as_str(), |(head, _)| head);
            let base = base.trim();
            if base.is_empty() {
                continue;
            }
            let path = if base == raw_prefix {
                continue;
            } else if raw_prefix.is_empty() || base.starts_with(&format!("{raw_prefix}/")) {
                root.join(base)
            } else {
                root.join(raw_prefix).join(base)
            };
            if !path.is_file() {
                found.push(Finding::new(
                    "L6",
                    Level::Error,
                    Some(info.key.clone()),
                    format!("source `{source}` does not exist"),
                ));
            }
        }
    }
}

/// `L12` and `L13` for one page: it cites nothing, or it is still unfinished.
///
/// Both are warnings, not errors: a draft that an agent has not finished writing
/// yet is a normal state of the tree, but a *published* page with nothing behind
/// it is what this rule exists to catch. `sync` calls the same function over the
/// pages it just handed to an agent, so the round warns without waiting for
/// somebody to run `lint`.
pub fn page_quality(key: &str, front: &page::FrontMatter, body: &str) -> Vec<Finding> {
    let mut found = Vec::new();
    if front.sources.is_empty() {
        found.push(Finding::new(
            "L12",
            Level::Warning,
            Some(key.to_owned()),
            String::from("cites no source; a conclusion with nothing behind it cannot be checked"),
        ));
    }
    if let Some(marker) = first_unfinished_marker(body) {
        found.push(Finding::new(
            "L13",
            Level::Warning,
            Some(key.to_owned()),
            format!("unfinished marker `{marker}` left in the body"),
        ));
    }
    found
}

/// Markers an author leaves behind on purpose while a page is half-written.
const UNFINISHED_MARKERS: [&str; 4] = ["TODO", "FIXME", "待补", "占位"];

/// The first unfinished marker in `body` outside a fenced block.
///
/// Fenced blocks are skipped because a page may legitimately quote code that
/// contains `TODO`. One marker is enough to report a page: five of them are one
/// unfinished page, not five findings.
fn first_unfinished_marker(body: &str) -> Option<&'static str> {
    let mut in_fence = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(marker) = UNFINISHED_MARKERS
            .iter()
            .find(|marker| line.contains(*marker))
        {
            return Some(marker);
        }
    }
    None
}

/// `L8`: the body must reach `[audit].min_page_chars` characters.
///
/// Whitespace around the body does not count, and characters are `char`s, not
/// bytes: a nine-character Chinese body is nine, however many bytes it takes.
fn check_short_pages(cfg: &Config, pages: &[PageInfo], found: &mut Vec<Finding>) {
    let minimum = cfg.audit.min_page_chars;
    for info in pages {
        let chars = info.body.trim().chars().count();
        if chars < minimum {
            found.push(Finding::new(
                "L8",
                Level::Warning,
                Some(info.key.clone()),
                format!("body has {chars} characters, below the {minimum} minimum"),
            ));
        }
    }
}

/// `L7`: a file under `raw/` that the ledger does not know is invisible to
/// every other check, so it is reported.
///
/// Both halves of the question come from `scan`: [`scan::raw_keys`] is the file
/// set `scan --layer raw` classifies, and [`scan::RawLedger`] is the ledger join.
/// Answering either one here again is how this rule once kept its own copy of
/// "known" and drifted from `scan`.
fn check_unledgered_raws(
    root: &Path,
    cfg: &Config,
    store: &Store,
    found: &mut Vec<Finding>,
) -> Result<()> {
    let ledger = RawLedger::load(store)?;
    for key in scan::raw_keys(root, cfg)? {
        if ledger.knows(&key) {
            continue;
        }
        found.push(Finding::new(
            "L7",
            Level::Warning,
            None,
            format!(
                "raw `{key}` is not in the ledger; if it declares `source:` and is not pinned, \
                 `ingest <that source key> --dry-run` registers it, otherwise delete it \
                 (`raw/` is derived: a file that cannot name its source is never adopted)"
            ),
        ));
    }
    Ok(())
}

/// Which raws each page cites, as raw-relative keys.
///
/// A summary reference is not a raw file, so it never enters the ledger
/// accounting the caller builds on this map.
fn cited_raws(cfg: &Config, pages: &[PageInfo]) -> BTreeMap<String, Vec<String>> {
    let prefix = cfg.paths.raw.clone();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for info in pages {
        let sources: Vec<String> = info.front.sources.clone();
        for key in source_refs::source_keys(&sources, &prefix) {
            out.entry(key).or_default().push(info.key.clone());
        }
    }
    out
}

/// `L10`: one raw file cited by more than three pages is a sign the source is
/// doing too much, or that the split is wrong.
fn check_over_shared_raws(cited: &BTreeMap<String, Vec<String>>, found: &mut Vec<Finding>) {
    for (key, pages) in cited {
        if pages.len() > 3 {
            found.push(Finding::new(
                "L10",
                Level::Info,
                None,
                format!(
                    "raw `{key}` is cited by {} pages: {}",
                    pages.len(),
                    pages.join(", ")
                ),
            ));
        }
    }
}

/// `L9`: the ledger's digest of the body no longer matches the file while the
/// date still does.
///
/// No record means no finding: the ledger is not in git, so a fresh clone has
/// nothing to compare against and reporting the whole wiki would be noise.
fn check_page_meta(
    root: &Path,
    store: &Store,
    pages: &[PageInfo],
    found: &mut Vec<Finding>,
) -> Result<()> {
    let metas: BTreeMap<String, PageMetaRow> = store
        .all_page_metas()?
        .into_iter()
        .map(|row| (row.page_key.clone(), row))
        .collect();

    for info in pages {
        if is_asset(&info.front) {
            continue;
        }
        let Some(row) = metas.get(&info.key) else {
            continue;
        };
        if row.updated != info.front.updated.to_string() {
            continue;
        }
        let current = hash::hash_file(&root.join(&info.key), true)?;
        if current != row.hash {
            found.push(Finding::new(
                "L9",
                Level::Warning,
                Some(info.key.clone()),
                format!(
                    "body changed but `updated` is still {}; run `lint --fix`",
                    row.updated
                ),
            ));
        }
    }
    Ok(())
}

/// `L11`: an asset raw file no page cites cannot be reached from the wiki.
fn check_unreferenced_assets(
    store: &Store,
    cited: &BTreeMap<String, Vec<String>>,
    found: &mut Vec<Finding>,
) -> Result<()> {
    for row in store.all_raws()? {
        if cited.contains_key(&row.key) || !is_asset_key(&row.key, &row.source_key) {
            continue;
        }
        found.push(Finding::new(
            "L11",
            Level::Info,
            None,
            format!("asset `{}` is not referenced by any page", row.key),
        ));
    }
    Ok(())
}

/// Whether a page is an asset page (`asset: true`), whose body changes in
/// place and whose `updated` is not expected to move.
fn is_asset(front: &page::FrontMatter) -> bool {
    front
        .extra
        .iter()
        .any(|(key, value)| key == "asset" && value.trim() == "true")
}

/// Whether a raw file behaves like an asset: an image by extension, on either
/// the raw file itself or the source it came from.
fn is_asset_key(key: &str, source_key: &str) -> bool {
    const ASSET_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp"];
    let extension = |name: &str| {
        name.rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase())
    };
    extension(key)
        .or_else(|| extension(source_key))
        .is_some_and(|ext| ASSET_EXTENSIONS.contains(&ext.as_str()))
}

/// The page path without the wiki prefix and extension: `concept/a`.
fn rel_page(key: &str, wiki_prefix: &str) -> String {
    let trimmed = key
        .strip_prefix(wiki_prefix)
        .map_or(key, |rest| rest.strip_prefix('/').unwrap_or(rest));
    trimmed.trim_end_matches(".md").to_string()
}

/// Repository-relative path with `/` separators.
fn repo_key(root: &Path, path: &Path) -> String {
    paths::to_slash(path.strip_prefix(root).unwrap_or(path))
}

/// `[[target]]` values in `body`, skipping fenced code blocks.
pub(crate) fn wikilinks(body: &str) -> Vec<String> {
    let mut text = String::new();
    let mut fence = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if !fence {
            text.push_str(line);
            text.push('\n');
        }
    }
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else {
            break;
        };
        let target = after[..end].trim();
        if !target.is_empty() {
            out.push(target.to_string());
        }
        rest = &after[end + 2..];
    }
    out
}

/// Whether the frontmatter block of `text` has a top-level `name:` key.
fn has_field(text: &str, name: &str) -> bool {
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return false;
    }
    for line in lines {
        if line.trim_end() == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix(name) {
            if rest.starts_with(':') {
                return true;
            }
        }
    }
    false
}

/// The sentinel date the lenient probe uses for an unreadable `updated`.
fn epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 is a valid date")
}

/// Re-reads a page that failed to parse, repairing line by line at most four
/// times.
///
/// The reader is deliberately strict, and two of its refusals are worth
/// auditing rather than dropping the page: an unreadable `updated` (L5) and an
/// invalid `status` (L4). Each attempt removes the line the reader named, so
/// those two cases end with the rest of the frontmatter available to the other
/// rules; a date line inserted at the end covers the missing-`updated` case.
fn probe(text: &str) -> Option<(page::FrontMatter, String)> {
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let mut inserted_date = false;

    for _ in 0..4 {
        let joined = lines.join("\n");
        match page::parse(joined.as_bytes()) {
            Ok(pair) => return Some(pair),
            Err(PageError::BadFrontmatter { line, .. }) => {
                if line == 0 || line > lines.len() {
                    return None;
                }
                if lines[line - 1].trim_start().starts_with("---") {
                    if inserted_date {
                        return None;
                    }
                    let at = close_index(&lines)?;
                    lines.insert(at, String::from("updated: 1970-01-01"));
                    inserted_date = true;
                } else {
                    lines.remove(line - 1);
                }
            }
            Err(_) => return None,
        }
    }
    None
}

/// 0-based index of the frontmatter's closing `---` line.
fn close_index(lines: &[String]) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, line)| line.trim_end() == "---")
        .map(|(index, _)| index)
}
