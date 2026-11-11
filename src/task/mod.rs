//! The ingest task tree.
//!
//! A source becomes one root task (`breakdown`); the root is delivered with the
//! source text, the page index and the pages that already cite the raw, and the
//! agent answers with a leaf list. Each leaf (`page`) names one target page and
//! is delivered on its own; its payload is written to
//! `.wiki/cache/staging/<source_key>/...`, never to `.wiki/pages/`. When the
//! last leaf closes, the root merges the whole batch in one step.
//!
//! The tree is the execution state of one ingest unit; it lives in SQLite
//! (`tasks`, `task_baseline`), so "no row" means "nothing left to do". Closing a
//! task deletes it; history goes to the audit page.
//!
//! Two rules hold the whole thing together:
//!
//! - **Leaves first.** A root is only broken down when no leaf anywhere is
//!   pending, because a breakdown reads the index as it is now; breaking down a
//!   later source early would plan against pages that are about to change.
//! - **The tool owns the page structure.** `id`, `type`, `sources`, `updated`
//!   and the frontmatter syntax come from the task, not from the payload.

mod plan;
mod route;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{Local, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::hash;
use crate::ledger::Ledger;
use crate::page::{self, FrontMatter, Status};
use crate::paths;
use crate::query;
use crate::render;
use crate::store::{NewTask, Store, StoreError, TaskRow, WorkItem};

/// Failure while advancing or closing the task tree.
#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    /// The ledger could not be read or written.
    #[error("{0}")]
    Store(#[from] StoreError),
    /// A file or directory could not be read or written.
    #[error("cannot access {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A page was not valid.
    #[error("{0}")]
    Page(#[from] page::PageError),
    /// A path could not be made repository-relative.
    #[error("{0}")]
    Path(#[from] paths::PathError),
    /// A staged or merged page could not be written.
    #[error("{0}")]
    Render(#[from] render::RenderError),
    /// A file could not be digested.
    #[error("{0}")]
    Hash(#[from] hash::HashError),
    /// The search index could not be refreshed after a merge.
    #[error("{0}")]
    Query(#[from] query::QueryError),
    /// The payload or the task itself was not acceptable.
    #[error("{message}")]
    Bad { code: &'static str, message: String },
    /// No open task has that id.
    #[error("no open task with id {0}")]
    Unknown(i64),
}

impl TaskError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            TaskError::Store(_) | TaskError::Query(_) => "E_DB",
            TaskError::Io { .. }
            | TaskError::Path(_)
            | TaskError::Render(_)
            | TaskError::Hash(_) => "E_IO",
            TaskError::Page(err) => err.code(),
            TaskError::Bad { code, .. } => code,
            TaskError::Unknown(_) => "E_NO_TASK",
        }
    }
}

/// Result of the task helpers.
pub type Result<T> = std::result::Result<T, TaskError>;

fn bad(code: &'static str, message: impl Into<String>) -> TaskError {
    TaskError::Bad {
        code,
        message: message.into(),
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn today() -> chrono::NaiveDate {
    Local::now().date_naive()
}

/// What one breakdown task submits: leaves plus the pages it deliberately leaves
/// alone.
#[derive(Debug, Deserialize)]
struct BreakdownDoc {
    #[serde(default)]
    leaves: Vec<LeafSpec>,
    #[serde(default)]
    unchanged: Vec<Unchanged>,
}

#[derive(Debug, Deserialize)]
struct LeafSpec {
    #[serde(rename = "type")]
    typ: String,
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    brief: String,
}

#[derive(Debug, Deserialize)]
struct Unchanged {
    path: String,
    #[serde(default)]
    reason: String,
}

/// What one leaf submits: content, never structure.
#[derive(Debug, Deserialize)]
struct LeafPayload {
    title: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    body: String,
}

/// A page changed outside the task tree: the agent decides whether the edit
/// stands (`--accept`) or the sources behind it are regenerated.
#[derive(Debug, Serialize, Deserialize)]
struct ReconcilePayload {
    /// Hash of the page as it stands now, so `--accept` absorbs exactly this.
    page_hash: String,
    /// Raw keys the page cites; `--regenerate` re-queues each of them.
    raw_keys: Vec<String>,
}

/// What the agent decided about a page edited outside the task tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The edit stands; absorb exactly this page hash into the baseline.
    Accept,
    /// Re-queue every source the page cites and regenerate it.
    Regenerate,
}

/// The leaf brief the workspace shows the agent.
#[derive(Debug, Serialize)]
struct LeafNote {
    target_path: String,
    target_type: String,
    target_id: String,
    title: String,
    brief: String,
    source_key: String,
}

/// One root and its leaves, for `task status`.
#[derive(Debug, Serialize)]
pub struct RootView {
    pub id: i64,
    pub source_key: String,
    pub status: String,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub leaves: Vec<TaskRow>,
}

/// The task tree's current shape.
#[derive(Debug, Serialize)]
pub struct TaskStatus {
    pub roots: Vec<RootView>,
    pub current: Option<TaskRow>,
    pub reconcile: Vec<TaskRow>,
    pub failed: Vec<TaskRow>,
    pub blocked: Vec<TaskRow>,
}

/// What one advance actually settled, alongside the task it delivered.
///
/// A root finishing is a side effect of advancing; callers must report the
/// merged pages and confirmed sources instead of inferring completion from
/// `task.is_none()`.
#[derive(Debug, Default)]
pub struct Advance {
    pub task: Option<TaskRow>,
    pub merged_pages: Vec<String>,
    pub confirmed_sources: Vec<String>,
    pub issues: Vec<String>,
}

/// The work interface handed to the agent: only what the task needs, never a
/// database row.
#[derive(Debug, Serialize)]
pub struct TaskView {
    pub id: i64,
    pub kind: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<TaskSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<TaskTarget>,
    pub affected_pages: Vec<String>,
    pub submit: SubmitView,
}

#[derive(Debug, Serialize)]
pub struct TaskSource {
    pub key: String,
    pub read_ref: String,
}

#[derive(Debug, Serialize)]
pub struct TaskTarget {
    pub reference: String,
    pub exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SubmitView {
    pub command: String,
    pub schema: String,
}

/// Builds the agent-facing view of one task. Diagnostics stay in `status`.
pub fn view(root: &Path, cfg: &Config, store: &Store, task: &TaskRow) -> Result<TaskView> {
    let raw_key = work_item_raw_key(store, task.source_key.as_deref())?;
    let source = task.source_key.as_ref().map(|key| TaskSource {
        key: key.clone(),
        read_ref: if raw_key.is_empty() {
            String::new()
        } else {
            format!("{}/{}", cfg.paths.raw.trim_end_matches('/'), raw_key)
        },
    });
    let (leaf_title, leaf_brief) = leaf_note(task);
    let target = task.target_path.as_ref().map(|reference| TaskTarget {
        reference: reference.clone(),
        exists: root.join(reference).is_file(),
        title: leaf_title,
        brief: leaf_brief,
    });
    let affected_pages = if task.kind == "breakdown" && !raw_key.is_empty() {
        affected_pages(store, std::slice::from_ref(&raw_key))?
    } else {
        Vec::new()
    };
    let (suffix, schema) = match task.kind.as_str() {
        "page" => ("--payload <FILE>", "page-content-v1"),
        "breakdown" => ("--leaves <FILE>", "leaf-list-v1"),
        "reconcile" => ("--accept | --regenerate", "reconcile-v1"),
        _ => ("", ""),
    };
    Ok(TaskView {
        id: task.id,
        kind: task.kind.clone(),
        status: task.status.clone(),
        source,
        target,
        affected_pages,
        submit: SubmitView {
            command: format!("llm-wiki task done {} {suffix}", task.id),
            schema: String::from(schema),
        },
    })
}

fn leaf_note(task: &TaskRow) -> (Option<String>, Option<String>) {
    if task.kind != "page" {
        return (None, None);
    }
    let Ok(note) =
        serde_json::from_str::<serde_json::Value>(task.payload_json.as_deref().unwrap_or("null"))
    else {
        return (None, None);
    };
    (
        note.get("title")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        note.get("brief")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    )
}

/// Ensures a root exists for `item`, and snapshots `pages/` as its baseline.
///
/// Idempotent: the unique index on `(source_key) WHERE kind='breakdown'` is the
/// authority, so a round that runs twice keeps the first root and its baseline.
pub fn ensure_root(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    item: &WorkItem,
    stamp: &str,
) -> Result<bool> {
    if store
        .tasks()?
        .iter()
        .any(|t| t.kind == "breakdown" && t.source_key.as_deref() == Some(item.source_key.as_str()))
    {
        return Ok(false);
    }
    ensure_global_baseline(root, cfg, store)?;
    let mut new = NewTask::pending("breakdown", stamp);
    new.source_key = Some(item.source_key.clone());
    new.raw_hash = Some(item.raw_hash.clone());
    store.insert_task(&new)?;
    Ok(true)
}

/// Delivers the one task the next round should run, or `None` when the tree is
/// settled.
///
/// Order: finish any root whose leaves are all closed → a pending leaf → a
/// pending root. An `active` leaf is reset to `pending` first: one machine, one
/// agent, so an `active` row is a delivery from a previous round, not a lease.
pub fn advance(root: &Path, cfg: &Config, store: &mut Store) -> Result<Advance> {
    let mut out = Advance::default();
    let stamp = now();
    reconcile_scan(root, cfg, store)?;
    // Later steps read relations (affected pages, page quality); one refresh
    // here keeps every caller of `advance` on the same snapshot.
    query::refresh(root, cfg, store)?;
    for task in store.tasks()? {
        if (task.kind == "page" || task.kind == "reconcile") && task.status == "active" {
            store.set_task_status(task.id, "pending", &stamp)?;
        }
    }

    loop {
        let tasks = store.tasks()?;

        // 1. A root whose leaves are gone is merged here, not handed out.
        if let Some(root_task) = tasks.iter().find(|t| {
            t.kind == "breakdown"
                && t.status == "active"
                && t.payload_json.is_some()
                && !tasks.iter().any(|child| child.parent_id == Some(t.id))
        }) {
            let before = out.merged_pages.len();
            let source_key = root_task.source_key.clone().unwrap_or_default();
            match finish_root(root, cfg, store, root_task, &mut out.merged_pages) {
                Ok(()) => {
                    out.confirmed_sources.push(source_key);
                    let indexed: std::collections::BTreeSet<String> = store
                        .all_relation_nodes()?
                        .into_iter()
                        .map(|node| node.page_key)
                        .collect();
                    for key in &out.merged_pages[before..] {
                        if !indexed.contains(key) {
                            out.issues.push(format!(
                                "merged page {key} is missing from the relation index after refresh"
                            ));
                        }
                    }
                    refresh_global_baseline(root, cfg, store)?;
                    continue;
                }
                Err(err) => {
                    let message = format!("{}: {err}", err.code());
                    let grew = out.merged_pages.len() > before;
                    out.issues.push(if grew {
                        format!(
                            "source {source_key} merged {} page(s) but its settlement failed: {message}",
                            out.merged_pages.len() - before
                        )
                    } else {
                        format!("source {source_key} could not settle: {message}")
                    });
                    store.fail_task(root_task.id, &message, &stamp)?;
                    continue;
                }
            }
        }

        // 2. Leaves first: never break down a later source while any leaf is open.
        if let Some(task) = tasks
            .iter()
            .filter(|t| t.kind == "page" && t.status == "pending")
            .min_by_key(|t| t.id)
        {
            store.set_task_status(task.id, "active", &stamp)?;
            out.task = store.task(task.id)?;
            return Ok(out);
        }

        // 3. A page edited outside the tree goes to review before new work.
        if let Some(task) = tasks
            .iter()
            .filter(|t| t.kind == "reconcile" && t.status == "pending")
            .min_by_key(|t| t.id)
        {
            store.set_task_status(task.id, "active", &stamp)?;
            out.task = store.task(task.id)?;
            return Ok(out);
        }

        // 4. Only now, the earliest root waiting for a breakdown.
        if let Some(task) = tasks
            .iter()
            .filter(|t| {
                t.kind == "breakdown"
                    && (t.status == "pending" || t.status == "active")
                    && t.payload_json.is_none()
            })
            .min_by_key(|t| t.id)
        {
            let drifted = current_raw_hash(store, task.source_key.as_deref())?;
            if drifted.as_deref() != task.raw_hash.as_deref() {
                // The source changed under a plan made for older bytes: replan,
                // but do not let one file replay that forever.
                let attempts = task.attempts + 1;
                if attempts >= 2 {
                    store.set_task_status(task.id, "blocked", &stamp)?;
                    store.fail_task(
                        task.id,
                        "source changed twice while it was being planned",
                        &stamp,
                    )?;
                    continue;
                }
                store.update_task_source(task.id, drifted.as_deref(), attempts, &stamp)?;
            }
            store.set_task_status(task.id, "active", &stamp)?;
            out.task = store.task(task.id)?;
            return Ok(out);
        }

        // Nothing is owed. `reconcile_scan` has already absorbed edits that
        // have no source to answer to and queued the rest for review.
        return Ok(out);
    }
}

/// Closes one task the agent says it finished.
///
/// A `page` needs `payload` (the JSON content) and must leave `pages/` exactly
/// as the root's baseline found it; a `breakdown` needs `leaves`; a `reconcile`
/// needs exactly one of `accept` or `regenerate`. Anything else is a usage error
/// rather than a guess.
pub fn close(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    id: i64,
    payload: Option<&Path>,
    leaves: Option<&Path>,
    verdict: Option<Verdict>,
) -> Result<Advance> {
    let task = store.task(id)?.ok_or(TaskError::Unknown(id))?;
    match task.kind.as_str() {
        "page" if verdict.is_some() => {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                "only a reconcile task takes --accept or --regenerate",
            ))
        }
        "page" => {
            let payload = payload.ok_or_else(|| {
                bad(
                    "E_TASK_BAD_PAYLOAD",
                    "a page task needs --payload <file> with the page content",
                )
            })?;
            close_page(root, cfg, store, &task, payload)?;
        }
        "breakdown" => {
            let leaves = leaves.ok_or_else(|| {
                bad(
                    "E_TASK_BAD_PAYLOAD",
                    "a breakdown task needs --leaves <file> with the leaf list",
                )
            })?;
            close_breakdown(cfg, store, &task, leaves)?;
        }
        "reconcile" => {
            close_reconcile(root, cfg, store, &task, verdict)?;
        }
        other => {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("a {other} task is advanced by the tool, not by `task done`"),
            ))
        }
    }
    advance(root, cfg, store)
}

/// Resets a failed or blocked task to pending. Human-only; nothing calls it
/// automatically.
pub fn retry(store: &mut Store, id: i64) -> Result<TaskRow> {
    let task = store.task(id)?.ok_or(TaskError::Unknown(id))?;
    // A parked `convert` row is a failure counter, not a unit of work: clear
    // the note and drop the row, and the next round meets the source fresh.
    if task.kind == "convert" {
        if let Some(key) = task.source_key.as_deref() {
            crate::ledger::Ledger::new(store).clear_failure(key)?;
        }
        store.delete_task(id)?;
        return Ok(task);
    }
    if task.status == "pending" || task.status == "active" {
        return Ok(task);
    }
    store.retry_task(id, &now())?;
    store.task(id)?.ok_or(TaskError::Unknown(id))
}

/// Reads the tree without advancing it.
pub fn status(store: &mut Store) -> Result<TaskStatus> {
    let tasks = store.tasks()?;
    let mut roots = Vec::new();
    for task in tasks.iter().filter(|t| t.kind == "breakdown") {
        roots.push(RootView {
            id: task.id,
            source_key: task.source_key.clone().unwrap_or_default(),
            status: task.status.clone(),
            attempts: task.attempts,
            last_error: task.last_error.clone(),
            leaves: tasks
                .iter()
                .filter(|child| child.parent_id == Some(task.id))
                .cloned()
                .collect(),
        });
    }
    let current = tasks
        .iter()
        .filter(|t| t.kind == "page" && t.status == "pending")
        .min_by_key(|t| t.id)
        .or_else(|| {
            tasks
                .iter()
                .filter(|t| t.kind == "reconcile" && t.status == "pending")
                .min_by_key(|t| t.id)
        })
        .or_else(|| {
            tasks
                .iter()
                .filter(|t| t.kind == "breakdown" && t.status == "pending")
                .min_by_key(|t| t.id)
        })
        .cloned();
    Ok(TaskStatus {
        roots,
        current,
        reconcile: tasks
            .iter()
            .filter(|t| t.kind == "reconcile")
            .cloned()
            .collect(),
        failed: tasks
            .iter()
            .filter(|t| t.status == "failed")
            .cloned()
            .collect(),
        blocked: tasks
            .iter()
            .filter(|t| t.status == "blocked")
            .cloned()
            .collect(),
    })
}

/// The pages that already cite any of `raw_keys`, sorted.
///
/// Reads the relation index rather than walking the page tree: `advance`
/// refreshes the index first, so this is the same reverse lookup `lint` and
/// `query --source` use. A caller must not treat a stale index as truth.
pub fn affected_pages(store: &Store, raw_keys: &[String]) -> Result<Vec<String>> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for raw_key in raw_keys {
        out.extend(store.pages_citing(raw_key)?);
    }
    Ok(out.into_iter().collect())
}

/// Looks for authored pages that moved since the baseline.
///
/// A page with sources is queued as a `reconcile` task: the agent reads it and
/// decides whether the edit stands (`--accept`) or the sources behind it are
/// regenerated (`--regenerate`). A page with no sources has nothing to compare
/// against, so the baseline absorbs it. A page that disappeared is accepted as
/// a deletion here; a source that also disappeared is `on_missing`'s business,
/// not this function's.
fn reconcile_scan(root: &Path, cfg: &Config, store: &mut Store) -> Result<()> {
    ensure_global_baseline(root, cfg, store)?;
    let baseline: BTreeMap<String, String> = store.baseline(GLOBAL_BASELINE)?.into_iter().collect();
    let current = page_hashes(root, cfg)?;
    for key in baseline.keys() {
        if !current.contains_key(key) {
            store.delete_baseline_path(GLOBAL_BASELINE, key)?;
        }
    }
    let mut open: BTreeSet<String> = store
        .tasks()?
        .into_iter()
        .filter(|task| task.kind == "reconcile")
        .filter_map(|task| task.target_path)
        .collect();
    let stamp = now();
    for (key, digest) in &current {
        if baseline.get(key) == Some(digest) {
            continue;
        }
        let Ok((front, _)) = page::read(&root.join(key)) else {
            // An unparsable page is `doctor`'s to report; it cites nothing this
            // function could reconcile either.
            store.set_baseline(GLOBAL_BASELINE, key, digest)?;
            continue;
        };
        let raw_keys: Vec<String> = front
            .sources
            .iter()
            .map(|reference| reference_key(&cfg.paths.raw, reference))
            .filter(|raw| !raw.is_empty())
            .collect();
        if raw_keys.is_empty() || open.contains(key) {
            if raw_keys.is_empty() {
                store.set_baseline(GLOBAL_BASELINE, key, digest)?;
            }
            continue;
        }
        let payload = serde_json::to_string(&ReconcilePayload {
            page_hash: digest.clone(),
            raw_keys,
        })
        .map_err(|err| {
            bad(
                "E_TASK_BAD_PAYLOAD",
                format!("cannot encode reconcile task: {err}"),
            )
        })?;
        let mut new = NewTask::pending("reconcile", &stamp);
        new.target_path = Some(key.clone());
        new.payload_json = Some(payload);
        store.insert_task(&new)?;
        open.insert(key.clone());
    }
    Ok(())
}

/// Settles one page that changed outside the tree.
///
/// `--accept` absorbs the edit; `--regenerate` re-queues every source the page
/// cites, so the normal breakdown/leaf path replaces it. Both advance the
/// baseline past the current bytes: an accepted edit must not be reported
/// again, and a page about to be replaced must not race its own review.
fn close_reconcile(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    task: &TaskRow,
    verdict: Option<Verdict>,
) -> Result<()> {
    let verdict = verdict.ok_or_else(|| {
        bad(
            "E_TASK_BAD_PAYLOAD",
            "a reconcile task needs exactly one of --accept or --regenerate",
        )
    })?;
    let target = task.target_path.clone().ok_or_else(|| {
        bad(
            "E_TASK_BAD_PAYLOAD",
            format!("reconcile task {} has no target", task.id),
        )
    })?;
    let payload: ReconcilePayload =
        serde_json::from_str(task.payload_json.as_deref().ok_or_else(|| {
            bad(
                "E_TASK_BAD_PAYLOAD",
                format!("reconcile task {} has no payload", task.id),
            )
        })?)
        .map_err(|err| {
            bad(
                "E_TASK_BAD_PAYLOAD",
                format!("reconcile payload is not valid JSON: {err}"),
            )
        })?;

    if verdict == Verdict::Regenerate {
        let mut requeued = 0;
        for raw_key in &payload.raw_keys {
            let Some(raw) = store.get_raw(raw_key)? else {
                continue;
            };
            let item = WorkItem {
                source_key: raw.source_key.clone(),
                raw_key: raw.key.clone(),
                raw_hash: raw.hash.clone(),
            };
            Ledger::new(store).import_work_item(&item)?;
            ensure_root(root, cfg, store, &item, &now())?;
            requeued += 1;
        }
        if requeued == 0 {
            return Err(bad(
                "E_TASK_BAD_SOURCE",
                format!(
                    "no raw left to regenerate from; the page cites {}",
                    payload.raw_keys.join(", ")
                ),
            ));
        }
    }
    store.set_baseline(GLOBAL_BASELINE, &target, &payload.page_hash)?;
    store.delete_task(task.id)?;
    Ok(())
}

fn close_page(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    task: &TaskRow,
    payload_path: &Path,
) -> Result<()> {
    task.parent_id.ok_or_else(|| {
        bad(
            "E_TASK_BAD_PAYLOAD",
            format!("page task {} has no parent root", task.id),
        )
    })?;
    let target = task.target_path.clone().ok_or_else(|| {
        bad(
            "E_TASK_BAD_PAYLOAD",
            format!("page task {} has no target", task.id),
        )
    })?;
    let (typ, id) = split_target(cfg, &target)?;
    let raw_key = work_item_raw_key(store, task.source_key.as_deref())?;

    let bytes = std::fs::read(payload_path).map_err(|source| TaskError::Io {
        path: payload_path.to_path_buf(),
        source,
    })?;
    let doc: LeafPayload = serde_json::from_slice(&bytes).map_err(|err| {
        bad(
            "E_TASK_BAD_PAYLOAD",
            format!("payload is not valid JSON: {err}"),
        )
    })?;
    let status = parse_status(doc.status.as_deref())?;
    if doc.title.trim().is_empty() {
        return Err(bad("E_TASK_BAD_PAYLOAD", "payload needs a non-empty title"));
    }

    // A page that already exists keeps its other sources: a rewrite of one
    // section for a new raw must not silently drop an older citation.
    let mut sources = Vec::new();
    if let Some(existing) = read_page(root, cfg, &target)? {
        sources = existing.sources.clone();
    }
    if !sources
        .iter()
        .any(|reference| reference_key(&cfg.paths.raw, reference) == raw_key)
    {
        // SCHEMA.md names sources as `.wiki/raw/*.md` paths, so write the
        // repository-relative path rather than the bare raw key.
        sources.push(format!(
            "{}/{}",
            cfg.paths.raw.trim_start_matches("./").trim_end_matches('/'),
            raw_key
        ));
    }

    let front = FrontMatter {
        id: id.clone(),
        title: doc.title,
        typ: typ.clone(),
        summary: doc.summary,
        tags: doc.tags,
        sources,
        status,
        updated: today(),
        extra: Vec::new(),
    };
    let rendered = page::render(&front, &doc.body);
    // Round-trip before anything lands: a payload the parser cannot read back is
    // rejected now, not discovered by `lint` later.
    page::parse(&rendered)?;

    let staging = staging_path(
        root,
        cfg,
        task.source_key.as_deref().unwrap_or_default(),
        &typ,
        &id,
    );
    if let Some(parent) = staging.parent() {
        std::fs::create_dir_all(parent).map_err(|source| TaskError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    render::write_atomic(&staging, &rendered)?;

    // Nothing outside this leaf's target may have changed since the root opened.
    let current = page_hashes(root, cfg)?;
    let baseline: BTreeMap<String, String> = store.baseline(GLOBAL_BASELINE)?.into_iter().collect();
    let diff = baseline_diff(&current, &baseline);
    if !diff.is_empty() {
        return Err(bad(
            "E_PAGE_CONFLICT",
            format!(
                "pages changed outside this task: {}; revert them or close the task after reviewing",
                diff.join(", ")
            ),
        ));
    }

    store.set_task_payload(
        task.id,
        &String::from_utf8_lossy(&bytes),
        Some(&format!("[\"{target}\"]")),
        &now(),
    )?;
    store.delete_task(task.id)?;
    Ok(())
}

fn close_breakdown(
    cfg: &Config,
    store: &mut Store,
    task: &TaskRow,
    leaves_path: &Path,
) -> Result<()> {
    let source_key = task
        .source_key
        .clone()
        .ok_or_else(|| bad("E_TASK_BAD_PAYLOAD", "breakdown task has no source"))?;
    let raw_key = work_item_raw_key(store, Some(&source_key))?;
    let bytes = std::fs::read(leaves_path).map_err(|source| TaskError::Io {
        path: leaves_path.to_path_buf(),
        source,
    })?;
    let doc: BreakdownDoc = serde_json::from_slice(&bytes).map_err(|err| {
        bad(
            "E_TASK_BAD_PAYLOAD",
            format!("leaf list is not valid JSON: {err}"),
        )
    })?;

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut planned: Vec<(String, String, String, String, String)> = Vec::new();
    for leaf in &doc.leaves {
        if !cfg.schema.types.iter().any(|known| known == &leaf.typ) {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!(
                    "type '{}' is not in [schema].types ({})",
                    leaf.typ,
                    cfg.schema.types.join(", ")
                ),
            ));
        }
        if !is_valid_id(&leaf.id) {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("'{}' is not a valid page id (kebab-case)", leaf.id),
            ));
        }
        let path = page_path(cfg, &leaf.typ, &leaf.id);
        if !seen.insert(path.clone()) {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("{path} appears more than once in the leaf list"),
            ));
        }
        planned.push((
            path,
            leaf.typ.clone(),
            leaf.id.clone(),
            leaf.title.clone(),
            leaf.brief.clone(),
        ));
    }

    // Every page that already cites this raw must be a leaf or carry a reason.
    let affected = affected_pages(store, std::slice::from_ref(&raw_key))?;
    let mut unchanged: BTreeMap<String, String> = BTreeMap::new();
    for item in &doc.unchanged {
        if item.reason.trim().is_empty() {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("{} is listed as unchanged without a reason", item.path),
            ));
        }
        unchanged.insert(item.path.clone(), item.reason.clone());
    }
    for key in &affected {
        if !seen.contains(key) && !unchanged.contains_key(key) {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!(
                    "{key} cites this source but has neither a leaf nor an explicit reason to stay unchanged"
                ),
            ));
        }
    }

    for (path, typ, id, title, brief) in planned {
        let mut new = NewTask::pending("page", &now());
        new.parent_id = Some(task.id);
        new.source_key = Some(source_key.clone());
        new.raw_hash = task.raw_hash.clone();
        new.target_path = Some(path.clone());
        new.allowed_json = Some(format!("[\"{path}\"]"));
        new.payload_json = Some(
            serde_json::to_string(&LeafNote {
                target_path: path,
                target_type: typ,
                target_id: id,
                title,
                brief,
                source_key: source_key.clone(),
            })
            .unwrap_or_default(),
        );
        store.insert_task(&new)?;
    }

    store.set_task_payload(task.id, &String::from_utf8_lossy(&bytes), None, &now())?;
    store.set_task_status(task.id, "active", &now())?;
    Ok(())
}

