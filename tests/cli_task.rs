//! The task tree: a source is routed, then each target is written, then the
//! root settles.
use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;

fn run(dir: &TempDir, args: &[&str]) -> (i32, Value) {
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .current_dir(dir.path())
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    let data = match value.get("data") {
        Some(data) => data.clone(),
        None => value,
    };
    (out.status.code().unwrap(), data)
}

fn repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    llm_wiki::init::run(dir.path(), false).unwrap();
    std::fs::write(dir.path().join("a.md"), "# A\n\nEvidence A\n").unwrap();
    std::fs::write(dir.path().join("b.md"), "# B\n\nEvidence B\n").unwrap();
    let (code, body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(code, 0, "{body}");
    dir
}

/// A repository with only `a.md`, already ingested but not routed.
fn single_source() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    llm_wiki::init::run(dir.path(), false).unwrap();
    std::fs::write(dir.path().join("a.md"), "# A\n\nEvidence A\n").unwrap();
    let (code, body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(code, 0, "{body}");
    dir
}

/// Writes a JSON payload inside the engine, where no round will treat it as a
/// source.
fn temp_json(dir: &TempDir, name: &str, body: &str) -> String {
    let path = dir.path().join(".wiki").join(name);
    std::fs::write(&path, body).unwrap();
    path.to_string_lossy().into_owned()
}

fn next_task(dir: &TempDir) -> Value {
    let (code, body) = run(dir, &["ingest"]);
    assert_eq!(code, 0, "{body}");
    body["next_task"].clone()
}

fn task_id(task: &Value) -> i64 {
    task["id"].as_i64().unwrap()
}

/// A route that creates `concept/alpha.md` from the current source.
fn route_create_alpha(dir: &TempDir) -> String {
    temp_json(
        dir,
        "route.json",
        &json!({
            "points": [{"id": "p1", "finding": "A 的结论", "evidence": "a.md"}],
            "assignments": [{"point": "p1", "action": "create", "proposal": "n1"}],
            "proposals": [{
                "id": "n1",
                "type": "concept",
                "title": "Alpha",
                "summary": "A conclusion",
                "why_new": "a.md 的结论自成一页",
                "why_not_existing": "库里还没有这一页",
            }],
            "maintenance": [],
            "searches": [{"query": "Alpha", "candidates": [], "reason": "没有已有页可承载"}],
        })
        .to_string(),
    )
}

fn page_payload(dir: &TempDir, name: &str, title: &str, body: &str) -> String {
    temp_json(
        dir,
        name,
        &json!({
            "title": title,
            "summary": "A conclusion",
            "tags": ["t"],
            "status": "draft",
            "body": body,
        })
        .to_string(),
    )
}

fn payload(dir: &TempDir) -> String {
    page_payload(dir, "payload.json", "Alpha", "Alpha body text\n")
}

fn close_route(dir: &TempDir, task: &Value) -> Value {
    let route = route_create_alpha(dir);
    let (code, body) = run(
        dir,
        &[
            "task",
            "done",
            &task_id(task).to_string(),
            "--payload",
            &route,
        ],
    );
    assert_eq!(code, 0, "{body}");
    body
}

fn close_alpha_page(dir: &TempDir, body: &str) -> Value {
    let task = next_task(dir);
    assert_eq!(task["kind"], "page", "{task}");
    let payload = page_payload(dir, "payload.json", "Alpha", body);
    let (code, response) = run(
        dir,
        &[
            "task",
            "done",
            &task_id(&task).to_string(),
            "--payload",
            &payload,
        ],
    );
    assert_eq!(code, 0, "{response}");
    response
}

/// A route that updates `concept/alpha.md` from the current source.
fn route_update_alpha(dir: &TempDir) -> String {
    temp_json(
        dir,
        "route-update.json",
        &json!({
            "points": [{"id": "p1", "finding": "B 补充 Alpha", "evidence": "b.md"}],
            "assignments": [{
                "point": "p1",
                "action": "update",
                "target": ".wiki/pages/concept/alpha.md",
                "change": "补 Beta 一节",
                "basis": "b.md",
            }],
            "maintenance": [],
            "proposals": [],
            "searches": [],
        })
        .to_string(),
    )
}

