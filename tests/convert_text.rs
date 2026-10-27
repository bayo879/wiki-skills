//! Signature tests for the `builtin-text` backend and `convert`.
//!
//! The first test is the plan's original scenario; the rest cover the properties
//! the plan leaves implicit but that later tasks (chunking, ingest, lint) build
//! on: newline normalisation, frontmatter stripping, the fixed key order, the
//! `raw/` key collision rule, idempotency, and YAML-safe escaping of `source`.

use assert_cmd::Command;
use chrono::DateTime;
use llm_wiki::store::SourceRow;
use llm_wiki::{config::Config, convert, init, parse, store::Store};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn setup() -> (tempfile::TempDir, Store, Config) {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    (d, s, cfg)
}

/// A ledger row with an explicit content digest.
fn ledger_row(key: &str, hash: &str, size: i64) -> SourceRow {
    SourceRow {
        key: String::from(key),
        hash: String::from(hash),
        size,
        mtime: 1,
        raw_key: None,
        converted_at: None,
    }
}

/// The body of a raw file: everything after the closing `---` line.
fn body_of(raw: &str) -> &str {
    let mut opened = false;
    let mut offset = 0;
    for line in raw.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end() == "---" {
            if opened {
                return &raw[offset..];
            }
            opened = true;
        }
    }
    raw
}

/// Reads one top-level frontmatter field, undoing the double-quote escaping.
fn frontmatter_value(raw: &str, name: &str) -> Option<String> {
    let mut opened = false;
    for line in raw.lines() {
        let line = line.trim_end();
        if line == "---" {
            if opened {
                break;
            }
            opened = true;
            continue;
        }
        if !opened {
            continue;
        }
        if let Some(value) = line
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            return Some(unquote(value.trim()));
        }
    }
    None
}

fn unquote(value: &str) -> String {
    if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
        return String::from(value);
    }
    let mut out = String::new();
    let mut chars = value[1..value.len() - 1].chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Raw content with the volatile `converted_at` line removed.
