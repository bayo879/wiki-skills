# LLM Wiki Skills Package Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付 6 个 Claude Code skill（1 总纲 + 5 能力），把 `llm-wiki` CLI 包成 agent 能直接用的操作纪律。

**Architecture:** skill 只做三件事：判断该调哪个 CLI 子命令、把 `--json` 输出翻译成人话、在需要人判断的地方停下来问。所有簿记、哈希、索引、锁都在 CLI 里，skill 不重复实现，也不写死路径。

**Tech Stack:** Markdown + YAML frontmatter；校验用 Rust 集成测试（复用 CLI 仓库的 cargo test，三平台可跑）；安装用 POSIX sh 与 PowerShell 各一份。

**Spec:** `docs/superpowers/specs/2026-10-27-llm-wiki-skills-design.md`

## Global Constraints

- Claude Code 硬约束：frontmatter 只有 `name`（≤64 字符，`^[a-z0-9-]+$`，等于目录名）与 `description`（≤1024 字符，第三人称、说清"何时用"）。
- 每个 SKILL.md 正文 < 500 行；不得通过相对路径引用 skill 包外或更深层的文件。
- skill 里出现的每个 `llm-wiki <sub>` 必须是 CLI 真实存在的子命令，由测试对着白名单校验。
- 不复制 CLI 能算出来的东西：不写哈希、不写日期、不手写 index/log。
- 不写死绝对路径；仓库根用 `llm-wiki doctor --json` 返回的 `data.root`。
- 中文正文；命令与 JSON 字段名保持英文。

## Skill 边界（互斥，不得重叠）

| skill | 拥有 | 明确不碰 |
|---|---|---|
| `llm-wiki` | 初始化、路由、四选项话术 | 任何子命令的执行细节 |
| `wiki-scan` | `scan` / `convert` 的调用与变更解释 | 写页面、改配置 |
| `wiki-parse` | `parse` 调试与后端选择 | 批量转换、摄取 |
| `wiki-ingest` | `ingest begin/commit/abort` 与正文写作 | 扫描、lint |
| `wiki-audit` | `lint` 与判断层分析 | 机械修复（交 `--fix`） |
| `wiki-sync` | `sync` 编排与失败退避 | 初始化、单源交互摄取 |

---

## File Structure

```
wiki-skills/
├── skills/
│   ├── llm-wiki/SKILL.md
│   ├── wiki-scan/SKILL.md
│   ├── wiki-parse/SKILL.md
│   ├── wiki-ingest/SKILL.md
│   ├── wiki-audit/SKILL.md
│   └── wiki-sync/SKILL.md
├── scripts/
│   ├── install.sh
│   └── install.ps1
└── tests/
    └── skills_pack.rs
```

安装目标（二选一，由脚本参数决定）：`~/.claude/skills/`（全局）或 `<repo>/.claude/skills/`（项目内）。

---

### Task 1: 校验器与 skill 骨架

**Files:**
- Create: `tests/skills_pack.rs`, `skills/{llm-wiki,wiki-scan,wiki-parse,wiki-ingest,wiki-audit,wiki-sync}/SKILL.md`

**Interfaces:**
- Consumes: 无
- Produces:
  - `skills_pack.rs` 里的 `parse_skill(text) -> (BTreeMap<String,String>, String)`（返回 frontmatter 与正文）
  - 常量 `VALID_SUBCOMMANDS: &[&str]`，列出 CLI 全部子命令

- [ ] **Step 1: 写失败的测试**

