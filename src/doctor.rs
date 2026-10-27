//! `doctor`: pre-flight checks on a wiki repository.
//!
//! Every other command assumes the repository is sound and reports one error
//! when it is not. `doctor` exists because "not sound" has a dozen causes that
//! look alike from the failing command: a contract that never parsed, a schema
//! file deleted, staging half-torn, a database left behind by a newer build.
//! It runs all the checks it can, in one pass, and never stops at the first
//! problem.
//!
//! Two rules keep it honest:
//!
//! - it only reads. Nothing here creates, migrates or repairs anything, so the
//!   report describes the repository as it arrived rather than as the check
//!   left it;
//! - a check that cannot run is reported, not skipped. The unknown is a
//!   finding.
//!
//! The levels mean what they say. [`Level::Fail`] is "a command will fail or
//! corrupt something if you continue"; [`Level::Warn`] is "this is suspicious
//! but survivable". The process exit code is `1` when any check failed and `0`
//! otherwise, so a CI step can gate on `doctor` without parsing its output.
//!
//! `external_backend` deliberately fails harder than its table entry in the
//! plan says. `parse.primary = external` with no `[parse.external]` section is
//! a contract that cannot work: every parse reaches the backend lookup and
//! fails with `E_NO_EXTERNAL`. The config validator does not reject it because
//! the section is optional under `auto`, so `doctor` is the only place that can
//! catch it before the first parse. A configured but unreachable backend stays
//! a warning: the service may simply be down, and `doctor` should not fail a
//! laptop that is offline.

use std::fs;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::config::{self, Config, Primary};
use crate::lock;
use crate::staging;
use crate::store::{self, Store, StoreError};

/// The contract file at the repository root.
pub const CONFIG_FILE: &str = "wiki.toml";

/// The schema description `init` writes next to the contract.
pub const SCHEMA_FILE: &str = "SCHEMA.md";

/// The sections a complete contract carries. A missing one is not fatal — the
/// loader defaults it — but it usually means the file was hand-trimmed.
const SECTIONS: [&str; 9] = [
    "paths", "formats", "schema", "parse", "ingest", "audit", "versions", "sync", "memory",
];

/// The ledger file inside the engine directory.
const DB_FILE: &str = "state.db";

/// The lock file. `lock.rs` fixes it at `.wiki/lock`, so this check looks in
/// the same place the commands do rather than at the configured engine path.
const LOCK_FILE: &str = "lock";

/// How long a TCP probe may take before the backend is called unreachable.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Frontmatter keys whose value is a credential, never a variable name.
const CREDENTIAL_KEYS: [&str; 7] = [
    "token",
    "api_key",
    "apikey",
    "secret",
    "password",
    "authorization",
    "bearer",
];

/// Severity of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// The check passed.
    Ok,
    /// Suspicious, but a command would still run.
    Warn,
    /// A command will fail, or the repository is already inconsistent.
    Fail,
}

impl Level {
    /// Uppercase short name for the human table (`ok`, `warn`, `FAIL`).
    pub fn label(&self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        }
    }
}

/// One check's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// Stable machine-readable name, such as `external_backend`.
    pub name: &'static str,
    /// Severity.
    pub level: Level,
    /// One sentence a human can act on.
    pub detail: String,
}

impl Check {
    /// A passing check.
    fn ok(name: &'static str, detail: impl Into<String>) -> Check {
        Check {
            name,
            level: Level::Ok,
            detail: detail.into(),
        }
    }

    /// A suspicious but survivable state.
    fn warn(name: &'static str, detail: impl Into<String>) -> Check {
        Check {
            name,
            level: Level::Warn,
            detail: detail.into(),
        }
    }

    /// A state that will make a command fail.
    fn fail(name: &'static str, detail: impl Into<String>) -> Check {
        Check {
            name,
            level: Level::Fail,
            detail: detail.into(),
        }
    }
}

