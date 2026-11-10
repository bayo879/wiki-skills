//! Completion acknowledges one raw hash; it is not publication.
use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

fn run(dir: &TempDir, args: &[&str]) -> (i32, Value) {
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .current_dir(dir.path())
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        serde_json::from_slice(&out.stdout).unwrap(),
    )
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

fn raw_hash(dir: &TempDir) -> String {
    llm_wiki::hash::hash_file(&dir.path().join(".wiki/raw/a.md"), true).unwrap()
}

#[test]
fn done_requires_the_read_hash() {
    let dir = tempfile::tempdir().unwrap();
    let (code, body) = run(&dir, &["done", "a.md"]);
    assert_eq!(code, 2);
    assert_eq!(body["error"]["code"], "E_USAGE");
}

#[test]
fn done_clears_only_one_source_and_repeating_it_is_a_noop() {
    let dir = repo();
    let hash = raw_hash(&dir);
    let page = dir.path().join(".wiki/pages/a.md");
    std::fs::write(&page, "author bytes with no automatic sources\n").unwrap();
    for expected in [true, false] {
        let (code, body) = run(&dir, &["done", "a.md", "--hash", &hash]);
        assert_eq!(code, 0, "{body}");
        assert_eq!(body["data"]["completed"], expected);
        assert_eq!(
            std::fs::read_to_string(&page).unwrap(),
            "author bytes with no automatic sources\n"
        );
    }
    std::fs::remove_file(page).unwrap();
    let (code, body) = run(&dir, &["ingest"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["pending"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["pending"][0]["source_key"], "b.md");
}

#[test]
fn done_drift_is_exit_one_and_keeps_pending() {
    let dir = repo();
    let hash = raw_hash(&dir);
    std::fs::write(dir.path().join("a.md"), "changed source before ingest\n").unwrap();
    let (code, body) = run(&dir, &["done", "a.md", "--hash", &hash]);
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_SOURCE_DRIFT");
    let cfg = llm_wiki::config::Config::load(dir.path()).unwrap();
    let store =
        llm_wiki::store::Store::open(&dir.path().join(cfg.paths.engine).join("state.db")).unwrap();
    assert_eq!(store.work_items().unwrap().len(), 2);
}

#[test]
fn done_with_an_old_raw_hash_keeps_the_new_work() {
    let dir = repo();
    let old_hash = raw_hash(&dir);
    std::fs::write(dir.path().join("a.md"), "# New A\n\nNew evidence\n").unwrap();
    let (code, body) = run(&dir, &["ingest", "a.md", "--force"]);
    assert_eq!(code, 0, "{body}");
    let (code, body) = run(&dir, &["done", "a.md", "--hash", &old_hash]);
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_SOURCE_DRIFT");
    let (code, body) = run(&dir, &["ingest"]);
    assert_eq!(code, 0, "{body}");
    let pending = body["data"]["pending"].as_array().unwrap();
    assert!(pending
        .iter()
        .any(|item| item["source_key"] == "a.md" && item["raw_hash"] != old_hash));
}

#[test]
fn directly_written_pages_are_queryable_before_done() {
    let dir = repo();
    let page = dir.path().join(".wiki/pages/concept/alpha.md");
    std::fs::create_dir_all(page.parent().unwrap()).unwrap();
    let text = "---\nid: alpha\ntitle: Alpha\ntype: concept\nsummary: A conclusion\nstatus: draft\nupdated: 2026-01-01\nsources: [.wiki/raw/a.md]\n---\nuniquelivepagekeyword\n";
    std::fs::write(&page, text).unwrap();
    let (code, body) = run(&dir, &["query", "uniquelivepagekeyword"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["count"], 1);
    assert_eq!(std::fs::read_to_string(page).unwrap(), text);
}

#[test]
fn done_obeys_the_repository_lock() {
    let dir = repo();
    let hash = raw_hash(&dir);
    let _lock = llm_wiki::lock::acquire(dir.path()).unwrap();
    let (code, body) = run(&dir, &["done", "a.md", "--hash", &hash]);
    assert_eq!(code, 4, "{body}");
    assert_eq!(body["error"]["code"], "E_LOCKED");
}

#[test]
fn done_outside_the_open_batch_is_busy() {
    let dir = repo();
    let a_hash = raw_hash(&dir);
    let lease = serde_json::json!({
        "started_at": chrono::Utc::now().to_rfc3339(),
        "pid": 1,
        "run_id": "test",
        "items": [{
            "source_key": "b.md",
            "raw_key": ".wiki/raw/b.md",
            "raw_hash": llm_wiki::hash::hash_file(&dir.path().join(".wiki/raw/b.md"), true).unwrap(),
        }],
    });
    std::fs::write(dir.path().join(".wiki/work.lock"), lease.to_string()).unwrap();

    let (code, body) = run(&dir, &["done", "a.md", "--hash", &a_hash]);
    assert_eq!(code, 4, "{body}");
    assert_eq!(body["error"]["code"], "E_WORK_LEASE");
    let cfg = llm_wiki::config::Config::load(dir.path()).unwrap();
    let store =
        llm_wiki::store::Store::open(&dir.path().join(cfg.paths.engine).join("state.db")).unwrap();
    assert_eq!(
        store.work_items().unwrap().len(),
        2,
        "a busy acknowledgement must not clear anything"
    );
}

#[test]
fn acknowledging_one_source_heartbeats_the_batch() {
    let dir = repo();
    let a_hash = raw_hash(&dir);
    let file = dir.path().join(".wiki/work.lock");
    let mut body: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    // A batch whose only sign of life is this acknowledgement.
    body["started_at"] = Value::String("2020-01-01T00:00:00Z".to_string());
    body["refreshed_at"] = Value::String("2020-01-01T00:00:00Z".to_string());
    std::fs::write(&file, body.to_string()).unwrap();
    let before = body["items"].as_array().unwrap().len();

    let (code, out) = run(&dir, &["done", "a.md", "--hash", &a_hash]);
    assert_eq!(code, 0, "{out}");

    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(
        saved["started_at"], "2020-01-01T00:00:00Z",
        "the batch keeps the moment it opened"
    );
    assert_ne!(
        saved["refreshed_at"], "2020-01-01T00:00:00Z",
        "acknowledging is progress, so expiry counts from now"
    );
    let items = saved["items"].as_array().unwrap();
    assert_eq!(items.len(), before - 1, "only the acknowledged source left");
    assert!(
        !items.iter().any(|item| item["source_key"] == "a.md"),
        "the acknowledged source is not handed over again"
    );
}
