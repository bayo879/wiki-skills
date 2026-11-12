# PDF 字体 ToUnicode 覆盖不全时仍会静默丢字

## 问题

Type0 / Identity-H 字体只把部分绘制 code 映射到 Unicode 时，`pdf-extract 0.7.12` 对未映射的 code 返回空串（依赖源码 `pdf-extract-0.7.12/src/lib.rs:1037-1044`），而当前校验只确认 `ToUnicode` 流本身可解码（`src/parse/pdf/validate.rs:607-665` 的 `check_font`），不检查页面实际绘制的每个 code 是否都有映射。

只要同一页还有另一个可映射字符，提取结果就非空，逐页门禁（`src/parse/pdf.rs` 的 `has_nonblank_text && blank`）不会触发，整份照常登记成功，未映射的字被静默丢弃。

复现（合成样本，code `<0001>` 映射为 `A`，`<0002>` 未映射，同一页用 TJ 绘制两者）：

```text
exit 0, converted=1, failures=0
raw 尾部：first page body\n\nA
```

`<0002>` 消失，只有 `below_min_chars` 警告。这是既有字体覆盖边界，不是本轮 `'`/`"` 修复引入。

## 影响

- 少数字体只提供部分 `ToUnicode` 映射的 PDF 会登记成功，正文缺字且没有页级失败提示。
- spec §4.2 已声明“未发现错误不等于逐字准确”，因此本轮不把它升级为阻断；但 `docs/readme/parse/external.md` 里“不静默丢字”的说法目前只覆盖命名 CMap，不覆盖这一边界，需要在文档中限定范围。

## 候选方案

未定案。可选方向：

1. 在扫描阶段按当前字体收集实际绘制的 code，解析 `ToUnicode` 的 bfchar/bfrange 覆盖范围并缓存，发现未覆盖的可见 code 时报 `lossy_decode`（可能对使用预定义 CMap 或复杂编码的字体产生误报，需要先确定只对 Identity-H/V 生效）。
2. 维持现状，把边界写进解析说明与失败可见性小节，明确“只保证已映射字符不丢”。
3. patch/vendor `pdf-extract`，让未映射 code 返回替代字符或错误，由上层决定。

## 待确认与验收条件

- [ ] 确认该边界是否属于本轮“正文可靠提取”的验收范围，还是留作独立修复。
- [ ] 若修复：补“部分 ToUnicode 覆盖 + 同页有可映射字符”的合成样本测试，断言整份 `lossy_decode` 并点名页号。
- [ ] 若保留：在 `docs/readme/parse/external.md` 明确限定“不静默丢字”的适用范围。
- [ ] 复核预定义 CMap、竖排 Identity-V 与多字节简单字体不会被误判。
