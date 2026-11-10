//! `wiki.toml`: loading and validation of the project contract.
//!
//! The file is the single source of configuration for a wiki repository. Every
//! section is optional in the file: a missing section (or a missing key inside
//! it) falls back to the default shown in the design document's complete
//! `wiki.toml` example, so a hand-written three-line config still works.
//!
//! Sizes are written as human strings (`"2MB"`, `"64KB"`, or a plain byte
//! count) and are turned into bytes while the file is parsed, so consumers never
//! deal with units. A malformed size therefore fails [`Config::load`] with
//! `E_BAD_CONFIG`; [`Config::validate`] re-checks the numeric invariants that
//! cannot be expressed by deserialization alone.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The hidden engine directory. Everything `llm-wiki` writes lives under it.
pub const ENGINE_DIR: &str = ".wiki";

/// Name of the contract file, relative to the repository root.
pub const CONFIG_FILE: &str = ".wiki/wiki.toml";

/// The schema page `init` writes, relative to the repository root.
pub const SCHEMA_FILE: &str = ".wiki/SCHEMA.md";

/// Failure while reading or validating `wiki.toml`.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The wiki root has no `wiki.toml` yet.
    #[error("no {CONFIG_FILE} in {}; run `llm-wiki doctor --fix` to create it", root.display())]
    NoConfig { root: PathBuf },
    /// The file exists but could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid TOML, or a value has the wrong shape or unit.
    #[error("{message}")]
    Bad { message: String },
    /// A section that must hold an environment variable name holds a token.
    #[error("{message}")]
    TokenInConfig { message: String },
}

impl ConfigError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            ConfigError::NoConfig { .. } => "E_NO_CONFIG",
            ConfigError::TokenInConfig { .. } => "E_TOKEN_IN_CONFIG",
            ConfigError::Io { .. } | ConfigError::Bad { .. } => "E_BAD_CONFIG",
        }
    }
}

/// Result of the config helpers.
pub type Result<T> = std::result::Result<T, ConfigError>;

/// The whole `wiki.toml` contract, one field per same-named section.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Fixed at [`PathsCfg::default`]: the layout is not configurable, so a
    /// hand-edited `[paths]` section cannot move writes outside the engine
    /// directory. The field stays for call sites that read a location.
    #[serde(skip)]
    pub paths: PathsCfg,
    pub scan: ScanCfg,
    pub formats: FormatsCfg,
    pub schema: SchemaCfg,
    pub parse: ParseCfg,
    pub ingest: IngestCfg,
    pub audit: AuditCfg,
    #[serde(skip_serializing)]
    pub versions: VersionsCfg,
}

