//! Consumer-visible output boundaries, exercised through the real CLI.
use std::process::{Command, Output};
use tempfile::tempdir;

fn invoke(root: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}

#[test]
fn normal_and_empty_reports_have_context() {
    let dir = tempdir().unwrap();
    for args in [
        vec!["--json", "ignore"],
        vec!["--json", "query", "不存在的材料"],
    ] {
        let out = invoke(dir.path(), &args);
        assert!(out.status.success(), "{:?}", out);
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["ok"], true);
        let context = &value["context"];
        for key in ["mode", "help", "summary"] {
            assert!(
                !context[key].as_str().unwrap_or("").is_empty(),
                "missing {key}: {value}"
            );
        }
        assert!(context["continuations"].is_array());
        let help = context["help"].as_str().unwrap();
        let args: Vec<_> = help.split_whitespace().skip(1).collect();
        let explained = invoke(dir.path(), &args);
        assert!(explained.status.success());
        assert!(String::from_utf8_lossy(&explained.stdout).contains("OUTPUT"));
    }
}

#[test]
fn help_and_version_stay_plain_text_with_json() {
    let dir = tempdir().unwrap();
    for args in [
        vec!["--json", "--help"],
        vec!["--json", "--version"],
        vec!["--json", "query", "--help"],
    ] {
        let out = invoke(dir.path(), &args);
        assert!(out.status.success());
        assert!(serde_json::from_slice::<serde_json::Value>(&out.stdout).is_err());
    }
    assert!(!dir.path().join(".wiki").exists());
}

fn context_of(dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let out = invoke(dir, args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["context"].clone()
}

#[test]
fn each_mode_has_its_own_facts() {
    let dir = tempdir().unwrap();
    let cases = [
        (vec!["--json", "ignore"], "ignore.list", "rule"),
        (vec!["--json", "doctor"], "doctor", "check"),
        (vec!["--json", "read", "schema"], "read", "line window"),
        (
            vec!["--json", "query", "--graph"],
            "query.graph",
            "wikilink",
        ),
        (
            vec!["--json", "task", "status"],
            "task.status",
            "without advancing",
        ),
    ];
    for (args, mode, summary_fact) in cases {
        let context = context_of(dir.path(), &args);
        assert_eq!(context["mode"], mode, "{args:?}");
        assert!(
            context["summary"].as_str().unwrap().contains(summary_fact),
            "{mode}: summary must state {summary_fact}: {context}"
        );
        assert!(context["help"].as_str().unwrap().contains("--help"));
    }
}

#[test]
fn every_subcommand_help_documents_its_output() {
    let dir = tempdir().unwrap();
    let cases = [
        (vec!["ingest", "--help"], "dry run"),
        (vec!["read", "--help"], "line window"),
        (vec!["doctor", "--help"], "Warn"),
        (vec!["task", "--help"], "settlement"),
        (vec!["task", "done", "--help"], "confirmed_sources"),
        (vec!["task", "status", "--help"], "does not prove"),
        (vec!["task", "retry", "--help"], "returned old row"),
        (vec!["query", "--help"], "not total matches"),
        (vec!["ignore", "--help"], "negative rules"),
    ];
    for (args, fact) in cases {
        let out = invoke(dir.path(), &args);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{args:?}");
        assert!(text.contains("OUTPUT"), "{args:?} lacks the output section");
        assert!(
            text.contains(fact),
            "{args:?} must explain {fact:?}:\n{text}"
        );
    }
}

fn page_repo() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let out = invoke(dir.path(), &["doctor", "--fix"]);
    assert!(out.status.success());
    let pages = dir.path().join(".wiki/pages/concept");
    std::fs::create_dir_all(&pages).unwrap();
    std::fs::write(
        pages.join("a.md"),
        "---\nid: a\ntitle: A\ntype: concept\nsummary: a\ntype: concept\nstatus: stable\nupdated: 2026-11-12\n---\n\n[[b]]\n",
    )
    .unwrap();
    std::fs::write(
        pages.join("b.md"),
        "---\nid: b\ntitle: B\ntype: concept\nsummary: b\nstatus: stable\nupdated: 2026-11-12\n---\n\n- [[a]]\n",
    )
    .unwrap();
    dir
}

