//! Signature tests for UTF-8-safe chunking and on-demand chunk reads.
//!
//! `split` is pure and must never cut a character in half; `materialize` writes
//! one file per range without ever overwriting an existing one; `read_range`
//! serves a 1-based inclusive window without touching the other chunks. The
//! large-file tests use an 8 MB body so a reader that slurps everything would
//! show up as a digest mismatch or a decode error.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use llm_wiki::chunk;
use tempfile::tempdir;

/// Asserts that `ranges` partition `text` exactly, on character boundaries.
fn assert_partition(text: &str, ranges: &[(usize, usize)]) {
    if text.is_empty() {
        assert!(ranges.is_empty(), "empty text must yield no ranges");
        return;
    }
    assert!(!ranges.is_empty(), "non-empty text must yield ranges");
    assert_eq!(ranges[0].0, 0, "the first range must start at zero");
    assert_eq!(
        ranges.last().unwrap().1,
        text.len(),
        "the last range must end at the text length"
    );
    let mut expected_start = 0;
    for &(start, end) in ranges {
        assert_eq!(start, expected_start, "ranges must be contiguous");
        assert!(start < end, "ranges must be non-empty and increasing");
        assert!(end <= text.len());
        assert!(
            text.is_char_boundary(start),
            "cut at {start} is inside a character"
        );
        assert!(
            text.is_char_boundary(end),
            "cut at {end} is inside a character"
        );
        expected_start = end;
    }
    let joined: String = ranges.iter().map(|&(a, b)| &text[a..b]).collect();
    assert_eq!(joined, text, "the ranges must reassemble the text exactly");
}

#[test]
fn reassembly_is_lossless() {
    let text = "中文行\n".repeat(200_000);
    let ranges = chunk::split(&text, 64 * 1024);
    assert!(
        ranges.len() > 10,
        "expected many chunks, got {}",
        ranges.len()
    );
    let joined: String = ranges.iter().map(|&(a, b)| &text[a..b]).collect();
    assert_eq!(joined, text);
    assert_partition(&text, &ranges);
}

#[test]
fn boundaries_respect_utf8_and_prefer_newlines() {
    let emoji = "😀".repeat(1000);
    for (start, end) in chunk::split(&emoji, 64) {
        assert!(emoji.is_char_boundary(start));
        assert!(emoji.is_char_boundary(end));
    }

    let lines = "abc\n".repeat(100);
    let ranges = chunk::split(&lines, 10);
    let bytes = lines.as_bytes();
    for &(_, end) in &ranges {
        assert!(
            end == lines.len() || bytes[end] == b'\n' || bytes[end - 1] == b'\n',
            "cut at {end} is neither at a newline nor at the end"
        );
    }
    assert_partition(&lines, &ranges);
}

#[test]
fn materialize_is_idempotent() {
    let d = tempdir().unwrap();
    let text = "x".repeat(200_000);
    let first = chunk::materialize(d.path(), "h1", &text, 64 * 1024).unwrap();
    let second = chunk::materialize(d.path(), "h1", &text, 64 * 1024).unwrap();
    assert_eq!(first, second);
    assert!(first.len() > 1);
}

#[test]
fn empty_text_has_no_chunks() {
    let d = tempdir().unwrap();
    assert!(chunk::split("", 64).is_empty());
    let paths = chunk::materialize(d.path(), "empty", "", 64).unwrap();
    assert!(paths.is_empty());
    assert!(!d.path().join("empty").exists(), "no directory for no text");
    let err = chunk::read_range(d.path(), "empty", 1, 1).unwrap_err();
    assert_eq!(err.code(), "E_BAD_ARGS");
}

#[test]
fn target_larger_than_text_is_one_chunk() {
    assert_eq!(chunk::split("abc", 1024), vec![(0, 3)]);
}

#[test]
fn zero_target_yields_no_ranges_and_is_rejected_by_materialize() {
    assert!(chunk::split("abc", 0).is_empty());
    let d = tempdir().unwrap();
    let err = chunk::materialize(d.path(), "z", "abc", 0).unwrap_err();
    assert_eq!(err.code(), "E_BAD_ARGS");
}

#[test]
fn chunk_paths_sort_in_numeric_order() {
    let d = tempdir().unwrap();
    let text = "x".repeat(25 * 40);
    let ranges = chunk::split(&text, 40);
    assert_eq!(ranges.len(), 25);
    let paths = chunk::materialize(d.path(), "h25", &text, 40).unwrap();
    assert_eq!(paths.len(), 25);
    for (i, path) in paths.iter().enumerate() {
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            format!("chunk-{:04}.md", i + 1)
        );
    }
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(
        sorted, paths,
        "lexicographic order must equal numeric order"
    );
}

