//! `route-v1`: what one source is supposed to become.
//!
//! The payload is the agent's own account of the source: the points it found,
//! where each point should go, and what happened to the pages that already cite
//! the source. [`parse`] only reads JSON; [`validate_shape`] checks the
//! cross-field rules the schema cannot express. Both report
//! `E_TASK_BAD_PAYLOAD` and name the point, page or proposal at fault.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;

use super::plan::{Origin, PlanPoint};
use super::{bad, Result};
use crate::config::Config;
use crate::hash;
use crate::page;

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

/// One target page and everything the route contributes to it.
///
/// `before` is the page hash at planning time, or `None` when the route creates
/// the page. It is the snapshot the page task checks before writing, so a page
/// edited by someone else after routing is not silently overwritten.
#[derive(Debug, Clone)]
pub(crate) struct PlannedTarget {
    /// Repository-relative path of the page.
    pub path: String,
    /// Page title: the existing page's title or the proposal's.
    pub title: String,
    /// Whether the page already existed.
    pub origin: Origin,
    /// Hash of the existing page; `None` for a created page.
    pub before: Option<String>,
    /// Points assigned to this page.
    pub points: Vec<PlanPoint>,
    /// What the page must add or change, one line per instruction.
    pub change: String,
}

/// A target accumulated while assignments and maintenance are grouped.
struct Group {
    path: String,
    title: String,
    origin: Origin,
    before: Option<String>,
    /// Point and its position in the payload, so output follows point order.
    points: Vec<(usize, PlanPoint)>,
    /// Change instruction and the point it belongs to.
    changes: Vec<(usize, String)>,
    /// Fallback instruction when no assignment carried one: a proposal summary
    /// for a created page or the maintenance reason for an existing one.
    base_change: String,
}

impl Group {
    fn add(&mut self, point_index: usize, point: &Point, change: Option<&str>) {
        self.points.push((
            point_index,
            PlanPoint {
                id: point.id.clone(),
                finding: point.finding.clone(),
                evidence: point.evidence.clone(),
                boundary: point.boundary.clone(),
            },
        ));
        if let Some(change) = change.filter(|text| !text.is_empty()) {
            self.changes.push((point_index, change.to_string()));
        }
    }

    fn finish(mut self) -> PlannedTarget {
        self.points.sort_by_key(|(index, _)| *index);
        self.changes.sort_by_key(|(index, _)| *index);
        let change = if self.changes.is_empty() {
            self.base_change
        } else {
            self.changes
                .into_iter()
                .map(|(_, change)| change)
                .collect::<Vec<_>>()
                .join("\n")
        };
        PlannedTarget {
            path: self.path,
            title: self.title,
            origin: self.origin,
            before: self.before,
            points: self.points.into_iter().map(|(_, point)| point).collect(),
            change,
        }
    }
}

