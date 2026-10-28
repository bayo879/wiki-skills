---
name: wiki-parse
description: 某份源文件的解析结果不对、转换时报错、正文为空或乱码，或想确认某扩展名由哪个内置后端处理、自动回退是否换了结果、要不要接入外部解析服务时使用。说明单文件试解析与按块查看的用法、大文件按块取样而不整份读入、五个内置后端与三档后端选择、外部服务的令牌只从环境变量读取且不得写进配置、警告与错误码怎么读、扫描件与图片只产链接不做文字识别、失败时如何区分后端与配置问题，以及确认解析正常后如何交回批量转换。不负责扫描、批量转换与摄取。
---

# wiki-parse

命令是 `llm-wiki`；不在 PATH 上时，用仓库根的 `.wiki/llm-wiki`。

`llm-wiki parse` 是 `convert` 的只读探针：同一解析层、同一后端、同一批警告，区别只是不写 `raw/`。用来回答「这份源为什么产出不对」。

不碰：批量转换（`wiki-scan`）、摄取与正文写作（`wiki-ingest`）、扫描。

## 何时用

- 某份文件 convert 后正文为空、乱码、链接断。
- convert 报解析错误，要分清是后端、配置还是源的问题。
- 想确认某个扩展名会被哪个后端认领、`auto` 是否换了结果。
- 想评估要不要接外部解析服务。

## 先确认仓库

```bash
llm-wiki doctor --json
```

`data.checks` 里优先看 `token`（`wiki.toml` 是否写了凭据）与 `external_backend`（外部服务是否可达、`primary` 与配置是否对得上）。有 `fail` 项（退出码 1）就先修，别在坏仓库上解读解析结果。`--key` 是相对 `source/` 的路径（`/` 分隔），可从 `llm-wiki scan --json` 的清单里抄。

## 看一份源

```bash
llm-wiki parse --key <key> --json
```

看 `data.mode`：

- `full`：小文件。字段 `backend`、`chars`、`warnings`、`markdown_head`。`markdown_head` 只有前 200 字符；要看正文加 `--chunk 1`（不超过 `chunk_target` 的文件一次拿全）。
- `chunked`：超过 `parse.chunk_threshold`（默认 2MB）的大文件。字段 `chunks`、`chunks_dir`（分块物化在 `.wiki/cache/<hash>/`），块大小取 `parse.chunk_target`（默认 64KB）。

大文件按块取，不要整份塞进上下文：

```bash
llm-wiki parse --key <key> --chunk 3 --json      # 第 3 块
llm-wiki parse --key <key> --range 1-3 --json    # 第 1 到第 3 块
```

编号 1-based，`--range` 是闭区间、A ≤ B，两个参数互斥。越界或写法错是 `E_BAD_ARGS`。抽样 2–3 块足够判断，别一口气取几十块。

`--json` 不写 `raw/`、不做转换；但大文件和 `--chunk`/`--range` 会把分块写到 `.wiki/cache/<hash>/`，可随时删。

## 五个内置后端

| id | 认什么 | 特点 |
|---|---|---|
| `builtin-text` | `md` `markdown` `txt` | 去 BOM、统一换行、去源 frontmatter，基本直通 |
| `builtin-html` | `html` `htm` | 抽正文，标签转 markdown，脚本与样式丢弃 |
| `builtin-pdf` | `pdf` | 有文本层才抽；无文本层降级成指向 `../source/<key>` 的图片链接 |
| `builtin-asset` | `png` `jpg` `jpeg` `gif` `webp` `svg` | 不读内容，只产链接；不做 OCR |
| `external-http` | `md` `markdown` `txt` `html` `htm` `pdf` | 配了 `[parse.external]` 才存在，POST 给外部服务取 markdown |

图片只产链接；raw frontmatter 里 `asset: true` 是预期行为，不是解析失败。

## `primary` 三档

- `auto`（默认）：先内置。装了外部且扩展是 `html`/`htm`/`pdf` 时，外部只是第二候选：内置报错，或结果不达标（`markdown` 字符数 < `min_chars`，默认 200，或无文本层）才发请求。外部失败：内置成功就保留内置并加 `external_failed`；内置也失败才是 `E_BOTH_FAILED`，消息里两个原因都要转述给人。`md`/`txt` 永不外部。
- `builtin`：只用内置，忽略外部配置。
- `external`：文本类强制外部；缺 `[parse.external]` 报 `E_BAD_CONFIG`，图片仍走 `builtin-asset`，其它扩展 `E_NO_PARSER`。