impl Config {
    /// Reads `root/wiki.toml`.
    ///
    /// A missing file is [`ConfigError::NoConfig`] rather than a plain IO error,
    /// so callers can point the user at `llm-wiki doctor --fix`.
    pub fn load(root: &Path) -> Result<Config> {
        let path = root.join(CONFIG_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(ConfigError::NoConfig {
                    root: root.to_path_buf(),
                })
            }
            Err(source) => return Err(ConfigError::Io { path, source }),
        };
        let cfg: Config = toml::from_str(&text).map_err(|err| ConfigError::Bad {
            message: format!("{CONFIG_FILE} is invalid: {err}"),
        })?;
        // An unknown `sync.on_missing` is a contract error, not a value to keep
        // around: silently falling back would hide a typo behind "do nothing".
        cfg.validate_on_missing()?;
        Ok(cfg)
    }

    /// Checks the parts of the contract that deserialization cannot.
    ///
    /// Covers the page-type enum, the parse backend choice, the numeric
    /// invariants between the three size settings, the `sync.on_missing` policy,
    /// and the ban on storing a token instead of an environment variable name.
    pub fn validate(&self) -> Result<()> {
        self.validate_types()?;
        self.validate_primary()?;
        self.validate_sizes()?;
        self.validate_external()?;
        self.validate_on_missing()?;
        Ok(())
    }

    fn validate_on_missing(&self) -> Result<()> {
        match self.ingest.on_missing.as_str() {
            "mark_stale" | "ignore" => Ok(()),
            other => Err(ConfigError::Bad {
                message: format!(
                    "sync.on_missing must be \"mark_stale\" or \"ignore\", not {other:?}"
                ),
            }),
        }
    }

    fn validate_types(&self) -> Result<()> {
        if self.schema.types.is_empty() {
            return Err(ConfigError::Bad {
                message: String::from("schema.types must list at least one page type"),
            });
        }
        let mut seen = BTreeSet::new();
        for name in &self.schema.types {
            if name.trim().is_empty() {
                return Err(ConfigError::Bad {
                    message: String::from("schema.types contains an empty type name"),
                });
            }
            if !seen.insert(name) {
                return Err(ConfigError::Bad {
                    message: format!("schema.types contains the duplicate type {name:?}"),
                });
            }
        }
        Ok(())
    }

    fn validate_primary(&self) -> Result<()> {
        // `Primary` is an enum with exactly these three variants, so an unknown
        // spelling already fails during deserialization. The match states the
        // allowed set in one place and keeps the contract explicit.
        match self.parse.primary {
            Primary::Auto | Primary::Builtin | Primary::External => Ok(()),
        }
    }

    fn validate_sizes(&self) -> Result<()> {
        for (name, value) in [
            ("parse.chunk_threshold", self.parse.chunk_threshold),
            ("parse.max_source_bytes", self.parse.max_source_bytes),
            ("parse.chunk_target", self.parse.chunk_target),
        ] {
            if value == 0 {
                return Err(ConfigError::Bad {
                    message: format!("{name} must be greater than zero"),
                });
            }
        }
        if self.parse.chunk_target > self.parse.chunk_threshold {
            return Err(ConfigError::Bad {
                message: String::from("parse.chunk_target must not exceed parse.chunk_threshold"),
            });
        }
        Ok(())
    }

    fn validate_external(&self) -> Result<()> {
        let Some(external) = &self.parse.external else {
            return Ok(());
        };
        if looks_like_token(&external.auth_env) {
            return Err(ConfigError::TokenInConfig {
                message: String::from(
                    "parse.external.auth_env must be the name of an environment variable, \
                     not a token (a credential prefix or a string longer than 40 characters)",
                ),
            });
        }
        Ok(())
    }
}

/// The fixed repository layout, relative to the root.
///
/// `source` is the empty string: every non-hidden file in the repository is a
/// source. `raw`, `wiki` and `engine` all sit under `.wiki`, which is also what
/// makes "never write outside the engine" structural rather than a convention.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathsCfg {
    pub source: String,
    pub raw: String,
    pub wiki: String,
    pub engine: String,
}

impl Default for PathsCfg {
    fn default() -> Self {
        Self {
            source: String::new(),
            raw: String::from(".wiki/raw"),
            wiki: String::from(".wiki/pages"),
            engine: String::from(ENGINE_DIR),
        }
    }
}

/// `[scan]`: which directories under the repository root are not sources.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScanCfg {
    /// Directory names skipped anywhere in the tree, in addition to the
    /// dot-directories [`crate::paths::is_ignored`] already skips.
    pub ignore: Vec<String>,
}

impl Default for ScanCfg {
    fn default() -> Self {
        Self {
            ignore: ["node_modules", "target", "dist", "build", "vendor", ".git"]
                .iter()
                .map(|name| String::from(*name))
                .collect(),
        }
    }
}

/// `[formats]`: the source-file whitelist. Images are linked, never parsed.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FormatsCfg {
    pub allow: Vec<String>,
}

impl Default for FormatsCfg {
    fn default() -> Self {
        Self {
            allow: [
                "md", "txt", "html", "pdf", "png", "jpg", "jpeg", "gif", "webp", "svg",
            ]
            .iter()
            .map(|ext| String::from(*ext))
            .collect(),
        }
    }
}