/// A repository with one source already settled into one page.
fn one_source_repo() -> TempDir {
    let dir = single_source();
    let route = next_task(&dir);
    close_route(&dir, &route);
    let body = close_alpha_page(&dir, "Alpha body text\n");
    assert!(body["next_task"].is_null(), "{body}");
    dir
}

/// `a.md` is settled into `concept/alpha.md`; `b.md` still waits for its route.
fn settled_alpha_with_b_pending() -> TempDir {
    let dir = repo();
    let route = next_task(&dir);
    close_route(&dir, &route);
    close_alpha_page(&dir, "Alpha body text\n");
    dir
}

/// Closes `b.md`'s route as an update of `alpha` and returns its page task.
fn deliver_update_alpha_page(dir: &TempDir) -> Value {
    let route = next_task(dir);
    assert_eq!(route["kind"], "route", "{route}");
    assert_eq!(route["source"]["key"], "b.md", "{route}");
    let doc = route_update_alpha(dir);
    let (code, body) = run(
        dir,
        &[
            "task",
            "done",
            &task_id(&route).to_string(),
            "--payload",
            &doc,
        ],
    );
    assert_eq!(code, 0, "{body}");
    let page = next_task(dir);
    assert_eq!(page["kind"], "page", "{page}");
    assert_eq!(page["stage"], "writing", "{page}");
    assert!(
        page["target"]["before"].is_string(),
        "a page view carries the snapshot the route took: {page}"
    );
    assert!(
        page["guide"]["goal"]
            .as_str()
            .is_some_and(|goal| !goal.is_empty()),
        "{page}"
    );
    assert!(
        page["contract"]["fields"].as_array().unwrap().len() >= 5,
        "{page}"
    );
    page
}

#[test]
fn a_source_is_routed_before_any_page() {
    let dir = repo();
    let task = next_task(&dir);
    assert_eq!(task["kind"], "route", "{task}");
    assert_eq!(task["source"]["key"], "a.md", "{task}");
    assert!(task["target"].is_null(), "{task}");
    assert_eq!(task["submit"]["schema"], "route-v1", "{task}");
    assert!(
        task["affected_pages"]
            .as_array()
            .expect("affected pages")
            .is_empty(),
        "{task}"
    );
}

#[test]
fn route_update_and_create_make_one_page_per_target() {
    let dir = repo();
    // Settle a.md into concept/alpha.md first.
    let route = next_task(&dir);
    close_route(&dir, &route);
    close_alpha_page(&dir, "Alpha body text\n");

    // b.md both updates alpha and creates beta.
    let route = next_task(&dir);
    assert_eq!(route["kind"], "route", "{route}");
    assert_eq!(route["source"]["key"], "b.md", "{route}");
    let doc = temp_json(
        &dir,
        "route.json",
        &json!({
            "points": [
                {"id": "p1", "finding": "补一段 Beta", "evidence": "b.md"},
                {"id": "p2", "finding": "Beta 自成一页", "evidence": "b.md"},
            ],
            "assignments": [
                {
                    "point": "p1",
                    "action": "update",
                    "target": ".wiki/pages/concept/alpha.md",
                    "change": "补 Beta 一节",
                    "basis": "b.md",
                },
                {"point": "p2", "action": "create", "proposal": "n2"},
            ],
            "proposals": [{
                "id": "n2",
                "type": "concept",
                "title": "Beta",
                "summary": "B conclusion",
                "why_new": "b.md 的结论另立一页",
                "why_not_existing": "alpha 只讲 A",
            }],
            "maintenance": [],
            "searches": [{"query": "Beta", "candidates": [], "reason": "没有已有页可承载"}],
        })
        .to_string(),
    );
    let (code, body) = run(
        &dir,
        &[
            "task",
            "done",
            &task_id(&route).to_string(),
            "--payload",
            &doc,
        ],
    );
    assert_eq!(code, 0, "{body}");

    let first = next_task(&dir);
    assert_eq!(first["kind"], "page", "{first}");
    assert!(
        first["target"]["reference"]
            .as_str()
            .unwrap()
            .ends_with("concept/alpha.md"),
        "{first}"
    );
    let alpha_payload = page_payload(
        &dir,
        "alpha.json",
        "Alpha",
        "Alpha body text\n\nBeta part\n",
    );
    let (code, body) = run(
        &dir,
        &[
            "task",
            "done",
            &task_id(&first).to_string(),
            "--payload",
            &alpha_payload,
        ],
    );
    assert_eq!(code, 0, "{body}");

    let second = next_task(&dir);
    assert_eq!(second["kind"], "page", "{second}");
    assert!(
        second["target"]["reference"]
            .as_str()
            .unwrap()
            .ends_with("concept/beta.md"),
        "{second}"
    );
    let beta_payload = page_payload(&dir, "beta.json", "Beta", "Beta body\n");
    let (code, body) = run(
        &dir,
        &[
            "task",
            "done",
            &task_id(&second).to_string(),
            "--payload",
            &beta_payload,
        ],
    );
    assert_eq!(code, 0, "{body}");
    assert!(body["next_task"].is_null(), "{body}");

    let alpha = std::fs::read_to_string(dir.path().join(".wiki/pages/concept/alpha.md")).unwrap();
    assert!(alpha.contains("Beta part"), "{alpha}");
    assert!(dir.path().join(".wiki/pages/concept/beta.md").exists());
}

