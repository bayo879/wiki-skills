//! `llm-wiki doctor` end-to-end: one deliberately broken repository per check.
//!
//! Every check must be able to say `FAIL` on its own, and the exit code must
//! follow: any `FAIL` is exit 1, warnings alone are exit 0. The repository
//! every test starts from is a real `init` run, so the checks see exactly what
//! a user's repository looks like.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use llm_wiki::lock;
use serde_json::Value;
use tempfile::TempDir;

const CHECKS: [&str; 10] = [
    "config",
    "schema",
    "paths",
    "db",
    "tasks",
    "fts5",
    "legacy_layout",
    "lock",
    "token",
    "external_backend",
];

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

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A fresh repository, made by the real `init`.
///
/// `init` is no longer a command — `doctor --fix` is the only way in — so the
/// tests call the same function the repair path calls.
fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    llm_wiki::init::run(dir.path(), false).expect("init");
    dir
}

/// Runs `doctor --json` and returns (exit code, parsed envelope).
fn doctor(dir: &Path) -> (i32, Value) {
    let output = run(dir, &["--json", "doctor"]);
    let json: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("doctor must emit JSON, got {:?}", output.stdout));
    (output.status.code().expect("doctor exits normally"), json)
}

/// The level reported for one check.
fn level(json: &Value, name: &str) -> String {
    let checks = json["data"]["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("no checks array in {json}"));
    checks
        .iter()
        .find(|check| check["name"] == name)
        .unwrap_or_else(|| panic!("check {name} is missing from {json}"))["level"]
        .as_str()
        .expect("level is a string")
        .to_string()
}

/// The detail text of one check.
fn check_detail(json: &Value, name: &str) -> String {
    json["data"]["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("no checks array in {json}"))
        .iter()
        .find(|check| check["name"] == name)
        .unwrap_or_else(|| panic!("check {name} is missing"))["detail"]
        .as_str()
        .expect("detail is a string")
        .to_string()
}

/// Edits one line of `wiki.toml`, keeping the rest byte-identical.
fn edit_toml_line(dir: &Path, prefix: &str, replacement: &str) {
    let path = dir.join(".wiki/wiki.toml");
    let text = fs::read_to_string(&path).expect("wiki.toml exists");
    let mut out = String::new();
    let mut replaced = false;
    for line in text.lines() {
        if line.trim_start().starts_with(prefix) {
            out.push_str(replacement);
            replaced = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    assert!(replaced, "wiki.toml has no line starting with {prefix}");
    fs::write(&path, out).expect("wiki.toml is writable");
}

/// Rewrites `wiki/audit` style reports the way `lint` expects them.
fn write_page(dir: &Path, key: &str, front: &str, body: &str) {
    let path = dir.join(".wiki/staging").join(key);
    fs::create_dir_all(path.parent().unwrap()).expect("staging dir");
    fs::write(&path, format!("---\n{front}\n---\n\n{body}\n")).expect("page is writable");
}

#[test]
fn a_healthy_repository_exits_zero_and_names_every_check() {
    let dir = repo();
    let (code, json) = doctor(dir.path());
    assert_eq!(code, 0, "warnings alone must not fail the run");
    assert_eq!(json["ok"], Value::Bool(true));
    assert_eq!(json["cmd"], "doctor");

    let checks = json["data"]["checks"].as_array().expect("checks array");
    let names: Vec<&str> = checks
        .iter()
        .map(|check| check["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, CHECKS, "every check is reported, in order");
    for check in checks {
        assert_ne!(
            check["level"], "fail",
            "nothing fails on a fresh repo: {check}"
        );
        assert!(
            check["detail"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "every check explains itself: {check}"
        );
    }
    // `db` is the one expected warning: nothing has created the ledger yet.
    assert_eq!(level(&json, "db"), "warn");
}

#[test]
fn a_missing_wiki_toml_fails_the_config_check() {
    let dir = repo();
    fs::remove_file(dir.path().join(".wiki/wiki.toml")).expect("removable");
    let (code, json) = doctor(dir.path());
    assert_eq!(code, 1, "a FAIL is exit 1");
    assert_eq!(level(&json, "config"), "fail");
    // The verdict lives in the checks (the envelope's `ok` only says the
    // command ran, the same way `lint` reports findings).
    let detail = check_detail(&json, "config");
    assert!(
        detail.contains(".wiki/wiki.toml"),
        "the finding names the file: {detail}"
    );
}

#[test]
fn a_missing_config_section_is_only_a_warning() {
    let dir = repo();
    let path = dir.path().join(".wiki/wiki.toml");
    let text = fs::read_to_string(&path).expect("readable");
    let cut = text.find("[ingest]").expect("[ingest] is in the contract");
    fs::write(&path, &text[..cut]).expect("writable");
    let (code, json) = doctor(dir.path());
    assert_eq!(code, 0, "a defaulted section is a warning, not a failure");
    assert_eq!(level(&json, "config"), "warn");
}

#[test]
fn a_missing_schema_fails_the_schema_check() {
    let dir = repo();
    fs::remove_file(dir.path().join(".wiki/SCHEMA.md")).expect("removable");
    let (code, json) = doctor(dir.path());
    assert_eq!(code, 1);
    assert_eq!(level(&json, "schema"), "fail");
}

#[test]
fn a_missing_wiki_directory_fails_the_paths_check() {
    let dir = repo();
    fs::remove_dir_all(dir.path().join(".wiki/pages")).expect("removable");
    let (code, json) = doctor(dir.path());
    assert_eq!(code, 1);
    assert_eq!(level(&json, "paths"), "fail");
}

#[test]
fn an_unreadable_ledger_fails_the_db_check() {
    let dir = repo();
    // The store opens the file eagerly, so corruption is the visible shape of
    // "the ledger cannot be read". The newer-schema branch is unit-tested next
    // to the check itself, because `user_version` has no public setter and
    // `rusqlite` is not a dev-dependency.
    fs::write(dir.path().join(".wiki/state.db"), b"not a sqlite file").expect("writable");
    let (code, json) = doctor(dir.path());
    assert_eq!(code, 1);
    assert_eq!(level(&json, "db"), "fail");
}

#[test]
fn fts5_is_reported_ok_by_this_build() {
    let dir = repo();
    let (code, json) = doctor(dir.path());
    assert_eq!(code, 0);
    assert_eq!(level(&json, "fts5"), "ok");
    // The FAIL branch needs a SQLite built without FTS5; that is unit-tested
    // inside `src/doctor.rs` against the manual probe.
}

#[test]
fn a_root_contract_from_the_old_layout_is_a_warning() {
    let dir = repo();
    fs::write(dir.path().join("wiki.toml"), "[paths]\n").expect("writable");
    let (code, json) = doctor(dir.path());
    assert_eq!(code, 0);
    assert_eq!(level(&json, "legacy_layout"), "warn");
    let detail = check_detail(&json, "legacy_layout");
    assert!(
        detail.contains("wiki.toml"),
        "the finding names it: {detail}"
    );
}

#[test]
fn a_page_log_from_the_old_journal_layout_is_a_warning() {
    let dir = repo();
    fs::write(dir.path().join(".wiki/pages/log.md"), "# 日志\n").expect("writable");
    let (code, json) = doctor(dir.path());
    assert_eq!(code, 0);
    assert_eq!(level(&json, "legacy_layout"), "warn");
    let detail = check_detail(&json, "legacy_layout");
    assert!(
        detail.contains(".wiki/pages/log.md"),
        "the finding names it: {detail}"
    );
}

#[test]
fn a_held_lock_is_a_warning_and_a_leftover_file_is_not() {
    let dir = repo();
    let path = dir.path().join(".wiki/lock");
    {
        let _held = lock::acquire(dir.path()).expect("lock");
        let (code, json) = doctor(dir.path());
        assert_eq!(code, 0, "a lock may be legitimate");
        assert_eq!(level(&json, "lock"), "warn", "a live command holds it");
    }
    // Releasing keeps the inode; the leftover body describes nobody.
    assert!(path.is_file(), "the lock file is kept, not unlinked");
    let (code, json) = doctor(dir.path());
    assert_eq!(code, 0);
    assert_eq!(level(&json, "lock"), "ok", "a leftover file is not held");

    // Age says nothing: an ancient file that nobody holds is still just a file.
    let file = fs::File::options().write(true).open(&path).expect("open");
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
        .expect("set mtime");
    drop(file);
    let (code, json) = doctor(dir.path());
    assert_eq!(code, 0);
    assert_eq!(level(&json, "lock"), "ok", "age never makes a lock stale");
    assert!(fs::metadata(&path).is_ok(), "doctor never deletes the lock");
}

#[test]
fn legacy_review_and_version_data_are_ignored_and_preserved() {
    let dir = repo();
    write_page(
        dir.path(),
        "concept/alpha.md",
        "id: alpha\ntitle: A\ntype: concept\nsummary: s\nsources: []\nstatus: draft",
        "body",
    );
    let legacy = [
        ".wiki/staging/concept/alpha.md",
        ".wiki/staging/manifest.json",
        ".wiki/objects/ab/legacy",
        ".wiki/versions/v001/manifest.json",
    ];
    for path in &legacy[1..] {
        let path = dir.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"legacy bytes, not a valid manifest").unwrap();
    }
    let before: Vec<_> = legacy
        .iter()
        .map(|path| fs::read(dir.path().join(path)).unwrap())
        .collect();
    for args in [
        vec!["--json", "doctor"],
        vec!["--json", "doctor", "--only", "scan"],
        vec!["--json", "doctor", "--fix"],
    ] {
        let output = run(dir.path(), &args);
        assert!(output.status.success(), "{}", stderr(&output));
        let json: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(json["data"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|check| check["name"] != "staging"));
        for (path, expected) in legacy.iter().zip(&before) {
            assert_eq!(
                fs::read(dir.path().join(path)).unwrap(),
                *expected,
                "{path}"
            );
        }
    }
}

#[test]
fn a_literal_token_is_a_failure() {
    let dir = repo();
    let path = dir.path().join(".wiki/wiki.toml");
    let text = fs::read_to_string(&path).expect("readable");
    fs::write(&path, format!("token = \"abc123\"\n{text}")).expect("writable");
    let (code, json) = doctor(dir.path());
    assert_eq!(code, 1);
    assert_eq!(level(&json, "token"), "fail");
    // `deny_unknown_fields` makes the same line a config error too: a bare
    // `token` key is not part of the contract. Both checks point at one fix.
    assert_eq!(level(&json, "config"), "fail");
}

#[test]
fn external_primary_without_a_section_is_a_failure() {
    // The hole T11 left: saying `primary = "external"` while never configuring
    // the service makes every parse fail at run time, so doctor must catch it
    // before the first source is touched.
    let dir = repo();
    edit_toml_line(dir.path(), "primary", "primary = \"external\"");
    let (code, json) = doctor(dir.path());
    assert_eq!(code, 1);
    assert_eq!(level(&json, "external_backend"), "fail");
    assert_eq!(level(&json, "config"), "ok");
    let detail = check_detail(&json, "external_backend");
    assert!(
        detail.contains("parse.external"),
        "the finding names the missing table: {detail}"
    );
}

#[test]
fn an_unreachable_external_service_is_only_a_warning() {
    let dir = repo();
    edit_toml_line(dir.path(), "primary", "primary = \"external\"");
    let path = dir.path().join(".wiki/wiki.toml");
    let text = fs::read_to_string(&path).expect("readable");
    fs::write(
        &path,
        format!(
            "{text}\n[parse.external]\ntype = \"http\"\nurl = \"http://127.0.0.1:1/parse\"\nauth_env = \"WIKI_PARSE_TOKEN\"\nfields = {{ file = \"@file\" }}\n"
        ),
    )
    .expect("writable");
    let (code, json) = doctor(dir.path());
    assert_eq!(
        code, 0,
        "a service that may be down is not a broken repository"
    );
    assert_eq!(level(&json, "external_backend"), "warn");
}

#[test]
fn the_human_table_marks_each_level() {
    let dir = repo();
    fs::remove_file(dir.path().join(".wiki/wiki.toml")).expect("removable");
    let output = run(dir.path(), &["doctor"]);
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("FAIL"), "a failure is visible: {stdout}");
    assert!(
        stdout.lines().any(|line| line.contains("config")),
        "the failing check is named: {stdout}"
    );
}

#[test]
fn doctor_fix_bootstraps_a_fresh_repo() {
    let dir = tempfile::tempdir().expect("temp dir");
    let output = run(dir.path(), &["doctor", "--fix"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(dir.path().join(".wiki/wiki.toml").is_file());
    assert!(dir.path().join(".wiki/pages").is_dir());
    for legacy in [".wiki/staging", ".wiki/objects", ".wiki/versions"] {
        assert!(
            !dir.path().join(legacy).exists(),
            "must not create {legacy}"
        );
    }
}

#[test]
fn doctor_fix_restores_a_missing_schema_contract() {
    let dir = repo();
    fs::remove_file(dir.path().join(".wiki/SCHEMA.md")).expect("removable");
    let (code, json) = doctor(dir.path());
    assert_eq!(code, 1);
    assert_eq!(level(&json, "schema"), "fail");

    let fixed = run(dir.path(), &["doctor", "--fix"]);
    assert!(fixed.status.success(), "{}", stderr(&fixed));
    assert!(dir.path().join(".wiki/SCHEMA.md").is_file());

    let (code, json) = doctor(dir.path());
    assert_eq!(code, 0, "{json}");
    assert_eq!(level(&json, "schema"), "ok");
}

#[test]
fn doctor_fix_is_idempotent() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = run(dir.path(), &["doctor", "--fix"]);
    assert!(first.status.success(), "{}", stderr(&first));
    let second = run(dir.path(), &["doctor", "--fix"]);
    assert!(second.status.success(), "{}", stderr(&second));
}

#[test]
fn doctor_only_session_reports_a_record_without_a_page() {
    let dir = repo();
    fs::create_dir_all(dir.path().join("sessions")).expect("sessions dir");
    fs::write(dir.path().join("sessions/a.md"), "# 会话\n\n结论\n").expect("write");
    let output = run(dir.path(), &["--json", "doctor", "--only", "session"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let json: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(level(&json, "session"), "fail", "{json}");
    assert!(
        check_detail(&json, "session").contains("sessions/a.md"),
        "{json}"
    );
}

#[test]
fn doctor_only_convert_reports_an_unparseable_source() {
    let dir = repo();
    fs::write(dir.path().join("bad.pdf"), b"not a pdf").expect("write");
    let output = run(dir.path(), &["--json", "doctor", "--only", "convert"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let json: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(level(&json, "convert"), "fail", "{json}");
}