/// Runs every check, in report order.
///
/// `cfg` is the contract the caller already loaded, when it has one. The
/// contract is still read from disk here: a caller that could not load it
/// passes `None` and `doctor` reports the parse error itself instead of
/// refusing to run.
pub fn run(root: &Path, cfg: Option<&Config>) -> Vec<Check> {
    let text = fs::read_to_string(root.join(CONFIG_FILE)).ok();
    let parsed = text
        .as_deref()
        .and_then(|text| toml::from_str::<Config>(text).ok());
    let fallback = Config::default();
    let effective = cfg.or(parsed.as_ref()).unwrap_or(&fallback);

    vec![
        check_config(root, text.as_deref()),
        check_schema(root),
        check_paths(root, effective),
        check_db(root, effective),
        check_fts5(),
        check_engine_ignore(root, effective),
        check_versions_tracked(root),
        check_lock(root, effective),
        check_staging(root),
        check_token(text.as_deref()),
        check_external_backend(effective),
    ]
}

/// `config`: the contract exists, parses, validates, and carries its sections.
fn check_config(root: &Path, text: Option<&str>) -> Check {
    const NAME: &str = "config";
    let path = root.join(CONFIG_FILE);
    let Some(text) = text else {
        return Check::fail(
            NAME,
            format!("{} is missing; run `llm-wiki init`", path.display()),
        );
    };

    let value: toml::Value = match text.parse() {
        Ok(value) => value,
        Err(err) => return Check::fail(NAME, format!("{} does not parse: {err}", path.display())),
    };
    let cfg: Config = match toml::from_str(text) {
        Ok(cfg) => cfg,
        Err(err) => return Check::fail(NAME, format!("{} is invalid: {err}", path.display())),
    };
    if let Err(err) = cfg.validate() {
        return Check::fail(NAME, format!("{} is invalid: {err}", path.display()));
    }

    let missing: Vec<&str> = SECTIONS
        .iter()
        .copied()
        .filter(|section| !value.get(*section).is_some_and(toml::Value::is_table))
        .collect();
    if missing.is_empty() {
        Check::ok(
            NAME,
            format!("{} parses and carries every section", CONFIG_FILE),
        )
    } else {
        Check::warn(
            NAME,
            format!(
                "{} omits [{}]; the built-in defaults apply, so a typo in a section name is silent",
                CONFIG_FILE,
                missing.join("], [")
            ),
        )
    }
}

/// `schema`: the schema description the agent reads is present.
fn check_schema(root: &Path) -> Check {
    const NAME: &str = "schema";
    if root.join(SCHEMA_FILE).is_file() {
        Check::ok(NAME, format!("{SCHEMA_FILE} is present"))
    } else {
        Check::fail(
            NAME,
            format!("{SCHEMA_FILE} is missing; run `llm-wiki init`"),
        )
    }
}

/// `paths`: the four directories the commands write into exist.
fn check_paths(root: &Path, cfg: &Config) -> Check {
    const NAME: &str = "paths";
    let wanted = [
        ("source", &cfg.paths.source),
        ("raw", &cfg.paths.raw),
        ("wiki", &cfg.paths.wiki),
        ("engine", &cfg.paths.engine),
    ];
    let missing: Vec<String> = wanted
        .iter()
        .filter(|(_, path)| !root.join(path).is_dir())
        .map(|(name, path)| format!("{name} ({path})"))
        .collect();
    if missing.is_empty() {
        Check::ok(NAME, "source/, raw/, wiki/ and the engine directory exist")
    } else {
        Check::fail(
            NAME,
            format!(
                "{} missing; run `llm-wiki init` or fix [paths]",
                missing.join(", ")
            ),
        )
    }
}

