//! The task tree: advancing ingest is one close at a time.
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

fn leaves(dir: &TempDir, leaf: &str) -> String {
    temp_json(
        dir,
        "leaves.json",
        &json!({"leaves":[{"type":"concept","id":"alpha","title":"Alpha","brief":leaf}]})
            .to_string(),
    )
}

fn payload(dir: &TempDir) -> String {
    temp_json(
        dir,
        "payload.json",
        &json!({
            "title": "Alpha",
            "summary": "A conclusion",
            "tags": ["t"],
            "status": "draft",
            "body": "Alpha body text\n",
        })
        .to_string(),
    )
}

#[test]
fn a_breakdown_hands_out_one_page_leaf() {
    let dir = repo();
    let task = next_task(&dir);
    assert_eq!(task["kind"], "breakdown");
    let id = task["id"].as_i64().unwrap();
    let leaves = leaves(&dir, "写 A 的结论");
    let (code, body) = run(
        &dir,
        &["task", "done", &id.to_string(), "--leaves", &leaves],
    );
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["next_task"]["kind"], "page");
    assert!(
        body["next_task"]["target_path"]
            .as_str()
            .unwrap()
            .ends_with("concept/alpha.md"),
        "{body}"
    );
}

#[test]
fn the_last_leaf_merges_the_batch_and_clears_the_queue() {
    let dir = repo();
    let root = next_task(&dir);
    let root_id = root["id"].as_i64().unwrap();
    let leaves = leaves(&dir, "写 A 的结论");
    let (_, after) = run(
        &dir,
        &["task", "done", &root_id.to_string(), "--leaves", &leaves],
    );
    let leaf_id = after["next_task"]["id"].as_i64().unwrap();
    let payload = payload(&dir);
    let (code, body) = run(
        &dir,
        &["task", "done", &leaf_id.to_string(), "--payload", &payload],
    );
    assert_eq!(code, 0, "{body}");
    // The other source still has a root, so the tool hands that one out next.
    assert_eq!(body["next_task"]["source_key"], "b.md", "{body}");

    let page = dir.path().join(".wiki/pages/concept/alpha.md");
    let text = std::fs::read_to_string(&page).unwrap();
    assert!(text.contains("Alpha body text"), "{text}");
    assert!(
        text.contains(".wiki/raw/a.md"),
        "the tool fills sources: {text}"
    );

    let (_, again) = run(&dir, &["ingest"]);
    assert!(
        !again["pending"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["source_key"] == "a.md"),
        "closing the tree clears its queue entry: {again}"
    );
}

#[test]
fn a_page_edited_outside_the_task_is_refused() {
    let dir = repo();
    let root = next_task(&dir);
    let root_id = root["id"].as_i64().unwrap();
    let leaves = leaves(&dir, "写 A 的结论");
    let (_, after) = run(
        &dir,
        &["task", "done", &root_id.to_string(), "--leaves", &leaves],
    );
    let leaf_id = after["next_task"]["id"].as_i64().unwrap();

    let stray = dir.path().join(".wiki/pages/concept/stray.md");
    std::fs::create_dir_all(stray.parent().unwrap()).unwrap();
    std::fs::write(
        &stray,
        "---\nid: stray\ntitle: Stray\ntype: concept\nsummary: x\nstatus: draft\nupdated: 2026-01-01\nsources: [\".wiki/raw/a.md\"]\n---\nstray\n",
    )
    .unwrap();

    let payload = payload(&dir);
    let (code, body) = run(
        &dir,
        &["task", "done", &leaf_id.to_string(), "--payload", &payload],
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
        &["task", "done", &leaf_id.to_string(), "--payload", &payload],
    );
    assert_eq!(code, 0, "{body}");
}