#[test]
fn the_last_page_settles_the_source() {
    let dir = single_source();
    let route = next_task(&dir);
    close_route(&dir, &route);

    let page = next_task(&dir);
    assert_eq!(page["kind"], "page", "{page}");
    let leaf = page_payload(&dir, "payload.json", "Alpha", "完整正文\n");
    let (code, body) = run(
        &dir,
        &[
            "task",
            "done",
            &task_id(&page).to_string(),
            "--payload",
            &leaf,
        ],
    );
    assert_eq!(code, 0, "{body}");
    assert!(body["next_task"].is_null(), "{body}");

    assert!(dir.path().join(".wiki/pages/concept/alpha.md").exists());
    assert!(dir.path().join(".wiki/raw/a.md").exists());
    let merged = std::fs::read_to_string(dir.path().join(".wiki/pages/concept/alpha.md")).unwrap();
    assert!(
        merged.contains(".wiki/raw/a.md"),
        "the merged page cites the source it was written from: {merged}"
    );
    let (code, status) = run(&dir, &["task", "status"]);
    assert_eq!(code, 0, "{status}");
    assert!(
        status["roots"].as_array().expect("roots").is_empty(),
        "{status}"
    );
}

#[test]
fn a_target_changed_after_routing_is_refused() {
    let dir = settled_alpha_with_b_pending();
    let page = deliver_update_alpha_page(&dir);
    let alpha = dir.path().join(".wiki/pages/concept/alpha.md");
    let mut text = std::fs::read_to_string(&alpha).unwrap();
    text.push_str("\nchanged outside\n");
    std::fs::write(&alpha, text).unwrap();

    let payload = page_payload(&dir, "payload.json", "Alpha", "新正文\n");
    let (code, body) = run(
        &dir,
        &[
            "task",
            "done",
            &task_id(&page).to_string(),
            "--payload",
            &payload,
        ],
    );
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_PAGE_CONFLICT");
    assert!(
        !dir.path()
            .join(".wiki/cache/staging/b.md/concept/alpha.md")
            .exists(),
        "a refused page leaves no staging file"
    );
    assert_eq!(task_id(&next_task(&dir)), task_id(&page));
}

#[test]
fn a_changed_raw_is_refused_before_staging() {
    let dir = settled_alpha_with_b_pending();
    let page = deliver_update_alpha_page(&dir);
    std::fs::write(dir.path().join(".wiki/raw/b.md"), "tampered").unwrap();

    let payload = page_payload(&dir, "payload.json", "Alpha", "新正文\n");
    let (code, body) = run(
        &dir,
        &[
            "task",
            "done",
            &task_id(&page).to_string(),
            "--payload",
            &payload,
        ],
    );
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_SOURCE_DRIFT");
    assert!(
        !dir.path()
            .join(".wiki/cache/staging/b.md/concept/alpha.md")
            .exists(),
        "drift is reported before staging"
    );
}