fn without_converted_at(raw: &str) -> String {
    raw.lines()
        .filter(|line| !line.starts_with("converted_at:"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn json_of(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout is not JSON: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[test]
fn md_source_becomes_raw_with_traceability_frontmatter() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    fs::create_dir_all(d.path().join("source/子目录")).unwrap();
    fs::write(
        d.path().join("source/子目录/说明.md"),
        "---\ntitle: 忽略\n---\n# 正文\n\n内容\n",
    )
    .unwrap();

    let out = convert::convert_one(d.path(), &cfg, &mut s, "子目录/说明.md").unwrap();
    assert_eq!(out.raw_path, "raw/子目录/说明.md");
    assert_eq!(out.backend, "builtin-text");
    let raw = fs::read_to_string(d.path().join("raw/子目录/说明.md")).unwrap();
    assert!(raw.contains("source: source/子目录/说明.md"));
    assert!(raw.contains("parser: builtin-text"));
    assert!(raw.contains("pin: false"));
    assert!(
        !raw.contains("title: 忽略"),
        "源文件的 frontmatter 必须被剥掉"
    );
    assert_eq!(body_of(&raw), "# 正文\n\n内容\n");
    assert_eq!(out.chars, body_of(&raw).chars().count());
}

#[test]
fn crlf_is_normalised_to_lf() {
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("source/a.txt"), "a\r\nb\r\n").unwrap();

    convert::convert_one(d.path(), &cfg, &mut s, "a.txt").unwrap();
    let raw = fs::read_to_string(d.path().join("raw/a.md")).unwrap();
    assert_eq!(body_of(&raw), "a\nb\n");
}

#[test]
fn a_crlf_file_with_frontmatter_still_loses_its_frontmatter() {
    let (d, mut s, cfg) = setup();
    fs::write(
        d.path().join("source/a.md"),
        "---\r\ntitle: 标题\r\n---\r\n正文\r\n",
    )
    .unwrap();

    convert::convert_one(d.path(), &cfg, &mut s, "a.md").unwrap();
    let raw = fs::read_to_string(d.path().join("raw/a.md")).unwrap();
    assert_eq!(body_of(&raw), "正文\n");
    assert!(!raw.contains("title: 标题"));
}

#[test]
fn plain_text_without_frontmatter_passes_through() {
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("source/b.md"), "普通文本\n第二行\n").unwrap();

    let out = convert::convert_one(d.path(), &cfg, &mut s, "b.md").unwrap();
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    let raw = fs::read_to_string(d.path().join("raw/b.md")).unwrap();
    assert_eq!(body_of(&raw), "普通文本\n第二行\n");
}

#[test]
fn source_that_is_all_frontmatter_warns_empty() {
    let (d, mut s, cfg) = setup();
    fs::write(
        d.path().join("source/empty.md"),
        "---\ntitle: 只有 frontmatter\n---\n",
    )
    .unwrap();

    let out = convert::convert_one(d.path(), &cfg, &mut s, "empty.md").unwrap();
    assert!(
        out.warnings.iter().any(|warning| warning.code == "empty"),
        "expected an `empty` warning, got {:?}",
        out.warnings
    );
    assert_eq!(out.chars, 0);
}

#[test]
fn raw_frontmatter_key_order_is_fixed() {
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("source/a.md"), "# 正文\n").unwrap();

    convert::convert_one(d.path(), &cfg, &mut s, "a.md").unwrap();
    let raw = fs::read_to_string(d.path().join("raw/a.md")).unwrap();
    let keys: Vec<&str> = raw
        .lines()
        .skip(1)
        .take_while(|line| *line != "---")
        .map(|line| line.split(':').next().unwrap())
        .collect();
    // The text backend never sets `asset` / `no_text_layer`, so the fixed order
    // collapses to these five keys; asset variants are covered by the unit test
    // on the renderer.
    assert_eq!(
        keys,
        ["source", "source_hash", "parser", "converted_at", "pin"]
    );
}

#[test]
fn raw_key_collision_is_refused() {
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("source/a.md"), "# from md\n").unwrap();
    fs::write(d.path().join("source/a.txt"), "# from txt\n").unwrap();

    convert::convert_one(d.path(), &cfg, &mut s, "a.md").unwrap();
    let before = fs::read_to_string(d.path().join("raw/a.md")).unwrap();

    let err = convert::convert_one(d.path(), &cfg, &mut s, "a.txt").unwrap_err();
    assert_eq!(err.code(), "E_RAW_COLLISION");
    assert!(err.to_string().contains("raw/a.md"), "{err}");

    // The first conversion's output must be untouched.
    let after = fs::read_to_string(d.path().join("raw/a.md")).unwrap();
    assert_eq!(after, before);
    assert_eq!(
        frontmatter_value(&after, "source").as_deref(),
        Some("source/a.md")
    );
    assert_eq!(body_of(&after), "# from md\n");
}

#[test]
fn a_foreign_raw_file_without_an_owner_is_never_overwritten() {
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("source/a.md"), "# 正文\n").unwrap();
    // A hand-made raw file: nothing proves it came from `source/a.md`, so as far
    // as conversion can tell it belongs to someone else.
    fs::write(d.path().join("raw/a.md"), "# 手工文件\n").unwrap();

    let err = convert::convert_one(d.path(), &cfg, &mut s, "a.md").unwrap_err();
    assert_eq!(err.code(), "E_RAW_COLLISION");
    assert_eq!(
        fs::read_to_string(d.path().join("raw/a.md")).unwrap(),
        "# 手工文件\n"
    );
}

