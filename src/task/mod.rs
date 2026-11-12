//! The ingest task tree.
//!
//! A source becomes one root task (`breakdown`) with a single `route` child. The
//! route child reads the source, searches the existing wiki and decides which
//! pages the source changes; closing it plans one `page` task per target. Each
//! `page` task is delivered on its own and its payload is written to
//! `.wiki/cache/staging/<source_key>/...`, never to `.wiki/pages/`. When the
//! last page closes, the root merges the whole batch in one step.
//!
//! The tree is the execution state of one ingest unit; it lives in SQLite
//! (`tasks`, `task_baseline`), so "no row" means "nothing left to do". Closing a
//! task deletes it; history goes to the audit page.
//!
//! Two rules hold the whole thing together:
//!
//! - **Route before writing.** No page task exists until a route has read the
//!   source and the index as they are now; planning against a stale index is
//!   how a source gets filed into the wrong page.
//! - **The tool owns the page structure.** `id`, `type`, `sources`, `updated`
//!   and the frontmatter syntax come from the task, not from the payload.

mod contract;
mod plan;
mod route;

/// The self-describing half of [`TaskView`], exposed so a caller can name the
/// type. The module itself stays private: only these five types leave `task`.
pub use contract::{Contract, Example, Field, FilledBy, Guide};

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

use plan::{PagePlan, RootPlan, Stage, WriteStatus};

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

