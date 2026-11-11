//! `route-v1`: what one source is supposed to become.
//!
//! The payload is the agent's own account of the source: the points it found,
//! where each point should go, and what happened to the pages that already cite
//! the source. [`parse`] only reads JSON; [`validate_shape`] checks the
//! cross-field rules the schema cannot express. Both report
//! `E_TASK_BAD_PAYLOAD` and name the point, page or proposal at fault.

use std::collections::BTreeSet;

use serde::Deserialize;

use super::{bad, Result};

/// One piece of information extracted from the source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Point {
    /// Stable only inside this payload; not a page id.
    pub id: String,
    /// What the source says.
    pub finding: String,
    /// Where in the source it says it.
    pub evidence: String,
    /// What must not be claimed beyond the source, when that matters.
    pub boundary: Option<String>,
}

/// Where one point goes: write it, found a page for it, or leave it alone.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Assignment {
    /// [`Point::id`] this assignment answers.
    pub point: String,
    /// `update`, `create` or `skip`.
    pub action: String,
    /// Existing page to revise; `update` only.
    pub target: Option<String>,
    /// What the revision adds or changes; `update` only.
    pub change: Option<String>,
    /// Why the change is warranted; `update` only.
    pub basis: Option<String>,
    /// [`Proposal::id`] to found a new page from; `create` only.
    pub proposal: Option<String>,
    /// Why nothing is written; `skip` only.
    pub reason: Option<String>,
    /// Page that already carries the point; `skip` only.
    pub covered_by: Option<String>,
}

/// The verdict for one page that already cites the source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Maintenance {
    /// Repository-relative page path.
    pub page: String,
    /// `update` or `unchanged`.
    pub action: String,
    /// Why the page is revised or deliberately left as it is.
    pub reason: String,
}

/// A page the route wants to create. The CLI allocates its path and id.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Proposal {
    /// Stable only inside this payload.
    pub id: String,
    /// One of `[schema].types`.
    #[serde(rename = "type")]
    pub typ: String,
    /// Semantic title; the CLI derives the slug from it.
    pub title: String,
    /// What the page says.
    pub summary: String,
    /// Why the point deserves a page of its own.
    pub why_new: String,
    /// Why the similar pages the agent read cannot carry it.
    pub why_not_existing: String,
}

/// A search the agent actually considered, for the record.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Search {
    /// Query terms considered.
    pub query: String,
    /// Pages the query surfaced.
    #[serde(default)]
    pub candidates: Vec<String>,
    /// Why the candidates did or did not fit.
    pub reason: String,
}

/// One complete `route-v1` payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RouteDoc {
    /// Information the source carries.
    #[serde(default)]
    pub points: Vec<Point>,
    /// Disposition of every point.
    #[serde(default)]
    pub assignments: Vec<Assignment>,
    /// Verdict for every affected page.
    #[serde(default)]
    pub maintenance: Vec<Maintenance>,
    /// Pages the `create` assignments ask for.
    #[serde(default)]
    pub proposals: Vec<Proposal>,
    /// Searches the agent ran, when it wants to record them.
    #[serde(default)]
    pub searches: Vec<Search>,
    /// Why a source with no points still needs no page.
    #[serde(default)]
    pub source_reason: Option<String>,
}

/// Deserializes a `route-v1` payload.
pub(crate) fn parse(bytes: &[u8]) -> Result<RouteDoc> {
    serde_json::from_slice(bytes).map_err(|err| {
        bad(
            "E_TASK_BAD_PAYLOAD",
            format!("route payload is not valid JSON: {err}"),
        )
    })
}

/// Checks every reference and cross-field rule, in the order a reader would
/// expect: points, their assignments, the proposals they name, the pages that
/// already exist, and finally the "nothing to write" case.
pub(crate) fn validate_shape(doc: &RouteDoc, affected: &[String], types: &[String]) -> Result<()> {
    let point_ids = point_ids(doc)?;
    let proposal_ids: BTreeSet<&str> = doc.proposals.iter().map(|p| p.id.as_str()).collect();
    ensure_every_point_has_an_assignment(doc)?;
    check_assignments(doc, &point_ids, &proposal_ids)?;
    check_proposals(doc, types)?;
    check_maintenance(doc, affected)?;
    check_empty_route(doc)?;
    check_searches(doc)?;
    Ok(())
}

