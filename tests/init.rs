use std::collections::BTreeSet;
use std::path::Path;

use tempfile::tempdir;

/// Runs the initialiser the way `doctor --fix` does.
fn init(dir: &Path) -> llm_wiki::init::InitOutcome {
    llm_wiki::init::run(dir, false).expect("init succeeds")
}

/// Writes a `wiki.toml` fixture and loads it.
fn load_with(body: &str) -> llm_wiki::config::Config {
    let d = tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(".wiki")).unwrap();
    std::fs::write(d.path().join(".wiki/wiki.toml"), body).unwrap();
    llm_wiki::config::Config::load(d.path()).unwrap()
}

#[test]
fn init_creates_contracts_and_engine_dirs() {
    let d = tempdir().unwrap();
    init(d.path());
    for p in [
        ".wiki/wiki.toml",
        ".wiki/SCHEMA.md",
        ".wiki/raw",
        ".wiki/pages",
        ".wiki/pages/audit",
        ".wiki/cache",
    ] {
        assert!(d.path().join(p).exists(), "missing {p}");
    }
    for legacy in [".wiki/staging", ".wiki/objects", ".wiki/versions"] {
        assert!(!d.path().join(legacy).exists(), "must not create {legacy}");
    }
}

#[test]
fn init_is_idempotent_and_does_not_overwrite() {
    let d = tempdir().unwrap();
    init(d.path());
    std::fs::write(
        d.path().join(".wiki/wiki.toml"),
        "[schema]\ntypes = [\"only\"]\n",
    )
    .unwrap();
    init(d.path());
    let body = std::fs::read_to_string(d.path().join(".wiki/wiki.toml")).unwrap();
    assert!(body.contains("types = [\"only\"]"));
}

#[test]
fn init_reports_created_then_skipped() {
    let d = tempdir().unwrap();
    let first = init(d.path());
    assert!(first.created.iter().any(|v| v == ".wiki/wiki.toml"));
    assert!(first.created.iter().any(|v| v == ".wiki/pages"));

    let second = init(d.path());
    assert!(second.skipped.iter().any(|v| v == ".wiki/wiki.toml"));
    assert!(second.skipped.iter().any(|v| v == ".wiki/pages"));
    assert!(second.created.is_empty());
}

/// Top-level table names of a TOML document, in the order they appear.
fn sections(text: &str) -> std::collections::BTreeSet<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix('['))
        .filter_map(|rest| rest.split(']').next())
        .filter(|name| !name.contains('.'))
        .map(|name| name.trim().to_string())
        .collect()
}

#[test]
fn the_template_names_every_config_section() {
    // One source of truth: the template must document exactly the sections
    // `Config` accepts. A new section needs both a field and a template line.
    let d = tempdir().unwrap();
    init(d.path());
    let template = std::fs::read_to_string(d.path().join(".wiki/wiki.toml")).unwrap();
    let defaults =
        toml::to_string(&llm_wiki::config::Config::default()).expect("config serializes");
    assert_eq!(
        sections(&template),
        sections(&defaults),
        "the template and Config disagree on sections"
    );
    assert!(
        template.contains("session_dir"),
        "the template must spell out the session directory knob"
    );
}

#[test]
fn init_ends_with_the_fixed_follow_up_prompt() {
    let d = tempdir().unwrap();
    let outcome = init(d.path());
    let next = outcome.next.as_str();
    let root = d.path();
    assert!(
        next.contains(&format!("`{}`", root.display())),
        "the prompt must name the repository it just initialised: {next}"
    );
    assert!(next.contains("llm-wiki ingest"), "{next}");
    assert!(
        next.contains("01:00"),
        "the default period must be stated: {next}"
    );
}

#[test]
fn init_force_rewrites_contracts_only() {
    let d = tempdir().unwrap();
    init(d.path());
    std::fs::write(
        d.path().join(".wiki/wiki.toml"),
        "[schema]\ntypes = [\"only\"]\n",
    )
    .unwrap();
    std::fs::write(d.path().join(".wiki/SCHEMA.md"), "hand written\n").unwrap();
    std::fs::create_dir_all(d.path().join(".wiki/pages/my-type")).unwrap();
    std::fs::write(d.path().join("keep.md"), "keep me\n").unwrap();

    llm_wiki::init::run(d.path(), true).expect("init --force");

    let body = std::fs::read_to_string(d.path().join(".wiki/wiki.toml")).unwrap();
    assert!(
        !body.contains("types = [\"only\"]"),
        "force must rewrite wiki.toml"
    );
    assert!(
        body.contains("max_source_bytes"),
        "template must be restored"
    );
    let schema = std::fs::read_to_string(d.path().join(".wiki/SCHEMA.md")).unwrap();
    assert_ne!(schema, "hand written\n", "force must rewrite SCHEMA.md");
    assert!(d.path().join("keep.md").exists());
    assert!(d.path().join(".wiki/pages/my-type").is_dir());
}

