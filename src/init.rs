//! Repository bootstrap: write the two contract files and create the engine
//! directories.
//!
//! Every path `init` touches is under `.wiki/`. The repository outside it is
//! read-only to the tool: it holds the user's own files, which are scanned as
//! sources but never created, rewritten or deleted.
//!
//! `init` only ever creates what is missing. It does not load or validate an
//! existing config (that belongs to `doctor` and `lint`), so a repository with a
//! deliberately broken config can still be repaired by `--force`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::layout::LAYOUT;

/// The two contract files and the content `init` gives them.
const CONTRACT_FILES: [(&str, &str); 2] = [
    (".wiki/wiki.toml", WIKI_TOML),
    (".wiki/SCHEMA.md", SCHEMA_MD),
];

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

    for entry in LAYOUT {
        let dir = entry.path;
        let path = root.join(dir);
        if path.is_dir() {
            outcome.skipped.push(dir.to_string());
            continue;
        }
        std::fs::create_dir_all(&path).map_err(|source| InitError::Io {
            path: path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
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
            path: path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
            source,
        })?;
        outcome.created.push(name.to_string());
    }

    Ok(outcome)
}

/// Default `wiki.toml`: every section spelled out, every default made visible.
const WIKI_TOML: &str = r#"# llm-wiki contract. The single source of configuration.
# Lives in .wiki/ and stays local: the whole engine is derived state.

[scan]
# Directory names skipped anywhere under the repository root. Everything else
# that is not hidden is a source: the repository itself is the source tree.
ignore = ["node_modules", "target", "dist", "build", "vendor", ".git"]

[formats]
# Whitelist for sources. Images are only linked, never parsed.
allow = ["md", "txt", "html", "pdf", "png", "jpg", "jpeg", "gif", "webp", "svg"]

[schema]
# Page type enum. Edit it here; SCHEMA.md documents the fixed mechanism fields.
types = ["concept", "entity", "source", "comparison", "synthesis"]

[parse]
primary          = "auto"   # auto | builtin | external
chunk_threshold  = "2MB"    # above this a source is always chunked
max_source_bytes = "100MB"  # hard limit for scanning, preview and conversion
chunk_target     = "64KB"   # target size of one text chunk
min_chars        = 200      # below this a builtin result may fall back to the external service

# Optional external parse service. Only ever write the *name* of the environment
# variable that holds the token, never the token itself.
# [parse.external]
# type      = "http"
# url       = "http://host/api/parse"
# auth_env  = "WIKI_PARSE_TOKEN"
# fields    = { file = "@file" }   # 服务端要的附加表单字段都写在这里
# response  = { text = "data", ok_code = 200 }
# timeout_s = 120

[ingest]
# Where the session-distillation skill writes its records. Pages sourced from
# here are what `query --no-archive` hides. Empty string disables the marker.
session_dir = "sessions"
max_per_run         = 20
# Seconds a source must sit untouched, with its bytes still matching what the
# scan read, before a round converts it. 0 converts everything immediately.
settle_seconds      = 300
read_retries        = 3
lag_warn_rounds     = 3
# What a round does with a source somebody deleted. "forget" drops the page's
# reference to it, the raw file, its ledger row and the pending work item, and
# deletes a page left with no source at all; "mark_stale" only marks those pages
# stale; "ignore" leaves everything alone.
on_missing          = "forget"

[audit]
min_page_chars = 200


"#;

/// Default `SCHEMA.md`: the conventions the CLI enforces, for humans and agents.
const SCHEMA_MD: &str = r#"# Page schema

这是本 wiki 的页面约定，人与其 agent 共读，随 `.wiki/` 留在本机。
机器可读的一半在 `wiki.toml`；本文件解释 CLI 会强制检查的规则。

仓库里除 `.wiki/` 和隐藏目录之外的所有文件都是源文件：CLI 只读，从不修改。

## 机制字段

每个页面位于 `.wiki/pages/<type>/<id>.md`，开头是这段 frontmatter：

| 字段 | 必填 | 含义 |
|---|---|---|
| `id` | 是 | kebab-case，等于文件名，等于 `[[链接]]` 的目标 |
| `title` | 是 | 页面标题，进 `index.md` |
| `type` | 是 | 取自 `wiki.toml` 的 `[schema].types` |
| `summary` | 是 | 一句话摘要，进 `index.md` |
| `tags` | 否 | 自由标签列表 |
| `sources` | 是 | 本页依据的 `.wiki/raw/*.md` 路径；没有它就是「无源声称」 |
| `status` | 是 | `draft` / `stable` / `stale` |
| `updated` | 是 | 作者填写的 ISO 日期，内容修改时同步更新 |

必填字段与字段类型是固定的；CLI 的校验器不会为了省事而放松。
内容经 `task done` 提交：工具填 `id`、`type`、`sources` 与 `updated`，agent 只给标题、摘要、标签、状态与正文。
最后一次 page 任务关闭后根任务整批合并；`task status` 看进度，`task retry <id>` 重置失败任务。不发布、不回退正文。

## type 枚举

`type` 的合法值来自 `wiki.toml` 的 `[schema].types`，例如 `concept`、`entity`、
`source`、`comparison`、`synthesis`。要增删类型，改 `wiki.toml` 而不是改本文件；
但改了枚举就要同步迁移已有页面，否则 lint 会报 L1。枚举可以演进，机制字段不行。

## 图片路径

图片留在仓库里原本的位置，页面只引用、不复制；不做识别（不 OCR），也不会被当成来源转换——只有链接、没有正文的解析结果会被拒收，不产生 raw。
页面在 `.wiki/pages/<type>/<id>.md`，比仓库根深三层，因此引用仓库根下的图片统一写：

    ![说明](../../../<相对仓库根的路径>)

`.wiki/raw/` 镜像源文件在仓库里的相对结构。

## 归档页

`.wiki/pages/archive/<id>.md` 放会话摘要，但不进 `index.md`：它记录的是记忆，不是知识目录的条目。
归档页与普通页面同在 `.wiki/pages/` 下，引用源图片的深度相同，也写 `../../../<相对仓库根的路径>`。
归档页的 frontmatter 另有两个字段：`origin`（`document` / `conversation`，说明摘要由文档还是会话蒸馏而来）
与 `archived: true`（只在 `.wiki/pages/archive/` 下为真，等于声明「这是摘要，不是页面」，因而不受空页规则约束）。

## status 三态

- `draft`：内容尚不成熟，不当作结论引用；合并后也会被检索到。
- `stable`：作者判断当前可信的结论，不代表经过发布审批或版本备份。
- `stale`：已过期。源发生变更、源缺失或结论被新源推翻，等待重新审阅；
  保留页面和源引用，不自动改写结论，也不自动删除。

状态由作者维护，CLI 不替作者判断结论是否可信。唯一例外是来源文件消失：
`[ingest].on_missing = "forget"`（默认）时，引用该来源的页会去掉这一条 `sources` 并转 `stale`；
如果它一条来源都不剩，整页会被删掉——没有资料支撑的结论留着只会误导下一位读者。
`"mark_stale"` 只转 `stale`，`"ignore"` 什么都不做，两者都留给作者收拾。
"#;