/// Merges one settled root: validate the batch, confirm nobody touched `pages/`,
/// write every staged page atomically, then clear the work item and the tree.
fn finish_root(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    task: &TaskRow,
    merged: &mut Vec<String>,
) -> Result<()> {
    let source_key = task
        .source_key
        .clone()
        .ok_or_else(|| bad("E_TASK_BAD_PAYLOAD", "root has no source"))?;
    let raw_key = work_item_raw_key(store, Some(&source_key))?;
    let staged = staged_pages(root, cfg, &source_key, &raw_key)?;

    let current = page_hashes(root, cfg)?;
    let baseline: BTreeMap<String, String> = store.baseline(GLOBAL_BASELINE)?.into_iter().collect();
    let diff = baseline_diff(&current, &baseline);
    if !diff.is_empty() {
        return Err(bad(
            "E_PAGE_CONFLICT",
            format!(
                "pages changed while this batch was being written: {}; the batch stays in staging",
                diff.join(", ")
            ),
        ));
    }

    for (key, bytes) in &staged {
        let path = root.join(key);
        render::write_atomic(&path, bytes)?;
        merged.push(key.clone());
    }
    query::refresh(root, cfg, store)?;

    let raw_hash = task.raw_hash.clone().unwrap_or_default();
    if !raw_key.is_empty() {
        // Same four-way hash check `done` used to run: a source that drifted
        // while the batch was being written keeps its queue entry, and the
        // root comes back to be re-broken down.
        crate::worklist::complete(root, cfg, store, &source_key, &raw_hash).map_err(|err| {
            bad(
                "E_SOURCE_DRIFT",
                format!("{err}; the batch stays in staging"),
            )
        })?;
    }
    store.delete_task(task.id)?;
    let dir = staging_root(root, cfg, &source_key);
    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

/// Every staged page of one root, validated and keyed by repository path.
fn staged_pages(
    root: &Path,
    cfg: &Config,
    source_key: &str,
    raw_key: &str,
) -> Result<Vec<(String, Vec<u8>)>> {
    let dir = staging_root(root, cfg, source_key);
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in walkdir::WalkDir::new(&dir).follow_links(false) {
        let entry = entry.map_err(|err| TaskError::Io {
            path: dir.clone(),
            source: std::io::Error::other(err.to_string()),
        })?;
        if !entry.file_type().is_file() || entry.path().extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let bytes = std::fs::read(entry.path()).map_err(|source| TaskError::Io {
            path: entry.path().to_path_buf(),
            source,
        })?;
        let (front, _) = page::parse(&bytes)?;
        let key = page_path(cfg, &front.typ, &front.id);
        if !cfg.schema.types.iter().any(|known| known == &front.typ) {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("{key} has a type outside [schema].types"),
            ));
        }
        if !front
            .sources
            .iter()
            .any(|reference| reference_key(&cfg.paths.raw, reference) == raw_key)
        {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("{key} does not cite this source"),
            ));
        }
        if front.title.trim().is_empty() {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("{key} has an empty title"),
            ));
        }
        out.push((key, bytes));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// The baseline every root shares.