/// Unique, non-empty point ids.
fn point_ids(doc: &RouteDoc) -> Result<BTreeSet<&str>> {
    let mut ids = BTreeSet::new();
    for point in &doc.points {
        if point.id.trim().is_empty() {
            return Err(bad("E_TASK_BAD_PAYLOAD", "a point needs a non-empty id"));
        }
        if !ids.insert(point.id.as_str()) {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("point id '{}' appears more than once", point.id),
            ));
        }
    }
    Ok(ids)
}

/// Every point must say what should happen to it.
fn ensure_every_point_has_an_assignment(doc: &RouteDoc) -> Result<()> {
    let assigned: BTreeSet<&str> = doc.assignments.iter().map(|a| a.point.as_str()).collect();
    for point in &doc.points {
        if !assigned.contains(point.id.as_str()) {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("point '{}' has no assignment", point.id),
            ));
        }
    }
    Ok(())
}

/// Every assignment names a real point and carries only its action's fields.
fn check_assignments(
    doc: &RouteDoc,
    point_ids: &BTreeSet<&str>,
    proposal_ids: &BTreeSet<&str>,
) -> Result<()> {
    for assignment in &doc.assignments {
        if !point_ids.contains(assignment.point.as_str()) {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!(
                    "assignment refers to point '{}', which is not in points",
                    assignment.point
                ),
            ));
        }
        match assignment.action.as_str() {
            "skip" => check_skip(assignment)?,
            "update" => check_update(assignment)?,
            "create" => check_create(assignment, proposal_ids)?,
            other => {
                return Err(bad(
                    "E_TASK_BAD_PAYLOAD",
                    format!(
                        "point '{}': action '{other}' is not update, create or skip",
                        assignment.point
                    ),
                ))
            }
        }
    }
    Ok(())
}

/// `skip` explains itself and carries nothing that would write a page.
fn check_skip(assignment: &Assignment) -> Result<()> {
    reject_carried(
        &assignment.point,
        "skip",
        &[
            ("target", assignment.target.is_some()),
            ("change", assignment.change.is_some()),
            ("basis", assignment.basis.is_some()),
            ("proposal", assignment.proposal.is_some()),
        ],
    )?;
    require_field(
        &assignment.point,
        "skip",
        "reason",
        assignment.reason.as_deref(),
    )
}

/// `update` needs a page, the change, and the evidence behind it.
fn check_update(assignment: &Assignment) -> Result<()> {
    require_field(
        &assignment.point,
        "update",
        "target",
        assignment.target.as_deref(),
    )?;
    require_field(
        &assignment.point,
        "update",
        "change",
        assignment.change.as_deref(),
    )?;
    require_field(
        &assignment.point,
        "update",
        "basis",
        assignment.basis.as_deref(),
    )?;
    reject_carried(
        &assignment.point,
        "update",
        &[
            ("proposal", assignment.proposal.is_some()),
            ("reason", assignment.reason.is_some()),
            ("covered_by", assignment.covered_by.is_some()),
        ],
    )
}

/// `create` names a proposal that the payload actually defines; the CLI
/// allocates the path, so a target here would be an ambiguous second opinion.
fn check_create(assignment: &Assignment, proposal_ids: &BTreeSet<&str>) -> Result<()> {
    require_field(
        &assignment.point,
        "create",
        "proposal",
        assignment.proposal.as_deref(),
    )?;
    let proposal = assignment.proposal.as_deref().unwrap_or_default();
    if !proposal_ids.contains(proposal) {
        return Err(bad(
            "E_TASK_BAD_PAYLOAD",
            format!(
                "point '{}': create refers to proposal '{proposal}', which is not in proposals",
                assignment.point
            ),
        ));
    }
    reject_carried(
        &assignment.point,
        "create",
        &[
            ("target", assignment.target.is_some()),
            ("change", assignment.change.is_some()),
            ("basis", assignment.basis.is_some()),
            ("reason", assignment.reason.is_some()),
            ("covered_by", assignment.covered_by.is_some()),
        ],
    )
}

