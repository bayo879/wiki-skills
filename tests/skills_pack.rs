//! Checks that every skill in `skills/` obeys the Claude Code skill contract.
//!
//! The rules come from the harness itself: frontmatter may hold only `name` and
//! `description`, `name` must equal the directory and be a lowercase slug,
//! `description` is capped at 1024 characters, and a skill body should stay
//! under 500 lines. Two more rules are this project's own: a skill may only
//! name `llm-wiki` subcommands that really exist, and it may not reach outside
//! its own directory with a relative path.
//!
//! The subcommand list is read from the built binary rather than hardcoded, so
//! adding a subcommand to the CLI never leaves the skills silently stale.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const SKILLS_DIR: &str = "skills";

/// Splits a `SKILL.md` into its frontmatter map and its body.
fn parse_frontmatter(text: &str) -> (BTreeMap<String, String>, &str) {
    let rest = text
        .strip_prefix("---\n")
        .unwrap_or_else(|| panic!("must open with the `---` fence:\n{text}"));
    let end = rest
        .find("\n---\n")
        .unwrap_or_else(|| panic!("frontmatter is never closed:\n{text}"));
    let mut map = BTreeMap::new();
    for line in rest[..end].lines() {
        if let Some((key, value)) = line.split_once(':') {
            map.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    (map, &rest[end + 5..])
}

/// Every skill directory with its `SKILL.md` contents.
fn skills() -> Vec<(String, String)> {
    let mut found = Vec::new();
    for entry in fs::read_dir(SKILLS_DIR).unwrap_or_else(|err| panic!("no skills dir: {err}")) {
        let entry = entry.unwrap();
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let path = PathBuf::from(SKILLS_DIR).join(&name).join("SKILL.md");
        let text = fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path:?}: {err}"));
        // A checkout without `.gitattributes` can hand us CRLF (and a hand-edited
        // file can carry a BOM). The tools only ever write LF, and the fence is
        // compared byte for byte, so normalise once here.
        let text = text
            .strip_prefix('\u{feff}')
            .unwrap_or(&text)
            .replace("\r\n", "\n");
        found.push((name, text));
    }
    found.sort();
    found
}

/// The subcommands the installed binary advertises, read from `--help`.
fn real_subcommands() -> BTreeSet<String> {
    let bin = env!("CARGO_BIN_EXE_llm-wiki");
    let out = Command::new(bin)
        .arg("--help")
        .output()
        .expect("running `llm-wiki --help`");
    assert!(out.status.success(), "--help must succeed");
    let text = String::from_utf8_lossy(&out.stdout);
    let mut names = BTreeSet::new();
    let mut in_commands = false;
    for line in text.lines() {
        if line.trim_start().starts_with("Commands:") {
            in_commands = true;
            continue;
        }
        if in_commands {
            if line.trim().is_empty() {
                break;
            }
            if let Some(name) = line.split_whitespace().next() {
                names.insert(name.to_string());
            }
        }
    }
    assert!(!names.is_empty(), "could not read subcommands from --help");
    names
}

#[test]
fn every_skill_has_valid_frontmatter() {
    let all = skills();
    assert_eq!(all.len(), 6, "the package ships exactly six skills");
    for (dir, text) in all {
        let (fm, body) = parse_frontmatter(&text);
        assert_eq!(
            fm.len(),
            2,
            "{dir}: frontmatter may only hold `name` and `description`, got {:?}",
            fm.keys().collect::<Vec<_>>()
        );
        let name = fm
            .get("name")
            .unwrap_or_else(|| panic!("{dir}: missing `name`"));
        assert_eq!(name, &dir, "{dir}: `name` must equal the directory name");
        assert!(
            name.len() <= 64,
            "{dir}: `name` is {} characters, over the 64 cap",
            name.len()
        );
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{dir}: `name` must match ^[a-z0-9-]+$"
        );
        let description = fm
            .get("description")
            .unwrap_or_else(|| panic!("{dir}: missing `description`"));
        assert!(!description.trim().is_empty(), "{dir}: empty description");
        assert!(
            description.chars().count() <= 1024,
            "{dir}: description is {} characters, over the 1024 cap",
            description.chars().count()
        );
        assert!(
            body.lines().count() < 500,
            "{dir}: body is {} lines, over the 500 line budget",
            body.lines().count()
        );
    }
}

#[test]
fn no_skill_mentions_an_unknown_subcommand() {
    let known = real_subcommands();
    for (dir, text) in skills() {
        for word in text.split_whitespace() {
            let Some(rest) = word.strip_prefix("`llm-wiki ") else {
                continue;
            };
            let sub = rest
                .trim_end_matches(['`', ',', '.', ';', ':'])
                .split_whitespace()
                .next()
                .unwrap_or("");
            if sub.is_empty() {
                continue;
            }
            assert!(
                known.contains(sub),
                "{dir}: mentions `llm-wiki {sub}`, which the binary does not have"
            );
        }
    }
}

#[test]
fn no_skill_reaches_outside_its_directory() {
    for (dir, text) in skills() {
        assert!(
            !text.contains("](../"),
            "{dir}: relative references may only go one level deep"
        );
        for forbidden in ["/Users/", "C:\\\\Users", "/home/"] {
            assert!(
                !text.contains(forbidden),
                "{dir}: contains the absolute path prefix {forbidden}"
            );
        }
    }
}

#[test]
fn every_skill_names_its_cli_entry_point() {
    for (dir, text) in skills() {
        assert!(
            text.contains("llm-wiki"),
            "{dir}: a skill must tell the agent which command to run"
        );
    }
}
