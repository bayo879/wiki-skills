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

/// A repository with one source already settled into one page.
fn one_source_repo() -> TempDir {
    let dir = single_source();
    let route = next_task(&dir);
    close_route(&dir, &route);
    let body = close_alpha_page(&dir, "Alpha body text\n");
    assert!(body["next_task"].is_null(), "{body}");
    dir
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
    let (code, status) = run(&dir, &["task", "status"]);
    assert_eq!(code, 0, "{status}");
    assert!(
        status["roots"].as_array().expect("roots").is_empty(),
        "{status}"
    );
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
fn a_page_edited_outside_the_task_is_refused() {
    let dir = single_source();
    let route = next_task(&dir);
    close_route(&dir, &route);
    let page = next_task(&dir);
    let page_id = task_id(&page);

    let stray = dir.path().join(".wiki/pages/concept/stray.md");
    std::fs::create_dir_all(stray.parent().unwrap()).unwrap();
    std::fs::write(
        &stray,
        "---\nid: stray\ntype: concept\ntitle: Stray\nsources:\n  - .wiki/raw/a.md\n---\n\nout of band\n",
    )
    .unwrap();
    let payload = payload(&dir);
    let (code, body) = run(
        &dir,
        &["task", "done", &page_id.to_string(), "--payload", &payload],
    );
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_PAGE_CONFLICT");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("stray.md"),
        "the offending file is named: {body}"
    );

    // The untouched tree is still there: removing the stray page unblocks it.
    std::fs::remove_file(&stray).unwrap();
    let (code, body) = run(
        &dir,
        &["task", "done", &page_id.to_string(), "--payload", &payload],
    );
    assert_eq!(code, 0, "{body}");
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
fn leaves_is_rejected_and_a_route_needs_a_payload() {
    let dir = repo();
    let route = next_task(&dir);
    let id = task_id(&route);
    let leaves = temp_json(
        &dir,
        "leaves.json",
        &json!({"leaves":[{"type":"concept","id":"alpha","title":"Alpha","brief":"写结论"}]})
            .to_string(),
    );
    let (code, body) = run(
        &dir,
        &["task", "done", &id.to_string(), "--leaves", &leaves],
    );
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_TASK_BAD_PAYLOAD");

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
    // Asking twice changes nothing.
    let (_, again) = run(&dir, &["task", "status"]);
    assert_eq!(again["roots"], body["roots"]);
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