/// `db`: the ledger exists and this build can read it.
///
/// A missing database is not a failure: every command creates it on first use,
/// so a fresh `init` must come back green. A corrupt file, a file a newer build
/// wrote, or one that cannot be opened at all is a failure — those are the
/// states that make the next command lose data or refuse to run.
fn check_db(root: &Path, cfg: &Config) -> Check {
    const NAME: &str = "db";
    let path = root.join(&cfg.paths.engine).join(DB_FILE);
    if !path.is_file() {
        return Check::warn(
            NAME,
            format!(
                "{} does not exist yet; the first scan or query creates it",
                path.display()
            ),
        );
    }

    let store = match Store::open(&path) {
        Ok(store) => store,
        Err(StoreError::SchemaTooNew {
            found, supported, ..
        }) => {
            return Check::fail(
                NAME,
                format!(
                    "{} was written by a newer build (schema {found}, this build reads up to {supported})",
                    path.display()
                ),
            )
        }
        Err(err) => {
            return Check::fail(
                NAME,
                format!("{} cannot be opened: {err}", path.display()),
            )
        }
    };
    match store.user_version() {
        Ok(version) if version > store::SCHEMA_VERSION => Check::fail(
            NAME,
            format!(
                "{} reports schema {version}, but this build reads up to {}; upgrade the CLI",
                path.display(),
                store::SCHEMA_VERSION
            ),
        ),
        Ok(version) => Check::ok(
            NAME,
            format!("{} opens at schema {version}", path.display()),
        ),
        Err(err) => Check::fail(
            NAME,
            format!("{} has no readable schema version: {err}", path.display()),
        ),
    }
}

/// `fts5`: the build can maintain the full-text index.
fn check_fts5() -> Check {
    fts5_check(store::fts5_supported())
}

/// The verdict for one FTS5 probe, split out so the false branch is testable.
fn fts5_check(available: store::Result<bool>) -> Check {
    const NAME: &str = "fts5";
    match available {
        Ok(true) => Check::ok(NAME, "SQLite was built with FTS5"),
        Ok(false) => Check::fail(
            NAME,
            "SQLite was built without FTS5; `query` and the index cannot work",
        ),
        Err(err) => Check::fail(NAME, format!("cannot inspect the SQLite build: {err}")),
    }
}

/// `engine_ignore`: `.wiki/.gitignore` hides the engine's runtime files.
fn check_engine_ignore(root: &Path, cfg: &Config) -> Check {
    const NAME: &str = "engine_ignore";
    let path = root.join(&cfg.paths.engine).join(".gitignore");
    let display = path.strip_prefix(root).unwrap_or(&path);
    match fs::read_to_string(&path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Check::warn(
            NAME,
            format!(
                "{} is missing; cache/, staging/ and state.db would be committed",
                display.display()
            ),
        ),
        Err(err) => Check::warn(NAME, format!("cannot read {}: {err}", display.display())),
        Ok(text) => {
            if is_ignored_by(&text, "versions") {
                Check::warn(
                    NAME,
                    format!(
                        "{} ignores versions/; checkpoint history would stay out of git",
                        display.display()
                    ),
                )
            } else {
                Check::ok(
                    NAME,
                    format!(
                        "{} hides the runtime files and keeps versions/ tracked",
                        display.display()
                    ),
                )
            }
        }
    }
}

/// `versions_tracked`: the checkpoint snapshots are not ignored by git.
fn check_versions_tracked(root: &Path) -> Check {
    const NAME: &str = "versions_tracked";
    let path = root.join(".gitignore");
    match fs::read_to_string(&path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Check::ok(NAME, "no root .gitignore, so .wiki/versions/ is tracked")
        }
        Err(err) => Check::warn(NAME, format!("cannot read .gitignore: {err}")),
        Ok(text) => {
            if is_ignored_by(&text, ".wiki/versions") {
                Check::warn(
                    NAME,
                    ".gitignore ignores .wiki/versions/; checkpoint history would not be committed",
                )
            } else {
                Check::ok(
                    NAME,
                    ".wiki/versions/ is not ignored by the root .gitignore",
                )
            }
        }
    }
}