///
/// One machine and one author means there is one "what was here when this
/// batch started" per repository, not one per root: when two trees overlap,
/// the pages merged by the first must not look like stray edits to the second.
const GLOBAL_BASELINE: i64 = 0;

/// Takes the first snapshot of `pages/` if the repository has none yet.
fn ensure_global_baseline(root: &Path, cfg: &Config, store: &mut Store) -> Result<()> {
    if store.baseline(GLOBAL_BASELINE)?.is_empty() {
        refresh_global_baseline(root, cfg, store)?;
    }
    Ok(())
}

/// Replaces the baseline with the current state of `pages/`.
fn refresh_global_baseline(root: &Path, cfg: &Config, store: &mut Store) -> Result<()> {
    store.clear_baseline(GLOBAL_BASELINE)?;
    for (key, digest) in page_hashes(root, cfg)? {
        store.set_baseline(GLOBAL_BASELINE, &key, &digest)?;
    }
    Ok(())
}

/// Every page under `pages/`, keyed by repository path, with its digest.
fn page_hashes(root: &Path, cfg: &Config) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root.join(&cfg.paths.wiki)).follow_links(false) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() || entry.path().extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let key = paths::identity_key(root, entry.path())?;
        // Audit notes live under pages/ but are bookkeeping, not authored
        // knowledge: every round appends to them, so they never belong in a
        // baseline comparison.
        if key.starts_with(&format!("{}/audit/", cfg.paths.wiki)) {
            continue;
        }
        // The index is rendered by the tool on every round; like the audit
        // notes it is derived state, not an authored page.
        if key == format!("{}/index.md", cfg.paths.wiki) {
            continue;
        }
        out.insert(key, hash::hash_file(entry.path(), true)?);
    }
    Ok(out)
}

