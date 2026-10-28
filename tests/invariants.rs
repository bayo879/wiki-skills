//! Structural invariants of this repository, checked over the source tree.
//!
//! These are not behaviour tests: they read `src/` and the docs as text and fail
//! when a rule that `docs/readme/architecture.md` states stops being true. A
//! rule that only lives in prose is a rule a future change can quietly break,
//! so every claim in that file's 不变量 section is either pinned here or marked
//! as not machine-checked yet.
//!
//! The checks are deliberately narrow. Each one names the way it can be fooled,
//! because a guard that silently accepts a violation is worse than no guard.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Every `.rs` file under `src/`, with its text.
fn rust_sources() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
                continue;
            }
            if path.extension().is_some_and(|ext| ext == "rs") {
                let text = fs::read_to_string(&path).unwrap_or_default();
                out.push((path, text));
            }
        }
    }

    let mut out = Vec::new();
    walk(Path::new("src"), &mut out);
    assert!(
        !out.is_empty(),
        "no sources found; run the test from the crate root"
    );
    out
}

/// A file's text with its trailing `#[cfg(test)] mod tests` removed.
///
/// Every inline test module in this crate sits at the end of its file, so tests
/// that build a fixture database by hand (a `pragma_update` here, a raw `INSERT`
/// there) cannot trip the production-code checks.
fn without_tests(text: &str) -> &str {
    match text.find("#[cfg(test)]") {
        Some(at) => &text[..at],
        None => text,
    }
}

/// Files under `src/` whose name is not `store.rs`, with test modules stripped.
fn non_store_sources() -> Vec<(PathBuf, String)> {
    rust_sources()
        .into_iter()
        .filter(|(path, _)| path.file_name().is_none_or(|name| name != "store.rs"))
        .map(|(path, text)| (path, String::from(without_tests(&text))))
        .collect()
}

/// The body of the first function whose name matches, up to the next top-level
/// `fn`. Used to scope a check to one function instead of a whole file.
fn function_body<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let start = text.find(&format!("fn {name}("))?;
    let rest = &text[start..];
    let end = rest[2..]
        .find("\nfn ")
        .map(|at| at + 2)
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Every markdown file whose prose is part of the shipped documentation.
fn shipped_docs() -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    out.push((
        PathBuf::from("README.md"),
        fs::read_to_string("README.md").unwrap_or_default(),
    ));
    collect_markdown(Path::new("docs/readme"), &mut out);
    collect_markdown(Path::new("skills"), &mut out);
    out
}

fn collect_markdown(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown(&path, out);
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "md") {
            let text = fs::read_to_string(&path).unwrap_or_default();
            out.push((path, text));
        }
    }
}

// ------------------------------------------------------------------ SQL 单点