/// `lock`: no live lock is left behind.
fn check_lock(root: &Path, _cfg: &Config) -> Check {
    const NAME: &str = "lock";
    let path = root.join(".wiki").join(LOCK_FILE);
    if !path.is_file() {
        return Check::ok(NAME, "no lock file is present");
    }
    if lock::is_stale(root, lock::LOCK_TTL_SECS) {
        Check::ok(
            NAME,
            "the lock file is older than its TTL; the next command reclaims it",
        )
    } else {
        Check::warn(
            NAME,
            "a fresh lock file is present; another command may be running, or a run crashed inside the TTL",
        )
    }
}

/// `staging`: the review queue and its ledger agree.
///
/// Candidates without a manifest are the torn state that makes the next round
/// refuse with `E_STAGING_CORRUPT`. A manifest without candidates is the normal
/// state after an approval moved the last candidates into `wiki/`, so it passes
/// with a note rather than crying wolf.
fn check_staging(root: &Path) -> Check {
    const NAME: &str = "staging";
    let candidates = staging::candidates_present(root);
    let manifest = staging::manifest_present(root);
    match candidates {
        Err(err) => Check::fail(NAME, format!("cannot inspect staging: {err}")),
        Ok(true) if manifest => Check::ok(NAME, "staged candidates have their manifest"),
        Ok(true) => Check::warn(
            NAME,
            "staging holds candidates but manifest.json is gone; the next round reports E_STAGING_CORRUPT",
        ),
        Ok(false) if manifest => Check::ok(
            NAME,
            "the manifest is present and no candidates are waiting (normal after approve)",
        ),
        Ok(false) => Check::ok(NAME, "staging is empty"),
    }
}

/// `token`: no credential was pasted into the contract.
fn check_token(text: Option<&str>) -> Check {
    const NAME: &str = "token";
    let Some(text) = text else {
        return Check::ok(NAME, "no wiki.toml to scan");
    };
    let hits = scan_credentials(text);
    if hits.is_empty() {
        Check::ok(
            NAME,
            "no bearer token or literal secret is stored in wiki.toml",
        )
    } else {
        Check::fail(
            NAME,
            format!(
                "wiki.toml looks like it stores a credential ({}); keep it in the environment and reference it with auth_env",
                hits.join(", ")
            ),
        )
    }
}

/// Lines of `text` that look like a stored credential.
fn scan_credentials(text: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.to_ascii_lowercase().contains("bearer ") {
            hits.push(format!("line {}: a Bearer header", index + 1));
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_ascii_lowercase();
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if value.is_empty() || value.starts_with("${") {
            continue;
        }
        let is_env_name = key == "auth_env";
        if is_env_name && config::looks_like_token(value) {
            hits.push(format!(
                "line {}: `{key}` holds a token, not a name",
                index + 1
            ));
        } else if !is_env_name && is_credential_key(&key) {
            hits.push(format!("line {}: `{key}`", index + 1));
        }
    }
    hits
}

/// Whether a frontmatter key names a credential rather than a setting.
fn is_credential_key(key: &str) -> bool {
    CREDENTIAL_KEYS.contains(&key)
        || key.ends_with("_token")
        || key.ends_with("_secret")
        || key.ends_with("_api_key")
        || key.ends_with("_password")
}

/// `external_backend`: the parse backend the contract selects can work.
fn check_external_backend(cfg: &Config) -> Check {
    const NAME: &str = "external_backend";
    match (cfg.parse.primary, cfg.parse.external.as_ref()) {
        (Primary::External, None) => Check::fail(
            NAME,
            "parse.primary = external but [parse.external] is missing; every parse would fail with E_NO_EXTERNAL",
        ),
        (Primary::External, Some(external)) => probe_backend(NAME, external, "required by parse.primary"),
        (Primary::Auto, Some(external)) => {
            probe_backend(NAME, external, "used when no builtin backend claims a source")
        }
        (Primary::Auto, None) => Check::ok(
            NAME,
            "no external backend configured; the builtin backends handle every supported format",
        ),
        (Primary::Builtin, Some(_)) => Check::ok(
            NAME,
            "[parse.external] is configured but parse.primary = builtin, so it is never called",
        ),
        (Primary::Builtin, None) => Check::ok(NAME, "no external backend configured"),
    }
}

