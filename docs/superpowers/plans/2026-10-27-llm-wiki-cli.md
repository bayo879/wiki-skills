# LLM Wiki CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付单二进制 `llm-wiki`，实现 `source/` → `raw/` → `wiki/` 的扫描、转换、摄取、审计、同步与检索。

**Architecture:** Rust CLI，无常驻进程。`source/` 是只读事实层，`raw/` 是 CLI 生成的 markdown 层，`wiki/` 是页面层，`.wiki/` 放可重建索引与版本快照。哈希、`index.md`、`log.md`、页面 `updated` 一律由 CLI 写，LLM 只写正文。

**Tech Stack:** Rust 1.75+；clap / serde / rusqlite(bundled, 含 FTS5) / blake3 / lopdf / pdf-extract / scraper / ureq / walkdir / unicode-normalization。

**Spec:** `docs/superpowers/specs/2026-10-27-llm-wiki-skills-design.md`

## Global Constraints

- 单二进制，release 开 LTO + strip，目标 < 15 MB；运行期不联网（`external-http` 后端除外）。
- 平台：darwin-arm64/x64、linux-arm64/x64、windows-x64。
- 退出码：`0` 成功 ｜ `1` error 级问题 ｜ `2` 用法/环境错误 ｜ `3` 需人工确认 ｜ `4` 另一进程持锁。
- 面向 agent 的输出走 `--json`，stdout 只有一个 JSON 文档；人类模式只是渲染。
- 路径身份键：相对仓库根、`/` 分隔、NFC 归一、保留原始大小写、不跟符号链接、`.` 开头忽略。
- 写文件统一 LF；读入容忍 CRLF。
- 单源上限 `100MB`；≥ `2MB` 强制分块。
- 哈希用 blake3；≤ 阈值全量，超过用 `size + mtime` 快判 + 抽样（头 1MB、中段 1MB、尾 1MB）。
- 绝不手写 `index.md` / `log.md` / `updated`；绝不 OCR。

## Review Focus

- **CJK 与 emoji 文件名**：macOS 给 NFD、Linux 给 NFC，身份键必须归一后再比，否则同一文件被判成 `duplicate` 或 `moved`。期望行为：两种编码喂进去得到同一个 key。
- **空输入**：0 字节文件、只有 frontmatter 的 md、0 字节图片都不能 panic。期望行为：产出明确 warning 并继续，其余源不受影响。
- **封面扫描、正文有字的 PDF**：探针不能判成 `no_text_layer`。期望行为：抽样六页里只要一页有文本操作符，就走正常抽取。
- **写入中的文件**：同步服务写到一半时 `mtime` 抖动、读取中途 `ENOENT`。期望行为：账本不前移，标 `racy` 或 `vanished` 留到下一轮。
- **需要转义的路径**：图片名含空格、括号、`#`、`%` 时，写进 raw 的 markdown 链接必须用尖括号形式或百分号转义，否则页面渲染出来是断链。

---

## File Structure

```
wiki-skills/            # 仓库根即 crate 根
├── Cargo.toml
├── src/
│   ├── main.rs          # 参数解析与分发，唯一入口
│   ├── cli.rs           # clap 定义、子命令枚举
│   ├── report.rs        # JSON 信封、退出码
│   ├── paths.rs         # 身份键、忽略规则、slash 归一
│   ├── hash.rs          # blake3、抽样哈希
│   ├── config.rs        # wiki.toml 结构、加载、校验
│   ├── init.rs          # init：写契约文件与目录
│   ├── store.rs         # state.db：schema、迁移、读写
│   ├── lock.rs          # .wiki/lock 文件锁
│   ├── scan.rs          # 两段扫描与变更分类
│   ├── parse/
│   │   ├── mod.rs       # Parser trait、注册表、auto 回退
│   │   ├── text.rs      # md/txt
│   │   ├── html.rs      # html 正文抽取
│   │   ├── pdf.rs       # 探针 + 分页抽取
│   │   ├── asset.rs     # 图片（只产链接）
│   │   └── external.rs  # HTTP 解析服务适配器
│   ├── convert.rs       # source→raw 落盘、frontmatter
│   ├── page.rs          # 页面 frontmatter 读取与行级回写
│   ├── chunk.rs         # 大文件分块与按需取块
│   ├── staging.rs       # 候选快照与 manifest
│   ├── ingest.rs        # begin / commit / abort
│   ├── render.rs        # index.md 与 log.md 生成
│   ├── lint.rs          # L1–L12
│   ├── version.rs       # versions、checkpoint、决策门
│   ├── sync.rs          # 无人值守编排
│   ├── query.rs         # FTS5 检索与 Hit
│   ├── graph.rs         # 链接图与反链
│   └── doctor.rs        # 自检
└── tests/
    ├── cli_smoke.rs     # Task 1
    ├── page_frontmatter.rs … query.rs   # Task 2–20，每个 Task 一个文件
    ├── e2e.rs           # Task 21
    └── fixtures/        # 8 个难看样本
```

每个文件一个职责；`main.rs` 只做分发，不放业务逻辑。

---

### Task 1: 骨架、JSON 信封与退出码

**Files:**
- Create: `Cargo.toml`, `src/main.rs`, `src/cli.rs`, `src/report.rs`
- Test: `tests/cli_smoke.rs`

**Interfaces:**
- Consumes: 无
- Produces: `report::Exit`（`Ok=0, Error=1, Usage=2, NeedsConfirm=3, Busy=4`）；`report::emit<T: Serialize>(cmd: &str, data: T) -> !`；`report::fail(cmd: &str, exit: Exit, code: &str, msg: &str) -> !`

- [ ] **Step 1: 写失败的冒烟测试**

```rust
// tests/cli_smoke.rs
use assert_cmd::Command;

#[test]
fn version_emits_one_json_document() {
    let out = Command::cargo_bin("llm-wiki").unwrap()
        .args(["--json", "version"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["cmd"], "version");
    assert!(v["data"]["version"].is_string());
}

#[test]
fn unknown_subcommand_exits_2() {
    let out = Command::cargo_bin("llm-wiki").unwrap()
        .args(["--json", "nope"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "E_USAGE");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test cli_smoke`
Expected: FAIL，`no binary llm-wiki` 或编译失败。

- [ ] **Step 3: 建 crate 与 JSON 信封**

`Cargo.toml` 依赖固定为：`clap = { version = "4", features = ["derive"] }`、`serde = { version = "1", features = ["derive"] }`、`serde_json = "1"`、`toml = "0.8"`、`blake3 = "1"`、`rusqlite = { version = "0.31", features = ["bundled"] }`、`unicode-normalization = "0.1"`、`thiserror = "1"`、`chrono = { version = "0.4", features = ["serde"] }`、`walkdir = "2"`、`scraper = "0.19"`、`lopdf = "0.32"`、`pdf-extract = "0.7"`、`ureq = { version = "3", features = ["multipart"] }`（2.x 无 multipart feature，只能 3.x）；dev：`tempfile = "3"`、`assert_cmd = "2"`、`predicates = "3"`、`serde_json = "1"`。

`report.rs` 提供信封：成功是 `{"ok":true,"cmd":"<name>","data":{...}}`，失败是 `{"ok":false,"cmd":"<name>","error":{"code":"E_*","message":"..."}}`。`--json` 关闭时同一份数据渲染成人读文本，退出码相同。

`cli.rs` 用 `#[derive(Parser)]`，`--json` 是全局 flag，子命令枚举先只放 `Version`，后续任务逐个加。

`main.rs`：解析 → 分发 → 用 `report::emit` 或 `report::fail` 收尾。clap 的解析错误要转成 `E_USAGE` + 退出码 2，不能让它按默认格式打到 stderr。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test cli_smoke`
Expected: PASS，2 passed。

- [ ] **Step 5: 提交**

```bash
git add Cargo.toml src/main.rs src/cli.rs src/report.rs tests/cli_smoke.rs
git commit -m "feat: crate skeleton with json envelope and exit codes"
```

---

### Task 2: 路径身份键与哈希

**Files:**
- Create: `src/paths.rs`, `src/hash.rs`
- Modify: `src/main.rs:1-40`（加 `mod` 声明）
- Test: `tests/paths.rs`

**Interfaces:**
- Consumes: 无
- Produces:
  - `paths::identity_key(root: &Path, p: &Path) -> Result<String>`
  - `paths::is_ignored(rel: &str) -> bool`
  - `paths::to_slash(p: &Path) -> String`
  - `paths::join_key(root: &Path, key: &str) -> PathBuf`
  - `hash::hash_bytes(b: &[u8]) -> String`（返回 `"blake3:<hex>"`）
  - `hash::hash_file(p: &Path, full: bool) -> Result<String>`

- [ ] **Step 1: 写失败的测试**

```rust
// tests/paths.rs
use llm_wiki::paths::{identity_key, is_ignored, to_slash};
use std::path::Path;

#[test]
fn nfd_and_nfc_inputs_produce_same_key() {
    let root = Path::new("/root");
    let nfc = identity_key(root, Path::new("/root/资料/café.md")).unwrap();
    let nfd = identity_key(root, Path::new("/root/资料/cafe\u{0301}.md")).unwrap();
    assert_eq!(nfc, "资料/café.md");
    assert_eq!(nfc, nfd);
}

#[test]
fn case_is_preserved_and_separators_are_slashes() {
    let k = identity_key(Path::new("/r"), Path::new("/r/a/b.MD")).unwrap();
    assert_eq!(k, "a/b.MD");
}

#[test]
fn dot_prefixed_components_are_ignored() {
    assert!(is_ignored(".git/config"));
    assert!(is_ignored("a/.hidden/x.md"));
    assert!(!is_ignored("a/b.md"));
}

