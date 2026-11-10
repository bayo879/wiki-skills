use llm_wiki::{config::Config, convert, init, store::Store};
use std::fs;

#[test]
fn legacy_queue_imports_valid_entries_once_and_keeps_broken_entries_visible() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut store = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    store.migrate().unwrap();
    fs::write(d.path().join("a.txt"), "legacy").unwrap();
    convert::convert_one(d.path(), &cfg, &mut store, "a.txt").unwrap();
    let item = store.work_items().unwrap()[0].clone();
    llm_wiki::worklist::complete(d.path(), &cfg, &mut store, "a.txt", &item.raw_hash).unwrap();
    fs::create_dir_all(d.path().join(".wiki/staging")).unwrap();
    let pending = d.path().join(".wiki/staging/pending.json");
    fs::write(&pending, r#"{"a.md":"untrusted-old-hash","broken.md":"x"}"#).unwrap();
    let original = fs::read(&pending).unwrap();
    let warnings = llm_wiki::worklist::import_legacy(d.path(), &cfg, &mut store).unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("broken.md"));
    assert_eq!(store.work_items().unwrap(), vec![item.clone()]);
    llm_wiki::worklist::complete(d.path(), &cfg, &mut store, "a.txt", &item.raw_hash).unwrap();
    assert_eq!(
        llm_wiki::worklist::import_legacy(d.path(), &cfg, &mut store)
            .unwrap()
            .len(),
        1
    );
    assert!(store.work_items().unwrap().is_empty());
    assert_eq!(fs::read(pending).unwrap(), original);
}

use tempfile::tempdir;

#[test]
fn done_rejects_old_hash_and_only_clears_the_matching_source() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut store = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    store.migrate().unwrap();
    for key in ["a.txt", "b.txt"] {
        fs::write(d.path().join(key), key).unwrap();
        convert::convert_one(d.path(), &cfg, &mut store, key).unwrap();
    }
    let first = store.work_items().unwrap()[0].clone();
    fs::write(d.path().join("a.txt"), "changed source").unwrap();
    assert!(
        llm_wiki::worklist::complete(d.path(), &cfg, &mut store, "a.txt", &first.raw_hash).is_err()
    );
    convert::convert_one(d.path(), &cfg, &mut store, "a.txt").unwrap();
    assert!(
        llm_wiki::worklist::complete(d.path(), &cfg, &mut store, "a.txt", &first.raw_hash).is_err()
    );
    let current = store.work_items().unwrap()[0].clone();
    assert!(
        llm_wiki::worklist::complete(d.path(), &cfg, &mut store, "a.txt", &current.raw_hash)
            .unwrap()
    );
    assert!(
        !llm_wiki::worklist::complete(d.path(), &cfg, &mut store, "a.txt", &current.raw_hash)
            .unwrap()
    );
    assert_eq!(store.work_items().unwrap()[0].source_key, "b.txt");
    fs::remove_file(d.path().join(".wiki/raw/b.md")).unwrap();
    let remaining = store.work_items().unwrap()[0].clone();
    assert!(
        llm_wiki::worklist::complete(d.path(), &cfg, &mut store, "b.txt", &remaining.raw_hash)
            .is_err()
    );
    assert_eq!(store.work_items().unwrap().len(), 1);
}

#[test]
fn successful_conversion_persists_a_recoverable_source_work_item() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    fs::write(d.path().join("a.txt"), "source A").unwrap();
    let mut store = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    store.migrate().unwrap();
    convert::convert_one(d.path(), &cfg, &mut store, "a.txt").unwrap();
    drop(store);
    let store = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    let items = store.work_items().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source_key, "a.txt");
    assert_eq!(items[0].raw_key, "a.md");
    assert_eq!(
        items[0].raw_hash,
        llm_wiki::hash::hash_file(&d.path().join(".wiki/raw/a.md"), true).unwrap()
    );
}

#[test]
fn failed_enqueue_rolls_back_conversion_baseline_and_can_retry() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let db = d.path().join(".wiki/state.db");
    let mut store = Store::open(&db).unwrap();
    store.migrate().unwrap();
    fs::write(d.path().join("a.txt"), "old").unwrap();
    convert::convert_one(d.path(), &cfg, &mut store, "a.txt").unwrap();
    let before = store.get_source("a.txt").unwrap().unwrap();
    let items = store.work_items().unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_work BEFORE INSERT ON work_items BEGIN SELECT RAISE(ABORT, 'injected enqueue failure'); END;").unwrap();
    fs::write(d.path().join("a.txt"), "new bytes").unwrap();
    assert!(convert::convert_one(d.path(), &cfg, &mut store, "a.txt").is_err());
    assert_eq!(store.get_source("a.txt").unwrap().unwrap(), before);
    assert_eq!(store.work_items().unwrap(), items);
    conn.execute_batch("DROP TRIGGER fail_work;").unwrap();
    convert::convert_one(d.path(), &cfg, &mut store, "a.txt").unwrap();
    assert_ne!(store.work_items().unwrap(), items);
}
