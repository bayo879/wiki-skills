use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn install(root: &Path) -> PathBuf {
    let cli = root.join(".wiki/cli");
    fs::create_dir_all(&cli).unwrap();
    fs::create_dir_all(root.join(".wiki/skills/llm-wiki")).unwrap();
    fs::write(root.join(".wiki/readme.md"), "installation guide").unwrap();
    let bin = cli.join(format!("llm-wiki{}", std::env::consts::EXE_SUFFIX));
    fs::copy(env!("CARGO_BIN_EXE_llm-wiki"), &bin).unwrap();
    bin
}

#[test]
fn distributed_binary_initialises_its_own_root_from_another_directory() {
    for args in [
        vec!["ingest", "--dry-run", "--json"],
        vec!["doctor", "--json"],
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("a package with spaces");
        let bin = install(&root);
        let caller = tempfile::tempdir().unwrap();
        let out = Command::new(bin)
            .args(args)
            .current_dir(caller.path())
            .output()
            .unwrap();
        assert!(
            root.join(".wiki/wiki.toml").is_file(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(root.join(".wiki/pages").is_dir());
        assert!(!caller.path().join(".wiki").exists());
    }
}

#[test]
fn installed_resources_in_current_directory_are_a_fresh_install() {
    let root = tempfile::tempdir().unwrap();
    install(root.path());
    let out = Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
        .args(["ingest", "--dry-run", "--json"])
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(root.path().join(".wiki/wiki.toml").exists());
}

#[test]
fn installed_binary_does_not_reinitialise_a_damaged_database() {
    let root = tempfile::tempdir().unwrap();
    let bin = install(root.path());
    fs::write(root.path().join(".wiki/unknown-state"), "preserve").unwrap();
    let caller = tempfile::tempdir().unwrap();
    let out = Command::new(bin)
        .args(["ingest", "--json"])
        .current_dir(caller.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("E_NOT_INITIALISED"));
    assert!(!root.path().join(".wiki/wiki.toml").exists());
    assert!(!caller.path().join(".wiki").exists());
}

fn invoke(bin: &Path, caller: &Path, args: &[&str]) -> serde_json::Value {
    let out = Command::new(bin)
        .args(args)
        .arg("--json")
        .current_dir(caller)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["data"].clone()
}

#[test]
fn relative_source_route_and_payload_use_the_package_root() {
    let root = tempfile::tempdir().unwrap();
    let bin = install(root.path());
    let caller = tempfile::tempdir().unwrap();
    fs::write(root.path().join("source.md"), "# Alpha\nPackage source\n").unwrap();
    let data = invoke(&bin, caller.path(), &["ingest", "source.md", "--force"]);
    let id = data["next_task"]["id"].as_i64().unwrap().to_string();
    fs::write(
        root.path().join(".wiki/route.json"),
        r#"{"points":[{"id":"p1","finding":"Package conclusion","evidence":"source.md"}],"assignments":[{"point":"p1","action":"create","proposal":"n1"}],"proposals":[{"id":"n1","type":"concept","title":"Alpha","summary":"Package conclusion","why_new":"the source stands alone","why_not_existing":"no page covers it"}],"maintenance":[],"searches":[{"query":"Alpha","candidates":[],"reason":"no existing page"}]}"#,
    )
    .unwrap();
    let data = invoke(
        &bin,
        caller.path(),
        &["task", "done", &id, "--payload", ".wiki/route.json"],
    );
    let page = data["next_task"]["id"].as_i64().unwrap().to_string();
    fs::write(root.path().join(".wiki/payload.json"), r#"{"title":"Alpha","summary":"Package conclusion","tags":[],"status":"draft","body":"Package knowledge."}"#).unwrap();
    invoke(
        &bin,
        caller.path(),
        &["task", "done", &page, "--payload", ".wiki/payload.json"],
    );
    assert!(root.path().join(".wiki/pages/concept/alpha.md").exists());
    assert!(!caller.path().join(".wiki").exists());
}

#[cfg(unix)]
#[test]
fn path_symlink_tracks_a_relocated_package() {
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("original");
    install(&original);
    let moved = temp.path().join("relocated package");
    fs::rename(&original, &moved).unwrap();
    let link = temp.path().join("llm-wiki");
    std::os::unix::fs::symlink(moved.join(".wiki/cli/llm-wiki"), &link).unwrap();
    let caller = tempfile::tempdir().unwrap();
    let out = Command::new(link)
        .args(["ingest", "--dry-run"])
        .current_dir(caller.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(moved.join(".wiki/wiki.toml").exists());
    assert!(!caller.path().join(".wiki").exists());
}