#[test]
fn read_range_joins_the_first_two_chunks() {
    let d = tempdir().unwrap();
    let text = "line line\n".repeat(500);
    let ranges = chunk::split(&text, 100);
    let paths = chunk::materialize(d.path(), "join", &text, 100).unwrap();
    assert!(ranges.len() > 2);

    let got = chunk::read_range(d.path(), "join", 1, 2).unwrap();
    let expected = text[..ranges[1].1].to_string();
    assert_eq!(got, expected);
    let mut files = String::new();
    for path in &paths[..2] {
        files.push_str(&fs::read_to_string(path).unwrap());
    }
    assert_eq!(got, files);
}

#[test]
fn read_range_rejects_bad_bounds_without_panicking() {
    let d = tempdir().unwrap();
    let text = "abc\n".repeat(100);
    chunk::materialize(d.path(), "bounds", &text, 16).unwrap();
    for (from, to) in [(0, 1), (1, 99), (2, 1)] {
        let err = chunk::read_range(d.path(), "bounds", from, to).unwrap_err();
        assert_eq!(err.code(), "E_BAD_ARGS", "range {from}-{to}");
    }
}

#[test]
fn crlf_is_never_split_between_cr_and_lf() {
    let text = "Hello, world!\r\nSecond line here.\r\n".repeat(50);
    let bytes = text.as_bytes();
    for target in 2..40 {
        let ranges = chunk::split(&text, target);
        assert_partition(&text, &ranges);
        for &(_, end) in &ranges[..ranges.len() - 1] {
            assert!(
                !(bytes[end] == b'\n' && bytes[end - 1] == b'\r'),
                "target {target} cut between CR and LF at {end}"
            );
        }
    }
}

#[test]
fn materialize_never_overwrites_an_edited_chunk() {
    let d = tempdir().unwrap();
    let text = "x".repeat(300);
    let paths = chunk::materialize(d.path(), "keep", &text, 100).unwrap();
    fs::write(&paths[0], "edited by hand").unwrap();
    let again = chunk::materialize(d.path(), "keep", &text, 100).unwrap();
    assert_eq!(again, paths);
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "edited by hand");
}

#[test]
fn mixed_chinese_and_emoji_chunk_safely_for_tiny_targets() {
    let text = "中文😀abc\n".repeat(500);
    for target in [1, 2, 3, 7, 13] {
        let ranges = chunk::split(&text, target);
        assert_partition(&text, &ranges);
        for &(start, end) in &ranges {
            assert!(!text[start..end].is_empty());
        }
    }
}

#[test]
fn read_range_reads_only_the_requested_chunks() {
    let d = tempdir().unwrap();
    let text = "y".repeat(8 * 1024 * 1024);
    let paths = chunk::materialize(d.path(), "big", &text, 64 * 1024).unwrap();
    assert_eq!(paths.len(), 128);
    // A chunk outside the requested window is damaged: a reader that walks the
    // whole cache fails on it, while an on-demand reader never opens it.
    fs::write(&paths[99], b"\xff\xfe").unwrap();

    let got = chunk::read_range(d.path(), "big", 1, 2).unwrap();
    assert_eq!(got, text[..2 * 64 * 1024]);
}

/// A repo whose parse budget is small enough that a few kilobytes are "large".
fn tiny_repo() -> tempfile::TempDir {
    let d = tempdir().unwrap();
    fs::create_dir(d.path().join("source")).unwrap();
    fs::write(
        d.path().join("wiki.toml"),
        "[parse]\nchunk_threshold = \"1KB\"\nmax_source_bytes = \"10KB\"\nchunk_target = \"256B\"\n",
    )
    .unwrap();
    d
}

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
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

/// The single hash directory under `.wiki/cache`, after on-demand materialization.
fn only_cache_dir(repo: &Path) -> PathBuf {
    let cache = repo.join(".wiki/cache");
    let mut dirs: Vec<PathBuf> = fs::read_dir(&cache)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(dirs.len(), 1, "expected one hash directory: {dirs:?}");
    dirs.remove(0)
}

