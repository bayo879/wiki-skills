//! `lint`: the thirteen audit rules over `wiki/`, and the mechanical `--fix`.
//!
//! `lint` reads the wiki directory and the ledger and returns findings sorted by
//! severity, rule and page, so the output is a stable value that can be
//! snapshotted. `fix` repairs the defects that need no judgement — a missing or
//! malformed `updated`, an `updated` that did not move when the body changed,
//! and a stale `index.md` — then re-runs `lint` and reports what is left.
//!
//! The rules are the contract from the spec (`docs/superpowers/specs/...` §5.4),
//! one test per rule in `tests/lint.rs`:
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
//! - `L11` warning: a `log.md` entry does not match `## [date] action | title`.
//! - `L12` info: an asset raw file is cited by no page.
//! - `L13` info: a sealed summary nothing cites and nobody recalled.
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

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use chrono::{Local, NaiveDate};
use serde::Serialize;
use walkdir::WalkDir;

use crate::archive;
use crate::config::Config;
use crate::hash;
use crate::page::{self, PageError};
use crate::paths;
use crate::render;
use crate::staging;
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
    /// Rule identifier, `L1` through `L13`.
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
}

impl LintError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            LintError::Render(err) => err.code(),
            LintError::Store(_) => "E_DB",
            LintError::Hash(_) => "E_IO",
            LintError::Page(err) => err.code(),
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
    check_unledgered_raws(root, cfg, store, &mut found)?;

    let cited = cited_raws(cfg, &pages);
    check_over_shared_raws(&cited, &mut found);
    check_page_meta(root, store, &pages, &mut found)?;
    check_log(root, cfg, &mut found);
    check_unreferenced_assets(store, &cited, &mut found)?;
    check_idle_summaries(root, cfg, store, &pages, &mut found)?;

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
            if set_updated_lenient(&root.join(key), today).is_ok() {
                fixed += 1;
            }
        }
    }

    for finding in found.iter().filter(|f| f.rule == "L9") {
        if let Some(key) = &finding.page {
            let path = root.join(key);
            set_updated_lenient(&path, today)?;
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
    let mut known: BTreeSet<String> = BTreeSet::new();
    for info in pages {
        known.insert(info.id.clone());
        known.insert(info.rel.clone());
        known.insert(info.key.clone());
        known.insert(info.key.trim_end_matches(".md").to_string());
        known.insert(format!("{}/{}", cfg.paths.wiki, info.rel));
    }

    for info in pages {
        for target in wikilinks(&info.body) {
            let wanted = normalise_link(&target, &cfg.paths.wiki);
            if wanted.is_empty() {
                continue;
            }
            if !known.contains(&wanted) {
                found.push(Finding::new(
                    "L2",
                    Level::Error,
                    Some(info.key.clone()),
                    format!("link `[[{target}]]` names no page"),
                ));
            }
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
            if is_archive_reference(source, cfg) {
                continue;
            }
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

/// `L8`: the body must reach `[audit].min_page_chars` characters.
///
/// Whitespace around the body does not count, and characters are `char`s, not
/// bytes: a nine-character Chinese body is nine, however many bytes it takes.
/// An archived summary is exempt: it is a distillation, and two sentences about a
/// month are all it promises.
fn check_short_pages(cfg: &Config, pages: &[PageInfo], found: &mut Vec<Finding>) {
    let minimum = cfg.audit.min_page_chars;
    for info in pages {
        if is_archived(&info.front) {
            continue;
        }
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
fn check_unledgered_raws(
    root: &Path,
    cfg: &Config,
    store: &Store,
    found: &mut Vec<Finding>,
) -> Result<()> {
    let raw_dir = root.join(&cfg.paths.raw);
    if !raw_dir.is_dir() {
        return Ok(());
    }
    let known: BTreeSet<String> = store.all_raws()?.into_iter().map(|row| row.key).collect();
    for entry in WalkDir::new(&raw_dir).follow_links(false) {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(&raw_dir).unwrap_or(entry.path());
        let key = paths::to_slash(rel);
        if paths::is_ignored(&key) || known.contains(&key) {
            continue;
        }
        found.push(Finding::new(
            "L7",
            Level::Warning,
            None,
            format!("raw `{key}` is not in the ledger; run `scan` to register it"),
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
        let sources: Vec<String> = info
            .front
            .sources
            .iter()
            .filter(|source| !is_archive_reference(source, cfg))
            .cloned()
            .collect();
        for key in staging::source_keys(&sources, &prefix) {
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
                    "body changed but `updated` is still {}; bump it or run `lint --fix`",
                    row.updated
                ),
            ));
        }
    }
    Ok(())
}

/// `L11`: every `## ` line of `log.md` must be a log entry.
fn check_log(root: &Path, cfg: &Config, found: &mut Vec<Finding>) {
    let path = root.join(&cfg.paths.wiki).join("log.md");
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    let key = format!("{}/log.md", cfg.paths.wiki.trim_end_matches('/'));
    for (index, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.starts_with("## ") && !line.starts_with("## [") {
            found.push(Finding::new(
                "L11",
                Level::Warning,
                Some(key.clone()),
                format!(
                    "log line {}: `{line}` does not match `## [date] action | title`",
                    index + 1
                ),
            ));
        }
    }
}

/// `L12`: an asset raw file no page cites cannot be reached from the wiki.
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
            "L12",
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

/// Whether a page is an archived summary (`archived: true`), whose body is a
/// distillation and whose citations were re-hung by a seal.
fn is_archived(front: &page::FrontMatter) -> bool {
    front
        .extra
        .iter()
        .any(|(key, value)| key == "archived" && value.trim() == "true")
}

/// Whether a `sources` entry names a conversation summary rather than a raw
/// file.
fn is_archive_reference(source: &str, cfg: &Config) -> bool {
    resolved_reference(source, cfg).starts_with(&format!("{}/", archive::DIR))
}

/// The wiki-relative name a reference resolves to.
///
/// A summary citation is written from the page, so it may arrive as
/// `../../wiki/archive/x.md`, `wiki/archive/x.md` or `archive/x.md`; dropping the
/// leading `.` segments and the wiki prefix leaves one name to compare.
fn resolved_reference(target: &str, cfg: &Config) -> String {
    let mut text = target
        .split_once('#')
        .map_or(target, |(head, _)| head)
        .trim();
    while let Some(rest) = text.strip_prefix("./").or_else(|| text.strip_prefix("../")) {
        text = rest;
    }
    normalise_link(text, &cfg.paths.wiki)
}

/// `L13`: a summary nothing cites and nobody recalled is dead weight.
///
/// The age counts from the last `recall`, falling back to the seal: a summary no
/// page points at and no session has read is invisible to the wiki, and the next
/// purge is the only thing that will make it disappear.
///
/// Inbound references are the whole wiki's `sources` and `[[links]]`; a summary
/// citing another summary does not keep it alive, which is the same rule
/// `archive::purge` applies to the pages it may delete.
fn check_idle_summaries(
    root: &Path,
    cfg: &Config,
    store: &Store,
    pages: &[PageInfo],
    found: &mut Vec<Finding>,
) -> Result<()> {
    let archives = store.all_archives()?;
    if archives.is_empty() {
        return Ok(());
    }
    let archive_dir = archive::dir(root, cfg);
    let mut inbound: BTreeSet<String> = BTreeSet::new();
    for info in pages {
        if root.join(&info.key).starts_with(&archive_dir) {
            continue;
        }
        for source in &info.front.sources {
            let name = resolved_reference(source, cfg);
            if name.starts_with(&format!("{}/", archive::DIR)) {
                inbound.insert(name);
            }
        }
        for link in wikilinks(&info.body) {
            inbound.insert(resolved_reference(&link, cfg));
        }
    }

    let today = Local::now().date_naive();
    for row in &archives {
        let rel = rel_page(&row.key, &cfg.paths.wiki);
        let stem = rel.rsplit('/').next().unwrap_or(&rel);
        if inbound.contains(&rel) || inbound.contains(stem) {
            continue;
        }
        let Some(last) = row
            .last_recalled_at
            .as_deref()
            .and_then(archive::parse_date)
            .or_else(|| archive::parse_date(&row.sealed_at))
        else {
            // An unreadable timestamp cannot prove the summary is idle, and the
            // safe direction for an advisory finding is silence.
            continue;
        };
        let idle = (today - last).num_days();
        if idle <= i64::from(cfg.memory.idle_days) {
            continue;
        }
        found.push(Finding::new(
            "L13",
            Level::Info,
            Some(row.key.clone()),
            format!("no page cites this summary and it has not been recalled in {idle} days"),
        ));
    }
    Ok(())
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

/// A link target as the set of page names it may mean.
pub(crate) fn normalise_link(target: &str, wiki_prefix: &str) -> String {
    let mut text = target
        .split_once('#')
        .map_or(target, |(head, _)| head)
        .trim();
    loop {
        let stripped = text.strip_prefix("./").unwrap_or(text);
        if stripped == text {
            break;
        }
        text = stripped;
    }
    let prefix = wiki_prefix.trim_start_matches("./").trim_end_matches('/');
    if !prefix.is_empty() {
        if let Some(rest) = text.strip_prefix(prefix) {
            if let Some(rest) = rest.strip_prefix('/') {
                text = rest;
            }
        }
    }
    text.trim_end_matches(".md").to_string()
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

/// Writes `date` into the page's `updated` field, inserting the line when it is
/// absent or unusable.
///
/// The reader's own `write_updated` refuses to invent a missing field, which is
/// the right default for a tool that must not guess; `--fix` is the explicit
/// request, so it may.
fn set_updated_lenient(path: &Path, date: NaiveDate) -> Result<()> {
    let text = fs::read_to_string(path).map_err(|source| {
        LintError::Page(PageError::Io {
            path: path.to_path_buf(),
            source,
        })
    })?;
    let lines: Vec<&str> = text.lines().collect();
    let Some(close) = close_index_lines(&lines) else {
        return Err(LintError::Page(PageError::BadFrontmatter {
            line: 1,
            message: String::from("page has no frontmatter block"),
        }));
    };
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = String::new();
    let mut replaced = false;
    for (index, line) in lines.iter().enumerate() {
        if index > 0 && index < close && line.trim_start().starts_with("updated:") {
            if !replaced {
                out.push_str(&format!("updated: {date}"));
                out.push_str(eol);
                replaced = true;
            }
            continue;
        }
        if index == close && !replaced {
            out.push_str(&format!("updated: {date}"));
            out.push_str(eol);
        }
        out.push_str(line);
        out.push_str(eol);
    }
    fs::write(path, out).map_err(|source| {
        LintError::Page(PageError::Io {
            path: path.to_path_buf(),
            source,
        })
    })
}

/// Like [`close_index`] but over borrowed lines.
fn close_index_lines(lines: &[&str]) -> Option<usize> {
    if lines.first().map(|line| line.trim_end()) != Some("---") {
        return None;
    }
    lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, line)| line.trim_end() == "---")
        .map(|(index, _)| index)
}