/// `[schema]`: the configurable half of the page frontmatter.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SchemaCfg {
    pub types: Vec<String>,
}

impl Default for SchemaCfg {
    fn default() -> Self {
        Self {
            types: ["concept", "entity", "source", "comparison", "synthesis"]
                .iter()
                .map(|name| String::from(*name))
                .collect(),
        }
    }
}

/// `[parse]`: how sources are turned into markdown.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ParseCfg {
    pub primary: Primary,
    /// Sources larger than this are always chunked. Stored as bytes.
    #[serde(deserialize_with = "de_size")]
    pub chunk_threshold: u64,
    /// Sources larger than this are rejected unless `--force-large` is given.
    #[serde(deserialize_with = "de_size")]
    pub max_source_bytes: u64,
    /// Target size of one text chunk. Stored as bytes.
    #[serde(deserialize_with = "de_size")]
    pub chunk_target: u64,
    /// Below this many markdown characters the builtin result counts as too thin
    /// and `auto` mode may fall back to the external service.
    pub min_chars: usize,
    /// Optional external HTTP parsing service.
    pub external: Option<ExternalCfg>,
}

impl Default for ParseCfg {
    fn default() -> Self {
        Self {
            primary: Primary::Auto,
            chunk_threshold: 2 * 1024 * 1024,
            max_source_bytes: 100 * 1024 * 1024,
            chunk_target: 64 * 1024,
            min_chars: 200,
            external: None,
        }
    }
}

/// `[parse].primary`: which parsing backend to use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Primary {
    /// Built in first, external service as the fallback.
    #[default]
    Auto,
    /// Built in only.
    Builtin,
    /// External service only.
    External,
}

/// `[parse.external]`: an optional HTTP service that returns markdown.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExternalCfg {
    /// Transport kind; only `http` exists today.
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    /// Name of the environment variable that holds the bearer token.
    pub auth_env: String,
    /// Multipart form fields, in the order the service expects them. A value of
    /// `@file` means "send the source file here".
    pub fields: BTreeMap<String, String>,
    pub response: ExternalResponse,
    pub timeout_s: u64,
}

impl Default for ExternalCfg {
    fn default() -> Self {
        Self {
            kind: String::from("http"),
            url: String::new(),
            auth_env: String::new(),
            fields: BTreeMap::from([(String::from("file"), String::from("@file"))]),
            response: ExternalResponse::default(),
            timeout_s: 120,
        }
    }
}

/// `[parse.external.response]`: where the markdown and the success code live.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExternalResponse {
    /// Dot-separated JSON path of the markdown text.
    pub text: String,
    /// HTTP status that counts as success.
    pub ok_code: u16,
}

impl Default for ExternalResponse {
    fn default() -> Self {
        Self {
            text: String::from("data"),
            ok_code: 200,
        }
    }
}

/// `[ingest]`: page-count caps.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct IngestCfg {
    /// Pages one slice of a source may justify. A slice is one chunk of a large
    /// source, or the whole of a small one, so the cap follows how much text a
    /// page had to work with rather than how many files were scanned. `0`
    /// disables the brake.
    #[serde(skip_serializing)]
    pub max_pages_per_slice: usize,
    /// New pages one merge may add before `submit` stops and asks.
    #[serde(skip_serializing)]
    pub max_new_pages_per_run: usize,
    /// Directory holding distilled session records, relative to the repository
    /// root. Pages whose sources live here are what `query --no-archive` hides.
    /// An empty string disables the marker.
    pub session_dir: String,
    /// Most sources one round takes.
    pub max_per_run: usize,
    /// Pending sources above this many mean the queue is not keeping up.
    #[serde(skip_serializing)]
    pub max_pending_sources: usize,
    /// Seconds a source must sit untouched before a round may take it. The
    /// round then re-hashes the file: it converts only when the bytes are still
    /// the ones `scan` read. `0` takes every source immediately.
    pub settle_seconds: u64,
    /// Read attempts before a source is reported as unreadable.
    pub read_retries: u32,
    /// Rounds a source may lag before the audit reports it.
    pub lag_warn_rounds: u32,
    /// What a round does with a source that disappeared from disk.
    pub on_missing: String,
}