#[test]
fn an_unrelated_page_change_does_not_block_the_page() {
    let dir = settled_alpha_with_b_pending();
    let page = deliver_update_alpha_page(&dir);
    let gamma = dir.path().join(".wiki/pages/concept/gamma.md");
    std::fs::write(&gamma, "无关改动").unwrap();

    let payload = page_payload(&dir, "payload.json", "Alpha", "新正文\n");
    let (code, body) = run(
        &dir,
        &[
            "task",
            "done",
            &task_id(&page).to_string(),
            "--payload",
            &payload,
        ],
    );
    assert_eq!(code, 0, "{body}");
    assert!(body["next_task"].is_null(), "{body}");
    assert_eq!(std::fs::read_to_string(&gamma).unwrap(), "无关改动");
}

#[test]
fn a_route_that_writes_nothing_settles_the_source() {
    let dir = single_source();
    let route = next_task(&dir);
    let doc = temp_json(
        &dir,
        "empty-route.json",
        &json!({
            "points": [],
            "assignments": [],
            "proposals": [],
            "maintenance": [],
            "searches": [],
            "source_reason": "已有页覆盖，不需要改动",
        })
        .to_string(),
    );
    let (code, body) = run(
        &dir,
        &[
            "task",
            "done",
            &task_id(&route).to_string(),
            "--payload",
            &doc,
        ],
    );
    assert_eq!(code, 0, "{body}");
    assert!(body["next_task"].is_null(), "{body}");
    let (code, status) = run(&dir, &["task", "status"]);
    assert_eq!(code, 0, "{status}");
    assert!(
        status["roots"].as_array().expect("roots").is_empty(),
        "{status}"
    );
    assert!(!dir.path().join(".wiki/pages/concept/alpha.md").exists());
}

#[test]
fn a_page_with_an_empty_body_is_refused() {
    let dir = single_source();
    let route = next_task(&dir);
    close_route(&dir, &route);
    let page = next_task(&dir);
    let page_id = task_id(&page);
    let bad = page_payload(&dir, "empty.json", "Alpha", "  \n\t\n");
    let (code, body) = run(
        &dir,
        &["task", "done", &page_id.to_string(), "--payload", &bad],
    );
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_TASK_BAD_PAYLOAD");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("body is empty"),
        "{body}"
    );
    assert!(!dir.path().join(".wiki/pages/concept/alpha.md").exists());
    assert!(!dir
        .path()
        .join(".wiki/cache/staging/a.md/concept/alpha.md")
        .exists());

    // The task is still open: a real body closes it.
    let again = next_task(&dir);
    assert_eq!(task_id(&again), page_id, "{again}");
    let good = payload(&dir);
    let (code, body) = run(
        &dir,
        &["task", "done", &page_id.to_string(), "--payload", &good],
    );
    assert_eq!(code, 0, "{body}");
    assert!(body["next_task"].is_null(), "{body}");
}

#[test]
fn task_done_obeys_the_repository_lock() {
    let dir = single_source();
    let route = next_task(&dir);
    let id = task_id(&route);
    let doc = route_create_alpha(&dir);
    let _lock = llm_wiki::lock::acquire(dir.path()).unwrap();
    let (code, body) = run(&dir, &["task", "done", &id.to_string(), "--payload", &doc]);
    assert_eq!(code, 4, "{body}");
    assert_eq!(body["error"]["code"], "E_LOCKED");
}

#[test]
fn an_unknown_task_is_reported() {
    let dir = repo();
    let (code, body) = run(&dir, &["task", "done", "9999"]);
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_NO_TASK");
    let (code, body) = run(&dir, &["task", "retry", "9999"]);
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_NO_TASK");
}

