# parse 配置

`wiki.toml` 只有仓库根目录一份，`llm-wiki init` 生成。本节只覆盖 `[parse]` 与 `[parse.external]`；解析行为见 [解析后端与 auto 回退](../parse/external.md)。

## `[parse]`

| 键 | 类型 | 默认 | 含义 |
|---|---|---|---|
| `primary` | `"auto"` \| `"builtin"` \| `"external"` | `"auto"` | 后端选择模式 |
| `chunk_threshold` | 尺寸 | `2MB` | 计算源指纹时的分界：不超过它全量哈希，超过它采样哈希（见 `src/convert.rs:432`） |
| `max_source_bytes` | 尺寸 | `100MB` | 超过它的源拒绝转换；`--force-large` 尚未实现，目前没有放行开关 |
| `chunk_target` | 尺寸 | `64KB` | 单个文本块的目标大小；目前只参与校验，还没有代码读取它 |
| `min_chars` | 整数 | `200` | 内置结果字符数低于它时，`auto` 才可能回退外部 |
| `[parse.external]` | 表 | 无 | 可选的外部解析服务；不写就是没有 |

尺寸接受纯字节数或二进制单位，大小写不敏感：

```toml
[parse]
chunk_threshold  = "2MB"     # 或 2097152
chunk_target     = "64KB"
max_source_bytes = "100MB"
min_chars        = 200
primary          = "auto"
```

校验规则（`Config::validate`）：

- `chunk_threshold`、`max_source_bytes`、`chunk_target` 必须大于 0；
- `chunk_target` 不得大于 `chunk_threshold`；
- `min_chars` 允许为 0，语义是「永不因过短回退」；
- 尺寸串写错（`"MB"`、`"-1MB"`、`"2.5MB"`、未知单位）在 `Config::load` 就报 `E_BAD_CONFIG`。

注意：`primary = "external"` 但没写 `[parse.external]` 能通过 `load`/`validate`，第一次解析时才报 `E_BAD_CONFIG`（`no [parse.external] section is configured`）。

## `[parse.external]`

| 键 | 类型 | 默认 | 含义 |
|---|---|---|---|
| `type` | 字符串 | `"http"` | 传输类型；目前只支持 `http` |
| `url` | 字符串 | 空 | 服务地址，POST |
| `auth_env` | 字符串 | 空 | 保存 bearer token 的**环境变量名** |
| `fields` | 表 | `{ file = "@file" }` | multipart 字段；值恰为 `@file` 的字段承载源文件，必须有一个 |
| `response.text` | 字符串 | `"data"` | 从 JSON 响应取 markdown 的点分路径 |
| `response.ok_code` | 整数 | `200` | 视为成功的 HTTP 状态码 |
| `timeout_s` | 整数 | `120` | 单次请求的全局超时（秒），必须大于 0 |

```toml
[parse.external]
type      = "http"
url       = "http://host/api/parse"
auth_env  = "WIKI_PARSE_TOKEN"
fields    = { file = "@file", fileFlag = "N", system = "BSS" }
response  = { text = "data", ok_code = 200 }
timeout_s = 120
```

`fields` 是 map，序列化成 multipart 时按字段名字典序发送；服务端不应依赖 TOML 里的书写顺序。

`auth_env` 存变量名，不存 token。以下值会被 `Config::validate` 以 `E_TOKEN_IN_CONFIG` 拒绝：

- 以 `sk-`、`ghp_`、`eyJ` 开头；
- 以 `Bearer ` 开头（大小写不敏感，含前导空白）；
- 长度超过 40 且不含空白字符。

## 缺失与未知键

`Config` 用 `#[serde(default)]`：缺失的节与键取内置默认值，所以只写两三行也能 `load`。未知键既不会报错也不会生效（没有 `deny_unknown_fields`），拼错键名时要靠 `llm-wiki init` 生成的模板或本页对照。

## 错误码

| 码 | 触发 |
|---|---|
| `E_NO_CONFIG` | 仓库根目录没有 `wiki.toml` |
| `E_BAD_CONFIG` | 非法 TOML、类型/单位错误、数值不变量不满足 |
| `E_TOKEN_IN_CONFIG` | `auth_env` 里写的是 token 字面量 |