#[test]
fn failures_explain_what_ok_false_proves() {
    let dir = tempdir().unwrap();
    let out = invoke(dir.path(), &["--json", "query"]);
    assert_eq!(out.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["ok"], false);
    let error = &value["error"];
    assert_eq!(error["exit"], 2);
    assert!(error["meaning"]
        .as_str()
        .unwrap()
        .contains("does not prove"));
    assert!(error["next"].as_str().unwrap().contains("llm-wiki"));
    assert!(error["state"].as_str().unwrap().contains("unknown"));

    let human = invoke(dir.path(), &["query"]);
    assert_eq!(human.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&human.stderr);
    assert!(stderr.contains("exit: 2"), "{stderr}");
    assert!(stderr.contains("meaning:"), "{stderr}");
    assert!(stderr.contains("next:"), "{stderr}");
}

#[test]
fn read_reports_empty_and_out_of_range_windows_without_failing() {
    let dir = tempdir().unwrap();
    assert!(invoke(dir.path(), &["doctor", "--fix"]).status.success());
    std::fs::write(dir.path().join(".wiki/raw/empty.txt"), "").unwrap();
    let empty = invoke(dir.path(), &["--json", "read", "raw:empty.txt"]);
    assert!(empty.status.success());
    let value: serde_json::Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert_eq!(value["data"]["total_lines"], 0);
    assert_eq!(value["data"]["line_start"], 0);
    assert_eq!(value["data"]["line_end"], 0);
    assert!(value["context"]["summary"]
        .as_str()
        .unwrap()
        .contains("empty"));

    std::fs::write(dir.path().join(".wiki/raw/two.txt"), "one\ntwo\n").unwrap();
    let beyond = invoke(
        dir.path(),
        &["--json", "read", "raw:two.txt", "--offset", "9"],
    );
    assert!(beyond.status.success());
    let value: serde_json::Value = serde_json::from_slice(&beyond.stdout).unwrap();
    assert_eq!(value["data"]["offset"], 9);
    assert_eq!(value["data"]["total_lines"], 2);
    assert_eq!(value["data"]["line_start"], 0);
    assert_eq!(value["data"]["line_end"], 0);
    assert!(value["context"]["summary"]
        .as_str()
        .unwrap()
        .contains("beyond the last line"));
}

#[test]
fn a_negated_ignore_rule_reports_not_ignored() {
    let dir = tempdir().unwrap();
    assert!(invoke(dir.path(), &["ignore", "--add", "a.md"])
        .status
        .success());
    assert!(invoke(dir.path(), &["ignore", "--add", "!a.md"])
        .status
        .success());
    let out = invoke(dir.path(), &["--json", "ignore", "--check", "a.md"]);
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["data"]["ignored"], false);
    assert_eq!(value["data"]["pattern"], "!a.md");
    assert!(value["context"]["summary"]
        .as_str()
        .unwrap()
        .contains("not ignored"));

    let human = invoke(dir.path(), &["ignore", "--check", "a.md"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("not ignored"), "{text}");
    assert!(text.contains("!a.md"), "{text}");
}