#[test]
fn reconverting_a_changed_source_overwrites_its_own_raw() {
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("source/a.md"), "# 第一版\n").unwrap();
    let first = convert::convert_one(d.path(), &cfg, &mut s, "a.md").unwrap();

    fs::write(d.path().join("source/a.md"), "# 第二版\n").unwrap();
    let second = convert::convert_one(d.path(), &cfg, &mut s, "a.md").unwrap();

    assert_eq!(first.raw_path, second.raw_path);
    assert!(!first.skipped);
    assert!(
        !second.skipped,
        "an unpinned raw of the same source is replaced"
    );
    let raw = fs::read_to_string(d.path().join("raw/a.md")).unwrap();
    assert_eq!(body_of(&raw), "# 第二版\n");
    assert_eq!(frontmatter_value(&raw, "pin").as_deref(), Some("false"));
}

#[test]
fn a_pinned_raw_file_is_left_alone() {
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("source/a.md"), "# 生成版\n").unwrap();
    convert::convert_one(d.path(), &cfg, &mut s, "a.md").unwrap();
    let converted_at = s.get_source("a.md").unwrap().unwrap().converted_at;

    // A human pins the raw file and edits its body; a later conversion must not
    // undo that edit, because the pinned body is now the source of truth.
    let pinned = fs::read_to_string(d.path().join("raw/a.md"))
        .unwrap()
        .replace("pin: false", "pin: true")
        .replace("# 生成版", "# 手改版");
    fs::write(d.path().join("raw/a.md"), &pinned).unwrap();

    fs::write(d.path().join("source/a.md"), "# 又改版\n").unwrap();
    let out = convert::convert_one(d.path(), &cfg, &mut s, "a.md").unwrap();

    assert!(out.skipped, "a pinned raw must not be rewritten");
    assert_eq!(out.skip_reason.as_deref(), Some("pinned"));
    assert_eq!(out.chars, 0);
    assert_eq!(out.backend, "builtin-text");
    assert_eq!(
        fs::read_to_string(d.path().join("raw/a.md")).unwrap(),
        pinned,
        "the pinned file must be byte-for-byte untouched"
    );
    let row = s.get_source("a.md").unwrap().unwrap();
    assert_eq!(row.converted_at, converted_at, "a skip must not re-stamp");
    assert_eq!(row.raw_key.as_deref(), Some("raw/a.md"));
}

#[test]
fn a_pinned_raw_of_another_source_is_still_a_collision() {
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("source/a.md"), "# md\n").unwrap();
    fs::write(d.path().join("source/a.txt"), "# txt\n").unwrap();
    convert::convert_one(d.path(), &cfg, &mut s, "a.md").unwrap();
    let pinned = fs::read_to_string(d.path().join("raw/a.md"))
        .unwrap()
        .replace("pin: false", "pin: true");
    fs::write(d.path().join("raw/a.md"), &pinned).unwrap();

    // Pin protects the body, not the owner: another source still cannot take it.
    let err = convert::convert_one(d.path(), &cfg, &mut s, "a.txt").unwrap_err();
    assert_eq!(err.code(), "E_RAW_COLLISION");
    assert_eq!(
        fs::read_to_string(d.path().join("raw/a.md")).unwrap(),
        pinned
    );
}

#[test]
fn converting_twice_is_idempotent_apart_from_converted_at() {
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("source/a.md"), "# 正文\n").unwrap();

    convert::convert_one(d.path(), &cfg, &mut s, "a.md").unwrap();
    let first = fs::read_to_string(d.path().join("raw/a.md")).unwrap();
    convert::convert_one(d.path(), &cfg, &mut s, "a.md").unwrap();
    let second = fs::read_to_string(d.path().join("raw/a.md")).unwrap();

    assert_eq!(without_converted_at(&first), without_converted_at(&second));
    let stamp = frontmatter_value(&second, "converted_at").unwrap();
    assert!(
        DateTime::parse_from_rfc3339(&stamp).is_ok(),
        "converted_at must be RFC 3339: {stamp:?}"
    );
    assert_eq!(
        s.get_source("a.md").unwrap().unwrap().raw_key.as_deref(),
        Some("raw/a.md")
    );
}

