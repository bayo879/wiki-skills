//! Root and page plan payloads.
//!
//! A root task (`breakdown`) carries one `root-plan-v1`: the stage it is in and
//! the target pages the route decided on. Each page task carries one
//! `page-plan-v1`: the frozen snapshot of its target plus what that page has to
//! say. Both are stored in `tasks.payload_json`, so the schema version is
//! checked on every read.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::{bad, Result};
use crate::store::TaskRow;

/// Schema tag of the root payload.
pub(crate) const ROOT_PLAN_SCHEMA: &str = "root-plan-v1";
/// Schema tag of a page payload.
pub(crate) const PAGE_PLAN_SCHEMA: &str = "page-plan-v1";

/// Business stage of one root. Execution state (`pending`/`active`/...) stays
/// on the task row; this is what the root has finished doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stage {
    /// A route exists and no target has been planned yet.
    Routing,
    /// Targets are planned; page tasks write them.
    Writing,
    /// No page is outstanding; the root is merging or has merged.
    Settling,
}

/// Whether a target page existed when the route planned it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Origin {
    /// The page already existed; `before` records its hash.
    Existing,
    /// The route decided to create the page; `before` is `None`.
    Created,
}

/// How far one target has been written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WriteStatus {
    /// Planned, not staged yet.
    Planned,
    /// The page task wrote a draft into staging and recorded `expected`.
    Staged,
    /// The draft was merged into `.wiki/pages/`.
    Written,
}

/// One piece of information the page has to carry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanPoint {
    /// Stable id, unique within the route payload.
    pub id: String,
    /// What the source says.
    pub finding: String,
    /// Where in the source it says it.
    pub evidence: String,
    /// What must not be claimed beyond the source, when that matters.
    pub boundary: Option<String>,
}

/// One target page and what the source contributes to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TargetPlan {
    /// Repository-relative page path.
    pub path: String,
    /// Page title as planned.
    pub title: String,
    /// Whether the page is an existing one or a new one.
    pub origin: Origin,
    /// Hash of the page when it was planned; `None` for a new page.
    pub before: Option<String>,
    /// Hash of the merged body once staged; `None` until then.
    pub expected: Option<String>,
    /// Write progress.
    pub status: WriteStatus,
    /// Information points this page must carry.
    pub points: Vec<PlanPoint>,
    /// What the route asks the page to change.
    pub change: String,
}

/// The root payload: stage plus every planned target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RootPlan {
    /// Always [`ROOT_PLAN_SCHEMA`].
    pub schema: String,
    /// Current business stage.
    pub stage: Stage,
    /// Source the root settles.
    pub source_key: String,
    /// Raw the source was converted to.
    pub raw_key: String,
    /// Fixed raw hash the tree was built around.
    pub raw_hash: String,
    /// Targets decided by the route; empty while routing.
    pub targets: Vec<TargetPlan>,
}

impl RootPlan {
    /// A root that has just been created and waits for its route.
    ///
    /// Routing only knows the raw; `source_key` is filled in when the route
    /// completes and the root moves to writing.
    pub(crate) fn new_routing(raw_key: &str, raw_hash: &str) -> Self {
        RootPlan {
            schema: ROOT_PLAN_SCHEMA.to_string(),
            stage: Stage::Routing,
            source_key: String::new(),
            raw_key: raw_key.to_string(),
            raw_hash: raw_hash.to_string(),
            targets: Vec::new(),
        }
    }

    /// Serializes the plan for `tasks.payload_json`.
    pub(crate) fn json(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|err| {
            bad(
                "E_TASK_BAD_PAYLOAD",
                format!("cannot encode root plan: {err}"),
            )
        })
    }

    /// Reads the plan off a `breakdown` task and checks its schema.
    pub(crate) fn parse(task: &TaskRow) -> Result<Self> {
        let plan: RootPlan = parse_payload(task, "breakdown")?;
        if plan.schema != ROOT_PLAN_SCHEMA {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!(
                    "task {} root plan has schema '{}', expected '{ROOT_PLAN_SCHEMA}'",
                    task.id, plan.schema
                ),
            ));
        }
        Ok(plan)
    }
}

/// The page payload: one frozen target plus the raw it was planned against.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PagePlan {
    /// Always [`PAGE_PLAN_SCHEMA`].
    pub schema: String,
    /// Id of the root this page belongs to.
    pub root: i64,
    /// Raw the page is written from.
    pub raw_key: String,
    /// Fixed raw hash the page task checks before writing.
    pub raw_hash: String,
    /// Snapshot and instructions for the one page.
    pub target: TargetPlan,
}