改模式只改 `wiki.toml` 的 `[parse].primary`，改完重跑 `parse`。

## 外部服务怎么配

```toml
[parse.external]
type      = "http"
url       = "http://parser.example/api/parse"
auth_env  = "WIKI_PARSE_TOKEN"          # 变量名，不是 token
fields    = { file = "@file" }
response  = { text = "data", ok_code = 200 }
timeout_s = 120
```

- token 只从环境变量读：`auth_env` 写变量名，运行时读 `WIKI_PARSE_TOKEN`。
- `auth_env` 里写出 token 字面量（`Bearer xxx`、`sk-`/`ghp_`/`eyJ` 开头、超过 40 位且无空白）会被拒绝：`E_TOKEN_IN_CONFIG`。
- 变量没设：`E_NO_AUTH_ENV`，消息只带变量名。
- 别把 token 打印进日志、写进任何文件或提交进仓库；也不要为绕校验改校验。
- 服务要返回字符串，按 `response.text` 点分路径取；非 `ok_code`、超时、非 JSON → `E_EXTERNAL`。
- `@file` 字段承载源文件，必须恰好有一个；`fields` 按字段名字典序发送。

`auto` 下只要写了 `[parse.external]`，`html`/`pdf` 在跑内置前就会校验整节；配坏（如 `url` 为空）直接 `E_BAD_CONFIG`，即使内置本可成功。要么修好，要么删掉这节或改 `primary = "builtin"`。

## 警告怎么读

| 码 | 读法 |
|---|---|
| `no_text_layer` | PDF 无文本层，已降级成链接。不是失败，raw 没坏；要正文只能走外部服务或人工 |
| `page_split_unknown` | 多页 PDF 被抽成单段，属 info，分页结构丢了但正文可用 |
| `bad_utf8` | 源不是 UTF-8，非法字节被替换成 U+FFFD；先去查源编码 |
| `below_min_chars` | 内置产出低于 `min_chars`，`auto` 会据此回退外部；看 `chars` 判断是否真的短 |
| `empty` | 正文为空（空图、空文档）；链接或文件仍需保留 |
| `external_used` | 本次结果来自外部；`message` 写明回退原因 |
| `external_failed` | 外部失败，落的是内置结果；原因在 `message` |

没看到 `below_min_chars` 时，看 `external_used` / `external_failed` 的 `message` 与 `chars`：`auto` 的过短判定与回退原因就写在那里。

## 错误码

| 码 | 含义 |
|---|---|
| `E_NO_PARSER` | 没有后端认这个扩展名；先核对扩展名与 `primary` |
| `E_IO` | 读不到 `source/<key>`；核对 key |
| `E_OVERSIZE` | 超过 `parse.max_source_bytes`（默认 100MB），无放行开关 |
| `E_BAD_SOURCE` | 后端认领后解码失败，如坏 PDF |
| `E_BAD_CONFIG` | `[parse.external]` 不全：缺节、`type`≠`http`、`url`/`auth_env` 空、没有 `@file` 字段、`timeout_s` 为 0 |
| `E_NO_AUTH_ENV` | 变量未设 |
| `E_EXTERNAL` | 连接、超时、状态码或响应路径不符 |
| `E_BOTH_FAILED` | `auto` 下内置与外部都失败，两个原因一起报 |
| `E_BAD_ARGS` | chunk/range 写法或编号不对 |

失败退出码 2，`--json` 时在 `error.code` 与 `error.message`。

## 定位顺序

1. `doctor` 确认仓库。
2. `parse --json` 看 `backend` 和 `warnings`，先分清是谁产出。
3. 按警告表分流：源问题就去换源或换编码，配置问题才改 `wiki.toml`。
4. 拿同扩展名的正常文件对比，确认不是全局配置坏了。
5. 大文件抽 2–3 块，别把整份读进上下文。

## 该不该接外部

默认不接。内置对 `md`/`txt`/`html` 已经够用。只有同时满足才值得接：有扫描 PDF 或重脚本页面，内置给出 `no_text_layer` 或远低于预期，且外部服务能稳定返回 markdown。代价是网络、token 管理、超时，以及失败时的回退分支。图片不会因为外部而变好，没有 OCR。

## 交接

解析确认无误后回 `wiki-scan`，用 `llm-wiki convert --key <key> --json` 落盘（批量交给它）。不要拿 `parse` 的输出手抄成 raw，也不要在 parse 阶段改 staging。`backend` 会写进 raw frontmatter，后续可据此筛需要重转的源。