#[test]
fn source_name_with_hash_is_quoted_and_round_trips() {
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("source/说明#1.md"), "# 标题\n").unwrap();

    convert::convert_one(d.path(), &cfg, &mut s, "说明#1.md").unwrap();
    let raw = fs::read_to_string(d.path().join("raw/说明#1.md")).unwrap();
    let line = raw
        .lines()
        .find(|line| line.starts_with("source:"))
        .unwrap();
    assert!(
        line.trim_start_matches("source:").trim().starts_with('"'),
        "a value containing `#` must be quoted: {line}"
    );
    assert_eq!(
        frontmatter_value(&raw, "source").as_deref(),
        Some("source/说明#1.md")
    );

    // A second conversion must recognise its own quoted output and not report a
    // collision against itself.
    convert::convert_one(d.path(), &cfg, &mut s, "说明#1.md").unwrap();
}

#[cfg(unix)]
#[test]
fn source_name_with_colon_space_and_quote_is_escaped() {
    let name = "a b\"c:d.md";
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("source").join(name), "# 正文\n").unwrap();

    convert::convert_one(d.path(), &cfg, &mut s, name).unwrap();
    let raw = fs::read_to_string(d.path().join("raw").join(name)).unwrap();
    assert_eq!(
        frontmatter_value(&raw, "source").as_deref(),
        Some(format!("source/{name}").as_str())
    );
    // The escaped value must survive another pass.
    convert::convert_one(d.path(), &cfg, &mut s, name).unwrap();
}

#[test]
fn conversion_records_raw_key_without_touching_other_ledger_fields() {
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("source/a.md"), "alpha").unwrap();
    let row = ledger_row(
        "a.md",
        "blake3:0000000000000000000000000000000000000000000000000000000000000000",
        5,
    );
    s.upsert_source(&row).unwrap();

    convert::convert_one(d.path(), &cfg, &mut s, "a.md").unwrap();
    let got = s.get_source("a.md").unwrap().unwrap();
    assert_eq!(got.key, row.key);
    assert_eq!(
        got.hash, row.hash,
        "the content digest must stay as recorded"
    );
    assert_eq!(got.size, row.size);
    assert_eq!(got.mtime, row.mtime);
    assert_eq!(got.raw_key.as_deref(), Some("raw/a.md"));
    assert!(got.converted_at.is_some());
}

#[test]
fn converting_an_unscanned_source_keeps_the_next_scan_quiet() {
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("source/fresh.md"), "# 新\n").unwrap();

    // `convert --key` synthesises the ledger row; the digest it guesses must
    // match what `scan` computes, or the source looks new again right after.
    convert::convert_one(d.path(), &cfg, &mut s, "fresh.md").unwrap();
    let report = llm_wiki::scan::scan_source(d.path(), &cfg, &s).unwrap();
    assert!(
        report.changes.iter().all(|change| change.key != "fresh.md"),
        "a converted source must not look new or changed: {:?}",
        report.changes
    );
}

#[test]
fn invalid_utf8_is_replaced_with_a_warning() {
    let (d, mut s, cfg) = setup();
    let mut bytes = b"# ok\n".to_vec();
    bytes.push(0xff);
    bytes.push(b'\n');
    fs::write(d.path().join("source/bad.md"), &bytes).unwrap();

    let out = convert::convert_one(d.path(), &cfg, &mut s, "bad.md").unwrap();
    assert!(
        out.warnings
            .iter()
            .any(|warning| warning.code == "bad_utf8"),
        "expected a `bad_utf8` warning, got {:?}",
        out.warnings
    );
    let raw = fs::read_to_string(d.path().join("raw/bad.md")).unwrap();
    assert!(body_of(&raw).contains('\u{fffd}'));
}

