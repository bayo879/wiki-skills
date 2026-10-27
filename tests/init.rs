use std::path::Path;

use assert_cmd::Command;
use tempfile::tempdir;

/// Runs `llm-wiki --json <args...>` with `dir` as the working directory.
fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

/// Writes a `wiki.toml` fixture and loads it.
fn load_with(body: &str) -> llm_wiki::config::Config {
    let d = tempdir().unwrap();
    std::fs::write(d.path().join("wiki.toml"), body).unwrap();
    llm_wiki::config::Config::load(d.path()).unwrap()
}

#[test]
fn init_creates_contracts_and_engine_dirs() {
    let d = tempdir().unwrap();
    let out = run(d.path(), &["init"]);
    assert_eq!(out.status.code(), Some(0));
    for p in [
        "wiki.toml",
        "SCHEMA.md",
        "source",
        "raw",
        "wiki",
        "wiki/audit",
        ".wiki/cache",
        ".wiki/staging",
        ".wiki/versions",
    ] {
        assert!(d.path().join(p).exists(), "missing {p}");
    }
}

#[test]
fn init_is_idempotent_and_does_not_overwrite() {
    let d = tempdir().unwrap();
    run(d.path(), &["init"]);
    std::fs::write(d.path().join("wiki.toml"), "[paths]\nsource=\"S\"\n").unwrap();
    let out = run(d.path(), &["init"]);
    assert_eq!(out.status.code(), Some(0));
    let body = std::fs::read_to_string(d.path().join("wiki.toml")).unwrap();
    assert!(body.contains("source=\"S\""));
}

#[test]
fn init_reports_created_then_skipped() {
    let d = tempdir().unwrap();
    let first = run(d.path(), &["init"]);
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["cmd"], "init");
    let created = first["data"]["created"].as_array().unwrap();
    assert!(created.iter().any(|v| v == "wiki.toml"));
    assert!(created.iter().any(|v| v == ".wiki/staging"));

    let second = run(d.path(), &["init"]);
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    let skipped = second["data"]["skipped"].as_array().unwrap();
    assert!(skipped.iter().any(|v| v == "wiki.toml"));
    assert!(skipped.iter().any(|v| v == "source"));
    assert!(second["data"]["created"].as_array().unwrap().is_empty());
}

#[test]
fn init_force_rewrites_contracts_only() {
    let d = tempdir().unwrap();
    run(d.path(), &["init"]);
    std::fs::write(d.path().join("wiki.toml"), "[paths]\nsource=\"S\"\n").unwrap();
    std::fs::write(d.path().join("SCHEMA.md"), "hand written\n").unwrap();
    std::fs::create_dir_all(d.path().join("wiki/my-type")).unwrap();
    std::fs::write(d.path().join("source/keep.md"), "keep me\n").unwrap();

    let out = run(d.path(), &["init", "--force"]);
    assert_eq!(out.status.code(), Some(0));

    let body = std::fs::read_to_string(d.path().join("wiki.toml")).unwrap();
    assert!(
        !body.contains("source=\"S\""),
        "force must rewrite wiki.toml"
    );
    assert!(
        body.contains("max_source_bytes"),
        "template must be restored"
    );
    let schema = std::fs::read_to_string(d.path().join("SCHEMA.md")).unwrap();
    assert_ne!(schema, "hand written\n", "force must rewrite SCHEMA.md");
    assert!(d.path().join("source/keep.md").exists());
    assert!(d.path().join("wiki/my-type").is_dir());
}

#[test]
fn init_template_loads_and_validates() {
    let d = tempdir().unwrap();
    run(d.path(), &["init"]);
    let cfg = llm_wiki::config::Config::load(d.path()).unwrap();
    cfg.validate().unwrap();

    assert_eq!(cfg.paths.source, "source");
    assert_eq!(cfg.paths.engine, ".wiki");
    assert!(cfg.formats.allow.iter().any(|f| f == "pdf"));
    assert!(cfg.schema.types.iter().any(|t| t == "concept"));

    assert_eq!(cfg.parse.primary, llm_wiki::config::Primary::Auto);
    assert_eq!(cfg.parse.max_source_bytes, 100 * 1024 * 1024);
    assert_eq!(cfg.parse.chunk_threshold, 2 * 1024 * 1024);
    assert_eq!(cfg.parse.chunk_target, 64 * 1024);
    assert_eq!(cfg.parse.min_chars, 200);
    assert!(cfg.parse.external.is_none());

    assert_eq!(cfg.ingest.max_pages_per_source, 12);
    assert!(cfg.ingest.review_before_write);
    assert_eq!(cfg.audit.min_page_chars, 200);
    assert_eq!(cfg.versions.keep, 5);

    assert!(!cfg.sync.auto_ingest);
    assert_eq!(cfg.sync.max_per_run, 20);
    assert_eq!(cfg.sync.max_pending_sources, 200);
    assert_eq!(cfg.sync.settle_seconds, 10);
    assert_eq!(cfg.sync.stable_streak, 3);
    assert_eq!(cfg.sync.read_retries, 3);
    assert_eq!(cfg.sync.lag_warn_rounds, 3);
    assert_eq!(cfg.sync.on_missing, "mark_stale");
}

