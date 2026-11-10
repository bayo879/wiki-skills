//! What the shipped documentation claims, held against what the binary does.
//!
//! The shipped skill and `docs/readme/` describe a JSON shape, a set of flags and
//! a set of error codes. Nothing in the compiler connects those sentences to
//! the code, so this file does it mechanically:
//!
//! * the JSON key shape of each documented command is snapshotted, so a field
//!   that appears or disappears shows up in review;
//! * every `--flag` named in the docs must exist in some `--help`;
//! * every backticked `snake_case` name in the docs must be a JSON key, a
//!   `wiki.toml` key, a `SCHEMA.md` field or a name listed in
//!   `tests/docs_tokens.allow`;
//! * every `E_*` code named in the docs must exist in `src/`;
//! * the skill must not quote `data.*` field paths (only the `error.*` envelope
//!   is allowed), so shapes stay the CLI's business.
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
    let _ = run_json_allowing(root, &["doctor", "--fix"], &[0, 1]);

    fs::write(
        root.join("a.md"),
        "# A\n\nThe retry limit is three attempts.\n",
    )
    .unwrap();
    fs::write(root.join("b.md"), "# B\n\nThe timeout is five seconds.\n").unwrap();
    // The shape capture expects a round that converts: backdate the sources past
    // `[ingest].settle_seconds` rather than turn the window off.
    for name in ["a.md", "b.md"] {
        let file = fs::File::options()
            .write(true)
            .open(root.join(name))
            .unwrap();
        file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(600))
            .unwrap();
    }
    dir
}

fn write_page(root: &Path, name: &str, body: &str) {
    let dir = root.join(".wiki/pages").join("concept");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(name),
        format!(
            "---\nid: {}\ntitle: {name}\ntype: concept\nsummary: about retries\nsources:\n  - .wiki/raw/a.md\nstatus: stable\nupdated: 2026-10-27\n---\n\n{body}\n",
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

    capture(&mut out, "doctor", &["doctor"]);
    capture(&mut out, "ingest-dry-run", &["ingest", "--dry-run"]);

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

    capture(&mut out, "query", &["query", "retries"]);
    capture(&mut out, "query-graph", &["query", "--graph"]);
    capture(&mut out, "ingest", &["ingest"]);
    let ingest = run_json(root, &["ingest"]);
    let digest = ingest["data"]["pending"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["source_key"] == "a.md")
        .unwrap()["raw_hash"]
        .as_str()
        .unwrap();
    capture(&mut out, "done", &["done", "a.md", "--hash", digest]);
    capture(&mut out, "ignore", &["ignore"]);
    out
}

fn snapshot_path() -> PathBuf {
    workspace()
        .join("tests")
        .join("snapshots")
        .join("cli-keys.json")
}

/// The shipped workflow must not send authors back to the retired publish API.
#[test]
fn direct_page_workflow_is_the_only_shipped_workflow() {
    let root = workspace();
    for relative in [
        "README.md",
        "skills/llm-wiki/SKILL.md",
        "skills/llm-wiki/content.md",
        "skills/llm-wiki/ops.md",
        "skills/llm-wiki-session/SKILL.md",
    ] {
        let text = fs::read_to_string(root.join(relative)).unwrap();
        for retired in [
            "llm-wiki submit",
            "llm-wiki version ",
            "`submit`",
            "`version restore",
            ".wiki/staging/<type>",
        ] {
            assert!(
                !text.contains(retired),
                "{relative}: retired workflow {retired}"
            );
        }
    }
    let skill = fs::read_to_string(root.join("skills/llm-wiki/SKILL.md")).unwrap();
    for required in ["done", "实际读取", "raw", "直接", "并发", "备份"] {
        assert!(
            skill.contains(required),
            "skill is missing the direct-page boundary: {required}"
        );
    }
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

/// The skill must not restate a JSON field path.
///
/// The contract the reposition rests on: whatever the agent needs to know about
/// a command's output is printed by the command — `ingest` says where the
/// author writes live pages and which schema applies. Only the envelope
/// keys survive in prose, because they are how a failure is read at all.
#[test]
fn the_skill_states_no_json_field_paths() {
    let allowed = ["error.code", "error.message", "error.detail"];
    let skills = workspace().join("skills");
    let mut found = Vec::new();
    for (path, text) in shipped_markdown() {
        if !path.starts_with(&skills) {
            continue;
        }
        for line in text.lines() {
            for (at, _) in line.match_indices("data.") {
                found.push(format!("{}:{}: {}", path.display(), at + 1, line.trim()));
            }
            for (at, _) in line.match_indices("error.") {
                if !allowed.iter().any(|name| line[at..].starts_with(name)) {
                    found.push(format!("{}:{}: {}", path.display(), at + 1, line.trim()));
                }
            }
        }
    }
    found.sort();
    found.dedup();
    assert!(
        found.is_empty(),
        "the skill restates a JSON field path; make the CLI hand it over instead:\n{}",
        found.join("\n")
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
    llm_wiki::init::run(dir.path(), false).expect("init");
    let toml =
        fs::read_to_string(dir.path().join(".wiki/wiki.toml")).expect("init wrote no wiki.toml");
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
    let schema =
        fs::read_to_string(dir.path().join(".wiki/SCHEMA.md")).expect("init wrote no SCHEMA.md");
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