```rust
// tests/skills_pack.rs
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const VALID_SUBCOMMANDS: &[&str] = &[
    "init", "scan", "convert", "parse", "ingest", "lint", "checkpoint",
    "approve", "reject", "instruct", "defer", "sync", "query", "doctor",
    "graph",
];

fn parse_frontmatter(t: &str) -> (BTreeMap<String, String>, &str) {
    let rest = t.strip_prefix("---\n").expect("必须以 --- 开头");
    let end = rest.find("\n---\n").expect("frontmatter 必须闭合");
    let mut m = BTreeMap::new();
    for line in rest[..end].lines() {
        if let Some((k, v)) = line.split_once(':') {
            m.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    (m, &rest[end + 5..])
}

fn skills() -> Vec<(String, String)> {
    fs::read_dir("skills").unwrap()
        .filter_map(|e| e.ok())
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            (name.clone(), fs::read_to_string(e.path().join("SKILL.md")).unwrap_or_default())
        })
        .collect()
}

#[test]
fn every_skill_has_valid_frontmatter() {
    let all = skills();
    assert_eq!(all.len(), 6, "必须有 6 个 skill");
    for (dir, text) in all {
        let (fm, body) = parse_frontmatter(&text);
        assert_eq!(fm.len(), 2, "{dir} 只能有 name 与 description");
        let name = fm.get("name").unwrap_or_else(|| panic!("{dir} 缺 name"));
        assert_eq!(name, &dir, "{dir} 的 name 必须等于目录名");
        assert!(name.len() <= 64 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{dir} 的 name 不合规");
        let d = fm.get("description").unwrap();
        assert!(!d.is_empty() && d.chars().count() <= 1024, "{dir} 的 description 超限");
        assert!(body.lines().count() < 500, "{dir} 正文超过 500 行");
    }
}

#[test]
fn no_skill_mentions_an_unknown_subcommand() {
    for (dir, text) in skills() {
        for word in text.split_whitespace() {
            if let Some(sub) = word.strip_prefix("`llm-wiki ") {
                let sub = sub.trim_end_matches(['`', ',', '.']).split_whitespace().next().unwrap_or("");
                if !sub.is_empty() {
                    assert!(VALID_SUBCOMMANDS.contains(&sub), "{dir} 提到未知子命令 {sub}");
                }
            }
        }
    }
}