/// Probes a configured backend, reporting unreachable as a warning.
fn probe_backend(name: &'static str, external: &config::ExternalCfg, role: &str) -> Check {
    match probe_target(&external.url) {
        Ok(target) => Check::ok(name, format!("{target} accepts connections ({role})")),
        Err(err) => Check::warn(
            name,
            format!("{err} ({role}); parsing that needs it will report an error"),
        ),
    }
}

/// Opens and immediately drops a TCP connection to the backend's host.
///
/// No HTTP request is sent: the probe only answers "is something listening",
/// which is all `doctor` needs and all that cannot have side effects on the
/// service.
fn probe_target(url: &str) -> Result<String, String> {
    let (host, port) = authority(url)?;
    let addrs = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|err| format!("cannot resolve {host}:{port}: {err}"))?;
    let mut last = String::from("no address resolved");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) {
            Ok(_) => return Ok(format!("{host}:{port}")),
            Err(err) => last = err.to_string(),
        }
    }
    Err(format!("cannot reach {host}:{port}: {last}"))
}

/// Extracts `(host, port)` from an http(s) URL without pulling in a URL crate.
fn authority(url: &str) -> Result<(String, u16), String> {
    let url = url.trim();
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("`{url}` is not an http(s) URL"))?;
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        other => return Err(format!("unsupported scheme `{other}` in `{url}`")),
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default();
    if authority.is_empty() {
        return Err(format!("`{url}` names no host"));
    }
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| format!("`{url}` has an unclosed IPv6 host"))?;
        let port = tail
            .strip_prefix(':')
            .and_then(|digits| digits.parse().ok())
            .unwrap_or(default_port);
        return Ok((String::from(host), port));
    }
    match authority.rsplit_once(':') {
        Some((host, digits)) if !host.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) => {
            let port = digits
                .parse()
                .map_err(|_| format!("`{url}` has a port outside the valid range"))?;
            Ok((String::from(host), port))
        }
        _ => Ok((String::from(authority), default_port)),
    }
}

/// Whether `path` is ignored by the gitignore rules in `rules`.
///
/// The matcher covers the patterns a repository root actually uses: comments,
/// negations, directory-only rules, `*`, `?` and `**`. Anchored rules match a
/// prefix of the path, because ignoring a directory ignores everything below
/// it; unanchored rules match one component at any depth, as git does.
fn is_ignored_by(rules: &str, path: &str) -> bool {
    let mut ignored = false;
    for line in rules.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (negated, pattern) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        if pattern_matches(pattern, path) {
            ignored = !negated;
        }
    }
    ignored
}

/// Whether one gitignore pattern matches `path`.
fn pattern_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches('/').trim_end_matches('/');
    if pattern.is_empty() {
        return false;
    }
    if pattern.contains('/') {
        let parts: Vec<&str> = path.split('/').collect();
        (1..=parts.len()).any(|end| glob_match(pattern, &parts[..end].join("/")))
    } else {
        path.split('/').any(|part| glob_match(pattern, part))
    }
}

/// A tiny glob matcher: `*` stops at `/`, `**` crosses it, `?` is one character.
fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    glob(&pattern, &text)
}