#[test]
fn text_backend_claims_markdown_and_txt_only() {
    let cfg = Config::default();
    let backends = parse::registry(&cfg);
    assert_eq!(backends.len(), 1);
    let backend = backends.first().unwrap();
    assert_eq!(backend.id(), "builtin-text");
    for ext in ["md", "MD", "markdown", "Markdown", "txt", "TXT"] {
        assert!(backend.can_handle(ext), "{ext} must be claimed");
    }
    for ext in ["pdf", "html", "png", ""] {
        assert!(!backend.can_handle(ext), "{ext} must not be claimed");
    }

    let (extracted, id) =
        parse::parse_with(&cfg, &parse::Source::inline("A.MD", b"# hi\n")).unwrap();
    assert_eq!(id, "builtin-text");
    assert_eq!(extracted.markdown, "# hi\n");

    let err = parse::parse_with(&cfg, &parse::Source::inline("a.pdf", b"%PDF")).unwrap_err();
    assert_eq!(err.code(), "E_NO_PARSER");
}

#[test]
fn cli_parse_does_not_write_raw_or_ledger() {
    let (d, mut s, _cfg) = setup();
    let body = "x".repeat(250);
    fs::write(d.path().join("source/a.md"), &body).unwrap();
    s.upsert_source(&ledger_row("a.md", "blake3:placeholder", 250))
        .unwrap();

    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json", "parse", "--key", "a.md"])
        .current_dir(d.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let value = json_of(&out);
    assert_eq!(value["ok"], true);
    assert_eq!(value["cmd"], "parse");
    assert_eq!(value["data"]["backend"], "builtin-text");
    assert_eq!(value["data"]["chars"], 250);
    // `markdown_head` is the first 200 characters, so it is a prefix of the body.
    let head = value["data"]["markdown_head"].as_str().unwrap();
    assert_eq!(head.chars().count(), 200);
    assert!(body.starts_with(head));
    assert!(value["data"]["warnings"].is_array());

    // Nothing may be written: no raw file, no ledger change.
    assert!(
        fs::read_dir(d.path().join("raw")).unwrap().next().is_none(),
        "parse must not create raw files"
    );
    assert_eq!(
        s.get_source("a.md").unwrap().unwrap().raw_key,
        None,
        "parse must not record a raw key"
    );
}

#[test]
fn cli_parse_truncates_the_head_to_200_chars() {
    let (d, _s, _cfg) = setup();
    let body = "y".repeat(250);
    fs::write(d.path().join("source/long.md"), &body).unwrap();

    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json", "parse", "--key", "long.md"])
        .current_dir(d.path())
        .output()
        .unwrap();
    let value = json_of(&out);
    let head = value["data"]["markdown_head"].as_str().unwrap();
    assert_eq!(head.chars().count(), 200);
    assert!(body.starts_with(head));
}

#[test]
fn cli_convert_emits_converted_array_and_respects_limit() {
    let (d, _s, _cfg) = setup();
    for key in ["a.md", "b.md", "c.md"] {
        fs::write(d.path().join("source").join(key), format!("# {key}\n")).unwrap();
    }

    let first = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json", "convert", "--limit", "1"])
        .current_dir(d.path())
        .output()
        .unwrap();
    assert_eq!(first.status.code(), Some(0), "{first:?}");
    let value = json_of(&first);
    assert_eq!(value["ok"], true);
    assert_eq!(value["cmd"], "convert");
    let converted = value["data"]["converted"].as_array().unwrap();
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0]["raw_path"], "raw/a.md");
    assert_eq!(converted[0]["backend"], "builtin-text");
    assert!(converted[0]["chars"].is_number());
    assert!(converted[0]["duration_ms"].is_number());
    assert!(converted[0]["warnings"].is_array());
    assert_eq!(converted[0]["skipped"], false);
    assert!(converted[0]["skip_reason"].is_null());
    assert!(Path::new(&d.path().join("raw/a.md")).is_file());
    assert!(!Path::new(&d.path().join("raw/b.md")).exists());

    // Each run takes the next source in key order, and a converted source stops
    // being pending, so the limit moves the frontier forward instead of redoing
    // the same file.
    let second = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json", "convert", "--limit", "1"])
        .current_dir(d.path())
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(0), "{second:?}");
    let value = json_of(&second);
    let converted = value["data"]["converted"].as_array().unwrap();
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0]["raw_path"], "raw/b.md");
    assert!(!Path::new(&d.path().join("raw/c.md")).exists());

    let third = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json", "convert"])
        .current_dir(d.path())
        .output()
        .unwrap();
    assert_eq!(third.status.code(), Some(0), "{third:?}");
    let value = json_of(&third);
    let converted = value["data"]["converted"].as_array().unwrap();
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0]["raw_path"], "raw/c.md");
    assert!(Path::new(&d.path().join("raw/c.md")).is_file());
}