/// `architecture.md`: the ledger tables are only ever written from `store.rs`.
///
/// Fooled by: SQL assembled from fragments (`"INSERT" + " INTO"`). This crate
/// has no such style, and adding one would be a deliberate escape, so the check
/// stays literal.
#[test]
fn only_store_writes_sql() {
    const NEEDLES: [&str; 5] = [
        "INSERT INTO",
        "DELETE FROM",
        "CREATE TABLE",
        "CREATE VIRTUAL TABLE",
        "UPDATE ",
    ];
    let mut offenders = Vec::new();
    for (path, text) in non_store_sources() {
        for needle in NEEDLES {
            if text.contains(needle) {
                offenders.push(format!("{} contains `{needle}`", path.display()));
            }
        }
        if text.contains("pragma_") || text.contains("PRAGMA ") {
            offenders.push(format!("{} touches a pragma", path.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "SQL outside src/store.rs breaks the single-writer invariant: {offenders:#?}"
    );
}

// --------------------------------------------------------------- 写入口封闭

/// `architecture.md`: exactly three modules write the ledger — `convert`
/// registers, `moves` rekeys, `archive purge` deletes.
///
/// Fooled by: writing the rows with raw SQL instead of these methods, which the
/// test above catches first.
#[test]
fn only_three_modules_write_the_ledger() {
    const WRITERS: [&str; 5] = [
        "upsert_source(",
        "upsert_raw(",
        "delete_source(",
        "delete_raw(",
        "delete_archive(",
    ];
    let mut callers: BTreeSet<String> = BTreeSet::new();
    for (path, text) in non_store_sources() {
        if WRITERS.iter().any(|needle| text.contains(needle)) {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            callers.insert(name);
        }
    }
    let found: BTreeSet<String> = callers;
    let expected: BTreeSet<String> = ["archive.rs", "convert.rs", "moves.rs"]
        .iter()
        .map(|name| String::from(*name))
        .collect();
    assert_eq!(
        found, expected,
        "the ledger has a new writer; either route it through one of the three or update the \
         invariant in docs/readme/architecture.md"
    );
}

// --------------------------------------------------------------- 对账唯一

/// `architecture.md`: the `raws` join lives in `scan` and nowhere else.
///
/// Fooled by: joining through a different table (`page_meta`) to answer the same
/// question. The behavioural half is `tests/reconcile.rs`, which fails if the
/// three answers ever disagree.
#[test]
fn only_scan_joins_the_raw_ledger() {
    // `L7` asks the question, so its body must go through `RawLedger`. `L12`
    // legitimately lists asset raws with `all_raws`, which is why this is scoped
    // to one function instead of banning the call from the file.
    let lint = fs::read_to_string("src/lint.rs").expect("src/lint.rs is missing");
    let body = function_body(&lint, "check_unledgered_raws")
        .expect("lint no longer has check_unledgered_raws; update this invariant");
    assert!(
        body.contains("RawLedger::load"),
        "L7 no longer asks scan::RawLedger"
    );
    assert!(
        !body.contains("all_raws("),
        "L7 joins the raws ledger itself again; ask scan::RawLedger instead"
    );

    let ingest = fs::read_to_string("src/ingest.rs").expect("src/ingest.rs is missing");
    assert!(
        ingest.contains("RawLedger::load"),
        "ingest begin no longer asks scan::RawLedger"
    );
    assert!(
        !ingest.contains("all_raws("),
        "ingest joins the raws ledger itself; ask scan::RawLedger instead"
    );
}

// ------------------------------------------------------- 文档不指向可删目录

/// The `superpowers` folder is disposable: it can be deleted at any moment, so
/// no shipped doc or source comment may point into it.
///
/// The needle is assembled at runtime so this file does not trip its own check.
#[test]
fn no_shipped_file_points_at_the_disposable_folder() {
    let needle = concat!("super", "powers");
    let mut offenders = Vec::new();
    for (path, text) in shipped_docs().into_iter().chain(rust_sources()) {
        if text.contains(needle) {
            offenders.push(path.display().to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "these files point at the disposable spec folder: {offenders:#?}"
    );
}

// ----------------------------------------------------- 不变量每条都有守卫

/// The 不变量 section must name, per line, the test that guards it.
///
/// Two forms are accepted: a live pointer `（检查：tests/<file>.rs::<test>）`
/// whose file and function must exist, or an explicit `（未机器化：<reason>）`.
/// A bare line fails: an invariant nobody checks is a wish, not an invariant.
///
/// Fooled by: pointing ten invariants at one broad test file. Requiring
/// `::<test_fn>` keeps each pointer specific, though it cannot prove that the
/// named test really guards the words around it — that part stays a review job.
#[test]
fn every_invariant_names_a_live_check() {
    let text = fs::read_to_string("docs/readme/architecture.md")
        .expect("docs/readme/architecture.md is missing");
    let section = text
        .split("## 不变量")
        .nth(1)
        .expect("architecture.md has no 不变量 section");

    let mut checked = 0;
    let mut unguarded = 0;
    let mut problems = Vec::new();
    for line in section.lines() {
        let Some(claim) = line.strip_prefix("- ") else {
            continue;
        };
        if let Some(rest) = claim.split("（检查：").nth(1) {
            let pointer = rest.split('）').next().unwrap_or_default().trim();
            let (file, function) = pointer
                .split_once("::")
                .unwrap_or_else(|| panic!("invariant pointer `{pointer}` names no test function"));
            let path = PathBuf::from(file);
            let body = fs::read_to_string(&path).unwrap_or_else(|_| {
                panic!(
                    "invariant points at {}, which does not exist",
                    path.display()
                )
            });
            assert!(
                body.contains(&format!("fn {function}(")),
                "invariant points at {file}::{function}, which is not defined there"
            );
            checked += 1;
        } else if claim.contains("（未机器化：") {
            unguarded += 1;
        } else {
            problems.push(claim.chars().take(40).collect::<String>());
        }
    }

    assert!(
        problems.is_empty(),
        "these invariants name no check and no reason: {problems:#?}"
    );
    assert!(
        checked >= 8,
        "only {checked} invariants point at a live check; the section was probably cut down"
    );
    assert!(
        unguarded <= 2,
        "{unguarded} invariants are unguarded; the debt went up, add a check or justify it"
    );
}

/// `store.rs` steps through the schema one constant per version, and nothing
/// forces it to: bumping `SCHEMA_VERSION` without adding a step leaves every
/// existing database silently at the old schema. This reads the numbers back.
#[test]
fn migrate_steps_through_every_schema_version() {
    let text = fs::read_to_string("src/store.rs").expect("src/store.rs is missing");
    let version: i64 = text
        .split("pub const SCHEMA_VERSION: i64 = ")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("store.rs no longer declares SCHEMA_VERSION")
        .trim()
        .parse()
        .expect("SCHEMA_VERSION is not a literal integer");

    let constants = text.matches("const MIGRATION_").count() as i64;
    assert_eq!(
        constants, version,
        "SCHEMA_VERSION is {version} but there are {constants} MIGRATION_ constants"
    );

    for step in 1..=version {
        let applied = format!("apply_migration(MIGRATION_{step}, {step})");
        assert_eq!(
            text.matches(&applied).count(),
            1,
            "migrate() must apply version {step} exactly once (`{applied}`)"
        );
    }

    assert!(
        text.contains("SchemaTooNew"),
        "migrate() no longer refuses a database from a newer build"
    );
}

/// The thirteen rules exist in three places that can drift apart: the doc
/// comment in `src/lint.rs` (rule and severity), the operator table in
/// `skills/wiki-audit/SKILL.md` (severity), and `tests/lint.rs` (one `l{n}_`
/// test per rule). This holds all three against rule numbers 1..=13.
#[test]
fn every_lint_rule_agrees_with_the_skill_table() {
    let lint = fs::read_to_string("src/lint.rs").expect("src/lint.rs is missing");
    let mut declared: Vec<(u32, String)> = Vec::new();
    for line in lint.lines() {
        let Some(rest) = line.strip_prefix("//! - `L") else {
            continue;
        };
        let (number, rest) = rest
            .split_once('`')
            .expect("a rule bullet in src/lint.rs has no closing backtick");
        let level = rest
            .trim_start()
            .strip_prefix(':')
            .unwrap_or(rest.trim_start())
            .trim_start()
            .split_once(':')
            .map(|(level, _)| level.trim().to_string())
            .expect("a rule bullet in src/lint.rs has no `level:` text");
        declared.push((
            number
                .parse()
                .expect("a rule bullet in src/lint.rs has a non-numeric rule number"),
            level,
        ));
    }
    assert_eq!(
        declared
            .iter()
            .map(|(number, _)| *number)
            .collect::<Vec<_>>(),
        (1..=13).collect::<Vec<u32>>(),
        "src/lint.rs must declare L1..L13 once each, in order"
    );

    let skill = fs::read_to_string("skills/wiki-audit/SKILL.md")
        .expect("skills/wiki-audit/SKILL.md is missing");
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut in_table = false;
    for line in skill.lines() {
        if line.starts_with("| 检查 |") {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        if !line.starts_with('|') {
            break;
        }
        let cells: Vec<&str> = line.split('|').collect();
        if cells.len() < 4 || cells[1].trim().starts_with("---") {
            continue;
        }
        rows.push((cells[1].trim().to_string(), cells[2].trim().to_string()));
    }
    assert_eq!(
        rows.len(),
        declared.len(),
        "skills/wiki-audit/SKILL.md lists {} rules, src/lint.rs declares {}",
        rows.len(),
        declared.len()
    );
    for ((number, level), (label, table_level)) in declared.iter().zip(&rows) {
        assert_eq!(
            level, table_level,
            "L{number} (`{label}`) is `{level}` in src/lint.rs but `{table_level}` in the skill table"
        );
    }

    let tests = fs::read_to_string("tests/lint.rs").expect("tests/lint.rs is missing");
    for (number, _) in &declared {
        let prefix = format!("fn l{number}_");
        assert!(
            tests.contains(&prefix),
            "tests/lint.rs has no `l{number}_` test; every rule needs one"
        );
    }
}