/// Recursive engine behind [`glob_match`].
fn glob(pattern: &[char], text: &[char]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some('*') if pattern.get(1) == Some(&'*') => {
            let rest = if pattern.get(2) == Some(&'/') {
                &pattern[3..]
            } else {
                &pattern[2..]
            };
            (0..=text.len()).any(|end| glob(rest, &text[end..]))
        }
        Some('*') => (0..=text.len())
            .any(|end| text[..end].iter().all(|c| *c != '/') && glob(&pattern[1..], &text[end..])),
        Some('?') => !text.is_empty() && text[0] != '/' && glob(&pattern[1..], &text[1..]),
        Some(expected) => {
            !text.is_empty() && text[0] == *expected && glob(&pattern[1..], &text[1..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn repo() -> tempfile::TempDir {
        tempdir().unwrap()
    }

    #[test]
    fn a_newer_schema_is_a_failure() {
        let dir = repo();
        let path = dir.path().join(".wiki").join(DB_FILE);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);

        let check = check_db(dir.path(), &Config::default());
        assert_eq!(check.level, Level::Fail);
        assert!(check.detail.contains("99"), "{}", check.detail);
    }

    #[test]
    fn a_missing_ledger_is_only_a_warning() {
        let dir = repo();
        let check = check_db(dir.path(), &Config::default());
        assert_eq!(check.level, Level::Warn);
    }

    #[test]
    fn missing_fts5_is_a_failure() {
        let check = fts5_check(Ok(false));
        assert_eq!(check.level, Level::Fail);
        assert!(check.detail.contains("FTS5"));
    }

    #[test]
    fn external_selected_without_a_section_is_a_failure() {
        let cfg: Config = toml::from_str("[parse]\nprimary = \"external\"\n").unwrap();
        let check = check_external_backend(&cfg);
        assert_eq!(check.level, Level::Fail);
        assert!(check.detail.contains("E_NO_EXTERNAL"));
    }

    #[test]
    fn an_unreachable_backend_is_a_warning() {
        let cfg: Config = toml::from_str(
            "[parse]\nprimary = \"external\"\n[parse.external]\nurl = \"http://127.0.0.1:1\"\n",
        )
        .unwrap();
        let check = check_external_backend(&cfg);
        assert_eq!(check.level, Level::Warn, "{}", check.detail);
    }

    #[test]
    fn a_listening_backend_is_reported_ok() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = probe_target(&format!("http://127.0.0.1:{port}/parse")).unwrap();
        assert_eq!(target, format!("127.0.0.1:{port}"));
    }

    #[test]
    fn url_authority_handles_ports_and_schemes() {
        assert_eq!(
            authority("https://api.example.com/v1").unwrap(),
            (String::from("api.example.com"), 443)
        );
        assert_eq!(
            authority("http://127.0.0.1:8080/x?y#z").unwrap(),
            (String::from("127.0.0.1"), 8080)
        );
        assert_eq!(
            authority("https://user:pw@example.com/x").unwrap(),
            (String::from("example.com"), 443)
        );
        assert!(authority("ftp://example.com").is_err());
        assert!(authority("example.com").is_err());
    }

    #[test]
    fn gitignore_patterns_match_like_git() {
        let path = ".wiki/versions";
        for rules in [
            ".wiki/versions/",
            ".wiki/",
            ".wiki",
            "versions/",
            ".wiki/*",
            ".wiki/**",
            "*",
        ] {
            assert!(is_ignored_by(rules, path), "pattern {rules:?}");
        }
        for rules in [
            "target/",
            "/target",
            "*.tmp",
            "# .wiki/versions/",
            ".wiki/other/",
        ] {
            assert!(!is_ignored_by(rules, path), "pattern {rules:?}");
        }
        assert!(!is_ignored_by("!versions/", path));
        assert!(!is_ignored_by(".wiki/versions/\n!.wiki/versions/", path));
        assert!(!is_ignored_by("versions/\n!versions/", path));
    }

    #[test]
    fn a_bearer_string_is_detected() {
        let hits = scan_credentials("authorization = \"Bearer abc123\"\n");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].contains("Bearer"));
    }

    #[test]
    fn an_environment_variable_name_is_not_a_token() {
        let hits = scan_credentials("[parse.external]\nauth_env = \"WIKI_PARSE_TOKEN\"\n");
        assert!(hits.is_empty(), "{hits:?}");
    }

    #[test]
    fn a_literal_secret_is_detected() {
        let hits = scan_credentials("token = \"abc\"\n");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].contains("token"));
    }

    #[test]
    fn a_long_auth_env_value_is_a_token() {
        let hits = scan_credentials(&format!("auth_env = \"{}\"\n", "a".repeat(41)));
        assert_eq!(hits.len(), 1);
    }
}