/// Turns a validated route into one target per normalized page path.
///
/// Updates are grouped by path, creates by proposal, and a maintenance `update`
/// with no assignment adds its page. A maintenance `unchanged` adds nothing.
/// Targets are returned sorted by path; nothing is capped, because neither the
/// plan nor the design defines a target limit.
pub(crate) fn plan_targets(
    root: &Path,
    cfg: &Config,
    doc: &RouteDoc,
    affected: &[String],
) -> Result<Vec<PlannedTarget>> {
    let point_by_id: BTreeMap<&str, (usize, &Point)> = doc
        .points
        .iter()
        .enumerate()
        .map(|(index, point)| (point.id.as_str(), (index, point)))
        .collect();
    let proposal_by_id: BTreeMap<&str, &Proposal> = doc
        .proposals
        .iter()
        .map(|proposal| (proposal.id.as_str(), proposal))
        .collect();

    let mut groups: Vec<Group> = Vec::new();
    let mut group_slot: BTreeMap<String, usize> = BTreeMap::new();
    let mut taken: BTreeSet<String> = BTreeSet::new();

    for assignment in &doc.assignments {
        let Some((point_index, point)) = point_by_id.get(assignment.point.as_str()) else {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!(
                    "assignment refers to point '{}', which is not in points",
                    assignment.point
                ),
            ));
        };
        match assignment.action.as_str() {
            "update" => {
                let target = assignment.target.as_deref().unwrap_or_default();
                let path = checked_path(cfg, target)?;
                let slot = match group_slot.get(&path) {
                    Some(slot) => *slot,
                    None => {
                        let (title, before) = existing_page(root, &path)?;
                        taken.insert(path.clone());
                        groups.push(Group {
                            path: path.clone(),
                            title,
                            origin: Origin::Existing,
                            before: Some(before),
                            points: Vec::new(),
                            changes: Vec::new(),
                            base_change: String::new(),
                        });
                        group_slot.insert(path, groups.len() - 1);
                        groups.len() - 1
                    }
                };
                groups[slot].add(*point_index, point, assignment.change.as_deref());
            }
            "create" => {
                let proposal_id = assignment.proposal.as_deref().unwrap_or_default();
                let Some(proposal) = proposal_by_id.get(proposal_id) else {
                    return Err(bad(
                        "E_TASK_BAD_PAYLOAD",
                        format!(
                            "assignment refers to proposal '{proposal_id}', which is not in proposals"
                        ),
                    ));
                };
                let key = format!("proposal:{}", proposal.id);
                let slot = match group_slot.get(&key) {
                    Some(slot) => *slot,
                    None => {
                        let path = allocate_path(root, cfg, proposal, &taken)?;
                        taken.insert(path.clone());
                        groups.push(Group {
                            path,
                            title: proposal.title.clone(),
                            origin: Origin::Created,
                            before: None,
                            points: Vec::new(),
                            changes: Vec::new(),
                            base_change: proposal.summary.clone(),
                        });
                        group_slot.insert(key, groups.len() - 1);
                        groups.len() - 1
                    }
                };
                groups[slot].add(*point_index, point, None);
            }
            // `skip` writes nothing; any other action is rejected here too, so a
            // caller that skipped `validate_shape` cannot silently drop a write.
            "skip" => {}
            other => {
                return Err(bad(
                    "E_TASK_BAD_PAYLOAD",
                    format!("unknown assignment action '{other}'"),
                ))
            }
        }
    }

    for item in &doc.maintenance {
        if item.action != "update" {
            continue;
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
        let path = checked_path(cfg, &item.page)?;
        if group_slot.contains_key(&path) {
            continue;
        }
        let (title, before) = existing_page(root, &path)?;
        taken.insert(path.clone());
        groups.push(Group {
            path: path.clone(),
            title,
            origin: Origin::Existing,
            before: Some(before),
            points: Vec::new(),
            changes: Vec::new(),
            base_change: item.reason.clone(),
        });
        group_slot.insert(path, groups.len() - 1);
    }

    let mut planned: Vec<PlannedTarget> = groups.into_iter().map(Group::finish).collect();
    planned.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(planned)
}

/// Lowercase ASCII slug: runs of anything else become one `-`, trimmed; an
/// all-non-ASCII title (Chinese, for example) falls back to `page`.
pub(crate) fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut separator = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            separator = false;
        } else if !separator && !out.is_empty() {
            out.push('-');
            separator = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        String::from("page")
    } else {
        out
    }
}

/// Reads the existing page: its frontmatter title and its current hash.
///
/// A missing page is the agent's own path mistake, not an environment failure:
/// it is reported as a bad payload naming the target, so the fix is to query
/// again rather than to retry. A page that exists but cannot be parsed keeps its
/// own code (`E_BAD_FRONTMATTER`, `E_NO_UPDATED`), because changing the path
/// would not fix it.
fn existing_page(root: &Path, path: &str) -> Result<(String, String)> {
    let full = root.join(path);
    let (front, _body) = match page::read(&full) {
        Ok(page) => page,
        Err(page::PageError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            return Err(bad(
                "E_TASK_BAD_PAYLOAD",
                format!(
                    "target '{path}' does not exist; query again and point at an existing page"
                ),
            ))
        }
        Err(other) => return Err(other.into()),
    };
    let before = hash::hash_file(&full, true)?;
    Ok((front.title, before))
}