impl PagePlan {
    /// Serializes the plan for `tasks.payload_json`.
    pub(crate) fn json(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|err| {
            bad(
                "E_TASK_BAD_PAYLOAD",
                format!("cannot encode page plan: {err}"),
            )
        })
    }

    /// Reads the plan off a `page` task and checks its schema.
    pub(crate) fn parse(task: &TaskRow) -> Result<Self> {
        let plan: PagePlan = parse_payload(task, "page")?;
        if plan.schema != PAGE_PLAN_SCHEMA {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!(
                    "task {} page plan has schema '{}', expected '{PAGE_PLAN_SCHEMA}'",
                    task.id, plan.schema
                ),
            ));
        }
        Ok(plan)
    }
}

/// Deserializes `task.payload_json`, insisting on the task kind first.
fn parse_payload<T: DeserializeOwned>(task: &TaskRow, kind: &str) -> Result<T> {
    if task.kind != kind {
        return Err(bad(
            "E_TASK_BAD_PAYLOAD",
            format!(
                "task {} is a '{}' task, not a '{}' task",
                task.id, task.kind, kind
            ),
        ));
    }
    let json = task.payload_json.as_deref().ok_or_else(|| {
        bad(
            "E_TASK_BAD_PAYLOAD",
            format!("task {} has no plan payload", task.id),
        )
    })?;
    serde_json::from_str(json).map_err(|err| {
        bad(
            "E_TASK_BAD_PAYLOAD",
            format!("task {} plan is not valid JSON: {err}", task.id),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str, payload: Option<&str>) -> TaskRow {
        TaskRow {
            id: 7,
            parent_id: None,
            kind: kind.to_string(),
            status: "pending".to_string(),
            source_key: Some("a.md".to_string()),
            raw_hash: Some("blake3:aa".to_string()),
            target_path: None,
            allowed_json: None,
            payload_json: payload.map(str::to_string),
            attempts: 0,
            last_error: None,
            created_at: "2026-11-11T00:00:00Z".to_string(),
            updated_at: "2026-11-11T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn a_root_plan_starts_in_routing() {
        let plan = RootPlan::new_routing("raw/a.md", "blake3:aa");
        assert_eq!(plan.schema, ROOT_PLAN_SCHEMA);
        assert_eq!(plan.stage, Stage::Routing);
        assert!(plan.targets.is_empty());
    }

    #[test]
    fn a_page_plan_round_trips() {
        let target = TargetPlan {
            path: ".wiki/pages/concept/a.md".into(),
            title: "A".into(),
            origin: Origin::Existing,
            before: Some("blake3:aa".into()),
            expected: None,
            status: WriteStatus::Planned,
            points: vec![],
            change: "补一段".into(),
        };
        let plan = PagePlan {
            schema: PAGE_PLAN_SCHEMA.into(),
            root: 7,
            raw_key: "raw/a.md".into(),
            raw_hash: "blake3:aa".into(),
            target,
        };
        let parsed: PagePlan = serde_json::from_str(&plan.json().unwrap()).unwrap();
        assert_eq!(parsed.target.path, ".wiki/pages/concept/a.md");
    }

    #[test]
    fn a_root_plan_round_trips_through_its_task() {
        let mut plan = RootPlan::new_routing("raw/a.md", "blake3:aa");
        plan.source_key = "a.md".into();
        let task = row("breakdown", Some(&plan.json().unwrap()));
        let parsed = RootPlan::parse(&task).unwrap();
        assert_eq!(parsed.source_key, "a.md");
        assert_eq!(parsed.raw_hash, "blake3:aa");
    }

    #[test]
    fn a_plan_on_the_wrong_task_kind_is_rejected() {
        let plan = RootPlan::new_routing("raw/a.md", "blake3:aa");
        let task = row("page", Some(&plan.json().unwrap()));
        let err = RootPlan::parse(&task).unwrap_err();
        assert!(err.to_string().contains("not a 'breakdown' task"));
    }

    #[test]
    fn a_plan_without_payload_is_rejected() {
        let err = PagePlan::parse(&row("page", None)).unwrap_err();
        assert!(err.to_string().contains("no plan payload"));
    }

    #[test]
    fn a_plan_with_another_schema_is_rejected() {
        let plan = PagePlan {
            schema: "page-plan-v0".into(),
            root: 7,
            raw_key: "raw/a.md".into(),
            raw_hash: "blake3:aa".into(),
            target: TargetPlan {
                path: ".wiki/pages/concept/a.md".into(),
                title: "A".into(),
                origin: Origin::Created,
                before: None,
                expected: None,
                status: WriteStatus::Planned,
                points: vec![],
                change: "新建".into(),
            },
        };
        let task = row("page", Some(&plan.json().unwrap()));
        let err = PagePlan::parse(&task).unwrap_err();
        assert!(err.to_string().contains("expected 'page-plan-v1'"));
    }

    #[test]
    fn an_unknown_plan_field_is_rejected() {
        let json = r#"{"schema":"root-plan-v1","stage":"routing","source_key":"a.md",
            "raw_key":"raw/a.md","raw_hash":"blake3:aa","targets":[],"extra":1}"#;
        let err = RootPlan::parse(&row("breakdown", Some(json))).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }
}
