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

use serde::Deserialize;

/// Name of the contract file at the wiki root.
pub const CONFIG_FILE: &str = "wiki.toml";

/// Failure while reading or validating `wiki.toml`.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The wiki root has no `wiki.toml` yet.
    #[error("no {CONFIG_FILE} in {}; run `llm-wiki init` to create it", root.display())]
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
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub paths: PathsCfg,
    pub formats: FormatsCfg,
    pub schema: SchemaCfg,
    pub parse: ParseCfg,
    pub ingest: IngestCfg,
    pub audit: AuditCfg,
    pub versions: VersionsCfg,
    pub sync: SyncCfg,
    pub memory: MemoryCfg,
}

impl Config {
    /// Reads `root/wiki.toml`.
    ///
    /// A missing file is [`ConfigError::NoConfig`] rather than a plain IO error,
    /// so callers can point the user at `llm-wiki init`.
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
        toml::from_str(&text).map_err(|err| ConfigError::Bad {
            message: format!("{CONFIG_FILE} is invalid: {err}"),
        })
    }

    /// Checks the parts of the contract that deserialization cannot.
    ///
    /// Covers the page-type enum, the parse backend choice, the numeric
    /// invariants between the three size settings, and the ban on storing a
    /// token instead of an environment variable name.
    pub fn validate(&self) -> Result<()> {
        self.validate_types()?;
        self.validate_primary()?;
        self.validate_sizes()?;
        self.validate_external()?;
        Ok(())
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

/// `[paths]`: where the five repository locations live, relative to the root.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PathsCfg {
    pub source: String,
    pub raw: String,
    pub wiki: String,
    pub engine: String,
}

impl Default for PathsCfg {
    fn default() -> Self {
        Self {
            source: String::from("source"),
            raw: String::from("raw"),
            wiki: String::from("wiki"),
            engine: String::from(".wiki"),
        }
    }
}

/// `[formats]`: the source-file whitelist. Images are linked, never parsed.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
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
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
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
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
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
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
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

/// `[ingest]`: page-count cap and the review gate.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct IngestCfg {
    pub max_pages_per_source: usize,
    pub review_before_write: bool,
}

impl Default for IngestCfg {
    fn default() -> Self {
        Self {
            max_pages_per_source: 12,
            review_before_write: true,
        }
    }
}

/// `[audit]`: thresholds used by `lint`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
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
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct VersionsCfg {
    pub keep: usize,
}

impl Default for VersionsCfg {
    fn default() -> Self {
        Self { keep: 5 }
    }
}

/// `[sync]`: unattended-mode knobs.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SyncCfg {
    pub auto_ingest: bool,
    pub max_per_run: usize,
    pub max_pending_sources: usize,
    pub settle_seconds: u64,
    pub stable_streak: u32,
    pub read_retries: u32,
    pub lag_warn_rounds: u32,
    pub on_missing: String,
}

impl Default for SyncCfg {
    fn default() -> Self {
        Self {
            auto_ingest: false,
            max_per_run: 20,
            max_pending_sources: 200,
            settle_seconds: 10,
            stable_streak: 3,
            read_retries: 3,
            lag_warn_rounds: 3,
            on_missing: String::from("mark_stale"),
        }
    }
}

/// `[memory]`: the session-memory water marks from §5.11.
///
/// `session_soft_cap` counts the live session records that make the agent ask
/// for a distillation; `idle_days` is how long a sealed summary may go without a
/// `recall` before it becomes a purge candidate. `purge_cap` and `purge_days` are
/// the second condition: a summary is only cleared when the archive is both over
/// its capacity and past the deadline, so a quiet wiki never loses anything.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MemoryCfg {
    pub session_soft_cap: usize,
    pub idle_days: u32,
    pub purge_cap: usize,
    pub purge_days: u32,
}

impl Default for MemoryCfg {
    fn default() -> Self {
        Self {
            session_soft_cap: 50,
            idle_days: 60,
            purge_cap: 200,
            purge_days: 180,
        }
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