#[test]
fn cli_small_source_stays_whole() {
    let d = tiny_repo();
    fs::write(d.path().join("source/small.md"), "hello").unwrap();

    let out = run(d.path(), &["--json", "parse", "--key", "small.md"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let value = json_of(&out);
    assert_eq!(value["data"]["mode"], "full");
    assert_eq!(value["data"]["backend"], "builtin-text");
    assert_eq!(value["data"]["chars"], 5);
    assert_eq!(value["data"]["markdown_head"], "hello");
    assert!(value["data"]["warnings"].is_array());
    assert!(
        !d.path().join(".wiki").exists(),
        "a whole-source parse must not materialize chunks"
    );
}

#[test]
fn cli_large_source_reports_chunked_mode_and_materializes() {
    let d = tiny_repo();
    let body = "0123456789abcdef\n".repeat(400);
    fs::write(d.path().join("source/big.md"), &body).unwrap();

    let out = run(d.path(), &["--json", "parse", "--key", "big.md"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let value = json_of(&out);
    assert_eq!(value["data"]["mode"], "chunked");
    assert_eq!(value["data"]["backend"], "builtin-text");
    assert_eq!(value["data"]["chars"], body.chars().count());
    assert!(
        value["data"].get("markdown_head").is_none(),
        "chunked mode must not echo the whole text"
    );

    let chunks = value["data"]["chunks"].as_u64().unwrap() as usize;
    assert!(chunks > 1, "expected several chunks: {value}");
    let rel = value["data"]["chunks_dir"].as_str().unwrap();
    assert!(rel.starts_with(".wiki/cache/"), "{rel}");
    let dir = d.path().join(rel);
    assert!(dir.is_dir(), "chunks_dir must exist: {rel}");

    let mut joined = String::new();
    for i in 1..=chunks {
        joined.push_str(&fs::read_to_string(dir.join(format!("chunk-{i:04}.md"))).unwrap());
    }
    assert_eq!(joined, body);
}

#[test]
fn cli_chunk_flag_materializes_on_demand() {
    let d = tiny_repo();
    let body = "abcdefghij".repeat(300);
    fs::write(d.path().join("source/demand.md"), &body).unwrap();

    let out = run(
        d.path(),
        &["--json", "parse", "--key", "demand.md", "--chunk", "2"],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let value = json_of(&out);
    assert_eq!(value["data"]["mode"], "chunk");
    assert_eq!(value["data"]["chunk"], 2);
    let chunks = value["data"]["chunks"].as_u64().unwrap() as usize;
    assert!(chunks > 2);

    let dir = only_cache_dir(d.path());
    let file = dir.join("chunk-0002.md");
    assert!(file.is_file(), "the requested chunk must be materialized");
    let markdown = value["data"]["markdown"].as_str().unwrap();
    assert_eq!(markdown, fs::read_to_string(&file).unwrap());
    assert_eq!(value["data"]["chars"], markdown.chars().count());
}

#[test]
fn cli_range_flag_joins_cached_chunks() {
    let d = tiny_repo();
    let body = "0123456789abcdef\n".repeat(400);
    fs::write(d.path().join("source/big.md"), &body).unwrap();

    let out = run(
        d.path(),
        &["--json", "parse", "--key", "big.md", "--range", "1-2"],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let value = json_of(&out);
    assert_eq!(value["data"]["mode"], "range");
    assert_eq!(value["data"]["from"], 1);
    assert_eq!(value["data"]["to"], 2);

    let dir = only_cache_dir(d.path());
    let expected = format!(
        "{}{}",
        fs::read_to_string(dir.join("chunk-0001.md")).unwrap(),
        fs::read_to_string(dir.join("chunk-0002.md")).unwrap()
    );
    assert_eq!(value["data"]["markdown"], expected);
    assert_eq!(
        value["data"]["chunks"].as_u64().unwrap() as usize,
        fs::read_dir(&dir).unwrap().count()
    );
}

#[test]
fn cli_rejects_invalid_chunk_and_range_arguments() {
    let d = tiny_repo();
    fs::write(d.path().join("source/big.md"), "abcdefghij".repeat(300)).unwrap();

    for args in [
        vec!["--json", "parse", "--key", "big.md", "--chunk", "0"],
        vec!["--json", "parse", "--key", "big.md", "--chunk", "99"],
        vec!["--json", "parse", "--key", "big.md", "--range", "3"],
        vec!["--json", "parse", "--key", "big.md", "--range", "0-1"],
        vec!["--json", "parse", "--key", "big.md", "--range", "2-1"],
    ] {
        let out = run(d.path(), &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        let value = json_of(&out);
        assert_eq!(value["ok"], false, "{args:?}");
        assert_eq!(value["error"]["code"], "E_BAD_ARGS", "{args:?}");
    }
}

#[test]
fn cli_rejects_a_source_above_max_source_bytes() {
    let d = tiny_repo();
    fs::write(d.path().join("source/huge.md"), "x".repeat(11 * 1024)).unwrap();

    let out = run(d.path(), &["--json", "parse", "--key", "huge.md"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let value = json_of(&out);
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "E_OVERSIZE");
}

#[test]
fn cli_rejects_chunk_and_range_together() {
    let d = tiny_repo();
    fs::write(d.path().join("source/big.md"), "abcdefghij".repeat(300)).unwrap();

    let out = run(
        d.path(),
        &[
            "--json", "parse", "--key", "big.md", "--chunk", "1", "--range", "1-2",
        ],
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let value = json_of(&out);
    assert_eq!(value["error"]["code"], "E_USAGE");
}