#[test]
fn init_template_documents_min_chars() {
    let d = tempdir().unwrap();
    run(d.path(), &["init"]);
    let body = std::fs::read_to_string(d.path().join("wiki.toml")).unwrap();
    assert!(
        body.contains("min_chars"),
        "the template must document the fallback threshold"
    );
    let cfg = llm_wiki::config::Config::load(d.path()).unwrap();
    assert_eq!(cfg.parse.min_chars, 200);
}

#[test]
fn min_chars_can_be_overridden() {
    let cfg = load_with("[parse]\nmin_chars = 500\n");
    assert_eq!(cfg.parse.min_chars, 500);
}

#[test]
fn config_load_reports_missing_file() {
    let d = tempdir().unwrap();
    let err = llm_wiki::config::Config::load(d.path()).unwrap_err();
    assert_eq!(err.code(), "E_NO_CONFIG");
    assert!(err.to_string().contains("llm-wiki init"), "{err}");
}

#[test]
fn partial_config_uses_documented_defaults() {
    let cfg = load_with("[paths]\nsource = \"S\"\n");
    assert_eq!(cfg.paths.source, "S");
    assert_eq!(cfg.paths.raw, "raw");
    assert_eq!(cfg.parse.max_source_bytes, 100 * 1024 * 1024);
    assert_eq!(cfg.versions.keep, 5);
    cfg.validate().unwrap();
}

#[test]
fn sizes_accept_units_and_plain_numbers() {
    let cfg = load_with(
        "[parse]\nchunk_threshold = \"1MB\"\nchunk_target = \"4KB\"\nmax_source_bytes = 1048576\n",
    );
    assert_eq!(cfg.parse.chunk_threshold, 1024 * 1024);
    assert_eq!(cfg.parse.chunk_target, 4 * 1024);
    assert_eq!(cfg.parse.max_source_bytes, 1024 * 1024);

    for (text, want) in [
        ("2MB", 2 * 1024 * 1024),
        ("64KB", 64 * 1024),
        ("100MB", 100 * 1024 * 1024),
        ("1024", 1024),
        ("1GB", 1024 * 1024 * 1024),
        ("10mb", 10 * 1024 * 1024),
        ("8B", 8),
    ] {
        assert_eq!(
            llm_wiki::config::parse_size(text).unwrap(),
            want,
            "parsing {text}"
        );
    }
}

#[test]
fn bad_size_strings_are_rejected_as_bad_config() {
    for bad in ["MB", "12XB", "-1MB", "2.5MB", ""] {
        assert!(
            llm_wiki::config::parse_size(bad).is_err(),
            "{bad:?} must not parse"
        );
        assert_eq!(
            llm_wiki::config::parse_size(bad).unwrap_err().code(),
            "E_BAD_CONFIG"
        );
    }

    let d = tempdir().unwrap();
    std::fs::write(
        d.path().join("wiki.toml"),
        "[parse]\nmax_source_bytes = \"lots\"\n",
    )
    .unwrap();
    let err = llm_wiki::config::Config::load(d.path()).unwrap_err();
    assert_eq!(err.code(), "E_BAD_CONFIG");
    assert!(err.to_string().contains("max_source_bytes"), "{err}");
}

#[test]
fn validate_rejects_empty_and_duplicate_types() {
    let cfg = load_with("[schema]\ntypes = []\n");
    assert_eq!(cfg.validate().unwrap_err().code(), "E_BAD_CONFIG");

    let cfg = load_with("[schema]\ntypes = [\"concept\", \"concept\"]\n");
    assert_eq!(cfg.validate().unwrap_err().code(), "E_BAD_CONFIG");
}

