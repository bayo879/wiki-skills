//! The initialisation gate, driven through the real binary.
//!
//! The point of these tests is not that a missing file is an error — it is that
//! the error says *which* file and *what to do*, and that it fires before any
//! command does something half-way. A repository that was never `init`ed must
//! never look like a repository that is merely empty.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_llm-wiki")
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .current_dir(dir)
        .args(args)
        .output()
        .expect("llm-wiki is built for the tests")
}

/// Runs a command with `--json`, returning its exit code and envelope.
fn call(dir: &Path, args: &[&str]) -> (i32, Value) {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    let output = run(dir, &full);
    let json: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "`{}` must emit JSON: stdout={:?} stderr={:?}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().expect("llm-wiki exits normally"), json)
}

/// An initialised repository with one planted source.
fn repo() -> TempDir {
    let d = TempDir::new().unwrap();
    llm_wiki::init::run(d.path(), false).expect("init in a fresh directory");
    fs::write(d.path().join("a.md"), "# A\n\nalpha body\n").unwrap();
    d
}

#[test]
fn every_repository_command_bootstraps_an_empty_directory() {
    let d = TempDir::new().unwrap();
    // One command per family: the layout, the ledger, the work queue, the
    // pages and the search index. None of them may ask for a separate init.
    for args in [
        vec!["ingest"],
        vec!["task", "status"],
        vec!["ingest", "--dry-run"],
        vec!["query", "alpha"],
        vec!["query", "--graph"],
    ] {
        let (code, v) = call(d.path(), &args);
        assert!(
            d.path().join(".wiki/wiki.toml").is_file(),
            "`{}` did not bootstrap the engine: {v}",
            args.join(" ")
        );
        assert_ne!(
            v["error"]["code"],
            "E_NOT_INITIALISED",
            "`{}` must not expose initialisation: {v}",
            args.join(" ")
        );
        assert!(
            code == 0 || code == 1,
            "`{}` must run after bootstrapping: {v}",
            args.join(" ")
        );
    }
}

#[test]
fn a_fresh_directory_bootstraps_without_a_message() {
    let d = TempDir::new().unwrap();
    let out = run(d.path(), &["ingest"]);
    assert!(out.status.success());
    assert!(d.path().join(".wiki/wiki.toml").is_file());
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(!text.contains("not initialised"), "stderr: {text}");
}

#[test]
fn a_missing_product_directory_is_refused_even_when_the_contract_exists() {
    let d = repo();
    fs::remove_dir_all(d.path().join(".wiki/pages")).unwrap();
    let (code, v) = call(d.path(), &["ingest"]);
    assert_eq!(code, 2, "{v}");
    assert_eq!(v["error"]["code"], "E_NOT_INITIALISED", "{v}");
    assert_eq!(v["error"]["detail"]["missing"], ".wiki/pages", "{v}");
    assert_eq!(v["error"]["detail"]["kind"], "dir", "{v}");
}

#[test]
fn doctor_fix_is_idempotent_so_the_advice_is_always_safe_to_follow() {
    let d = repo();
    let (code, v) = call(d.path(), &["doctor", "--fix"]);
    assert_eq!(code, 0, "a second repair must not fail: {v}");
}

#[test]
fn an_engine_deleted_in_full_is_bootstrapped_again() {
    let d = repo();
    fs::remove_dir_all(d.path().join(".wiki")).unwrap();
    let (code, v) = call(d.path(), &["ingest"]);
    assert_eq!(code, 0, "a fresh engine is created: {v}");
    assert!(d.path().join(".wiki/wiki.toml").is_file());
    assert!(d.path().join(".wiki/pages").is_dir());
}

#[test]
fn provision_never_creates_the_authored_tree_but_repairs_its_children_when_it_exists() {
    let d = TempDir::new().unwrap();
    fs::create_dir_all(d.path().join(".wiki")).unwrap();
    fs::write(d.path().join(".wiki/wiki.toml"), "").unwrap();

    llm_wiki::precondition::provision(d.path()).unwrap();
    assert!(!d.path().join(".wiki/pages").exists());
    assert!(d.path().join(".wiki/raw").is_dir());

    // Once the product exists, its derived children are repairable again.
    fs::create_dir_all(d.path().join(".wiki/pages")).unwrap();
    llm_wiki::precondition::provision(d.path()).unwrap();
    assert!(d.path().join(".wiki/pages/audit").is_dir());
}

#[test]
fn doctor_on_a_fresh_directory_reports_healthy() {
    let d = TempDir::new().unwrap();
    let (code, v) = call(d.path(), &["doctor"]);
    assert_eq!(code, 0, "doctor bootstraps and reports: {v}");
    assert_eq!(v["ok"], true, "doctor reports, it does not refuse: {v}");
    assert!(d.path().join(".wiki/wiki.toml").is_file());
}