/// What one leaf submits: content, never structure.
#[derive(Debug, Deserialize)]
pub(crate) struct LeafPayload {
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

/// One root and its leaves, for `task status`.
#[derive(Debug, Serialize)]
pub struct RootView {
    pub id: i64,
    pub source_key: String,
    pub status: String,
    pub attempts: i64,
    pub last_error: Option<String>,
    /// The root plan's stage, or `None` when the plan cannot be read.
    pub stage: Option<String>,
    /// Leaves still pending or active: work this root is not done with.
    pub open_children: usize,
    pub leaves: Vec<TaskRow>,
}

/// The task tree's current shape.
#[derive(Debug, Serialize)]
pub struct TaskStatus {
    pub roots: Vec<RootView>,
    pub current: Option<TaskView>,
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
    /// Which step of the source's life this task belongs to: `routing`,
    /// `writing` or `reconcile`.
    pub stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<TaskSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<TaskTarget>,
    pub affected_pages: Vec<String>,
    pub submit: SubmitView,
    pub guide: Guide,
    pub contract: Contract,
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
    /// The page hash the route froze before this task existed; absent when the
    /// route creates the page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
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
    let (target_title, target_brief, target_before) = match page_plan(task) {
        Some(plan) => (
            Some(plan.target.title),
            Some(plan.target.change),
            plan.target.before,
        ),
        None => (None, None, None),
    };
    let target = task.target_path.as_ref().map(|reference| TaskTarget {
        reference: reference.clone(),
        exists: root.join(reference).is_file(),
        title: target_title,
        brief: target_brief,
        before: target_before,
    });
    let affected_pages =
        if matches!(task.kind.as_str(), "breakdown" | "route") && !raw_key.is_empty() {
            affected_pages(store, std::slice::from_ref(&raw_key))?
        } else {
            Vec::new()
        };
    let (stage, suffix, schema) = match task.kind.as_str() {
        "page" => (Some("writing"), "--payload <FILE>", "page-content-v1"),
        "route" => (Some("routing"), "--payload <FILE>", "route-v1"),
        "breakdown" => (None, "--leaves <FILE>", "leaf-list-v1"),
        "reconcile" => (Some("reconcile"), "--accept | --regenerate", "reconcile-v1"),
        _ => (None, "", ""),
    };
    Ok(TaskView {
        id: task.id,
        kind: task.kind.clone(),
        status: task.status.clone(),
        stage: stage.map(String::from),
        source,
        target,
        affected_pages,
        submit: SubmitView {
            command: format!("llm-wiki task done {} {suffix}", task.id),
            schema: String::from(schema),
        },
        guide: contract::guide(&task.kind),
        contract: contract::contract(schema, &cfg.schema.types),
    })
}

/// The page's own plan, when this is a page task and the plan still parses.
fn page_plan(task: &TaskRow) -> Option<PagePlan> {
    if task.kind != "page" {
        return None;
    }
    PagePlan::parse(task).ok()
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
    new.payload_json = Some(RootPlan::new_routing(&item.raw_key, &item.raw_hash).json()?);
    let mut route = NewTask::pending("route", stamp);
    route.source_key = Some(item.source_key.clone());
    route.allowed_json = Some("route-v1".to_string());
    store.create_root_with_route(&new, &route)?;
    Ok(true)
}

/// Delivers the one task the next round should run, or `None` when the tree is
/// settled.
///
/// Order: finish any root whose pages are all closed → a pending page → a
/// pending review → a pending route. An `active` task is reset to `pending`
/// first: one machine, one agent, so an `active` row is a delivery from a
/// previous round, not a lease.
pub fn advance(root: &Path, cfg: &Config, store: &mut Store) -> Result<Advance> {
    let mut out = Advance::default();
    let stamp = now();
    reconcile_scan(root, cfg, store)?;
    // Later steps read relations (affected pages, page quality); one refresh
    // here keeps every caller of `advance` on the same snapshot.
    query::refresh(root, cfg, store)?;
    for task in store.tasks()? {
        if matches!(task.kind.as_str(), "page" | "reconcile" | "route") && task.status == "active" {
            store.set_task_status(task.id, "pending", &stamp)?;
        }
    }

    loop {
        let tasks = store.tasks()?;

        // 1. A root whose pages are gone settles here, not handed out.
        if let Some(root_task) = tasks.iter().find(|t| owes_settlement(&tasks, t)) {
            let before = out.merged_pages.len();
            let source_key = root_task.source_key.clone().unwrap_or_default();
            match finish_root(root, cfg, store, root_task) {
                Ok(settled) => {
                    out.merged_pages.extend(settled.merged_pages);
                    out.confirmed_sources.extend(settled.confirmed_sources);
                    out.issues.extend(settled.issues);
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
                    continue;
                }
                Err(err) => {
                    let message = format!("{}: {err}", err.code());
                    out.issues
                        .push(format!("source {source_key} could not settle: {message}"));
                    // `finish_root` parks a page conflict or a drift on the
                    // root itself. Failing it here would overwrite `blocked`
                    // and hide the replan affordance.
                    let blocked = store
                        .task(root_task.id)?
                        .is_some_and(|row| row.status == "blocked");
                    if !blocked {
                        store.fail_task(root_task.id, &message, &stamp)?;
                    }
                    continue;
                }
            }
        }

        // 2. Pages before anything else: the route already froze the targets.
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

        // 4. Only now, the earliest root waiting for its route. A route under a
        //    root that is blocked or failed is not delivered: the root's plan
        //    is stale until a replan settles it.
        if let Some(task) = tasks
            .iter()
            .filter(|t| t.kind == "route" && t.status == "pending")
            .filter(|t| route_parent_is_open(&tasks, t))
            .min_by_key(|t| t.id)
        {
            if let Some(parent) = task.parent_id {
                store.set_task_status(parent, "active", &stamp)?;
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
/// A `route` needs `payload` (the route document) and settles the source; a
/// `page` needs `payload` (the page content) and must still match the snapshot
/// the route took of its own target; a `reconcile` needs exactly one of `accept`
/// or `regenerate`. `--leaves` no longer belongs to any task: breakdowns are
/// gone.
pub fn close(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    id: i64,
    payload: Option<&Path>,
    leaves: Option<&Path>,
    verdict: Option<Verdict>,
) -> Result<Advance> {
    if leaves.is_some() {
        return Err(bad(
            "E_TASK_BAD_PAYLOAD",
            "no task takes --leaves any more; close the route with --payload <file>",
        ));
    }
    let task = store.task(id)?.ok_or(TaskError::Unknown(id))?;
    match task.kind.as_str() {
        "route" if verdict.is_some() => {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                "only a reconcile task takes --accept or --regenerate",
            ))
        }
        "route" => {
            let payload = payload.ok_or_else(|| {
                bad(
                    "E_TASK_BAD_PAYLOAD",
                    "a route task needs --payload <file> with the route document",
                )
            })?;
            return route::close_route(root, cfg, store, &task, payload);
        }
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

/// Rebuilds a root's route without reverting pages the last round already
/// merged.
///
/// A root can stop with stale task rows in three ways: a crash after a page
/// landed but before its baseline, a drift that kept the queue entry, or a page
/// conflict. The first pass makes disk and baseline agree again; only then is
/// the old tree discarded. A target that is neither at its planned `before`
/// bytes nor at the bytes this root wrote is someone else's edit: the root is
/// blocked and nothing is deleted.
pub fn replan(root: &Path, cfg: &Config, store: &mut Store, id: i64) -> Result<()> {
    let task = store.task(id)?.ok_or(TaskError::Unknown(id))?;
    // Only a root has a plan to rebuild. A page or route id is not a task a
    // human can replan, and `E_NO_TASK` is the same answer `retry` gives.
    if task.kind != "breakdown" {
        return Err(TaskError::Unknown(id));
    }
    let source_key = task.source_key.clone().ok_or_else(|| {
        bad(
            "E_TASK_BAD_PAYLOAD",
            format!("root {} has no source", task.id),
        )
    })?;
    let plan = RootPlan::parse(&task)?;

    // Route the raw that is pending now, not the one the old plan froze. The
    // queue entry must still agree with the source and the raw file; otherwise
    // `ingest` has to convert the source again before a replan is meaningful.
    let item = store
        .work_items()?
        .into_iter()
        .find(|item| item.source_key == source_key)
        .ok_or_else(|| {
            bad(
                "E_SOURCE_DRIFT",
                format!(
                    "source {source_key} has no pending raw; run `ingest` to convert it before replanning"
                ),
            )
        })?;
    crate::worklist::verify(root, cfg, store, &source_key, &item.raw_hash).map_err(|err| {
        bad(
            "E_SOURCE_DRIFT",
            format!("{err}; run `ingest` to convert the source again before replanning"),
        )
    })?;

    // Recovery is a dry pass: a third hash on any target blocks the root before
    // a single baseline moves, so putting the page back lets the same replan
    // run again.
    let mut baselines = Vec::new();
    for target in &plan.targets {
        let current = current_target_hash(root, &target.path)?;
        let expected = target.expected.as_deref();
        if target.status == WriteStatus::Written
            || expected.is_some_and(|expected| current.as_deref() == Some(expected))
        {
            if let Some(digest) = expected.or(current.as_deref()) {
                baselines.push((target.path.clone(), digest.to_string()));
            }
            continue;
        }
        let unchanged = match (&target.before, &current) {
            (Some(before), Some(current_digest)) => current_digest == before,
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            continue;
        }
        let message = format!(
            "{} hashes to {} now, but this root planned {}; a replan does not overwrite another edit and the root is blocked",
            target.path,
            current.as_deref().unwrap_or("<missing>"),
            target
                .before
                .as_deref()
                .unwrap_or("<the page did not exist>"),
        );
        store.block_task(id, &format!("E_PAGE_CONFLICT: {message}"), &now())?;
        return Err(bad("E_PAGE_CONFLICT", message));
    }
    for (path, digest) in baselines {
        store.set_baseline(GLOBAL_BASELINE, &path, &digest)?;
    }

    // The old children and this round's staging are both discarded; the pages
    // themselves are not touched. `replan_root` drops the children and inserts
    // the fresh route in one transaction.
    let fresh = RootPlan::new_routing(&item.raw_key, &item.raw_hash);
    let stamp = now();
    let mut route = NewTask::pending("route", &stamp);
    route.source_key = Some(source_key.clone());
    route.allowed_json = Some("route-v1".to_string());
    let _ = std::fs::remove_dir_all(staging_root(root, cfg, &source_key));
    store.replan_root(id, &fresh.json()?, &route, &stamp)?;
    Ok(())
}

/// Reads the tree without advancing it.
///
/// The selection order is `advance`'s: a page, then a reconcile, then a route.
/// A root the tool still owes a merge outranks all of them, so `current` is
/// `None` while that settle is pending. This only reads: no row is set active
/// and no baseline moves.
pub fn status(root: &Path, cfg: &Config, store: &mut Store) -> Result<TaskStatus> {
    let tasks = store.tasks()?;
    let mut roots = Vec::new();
    for task in tasks.iter().filter(|t| t.kind == "breakdown") {
        roots.push(RootView {
            id: task.id,
            source_key: task.source_key.clone().unwrap_or_default(),
            status: task.status.clone(),
            attempts: task.attempts,
            last_error: task.last_error.clone(),
            stage: RootPlan::parse(task)
                .ok()
                .map(|plan| stage_name(&plan.stage).to_string()),
            open_children: tasks
                .iter()
                .filter(|child| {
                    child.parent_id == Some(task.id)
                        && matches!(child.status.as_str(), "pending" | "active")
                })
                .count(),
            leaves: tasks
                .iter()
                .filter(|child| child.parent_id == Some(task.id))
                .cloned()
                .collect(),
        });
    }
    let current = if tasks.iter().any(|task| owes_settlement(&tasks, task)) {
        None
    } else {
        pick_task(&tasks, "page")
            .or_else(|| pick_task(&tasks, "reconcile"))
            .or_else(|| pick_route(&tasks))
    };
    let current = match current {
        Some(task) => Some(view(root, cfg, store, task)?),
        None => None,
    };
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

/// The earliest pending or active task of one kind.
///
/// A delivered task stays `active` until it is closed, so status must count
/// both; only `advance` gets to reset the row to `pending`.
fn pick_task<'a>(tasks: &'a [TaskRow], kind: &str) -> Option<&'a TaskRow> {
    tasks
        .iter()
        .filter(|t| t.kind == kind && matches!(t.status.as_str(), "pending" | "active"))
        .min_by_key(|t| t.id)
}

/// A route is only as good as the root it belongs to: a blocked or failed root
/// has a stale plan, so its route must not be handed to an agent.
fn route_parent_is_open(tasks: &[TaskRow], route: &TaskRow) -> bool {
    route
        .parent_id
        .and_then(|id| tasks.iter().find(|task| task.id == id))
        .is_some_and(|parent| !matches!(parent.status.as_str(), "blocked" | "failed"))
}

fn pick_route(tasks: &[TaskRow]) -> Option<&TaskRow> {
    tasks
        .iter()
        .filter(|t| t.kind == "route" && matches!(t.status.as_str(), "pending" | "active"))
        .filter(|t| route_parent_is_open(tasks, t))
        .min_by_key(|t| t.id)
}

/// Whether the tool, not the agent, owes this root the next step.
///
/// One predicate for both surfaces: `advance` settles the root first and
/// `status` refuses to call it current, so the two cannot disagree about what
/// comes next. A pending root counts too: an ordinary retry parks a failed or
/// blocked settle as pending, and the next round must finish it.
fn owes_settlement(tasks: &[TaskRow], task: &TaskRow) -> bool {
    task.kind == "breakdown"
        && matches!(task.status.as_str(), "active" | "pending")
        && !tasks.iter().any(|child| child.parent_id == Some(task.id))
        && RootPlan::parse(task)
            .map(|plan| matches!(plan.stage, Stage::Writing | Stage::Settling))
            .unwrap_or(false)
}

fn stage_name(stage: &Stage) -> &'static str {
    match stage {
        Stage::Routing => "routing",
        Stage::Writing => "writing",
        Stage::Settling => "settling",
    }
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
            let source_key = raw.source_key.clone();
            // A root that is still open keeps its identity: replan it rather
            // than creating a second tree for the same source. The replan path
            // preserves every page that already landed and discards only the
            // stale plan. If it fails, this reconcile task is not deleted.
            let existing = store.tasks()?.into_iter().find(|task| {
                task.kind == "breakdown" && task.source_key.as_deref() == Some(source_key.as_str())
            });
            if let Some(root_task) = existing {
                replan(root, cfg, store, root_task.id)?;
            } else {
                let item = WorkItem {
                    source_key,
                    raw_key: raw.key.clone(),
                    raw_hash: raw.hash.clone(),
                };
                Ledger::new(store).import_work_item(&item)?;
                ensure_root(root, cfg, store, &item, &now())?;
            }
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
    let plan = PagePlan::parse(task)?;
    let target = plan.target.path.clone();
    let (typ, id) = split_target(cfg, &target)?;
    let raw_key = plan.raw_key.clone();
    let source_key = task.source_key.clone().ok_or_else(|| {
        bad(
            "E_TASK_BAD_PAYLOAD",
            format!("page task {} has no source", task.id),
        )
    })?;

    // The source the page was planned against must still be the one on disk:
    // nothing is rendered or staged until the raw and its ledger rows agree.
    crate::worklist::verify(root, cfg, store, &source_key, &plan.raw_hash)
        .map_err(|err| bad("E_SOURCE_DRIFT", format!("{err}; the page was not written")))?;

    // Check the target's own snapshot before touching staging. A page edited
    // after routing is not overwritten; a page created after routing is not
    // clobbered. Other pages are `reconcile_scan`'s business, not this task's.
    let full = root.join(&target);
    match plan.target.before.as_deref() {
        Some(before) => {
            let current = match hash::hash_file(&full, true) {
                Ok(digest) => digest,
                Err(hash::HashError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    return Err(bad(
                        "E_PAGE_CONFLICT",
                        format!(
                            "{target} disappeared after this task was planned; reroute the source or restore it"
                        ),
                    ));
                }
                Err(err) => return Err(TaskError::Hash(err)),
            };
            if current != before {
                return Err(bad(
                    "E_PAGE_CONFLICT",
                    format!(
                        "{target} changed after this task was planned; reroute the source or restore it"
                    ),
                ));
            }
        }
        None => {
            if full.exists() {
                return Err(bad(
                    "E_PAGE_CONFLICT",
                    format!(
                        "{target} was created after this task was planned; reroute the source or remove it"
                    ),
                ));
            }
        }
    }

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
    // A whitespace-only body renders a page with no content: reject it before
    // anything reaches staging so the task can be resubmitted with real text.
    if doc.body.trim().is_empty() {
        return Err(bad(
            "E_TASK_BAD_PAYLOAD",
            "payload body is empty; add the page content and resubmit (whitespace is not content)",
        ));
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

    let root_task = store
        .task(plan.root)?
        .ok_or(TaskError::Unknown(plan.root))?;
    let mut root_plan = RootPlan::parse(&root_task)?;
    let slot = root_plan
        .targets
        .iter_mut()
        .find(|entry| entry.path == target)
        .ok_or_else(|| {
            bad(
                "E_TASK_BAD_PAYLOAD",
                format!("root plan has no target for {target}"),
            )
        })?;
    slot.status = WriteStatus::Staged;
    slot.expected = Some(hash::hash_bytes(&rendered));

    // Stage only after the root plan accepted this target: a page task that
    // cannot be recorded leaves no orphan bytes behind.
    let staging = staging_path(root, cfg, &source_key, &typ, &id);
    if let Some(parent) = staging.parent() {
        std::fs::create_dir_all(parent).map_err(|source| TaskError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    render::write_atomic(&staging, &rendered)?;

    store.commit_page(plan.root, task.id, &root_plan.json()?, &now())?;
    Ok(())
}

/// Merges one settled root: write every staged page atomically, record its
/// expected hash in the baseline, then clear the work item and the tree.
///
/// Settlement is per target, not per batch: a page already on disk with the
/// expected bytes only gets its baseline, a page still at `before` is merged,
/// and any third hash blocks the root instead of overwriting an edit. The plan
/// is saved after every page, so a crash resumes at the next target.
fn finish_root(root: &Path, cfg: &Config, store: &mut Store, task: &TaskRow) -> Result<Advance> {
    let mut out = Advance::default();
    let source_key = task
        .source_key
        .clone()
        .ok_or_else(|| bad("E_TASK_BAD_PAYLOAD", "root has no source"))?;
    let mut plan = RootPlan::parse(task)?;

    // Persist the stage before touching a page: a crash after any merge is
    // recovered as a settle, not as a root still waiting for page tasks.
    let stamp = now();
    plan.stage = Stage::Settling;
    store.set_task_payload(task.id, &plan.json()?, None, &stamp)?;

    for index in 0..plan.targets.len() {
        let target = &mut plan.targets[index];
        match target.status {
            WriteStatus::Written => {}
            WriteStatus::Staged => {
                let expected = target.expected.clone().ok_or_else(|| {
                    bad(
                        "E_TASK_BAD_PAYLOAD",
                        format!("{} is staged without an expected hash", target.path),
                    )
                })?;
                let (typ, id) = split_target(cfg, &target.path)?;
                let staging = staging_path(root, cfg, &source_key, &typ, &id);
                let bytes = match std::fs::read(&staging) {
                    Ok(bytes) => bytes,
                    Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                        let message = format!(
                            "staging file {} for {} is missing; this page was not written and the root is blocked",
                            staging.display(),
                            target.path
                        );
                        store.block_task(
                            task.id,
                            &format!("E_PAGE_CONFLICT: {message}"),
                            &now(),
                        )?;
                        return Err(bad("E_PAGE_CONFLICT", message));
                    }
                    Err(source) => {
                        return Err(TaskError::Io {
                            path: staging,
                            source,
                        })
                    }
                };

                // The staged bytes must be the bytes the page task recorded;
                // otherwise the baseline would name content that never landed.
                let staged_hash = hash::hash_bytes(&bytes);
                if staged_hash != expected {
                    let message = format!(
                        "staging file {} for {} hashes to {staged_hash}, expected {expected}; this page was not written and the root is blocked",
                        staging.display(),
                        target.path
                    );
                    store.block_task(task.id, &format!("E_PAGE_CONFLICT: {message}"), &now())?;
                    return Err(bad("E_PAGE_CONFLICT", message));
                }

                let full = root.join(&target.path);
                let current = match hash::hash_file(&full, true) {
                    Ok(digest) => Some(digest),
                    Err(hash::HashError::Io { source, .. })
                        if source.kind() == std::io::ErrorKind::NotFound =>
                    {
                        None
                    }
                    Err(err) => return Err(TaskError::Hash(err)),
                };

                if current.as_deref() == Some(expected.as_str()) {
                    // A previous run already merged this page (the plan save or
                    // the root delete did not happen): only the baseline is owed.
                    store.set_baseline(GLOBAL_BASELINE, &target.path, &expected)?;
                } else {
                    let unchanged = match (&target.before, &current) {
                        (Some(before), Some(now)) => now == before,
                        (None, None) => true,
                        _ => false,
                    };
                    if !unchanged {
                        let message = format!(
                            "{} hashes to {} now, but this root planned {} and staged {expected}; this page was not written and the root is blocked",
                            target.path,
                            current.as_deref().unwrap_or("<missing>"),
                            target
                                .before
                                .as_deref()
                                .unwrap_or("<the page did not exist>"),
                        );
                        store.block_task(
                            task.id,
                            &format!("E_PAGE_CONFLICT: {message}"),
                            &now(),
                        )?;
                        return Err(bad("E_PAGE_CONFLICT", message));
                    }
                    // Only bytes that survive a parse may land. An already
                    // written page skips this: it is not re-parsed because it
                    // is not written again.
                    if let Err(err) = page::parse(&bytes) {
                        let message = format!(
                            "staging file {} for {} cannot be parsed ({err}); this page was not written and the root is blocked",
                            staging.display(),
                            target.path
                        );
                        store.block_task(
                            task.id,
                            &format!("E_PAGE_CONFLICT: {message}"),
                            &now(),
                        )?;
                        return Err(bad("E_PAGE_CONFLICT", message));
                    }
                    render::write_atomic(&full, &bytes)?;
                    store.set_baseline(GLOBAL_BASELINE, &target.path, &expected)?;
                    out.merged_pages.push(target.path.clone());
                }
                target.status = WriteStatus::Written;
            }
            WriteStatus::Planned => {
                return Err(bad(
                    "E_TASK_BAD_PAYLOAD",
                    format!("{} was planned but never staged", target.path),
                ))
            }
        }
        // Save after every page: a crash between two targets resumes at the
        // second one, and never re-writes the first.
        store.set_task_payload(task.id, &plan.json()?, None, &now())?;
    }

    query::refresh(root, cfg, store)?;

    if !plan.raw_key.is_empty() {
        // The same four-way hash check the route ran. Pages already merged and
        // baselines already advanced are not rolled back; only the queue entry
        // is kept, and the root is blocked so a replan, not a blind retry, is
        // the way back.
        if let Err(err) = crate::worklist::complete(root, cfg, store, &source_key, &plan.raw_hash) {
            let message = format!(
                "{err}; merged pages and advanced baselines are not rolled back; when the source and raw settle again, run `task retry {} --replan`",
                task.id
            );
            store.block_task(task.id, &format!("E_SOURCE_DRIFT: {message}"), &now())?;
            return Err(bad("E_SOURCE_DRIFT", message));
        }
    }
    store.delete_task(task.id)?;
    let _ = std::fs::remove_dir_all(staging_root(root, cfg, &source_key));
    out.confirmed_sources.push(source_key);
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

fn read_page(root: &Path, _cfg: &Config, target: &str) -> Result<Option<FrontMatter>> {
    let path = root.join(target);
    if !path.exists() {
        return Ok(None);
    }
    let (front, _) = page::read(&path)?;
    Ok(Some(front))
}

fn current_target_hash(root: &Path, target: &str) -> Result<Option<String>> {
    match hash::hash_file(&root.join(target), true) {
        Ok(digest) => Ok(Some(digest)),
        Err(hash::HashError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(err) => Err(TaskError::Hash(err)),
    }
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

/// A `sources:` entry without its `#p=..` fragment, relative to the raw dir.
fn reference_key(raw_dir: &str, reference: &str) -> String {
    crate::source_refs::source_keys(&[reference.to_string()], raw_dir)
        .into_iter()
        .next()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::plan::{Origin, RootPlan, TargetPlan, WriteStatus};
    use super::{
        finish_root, replan, retry, staging_path, staging_root, Advance, Verdict, GLOBAL_BASELINE,
    };
    use crate::config::Config;
    use crate::hash;
    use crate::store::{NewTask, Store};
    use crate::sync::{self, SyncArgs};

    fn test_page(id: &str, title: &str) -> String {
        format!(
            "---\nid: {id}\ntitle: {title}\ntype: concept\nsummary: s\ntags: []\n\
             sources: []\nstatus: draft\nupdated: 2026-01-01\n---\n\n{title} body\n"
        )
    }

    /// One repository with a converted source and a root whose one target is
    /// already staged, ready for settlement.
    ///
    /// The target page is *not* written: a test writes it (or does not) to
    /// choose between the settled, conflicting and pending branches.
    struct Fixture {
        store: Store,
        dir: TempDir,
        cfg: Config,
        root_id: i64,
        source_key: String,
        target: String,
    }

    impl Fixture {
        fn settling(target: &str, before: Option<&str>, staged: &str) -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            crate::init::run(dir.path(), false).unwrap();
            std::fs::write(dir.path().join("a.md"), "# A\n\nEvidence A\n").unwrap();
            let cfg = Config::load(dir.path()).unwrap();
            let db = dir.path().join(&cfg.paths.engine).join("state.db");
            let mut store = Store::open(&db).unwrap();
            store.migrate().unwrap();
            sync::run(
                dir.path(),
                &cfg,
                &mut store,
                &SyncArgs {
                    limit: cfg.ingest.max_per_run,
                    dry_run: false,
                    force: true,
                    only: Vec::new(),
                },
            )
            .unwrap();

            let root = store
                .tasks()
                .unwrap()
                .into_iter()
                .find(|task| task.kind == "breakdown")
                .expect("ingest creates one root");
            let source_key = root.source_key.clone().unwrap();
            let mut plan = RootPlan::parse(&root).unwrap();
            plan.stage = super::Stage::Settling;
            plan.source_key = source_key.clone();
            plan.targets = vec![TargetPlan {
                path: target.to_string(),
                title: "A".to_string(),
                origin: if before.is_some() {
                    Origin::Existing
                } else {
                    Origin::Created
                },
                before: before.map(|bytes| hash::hash_bytes(bytes.as_bytes())),
                expected: Some(hash::hash_bytes(staged.as_bytes())),
                status: WriteStatus::Staged,
                points: Vec::new(),
                change: "test".to_string(),
            }];
            store
                .set_task_payload(root.id, &plan.json().unwrap(), None, &super::now())
                .unwrap();
            store
                .set_task_status(root.id, "active", &super::now())
                .unwrap();

            let fixture = Fixture {
                store,
                dir,
                cfg,
                root_id: root.id,
                source_key,
                target: target.to_string(),
            };
            fixture.write_staging(staged);
            fixture
        }

        /// A blocked root whose first target already landed and whose second
        /// target is only staged: exactly the state a replan must recover.
        fn partially_written() -> Fixture {
            let page_a = test_page("a", "A");
            let page_b = test_page("b", "B");
            let b_target = ".wiki/pages/concept/b.md";
            let mut fixture = Fixture::settling(".wiki/pages/concept/a.md", None, &page_a);
            fixture.write_target(&page_a);

            let root = fixture.store.task(fixture.root_id).unwrap().unwrap();
            let mut plan = RootPlan::parse(&root).unwrap();
            plan.targets[0].status = WriteStatus::Written;
            plan.targets.push(TargetPlan {
                path: b_target.to_string(),
                title: "B".to_string(),
                origin: Origin::Created,
                before: None,
                expected: Some(hash::hash_bytes(page_b.as_bytes())),
                status: WriteStatus::Staged,
                points: Vec::new(),
                change: "test".to_string(),
            });
            fixture
                .store
                .set_task_payload(fixture.root_id, &plan.json().unwrap(), None, &super::now())
                .unwrap();
            fixture.write_staging_at(b_target, &page_b);
            fixture
                .store
                .block_task(fixture.root_id, "E_PAGE_CONFLICT: test", &super::now())
                .unwrap();
            fixture
        }

        fn staging(&self) -> PathBuf {
            self.staging_for(&self.target)
        }

        fn staging_for(&self, target: &str) -> PathBuf {
            let (typ, id) = super::split_target(&self.cfg, target).unwrap();
            staging_path(self.dir.path(), &self.cfg, &self.source_key, &typ, &id)
        }

        fn staging_root(&self) -> PathBuf {
            staging_root(self.dir.path(), &self.cfg, &self.source_key)
        }

        fn write_target(&self, text: &str) {
            let path = self.dir.path().join(&self.target);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        fn write_staging(&self, text: &str) {
            self.write_staging_at(&self.target, text);
        }

        fn write_staging_at(&self, target: &str, text: &str) {
            let path = self.staging_for(target);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        fn target_bytes(&self) -> Vec<u8> {
            std::fs::read(self.dir.path().join(&self.target)).unwrap()
        }

        fn target_bytes_or_missing(&self) -> Option<Vec<u8>> {
            std::fs::read(self.dir.path().join(&self.target)).ok()
        }

        fn baseline(&self, path: &str) -> Option<String> {
            self.store
                .baseline(GLOBAL_BASELINE)
                .unwrap()
                .into_iter()
                .find(|(key, _)| key == path)
                .map(|(_, digest)| digest)
        }

        fn root_is_gone(&self) -> bool {
            self.store.task(self.root_id).unwrap().is_none()
        }

        fn root_is_blocked(&self) -> bool {
            self.store
                .task(self.root_id)
                .unwrap()
                .is_some_and(|task| task.status == "blocked")
        }

        fn staging_exists(&self) -> bool {
            self.staging().exists()
        }

        fn advance(&mut self) -> super::Result<Advance> {
            let task = self.store.task(self.root_id).unwrap().expect("root exists");
            finish_root(self.dir.path(), &self.cfg, &mut self.store, &task)
        }

        /// Leaves the root as `advance` would find it: owed a settlement and
        /// with no child task to deliver first.
        fn settle_directly(&mut self) {
            let children: Vec<i64> = self
                .store
                .tasks()
                .unwrap()
                .into_iter()
                .filter(|task| task.parent_id == Some(self.root_id))
                .map(|task| task.id)
                .collect();
            for id in children {
                self.store.delete_task(id).unwrap();
            }
        }
    }

    #[test]
    fn an_already_written_page_is_not_written_twice() {
        let mut f = Fixture::settling(".wiki/pages/concept/a.md", Some("before"), "expected");
        f.write_target("expected");
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(f.dir.path().join(&f.target))
                .unwrap()
                .ino()
        };
        f.advance().unwrap();
        assert_eq!(f.target_bytes(), b"expected");
        #[cfg(unix)]
        assert_eq!(
            {
                use std::os::unix::fs::MetadataExt;
                std::fs::metadata(f.dir.path().join(&f.target))
                    .unwrap()
                    .ino()
            },
            inode,
            "a page that already has the expected bytes is not rewritten"
        );
        assert_eq!(
            f.baseline(".wiki/pages/concept/a.md"),
            Some(hash::hash_bytes(b"expected"))
        );
        assert!(f.root_is_gone());
    }

    #[test]
    fn a_third_hash_blocks_the_root_and_keeps_staging() {
        let mut f = Fixture::settling(".wiki/pages/concept/a.md", Some("before"), "expected");
        f.write_target("third");
        let err = f.advance().unwrap_err();
        assert_eq!(err.code(), "E_PAGE_CONFLICT");
        assert_eq!(f.target_bytes(), b"third");
        assert_eq!(f.baseline(".wiki/pages/concept/a.md"), None);
        assert!(f.staging_exists());
        assert!(f.root_is_blocked());
    }

    #[test]
    fn a_tampered_staging_file_blocks_instead_of_merging() {
        let mut f = Fixture::settling(".wiki/pages/concept/a.md", Some("before"), "expected");
        f.write_staging("tampered");
        f.advance().unwrap_err();
        assert_eq!(f.target_bytes_or_missing(), None);
        assert_eq!(f.baseline(".wiki/pages/concept/a.md"), None);
        assert!(f.root_is_blocked());
    }

    #[test]
    fn advance_does_not_fail_a_root_that_settlement_blocked() {
        let mut f = Fixture::settling(".wiki/pages/concept/a.md", Some("before"), "expected");
        f.write_target("third");
        // Reach settlement through `advance` itself, which used to rewrite the
        // root's `blocked` status to `failed` and erase the replan affordance.
        f.settle_directly();
        super::advance(f.dir.path(), &f.cfg, &mut f.store).unwrap();
        assert!(f.root_is_blocked());
        let row = f.store.task(f.root_id).unwrap().expect("root remains");
        assert!(
            row.last_error
                .as_deref()
                .unwrap_or_default()
                .starts_with("E_PAGE_CONFLICT"),
            "unexpected last_error: {:?}",
            row.last_error
        );
        assert!(f.staging_exists());
    }

    #[test]
    fn a_staging_file_that_cannot_be_parsed_blocks_without_writing() {
        let mut f = Fixture::settling(".wiki/pages/concept/a.md", Some("before"), "not a page");
        f.write_target("before");
        let err = f.advance().unwrap_err();
        assert_eq!(err.code(), "E_PAGE_CONFLICT");
        assert_eq!(f.target_bytes(), b"before");
        assert_eq!(f.baseline(".wiki/pages/concept/a.md"), None);
        assert!(f.staging_exists());
        assert!(f.root_is_blocked());
    }

    #[test]
    fn replan_keeps_written_pages_and_rebuilds_the_route() {
        let mut f = Fixture::partially_written();
        let written = f.target_bytes();
        assert_eq!(f.baseline(&f.target), None);
        let item = f
            .store
            .work_items()
            .unwrap()
            .into_iter()
            .find(|item| item.source_key == f.source_key)
            .expect("the source still has a pending raw");

        replan(f.dir.path(), &f.cfg, &mut f.store, f.root_id).unwrap();

        assert_eq!(f.target_bytes(), written, "a written page is not reverted");
        assert_eq!(
            f.baseline(&f.target),
            Some(hash::hash_bytes(&written)),
            "recovery records the baseline the crash missed"
        );
        let tasks = f.store.tasks().unwrap();
        let kinds: Vec<&str> = tasks.iter().map(|task| task.kind.as_str()).collect();
        assert_eq!(kinds, ["breakdown", "route"]);
        assert_eq!(tasks[0].status, "active");
        assert_eq!(tasks[0].last_error, None);
        let plan = RootPlan::parse(&tasks[0]).unwrap();
        assert_eq!(plan.stage, super::Stage::Routing);
        assert!(plan.targets.is_empty());
        assert_eq!(plan.raw_key, item.raw_key);
        assert_eq!(plan.raw_hash, item.raw_hash);
        assert!(
            !f.staging_root().exists()
                || std::fs::read_dir(f.staging_root())
                    .unwrap()
                    .next()
                    .is_none(),
            "the old round's staging is discarded"
        );
    }

    #[test]
    fn replan_refuses_a_non_root_task() {
        let mut f = Fixture::partially_written();
        let mut page = NewTask::pending("page", &super::now());
        page.parent_id = Some(f.root_id);
        let page_id = f.store.insert_task(&page).unwrap();
        let err = replan(f.dir.path(), &f.cfg, &mut f.store, page_id).unwrap_err();
        assert_eq!(err.code(), "E_NO_TASK");
    }

    #[test]
    fn a_pending_root_settles_after_an_ordinary_retry() {
        let page = test_page("a", "A");
        let mut f = Fixture::settling(".wiki/pages/concept/a.md", Some("before"), &page);
        f.settle_directly();
        f.write_target("third");
        f.advance().unwrap_err();
        assert!(f.root_is_blocked());

        f.write_target("before");
        retry(&mut f.store, f.root_id).unwrap();
        assert_eq!(f.store.task(f.root_id).unwrap().unwrap().status, "pending");

        super::advance(f.dir.path(), &f.cfg, &mut f.store).unwrap();
        assert!(f.root_is_gone(), "the pending root settles without a child");
        assert_eq!(f.target_bytes(), page.as_bytes());
        assert_eq!(
            f.baseline(".wiki/pages/concept/a.md"),
            Some(hash::hash_bytes(page.as_bytes()))
        );
    }

    #[test]
    fn a_route_under_a_blocked_or_failed_root_is_not_delivered() {
        let mut f = Fixture::settling(".wiki/pages/concept/a.md", None, "expected");
        let route_id = f
            .store
            .tasks()
            .unwrap()
            .into_iter()
            .find(|task| task.kind == "route" && task.parent_id == Some(f.root_id))
            .expect("the fixture root owns a route")
            .id;
        f.store
            .block_task(f.root_id, "E_PAGE_CONFLICT: test", &super::now())
            .unwrap();
        assert!(
            super::advance(f.dir.path(), &f.cfg, &mut f.store)
                .unwrap()
                .task
                .is_none(),
            "a blocked root's route is not exposed"
        );
        assert!(super::status(f.dir.path(), &f.cfg, &mut f.store)
            .unwrap()
            .current
            .is_none());

        f.store
            .set_task_status(f.root_id, "failed", &super::now())
            .unwrap();
        assert!(
            super::advance(f.dir.path(), &f.cfg, &mut f.store)
                .unwrap()
                .task
                .is_none(),
            "a failed root's route is not exposed"
        );
        assert_eq!(
            f.store.task(route_id).unwrap().unwrap().status,
            "pending",
            "the route stays queued for a replan"
        );
    }

    #[test]
    fn regenerating_a_review_replans_an_existing_blocked_root() {
        let mut f = Fixture::partially_written();
        let page = f.target_bytes();
        let raw_key = f
            .store
            .all_raws()
            .unwrap()
            .into_iter()
            .find(|raw| raw.source_key == f.source_key)
            .unwrap()
            .key;
        let payload = serde_json::json!({
            "page_hash": hash::hash_bytes(&page),
            "raw_keys": [raw_key],
        });
        let mut reconcile = NewTask::pending("reconcile", &super::now());
        reconcile.target_path = Some(f.target.clone());
        reconcile.payload_json = Some(payload.to_string());
        let reconcile_id = f.store.insert_task(&reconcile).unwrap();

        super::close(
            f.dir.path(),
            &f.cfg,
            &mut f.store,
            reconcile_id,
            None,
            None,
            Some(Verdict::Regenerate),
        )
        .unwrap();

        assert!(f.store.task(reconcile_id).unwrap().is_none());
        assert_eq!(f.target_bytes(), page, "the written page is preserved");
        let root = f.store.task(f.root_id).unwrap().expect("root remains");
        assert_eq!(root.status, "active");
        assert_eq!(root.last_error, None);
        let tasks = f.store.tasks().unwrap();
        let kinds: Vec<&str> = tasks.iter().map(|task| task.kind.as_str()).collect();
        assert_eq!(kinds, ["breakdown", "route"]);
        assert!(
            !f.staging_root().exists()
                || std::fs::read_dir(f.staging_root())
                    .unwrap()
                    .next()
                    .is_none(),
            "replanning through a review also drops the old staging"
        );
    }

    #[test]
    fn a_source_that_drifts_after_the_merge_blocks_with_a_replan_hint() {
        let page = "---\nid: a\ntitle: A\ntype: concept\nsummary: s\ntags: []\nsources: []\nstatus: stable\nupdated: 2026-01-01\n---\nbody\n";
        let mut f = Fixture::settling(".wiki/pages/concept/a.md", None, page);
        // The batch merges first; only then does the raw stop matching the
        // ledger, so `worklist::complete` is what fails.
        let raw = f.dir.path().join(&f.cfg.paths.raw).join(&f.source_key);
        std::fs::write(&raw, "# changed after the merge\n").unwrap();
        let err = f.advance().unwrap_err();
        assert_eq!(err.code(), "E_SOURCE_DRIFT");
        assert!(f.root_is_blocked());
        assert!(f.staging_exists());
        assert_eq!(f.target_bytes(), page.as_bytes());
        assert_eq!(
            f.baseline(".wiki/pages/concept/a.md"),
            Some(hash::hash_bytes(page.as_bytes()))
        );
        let row = f.store.task(f.root_id).unwrap().expect("root remains");
        let message = row.last_error.unwrap_or_default();
        assert!(message.starts_with("E_SOURCE_DRIFT"), "{message}");
        assert!(message.contains("--replan"), "{message}");
        assert!(!message.contains("batch stays in staging"), "{message}");
    }
}