#[test]
fn leaves_is_gone() {
    let dir = repo();
    let route = next_task(&dir);
    let id = task_id(&route);
    let (code, body) = run(
        &dir,
        &["task", "done", &id.to_string(), "--leaves", "x.json"],
    );
    assert_eq!(code, 2, "{body}");
    assert_eq!(body["error"]["code"], "E_USAGE");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--leaves"),
        "the usage error names the retired flag: {body}"
    );
}

#[test]
fn a_route_needs_a_payload() {
    let dir = repo();
    let route = next_task(&dir);
    let id = task_id(&route);
    let (code, body) = run(&dir, &["task", "done", &id.to_string()]);
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_TASK_BAD_PAYLOAD");
}

#[test]
fn status_reports_the_tree_without_advancing_it() {
    let dir = repo();
    let task = next_task(&dir);
    let (code, body) = run(&dir, &["task", "status"]);
    assert_eq!(code, 0, "{body}");
    let roots = body["roots"].as_array().expect("root list");
    assert!(
        roots.iter().any(|root| root["leaves"]
            .as_array()
            .map(|leaves| leaves.iter().any(|leaf| leaf["id"] == task["id"]))
            .unwrap_or(false)),
        "status shows the open route: {body}"
    );
    assert_eq!(
        body["current"], task,
        "status shows the same view ingest delivered: {body}"
    );
    // Asking twice changes nothing.
    let (_, again) = run(&dir, &["task", "status"]);
    assert_eq!(again["roots"], body["roots"]);
}

#[test]
fn ingest_and_status_return_the_same_task_view() {
    let dir = repo();
    let (code, ingest) = run(&dir, &["ingest"]);
    assert_eq!(code, 0, "{ingest}");
    let (code, status) = run(&dir, &["task", "status"]);
    assert_eq!(code, 0, "{status}");
    assert_eq!(ingest["next_task"], status["current"]);
    assert_eq!(
        ingest["next_task"]["guide"]["goal"], status["current"]["guide"]["goal"],
        "both surfaces explain the task the same way"
    );
    assert!(
        ingest["next_task"]["contract"]["fields"]
            .as_array()
            .unwrap()
            .len()
            > 5,
        "the route contract lists every field: {ingest}"
    );
}

#[test]
fn status_shows_stage_and_open_children() {
    let dir = repo();
    let (code, status) = run(&dir, &["task", "status"]);
    assert_eq!(code, 0, "{status}");
    assert_eq!(status["roots"][0]["stage"], "routing", "{status}");
    assert_eq!(status["roots"][0]["open_children"], 1, "{status}");
}

#[test]
fn a_page_edited_outside_the_tree_is_queued_for_review() {
    let dir = one_source_repo();
    let page = dir.path().join(".wiki/pages/concept/alpha.md");
    let mut text = std::fs::read_to_string(&page).unwrap();
    text.push_str("\nhand edit\n");
    std::fs::write(&page, text).unwrap();

    let task = next_task(&dir);
    assert_eq!(task["kind"], "reconcile", "{task}");
    assert_eq!(task["stage"], "reconcile", "{task}");
    assert!(
        task["guide"]["goal"].is_string() && task["contract"]["fields"].is_array(),
        "a reconcile view explains itself too: {task}"
    );
    assert!(
        task["target"]["reference"]
            .as_str()
            .unwrap()
            .ends_with("concept/alpha.md"),
        "{task}"
    );
    let (code, body) = run(
        &dir,
        &["task", "done", &task_id(&task).to_string(), "--accept"],
    );
    assert_eq!(code, 0, "{body}");
    assert!(body["next_task"].is_null(), "{body}");
}

#[test]
fn regenerating_review_puts_the_source_back_in_the_queue() {
    let dir = one_source_repo();
    let page = dir.path().join(".wiki/pages/concept/alpha.md");
    let mut text = std::fs::read_to_string(&page).unwrap();
    text.push_str("\nhand edit\n");
    std::fs::write(&page, text).unwrap();

    let task = next_task(&dir);
    assert_eq!(task["kind"], "reconcile", "{task}");
    let (code, body) = run(
        &dir,
        &["task", "done", &task_id(&task).to_string(), "--regenerate"],
    );
    assert_eq!(code, 0, "{body}");
    let next = body["next_task"].clone();
    assert_eq!(next["kind"], "route", "{next}");
    assert_eq!(next["source"]["key"], "a.md", "{next}");
}