/// Allocates `<wiki>/<type>/<slug>.md`, adding `-2`, `-3`, ... until neither the
/// filesystem nor this plan already holds the path.
fn allocate_path(
    root: &Path,
    cfg: &Config,
    proposal: &Proposal,
    taken: &BTreeSet<String>,
) -> Result<String> {
    let slug = slugify(&proposal.title);
    let wiki = cfg.paths.wiki.trim_end_matches('/');
    let mut suffix = 1usize;
    loop {
        let stem = if suffix == 1 {
            slug.clone()
        } else {
            format!("{slug}-{suffix}")
        };
        let path = checked_path(cfg, &format!("{wiki}/{}/{stem}.md", proposal.typ))?;
        if !taken.contains(&path) && !root.join(&path).exists() {
            return Ok(path);
        }
        suffix += 1;
    }
}

/// Normalizes separators and `.` components, then enforces the page-path rule.
fn checked_path(cfg: &Config, raw: &str) -> Result<String> {
    let path = normalize_target(raw)?;
    let wiki = cfg.paths.wiki.trim_end_matches('/');
    let prefix = format!("{wiki}/");
    let relative = path.strip_prefix(&prefix).ok_or_else(|| {
        bad(
            "E_TASK_BAD_PAYLOAD",
            format!("target '{path}' must be under {wiki}/"),
        )
    })?;
    if !relative.ends_with(".md") {
        return Err(bad(
            "E_TASK_BAD_PAYLOAD",
            format!("target '{path}' must be a .md page"),
        ));
    }
    if relative == "index.md" {
        return Err(bad(
            "E_TASK_BAD_PAYLOAD",
            format!("target '{path}' is the generated index, not a content page"),
        ));
    }
    if relative.starts_with("audit/") {
        return Err(bad(
            "E_TASK_BAD_PAYLOAD",
            format!("target '{path}' is under the derived audit/ directory"),
        ));
    }
    Ok(path)
}

