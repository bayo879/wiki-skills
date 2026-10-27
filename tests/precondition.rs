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
    let (code, _) = call(d.path(), &["init"]);
    assert_eq!(code, 0, "init must succeed in a fresh directory");
    fs::write(d.path().join("source/a.md"), "# A\n\nalpha body\n").unwrap();
    d
}

#[test]
fn every_repository_command_refuses_an_uninitialised_directory() {
    let d = TempDir::new().unwrap();
    // One command per family: reading the layout, the ledger, the staging area,
    // the pages, the versions and the search index.
    for args in [
        vec!["scan"],
        vec!["convert"],
        vec!["lint"],
        vec!["graph"],
        vec!["sync", "--dry-run"],
        vec!["ingest", "begin", "--sources", "raw/a.md"],
        vec!["approve"],
        vec!["checkpoint", "--label", "x"],
        vec!["query", "alpha"],
        vec!["archive", "status"],
        vec!["recall", "alpha"],
    ] {
        let (code, v) = call(d.path(), &args);
        assert_eq!(code, 2, "`{}` must be a usage error: {v}", args.join(" "));
        assert_eq!(v["ok"], false, "`{}`: {v}", args.join(" "));
        assert_eq!(v["error"]["code"], "E_NOT_INITIALISED", "{v}");
        assert_eq!(v["error"]["detail"]["missing"], "wiki.toml", "{v}");
        assert_eq!(v["error"]["detail"]["kind"], "file", "{v}");
        assert!(
            v["error"]["detail"]["hint"]
                .as_str()
                .unwrap_or_default()
                .contains("llm-wiki init"),
            "the hint must name the fix: {v}"
        );
    }
}

#[test]
fn the_message_names_the_missing_path_and_the_command_that_creates_it() {
    let d = TempDir::new().unwrap();
    let out = run(d.path(), &["scan"]);
    assert_eq!(out.status.code(), Some(2));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("wiki.toml"), "stderr: {text}");
    assert!(text.contains("not initialised"), "stderr: {text}");
    assert!(text.contains("llm-wiki init"), "stderr: {text}");
}

#[test]
fn a_missing_product_directory_is_refused_even_when_the_contract_exists() {
    let d = repo();
    fs::remove_dir_all(d.path().join("wiki")).unwrap();
    let (code, v) = call(d.path(), &["lint"]);
    assert_eq!(code, 2, "{v}");
    assert_eq!(v["error"]["code"], "E_NOT_INITIALISED", "{v}");
    assert_eq!(v["error"]["detail"]["missing"], "wiki", "{v}");
    assert_eq!(v["error"]["detail"]["kind"], "dir", "{v}");
}

#[test]
fn init_is_idempotent_so_the_advice_is_always_safe_to_follow() {
    let d = repo();
    let (code, v) = call(d.path(), &["init"]);
    assert_eq!(code, 0, "a second init must not fail: {v}");
    // The second run creates nothing: every path already exists.
    assert_eq!(
        v["data"]["created"].as_array().map(Vec::len),
        Some(0),
        "a second init must create nothing: {v}"
    );
}

#[test]
fn a_fresh_checkout_without_the_engine_directory_just_runs() {
    // `.wiki/` is ignored by git, so a clone never has it. Refusing there would
    // make "clone and run" impossible; the engine is provisioned instead.
    let d = repo();
    fs::remove_dir_all(d.path().join(".wiki")).unwrap();
    let (code, v) = call(d.path(), &["scan"]);
    assert_eq!(code, 0, "scan must provision the engine directory: {v}");
    assert!(d.path().join(".wiki").is_dir());
    assert!(d.path().join(".wiki/staging").is_dir());
    assert!(d.path().join(".wiki/cache").is_dir());
    assert!(d.path().join(".wiki/versions").is_dir());
}

#[test]
fn doctor_still_works_before_init_so_the_problem_can_be_diagnosed() {
    let d = TempDir::new().unwrap();
    let (code, v) = call(d.path(), &["doctor"]);
    assert_eq!(code, 1, "doctor reports the missing contract: {v}");
    assert_eq!(v["ok"], true, "doctor reports, it does not refuse: {v}");
}
