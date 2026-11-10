//! Source renames rewrite current pages, never retired review/version data.

use std::fs;

use llm_wiki::scan::{Change, ChangeKind, Layer};
use llm_wiki::store::Store;
use llm_wiki::{config::Config, convert, hash, init, moves, page};

#[test]
fn a_source_rename_rewrites_only_formal_pages_and_preserves_legacy_data() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init::run(root, false).unwrap();
    let cfg = Config::load(root).unwrap();
    let mut store = Store::open(&root.join(".wiki/state.db")).unwrap();
    store.migrate().unwrap();
    fs::write(root.join("old.md"), "source body").unwrap();
    convert::convert_one(root, &cfg, &mut store, "old.md").unwrap();
    let old_hash = hash::hash_file(&root.join(".wiki/raw/old.md"), true).unwrap();
    fs::rename(root.join("old.md"), root.join("new.md")).unwrap();
    let text = "---\nid: a\ntitle: A\ntype: concept\nsummary: s\nsources: [.wiki/raw/old.md]\nstatus: stable\nupdated: 2026-10-27\n---\n\nbody\n";
    let legacy = [
        ".wiki/staging/concept/a.md",
        ".wiki/objects/ab/page.md",
        ".wiki/versions/v001/page.md",
    ];
    for relative in std::iter::once(&".wiki/pages/concept/a.md").chain(legacy.iter()) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let report = moves::apply(
        root,
        &cfg,
        &mut store,
        &[Change {
            kind: ChangeKind::Moved,
            key: "new.md".into(),
            hash: Some("source hash".into()),
            moved_from: Some("old.md".into()),
            layer: Layer::Source,
        }],
    )
    .unwrap();
    assert!(report.is_clean());
    assert_eq!(report.applied[0].pages, vec![".wiki/pages/concept/a.md"]);
    assert_eq!(
        page::read(&root.join(".wiki/pages/concept/a.md"))
            .unwrap()
            .0
            .sources,
        vec![".wiki/raw/new.md"]
    );
    for path in legacy {
        assert_eq!(fs::read_to_string(root.join(path)).unwrap(), text, "{path}");
        assert!(
            store.get_page_meta(path).unwrap().is_none(),
            "must not index {path}"
        );
    }
    assert!(!root.join(".wiki/raw/old.md").exists());
    let new_raw = root.join(".wiki/raw/new.md");
    assert!(fs::read_to_string(&new_raw)
        .unwrap()
        .contains("source: new.md"));
    let new_hash = hash::hash_file(&new_raw, true).unwrap();
    assert_ne!(new_hash, old_hash);
    assert_eq!(store.get_raw("new.md").unwrap().unwrap().hash, new_hash);
    let pending = store.work_items().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].source_key, "new.md");
    assert_eq!(pending[0].raw_key, "new.md");
    assert_eq!(pending[0].raw_hash, new_hash);
}