#[test]
fn a_review_needs_exactly_one_verdict() {
    let dir = one_source_repo();
    let page = dir.path().join(".wiki/pages/concept/alpha.md");
    let mut text = std::fs::read_to_string(&page).unwrap();
    text.push_str("\nhand edit\n");
    std::fs::write(&page, text).unwrap();

    let task = next_task(&dir);
    assert_eq!(task["kind"], "reconcile", "{task}");
    let (code, body) = run(&dir, &["task", "done", &task_id(&task).to_string()]);
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_TASK_BAD_PAYLOAD");
}

#[test]
fn a_page_without_sources_is_absorbed_not_reviewed() {
    let dir = one_source_repo();
    std::fs::write(
        dir.path().join(".wiki/pages/concept/notes.md"),
        "---\nid: notes\ntype: concept\ntitle: Notes\nupdated: 2026-01-01\n---\n\nno sources\n",
    )
    .unwrap();
    let task = next_task(&dir);
    assert!(task.is_null(), "{task}");
}

#[test]
fn replan_rebuilds_a_roots_route_from_the_cli() {
    let dir = single_source();
    let (code, status) = run(&dir, &["task", "status"]);
    assert_eq!(code, 0, "{status}");
    let root_id = status["roots"][0]["id"].as_i64().unwrap();

    let (code, body) = run(&dir, &["task", "retry", &root_id.to_string(), "--replan"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["kind"], "breakdown", "{body}");
    assert_eq!(body["status"], "active", "{body}");
    assert_eq!(body["last_error"], Value::Null, "{body}");

    let next = next_task(&dir);
    assert_eq!(next["kind"], "route", "{next}");
    assert_eq!(next["source"]["key"], "a.md", "{next}");
}

#[test]
fn replan_refuses_a_non_root_task() {
    let dir = single_source();
    let route = next_task(&dir);
    assert_eq!(route["kind"], "route", "{route}");
    let (code, body) = run(
        &dir,
        &["task", "retry", &task_id(&route).to_string(), "--replan"],
    );
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_NO_TASK", "{body}");
    assert_eq!(route["kind"], "route", "the route is left alone");
}

#[test]
fn an_accepted_review_still_makes_the_old_page_plan_conflict() {
    let dir = settled_alpha_with_b_pending();
    let page = deliver_update_alpha_page(&dir);
    let path = dir.path().join(".wiki/pages/concept/alpha.md");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\nchanged outside\n");
    std::fs::write(&path, &text).unwrap();

    // `task status` is read-only; an `ingest` is what scans for authored edits.
    let page_task = next_task(&dir);
    assert_eq!(page_task["kind"], "page", "{page_task}");
    let (code, status) = run(&dir, &["task", "status"]);
    assert_eq!(code, 0, "{status}");
    let reconcile = status["reconcile"][0]["id"].as_i64().unwrap();
    let (code, body) = run(&dir, &["task", "done", &reconcile.to_string(), "--accept"]);
    assert_eq!(code, 0, "{body}");

    let (code, body) = run(
        &dir,
        &[
            "task",
            "done",
            &task_id(&page).to_string(),
            "--payload",
            &payload(&dir),
        ],
    );
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_PAGE_CONFLICT", "{body}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        text,
        "an accepted review does not bless the stale page plan"
    );
}

/// The id of the only root in the tree.
fn root_id(dir: &TempDir) -> i64 {
    let (code, status) = run(dir, &["task", "status"]);
    assert_eq!(code, 0, "{status}");
    status["roots"][0]["id"].as_i64().unwrap()
}

