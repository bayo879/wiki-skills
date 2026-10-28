//! What the shipped documentation claims, held against what the binary does.
//!
//! The six skills and `docs/readme/` describe a JSON shape, a set of flags and
//! a set of error codes. Nothing in the compiler connects those sentences to
//! the code, so this file does it mechanically:
//!
//! * the JSON key shape of each documented command is snapshotted, so a field
//!   that appears or disappears shows up in review;
//! * every `--flag` named in the docs must exist in some `--help`;
//! * every backticked `snake_case` name in the docs must be a JSON key, a
//!   `wiki.toml` key, a `SCHEMA.md` field or a name listed in
//!   `tests/docs_tokens.allow`;
//! * every `E_*` code named in the docs must exist in `src/`.
//!
//! Run with `LLM_WIKI_UPDATE_SNAPSHOTS=1` to rewrite the shape snapshot after
//! deliberately changing a command's output.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn bin() -> Command {
    Command::cargo_bin("llm-wiki").expect("the llm-wiki binary did not build")
}

/// Runs one subcommand in `dir` and returns its parsed JSON document.
fn run_json(dir: &Path, args: &[&str]) -> Value {
    run_json_allowing(dir, args, &[0])
}

/// Same, for a command whose documented exit status is 0 or 1, like `lint`.
fn run_json_allowing(dir: &Path, args: &[&str], ok: &[i32]) -> Value {
    let mut full = args.to_vec();
    full.push("--json");
    let output = bin()
        .current_dir(dir)
        .args(&full)
        .output()
        .unwrap_or_else(|error| panic!("running `llm-wiki {}` failed: {error}", full.join(" ")));
    let code = output.status.code().unwrap_or(-1);
    assert!(
        ok.contains(&code),
        "`llm-wiki {}` exited {code} (expected one of {ok:?}): {}",
        full.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "`llm-wiki {}` did not print JSON: {error}\n{}",
            full.join(" "),
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// A repository with two text sources, converted, and one page that cites a
/// raw file and a page id nothing defines — enough for every list-shaped field
/// to have at least one element, which is the only way a shape check can see
/// what is inside an array.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    run_json(root, &["init"]);

    fs::write(
        root.join("source").join("a.md"),
        "# A\n\nThe retry limit is three attempts.\n",
    )
    .unwrap();
    fs::write(
        root.join("source").join("b.md"),
        "# B\n\nThe timeout is five seconds.\n",
    )
    .unwrap();
    dir
}

/// Writes one well-formed page so the wiki is not empty.
fn write_page(root: &Path, name: &str, body: &str) {
    let dir = root.join("wiki").join("concept");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(name),
        format!(
            "---\nid: {}\ntitle: {name}\ntype: concept\nsummary: about retries\nsources:\n  - raw/a.md\nstatus: stable\nupdated: 2026-10-27\n---\n\n{body}\n",
            name.trim_end_matches(".md")
        ),
    )
    .unwrap();
}

/// Every leaf path in a JSON document, with `[]` standing for any array index.
///
/// Object keys are part of the path; array indices are not, because the point
/// is the shape of one element, not how many elements this fixture produced.
fn paths(value: &Value, prefix: String, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                paths(child, path, out);
            }
        }
        Value::Array(items) => {
            let path = format!("{prefix}[]");
            if items.is_empty() {
                out.insert(path.clone());
            }
            for item in items {
                paths(item, path.clone(), out);
            }
        }
        _ => {
            out.insert(prefix);
        }
    }
}

/// The documented commands, and the shape each of them prints.
///
/// The fixture is built one step at a time so that each command runs against a
/// repository in the state the docs describe for it: `scan` before `convert`,
/// `lint` after a page exists, and so on.
fn shapes() -> BTreeMap<String, BTreeSet<String>> {
    let dir = fixture();
    let root = dir.path();
    let mut out = BTreeMap::new();
    let capture = |out: &mut BTreeMap<String, BTreeSet<String>>, name: &str, args: &[&str]| {
        let value = run_json(root, args);
        let mut set = BTreeSet::new();
        paths(&value, String::new(), &mut set);
        out.insert(name.to_string(), set);
    };

    capture(&mut out, "init", &["init"]);
    capture(&mut out, "version", &["version"]);
    capture(&mut out, "doctor", &["doctor"]);
    capture(&mut out, "scan", &["scan"]);
    capture(&mut out, "parse", &["parse", "--key", "a.md"]);
    capture(&mut out, "convert", &["convert"]);

    write_page(
        root,
        "retry.md",
        "Retries stop at three; see [[missing-page]].",
    );
    write_page(
        root,
        "timeout.md",
        "The timeout is five seconds; see [[retry]].",
    );

    capture(&mut out, "query-reindex", &["query", "--reindex"]);
    capture(&mut out, "query", &["query", "retries"]);
    let lint = run_json_allowing(root, &["lint"], &[0, 1]);
    // Exit 1 with findings is the documented behaviour, so it is not a failure.
    assert_eq!(lint["ok"], Value::Bool(true));
    let mut set = BTreeSet::new();
    paths(&lint, String::new(), &mut set);
    out.insert(String::from("lint"), set);
    capture(&mut out, "graph", &["graph"]);
    capture(
        &mut out,
        "ingest-begin",
        &["ingest", "begin", "--sources", "a.md", "--dry-run"],
    );
    out
}