#[test]
fn convert_without_a_key_discovers_new_sources_by_scanning() {
    let (d, _s, _cfg) = setup();
    fs::write(d.path().join("source/a.md"), "# A\n").unwrap();
    fs::write(d.path().join("source/b.md"), "# B\n").unwrap();

    // No ledger rows exist: the pending set has to come from the filesystem, or
    // the first `convert` of a repository would find nothing to do.
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json", "convert", "--limit", "10"])
        .current_dir(d.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let value = json_of(&out);
    let converted = value["data"]["converted"].as_array().unwrap();
    assert_eq!(converted.len(), 2, "{value}");
    assert_eq!(converted[0]["raw_path"], "raw/a.md");
    assert_eq!(converted[1]["raw_path"], "raw/b.md");
    assert!(Path::new(&d.path().join("raw/a.md")).is_file());
    assert!(Path::new(&d.path().join("raw/b.md")).is_file());

    let scan = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json", "scan"])
        .current_dir(d.path())
        .output()
        .unwrap();
    assert_eq!(scan.status.code(), Some(0), "{scan:?}");
    let value = json_of(&scan);
    assert_eq!(
        value["data"]["changes"].as_array().unwrap().len(),
        0,
        "converting must leave nothing new or changed behind: {value}"
    );
}

#[test]
fn cli_convert_by_key_sees_a_source_it_never_scanned() {
    let (d, _s, _cfg) = setup();
    fs::write(d.path().join("source/lone.md"), "# 孤独\n").unwrap();

    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json", "convert", "--key", "lone.md"])
        .current_dir(d.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let value = json_of(&out);
    assert_eq!(
        value["data"]["converted"][0]["raw_path"], "raw/lone.md",
        "a source missing from the ledger must still convert"
    );
}

#[test]
fn cli_convert_reports_a_raw_collision() {
    let (d, _s, _cfg) = setup();
    fs::write(d.path().join("source/a.md"), "# md\n").unwrap();
    fs::write(d.path().join("source/a.txt"), "# txt\n").unwrap();

    let ok = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json", "convert", "--key", "a.md"])
        .current_dir(d.path())
        .output()
        .unwrap();
    assert_eq!(ok.status.code(), Some(0), "{ok:?}");

    let err = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json", "convert", "--key", "a.txt"])
        .current_dir(d.path())
        .output()
        .unwrap();
    assert_eq!(err.status.code(), Some(1), "{err:?}");
    let value = json_of(&err);
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "E_RAW_COLLISION");
}

#[test]
fn cli_parse_of_a_missing_source_is_a_runtime_error() {
    let (d, _s, _cfg) = setup();

    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json", "parse", "--key", "nope.md"])
        .current_dir(d.path())
        .output()
        .unwrap();
    // A missing file is discovered at runtime, so it is a finding (exit 1), not
    // a malformed invocation (exit 2).
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let value = json_of(&out);
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "E_IO");
}
