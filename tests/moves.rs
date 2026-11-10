//! Source renames rewrite current pages, never retired review/version data.
//!
//! A rename touches three places — pages, the raw file, the ledger — and any of
//! them can fail. These tests pin the order that keeps a half-finished move
//! retryable instead of leaving pages split between two raw keys.

use std::fs;
use std::path::Path;

use llm_wiki::scan::{Change, ChangeKind, Layer};
use llm_wiki::store::Store;
use llm_wiki::{config::Config, convert, hash, init, moves, page};
use tempfile::{tempdir, TempDir};

/// A repository with `old.md` converted, and one page citing its raw.
struct Fixture {
    dir: TempDir,
    cfg: Config,
    store: Store,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempdir().unwrap();
        let root = dir.path();
        init::run(root, false).unwrap();
        let cfg = Config::load(root).unwrap();
        let mut store = Store::open(&root.join(".wiki/state.db")).unwrap();
        store.migrate().unwrap();
        fs::write(root.join("old.md"), "source body").unwrap();
        convert::convert_one(root, &cfg, &mut store, "old.md").unwrap();
        let fixture = Fixture { dir, cfg, store };
        fixture.parse_page(".wiki/pages/concept/a.md");
        fixture
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn parse_page(&self, relative: &str) {
        let path = self.root().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, PAGE).unwrap();
    }

    fn rename_source(&self) {
        fs::rename(self.root().join("old.md"), self.root().join("new.md")).unwrap();
    }

    fn moved(&self) -> Change {
        Change {
            kind: ChangeKind::Moved,
            key: "new.md".into(),
            hash: Some("source hash".into()),
            moved_from: Some("old.md".into()),
            layer: Layer::Source,
        }
    }

    fn apply(&mut self) -> Result<moves::MoveReport, moves::MoveError> {
        let moved = self.moved();
        let root = self.root().to_path_buf();
        let cfg = self.cfg.clone();
        moves::apply(&root, &cfg, &mut self.store, &[moved])
    }

    fn page_sources(&self, relative: &str) -> Vec<String> {
        page::read(&self.root().join(relative)).unwrap().0.sources
    }
}

const PAGE: &str = "---\nid: a\ntitle: A\ntype: concept\nsummary: s\nsources: [.wiki/raw/old.md]\nstatus: stable\nupdated: 2026-10-27\n---\n\nbody\n";

#[test]
fn a_source_rename_rewrites_only_formal_pages_and_preserves_legacy_data() {
    let mut fixture = Fixture::new();
    let root = fixture.root().to_path_buf();
    let old_hash = hash::hash_file(&root.join(".wiki/raw/old.md"), true).unwrap();
    let legacy = [
        ".wiki/staging/concept/a.md",
        ".wiki/objects/ab/page.md",
        ".wiki/versions/v001/page.md",
    ];
    for relative in legacy {
        fixture.parse_page(relative);
    }
    fixture.rename_source();
    let report = fixture.apply().unwrap();
    assert!(report.is_clean());
    assert_eq!(report.applied[0].pages, vec![".wiki/pages/concept/a.md"]);
    assert_eq!(
        fixture.page_sources(".wiki/pages/concept/a.md"),
        vec![".wiki/raw/new.md"]
    );
    for path in legacy {
        assert_eq!(fs::read_to_string(root.join(path)).unwrap(), PAGE, "{path}");
        assert!(
            fixture.store.get_page_meta(path).unwrap().is_none(),
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
    assert_eq!(
        fixture.store.get_raw("new.md").unwrap().unwrap().hash,
        new_hash
    );
    let pending = fixture.store.work_items().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].source_key, "new.md");
    assert_eq!(pending[0].raw_key, "new.md");
    assert_eq!(pending[0].raw_hash, new_hash);
}

#[cfg(unix)]
#[test]
fn a_failed_page_rewrite_leaves_the_move_retryable() {
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = Fixture::new();
    fixture.rename_source();
    let page = fixture.root().join(".wiki/pages/concept/a.md");
    let writable = fs::metadata(&page).unwrap().permissions().mode();
    fs::set_permissions(&page, fs::Permissions::from_mode(0o444)).unwrap();

    let failed = fixture.apply();
    fs::set_permissions(&page, fs::Permissions::from_mode(writable)).unwrap();
    assert!(
        failed.is_err(),
        "a page that cannot be written must fail: {failed:?}"
    );

    // Nothing after the page step ran: the raw still sits under the old key and
    // the ledger still knows it, so the next round pairs the same two files.
    assert!(fixture.root().join(".wiki/raw/old.md").is_file());
    assert!(fixture.store.get_source("old.md").unwrap().is_some());
    assert_eq!(
        fixture.page_sources(".wiki/pages/concept/a.md"),
        vec![".wiki/raw/old.md"]
    );

    let report = fixture.apply().unwrap();
    assert!(report.is_clean());
    assert_eq!(
        fixture.page_sources(".wiki/pages/concept/a.md"),
        vec![".wiki/raw/new.md"]
    );
    assert!(fixture.store.get_source("old.md").unwrap().is_none());
    assert!(fixture.store.get_source("new.md").unwrap().is_some());
}

#[test]
fn a_half_finished_raw_rename_is_adopted_on_the_next_round() {
    let mut fixture = Fixture::new();
    fixture.rename_source();
    // The state a crash between the raw rename and the ledger move would leave:
    // the file is at the destination and already names the new source, while the
    // ledger still holds the old key.
    fs::rename(
        fixture.root().join(".wiki/raw/old.md"),
        fixture.root().join(".wiki/raw/new.md"),
    )
    .unwrap();
    let raw = fixture.root().join(".wiki/raw/new.md");
    let text = fs::read_to_string(&raw)
        .unwrap()
        .replace("source: old.md", "source: new.md");
    fs::write(&raw, text).unwrap();

    let report = fixture.apply().unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(
        fixture.page_sources(".wiki/pages/concept/a.md"),
        vec![".wiki/raw/new.md"]
    );
    assert!(fixture.store.get_source("old.md").unwrap().is_none());
    assert_eq!(
        fixture.store.get_raw("new.md").unwrap().unwrap().hash,
        hash::hash_file(&raw, true).unwrap()
    );
}

#[test]
fn a_raw_file_that_is_not_ours_is_never_overwritten() {
    let mut fixture = Fixture::new();
    fixture.rename_source();
    fixture.parse_page(".wiki/raw/new.md");
    let before = fs::read_to_string(fixture.root().join(".wiki/raw/new.md")).unwrap();

    let report = fixture.apply().unwrap();
    assert!(!report.is_clean());
    assert_eq!(report.collisions[0].from, "old.md");
    assert_eq!(
        fs::read_to_string(fixture.root().join(".wiki/raw/new.md")).unwrap(),
        before
    );
}