fn snapshot_path() -> PathBuf {
    workspace()
        .join("tests")
        .join("snapshots")
        .join("cli-keys.json")
}

#[test]
fn the_json_shape_of_each_command_matches_the_snapshot() {
    let live: BTreeMap<String, Vec<String>> = shapes()
        .into_iter()
        .map(|(name, set)| (name, set.into_iter().collect()))
        .collect();
    let rendered = format!("{}\n", serde_json::to_string_pretty(&live).unwrap());

    let path = snapshot_path();
    if std::env::var("LLM_WIKI_UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &rendered).unwrap();
        return;
    }

    let recorded = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{} is missing; run with LLM_WIKI_UPDATE_SNAPSHOTS=1 once and review the diff",
            path.display()
        )
    });
    assert_eq!(
        rendered, recorded,
        "a command's JSON shape changed; review the diff, then re-run with LLM_WIKI_UPDATE_SNAPSHOTS=1"
    );
}

/// Every flag the binary accepts, walking `--help` down the subcommand tree.
fn known_flags() -> BTreeSet<String> {
    fn visit(path: &[String], flags: &mut BTreeSet<String>) {
        let Some(output) = bin().args(path).arg("--help").output().ok() else {
            return;
        };
        if !output.status.success() {
            return;
        }
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
            if let Some(flag) = word.strip_prefix("--") {
                if flag.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                    flags.insert(format!("--{flag}"));
                }
            }
        }
        let mut in_commands = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed == "Commands:" {
                in_commands = true;
                continue;
            }
            if !in_commands {
                continue;
            }
            if trimmed.is_empty() {
                in_commands = false;
                continue;
            }
            let Some(word) = trimmed.split_whitespace().next() else {
                continue;
            };
            if !word.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                continue;
            }
            let mut next = path.to_vec();
            next.push(word.to_string());
            visit(&next, flags);
        }
    }

    let mut flags = BTreeSet::new();
    visit(&[], &mut flags);
    flags
}

/// The markdown the operator reads: the skills and the module docs.
fn shipped_markdown() -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![
        workspace().join("skills"),
        workspace().join("docs").join("readme"),
    ];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                let text = fs::read_to_string(&path).unwrap();
                out.push((path, text));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn every_flag_the_docs_name_exists() {
    let flags = known_flags();
    let mut unknown = Vec::new();
    for (path, text) in shipped_markdown() {
        for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
            if let Some(flag) = word.strip_prefix("--") {
                if !flag.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                    continue;
                }
                let flag = format!("--{flag}");
                if !flags.contains(&flag) {
                    unknown.push(format!("{}: {flag}", path.display()));
                }
            }
        }
    }
    unknown.sort();
    unknown.dedup();
    assert!(
        unknown.is_empty(),
        "the docs name flags the binary does not accept:\n{}",
        unknown.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Field and enum tables
// ---------------------------------------------------------------------------

/// `CamelCase` to the `snake_case` serde writes on the wire.
fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, character) in name.chars().enumerate() {
        if character.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(character.to_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

/// The wire names of a `#[serde(rename_all = "snake_case")]` enum's variants.
fn enum_variants(source: &Path, name: &str) -> Vec<String> {
    let text =
        fs::read_to_string(source).unwrap_or_else(|_| panic!("{} is missing", source.display()));
    let start = text
        .find(&format!("pub enum {name} {{"))
        .unwrap_or_else(|| panic!("{} has no `pub enum {name}`", source.display()));
    // The serde attribute sits on the line above the declaration, so look at a
    // window that straddles it.
    let attribute_window = &text[start.saturating_sub(300)..start];
    let body = &text[start..];
    let body = &body[..body.find("\n}").expect("an enum without a closing brace")];
    assert!(
        attribute_window.contains("rename_all = \"snake_case\""),
        "{}::{name} no longer renames variants to snake_case; update this check",
        source.display()
    );
    let mut variants = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("///") || trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some(variant) = trimmed.split(|c: char| !c.is_ascii_alphanumeric()).next() {
            if variant.chars().next().is_some_and(|c| c.is_uppercase()) {
                variants.push(snake_case(variant));
            }
        }
    }
    variants.sort();
    variants
}

/// The first column of the table that follows the marker comment on the line
/// `line`, trimmed of backticks, as a sorted set.
fn table_under(lines: &[&str], at: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut in_rows = false;
    for line in &lines[at + 1..] {
        let trimmed = line.trim();
        if trimmed.starts_with("| 字段 |") || (trimmed.starts_with('|') && !in_rows) {
            in_rows = true;
            continue;
        }
        if !trimmed.starts_with('|') {
            if !rows.is_empty() {
                break;
            }
            if trimmed.is_empty() || trimmed.starts_with("| 字段 |") {
                continue;
            }
            break;
        }
        let cell = trimmed.split('|').nth(1).unwrap_or("").trim();
        if cell.starts_with("---") {
            continue;
        }
        rows.push(cell.trim_matches('`').trim().to_string());
    }
    rows.sort();
    rows
}

/// Finds `<!-- enum: path::Type -->` and `<!-- fields: <snapshot target> -->`
/// markers and the table each introduces.
fn marked_tables() -> Vec<(String, String, Vec<String>, PathBuf)> {
    let mut out = Vec::new();
    for (path, text) in shipped_markdown() {
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            for kind in ["enum", "fields"] {
                let Some(rest) = trimmed.strip_prefix(&format!("<!-- {kind}:")) else {
                    continue;
                };
                let target = rest.trim_end_matches("-->").trim().to_string();
                out.push((
                    kind.to_string(),
                    target,
                    table_under(&lines, index),
                    path.clone(),
                ));
            }
        }
    }
    out
}