#[test]
fn init_template_loads_and_validates() {
    let d = tempdir().unwrap();
    init(d.path());
    let cfg = llm_wiki::config::Config::load(d.path()).unwrap();
    cfg.validate().unwrap();

    assert_eq!(cfg.paths.source, "");
    assert_eq!(cfg.paths.raw, ".wiki/raw");
    assert_eq!(cfg.paths.wiki, ".wiki/pages");
    assert_eq!(cfg.paths.engine, ".wiki");
    assert!(cfg.formats.allow.iter().any(|f| f == "pdf"));
    assert!(cfg.schema.types.iter().any(|t| t == "concept"));

    assert_eq!(cfg.parse.primary, llm_wiki::config::Primary::Auto);
    assert_eq!(cfg.parse.max_source_bytes, 100 * 1024 * 1024);
    assert_eq!(cfg.parse.chunk_threshold, 2 * 1024 * 1024);
    assert_eq!(cfg.parse.chunk_target, 64 * 1024);
    assert_eq!(cfg.parse.min_chars, 200);
    assert!(cfg.parse.external.is_none());

    assert_eq!(cfg.ingest.max_pages_per_slice, 5);
    assert_eq!(cfg.audit.min_page_chars, 200);
    assert_eq!(cfg.versions.keep, 5);

    assert_eq!(cfg.ingest.max_per_run, 20);
    assert_eq!(cfg.ingest.max_pending_sources, 200);
    assert_eq!(cfg.ingest.settle_seconds, 300);
    assert_eq!(cfg.ingest.read_retries, 3);
    assert_eq!(cfg.ingest.lag_warn_rounds, 3);
    assert_eq!(cfg.ingest.on_missing, "mark_stale");
}

#[test]
fn init_template_documents_min_chars() {
    let d = tempdir().unwrap();
    init(d.path());
    let body = std::fs::read_to_string(d.path().join(".wiki/wiki.toml")).unwrap();
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
    assert!(err.to_string().contains("doctor --fix"), "{err}");
}

#[test]
fn partial_config_uses_documented_defaults() {
    let cfg = load_with("[schema]\ntypes = [\"concept\"]\n");
    assert_eq!(cfg.paths.source, "");
    assert_eq!(cfg.paths.raw, ".wiki/raw");
    assert_eq!(cfg.paths.wiki, ".wiki/pages");
    assert_eq!(cfg.parse.max_source_bytes, 100 * 1024 * 1024);
    assert_eq!(cfg.versions.keep, 5);
    cfg.validate().unwrap();
}

#[test]
fn a_hand_edited_paths_section_is_rejected() {
    // The layout is not configurable, so `[paths]` is an unknown section and
    // `deny_unknown_fields` refuses the file outright instead of silently
    // ignoring it. The engine directory therefore cannot be moved by hand.
    let d = tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(".wiki")).unwrap();
    std::fs::write(
        d.path().join(".wiki/wiki.toml"),
        "[paths]\nsource = \"elsewhere\"\n",
    )
    .unwrap();
    let err = llm_wiki::config::Config::load(d.path()).unwrap_err();
    assert!(err.to_string().contains("unknown field `paths`"), "{err}");
    assert_eq!(err.code(), "E_BAD_CONFIG");
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
    std::fs::create_dir_all(d.path().join(".wiki")).unwrap();
    std::fs::write(
        d.path().join(".wiki/wiki.toml"),
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
    std::fs::create_dir_all(d.path().join(".wiki")).unwrap();
    std::fs::write(
        d.path().join(".wiki/wiki.toml"),
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
fn init_defaults_the_session_dir_to_the_sessions_directory() {
    let d = tempdir().unwrap();
    init(d.path());
    let cfg = llm_wiki::config::Config::load(d.path()).expect("contract");
    assert_eq!(cfg.ingest.session_dir, "sessions");
}

#[test]
fn generated_contract_carries_the_ingest_defaults() {
    let d = tempdir().unwrap();
    init(d.path());
    let cfg = llm_wiki::config::Config::load(d.path()).unwrap();
    assert_eq!(cfg.ingest.max_pages_per_slice, 5);
    assert_eq!(cfg.ingest.max_new_pages_per_run, 20);
    assert_eq!(cfg.ingest.session_dir, "sessions");
}

/// Collects every leaf key path of a TOML value, `a.b.c` style.
fn key_paths(value: &toml::Value, prefix: &str, out: &mut BTreeSet<String>) {
    match value {
        toml::Value::Table(table) => {
            for (key, child) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                key_paths(child, &path, out);
            }
        }
        _ => {
            out.insert(String::from(prefix));
        }
    }
}

#[test]
fn the_generated_contract_names_exactly_the_keys_the_loader_accepts() {
    // The template is the only discoverable list of configuration keys, so a
    // key added to `Config` but not to the template is invisible to whoever
    // reads `wiki.toml`. Comparing leaf paths both ways keeps the promise the
    // header makes: every default is written out.
    let d = tempdir().unwrap();
    init(d.path());
    let text = std::fs::read_to_string(d.path().join(".wiki/wiki.toml")).expect("generated");
    let template: toml::Value = toml::from_str(&text).expect("the template is TOML");
    let cfg = llm_wiki::config::Config::load(d.path()).expect("the template loads");
    let serialised = toml::Value::try_from(&cfg).expect("Config serialises to TOML");

    let mut in_template = BTreeSet::new();
    key_paths(&template, "", &mut in_template);
    let mut in_config = BTreeSet::new();
    key_paths(&serialised, "", &mut in_config);
    assert_eq!(
        in_template, in_config,
        "the generated contract and the Config type disagree; add the missing key to WIKI_TOML"
    );
}

#[test]
fn init_leaves_git_configuration_to_the_repository() {
    // `.wiki/` is local state, not a git-managed artifact: the tool must not
    // create a `.gitignore` inside it. Repositories that use git put `.wiki/`
    // in their own root ignore file; repositories without git do nothing.
    let d = tempdir().unwrap();
    init(d.path());
    assert!(
        !d.path().join(".wiki/.gitignore").exists(),
        "init must not write a git-related file under .wiki/"
    );
}