/// Paths added, changed or removed since the baseline. Empty means untouched.
fn baseline_diff(
    current: &BTreeMap<String, String>,
    baseline: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (key, digest) in current {
        match baseline.get(key) {
            Some(old) if old == digest => {}
            Some(_) => out.push(format!("{key} (changed)")),
            None => out.push(format!("{key} (added)")),
        }
    }
    for key in baseline.keys() {
        if !current.contains_key(key) {
            out.push(format!("{key} (deleted)"));
        }
    }
    out.sort();
    out
}

fn read_page(root: &Path, _cfg: &Config, target: &str) -> Result<Option<FrontMatter>> {
    let path = root.join(target);
    if !path.exists() {
        return Ok(None);
    }
    let (front, _) = page::read(&path)?;
    Ok(Some(front))
}

fn work_item_raw_key(store: &Store, source_key: Option<&str>) -> Result<String> {
    let Some(source_key) = source_key else {
        return Ok(String::new());
    };
    Ok(store
        .work_items()?
        .into_iter()
        .find(|item| item.source_key == source_key)
        .map(|item| item.raw_key)
        .unwrap_or_default())
}

fn current_raw_hash(store: &Store, source_key: Option<&str>) -> Result<Option<String>> {
    let Some(source_key) = source_key else {
        return Ok(None);
    };
    Ok(store
        .work_items()?
        .into_iter()
        .find(|item| item.source_key == source_key)
        .map(|item| item.raw_hash))
}