#[test]
fn a_conversion_that_changes_the_raw_blocks_the_open_root() {
    let dir = single_source();
    let root = root_id(&dir);
    let stale_route = next_task(&dir);
    assert_eq!(stale_route["kind"], "route", "{stale_route}");

    // The source is converted again while the root still has not routed it.
    std::fs::write(dir.path().join("a.md"), "# A\n\nEvidence A, revised\n").unwrap();
    let (code, body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(code, 0, "{body}");

    let (code, status) = run(&dir, &["task", "status"]);
    assert_eq!(code, 0, "{status}");
    let root_view = &status["roots"][0];
    assert_eq!(root_view["status"], "blocked", "{status}");
    let reason = root_view["last_error"].as_str().unwrap();
    assert!(reason.contains("E_SOURCE_DRIFT"), "{reason}");
    assert!(
        reason.contains(&format!("task retry {root} --replan")),
        "the reason must name the command that fixes it: {reason}"
    );
    assert_eq!(
        root_view["leaves"].as_array().unwrap().len(),
        1,
        "a blocked root keeps its children until a replan"
    );

    // The stale route is neither delivered nor accepted.
    let (code, body) = run(&dir, &["ingest"]);
    assert_eq!(code, 0, "{body}");
    assert!(body["next_task"].is_null(), "{body}");

    let route = route_create_alpha(&dir);
    let (code, body) = run(
        &dir,
        &[
            "task",
            "done",
            &task_id(&stale_route).to_string(),
            "--payload",
            &route,
        ],
    );
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_SOURCE_DRIFT", "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&root.to_string()),
        "the refusal must name the root: {body}"
    );
}

#[test]
fn a_replan_after_drift_routes_the_new_raw() {
    let dir = single_source();
    let root = root_id(&dir);
    let stale_route = next_task(&dir);

    std::fs::write(dir.path().join("a.md"), "# A\n\nEvidence A, revised\n").unwrap();
    let (code, body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(code, 0, "{body}");

    let (code, body) = run(&dir, &["task", "retry", &root.to_string(), "--replan"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["status"], "active", "{body}");

    let next = next_task(&dir);
    assert_eq!(next["kind"], "route", "{next}");
    assert_eq!(next["source"]["key"], "a.md", "{next}");
    assert_ne!(
        task_id(&next),
        task_id(&stale_route),
        "the old route is gone"
    );

    // The discarded route id is not a task any more.
    let route = route_create_alpha(&dir);
    let (code, body) = run(
        &dir,
        &[
            "task",
            "done",
            &task_id(&stale_route).to_string(),
            "--payload",
            &route,
        ],
    );
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_NO_TASK", "{body}");
}

#[test]
fn a_blocked_root_does_not_stop_another_source() {
    let dir = tempfile::tempdir().unwrap();
    llm_wiki::init::run(dir.path(), false).unwrap();
    std::fs::write(dir.path().join("a.md"), "# A\n\nEvidence A\n").unwrap();
    std::fs::write(dir.path().join("b.md"), "# B\n\nEvidence B\n").unwrap();
    let (code, body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(code, 0, "{body}");
    let first = next_task(&dir);
    assert_eq!(first["source"]["key"], "a.md", "{first}");

    std::fs::write(dir.path().join("a.md"), "# A\n\nEvidence A, revised\n").unwrap();
    let (code, body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(code, 0, "{body}");

    let next = next_task(&dir);
    assert_eq!(
        next["source"]["key"], "b.md",
        "the open source must still be routed: {next}"
    );
}

#[test]
fn a_refused_conversion_leaves_the_open_root_open() {
    let dir = single_source();
    let root = root_id(&dir);
    // A source whose body is only frontmatter is refused before conversion, so
    // no raw is written and no root is touched.
    std::fs::write(dir.path().join("c.md"), "---\ntitle: nothing\n---\n").unwrap();
    let (code, body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(
        code, 1,
        "a refused conversion is reported as a failure: {body}"
    );
    assert!(
        body["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("c.md")),
        "{body}"
    );

    let (code, status) = run(&dir, &["task", "status"]);
    assert_eq!(code, 0, "{status}");
    let root_view = &status["roots"][0];
    assert_eq!(root_view["id"].as_i64().unwrap(), root);
    assert_ne!(root_view["status"], "blocked", "{status}");
}