#[test]
fn no_deep_relative_references() {
    for (dir, text) in skills() {
        assert!(!text.contains("](../"), "{dir} 只能一层引用");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test skills_pack`
Expected: FAIL，`skills` 目录不存在或数量为 0。

- [ ] **Step 3: 建 6 个空骨架**

每个目录一个 `SKILL.md`，内容只有 frontmatter 加一行占位正文。`name` 等于目录名，`description` 先写「占位」，后续任务逐个替换。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test skills_pack`
Expected: PASS（`every_skill_has_valid_frontmatter`、`no_deep_relative_references` 通过；`no_skill_mentions_an_unknown_subcommand` 因为没有命令而空转通过）。

- [ ] **Step 5: 提交**

```bash
git add tests/skills_pack.rs skills/
git commit -m "test: skill package validator and skeleton"
```

---

### Task 2: `llm-wiki` 总纲

**Files:**
- Modify: `skills/llm-wiki/SKILL.md`
- Test: `tests/skills_pack.rs`（追加断言）

**Interfaces:**
- Consumes: 无
- Produces: 路由规则表（用户意图 → 子 skill）、四选项话术模板

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn llm_wiki_is_the_router() {
    let t = fs::read_to_string("skills/llm-wiki/SKILL.md").unwrap();
    for kw in ["wiki-scan", "wiki-parse", "wiki-ingest", "wiki-audit", "wiki-sync"] {
        assert!(t.contains(kw), "总纲必须路由到 {kw}");
    }
    for opt in ["批准", "不批准", "自行输入", "暂不处理"] {
        assert!(t.contains(opt), "总纲要给出四选项：{opt}");
    }
    assert!(t.contains("llm-wiki doctor"), "总纲要先确认仓库状态");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test skills_pack llm_wiki_is_the_router`
Expected: FAIL。

- [ ] **Step 3: 写正文**

固定四节：**何时用**（用户提到知识库、摄取、体检、同步）／**先确认状态**（`llm-wiki doctor --json`，`ok=false` 时先 `init` 或报告缺失项）／**路由表**（意图 → skill，一行一条，不展开细节）／**四选项话术**（原样可复制的四个编号选项：1 批准并合并 2 不批准并说明原因 3 我直接说怎么改 4 暂不处理）。

正文明确写：所有 `index.md`、`log.md`、`updated` 由 CLI 生成，任何 skill 都不得手写。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test skills_pack`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add skills/llm-wiki/SKILL.md tests/skills_pack.rs
git commit -m "feat: llm-wiki router skill"
```

---

### Task 3: `wiki-scan`

**Files:**
- Modify: `skills/wiki-scan/SKILL.md`
- Test: `tests/skills_pack.rs`（追加断言）

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn wiki_scan_covers_two_layer_change_semantics() {
    let t = fs::read_to_string("skills/wiki-scan/SKILL.md").unwrap();
    for kw in ["llm-wiki scan", "llm-wiki convert", "added", "modified", "moved",
               "removed", "duplicate", "source/", "raw/"] {
        assert!(t.contains(kw), "wiki-scan 必须覆盖 {kw}");
    }
    assert!(t.contains("--limit"), "必须提到分段推进");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test skills_pack wiki_scan_covers_two_layer_change_semantics`
Expected: FAIL。

- [ ] **Step 3: 写正文**

四节：**何时用**（"同步过来一批新文件""扫一下有什么变化"）／**两段的意义**（`source/` 是原始事实、`raw/` 是转换产物，判据是 `raw/` 内容变没变，所以只改文件元数据不该产生变更）／**怎么跑**（先 `scan` 看 A 段变更，再 `convert --limit` 分段，最后 `scan` 看 B 段）／**五类变更怎么讲给用户**（每类一句话，`moved` 要强调"不重新摄取、只迁引用"）。

明确写：不主动调 `convert` 处理超过 `[parse].chunk_threshold` 的文件而不告知用户；`oversize` 的源要单独列出来。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test skills_pack`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add skills/wiki-scan/SKILL.md tests/skills_pack.rs
git commit -m "feat: wiki-scan skill"
```

---

### Task 4: `wiki-parse`

**Files:**
- Modify: `skills/wiki-parse/SKILL.md`
- Test: `tests/skills_pack.rs`（追加断言）

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn wiki_parse_explains_backend_choice() {
    let t = fs::read_to_string("skills/wiki-parse/SKILL.md").unwrap();
    for kw in ["llm-wiki parse", "builtin-text", "builtin-html", "builtin-pdf",
               "builtin-asset", "external-http", "no_text_layer", "auth_env"] {
        assert!(t.contains(kw), "wiki-parse 必须覆盖 {kw}");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test skills_pack wiki_parse_explains_backend_choice`
Expected: FAIL。

- [ ] **Step 3: 写正文**

四节：**何时用**（单份文件解析结果不对、链接断、扫描件）／**后端表**（五个后端各一行，说明适用格式与产出）／**怎么定位**（`parse --json` 看 `backend` 与 `warnings`；`no_text_layer` 表示降级成链接，不是失败）／**何时改配置**（外部服务只改 `auth_env` 环境变量名，不写 token；`primary` 三档含义）。

明确写：不 OCR；图片只产链接，判成 asset 是预期行为。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test skills_pack`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add skills/wiki-parse/SKILL.md tests/skills_pack.rs
git commit -m "feat: wiki-parse skill"
```

---

### Task 5: `wiki-ingest`

**Files:**
- Modify: `skills/wiki-ingest/SKILL.md`
- Test: `tests/skills_pack.rs`（追加断言）

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn wiki_ingest_is_staged_and_human_gated() {
    let t = fs::read_to_string("skills/wiki-ingest/SKILL.md").unwrap();
    for kw in ["llm-wiki ingest begin", "llm-wiki ingest commit", "llm-wiki ingest abort",
               "--pages", "staging", "checkpoint", "引用结论前必须读正文"] {
        assert!(t.contains(kw), "wiki-ingest 必须覆盖 {kw}");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test skills_pack wiki_ingest_is_staged_and_human_gated`
Expected: FAIL。

- [ ] **Step 3: 写正文**

五节：**何时用**（"把这些文件整理进 wiki"）／**先汇报再落笔**（`begin` 拿受影响页 → 用中文列要点 → 得到确认才写）／**写哪里**（只写 staging，不碰 `wiki/`；正文自己写，frontmatter 的 `updated`/`sources` 交给 commit）／**commit 纪律**（`--pages` 必须显式列，全库反查是权威；写前逐页核对引用的事实能在 raw 里找到）／**收尾**（commit 后跑一次 `lint`，error 级必须当场修）。

明确写：引用结论前必须读正文，不得只看 summary 就下结论。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test skills_pack`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add skills/wiki-ingest/SKILL.md tests/skills_pack.rs
git commit -m "feat: wiki-ingest skill"
```

---

### Task 6: `wiki-audit`

**Files:**
- Modify: `skills/wiki-audit/SKILL.md`
- Test: `tests/skills_pack.rs`（追加断言）

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn wiki_audit_separates_mechanical_and_judgment() {
    let t = fs::read_to_string("skills/wiki-audit/SKILL.md").unwrap();
    for kw in ["llm-wiki lint", "--strict", "--fix", "L1", "L12", "error", "warning",
               "矛盾", "过时"] {
        assert!(t.contains(kw), "wiki-audit 必须覆盖 {kw}");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test skills_pack wiki_audit_separates_mechanical_and_judgment`
Expected: FAIL。

- [ ] **Step 3: 写正文**

四节：**何时用**（"体检""有没有说法不一致的地方"）／**机械层**（`lint --json`，按 error/warning/info 三档汇报；`--fix` 只修 L5 与索引过期）／**判断层**（读页面正文找矛盾与过时，一条结论必须挂两个页面路径，不猜）／**出报告**（写 `wiki/audit/<日期>-<主题>.md` 并进 git）。

明确写：不自动改页面，发现矛盾只报告并给出建议改动。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test skills_pack`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add skills/wiki-audit/SKILL.md tests/skills_pack.rs
git commit -m "feat: wiki-audit skill"
```

---

### Task 7: `wiki-sync`

**Files:**
- Modify: `skills/wiki-sync/SKILL.md`
- Test: `tests/skills_pack.rs`（追加断言）

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn wiki_sync_documents_unattended_rules() {
    let t = fs::read_to_string("skills/wiki-sync/SKILL.md").unwrap();
    for kw in ["llm-wiki sync", "exit 4", "settle_seconds", "stable_streak",
               "auto_ingest", "四选项", "wiki/audit/"] {
        assert!(t.contains(kw), "wiki-sync 必须覆盖 {kw}");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test skills_pack wiki_sync_documents_unattended_rules`
Expected: FAIL。

- [ ] **Step 3: 写正文**

五节：**何时用**（定时任务唤醒、"上游又同步了一批"）／**无人值守三规矩**（不删页不合并页；自动写入的页一律 `status: draft` 等人工审；每轮读 `wiki/audit/<日期>-sync.md`）／**失败怎么退**（`exit 3` → 用四选项问人；`exit 4` → 另一个进程在跑，本轮直接放弃并说明；连续失败 3 轮自动关 `auto_ingest`）／**调度建议**（高频小批量，`--limit` 控制单轮）／**别做的事**（不手改 `wiki.toml`，不改 `stable_streak`，不跳过 settle 窗口）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test skills_pack`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add skills/wiki-sync/SKILL.md tests/skills_pack.rs
git commit -m "feat: wiki-sync skill"
```

---

### Task 8: 安装脚本与 README

**Files:**
- Create: `scripts/install.sh`, `scripts/install.ps1`, `README.md`
- Modify: `.github/workflows/ci.yml`（加 `cargo test --test skills_pack`）

**Interfaces:**
- Consumes: `skills/`、`target/release/llm-wiki`
- Produces: `install.sh [--project <path>] [--prefix <dir>]`；等价 PowerShell

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn installer_mentions_both_targets() {
    let sh = fs::read_to_string("scripts/install.sh").unwrap();
    assert!(sh.contains(".claude/skills"));
    assert!(sh.contains("--project"));
    let ps = fs::read_to_string("scripts/install.ps1").unwrap();
    assert!(ps.contains(".claude\\skills"));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test skills_pack installer_mentions_both_targets`
Expected: FAIL。

- [ ] **Step 3: 写脚本与 README**

`install.sh`：`set -eu`；找二进制（`target/release/llm-wiki` 或 `$PREFIX/bin`）；`--project P` 时装到 `P/.claude/skills`，否则 `$HOME/.claude/skills`；逐个 `cp -R skills/<name> <dest>/`；已存在时提示并用 `--force` 覆盖；结束打印 `llm-wiki doctor` 的下一步。

`install.ps1`：同样的语义，用 `Copy-Item -Recurse -Force`，路径用 `Join-Path`。

README 四节按仓库文档规范：服务说明（这是什么、三个目录的关系）、部署（cargo build + install 脚本）、操作示例（`init` → 放文件进 `source/` → `convert` → 在 Claude Code 里说"整理进 wiki"）、特殊说明（配置段、环境变量 `WIKI_PARSE_TOKEN`、退出码表）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --test skills_pack && cargo test`
Expected: 全部 PASS。

- [ ] **Step 5: 提交**

```bash
git add scripts/ README.md .github/workflows/ci.yml tests/skills_pack.rs
git commit -m "feat: installers, readme and skills pack in ci"
```

---

## Execution Handoff

两份计划：CLI（21 个 Task）与 skills（8 个 Task）。CLI 先行——skill 的测试依赖真实的子命令清单。

执行方式二选一：Subagent-Driven（每 Task 一个新 subagent + 评审）或 Inline（当前会话按批次推进）。