#[test]
fn task_done_obeys_the_repository_lock() {
    let dir = repo();
    let root = next_task(&dir);
    let root_id = root["id"].as_i64().unwrap();
    let leaves = leaves(&dir, "写 A 的结论");
    let _lock = llm_wiki::lock::acquire(dir.path()).unwrap();
    let (code, body) = run(
        &dir,
        &["task", "done", &root_id.to_string(), "--leaves", &leaves],
    );
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
fn a_breakdown_without_leaves_is_a_usage_error() {
    let dir = repo();
    let root = next_task(&dir);
    let root_id = root["id"].as_i64().unwrap();
    let (code, body) = run(&dir, &["task", "done", &root_id.to_string()]);
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
        roots.iter().any(|row| row["id"] == task["id"]),
        "status shows the open root: {body}"
    );
    // Asking twice changes nothing.
    let (_, again) = run(&dir, &["task", "status"]);
    assert_eq!(again["roots"], body["roots"]);
}

/// A repository with one source already merged into one page.
fn one_source_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    llm_wiki::init::run(dir.path(), false).unwrap();
    std::fs::write(dir.path().join("a.md"), "# A\n\nEvidence A\n").unwrap();
    let (code, body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(code, 0, "{body}");
    let root_id = body["next_task"]["id"].as_i64().unwrap();
    let leaves = leaves(&dir, "写 A 的结论");
    let (code, body) = run(
        &dir,
        &["task", "done", &root_id.to_string(), "--leaves", &leaves],
    );
    assert_eq!(code, 0, "{body}");
    let leaf_id = body["next_task"]["id"].as_i64().unwrap();
    let payload = payload(&dir);
    let (code, body) = run(
        &dir,
        &["task", "done", &leaf_id.to_string(), "--payload", &payload],
    );
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["next_task"], Value::Null, "{body}");
    dir
}

fn page_path(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join(".wiki/pages/concept/alpha.md")
}

#[test]
fn a_page_edited_outside_the_tree_is_queued_for_review() {
    let dir = one_source_repo();
    let mut page = std::fs::read_to_string(page_path(&dir)).unwrap();
    page.push_str("\nHuman correction.\n");
    std::fs::write(page_path(&dir), page).unwrap();

    let task = next_task(&dir);
    assert_eq!(task["kind"], "reconcile", "{task}");
    assert!(
        task["target_path"]
            .as_str()
            .unwrap()
            .ends_with("concept/alpha.md"),
        "{task}"
    );
    // Accepting the edit absorbs it: the next round has nothing left to say.
    let id = task["id"].as_i64().unwrap();
    let (code, body) = run(&dir, &["task", "done", &id.to_string(), "--accept"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(next_task(&dir), Value::Null);
}

#[test]
fn regenerating_review_puts_the_source_back_in_the_queue() {
    let dir = one_source_repo();
    let mut page = std::fs::read_to_string(page_path(&dir)).unwrap();
    page.push_str("\nHuman correction.\n");
    std::fs::write(page_path(&dir), page).unwrap();

    let task = next_task(&dir);
    assert_eq!(task["kind"], "reconcile", "{task}");
    let id = task["id"].as_i64().unwrap();
    let (code, body) = run(&dir, &["task", "done", &id.to_string(), "--regenerate"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["next_task"]["kind"], "breakdown", "{body}");
    assert_eq!(body["next_task"]["source_key"], "a.md", "{body}");
}

#[test]
fn a_review_needs_exactly_one_verdict() {
    let dir = one_source_repo();
    let mut page = std::fs::read_to_string(page_path(&dir)).unwrap();
    page.push_str("\nHuman correction.\n");
    std::fs::write(page_path(&dir), page).unwrap();
    let id = next_task(&dir)["id"].as_i64().unwrap();
    let (code, body) = run(&dir, &["task", "done", &id.to_string()]);
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_TASK_BAD_PAYLOAD");
}

#[test]
fn a_page_without_sources_is_absorbed_not_reviewed() {
    let dir = one_source_repo();
    std::fs::write(
        dir.path().join(".wiki/pages/concept/notes.md"),
        "---\nid: \"notes\"\ntitle: \"Notes\"\ntype: \"concept\"\nsummary: \"\"\ntags: []\nsources: []\nstatus: \"draft\"\nupdated: \"2026-11-10\"\n---\n\nA note.\n",
    )
    .unwrap();
    assert_eq!(next_task(&dir), Value::Null);
}