/// The staging path for one page: `<engine>/cache/staging/<source>/<type>/<id>.md`.
fn staging_root(root: &Path, cfg: &Config, source_key: &str) -> PathBuf {
    root.join(&cfg.paths.engine)
        .join("cache")
        .join("staging")
        .join(source_key)
}

fn staging_path(root: &Path, cfg: &Config, source_key: &str, typ: &str, id: &str) -> PathBuf {
    staging_root(root, cfg, source_key)
        .join(typ)
        .join(format!("{id}.md"))
}

/// The repository path of one page.
fn page_path(cfg: &Config, typ: &str, id: &str) -> String {
    format!("{}/{typ}/{id}.md", cfg.paths.wiki.trim_end_matches('/'))
}

fn split_target(cfg: &Config, target: &str) -> Result<(String, String)> {
    let prefix = format!("{}/", cfg.paths.wiki.trim_end_matches('/'));
    let rest = target.strip_prefix(&prefix).ok_or_else(|| {
        bad(
            "E_TASK_BAD_PAYLOAD",
            format!("{target} is not under {}", cfg.paths.wiki),
        )
    })?;
    let (typ, file) = rest.split_once('/').ok_or_else(|| {
        bad(
            "E_TASK_BAD_PAYLOAD",
            format!("{target} must be <type>/<id>.md"),
        )
    })?;
    let id = file
        .strip_suffix(".md")
        .ok_or_else(|| bad("E_TASK_BAD_PAYLOAD", format!("{target} must end in .md")))?;
    Ok((typ.to_string(), id.to_string()))
}

fn parse_status(raw: Option<&str>) -> Result<Status> {
    match raw.unwrap_or("draft") {
        "draft" => Ok(Status::Draft),
        "stable" => Ok(Status::Stable),
        "stale" => Ok(Status::Stale),
        other => Err(bad(
            "E_TASK_BAD_PAYLOAD",
            format!("status '{other}' is not draft, stable or stale"),
        )),
    }
}

/// A page id is a kebab-case slug: lowercase letters, digits and single dashes.
fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('-')
        && !id.ends_with('-')
        && !id.contains("--")
        && id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

/// A `sources:` entry without its `#p=..` fragment, relative to the raw dir.
fn reference_key(raw_dir: &str, reference: &str) -> String {
    crate::source_refs::source_keys(&[reference.to_string()], raw_dir)
        .into_iter()
        .next()
        .unwrap_or_default()
}