#[test]
fn path_outside_root_is_an_error() {
    assert!(identity_key(Path::new("/r"), Path::new("/other/x.md")).is_err());
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test paths`
Expected: FAIL，`unresolved import llm_wiki`。为此需要在 `src/lib.rs` 暴露模块——本步同时创建 `src/lib.rs`，内容为 `pub mod paths; pub mod hash;`，`main.rs` 改为使用 `llm_wiki::` 前缀。

- [ ] **Step 3: 实现 identity_key / is_ignored / hash**

`identity_key`：`strip_prefix(root)` → 逐组件 `unicode_normalization::UnicodeNormalization::nfc()` → 拼 `/`。任一组件的 NFC 结果与原名不同**不报错**，只是归一。组件为空或 `.` 跳过；`..` 直接报错。

`is_ignored`：任一组件以 `.` 开头即为真。

`hash_file`：`full == true` 用流式 blake3；`false` 时读头 1MB + 中段 1MB + 尾 1MB，与 `size`、`mtime` 一起作为返回值参与比较的指纹（抽样哈希单独命名 `hash::fingerprint(p) -> Result<Fingerprint>`，`Fingerprint { size: u64, mtime: i64, sample: String }`）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test paths`
Expected: PASS，4 passed。

- [ ] **Step 5: 提交**

```bash
git add src/lib.rs src/paths.rs src/hash.rs src/main.rs tests/paths.rs
git commit -m "feat: nfc path identity keys and blake3 hashing"
```

---

### Task 3: 配置加载与 init

**Files:**
- Create: `src/config.rs`, `src/init.rs`
- Modify: `src/cli.rs`（加 `Init` 子命令）、`src/lib.rs`
- Test: `tests/init.rs`

**Interfaces:**
- Consumes: `paths::`
- Produces:
  - `config::Config` 结构，字段与 wiki.toml 同名分区一一对应
  - `config::Config::load(root: &Path) -> Result<Config>`
  - `config::Config::validate(&self) -> Result<()>`
  - `init::run(root: &Path, force: bool) -> Result<InitOutcome>`，`InitOutcome { created: Vec<String>, skipped: Vec<String> }`

- [ ] **Step 1: 写失败的测试**

```rust
// tests/init.rs
use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn init_creates_contracts_and_engine_dirs() {
    let d = tempdir().unwrap();
    let out = Command::cargo_bin("llm-wiki").unwrap()
        .args(["--json", "init"]).current_dir(d.path()).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    for p in ["wiki.toml", "SCHEMA.md", "source", "raw", "wiki",
              ".wiki/cache", ".wiki/staging", ".wiki/versions"] {
        assert!(d.path().join(p).exists(), "missing {p}");
    }
}

#[test]
fn init_is_idempotent_and_does_not_overwrite() {
    let d = tempdir().unwrap();
    Command::cargo_bin("llm-wiki").unwrap().args(["--json","init"]).current_dir(d.path()).output().unwrap();
    std::fs::write(d.path().join("wiki.toml"), "[paths]\nsource=\"S\"\n").unwrap();
    let out = Command::cargo_bin("llm-wiki").unwrap()
        .args(["--json","init"]).current_dir(d.path()).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let body = std::fs::read_to_string(d.path().join("wiki.toml")).unwrap();
    assert!(body.contains("source=\"S\""));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test init`
Expected: FAIL，`unrecognized subcommand 'init'`。

- [ ] **Step 3: 实现 Config 与 init**

`Config::load` 读 `wiki.toml`；缺失时报 `E_NO_CONFIG` 并提示跑 `init`。所有分区都要有 `#[serde(default)]`，缺省值取自设计稿 §3 的完整示例（`max_source_bytes = "100MB"`、`chunk_threshold = "2MB"`、`chunk_target = "64KB"`、`stable_streak = 3`、`read_retries = 3`、`lag_warn_rounds = 3`、`versions.keep = 5`）。

`Config::validate` 检查：`types` 非空且无重复；`parse.primary ∈ {auto,builtin,external}`；尺寸字符串能被解析成字节数（`"2MB"`/`"100MB"`/`"64KB"`/纯数字）；`[parse.external]` 存在时 `auth_env` 必须给名字而非 token 值——值匹配 `^(sk-|ghp_|eyJ)` 或长度 > 40 的连续串时报错 `E_TOKEN_IN_CONFIG`。

`init::run` 只在目标不存在时写文件；已存在就进 `skipped`。`SCHEMA.md` 内容固定为：机制字段清单、`type` 枚举来源说明、图片相对路径 `../../source/` 约定、`status` 三态含义。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test init`
Expected: PASS，2 passed。

- [ ] **Step 5: 提交**

```bash
git add src/config.rs src/init.rs src/cli.rs src/lib.rs tests/init.rs
git commit -m "feat: config loading and idempotent init"
```

---

### Task 4: state.db 与迁移

**Files:**
- Create: `src/store.rs`
- Modify: `src/lib.rs`
- Test: `tests/store.rs`

**Interfaces:**
- Consumes: 无
- Produces:
  - `store::SourceRow { key: String, hash: String, size: i64, mtime: i64, raw_key: Option<String>, converted_at: Option<String> }`
  - `store::Store::open(path: &Path) -> Result<Store>`；`Store::migrate(&mut self) -> Result<()>`
  - `Store::all_sources(&self) -> Result<Vec<SourceRow>>`；`get_source(&self, key: &str) -> Result<Option<SourceRow>>`；`upsert_source(&mut self, row: &SourceRow) -> Result<()>`；`delete_source(&mut self, key: &str) -> Result<()>`
  - `Store::pages_for_source(&self, raw_key: &str) -> Result<Vec<String>>`；`set_source_pages(&mut self, raw_key: &str, pages: &[String]) -> Result<()>`
  - `Store::fts5_available(&self) -> Result<bool>`

- [ ] **Step 1: 写失败的测试**

```rust
// tests/store.rs
use llm_wiki::store::{SourceRow, Store};
use tempfile::tempdir;

#[test]
fn rows_survive_reopen() {
    let d = tempdir().unwrap();
    let p = d.path().join("state.db");
    {
        let mut s = Store::open(&p).unwrap();
        s.migrate().unwrap();
        s.upsert_source(&SourceRow{ key:"a.pdf".into(), hash:"blake3:aa".into(),
            size:10, mtime:1, raw_key:None, converted_at:None }).unwrap();
    }
    let s = Store::open(&p).unwrap();
    assert_eq!(s.get_source("a.pdf").unwrap().unwrap().hash, "blake3:aa");
}

#[test]
fn bundled_sqlite_has_fts5() {
    let d = tempdir().unwrap();
    let mut s = Store::open(&d.path().join("state.db")).unwrap();
    s.migrate().unwrap();
    assert!(s.fts5_available().unwrap(), "bundled sqlite must ship FTS5");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test store`
Expected: FAIL，`unresolved import llm_wiki::store`。

- [ ] **Step 3: 建表与迁移**

迁移用 `PRAGMA user_version` 做版本号，从 0 升到 1 时建表：

```sql
CREATE TABLE sources(key TEXT PRIMARY KEY, hash TEXT NOT NULL, size INTEGER NOT NULL,
                     mtime INTEGER NOT NULL, raw_key TEXT, converted_at TEXT);
CREATE TABLE source_pages(raw_key TEXT NOT NULL, page_id TEXT NOT NULL,
                          PRIMARY KEY(raw_key, page_id));
CREATE TABLE versions(id INTEGER PRIMARY KEY, created_at TEXT NOT NULL,
                      note TEXT, kind TEXT NOT NULL);
CREATE TABLE pending(id INTEGER PRIMARY KEY CHECK(id=1), raw_key TEXT, raw_hash TEXT,
                     index_hash TEXT, log_hash TEXT, began_at TEXT);
CREATE TABLE sync_state(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE VIRTUAL TABLE fts USING fts5(page_id UNINDEXED, title, body, tokenize='unicode61');
```

`fts5_available` 执行 `SELECT count(*) FROM pragma_compile_options WHERE compile_options='ENABLE_FTS5'` 并断言 > 0——这是本任务的硬门禁，bundled 构建若不带 FTS5，后续 Task 20 会全线返工。

`open` 设 `PRAGMA journal_mode=WAL` 与 `PRAGMA foreign_keys=ON`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test store`
Expected: PASS，2 passed。

- [ ] **Step 5: 提交**

```bash
git add src/store.rs src/lib.rs tests/store.rs
git commit -m "feat: sqlite state store with migrations"
```

---

### Task 5: 扫描 A 段（source 层）

**Files:**
- Create: `src/scan.rs`
- Modify: `src/cli.rs`（加 `Scan` 子命令）、`src/lib.rs`
- Test: `tests/scan_signature.rs`

**Interfaces:**
- Consumes: `paths::`, `hash::`, `store::`, `config::`
- Produces:
  - `scan::ChangeKind { New, Changed, Moved, Missing, Duplicate, Ignored, Oversize }`
  - `scan::Change { kind: ChangeKind, key: String, hash: Option<String>, moved_from: Option<String> }`
  - `scan::scan_source(root: &Path, cfg: &Config, store: &Store) -> Result<Vec<Change>>`
  - `scan::ScanReport { changes: Vec<Change>, ignored_count: usize }`

- [ ] **Step 1: 写失败的测试**

```rust
// tests/scan_signature.rs
use llm_wiki::{config::Config, init, scan, store::Store};
use std::fs;
use tempfile::tempdir;

fn setup() -> (tempfile::TempDir, Store, Config) {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    (d, s, cfg)
}

#[test]
fn classifies_new_changed_moved_missing_duplicate() {
    let (d, mut s, cfg) = setup();
    let src = d.path().join("source");
    fs::write(src.join("a.md"), "alpha").unwrap();
    fs::write(src.join("b.md"), "same").unwrap();
    fs::write(src.join("c.md"), "same").unwrap();   // 与 b 同内容
    fs::write(src.join("x.unknownext"), "??").unwrap(); // 白名单外

    let rep = scan::scan_source(d.path(), &cfg, &s).unwrap();
    let kinds = |k| rep.changes.iter().filter(|c| c.kind == k).count();
    assert_eq!(kinds(scan::ChangeKind::New), 2);        // a 和一个规范路径
    assert_eq!(kinds(scan::ChangeKind::Duplicate), 1);
    assert_eq!(rep.ignored_count, 1);

    // 记 b 的账，然后改名 → moved
    let b = rep.changes.iter().find(|c| c.key == "b.md").unwrap();
    s.upsert_source(&llm_wiki::store::SourceRow{ key:"b.md".into(),
        hash: b.hash.clone().unwrap(), size:4, mtime:1,
        raw_key:None, converted_at:None }).unwrap();
    fs::rename(src.join("b.md"), src.join("b2.md")).unwrap();
    let rep = scan::scan_source(d.path(), &cfg, &s).unwrap();
    let moved = rep.changes.iter().find(|c| c.kind == scan::ChangeKind::Moved).unwrap();
    assert_eq!(moved.moved_from.as_deref(), Some("b.md"));
    assert_eq!(moved.key, "b2.md");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test scan_signature`
Expected: FAIL，`unresolved import llm_wiki::scan`。

- [ ] **Step 3: 实现两阶段比对**

顺序是固定的，实现不能改：

1. `walkdir` 遍历 `source/`，`follow_links(false)`，跳过 `is_ignored`。
2. 扩展名不在 `cfg.formats.allow` → `ignored_count += 1`，不产生 Change。
3. 文件 size > `cfg.parse.max_source_bytes` → `Oversize`，不读内容。
4. 算哈希（≤ `chunk_threshold` 全量，否则 `Fingerprint`）。
5. 用 key 查账本：无记录 → `New`；有且哈希相同 → 无变更（不产生 Change）；有且不同 → `Changed`。
6. 本轮所有 `New` 候选按哈希分组：同一哈希按 key 字典序保留第一份，其余改判 `Duplicate`
   （必须先做这一步，否则同哈希的第二个副本会被误判成 `Moved`）。
7. 剩下的 `New` 候选，若账本里存在同哈希、且本轮磁盘上已消失的 key（即别的记录会判成 `Missing`），
   改判 `Moved`，`moved_from` 填那个 key。**一个账本 key 只能被认领一次**，被认领后不再参与匹配。
8. 账本里有、磁盘上没有 → `Missing`。

`scan` 只读 state（streak 计数在 Task 19 的 sync 里做），测试里连续调用两次必须得到相同结果。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test scan_signature`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/scan.rs src/cli.rs tests/scan_signature.rs
git commit -m "feat: source layer scan with five change kinds"
```

---

### Task 6: Parser trait、text 后端与 convert

**Files:**
- Create: `src/parse/mod.rs`, `src/parse/text.rs`, `src/convert.rs`
- Modify: `src/lib.rs`, `src/cli.rs`（加 `Convert`、`Parse`）
- Test: `tests/convert_text.rs`

**Interfaces:**
- Consumes: `config::`, `hash::`, `scan::`
- Produces:
  - `parse::Extracted { markdown: String, warnings: Vec<Warning>, asset: bool, no_text_layer: bool }`
  - `parse::Warning { code: String, message: String }`
  - `trait Parser { fn id(&self) -> &'static str; fn can_handle(&self, ext: &str) -> bool; fn parse(&self, input: &Source) -> Result<Extracted>; }`
  - `parse::registry(cfg: &Config) -> Vec<Box<dyn Parser>>`
  - `parse::parse_with(cfg: &Config, src: &Source) -> Result<(Extracted, String)>`（第二个值是选中的 backend id）
  - `convert::convert_one(root, cfg, store, key: &str) -> Result<ConvertOutcome>`
  - `ConvertOutcome { raw_path: String, chars: usize, backend: String, duration_ms: u64, warnings: Vec<Warning> }`

- [ ] **Step 1: 写失败的测试**

```rust
// tests/convert_text.rs
use llm_wiki::{config::Config, convert, init, store::Store};
use std::fs;
use tempfile::tempdir;

#[test]
fn md_source_becomes_raw_with_traceability_frontmatter() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    fs::create_dir_all(d.path().join("source/子目录")).unwrap();
    fs::write(d.path().join("source/子目录/说明.md"), "---\ntitle: 忽略\n---\n# 正文\n\n内容\n").unwrap();

    let out = convert::convert_one(d.path(), &cfg, &mut s, "子目录/说明.md").unwrap();
    assert_eq!(out.raw_path, "raw/子目录/说明.md");
    assert_eq!(out.backend, "builtin-text");
    let raw = fs::read_to_string(d.path().join("raw/子目录/说明.md")).unwrap();
    assert!(raw.contains("source: source/子目录/说明.md"));
    assert!(raw.contains("parser: builtin-text"));
    assert!(raw.contains("pin: false"));
    assert!(!raw.contains("title: 忽略"), "源文件的 frontmatter 必须被剥掉");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test convert_text`
Expected: FAIL，`unresolved import llm_wiki::convert`。

- [ ] **Step 3: 实现 trait、text 后端与落盘**

`text.rs`：读 UTF-8；`---` 开头则剥掉首个 frontmatter 块；CRLF 转 LF；正文长度 0 → `Warning { code: "empty", .. }`。后端 id 固定 `"builtin-text"`。

`convert.rs` 写 `raw/<源相对路径去掉扩展名>.md`，frontmatter 键序固定：

```
source / source_hash / parser / converted_at / asset / no_text_layer / pin
```

`asset` 与 `no_text_layer` 只在为 true 时出现。写完调 `store.upsert_source` 记 `raw_key` 与 `converted_at`。

`parse` 子命令只打印 `convert_one` 同构的结构，不落盘、不记账本。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test convert_text`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/parse/mod.rs src/parse/text.rs src/convert.rs src/cli.rs tests/convert_text.rs
git commit -m "feat: parser trait and text backend with raw conversion"
```

---

### Task 7: 页面 frontmatter 读取与行级回写

**Files:**
- Create: `src/page.rs`
- Modify: `src/lib.rs`
- Test: `tests/page_frontmatter.rs`

**Interfaces:**
- Consumes: 无
- Produces:
  - `page::Status { Draft, Stable, Stale }`（`as_str()` 返回 `"draft"|"stable"|"stale"`）
  - `page::FrontMatter { id, title, typ, summary, tags: Vec<String>, sources: Vec<String>, status: Status, updated: NaiveDate, extra: Vec<(String, String)> }`
  - `page::read(p: &Path) -> Result<(FrontMatter, String)>`（第二项是正文）
  - `page::write_updated(p: &Path, d: NaiveDate) -> Result<()>`
  - `page::set_status(p: &Path, s: Status) -> Result<()>`

- [ ] **Step 1: 写失败的测试**

```rust
// tests/page_frontmatter.rs
use chrono::NaiveDate;
use llm_wiki::page;
use std::fs;
use tempfile::tempdir;

const SRC: &str = "---\n# 这是注释，必须保留\nstatus: draft   # 行尾注释也保留\nid: order\nunknown_key: 保留我\ntitle: \"订单\"\nupdated: 2026-01-01\n---\n\n正文\n";

#[test]
fn write_updated_touches_only_that_line() {
    let d = tempdir().unwrap();
    let p = d.path().join("order.md");
    fs::write(&p, SRC).unwrap();
    page::write_updated(&p, NaiveDate::from_ymd_opt(2026,10,27).unwrap()).unwrap();
    let got = fs::read_to_string(&p).unwrap();
    let expect = SRC.replace("updated: 2026-01-01", "updated: 2026-10-27");
    assert_eq!(got, expect, "除 updated 外必须逐字节不变");
}

#[test]
fn read_parses_known_and_keeps_unknown() {
    let d = tempdir().unwrap();
    let p = d.path().join("order.md");
    fs::write(&p, SRC).unwrap();
    let (fm, body) = page::read(&p).unwrap();
    assert_eq!(fm.id, "order");
    assert_eq!(fm.typ, "");
    assert_eq!(fm.status, page::Status::Draft);
    assert!(body.contains("正文"));
    assert!(fm.extra.iter().any(|(k,_)| k == "unknown_key"));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test page_frontmatter`
Expected: FAIL，`unresolved import llm_wiki::page`。

- [ ] **Step 3: 实现行级替换**

**不做全量 round-trip**。`write_updated` 的实现是：找到 frontmatter 内行首为 `updated:` 的那一行，只替换冒号后的值，行尾注释、缩进、引号风格全部保持。找不到就报 `E_NO_UPDATED`，不擅自插入。

`read` 用最小 YAML 子集解析（键值、`[a, b]` 行内数组），解析失败报 `E_BAD_FRONTMATTER` 并携带行号。未知键进 `extra`，不改写。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test page_frontmatter`
Expected: PASS，2 passed。

- [ ] **Step 5: 提交**

```bash
git add src/page.rs src/lib.rs tests/page_frontmatter.rs
git commit -m "feat: line-level frontmatter writeback"
```

---

### Task 8: html 后端

**Files:**
- Create: `src/parse/html.rs`
- Modify: `src/parse/mod.rs`
- Test: `tests/parse_html.rs`

**Interfaces:**
- Consumes: `parse::{Parser, Extracted, Warning}`
- Produces: `parse::html::HtmlParser`，`id() == "builtin-html"`，`can_handle` 认 `html|htm`

- [ ] **Step 1: 写失败的测试**

```rust
// tests/parse_html.rs
use llm_wiki::parse::html::HtmlParser;
use llm_wiki::parse::{Parser, Source};

#[test]
fn drops_navigation_and_keeps_article() {
    let html = r#"<html><body><nav>导航</nav>
        <article><h1>订单</h1><p>正文段落</p><a href="/x">链接</a></article>
        <script>var a=1</script></body></html>"#;
    let out = HtmlParser::new().parse(&Source::inline("x.html", html.as_bytes())).unwrap();
    assert!(out.markdown.contains("# 订单"));
    assert!(out.markdown.contains("正文段落"));
    assert!(out.markdown.contains("[链接](/x)"));
    assert!(!out.markdown.contains("导航"));
    assert!(!out.markdown.contains("var a=1"));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test parse_html`
Expected: FAIL，模块不存在。

- [ ] **Step 3: 实现抽取**

`scraper` 解析；容器优先级 `article` → `main` → `body`；删除 `script/style/nav/footer/aside`。映射：`h1..h3` → `#..###`，`p` → 空行分段，`a` → `[文本](href)`，`img` → `![alt](src)`，`li` → `- `，`pre/code` → 围栏代码块。抽不出正文（结果 < 1 字符）时返回 `Warning { code: "empty" }`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test parse_html`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/parse/html.rs src/parse/mod.rs tests/parse_html.rs
git commit -m "feat: builtin-html parser"
```

---

### Task 9: asset 后端（图片只做链接）

**Files:**
- Create: `src/parse/asset.rs`
- Modify: `src/parse/mod.rs`, `src/config.rs`（`formats.allow` 加图片扩展名）
- Test: `tests/parse_asset.rs`

**Interfaces:**
- Consumes: `parse::{Parser, Extracted}`
- Produces:
  - `parse::asset::AssetParser`，`id() == "builtin-asset"`，`can_handle` 认 `png|jpg|jpeg|gif|webp|svg`
  - `parse::asset::link_for(source_key: &str) -> String`——生成指向 `source/` 的相对链接；`source_key` 不含 `source/` 前缀

- [ ] **Step 1: 写失败的测试**

```rust
// tests/parse_asset.rs
use llm_wiki::parse::asset::{link_for, AssetParser};
use llm_wiki::parse::{Parser, Source};

#[test]
fn image_becomes_link_only_markdown() {
    let out = AssetParser::new().parse(&Source::inline("dir/a.png", b"\x89PNG")).unwrap();
    assert!(out.asset);
    assert!(!out.no_text_layer);
    assert_eq!(out.markdown.trim(), "![](../../source/dir/a.png)");
}

#[test]
fn names_with_spaces_and_parens_are_angle_bracketed() {
    let link = link_for("dir/图 (1)#x.png");
    assert_eq!(link, "![](<../../source/dir/图 (1)#x.png>)");
}

#[test]
fn empty_image_file_is_not_a_crash() {
    let out = AssetParser::new().parse(&Source::inline("a.png", b"")).unwrap();
    assert!(out.asset);
    assert!(out.warnings.iter().any(|w| w.code == "empty"));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test parse_asset`
Expected: FAIL。

- [ ] **Step 3: 实现链接生成与转义**

相对路径按**源 key 的层级深度**计算：前缀是 `../` 重复「key 的路径分量数」次（含文件名）。`raw/` 镜像 `source/` 的相对结构，所以 `raw/<key 去扩展名>.md` 的深度恰好等于 key 的分量数。例：key `dir/a.png` → 深度 2 → `../../source/dir/a.png`；key `scan.pdf` → 深度 1 → `../source/scan.pdf`。原计划写的「固定 `../../`」在 key 只有一层时是错的（会指到仓库外）。

**本任务还要把后端接线进 `parse::registry`**：Task 8 的 html 后端没有注册，`parse_with("x.html")` 目前报 `E_NO_PARSER`。改为返回 `[text, html, asset]`，并把 `tests/convert_text.rs` 里 `registry().len() == 1` 的断言改成按 id 查找。pdf 由 T10 接、external 由 T11 接。

转义规则：目标路径含空格、`(`、`)`、`#`、`%`、`<`、`>` 中任一 → 用 `<...>` 包裹；否则原样。空文件仍然产出链接，追加 `empty` warning。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test parse_asset`
Expected: PASS，3 passed。

- [ ] **Step 5: 提交**

```bash
git add src/parse/asset.rs src/parse/mod.rs src/config.rs tests/parse_asset.rs
git commit -m "feat: asset parser emitting link-only markdown"
```

---

### Task 10: pdf 后端与无文本层探针

**Files:**
- Create: `src/parse/pdf.rs`, `tests/common/mod.rs`
- Modify: `src/parse/mod.rs`
- Test: `tests/parse_pdf.rs`

**Interfaces:**
- Consumes: `parse::{Parser, Extracted, Warning}`
- Produces:
  - `parse::pdf::PdfParser`，`id() == "builtin-pdf"`，`can_handle` 认 `pdf`
  - `parse::pdf::has_text_layer(path: &Path) -> Result<bool>`——抽样头 2 + 中 2 + 尾 2 页
  - `tests::common::make_pdf(path: &Path, body: &str, pages: usize)`、`make_scanned_pdf(path: &Path, pages: usize)`（后者内容流里不含文本操作符）、`make_mixed_pdf(path: &Path)`（第 1 页无文本、第 5 页有）
  - `parse::Source::path(p: &Path) -> Result<Source>`（读文件字节，`key` 取文件名、`ext` 取扩展名）——测试用到，若 Task 6 没建就本任务补在 `src/parse/mod.rs`
  - 探针内部用 `Document::load_mem(&bytes)`，对外仍暴露 `has_text_layer(path: &Path)`，两者共用一个接收 `&[u8]` 的内部函数

- [ ] **Step 1: 写失败的测试**

```rust
// tests/parse_pdf.rs
mod common;
use llm_wiki::parse::pdf::{has_text_layer, PdfParser};
use llm_wiki::parse::{Parser, Source};
use tempfile::tempdir;

#[test]
fn text_pdf_has_text_layer() {
    let d = tempdir().unwrap();
    let p = d.path().join("t.pdf");
    common::make_pdf(&p, "hello world", 4);
    assert!(has_text_layer(&p).unwrap());
    let out = PdfParser::new().parse(&Source::path(&p)).unwrap();
    assert!(out.markdown.contains("hello world"));
    assert!(!out.asset);
}

#[test]
fn scanned_pdf_degrades_to_asset_link() {
    let d = tempdir().unwrap();
    let p = d.path().join("scan.pdf");
    common::make_scanned_pdf(&p, 6);
    assert!(!has_text_layer(&p).unwrap());
    let out = PdfParser::new().parse(&Source::path(&p)).unwrap();
    assert!(out.asset);
    assert!(out.no_text_layer);
    assert!(out.markdown.contains("](../source/scan.pdf)"), "key 只有一层，前缀是一个 ../");
}

#[test]
fn cover_scan_with_text_body_is_not_degraded() {
    let d = tempdir().unwrap();
    let p = d.path().join("mixed.pdf");
    common::make_mixed_pdf(&p);   // 第 1 页无文本，第 5 页有
    assert!(has_text_layer(&p).unwrap());
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test parse_pdf`
Expected: FAIL。

- [ ] **Step 3: 实现探针与抽取**

探针不解析字体：用 `lopdf` 打开，取页对象 id 列表，抽样 `head 2 + middle 2 + tail 2`（不足六页则全取），对每页 `doc.get_page_content(id)` 拿解压后的内容流，检测是否存在文本操作符 `Tj`、`TJ`、`'`、`"`（按 token 边界匹配，避免匹配到字符串里的字母）。**六页全无才算扫描件**；任何一页有就返回 true。`lopdf` 打开失败时返回 true，让后续抽取去报真实错误。

抽取用 `pdf_extract::extract_text`，按 `\u{0C}` 切页；没有分隔符就单段输出并记 `page_split_unknown` warning。扫描件路径不调 `pdf-extract`，直接构造 `Extracted { asset: true, no_text_layer: true, markdown: link_for(...) }`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test parse_pdf`
Expected: PASS，3 passed。

- [ ] **Step 5: 提交**

```bash
git add src/parse/pdf.rs tests/common/mod.rs tests/parse_pdf.rs
git commit -m "feat: pdf parser with text-layer probe and no-OCR degradation"
```

---

### Task 11: external-http 后端与 auto 回退

**Files:**
- Create: `src/parse/external.rs`
- Modify: `src/parse/mod.rs`
- Test: `tests/parse_external.rs`

**Interfaces:**
- Consumes: `config::ParseCfg`
- Produces:
  - `parse::external::ExternalParser::from_config(&cfg.parse.external) -> Result<Self>`，`id() == "external-http"`
  - `parse::parse_with(cfg, src) -> Result<(Extracted, String)>` 的 `[parse].primary` 三分支逻辑
  - `parse::pick_backend(cfg, ext: &str) -> Vec<&'static str>`——返回尝试顺序，供测试断言

- [ ] **Step 1: 写失败的测试**

```rust
// tests/parse_external.rs
mod mock_http;
use llm_wiki::parse::pick_backend;

#[test]
fn auto_order_is_builtin_then_external_for_pdf() {
    let mut cfg = llm_wiki::config::Config::default();
    cfg.parse.primary = llm_wiki::config::Primary::Auto;
    cfg.parse.external = Some(mock_http::sample_external());
    assert_eq!(pick_backend(&cfg, "pdf"), vec!["builtin-pdf", "external-http"]);
    assert_eq!(pick_backend(&cfg, "md"), vec!["builtin-text"]);
}

#[test]
fn builtin_mode_never_tries_external() {
    let mut cfg = llm_wiki::config::Config::default();
    cfg.parse.primary = llm_wiki::config::Primary::Builtin;
    cfg.parse.external = Some(mock_http::sample_external());
    assert_eq!(pick_backend(&cfg, "pdf"), vec!["builtin-pdf"]);
}

#[test]
fn external_500_falls_back_and_reports_both_reasons() {
    let srv = mock_http::serve_once(500, "boom");
    let mut cfg = llm_wiki::config::Config::default();
    cfg.parse.primary = llm_wiki::config::Primary::External;
    cfg.parse.external = Some(mock_http::external_at(&srv.url));
    let err = llm_wiki::parse::parse_with(&cfg, &llm_wiki::parse::Source::inline("a.pdf", b"%PDF")).unwrap_err();
    assert!(err.to_string().contains("500"));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test parse_external`
Expected: FAIL，模块不存在。

- [ ] **Step 3: 实现适配器与回退**

外部服务：`ureq` multipart，字段顺序与名字来自 `cfg.parse.external.fields`（`{file="@file", fileFlag="N", system="BSS"}` 里的 `@file` 表示取文件内容）；header `Authorization: Bearer ${std::env::var(auth_env)?}`，环境变量缺失报 `E_NO_AUTH_ENV` 且**不含 token 值**；`timeout_s` 用 `ureq::Agent` 的全局超时（`config_builder().timeout_global(..)`）；响应 `resp.status()` 非 2xx 视为失败，正文用 `resp.body_mut().read_to_string()` 读取，再按 JSON 路径（如 `data`）取 markdown。注意 ureq 3 与 2.x API 不同，不要照抄旧片段。

`pick_backend` 规则：`auto` 下 `md|txt` 只走内置；`pdf|html` 走内置 + （`external` 已配置时才加）外部；图片只走 `builtin-asset`，永不进外部。`auto` 的内置产出不达标（`chars < min_chars` 或 `needs_ocr`）时才试外部；外部失败把两个原因都放进 `Error::BothFailed { builtin: String, external: String }`。

> 配置补充（本任务负责）：设计稿 §3 已补 `[parse].min_chars = 200`（决策 37）。本任务需要在 `src/config.rs` 的 `Parse` 分区加 `pub min_chars: usize`（默认 200），并在 `src/init.rs` 的 wiki.toml 模板里加上这一行，附一条「模板可 load+validate 回读」的断言；否则 `chars < min_chars` 无处可取。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test parse_external`
Expected: PASS，3 passed。

- [ ] **Step 5: 提交**

```bash
git add src/parse/external.rs tests/mock_http.rs tests/parse_external.rs
git commit -m "feat: external http parser with auto fallback"
```

---

### Task 12: 大文件分块与按需取块

**Files:**
- Create: `src/chunk.rs`
- Modify: `src/parse/mod.rs`、`src/cli.rs`（`Parse` 加 `--chunk` / `--range`）
- Test: `tests/chunk.rs`

**Interfaces:**
- Consumes: `config::`
- Produces:
  - `chunk::split(text: &str, target: usize) -> Vec<(usize, usize)>`（返回字节区间）
  - `chunk::materialize(cache_dir: &Path, hash: &str, text: &str, target: usize) -> Result<Vec<PathBuf>>`
  - `chunk::read_range(cache_dir: &Path, hash: &str, from: usize, to: usize) -> Result<String>`

- [ ] **Step 1: 写失败的测试**

```rust
// tests/chunk.rs
use llm_wiki::chunk;
use tempfile::tempdir;

#[test]
fn reassembly_is_lossless() {
    let text = "中文行\n".repeat(200_000);          // 约 1.4MB
    let ranges = chunk::split(&text, 64 * 1024);
    assert!(ranges.len() > 10);
    let joined: String = ranges.iter().map(|(a,b)| &text[*a..*b]).collect();
    assert_eq!(joined, text);
}

#[test]
fn boundaries_respect_utf8_and_prefer_newlines() {
    let text = "😀".repeat(1000);
    for (a, b) in chunk::split(&text, 64) {
        assert!(text.is_char_boundary(a) && text.is_char_boundary(b));
    }
    let lines = "abc\n".repeat(100);
    for (_, b) in chunk::split(&lines, 10) { assert!(b == lines.len() || lines.as_bytes()[b] == b'\n' || lines.as_bytes()[b-1] == b'\n'); }
}

#[test]
fn materialize_is_idempotent() {
    let d = tempdir().unwrap();
    let a = chunk::materialize(d.path(), "h1", &"x".repeat(200_000), 64*1024).unwrap();
    let b = chunk::materialize(d.path(), "h1", &"x".repeat(200_000), 64*1024).unwrap();
    assert_eq!(a, b);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test chunk`
Expected: FAIL。

- [ ] **Step 3: 实现切分**

从 `target` 起向前找最近的换行（最多回退 10%），找不到就回退到最近的 UTF-8 边界。切点必须满足 `is_char_boundary`。`materialize` 写到 `.wiki/cache/<hash>/chunk-<n>.md`，已存在则跳过。

`parse --chunk N` 返回单块内容，`--range a-b` 拼接多块；两者都要求文本已缓存。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test chunk`
Expected: PASS，3 passed。

- [ ] **Step 5: 提交**

```bash
git add src/chunk.rs src/parse/mod.rs src/cli.rs tests/chunk.rs
git commit -m "feat: utf8-safe chunking with on-demand ranges"
```

---

### Task 13: 扫描 B 段（规范化层）

**Files:**
- Create: `src/store.rs` 追加迁移 v2
- Modify: `src/scan.rs`, `src/lib.rs`
- Test: `tests/scan_raw.rs`

**Interfaces:**
- Consumes: `store::`, `hash::`
- Produces:
  - `store::RawRow { key: String, hash: String, source_key: String, converted_at: String }`
  - `Store::all_raws(&self) -> Result<Vec<RawRow>>`、`get_raw(&self, key: &str) -> Result<Option<RawRow>>`、`upsert_raw(&mut self, row: &RawRow) -> Result<()>`
  - `scan::scan_raw(root, cfg, store) -> Result<ScanReport>`（与 `scan_source` 一致，测试取 `.changes`）

- [ ] **Step 1: 写失败的测试**

```rust
// tests/scan_raw.rs
use llm_wiki::{config::Config, init, scan, store::{RawRow, Store}};
use std::fs;
use tempfile::tempdir;

#[test]
fn metadata_only_change_on_source_does_not_trigger_raw_change() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    fs::write(d.path().join("raw/a.md"), "正文").unwrap();
    s.upsert_raw(&RawRow{ key:"a.md".into(), hash:"blake3:".to_owned()+"aa".into(),
        source_key:"a.pdf".into(), converted_at:"2026-10-27".into() }).unwrap();

    let changes = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert!(changes.iter().all(|c| c.kind != scan::ChangeKind::Changed));

    fs::write(d.path().join("raw/a.md"), "正文变了").unwrap();
    let changes = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert_eq!(changes.iter().filter(|c| c.kind == scan::ChangeKind::Changed).count(), 1);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test scan_raw`
Expected: FAIL，`no method all_raws`。

- [ ] **Step 3: 加迁移 v2 与 B 段扫描**

```sql
CREATE TABLE raws(key TEXT PRIMARY KEY, hash TEXT NOT NULL,
                  source_key TEXT NOT NULL, converted_at TEXT NOT NULL);
```

迁移按 `PRAGMA user_version` 从 1 升到 2，**不改 v1 的表**。

`scan_raw` 遍历 `raw/`，跳过 `is_ignored`，算内容哈希，与 `raws` 比对，产出 `New / Changed / Missing` 三种（B 段不需要 `moved`/`duplicate`——raw 的路径由 convert 决定，不会漂移；出现同哈希两份说明 convert 有 bug，报 `duplicate`）。

被 `pin: true` 的 raw 的漂移判定**不放在这里**：`convert` 已经会跳过 pinned 的 raw（`skip_reason = "pinned"`），`scan_raw` 只如实报 `Changed`；「pin 之后被人手改过」由 T17 lint 的专门条目负责，本任务不给 `Change` 加字段。

**本任务还要改 `src/convert.rs`**：转换成功后同时 `store.upsert_raw(&RawRow{ ... })`，否则 `raws` 表永远是空的，`scan_raw` 每轮都报 New。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test scan_raw`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/store.rs src/scan.rs src/convert.rs src/lib.rs tests/scan_raw.rs
git commit -m "feat: raw layer scan and migration v2"
```

---

### Task 14: 候选快照与 manifest

**Files:**
- Create: `src/staging.rs`
- Modify: `src/lib.rs`
- Test: `tests/staging.rs`

**Interfaces:**
- Consumes: `config::`, `page::`
- Produces:
  - `staging::ManifestEntry { baseline_hash: Option<String>, current_hash: Option<String>, last_processed_at: Option<String>, pages: Vec<String> }`
  - `staging::Manifest { sources: BTreeMap<String, ManifestEntry> }`，`load(root)` / `save(root, &m)`
  - `staging::ensure(root, cfg) -> Result<EnsureOutcome>`（`EnsureOutcome { created: bool, copied: usize }`）
  - `staging::write_page(root, cfg, id: &str, content: &str) -> Result<()>`
  - `staging::pending_hashes(root) -> Result<BTreeMap<String, String>>`（读 `pending.json`）

- [ ] **Step 1: 写失败的测试**

```rust
// tests/staging.rs
use llm_wiki::{config::Config, init, staging};
use std::fs;
use tempfile::tempdir;

#[test]
fn staging_is_a_real_copy_not_a_hardlink() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    fs::create_dir_all(d.path().join("wiki/concept")).unwrap();
    fs::write(d.path().join("wiki/concept/a.md"), "A").unwrap();

    let out = staging::ensure(d.path(), &cfg).unwrap();
    assert!(out.created);
    let a = d.path().join("wiki/concept/a.md");
    let b = d.path().join(".wiki/staging/concept/a.md");
    assert!(b.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_ne!(fs::metadata(&a).unwrap().ino(), fs::metadata(&b).unwrap().ino(),
                   "禁止 hardlink，会破坏隔离");
    }
}

#[test]
fn ensure_twice_does_not_recopy() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    staging::ensure(d.path(), &cfg).unwrap();
    let again = staging::ensure(d.path(), &cfg).unwrap();
    assert!(!again.created);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test staging`
Expected: FAIL。

- [ ] **Step 3: 实现快照与清单**

`ensure` 首次把 `wiki/` 全量复制到 `.wiki/staging/`（真实复制，逐字节写），并生成空 `manifest.json`；之后只确保目录存在。`write_page` 写 staging 副本并按 id 更新 manifest 的 `pages`。

`manifest.json` 结构：`{"sources": {"<key>": {"baseline_hash":..., "current_hash":..., "last_processed_at":..., "pages":[...]}}}`，键排序稳定，便于 git diff。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test staging`
Expected: PASS，2 passed。

- [ ] **Step 5: 提交**

```bash
git add src/staging.rs src/lib.rs tests/staging.rs
git commit -m "feat: isolated staging snapshot with manifest"
```

---

### Task 15: ingest 三段（begin / commit / abort）

> **执行顺序：T16 先于 T15。** `commit` 要调 `render::write_index`，而 render 在 T16，依赖倒挂。先做 T16（渲染），再做本任务。

> 接口补充：`ingest::begin(root, cfg, store, input: &BeginInput) -> Result<BeginReport>`；`PageRef { id: String, path: String }`（`path` 是相对仓库根的页面路径，如 `wiki/concept/retry.md`）。

**Files:**
- Create: `src/ingest.rs`
- Modify: `src/lib.rs`, `src/cli.rs`
- Test: `tests/ingest.rs`

**Interfaces:**
- Consumes: `store::`, `staging::`, `page::`, `hash::`, `report::`
- Produces:
  - `ingest::BeginInput { sources: Vec<String>, pages: Vec<String>, dry_run: bool }`
  - `ingest::BeginReport { source_keys: Vec<String>, affected: Vec<PageRef>, new_pages: usize, orphan_pages: Vec<String>, pending_hash: String }`
  - `ingest::commit(root, cfg, store, pages: &[String]) -> Result<CommitReport>`
  - `ingest::abort(root, cfg, store) -> Result<AbortReport>`
  - `ingest::CommitReport { pages_written: Vec<String>, index_updated: bool }`

- [ ] **Step 1: 写失败的测试**

```rust
// tests/ingest.rs
use llm_wiki::{config::Config, init, ingest, store::Store};
use std::fs;
use tempfile::tempdir;

fn setup() -> (tempfile::TempDir, Config) {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    fs::write(d.path().join("raw/a.md"), "订单超时后重试三次。").unwrap();
    (d, cfg)
}

#[test]
fn commit_without_pages_is_a_usage_error() {
    let (d, cfg) = setup();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    let err = ingest::commit(d.path(), &cfg, &mut s, &[]).unwrap_err();
    assert!(err.to_string().contains("E_NO_PAGES"));
}

#[test]
fn begin_reports_affected_pages_from_full_scan() {
    let (d, cfg) = setup();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    fs::create_dir_all(d.path().join("wiki/concept")).unwrap();
    fs::write(d.path().join("wiki/concept/retry.md"),
        "---\nid: retry\ntitle: 重试\nsources:\n  - raw/a.md\nstatus: stable\nupdated: 2026-01-01\n---\n正文\n").unwrap();
    let r = ingest::begin(d.path(), &cfg, &mut s,
        &ingest::BeginInput { sources: vec!["raw/a.md".into()], pages: vec![], dry_run: false }).unwrap();
    assert!(r.affected.iter().any(|p| p.id == "retry"));
}

#[test]
fn abort_clears_pending_and_leaves_wiki_untouched() {
    let (d, cfg) = setup();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    ingest::begin(d.path(), &cfg, &mut s,
        &ingest::BeginInput { sources: vec!["raw/a.md".into()], pages: vec![], dry_run: false }).unwrap();
    ingest::abort(d.path(), &cfg, &mut s).unwrap();
    assert!(!d.path().join("wiki").read_dir().unwrap().any(|e| {
        e.unwrap().file_name().to_string_lossy().ends_with(".md")
    }));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test ingest`
Expected: FAIL。

- [ ] **Step 3: 实现三段式**

`begin` 的作用只有三件：全库反查 `page.sources` 找出受影响页、把「本次要动的源 + 快照哈希」写进 `pending` 表、返回报告。它**不改任何文件**。`dry_run` 时连 pending 都不写。

`commit` 按 `--pages` 显式列出的页面生成/更新 staging 副本，写回 `page.sources` 与 `updated`，调 `render::write_index`，把 `pending` 落成 `versions` 候选并清空 pending。页面的物理写入只发生在 staging，不动 `wiki/`。

`abort` 清 pending、删本轮 staging 增量，保留 staging 主体。

`begin` 里 `orphan_pages` 指「wiki 里有、但账本里 source 已不存在的页」，只报告不改。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test ingest`
Expected: PASS，3 passed。

- [ ] **Step 5: 提交**

```bash
git add src/ingest.rs src/lib.rs src/cli.rs tests/ingest.rs
git commit -m "feat: ingest begin/commit/abort with explicit page list"
```

---

### Task 16: 簿记渲染（index.md 与 log.md）

**Files:**
- Create: `src/render.rs`
- Modify: `src/lib.rs`
- Test: `tests/render.rs`

**Interfaces:**
- Consumes: `page::`, `store::`
- Produces:
  - `render::PageMeta { id, title, typ, summary, status }`
  - `render::render_index(pages: &[PageMeta], types: &[String]) -> String`
  - `render::LogEntry { date: NaiveDate, action: String, title: String, detail: Option<String> }`
  - `render::append_log(path: &Path, e: &LogEntry) -> Result<()>`
  - `render::write_index(root, cfg, store) -> Result<usize>`

- [ ] **Step 1: 写失败的测试**

```rust
// tests/render.rs
use chrono::NaiveDate;
use llm_wiki::render::{self, LogEntry, PageMeta};
use std::fs;
use tempfile::tempdir;

#[test]
fn index_groups_by_type_and_sorts_titles() {
    let pages = vec![
        PageMeta{ id:"b".into(), title:"乙".into(), typ:"concept".into(), summary:"s".into(), status:"stable".into() },
        PageMeta{ id:"a".into(), title:"甲".into(), typ:"concept".into(), summary:"s".into(), status:"stable".into() },
    ];
    let out = render::render_index(&pages, &["concept".to_string()]);
    let ia = out.find("甲").unwrap();
    let ib = out.find("乙").unwrap();
    assert!(ia < ib);
    assert!(out.contains("## concept"));
}

#[test]
fn append_log_preserves_existing_bytes() {
    let d = tempdir().unwrap();
    let p = d.path().join("log.md");
    let before = "# 日志\n\n## [2026-01-01] init | 初始化\n";
    fs::write(&p, before).unwrap();
    let e = LogEntry{ date: NaiveDate::from_ymd_opt(2026,10,27).unwrap(),
        action:"ingest".into(), title:"订单".into(), detail: None };
    render::append_log(&p, &e).unwrap();
    let after = fs::read_to_string(&p).unwrap();
    assert!(after.starts_with(before), "append-only，前缀必须原样");
    assert!(after.contains("## [2026-10-27] ingest | 订单"));
}

#[test]
fn log_title_with_pipe_does_not_break_parsing() {
    let e = LogEntry{ date: NaiveDate::from_ymd_opt(2026,10,27).unwrap(),
        action:"ingest".into(), title:"a | b".into(), detail: None };
    let line = e.to_line();
    assert_eq!(line.matches(" | ").count(), 1, "标题里的竖线要转义");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test render`
Expected: FAIL。

- [ ] **Step 3: 实现渲染**

index.md 结构固定：`# 索引` → 每个 type 一个 `## <type>` → 每个页面一行 `- [标题](wiki/<type>/<id>.md) — 摘要`。无页面的 type 不出现，`types` 里未使用的顺序保持 config 顺序。整份覆盖写，不追加。

log.md 只追加。行格式 `## [YYYY-MM-DD] <action> | <title>`，标题里的 ` | ` 替换成 `｜`（全角），`detail` 写成下一行的普通段落。写前读一次原文件，只在末尾追加，不做任何重排——保证 git diff 干净。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test render`
Expected: PASS，3 passed。

- [ ] **Step 5: 提交**

```bash
git add src/render.rs src/lib.rs tests/render.rs
git commit -m "feat: index and log rendering as cli bookkeeping"
```

---

### Task 17: lint 十二条与 `--fix`

**Files:**
- Create: `src/lint.rs`
- Modify: `src/lib.rs`, `src/cli.rs`
- Test: `tests/lint.rs`

**Interfaces:**
- Consumes: `page::`, `store::`, `config::`
- Produces:
  - `lint::Level { Error, Warning, Info }`（可比较、可排序）
  - `lint::Finding { rule: &'static str, level: Level, page: Option<String>, msg: String }`
  - `lint::lint(root, cfg, store) -> Result<Vec<Finding>>`（已按 level 降序、再按 rule 排序）
  - `lint::fix(root, cfg, store) -> Result<FixReport>`
  - `FixReport { fixed: usize, remaining: Vec<Finding> }`

- [ ] **Step 1: 写失败的测试**

```rust
// tests/lint.rs
use llm_wiki::{config::Config, init, lint, store::Store};
use std::fs;
use tempfile::tempdir;

fn page(dir: &std::path::Path, name: &str, body: &str) {
    fs::create_dir_all(dir.join("wiki/concept")).unwrap();
    fs::write(dir.join("wiki/concept").join(name), body).unwrap();
}

#[test]
fn l2_dead_link_is_an_error() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    page(d.path(), "a.md", "---\nid: a\ntitle: 甲\nstatus: stable\nupdated: 2026-01-01\n---\n见 [[不存在]]\n");
    let fs_ = lint::lint(d.path(), &cfg, &s).unwrap();
    assert!(fs_.iter().any(|f| f.rule == "L2" && f.level == lint::Level::Error));
}

#[test]
fn l12_unreferenced_asset_is_info() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    fs::create_dir_all(d.path().join("raw")).unwrap();
    fs::write(d.path().join("raw/logo.png.md"), "---\nsource: source/logo.png\nasset: true\n---\n![](../../source/logo.png)\n").unwrap();
    let fs_ = lint::lint(d.path(), &cfg, &s).unwrap();
    assert!(fs_.iter().any(|f| f.rule == "L12" && f.level == lint::Level::Info));
}

#[test]
fn strict_escalates_warnings() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    page(d.path(), "a.md", "---\nid: a\ntitle: 甲\nstatus: stable\n---\n");
    let fs_ = lint::lint(d.path(), &cfg, &s).unwrap();
    assert!(fs_.iter().any(|f| f.rule == "L5"));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test lint`
Expected: FAIL。

- [ ] **Step 3: 实现十二条**

L1 必填字段缺失（error）；L2 内部 `[[链接]]` 目标不存在（error）；L3 同一 id 出现在两个 type 目录（error）；L4 frontmatter 解析失败（error）；L5 `updated` 缺失或不是 ISO 日期（warning）；L6 `sources` 指向的 raw 不存在（error）；L7 `sources` 指向的 raw 未被 convert 记录（warning）；L8 页面正文字符数低于 `[audit].min_page_chars`（warning）；L9 `status` 之外的字段变化但 `updated` 未变（warning，`asset: true` 豁免）；L10 同一 raw 被 >3 个页面引用（info）；L11 `log.md` 行格式不合 `^## \[` 前缀（warning）；L12 未被任何页面引用的 asset raw（info）。

`--fix` 只修 L5（补 `updated` 为今天）与 L11 之外的机械项——具体是 L5 与「index.md 过期」；其余只报告。修完重新跑一遍 lint，返回 remaining。

`--strict` 把 warning 也算失败，info 不算。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test lint`
Expected: PASS，3 passed。

- [ ] **Step 5: 提交**

```bash
git add src/lint.rs src/lib.rs src/cli.rs tests/lint.rs
git commit -m "feat: lint rules L1-L12 with fix and strict mode"
```

---

### Task 18: 版本、决策门与 checkpoint

**Files:**
- Create: `src/version.rs`
- Modify: `src/lib.rs`, `src/cli.rs`
- Test: `tests/version.rs`

**Interfaces:**
- Consumes: `staging::`, `store::`, `report::Exit`
- Produces:
  - `version::checkpoint(root, cfg, store, label: &str) -> Result<String>`（返回生成的版本名）
  - `version::approve(root, cfg, store, confirm: bool) -> Result<ApproveOutcome>`
  - `version::ApproveOutcome { Applied { version: String }, NeedsConfirmation { reason: String, detail: String } }`
  - `version::reject(root, cfg, store, reason: &str) -> Result<()>`
  - `version::instruct(root, cfg, store, text: &str) -> Result<()>`
  - `version::defer(root, cfg, store) -> Result<()>`

- [ ] **Step 1: 写失败的测试**

```rust
// tests/version.rs
use llm_wiki::{config::Config, init, staging, store::Store, version};
use std::fs;
use tempfile::tempdir;

fn boot() -> (tempfile::TempDir, Config, Store) {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    staging::ensure(d.path(), &cfg).unwrap();
    (d, cfg, s)
}

#[test]
fn approve_without_confirm_asks_a_human() {
    let (d, cfg, mut s) = boot();
    let out = version::approve(d.path(), &cfg, &mut s, false).unwrap();
    assert!(matches!(out, version::ApproveOutcome::NeedsConfirmation { .. }));
}

#[test]
fn approve_with_confirm_merges_into_wiki() {
    let (d, cfg, mut s) = boot();
    staging::write_page(d.path(), &cfg, "concept/new", "---\nid: new\ntitle: 新\nstatus: draft\nupdated: 2026-10-27\n---\n正文\n").unwrap();
    let out = version::approve(d.path(), &cfg, &mut s, true).unwrap();
    assert!(matches!(out, version::ApproveOutcome::Applied { .. }));
    assert!(d.path().join("wiki/concept/new.md").exists());
}

#[test]
fn checkpoint_copies_current_wiki_and_prunes_old() {
    let (d, cfg, mut s) = boot();
    fs::write(d.path().join("wiki/log.md"), "x").unwrap();
    for i in 0..8 { version::checkpoint(d.path(), &cfg, &mut s, &format!("v{i}")).unwrap(); }
    let dirs: Vec<_> = fs::read_dir(d.path().join(".wiki/versions")).unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    assert_eq!(dirs.len(), 5, "versions.keep=5");
}

#[test]
fn defer_leaves_baseline_untouched() {
    let (d, cfg, mut s) = boot();
    fs::write(d.path().join("wiki/log.md"), "before").unwrap();
    version::defer(d.path(), &cfg, &mut s).unwrap();
    assert_eq!(fs::read_to_string(d.path().join("wiki/log.md")).unwrap(), "before");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test version`
Expected: FAIL。

- [ ] **Step 3: 实现决策门**

`approve` 前置检查：① staging 与 manifest 记录的哈希一致，否则 `E_STAGING_DRIFT`；② 依赖源当前哈希仍等于 staging 时记录的哈希，否则 `E_STAGING_STALE`（exit 1）。通过且 `confirm=false` 时返回 `NeedsConfirmation`，由 CLI 转成 exit 3 + JSON。`confirm=true` 时用 `rename` 把每个 staging 文件原子换到 `wiki/` 对应位置（同一文件系统内），写 `versions/<v>/` 完整副本，更新 baseline。

`reject` 清空 staging 增量并记录原因到 `log.md`；`instruct` 把用户原话写进 `.wiki/next-instruction.md` 供下一轮读；`defer` 什么都不动（staging 保留，下轮增量继续）。

`checkpoint` 复制 `wiki/` 到 `.wiki/versions/<label>-<seq>/`，按 `[versions].keep` 删最旧的，只删本工具建的目录。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test version`
Expected: PASS，4 passed。

- [ ] **Step 5: 提交**

```bash
git add src/version.rs src/lib.rs src/cli.rs tests/version.rs
git commit -m "feat: version gate with checkpoint and four-way decision"
```

---

### Task 19: sync 编排

**Files:**
- Create: `src/lock.rs`, `src/sync.rs`
- Modify: `src/lib.rs`, `src/cli.rs`
- Test: `tests/sync.rs`

**Interfaces:**
- Consumes: `scan::`, `convert::`, `ingest::`, `render::`, `store::`
- Produces:
  - `lock::acquire(root) -> Result<Lock>`（`Drop` 时释放；`E_LOCKED` 表示被别人持有）
  - `lock::is_stale(root, ttl_secs) -> bool`
  - `sync::SyncArgs { limit: usize, dry_run: bool, force: bool }`
  - `sync::SyncReport { run_id, changes: Vec<scan::Change>, converted: usize, ingested: Vec<String>, queued: usize, warnings: Vec<String>, failures: usize }`
  - `sync::run(root, cfg, store, args) -> Result<SyncReport>`

- [ ] **Step 1: 写失败的测试**

```rust
// tests/sync.rs
use llm_wiki::{config::Config, init, lock, store::Store, sync};
use std::fs;
use tempfile::tempdir;

#[test]
fn second_lock_is_exit_4() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let _h = lock::acquire(d.path()).unwrap();
    let e = lock::acquire(d.path()).unwrap_err();
    assert!(e.to_string().contains("E_LOCKED"));
}

#[test]
fn fresh_files_are_skipped_by_settle_window() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let mut cfg = Config::load(d.path()).unwrap();
    cfg.sync.settle_seconds = 3600;
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    fs::create_dir_all(d.path().join("source")).unwrap();
    fs::write(d.path().join("source/a.md"), "# hi").unwrap();
    let r = sync::run(d.path(), &cfg, &mut s, &sync::SyncArgs{ limit: 10, dry_run: false, force: false }).unwrap();
    assert_eq!(r.converted, 0, "刚写入的文件必须跳过");
    assert!(r.warnings.iter().any(|w| w.contains("settle")));
}

#[test]
fn stable_streak_gates_auto_ingest() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let mut cfg = Config::load(d.path()).unwrap();
    cfg.sync.settle_seconds = 0;
    cfg.sync.auto_ingest = true;
    cfg.sync.stable_streak = 3;
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    fs::create_dir_all(d.path().join("source")).unwrap();
    fs::write(d.path().join("source/a.md"), "# hi").unwrap();
    let mut ingested_at = None;
    for i in 0..3 {
        let r = sync::run(d.path(), &cfg, &mut s, &sync::SyncArgs{ limit: 10, dry_run: false, force: false }).unwrap();
        if !r.ingested.is_empty() { ingested_at = Some(i); break; }
    }
    assert_eq!(ingested_at, Some(2), "第 3 轮才允许自动摄取");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test sync`
Expected: FAIL。

- [ ] **Step 3: 实现编排**

顺序：`lock::acquire` → `scan_source` → 过滤 `settle_seconds` 内变动的 → `convert`（`--limit` 限流）→ `scan_raw` → 稳定计数（`sync_state` 表存每个源的连续一致轮数，达到 `stable_streak` 才允许自动 ingest）→ `auto_ingest` 为真时调 `ingest::begin/commit` → `render::write_index` → 写 `wiki/audit/<日期>-sync.md` → 释放锁。

失败计数存 `sync_state`；连续 3 轮失败把 `auto_ingest` 置 false 并写进报告（不写回 wiki.toml，只在 state 里记，避免 CLI 改配置）。

`--dry-run` 只到报告，不写任何文件。`--force` 忽略 settle 与 stable_streak。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test sync`
Expected: PASS，3 passed。

- [ ] **Step 5: 提交**

```bash
git add src/lock.rs src/sync.rs src/lib.rs src/cli.rs tests/sync.rs
git commit -m "feat: sync orchestration with lock, settle window and stability gate"
```

---

### Task 20: query 与 FTS5 索引

**Files:**
- Create: `src/query.rs`, `src/graph.rs`
- Modify: `src/store.rs`（migration v3）、`src/lib.rs`、`src/cli.rs`
- Test: `tests/query.rs`

**Interfaces:**
- Consumes: `store::`, `page::`
- Produces:
  - `query::segment(s: &str) -> String`（CJK 逐字加空格，非 CJK 原样）
  - `query::reindex(root, cfg, store) -> Result<usize>`
  - `query::Hit { page, title, typ, snippet, score, status, updated, links: Vec<LinkRef>, backlinks: usize, sources: Vec<SourceRef> }`
  - `query::SearchOpts { limit: usize, expand: usize, typ: Option<String> }`
  - `query::search(root, cfg, store, q: &str, o: &SearchOpts) -> Result<Vec<Hit>>`
  - `query::LinkRef { to: String, title: String, exists: bool }`
  - `query::SourceRef { raw: String, source: String, chunk: Option<String>, raw_available: bool }`
  - `graph::backlinks(store: &Store) -> Result<BTreeMap<String, usize>>`（目标 id → 指向它的页面数）

- [ ] **Step 1: 写失败的测试**

```rust
// tests/query.rs
use llm_wiki::{config::Config, init, query, store::Store};
use std::fs;
use tempfile::tempdir;

#[test]
fn cjk_is_segmented_per_char() {
    assert_eq!(query::segment("订单超时"), "订 单 超 时");
    assert_eq!(query::segment("PDF解析ok"), "PDF 解 析 ok");
}

#[test]
fn chinese_two_char_query_hits() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    fs::create_dir_all(d.path().join("wiki/concept")).unwrap();
    fs::write(d.path().join("wiki/concept/a.md"),
        "---\nid: a\ntitle: 订单超时\nstatus: stable\nupdated: 2026-01-01\n---\n订单超时后重试三次。\n").unwrap();
    query::reindex(d.path(), &cfg, &mut s).unwrap();
    let hits = query::search(d.path(), &cfg, &s, "超时", &query::SearchOpts{ limit: 5, expand: 0, typ: None }).unwrap();
    assert_eq!(hits[0].page, "wiki/concept/a.md");
}

#[test]
fn hit_carries_sources_and_links() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    fs::create_dir_all(d.path().join("wiki/concept")).unwrap();
    fs::write(d.path().join("wiki/concept/a.md"),
        "---\nid: a\ntitle: 甲\nsources:\n  - raw/a.md\nstatus: stable\nupdated: 2026-01-01\n---\n见 [[b]] 丙\n").unwrap();
    query::reindex(d.path(), &cfg, &mut s).unwrap();
    let h = &query::search(d.path(), &cfg, &s, "丙", &query::SearchOpts{ limit: 5, expand: 0, typ: None }).unwrap()[0];
    assert_eq!(h.sources[0].raw, "raw/a.md");
    assert_eq!(h.links[0].to, "b");
    assert!(!h.sources[0].raw_available);
}

#[test]
fn reindex_is_idempotent() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    let a = query::reindex(d.path(), &cfg, &mut s).unwrap();
    let b = query::reindex(d.path(), &cfg, &mut s).unwrap();
    assert_eq!(a, b);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test query`
Expected: FAIL。

- [ ] **Step 3: 实现索引与检索**

migration v3 只加 `page_meta` 表（存 type/status/updated/sources/links 供拼 Hit）；`fts` 虚表在 v1 已建好，直接复用，不再重复建。`reindex` 全量重建：清空 fts 与 page_meta → 遍历 `wiki/` → 正文切分后写 fts，元数据写 page_meta → 用 `graph::backlinks` 回填扇入度。返回页面数。

只索引 `wiki/`，`raw/` 不进——这是决策 28。查询时 `MATCH` 加 `bm25()` 排序，`status=stable` 与近期 `updated` 做轻微加权（乘 1.1 / 1.05，不让它盖过相关性）。snippet 用 `snippet(fts, 0, '**', '**', '…', 32)`。

Hit 的 `sources` 每项 `{ raw, source, chunk, raw_available }`，路径从 `page_meta.sources` 拼出；`raw_available` = 该 raw 文件当前存在。`links` 从正文正则 `\[\[([^\]]+)\]\]` 抽，查 page_meta 补 title；不存在的目标也返回，带 `exists: false`。

`--expand 1` 对每个 hit 追加邻页 summary，最多 5 个，超出截断并记 warning。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test query`
Expected: PASS，4 passed。

- [ ] **Step 5: 提交**

```bash
git add src/query.rs src/store.rs src/cli.rs tests/query.rs
git commit -m "feat: fts5 query over wiki with linked next steps"
```

---

### Task 21: doctor、e2e 夹具与 CI

**Files:**
- Create: `src/doctor.rs`, `tests/e2e.rs`, `tests/fixtures/`（8 个难看样本）, `.github/workflows/ci.yml`
- Modify: `src/lib.rs`, `src/cli.rs`
- Test: `tests/e2e.rs`

**Interfaces:**
- Consumes: 全部
- Produces:
  - `doctor::check(root, cfg) -> Result<Vec<doctor::Check>>`，`Check { name, ok: bool, detail: String }`
  - `doctor::run(...) -> Exit`

- [ ] **Step 1: 写失败的测试**

```rust
// tests/e2e.rs
use std::process::Command;

fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_llm-wiki")) }

fn copy_dir_all(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.path().is_dir() { copy_dir_all(&e.path(), &dst); }
        else { std::fs::copy(e.path(), &dst).unwrap(); }
    }
}

#[test]
fn full_pipeline_on_messy_fixtures() {
    let d = tempfile::tempdir().unwrap();
    let r = bin().args(["init", "--path"]).arg(d.path()).arg("--json").output().unwrap();
    assert!(r.status.success());
    // 8 个样本：中文名、emoji、空文件、超大、扫描 PDF、重复、深目录、无扩展名
    let fixtures = std::path::Path::new("tests/fixtures");
    copy_dir_all(fixtures, &d.path().join("source"));
    let r = bin().args(["convert", "--until-done", "--limit", "100"])
        .current_dir(d.path()).output().unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let r = bin().args(["scan", "--json"]).current_dir(d.path()).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&r.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert!(v["data"].is_object());
    let r = bin().args(["lint", "--strict"]).current_dir(d.path()).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&r.stdout).unwrap();
    assert!(v["data"]["findings"].is_array());
}

#[test]
fn doctor_reports_fts5_available() {
    let d = tempfile::tempdir().unwrap();
    bin().args(["init", "--path"]).arg(d.path()).output().unwrap();
    let r = bin().args(["doctor", "--json"]).current_dir(d.path()).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&r.stdout).unwrap();
    assert!(v["data"].as_array().unwrap().iter().any(|c| c["name"] == "fts5" && c["ok"] == true));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test e2e`
Expected: FAIL，doctor 未实现。

- [ ] **Step 3: 实现 doctor 与夹具**

doctor 检查项：`config`（能加载）、`fts5`（`SELECT sqlite_version()` + 建临时虚表）、`parsers`（backend 注册表非空）、`paths`（source/raw/wiki/.wiki 可写）、`token-in-config`（扫描 wiki.toml 是否含 `sk-` 或裸 Bearer）、`stale-lock`（`.wiki/lock` 存在但超时）、`versions-keep`（versions 目录数超限时提示）。每项配一句人话 `detail`。

夹具 8 个，全部进 `tests/fixtures/`：`中文 名称.md`、`emoji😀.md`、`empty.md`（0 字节）、`big.txt`（脚本生成 3MB，不进 git，测试里生成）、`scan.pdf`（无文本层）、`a.md` 与 `b.md` 内容完全相同（重复）、`deep/1/2/3/4/5/x.md`（深目录）、`noext`（无扩展名）。

CI：`.github/workflows/ci.yml`，matrix `macos-latest / ubuntu-latest / windows-latest`，steps：`cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test`。测试内不联网（外部解析全走本地 mock）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test`
Expected: 全部 PASS。

- [ ] **Step 5: 提交**

```bash
git add src/doctor.rs tests/ tests/fixtures .github/workflows/ci.yml
git commit -m "feat: doctor checks, e2e fixtures and cross-platform ci"
```

---

## Execution Handoff

计划到此完成，共 21 个 Task。执行时有两条路：

1. **Subagent-Driven**（推荐）：每个 Task 派一个新 subagent，Task 之间做代码评审，反馈快、上下文干净。
2. **Inline Execution**：在当前会话里按批次执行，带检查点供人工复核。

选定后从 Task 1 开始。