#[test]
fn validate_rejects_unknown_primary() {
    let d = tempdir().unwrap();
    std::fs::write(
        d.path().join("wiki.toml"),
        "[parse]\nprimary = \"network\"\n",
    )
    .unwrap();
    let err = llm_wiki::config::Config::load(d.path()).unwrap_err();
    assert_eq!(err.code(), "E_BAD_CONFIG");
}

#[test]
fn validate_rejects_token_in_auth_env() {
    for value in [
        "sk-live-abcdef",
        "ghp_secret",
        "eyJhbGciOi",
        &"x".repeat(41),
    ] {
        let cfg = load_with(&format!(
            "[parse.external]\nurl = \"http://127.0.0.1:9/parse\"\nauth_env = \"{value}\"\n"
        ));
        assert_eq!(
            cfg.validate().unwrap_err().code(),
            "E_TOKEN_IN_CONFIG",
            "value {value:?}"
        );
    }
}

#[test]
fn validate_rejects_a_bearer_literal_in_auth_env() {
    for value in ["Bearer abc123", "bearer abc", "  Bearer   token"] {
        let cfg = load_with(&format!(
            "[parse.external]\nurl = \"http://127.0.0.1:9/parse\"\nauth_env = \"{value}\"\n"
        ));
        assert_eq!(
            cfg.validate().unwrap_err().code(),
            "E_TOKEN_IN_CONFIG",
            "value {value:?}"
        );
    }
}

#[test]
fn validate_accepts_env_var_name_for_external() {
    let cfg = load_with(
        "[parse.external]\nurl = \"http://127.0.0.1:9/parse\"\nauth_env = \"WIKI_PARSE_TOKEN\"\n",
    );
    cfg.validate().unwrap();
    let ext = cfg.parse.external.unwrap();
    assert_eq!(ext.auth_env, "WIKI_PARSE_TOKEN");
    assert_eq!(ext.kind, "http");
    assert_eq!(ext.timeout_s, 120);
    assert_eq!(
        ext.fields.len(),
        1,
        "only the generic file part is defaulted"
    );
    assert_eq!(ext.fields.get("file").map(String::as_str), Some("@file"));
    assert_eq!(ext.response.text, "data");
    assert_eq!(ext.response.ok_code, 200);
}

#[test]
fn init_creates_archive_and_session_dirs() {
    let d = tempdir().unwrap();
    let out = run(d.path(), &["init"]);
    assert_eq!(out.status.code(), Some(0));
    // The archive holds sealed summaries (in git); the session directory is the
    // scratch space the layering fills and clears, so both must exist up front.
    for p in ["wiki/archive", ".wiki/sessions"] {
        assert!(d.path().join(p).is_dir(), "missing {p}");
    }
}

#[test]
fn generated_contract_carries_the_memory_defaults() {
    let d = tempdir().unwrap();
    run(d.path(), &["init"]);
    let cfg = llm_wiki::config::Config::load(d.path()).unwrap();
    assert_eq!(cfg.memory.session_soft_cap, 50);
    assert_eq!(cfg.memory.idle_days, 60);
    assert_eq!(cfg.memory.purge_cap, 200);
    assert_eq!(cfg.memory.purge_days, 180);
}

#[test]
fn memory_section_overrides_one_field_and_keeps_the_rest() {
    let cfg = load_with("[paths]\nsource = \"source\"\n\n[memory]\nidle_days = 7\n");
    assert_eq!(cfg.memory.idle_days, 7);
    assert_eq!(cfg.memory.session_soft_cap, 50);
    assert_eq!(cfg.memory.purge_cap, 200);
}

#[test]
fn init_writes_the_engine_gitignore() {
    let d = tempdir().unwrap();
    let out = run(d.path(), &["init"]);
    assert_eq!(out.status.code(), Some(0));
    let body = std::fs::read_to_string(d.path().join(".wiki/.gitignore")).unwrap();
    assert_eq!(
        body, "cache/\nstaging/\nsessions/\nstate.db\nstate.db-wal\nstate.db-shm\nlock\n",
        "the engine ignores what it can rebuild, and only that"
    );
}

#[test]
fn init_does_not_overwrite_a_hand_edited_engine_gitignore() {
    let d = tempdir().unwrap();
    run(d.path(), &["init"]);
    let path = d.path().join(".wiki/.gitignore");
    // `versions/` must stay committed: it is content, not a cache.
    std::fs::write(&path, "versions/\n").unwrap();

    let out = run(d.path(), &["init"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "versions/\n");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["data"]["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == ".wiki/.gitignore"),
        "a kept file is reported as skipped: {}",
        v["data"]
    );
}
