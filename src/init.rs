//! Repository bootstrap: write the two contract files and create the engine
//! directories.
//!
//! `init` only ever creates what is missing. It does not load or validate an
//! existing `wiki.toml` (that belongs to `doctor` and `lint`), so a repository
//! with a deliberately broken config can still be repaired by `--force`.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Directories that must exist after `init`, parents before children.
const DIRS: [&str; 10] = [
    "source",
    "raw",
    "wiki",
    "wiki/archive",
    "wiki/audit",
    ".wiki",
    ".wiki/cache",
    ".wiki/sessions",
    ".wiki/staging",
    ".wiki/versions",
];

/// The two human-editable contract files and the content `init` gives them.
const CONTRACT_FILES: [(&str, &str); 2] = [("wiki.toml", WIKI_TOML), ("SCHEMA.md", SCHEMA_MD)];

/// What one `init` run did, split by path so the caller can show both sides.
#[derive(Debug, Clone, Default, Serialize)]
pub struct InitOutcome {
    /// Paths written or created by this run (relative, `/`-separated).
    pub created: Vec<String>,
    /// Paths that already existed and were left alone.
    pub skipped: Vec<String>,
}

/// Failure while creating a directory or writing a contract file.
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("cannot create {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Result of the init helpers.
pub type Result<T> = std::result::Result<T, InitError>;

/// Creates the contract files and engine directories under `root`.
///
/// `force` rewrites `wiki.toml` and `SCHEMA.md` even when they exist; it never
/// deletes directories or user files.
pub fn run(root: &Path, force: bool) -> Result<InitOutcome> {
    let mut outcome = InitOutcome::default();

    for dir in DIRS {
        let path = root.join(dir);
        if path.is_dir() {
            outcome.skipped.push(dir.to_string());
            continue;
        }
        std::fs::create_dir_all(&path).map_err(|source| InitError::Io {
            path: path.clone(),
            source,
        })?;
        outcome.created.push(dir.to_string());
    }

    for (name, body) in CONTRACT_FILES {
        let path = root.join(name);
        if path.exists() && !force {
            outcome.skipped.push(name.to_string());
            continue;
        }
        std::fs::write(&path, body).map_err(|source| InitError::Io {
            path: path.clone(),
            source,
        })?;
        outcome.created.push(name.to_string());
    }

    Ok(outcome)
}

/// Default `wiki.toml`: every section spelled out, every default made visible.
const WIKI_TOML: &str = r#"# llm-wiki contract. The single source of configuration.
# Kept in git: humans and agents both read it.

[paths]
source = "source"
raw    = "raw"
wiki   = "wiki"
engine = ".wiki"

[formats]
# Whitelist for source/. Images are only linked, never parsed.
allow = ["md", "txt", "html", "pdf", "png", "jpg", "jpeg", "gif", "webp", "svg"]

[schema]
# Page type enum. Edit it here; SCHEMA.md documents the fixed mechanism fields.
types = ["concept", "entity", "source", "comparison", "synthesis"]

[parse]
primary          = "auto"   # auto | builtin | external
chunk_threshold  = "2MB"    # above this a source is always chunked
max_source_bytes = "100MB"  # above this a source is rejected, needs --force-large
chunk_target     = "64KB"   # target size of one text chunk

# Optional external parse service. Only ever write the *name* of the environment
# variable that holds the token, never the token itself.
# [parse.external]
# type      = "http"
# url       = "http://host/api/parse"
# auth_env  = "WIKI_PARSE_TOKEN"
# fields    = { file = "@file", fileFlag = "N", system = "BSS" }
# response  = { text = "data", ok_code = 200 }
# timeout_s = 120

[ingest]
max_pages_per_source = 12
review_before_write  = true

[audit]
min_page_chars = 200

[versions]
keep = 5

[sync]
auto_ingest         = false
max_per_run         = 20
max_pending_sources = 200
settle_seconds      = 10
stable_streak       = 3
read_retries        = 3
lag_warn_rounds     = 3
on_missing          = "mark_stale"

[memory]
# Live session records before the agent is asked to distil them.
session_soft_cap = 50
# Days a sealed summary may sit unread before it counts as idle.
idle_days        = 60
# A summary is only purged when the archive is BOTH over purge_cap and past
# purge_days, so a quiet wiki never loses its history.
purge_cap        = 200
purge_days       = 180
"#;

/// Default `SCHEMA.md`: the conventions the CLI enforces, for humans and agents.
const SCHEMA_MD: &str = r#"# Page schema

这是本 wiki 的页面约定，人与其 agent 共读，随仓库进 git。
机器可读的一半在 `wiki.toml`；本文件解释 CLI 会强制检查的规则。

## 机制字段

每个页面位于 `wiki/<type>/<id>.md`，开头是这段 frontmatter：

| 字段 | 必填 | 含义 |
|---|---|---|
| `id` | 是 | kebab-case，等于文件名，等于 `[[链接]]` 的目标 |
| `title` | 是 | 页面标题，进 `index.md` |
| `type` | 是 | 取自 `wiki.toml` 的 `[schema].types` |
| `summary` | 是 | 一句话摘要，进 `index.md` |
| `tags` | 否 | 自由标签列表 |
| `sources` | 是 | 本页依据的 `raw/*.md` 路径；没有它就是「无源声称」 |
| `status` | 是 | `draft` / `stable` / `stale` |
| `updated` | 是 | ISO 日期，只由 CLI 行级替换写入，禁止手写 |

必填字段与字段类型是固定的；CLI 的校验器不会为了省事而放松。
`updated` 由 CLI 写入时保留注释、键序、引号风格和未知字段。

## type 枚举

`type` 的合法值来自 `wiki.toml` 的 `[schema].types`，例如 `concept`、`entity`、
`source`、`comparison`、`synthesis`。要增删类型，改 `wiki.toml` 而不是改本文件；
但改了枚举就要同步迁移已有页面，否则 lint 会报 L1。枚举可以演进，机制字段不行。

## 图片路径

图片放在 `source/`，只登记链接、不做识别（不 OCR）。
页面在 `wiki/<type>/<id>.md`，比仓库根深两层，因此引用 `source/` 下的图片统一写：

    ![说明](../../source/<相对路径>)

`raw/` 镜像 `source/` 的相对结构，asset 型 raw 用与自身深度相符的相对路径。

## status 三态

- `draft`：草稿。已写但未经人工确认，`ingest commit` 之前的状态，不当作结论引用。
- `stable`：已批准。当前可信版本，`checkpoint` 会把它登记进 `.wiki/versions/`。
- `stale`：已过期。源发生变更、源缺失或结论被新源推翻，等待重新审阅；
  保留页面和源引用，不自动改写结论，也不自动删除。

三个状态都由 CLI 依据账本与审计结果设置，人可以在认可后手工改回 `stable`。
"#;
