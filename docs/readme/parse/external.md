# 解析后端与 auto 回退

`convert` 与 `parse` 共用同一套解析层：按扩展名选后端，把源字节变成 markdown。本页写实际行为，配置键见 [parse 配置](../config/parse.md)。

## 后端一览

| 后端 id | 认领的扩展名 | 说明 |
|---|---|---|
| `builtin-text` | `md` `markdown` `txt` | 原样读入，去 BOM，坏 UTF-8 用替换字符并告警 |
| `builtin-html` | `html` `htm` | 抽取正文文本 |
| `builtin-asset` | `png` `jpg` `jpeg` `gif` `webp` `svg` | 不读内容，产出 `![](../source/<key>)` 链接 |
| `builtin-pdf` | `pdf` | 抽取文本层；抽不到文本层时降级成资源链接并置 `no_text_layer` |
| `external-http` | `md` `markdown` `txt` `html` `htm` `pdf` | 把源文件 POST 给外部服务，取回 markdown |

后端 id 会写进 `raw/` 的 frontmatter 与 `--json` 输出，便于按解析器版本筛出需要重转的源。扩展名大小写不敏感。

## 三种 `primary` 模式

`pick_backend` 是唯一决定「先试谁、是否试外部」的地方：

| `primary` | 文本 / HTML / PDF | 图片 | 其它扩展 |
|---|---|---|---|
| `auto`（默认） | 内置优先；仅当配了 `[parse.external]` 且扩展是 `html`/`htm`/`pdf` 时，外部作为第二候选 | 只走 `builtin-asset`，永不外部 | `E_NO_PARSER` |
| `builtin` | 只走内置 | 只走内置 | `E_NO_PARSER` |
| `external` | 只走 `external-http`（`md`/`markdown`/`txt`/`html`/`htm`/`pdf`） | 仍走 `builtin-asset` | `E_NO_PARSER` |

`auto` 从不让 `md`/`txt` 走外部：纯文本没有「读不出字」的问题，内置解析就是恒等变换。

## `auto` 的回退判定

1. 先跑内置。
2. 内置**报错**：
   - 没配外部 → 原样返回这个错误；
   - 配了外部 → 试外部；成功则用外部结果并加 `external_used` 警告；失败则返回 `E_BOTH_FAILED`，`builtin` 与 `external` 两个原因都在 `message` 里。
3. 内置**成功**但结果「不达标」：
   - 不达标 = `markdown` 字符数 `< [parse].min_chars`，或 `asset == true`（PDF 无文本层，只有一张链接图）。
   - 不达标且配了外部 → 试外部：
     - 成功：用外部结果，加 `external_used` 警告；
     - 失败：**保留内置结果**，加 `external_failed` 警告（消息里是外部失败原因），命令仍算成功。
4. 内置成功且达标 → 直接返回，不发任何网络请求。

`min_chars` 数的是字符（`chars().count()`）不是字节；默认 200。

## 外部服务契约

请求（`ureq` 3.x）：

```
POST <url>
Authorization: Bearer <auth_env 指向的环境变量的值>
Content-Type: multipart/form-data; boundary=...

字段顺序 = fields 的字典序（BTreeMap）
值为 "@file" 的字段 → 文件 part，filename 取源文件名，内容是源文件原始字节
其它字段 → 普通文本 part，值原样发送
```

- 全局超时 `timeout_s` 秒，超时按 `E_EXTERNAL` 报错，不会挂死。
- HTTP 状态必须等于 `response.ok_code`（默认 200）；不一致报 `E_EXTERNAL`，消息带状态码与响应体前 200 字符。

响应：JSON，按 `response.text` 的点分路径取值；对象键与数组下标（十进制数字）都可走，例如 `data`、`data.markdown`、`result.items.0.text`。取到的必须是字符串，否则 `E_EXTERNAL`。

`wiki.toml` 示例（`init` 模板里以注释形式给出）：

```toml
[parse.external]
type      = "http"
url       = "http://host/api/parse"
auth_env  = "WIKI_PARSE_TOKEN"          # 变量名，不是 token
fields    = { file = "@file" }          # 附加字段按服务端要求填
response  = { text = "data", ok_code = 200 }
timeout_s = 120
```

## 错误码与警告

| 码 / 警告 | 触发 |
|---|---|
| `E_BAD_CONFIG` | `[parse.external]` 缺失、`type != "http"`、`url`/`auth_env` 为空、`timeout_s == 0`、`fields` 中没有一个值为 `@file` |
| `E_NO_AUTH_ENV` | `auth_env` 指向的环境变量未设置；消息只出现变量名 |
| `E_EXTERNAL` | 连接失败、超时、非 `ok_code`、响应不是 JSON、路径缺失或非字符串 |
| `E_BOTH_FAILED` | `auto` 下内置与外部都失败；两个原因都在消息里 |
| 警告 `external_used` | 本次结果来自外部后端 |
| 警告 `external_failed` | 外部尝试失败，保留的是内置结果 |

## 安全与测试

- token 只在发请求那一刻从环境变量读出，不落盘、不进 `--json`、不进任何错误消息。写进 `auth_env` 的 `Bearer ...`、`sk-...` 这类字面量会被 `Config::validate` 以 `E_TOKEN_IN_CONFIG` 拒绝。
- 外部路径的测试全部打 `tests/mock_http.rs` 起的 127.0.0.1 单次服务，不访问真网；token 用测试进程内自设的变量。