/// Runs a help entry and returns its plain text.
fn help_text(args: &[&str]) -> String {
    let dir = tempdir().unwrap();
    let mut full: Vec<&str> = args.to_vec();
    full.push("--help");
    let out = invoke(dir.path(), &full);
    assert!(out.status.success(), "{args:?} --help failed");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn every_documented_payload_key_is_named_in_its_help() {
    let dir = tempdir().unwrap();
    assert!(invoke(dir.path(), &["doctor", "--fix"]).status.success());
    std::fs::write(dir.path().join(".wiki/raw/two.txt"), "one\ntwo\n").unwrap();
    let read = invoke(
        dir.path(),
        &["--json", "read", "raw:two.txt", "--limit", "1"],
    );
    let value: serde_json::Value = serde_json::from_slice(&read.stdout).unwrap();
    let text = help_text(&["read"]);
    for key in value["data"].as_object().unwrap().keys() {
        assert!(text.contains(key.as_str()), "read help never names `{key}`");
    }

    assert!(invoke(dir.path(), &["ignore", "--add", "a.md"])
        .status
        .success());
    let check = invoke(dir.path(), &["--json", "ignore", "--check", "a.md"]);
    let value: serde_json::Value = serde_json::from_slice(&check.stdout).unwrap();
    let text = help_text(&["ignore"]);
    for key in value["data"].as_object().unwrap().keys() {
        assert!(
            text.contains(key.as_str()),
            "ignore help never names `{key}`"
        );
    }
}

#[test]
fn a_source_fragment_is_reported_and_dropped_from_the_read_command() {
    let dir = tempdir().unwrap();
    assert!(invoke(dir.path(), &["doctor", "--fix"]).status.success());
    std::fs::write(dir.path().join(".wiki/raw/note.md"), "one\ntwo\nthree\n").unwrap();
    let pages = dir.path().join(".wiki/pages/concept");
    std::fs::create_dir_all(&pages).unwrap();
    std::fs::write(
        pages.join("a.md"),
        "---\nid: a\ntitle: A\ntype: concept\nsummary: a\nstatus: stable\nupdated: 2026-11-12\nsources:\n  - \"raw:note.md#p=2\"\n---\n\nbody\n",
    )
    .unwrap();
    let out = invoke(
        dir.path(),
        &[
            "--json",
            "query",
            "--page",
            "concept/a",
            "--relation",
            "sources",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let sources = &value["data"]["sources"];
    assert_eq!(sources["requested"], true);
    assert_eq!(sources["total"], 1);
    let item = &sources["items"][0];
    assert_eq!(item["raw"], "raw:note.md#p=2");
    assert_eq!(item["raw_key"], "note.md");
    assert_eq!(item["fragment"], "p=2");
    assert_eq!(item["raw_available"], true);
    let command = item["read_command"].as_str().unwrap();
    assert!(!command.contains('#'), "{command}");
    assert!(command.contains("read 'raw:note.md'"), "{command}");

    // Both citation forms reach the same ledger key.
    let out = invoke(dir.path(), &["--json", "query", "--source", "raw:note.md"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["data"]["available"], true);
    assert_eq!(value["data"]["citing"]["total"], 1);
}

#[test]
fn unrequested_query_groups_are_named_not_left_empty() {
    let dir = page_repo();
    let out = invoke(
        dir.path(),
        &[
            "--json",
            "query",
            "--page",
            "concept/a",
            "--relation",
            "incoming",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let data = &value["data"];
    assert_eq!(data["incoming"]["requested"], true);
    assert_eq!(data["outgoing"]["requested"], false);
    assert_eq!(data["sources"]["requested"], false);

    let human = invoke(
        dir.path(),
        &["query", "--page", "concept/a", "--relation", "incoming"],
    );
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("outgoing (not requested)"), "{text}");
    assert!(text.contains("sources (not requested)"), "{text}");
}

#[test]
fn page_group_summary_quotes_the_payload_facts() {
    let dir = page_repo();
    let out = invoke(
        dir.path(),
        &[
            "--json",
            "query",
            "--page",
            "concept/a",
            "--relation",
            "incoming",
        ],
    );
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let summary = value["context"]["summary"].as_str().unwrap();
    assert!(summary.contains("incoming returned 1 of 1"), "{summary}");
    assert!(summary.contains("outgoing not requested"), "{summary}");
    assert!(summary.contains("sources not requested"), "{summary}");
    assert_ne!(
        value["data"]["incoming"]["total"], 0,
        "the summary must not claim an empty group: {value}"
    );
}

#[test]
fn a_truncated_text_hit_carries_an_executable_next_command() {
    let dir = tempdir().unwrap();
    assert!(invoke(dir.path(), &["doctor", "--fix"]).status.success());
    let pages = dir.path().join(".wiki/pages/concept");
    std::fs::create_dir_all(&pages).unwrap();
    let mut hub = String::from(
        "---\nid: hub\ntitle: Hubtoken\ntype: concept\nsummary: hubtoken\nstatus: stable\nupdated: 2026-11-12\n---\n\n",
    );
    for index in 1..=25 {
        std::fs::write(
            pages.join(format!("l{index}.md")),
            format!(
                "---\nid: l{index}\ntitle: L{index}\ntype: concept\nsummary: l{index}\nstatus: stable\nupdated: 2026-11-12\n---\n\nleaf\n"
            ),
        )
        .unwrap();
        hub.push_str(&format!("[[l{index}]]\n"));
    }
    std::fs::write(pages.join("hub.md"), hub).unwrap();

    let out = invoke(dir.path(), &["--json", "query", "hubtoken"]);
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let hit = &value["data"]["hits"][0];
    assert_eq!(hit["page"], ".wiki/pages/concept/hub.md", "{value}");
    let outgoing = &hit["outgoing"];
    assert_eq!(outgoing["total"], 25);
    assert_eq!(outgoing["items"].as_array().unwrap().len(), 20);
    assert_eq!(outgoing["truncated"], true);
    assert_eq!(outgoing["next_offset"], 20);
    let command = outgoing["next_command"].as_str().unwrap();
    assert!(command.contains("--offset 20"), "{command}");

    // The command is shell-quoted; undo the quotes for direct execution. The
    // continuation itself is a human command, so ask for JSON explicitly here.
    let mut args = vec!["--json"];
    args.extend(
        command
            .split_whitespace()
            .skip(1)
            .map(|arg| arg.trim_matches('\'')),
    );
    let next = invoke(dir.path(), &args);
    assert!(
        next.status.success(),
        "{}",
        String::from_utf8_lossy(&next.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&next.stdout).unwrap();
    assert_eq!(
        value["data"]["outgoing"]["items"].as_array().unwrap().len(),
        5
    );
    assert_eq!(value["data"]["outgoing"]["requested"], true);
}

#[test]
fn an_ambiguous_or_missing_page_is_an_error_not_an_empty_group() {
    let dir = tempdir().unwrap();
    assert!(invoke(dir.path(), &["doctor", "--fix"]).status.success());
    for (dir_name, kind) in [("concept", "concept"), ("note", "note")] {
        let pages = dir.path().join(format!(".wiki/pages/{dir_name}"));
        std::fs::create_dir_all(&pages).unwrap();
        std::fs::write(
            pages.join("same.md"),
            format!(
                "---\nid: same\ntitle: Same\ntype: {kind}\nsummary: same\nstatus: stable\nupdated: 2026-11-12\n---\n\nbody\n"
            ),
        )
        .unwrap();
    }
    let ambiguous = invoke(dir.path(), &["--json", "query", "--page", "same"]);
    // An unusable reference is a usage error, not a partial report.
    assert_eq!(ambiguous.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&ambiguous.stdout).unwrap();
    assert_eq!(value["error"]["code"], "E_PAGE_AMBIGUOUS");
    assert!(value["error"]["meaning"]
        .as_str()
        .unwrap()
        .contains("does not prove"));
    assert!(value["error"]["state"]
        .as_str()
        .unwrap()
        .contains("unknown"));

    let missing = invoke(dir.path(), &["--json", "query", "--page", "nope"]);
    assert_eq!(missing.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(value["error"]["code"], "E_PAGE_NOT_FOUND");
}
