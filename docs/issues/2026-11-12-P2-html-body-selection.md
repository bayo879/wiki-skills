# HTML 正文选择与表格结构仍可能丢内容

优先级：P2

## 问题

2026-11-12 修复 PDF 误判时，统一了「没有可用正文不算转换成功」的闸门（见 `docs/readme/parse/external.md`），但没有改 HTML 抽取算法。当前实现（`src/parse/html.rs`）仍有三种会丢内容却仍算成功的情形：

- 只取首个 `article` / `main` / `body`（`src/parse/html.rs:99-111`）。一个页面里多个 `article` 时，其余正文被丢弃，字符数仍可能超过 `[parse].min_chars`，`auto` 不会回退。
- 表格只按文本节点输出（`src/parse/html.rs:152-184`），单元格关系丢失，`|` 分隔与表头都不存在。
- 非 UTF-8 正文用替换字符解码并只加 `bad_utf8` 告警（`src/parse/html.rs:53-83`）；非空结果继续按成功提交。

这三条已由 `tests/parse_gate.rs` 的 `only_the_first_article_is_published`、`table_cells_survive_without_their_structure`、`invalid_utf8_in_a_body_keeps_the_readable_part` 固定为当前风险，不是猜测。它们不违反「无可用正文不得提交」的闸门：结果有正文，只是不完整。

## 影响

多 `article` 的抓取页、带表格的说明文档、GBK 编码的旧页面可能在「转换成功」的外观下丢失结构或整段内容，后续检索与整理会引用不完整的材料。字符数阈值只能发现「太少」，发现不了「少了一块」。

## 候选方案

未定案。可选方向：

- 正文选择改为「所有 `article` 按文档序拼接，没有 `article` 才退回 `main`/`body`」，并加样本测试；
- 表格保留行列结构（至少输出单元格分隔与表头行），或明确降级为纯文本并记 warning；
- 编码：对可识别声明的非 UTF-8 页面按声明解码，无法识别时保留现状告警；不做编码猜测。
- 是否需要把「已知有损」也纳入成功闸门（例如表格文档输出后没有单元格结构就报 `lossy_decode`），要先和 PDF 的 `lossy_decode` 语义对齐，不单独发明一套。

三个方向都改变已发布正文的形态，需要单独审批；本轮 spec 明确不扩大范围。

## 待确认与验收条件

- [ ] 用户确认是否以及按哪个方向改正文选择/表格/编码。
- [ ] 样本：多 `article` 页面、含表头与合并单元格的表格页、GBK 声明页面各一份，断言正文与结构。
- [ ] 若引入「有损即失败」，需同步 `docs/readme/parse/external.md` 的闸门说明与 `E_BAD_SOURCE` 原因表。
- [ ] 现有三条风险测试在行为改变后一并改写，不能只删断言。