impl Default for IngestCfg {
    fn default() -> Self {
        Self {
            max_pages_per_slice: 5,
            max_new_pages_per_run: 20,
            session_dir: String::from("sessions"),
            max_per_run: 20,
            max_pending_sources: 200,
            settle_seconds: 300,
            read_retries: 3,
            lag_warn_rounds: 3,
            on_missing: String::from("mark_stale"),
        }
    }
}

/// `[audit]`: thresholds used by `lint`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuditCfg {
    pub min_page_chars: usize,
}

impl Default for AuditCfg {
    fn default() -> Self {
        Self {
            min_page_chars: 200,
        }
    }
}

/// `[versions]`: how many history snapshots to keep.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct VersionsCfg {
    pub keep: usize,
}

impl Default for VersionsCfg {
    fn default() -> Self {
        Self { keep: 5 }
    }
}

/// Parses a byte size: a plain count (`"1024"`) or `B`/`KB`/`MB`/`GB` suffix,
/// case-insensitive, in binary multiples.
///
/// Also used by the chunking code for sizes that come from the command line
/// rather than from `wiki.toml`.
pub fn parse_size(text: &str) -> Result<u64> {
    let text = text.trim();
    let (digits, suffix) = match text.find(|c: char| !c.is_ascii_digit()) {
        Some(split) => text.split_at(split),
        None => (text, ""),
    };
    if digits.is_empty() {
        return Err(bad_size(
            text,
            "expected a number followed by B, KB, MB or GB",
        ));
    }
    let value: u64 = digits
        .parse()
        .map_err(|_| bad_size(text, "the number is too large"))?;
    let multiplier: u64 = match suffix.trim().to_ascii_uppercase().as_str() {
        "" | "B" => 1,
        "K" | "KB" => 1024,
        "M" | "MB" => 1024 * 1024,
        "G" | "GB" => 1024 * 1024 * 1024,
        _ => return Err(bad_size(text, "unknown unit, expected B, KB, MB or GB")),
    };
    value
        .checked_mul(multiplier)
        .ok_or_else(|| bad_size(text, "the size does not fit in 64 bits"))
}

fn bad_size(text: &str, why: &str) -> ConfigError {
    ConfigError::Bad {
        message: format!("invalid size {text:?}: {why}"),
    }
}

/// Serde hook that accepts either a size string or a plain integer.
fn de_size<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct SizeVisitor;

    impl serde::de::Visitor<'_> for SizeVisitor {
        type Value = u64;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a byte size such as \"2MB\", \"64KB\", or a plain byte count")
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> std::result::Result<u64, E> {
            parse_size(value).map_err(E::custom)
        }

        fn visit_u64<E: serde::de::Error>(self, value: u64) -> std::result::Result<u64, E> {
            Ok(value)
        }

        fn visit_i64<E: serde::de::Error>(self, value: i64) -> std::result::Result<u64, E> {
            u64::try_from(value).map_err(|_| E::custom("a size cannot be negative"))
        }
    }

    deserializer.deserialize_any(SizeVisitor)
}

/// Heuristic for a credential written where an environment variable name
/// belongs: a known token prefix, an `Authorization` header value pasted whole,
/// or a long whitespace-free blob.
pub fn looks_like_token(value: &str) -> bool {
    const TOKEN_PREFIXES: [&str; 3] = ["sk-", "ghp_", "eyJ"];
    let value = value.trim();
    let lowered = value.to_ascii_lowercase();
    TOKEN_PREFIXES
        .iter()
        .any(|prefix| value.starts_with(prefix))
        || lowered.starts_with("bearer ")
        || (value.chars().count() > 40 && !value.chars().any(char::is_whitespace))
}