#[test]
fn every_marked_enum_table_lists_the_variants_the_code_has() {
    let mut checked = 0;
    for (kind, target, rows, path) in marked_tables() {
        if kind != "enum" {
            continue;
        }
        let (file, name) = target
            .split_once("::")
            .unwrap_or_else(|| panic!("{}: `{target}` is not `file::Type`", path.display()));
        let mut expected = enum_variants(&workspace().join(file), name);
        expected.sort();
        assert_eq!(
            rows,
            expected,
            "{}: the table under `<!-- enum: {target} -->` does not list the variants of {name}",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 2,
        "expected at least the two scan tables to be marked"
    );
}

#[test]
fn every_marked_field_table_lists_the_keys_the_command_prints() {
    let shapes = shapes();
    let mut checked = 0;
    for (kind, target, rows, path) in marked_tables() {
        if kind != "fields" {
            continue;
        }
        let (command, rest) = target.split_once('.').unwrap_or((target.as_str(), ""));
        let all = shapes
            .get(command)
            .unwrap_or_else(|| panic!("{}: `{target}` names no captured command", path.display()));
        let prefix = if rest.is_empty() {
            String::from("data.")
        } else {
            format!("data.{rest}.")
        };
        let mut expected: Vec<String> = all
            .iter()
            .filter_map(|leaf| leaf.strip_prefix(&prefix))
            .map(|leaf| {
                leaf.split(['.', '['])
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        expected.sort();
        expected.dedup();
        assert_eq!(
            rows, expected,
            "{}: the table under `<!-- fields: {target} -->` does not list the keys the command prints",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 1,
        "expected at least one fields table to be marked"
    );
}

/// Objects whose every printed field must be named in the skill that documents
/// them. A field can be added to a command without the doc noticing; this is
/// the direction the snapshot alone cannot catch.
const COVERED: &[(&str, &str, &str)] = &[
    ("skills/wiki-scan/SKILL.md", "scan", "changes[]"),
    ("skills/wiki-scan/SKILL.md", "convert", "converted[]"),
    ("skills/wiki-audit/SKILL.md", "lint", "findings[]"),
    ("skills/wiki-audit/SKILL.md", "doctor", "checks[]"),
    ("skills/wiki-ingest/SKILL.md", "ingest-begin", "affected[]"),
];

#[test]
fn every_printed_field_of_a_documented_object_is_named_in_its_skill() {
    let shapes = shapes();
    let mut missing = Vec::new();
    for (skill, command, object) in COVERED {
        let text = fs::read_to_string(workspace().join(skill)).expect("a covered skill is missing");
        let prefix = format!("data.{object}.");
        let all = shapes
            .get(*command)
            .expect("a covered command was not captured");
        let name = object.trim_end_matches("[]");
        let fields: Vec<String> = all
            .iter()
            .filter(|leaf| leaf.starts_with(&prefix))
            .map(|leaf| {
                leaf.strip_prefix(&prefix)
                    .unwrap()
                    .split(['.', '['])
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect();
        // The line that enumerates an object's fields is the one that names
        // several of them. Anything else that happens to mention the object is
        // prose, and naming a field there does not document the shape: drop
        // `layer` from the enumeration and the old, whole-file check stayed
        // green because a later sentence still said "filter by `layer`".
        let named_on = |line: &str, field: &str| {
            line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|word| word == field)
        };
        let candidates: Vec<&str> = text.lines().filter(|line| line.contains(name)).collect();
        // A prose sentence can easily name two fields by accident ("filter by
        // `layer`, do not guess from `key`"), so demand half of them at least.
        let enough = std::cmp::max(2, fields.len().div_ceil(2));
        let enumerating: Vec<&str> = candidates
            .iter()
            .copied()
            .filter(|line| fields.iter().filter(|field| named_on(line, field)).count() >= enough)
            .collect();
        let considered = if enumerating.is_empty() {
            &candidates
        } else {
            &enumerating
        };
        for field in &fields {
            if !considered.iter().any(|line| named_on(line, field)) {
                missing.push(format!(
                    "{skill}: {command} prints `{field}`, no line enumerating `{object}` names it"
                ));
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "documented objects have fields the docs do not mention on their field list:\n{}",
        missing.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Closed-world vocabulary
// ---------------------------------------------------------------------------

/// Backticked spans that look like a `snake_case` name.
fn snake_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for span in text.split('`').skip(1).step_by(2) {
        let token = span.trim();
        let snake = token
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if snake
            && token.contains('_')
            && token.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        {
            out.push(token.to_string());
        }
    }
    out
}

/// Names the docs are allowed to use beyond what the code prints.
fn allowlist() -> BTreeSet<String> {
    let path = workspace().join("tests").join("docs_tokens.allow");
    let text = fs::read_to_string(&path).unwrap_or_default();
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect()
}

/// Every name the shipped docs may legitimately mention.
fn vocabulary() -> BTreeSet<String> {
    let mut names = BTreeSet::new();

    // JSON keys the binary actually prints.
    for set in shapes().values() {
        for leaf in set {
            for part in leaf.split(['.', '[']) {
                if !part.is_empty() {
                    names.insert(part.to_string());
                }
            }
        }
    }

    // `wiki.toml` keys, from the file `init` writes.
    let dir = tempfile::tempdir().expect("temp dir");
    run_json(dir.path(), &["init"]);
    let toml = fs::read_to_string(dir.path().join("wiki.toml")).expect("init wrote no wiki.toml");
    for line in toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some(section) = trimmed.strip_prefix('[') {
            for part in section.trim_end_matches(']').split('.') {
                names.insert(part.trim().to_string());
            }
            continue;
        }
        if let Some((key, _)) = trimmed.split_once('=') {
            names.insert(key.trim().to_string());
        }
    }

    // Fields `SCHEMA.md` declares, which the page frontmatter tables restate.
    let schema = fs::read_to_string(dir.path().join("SCHEMA.md")).expect("init wrote no SCHEMA.md");
    for token in snake_tokens(&schema) {
        names.insert(token);
    }
    for line in schema.lines() {
        if let Some((field, _)) = line.trim().split_once(':') {
            let field = field.trim();
            if field.chars().all(|c| c.is_ascii_lowercase() || c == '_') && !field.is_empty() {
                names.insert(field.to_string());
            }
        }
    }

    names.extend(string_literals());
    names.extend(allowlist());
    names
}

/// Every `snake_case` string literal in `src/`: warning codes, config keys,
/// ledger column names — the names the code itself writes down.
fn string_literals() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut stack = vec![workspace().join("src")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("src is missing").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = fs::read_to_string(&path).unwrap();
                for token in text
                    .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
                {
                    if token.contains('_') && token.len() <= 48 {
                        names.insert(token.to_string());
                    }
                }
            }
        }
    }
    names
}

/// Every `E_*` code the source can print.
fn error_codes() -> BTreeSet<String> {
    let mut codes = BTreeSet::new();
    let mut stack = vec![workspace().join("src")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("src is missing").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = fs::read_to_string(&path).unwrap();
                for piece in text.split('"') {
                    if piece.starts_with("E_")
                        && piece
                            .chars()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                    {
                        codes.insert(piece.to_string());
                    }
                }
            }
        }
    }
    codes
}

#[test]
fn the_docs_only_name_things_the_code_has() {
    let names = vocabulary();
    let codes = error_codes();
    let mut unknown = Vec::new();
    for (path, text) in shipped_markdown() {
        for token in snake_tokens(&text) {
            if !names.contains(&token) {
                unknown.push(format!("{}: `{token}`", path.display()));
            }
        }
        for piece in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if piece.starts_with("E_")
                && piece.len() > 2
                && piece
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                && !codes.contains(piece)
            {
                unknown.push(format!("{}: `{piece}`", path.display()));
            }
        }
    }
    unknown.sort();
    unknown.dedup();
    assert!(
        unknown.is_empty(),
        "the docs name things the code does not have (add a real one, or list an \
         unavoidable name in tests/docs_tokens.allow):\n{}",
        unknown.join("\n")
    );
}