/// Proposals are unique, typed, justified, and each one is actually used.
fn check_proposals(doc: &RouteDoc, types: &[String]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for proposal in &doc.proposals {
        if proposal.id.trim().is_empty() {
            return Err(bad("E_TASK_BAD_PAYLOAD", "a proposal needs a non-empty id"));
        }
        if !seen.insert(proposal.id.as_str()) {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("proposal id '{}' appears more than once", proposal.id),
            ));
        }
        if !types.iter().any(|known| known == &proposal.typ) {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!(
                    "proposal '{}' has type '{}', which is not in [schema].types ({})",
                    proposal.id,
                    proposal.typ,
                    types.join(", ")
                ),
            ));
        }
        for (field, value) in [
            ("title", &proposal.title),
            ("summary", &proposal.summary),
            ("why_new", &proposal.why_new),
            ("why_not_existing", &proposal.why_not_existing),
        ] {
            require_text(&proposal.id, field, value)?;
        }
    }

    let referenced: BTreeSet<&str> = doc
        .assignments
        .iter()
        .filter(|assignment| assignment.action == "create")
        .filter_map(|assignment| assignment.proposal.as_deref())
        .collect();
    for proposal in &doc.proposals {
        if !referenced.contains(proposal.id.as_str()) {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!(
                    "proposal '{}' is not referenced by any create assignment",
                    proposal.id
                ),
            ));
        }
    }
    Ok(())
}

/// Every affected page gets exactly one verdict with a reason; no page that
/// does not cite the source is dragged in.
fn check_maintenance(doc: &RouteDoc, affected: &[String]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for item in &doc.maintenance {
        if item.action != "update" && item.action != "unchanged" {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!(
                    "maintenance for {} has action '{}', expected update or unchanged",
                    item.page, item.action
                ),
            ));
        }
        if item.reason.trim().is_empty() {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("maintenance for {} needs a non-empty reason", item.page),
            ));
        }
        if !affected.iter().any(|page| page == &item.page) {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!(
                    "maintenance names {}, which is not an affected page",
                    item.page
                ),
            ));
        }
        if !seen.insert(item.page.as_str()) {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("{} appears more than once in maintenance", item.page),
            ));
        }
    }
    for page in affected {
        if !seen.contains(page.as_str()) {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("{page} is an affected page but maintenance has no verdict for it"),
            ));
        }
    }
    Ok(())
}

/// A route with no points still has to say why it writes nothing.
fn check_empty_route(doc: &RouteDoc) -> Result<()> {
    if doc.points.is_empty()
        && !doc
            .source_reason
            .as_deref()
            .is_some_and(|reason| !reason.trim().is_empty())
    {
        return Err(bad(
            "E_TASK_BAD_PAYLOAD",
            "a route with no points needs a non-empty source_reason",
        ));
    }
    Ok(())
}

/// A recorded search without terms or a verdict is noise, not evidence.
fn check_searches(doc: &RouteDoc) -> Result<()> {
    for (index, search) in doc.searches.iter().enumerate() {
        let label = index + 1;
        if search.query.trim().is_empty() {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("search #{label} needs a non-empty query"),
            ));
        }
        if search.reason.trim().is_empty() {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("search #{label} needs a non-empty reason"),
            ));
        }
    }
    Ok(())
}

fn nonempty(value: Option<&str>) -> bool {
    value.is_some_and(|text| !text.trim().is_empty())
}

fn require_field(point: &str, action: &str, field: &str, value: Option<&str>) -> Result<()> {
    if nonempty(value) {
        return Ok(());
    }
    Err(bad(
        "E_TASK_BAD_PAYLOAD",
        format!("point '{point}': {action} assignment needs a non-empty {field}"),
    ))
}

fn reject_carried(point: &str, action: &str, fields: &[(&str, bool)]) -> Result<()> {
    for (field, carried) in fields {
        if *carried {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!("point '{point}': {action} assignment cannot carry {field}"),
            ));
        }
    }
    Ok(())
}

