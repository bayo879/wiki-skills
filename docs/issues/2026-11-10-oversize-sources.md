# 超过 max_source_bytes 的源被静默跳过，普通 ingest 不提示

登记：2026-11-10　状态：未解决（已确认是缺陷，方案未定）　优先级：P2

## 问题

`scan` 把超过 `parse.max_source_bytes` 的文件标成 `Oversize`（`src/scan.rs` 的分类分支），
但 `sync::run` 只把 `New` / `Changed` 收进 `work_items`，`Oversize` 既不进待办也不进 `warnings`。
后果：

- 普通 `ingest` 退出码 0，人看不到「这个文件永远不会被转换」；
- 只有显式 `ingest <KEY>` 才通过 `hash::read_source` 报 `E_OVERSIZE`（exit 2）；
- `doctor` 的 `scan` 检查能看到，但没人规定每轮都跑。

## 影响

一个 100MB+ 的 PDF 放进源目录后，每轮 ingest 都安静跳过，作者以为已经整理过。

## 候选方案（未定案）

1. `sync` 把本轮的 `Oversize` key 汇总成一条 `warning`（键名 + 上限），退出码不变。
2. 同 1，但计入 `failures`，exit 1——更刺眼，代价是每轮都红。
3. 只在 `doctor` 报，`ingest` 保持安静——需要先决定「谁负责发现」。

## 待确认

- 超限源应该算「用户配置太小」还是「文件不该进源目录」？前者提示调配置，后者提示加忽略。
- 是否要支持按文件覆盖上限（类似 `--force-large`），还是让用户改 `wiki.toml`。

## 验收条件

- [ ] 一轮含超限源的 `ingest` 在人类输出里能看到文件名与上限。
- [ ] 有测试钉住「超限源不会静默消失」。