/// Repository-relative, `/`-separated and free of `.`/`..` components.
fn normalize_target(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(bad("E_TASK_BAD_PAYLOAD", "a target path is empty"));
    }
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return Err(bad(
            "E_TASK_BAD_PAYLOAD",
            format!("target '{raw}' must be repository-relative"),
        ));
    }
    let mut parts = Vec::new();
    for component in trimmed.split(['/', '\\']) {
        match component {
            "" | "." => continue,
            ".." => {
                return Err(bad(
                    "E_TASK_BAD_PAYLOAD",
                    format!("target '{raw}' contains a parent-directory component"),
                ))
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return Err(bad(
            "E_TASK_BAD_PAYLOAD",
            format!("target '{raw}' has no path components"),
        ));
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use crate::config::Config;

    use super::*;

    fn test_config() -> Config {
        Config::default()
    }

    /// Writes the smallest page the reader accepts, with `title` in its
    /// frontmatter, so plans are checked against real page bytes.
    fn write_page(root: &Path, rel: &str, title: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let id = path.file_stem().unwrap().to_string_lossy();
        let typ = path
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy();
        fs::write(
            &path,
            format!(
                "---\nid: {id}\ntitle: \"{title}\"\ntype: {typ}\nsummary: s\n\
                 status: stable\nupdated: 2026-11-11\n---\n\nbody\n"
            ),
        )
        .unwrap();
    }

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

    #[test]
    fn two_points_on_one_target_make_one_planned_target() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        write_page(d.path(), ".wiki/pages/concept/a.md", "A");
        let doc = parse(
            r#"{"points":[
            {"id":"p1","finding":"一","evidence":"e1"},
            {"id":"p2","finding":"二","evidence":"e2"}],
            "assignments":[
            {"point":"p1","action":"update","target":".wiki/pages/concept/a.md","change":"补一","basis":"b1"},
            {"point":"p2","action":"update","target":".wiki/pages/concept/a.md","change":"补二","basis":"b2"}],
            "maintenance":[{"page":".wiki/pages/concept/a.md","action":"update","reason":"有新增"}]}"#
                .as_bytes(),
        )
        .unwrap();
        let out = plan_targets(d.path(), &cfg, &doc, &[".wiki/pages/concept/a.md".into()]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].points.len(), 2);
    }

    #[test]
    fn a_chinese_title_gets_a_stable_fallback_slug() {
        assert_eq!(slugify("知识库设计"), "page");
        assert_eq!(slugify("A B"), "a-b");
    }

    #[test]
    fn a_collision_adds_a_suffix_instead_of_overwriting() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        write_page(d.path(), ".wiki/pages/concept/alpha.md", "old");
        let doc = parse(br#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
            "proposals":[{"id":"n1","type":"concept","title":"Alpha","summary":"s","why_new":"n","why_not_existing":"e"}],
            "assignments":[{"point":"p1","action":"create","proposal":"n1"}],
            "maintenance":[]}"#).unwrap();
        let out = plan_targets(d.path(), &cfg, &doc, &[]).unwrap();
        assert_eq!(out[0].path, ".wiki/pages/concept/alpha-2.md");
    }

    fn update_assignment(target: &str, change: &str) -> String {
        format!(
            r#"{{"points":[{{"id":"p1","finding":"f","evidence":"e"}}],
                "assignments":[{{"point":"p1","action":"update","target":"{target}",
                    "change":"{change}","basis":"b"}}]}}"#
        )
    }

    #[test]
    fn a_maintenance_update_without_an_assignment_plans_the_page() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        write_page(d.path(), ".wiki/pages/concept/a.md", "A");
        let doc = parse(
            r#"{"maintenance":[{"page":".wiki/pages/concept/a.md","action":"update",
                "reason":"补充结论"}],"source_reason":"只有一条结论"}"#
                .as_bytes(),
        )
        .unwrap();
        let out = plan_targets(d.path(), &cfg, &doc, &[".wiki/pages/concept/a.md".into()]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change, "补充结论");
        assert!(out[0].points.is_empty());
        assert_eq!(out[0].origin, Origin::Existing);
        assert_eq!(out[0].title, "A");
        let written = d.path().join(".wiki/pages/concept/a.md");
        let before = hash::hash_file(&written, true).unwrap();
        assert_eq!(out[0].before.as_deref(), Some(before.as_str()));
    }

    #[test]
    fn a_maintenance_unchanged_plans_nothing() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        write_page(d.path(), ".wiki/pages/concept/a.md", "A");
        let doc = parse(
            r#"{"maintenance":[{"page":".wiki/pages/concept/a.md","action":"unchanged",
                "reason":"看过，不改"}],"source_reason":"没有新增"}"#
                .as_bytes(),
        )
        .unwrap();
        let out = plan_targets(d.path(), &cfg, &doc, &[".wiki/pages/concept/a.md".into()]).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn a_missing_update_target_is_a_bad_payload() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        let doc =
            parse(update_assignment(".wiki/pages/concept/missing.md", "补").as_bytes()).unwrap();
        let err = plan_targets(d.path(), &cfg, &doc, &[]).unwrap_err();
        assert_eq!(err.code(), "E_TASK_BAD_PAYLOAD", "{err}");
        assert!(err.to_string().contains("missing.md"), "{err}");
    }

    #[test]
    fn an_existing_but_unreadable_update_target_keeps_its_own_error() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        let path = d.path().join(".wiki/pages/concept/a.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "no frontmatter here\n").unwrap();
        let doc = parse(update_assignment(".wiki/pages/concept/a.md", "补").as_bytes()).unwrap();
        let err = plan_targets(d.path(), &cfg, &doc, &[]).unwrap_err();
        assert_eq!(err.code(), "E_BAD_FRONTMATTER", "{err}");
    }

    #[test]
    fn an_unknown_assignment_action_is_rejected() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        let doc = parse(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "assignments":[{"point":"p1","action":"Update",
                    "target":".wiki/pages/concept/a.md"}]}"#
                .as_bytes(),
        )
        .unwrap();
        let err = plan_targets(d.path(), &cfg, &doc, &[]).unwrap_err();
        assert_eq!(err.code(), "E_TASK_BAD_PAYLOAD", "{err}");
        assert!(err.to_string().contains("Update"), "{err}");
    }

    #[test]
    fn derived_files_and_non_page_paths_are_rejected() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        for target in [
            "index.md",
            ".wiki/pages/index.md",
            ".wiki/pages/audit/2026-11-11.md",
            ".wiki/pages/concept/a.txt",
            "notes/a.md",
        ] {
            let doc = parse(update_assignment(target, "补").as_bytes()).unwrap();
            let err = plan_targets(d.path(), &cfg, &doc, &[]).unwrap_err();
            assert_eq!(err.code(), "E_TASK_BAD_PAYLOAD", "{target}: {err}");
            assert!(err.to_string().contains(target), "{target}: {err}");
        }
    }

    #[test]
    fn parent_and_absolute_targets_are_rejected() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        for target in [".wiki/pages/../a.md", "/etc/passwd"] {
            let doc = parse(update_assignment(target, "补").as_bytes()).unwrap();
            let err = plan_targets(d.path(), &cfg, &doc, &[]).unwrap_err();
            assert_eq!(err.code(), "E_TASK_BAD_PAYLOAD", "{target}: {err}");
            assert!(err.to_string().contains(target), "{target}: {err}");
        }
    }

    #[test]
    fn a_created_page_under_the_audit_directory_is_rejected() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        let doc = parse(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "proposals":[{"id":"n1","type":"audit","title":"A","summary":"s",
                    "why_new":"n","why_not_existing":"e"}],
                "assignments":[{"point":"p1","action":"create","proposal":"n1"}]}"#
                .as_bytes(),
        )
        .unwrap();
        let err = plan_targets(d.path(), &cfg, &doc, &[]).unwrap_err();
        assert_eq!(err.code(), "E_TASK_BAD_PAYLOAD", "{err}");
    }

    #[test]
    fn a_create_target_uses_the_proposal_and_has_no_before_hash() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        let doc = parse(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"}],
                "proposals":[{"id":"n1","type":"concept","title":"Alpha Beta",
                    "summary":"把结论写清楚","why_new":"n","why_not_existing":"e"}],
                "assignments":[{"point":"p1","action":"create","proposal":"n1"}]}"#
                .as_bytes(),
        )
        .unwrap();
        let out = plan_targets(d.path(), &cfg, &doc, &[]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, ".wiki/pages/concept/alpha-beta.md");
        assert_eq!(out[0].title, "Alpha Beta");
        assert_eq!(out[0].origin, Origin::Created);
        assert_eq!(out[0].before, None);
        assert_eq!(out[0].change, "把结论写清楚");
    }

    #[test]
    fn two_creates_with_one_title_are_numbered() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        let doc = parse(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"},
                {"id":"p2","finding":"g","evidence":"e"}],
                "proposals":[
                {"id":"n1","type":"concept","title":"Alpha","summary":"s1",
                    "why_new":"n","why_not_existing":"e"},
                {"id":"n2","type":"concept","title":"Alpha","summary":"s2",
                    "why_new":"n","why_not_existing":"e"}],
                "assignments":[{"point":"p1","action":"create","proposal":"n1"},
                {"point":"p2","action":"create","proposal":"n2"}]}"#
                .as_bytes(),
        )
        .unwrap();
        let out = plan_targets(d.path(), &cfg, &doc, &[]).unwrap();
        assert_eq!(out.len(), 2);
        let paths: BTreeSet<&str> = out.iter().map(|target| target.path.as_str()).collect();
        assert!(paths.contains(".wiki/pages/concept/alpha.md"), "{paths:?}");
        assert!(
            paths.contains(".wiki/pages/concept/alpha-2.md"),
            "{paths:?}"
        );
    }

    #[test]
    fn changes_and_points_follow_point_order() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        write_page(d.path(), ".wiki/pages/concept/a.md", "A");
        let doc = parse(
            r#"{"points":[
                {"id":"p1","finding":"一","evidence":"e1"},
                {"id":"p2","finding":"二","evidence":"e2"}],
                "assignments":[
                {"point":"p2","action":"update","target":".wiki/pages/concept/a.md",
                    "change":"补二","basis":"b2"},
                {"point":"p1","action":"update","target":".wiki/pages/concept/a.md",
                    "change":"补一","basis":"b1"}],
                "maintenance":[{"page":".wiki/pages/concept/a.md",
                    "action":"update","reason":"有新增"}]}"#
                .as_bytes(),
        )
        .unwrap();
        let out = plan_targets(d.path(), &cfg, &doc, &[".wiki/pages/concept/a.md".into()]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change, "补一\n补二");
        let ids: Vec<&str> = out[0]
            .points
            .iter()
            .map(|point| point.id.as_str())
            .collect();
        assert_eq!(ids, ["p1", "p2"]);
    }

    #[test]
    fn differently_spelled_targets_group_onto_one_path() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        write_page(d.path(), ".wiki/pages/concept/a.md", "A");
        let doc = parse(
            r#"{"points":[
                {"id":"p1","finding":"一","evidence":"e1"},
                {"id":"p2","finding":"二","evidence":"e2"}],
                "assignments":[
                {"point":"p1","action":"update","target":"./.wiki/pages/concept/a.md",
                    "change":"补一","basis":"b1"},
                {"point":"p2","action":"update","target":".wiki/pages/concept//a.md",
                    "change":"补二","basis":"b2"}],
                "maintenance":[{"page":".wiki/pages/concept/a.md",
                    "action":"update","reason":"有新增"}]}"#
                .as_bytes(),
        )
        .unwrap();
        let out = plan_targets(d.path(), &cfg, &doc, &[".wiki/pages/concept/a.md".into()]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, ".wiki/pages/concept/a.md");
        assert_eq!(out[0].points.len(), 2);
    }

    #[test]
    fn planned_targets_are_sorted_by_path() {
        let d = tempdir().unwrap();
        let cfg = test_config();
        write_page(d.path(), ".wiki/pages/concept/a.md", "A");
        let doc = parse(
            r#"{"points":[{"id":"p1","finding":"f","evidence":"e"},
                {"id":"p2","finding":"g","evidence":"e"}],
                "proposals":[{"id":"n1","type":"concept","title":"Beta",
                    "summary":"s","why_new":"n","why_not_existing":"e"}],
                "assignments":[
                {"point":"p1","action":"update","target":".wiki/pages/concept/a.md",
                    "change":"补","basis":"b"},
                {"point":"p2","action":"create","proposal":"n1"}],
                "maintenance":[{"page":".wiki/pages/concept/a.md",
                    "action":"update","reason":"有新增"}]}"#
                .as_bytes(),
        )
        .unwrap();
        let out = plan_targets(d.path(), &cfg, &doc, &[".wiki/pages/concept/a.md".into()]).unwrap();
        let paths: Vec<&str> = out.iter().map(|target| target.path.as_str()).collect();
        assert_eq!(
            paths,
            [".wiki/pages/concept/a.md", ".wiki/pages/concept/beta.md"]
        );
    }

    #[test]
    fn slugify_collapses_and_trims_separators() {
        assert_eq!(slugify("Hello,  World!"), "hello-world");
        assert_eq!(slugify("---"), "page");
        assert_eq!(slugify("  A  "), "a");
    }
}