fn require_text(id: &str, field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(bad(
            "E_TASK_BAD_PAYLOAD",
            format!("proposal '{id}' needs a non-empty {field}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types() -> Vec<String> {
        vec!["concept".to_string()]
    }

    fn page() -> String {
        ".wiki/pages/concept/a.md".to_string()
    }

    fn error(json: &str, affected: &[String]) -> String {
        let doc = parse(json.as_bytes()).unwrap();
        validate_shape(&doc, affected, &types())
            .unwrap_err()
            .to_string()
    }

    /// Validates one assignment (`"point":"p1",...` without the braces) in an
    /// otherwise valid payload and returns the rejection.
    fn reject(assignment: &str) -> crate::task::TaskError {
        let json = format!(
            r#"{{"points":[{{"id":"p1","finding":"f","evidence":"e"}}],
                "assignments":[{{{assignment}}}],
                "proposals":[{{"id":"n1","type":"concept","title":"N","summary":"s",
                    "why_new":"n","why_not_existing":"e"}}]}}"#
        );
        let doc = parse(json.as_bytes()).unwrap();
        validate_shape(&doc, &[], &types()).unwrap_err()
    }

    /// Asserts a rejection names the action and the offending field, and uses
    /// the one payload error code.
    fn assert_rejected(err: &crate::task::TaskError, action: &str, field: &str) {
        let message = err.to_string();
        assert!(message.contains(action), "{message}");
        assert!(message.contains(field), "{message}");
        assert_eq!(err.code(), "E_TASK_BAD_PAYLOAD", "{message}");
    }

    #[test]
    fn a_point_without_an_assignment_is_rejected() {
        let doc = parse(br#"{"points":[{"id":"p1","finding":"f","evidence":"e"}]}"#).unwrap();
        let err = validate_shape(&doc, &[], &["concept".into()]).unwrap_err();
        assert!(err.to_string().contains("p1"));
    }

    #[test]
    fn skip_needs_a_reason() {
        let err = error(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "assignments":[{"point":"p1","action":"skip"}]}"#,
            &[],
        );
        assert!(err.contains("p1"), "{err}");
        assert!(err.contains("reason"), "{err}");
    }

    #[test]
    fn a_create_assignment_cannot_name_a_target() {
        let err = error(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "assignments":[{"point":"p1","action":"create","proposal":"n1",
                    "target":".wiki/pages/concept/a.md"}],
                "proposals":[{"id":"n1","type":"concept","title":"N","summary":"s",
                    "why_new":"n","why_not_existing":"e"}]}"#,
            &[],
        );
        assert!(err.contains("p1"), "{err}");
        assert!(err.contains("target"), "{err}");
    }

    #[test]
    fn skip_cannot_carry_a_target() {
        let doc = parse(r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
            "assignments":[{"point":"p1","action":"skip","reason":"已有","target":".wiki/pages/concept/a.md"}]}"#.as_bytes()).unwrap();
        assert!(validate_shape(&doc, &[], &["concept".into()]).is_err());
    }

    #[test]
    fn every_affected_page_needs_a_verdict() {
        let doc = parse(br#"{}"#).unwrap();
        let err = validate_shape(
            &doc,
            &[".wiki/pages/concept/a.md".into()],
            &["concept".into()],
        )
        .unwrap_err();
        assert!(err.to_string().contains("concept/a.md"));
    }

    #[test]
    fn duplicate_point_ids_are_rejected() {
        let err = error(
            r#"{"points":[
                {"id":"p1","finding":"a","evidence":"e"},
                {"id":"p1","finding":"b","evidence":"e"}]}"#,
            &[],
        );
        assert!(err.contains("p1"), "{err}");
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn an_empty_point_id_is_rejected() {
        let err = error(
            r#"{"points":[{"id":"","finding":"f","evidence":"e"}]}"#,
            &[],
        );
        assert!(err.contains("empty id"), "{err}");
    }

    #[test]
    fn an_assignment_to_an_unknown_point_is_rejected() {
        let err = error(
            r#"{"points":[],
                "assignments":[{"point":"p9","action":"skip","reason":"x"}],
                "source_reason":"nothing"}"#,
            &[],
        );
        assert!(err.contains("p9"), "{err}");
    }

    #[test]
    fn an_update_assignment_needs_target_change_and_basis() {
        let err = error(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "assignments":[{"point":"p1","action":"update",
                    "target":".wiki/pages/concept/a.md","change":"补一段"}]}"#,
            &[],
        );
        assert!(err.contains("p1"), "{err}");
        assert!(err.contains("basis"), "{err}");
    }

    #[test]
    fn a_create_assignment_needs_a_known_proposal() {
        let err = error(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "assignments":[{"point":"p1","action":"create","proposal":"n1"}]}"#,
            &[],
        );
        assert!(err.contains("n1"), "{err}");
    }

    #[test]
    fn skip_cannot_carry_a_proposal() {
        let err = error(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "assignments":[{"point":"p1","action":"skip","reason":"已有","proposal":"n1"}]}"#,
            &[],
        );
        assert!(err.contains("skip"), "{err}");
        assert!(err.contains("proposal"), "{err}");
    }

    #[test]
    fn an_unknown_action_is_rejected() {
        let err = error(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "assignments":[{"point":"p1","action":"delete"}]}"#,
            &[],
        );
        assert!(err.contains("delete"), "{err}");
    }

    #[test]
    fn a_proposal_outside_the_schema_types_is_rejected() {
        let err = error(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "assignments":[{"point":"p1","action":"create","proposal":"n1"}],
                "proposals":[{"id":"n1","type":"person","title":"N","summary":"s",
                    "why_new":"n","why_not_existing":"e"}]}"#,
            &[],
        );
        assert!(err.contains("person"), "{err}");
        assert!(err.contains("n1"), "{err}");
    }

    #[test]
    fn a_proposal_reason_must_be_nonempty() {
        let err = error(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "assignments":[{"point":"p1","action":"create","proposal":"n1"}],
                "proposals":[{"id":"n1","type":"concept","title":"N","summary":"s",
                    "why_new":"  ","why_not_existing":"e"}]}"#,
            &[],
        );
        assert!(err.contains("n1"), "{err}");
        assert!(err.contains("why_new"), "{err}");
    }

    #[test]
    fn a_proposal_no_create_references_is_rejected() {
        let err = error(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "assignments":[{"point":"p1","action":"skip","reason":"已有"}],
                "proposals":[{"id":"n1","type":"concept","title":"N","summary":"s",
                    "why_new":"n","why_not_existing":"e"}]}"#,
            &[],
        );
        assert!(err.contains("n1"), "{err}");
        assert!(err.contains("not referenced"), "{err}");
    }

    #[test]
    fn duplicate_proposal_ids_are_rejected() {
        let err = error(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "assignments":[{"point":"p1","action":"create","proposal":"n1"}],
                "proposals":[
                    {"id":"n1","type":"concept","title":"N","summary":"s","why_new":"n","why_not_existing":"e"},
                    {"id":"n1","type":"concept","title":"M","summary":"s","why_new":"n","why_not_existing":"e"}]}"#,
            &[],
        );
        assert!(err.contains("n1"), "{err}");
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn maintenance_cannot_name_a_page_that_is_not_affected() {
        let err = error(
            r#"{"maintenance":[{"page":".wiki/pages/concept/b.md",
                "action":"unchanged","reason":"看过"}]}"#,
            &[page()],
        );
        assert!(err.contains("concept/b.md"), "{err}");
        assert!(err.contains("affected"), "{err}");
    }

    #[test]
    fn a_maintenance_reason_must_be_nonempty() {
        let err = error(
            r#"{"maintenance":[{"page":".wiki/pages/concept/a.md",
                "action":"unchanged","reason":"  "}]}"#,
            &[page()],
        );
        assert!(err.contains("concept/a.md"), "{err}");
        assert!(err.contains("reason"), "{err}");
    }

    #[test]
    fn a_maintenance_action_is_update_or_unchanged() {
        let err = error(
            r#"{"maintenance":[{"page":".wiki/pages/concept/a.md",
                "action":"delete","reason":"看过"}]}"#,
            &[page()],
        );
        assert!(err.contains("concept/a.md"), "{err}");
        assert!(err.contains("delete"), "{err}");
    }

    #[test]
    fn duplicate_maintenance_entries_are_rejected() {
        let err = error(
            r#"{"maintenance":[
                {"page":".wiki/pages/concept/a.md","action":"unchanged","reason":"看过"},
                {"page":".wiki/pages/concept/a.md","action":"update","reason":"补"}]}"#,
            &[page()],
        );
        assert!(err.contains("concept/a.md"), "{err}");
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn an_empty_route_needs_a_source_reason() {
        let err = error(
            r#"{"maintenance":[{"page":".wiki/pages/concept/a.md",
                "action":"unchanged","reason":"看过"}]}"#,
            &[page()],
        );
        assert!(err.contains("source_reason"), "{err}");
    }

    #[test]
    fn an_empty_route_with_a_source_reason_is_valid() {
        let doc = parse(
            r#"{"maintenance":[{"page":".wiki/pages/concept/a.md",
                "action":"unchanged","reason":"看过"}],
                "source_reason":"没有可写入的信息"}"#
                .as_bytes(),
        )
        .unwrap();
        validate_shape(&doc, &[page()], &types()).unwrap();
    }

    #[test]
    fn a_search_needs_a_query_and_a_reason() {
        let err = error(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "assignments":[{"point":"p1","action":"skip","reason":"已有"}],
                "searches":[{"query":"  ","reason":"搜过"}]}"#,
            &[],
        );
        assert!(err.contains("query"), "{err}");
    }

    #[test]
    fn a_search_needs_a_reason() {
        let err = error(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "assignments":[{"point":"p1","action":"skip","reason":"已有"}],
                "searches":[{"query":"alpha","reason":" "}]}"#,
            &[],
        );
        assert!(err.contains("search #1"), "{err}");
        assert!(err.contains("reason"), "{err}");
    }

    #[test]
    fn an_unknown_field_is_rejected_by_the_parser() {
        let err = parse(br#"{"points":[],"extra":1}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown field"), "{err}");
    }

    #[test]
    fn nested_unknown_fields_are_rejected_by_the_parser() {
        for json in [
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e","extra":1}]}"#,
            r#"{"assignments":[{"point":"p1","action":"skip","reason":"r","extra":1}]}"#,
        ] {
            let err = parse(json.as_bytes()).unwrap_err();
            assert!(err.to_string().contains("unknown field"), "{err}");
            assert_eq!(err.code(), "E_TASK_BAD_PAYLOAD", "{err}");
        }
    }

    #[test]
    fn an_update_assignment_cannot_carry_another_actions_fields() {
        let base = r#""point":"p1","action":"update","target":".wiki/pages/concept/a.md",
            "change":"补一段","basis":"b""#;
        for (field, value) in [
            ("proposal", r#""n1""#),
            ("reason", r#""已有""#),
            ("covered_by", r#""...b.md""#),
        ] {
            let err = reject(&format!("{base},\"{field}\":{value}"));
            assert_rejected(&err, "update", field);
        }
    }

    #[test]
    fn a_create_assignment_cannot_carry_another_actions_fields() {
        let base = r#""point":"p1","action":"create","proposal":"n1""#;
        for (field, value) in [
            ("target", r#""...a.md""#),
            ("change", r#""补一段""#),
            ("basis", r#""b""#),
            ("reason", r#""已有""#),
            ("covered_by", r#""...b.md""#),
        ] {
            let err = reject(&format!("{base},\"{field}\":{value}"));
            assert_rejected(&err, "create", field);
        }
    }

    #[test]
    fn a_skip_assignment_carries_only_reason_and_covered_by() {
        for (field, value) in [
            ("target", r#""...a.md""#),
            ("change", r#""补一段""#),
            ("basis", r#""b""#),
            ("proposal", r#""n1""#),
        ] {
            let err = reject(&format!(
                r#""point":"p1","action":"skip","reason":"已有","{field}":{value}"#
            ));
            assert_rejected(&err, "skip", field);
        }
        let err = reject(r#""point":"p1","action":"skip""#);
        assert_rejected(&err, "skip", "reason");
    }

    #[test]
    fn a_complete_route_is_valid() {
        let doc = parse(
            r#"{"points":[
                {"id":"p1","finding":"一","evidence":"e1","boundary":"b"},
                {"id":"p2","finding":"二","evidence":"e2"},
                {"id":"p3","finding":"三","evidence":"e3"}],
                "assignments":[
                {"point":"p1","action":"update","target":".wiki/pages/concept/a.md",
                    "change":"补一","basis":"b1"},
                {"point":"p2","action":"create","proposal":"n1"},
                {"point":"p3","action":"skip","reason":"已有","covered_by":".wiki/pages/concept/a.md"}],
                "maintenance":[{"page":".wiki/pages/concept/a.md","action":"update","reason":"有新增"}],
                "proposals":[{"id":"n1","type":"concept","title":"N","summary":"s",
                    "why_new":"n","why_not_existing":"e"}],
                "searches":[{"query":"alpha","candidates":[".wiki/pages/concept/a.md"],"reason":"搜过"}],
                "source_reason":"提取到三条"}"#
                .as_bytes(),
        )
        .unwrap();
        validate_shape(&doc, &[page()], &types()).unwrap();
    }
}
