# 旧库兼容代码留在 lib 里，与「删除版本管理」的范围差

登记：2026-11-10　状态：未完成（待决定保留还是删除）

## 问题

CLI 面已经删除 `submit` 与 `version`（clap 拒绝，`tests/cli_submit.rs`、
`tests/cli_version.rs` 钉住），但库层仍完整保留旧发布路径：

- `src/lib.rs` 仍导出 `pub mod ingest`、`staging`、`version`；
- `tests/version.rs` 仍有 35 个测试覆盖 `approve`/`checkpoint`/`reject`/`rollback`；
  `tests/ingest.rs` 仍测 `begin`/`submit`/`abort`；
- `src/ingest.rs:346` 仍调 `version::checkpoint`。

另外 `Ledger::forget_source`（`src/ledger.rs:90-95`）只删 `raws`/`sources`，不清
`work_items` 与 `source_facts`；当前没有生产调用，将来接上 CLI 会留孤儿待办。

## 影响

维护者读 `AGENTS.md` §2.9「没有候选、没有发布、没有工具内历史」会以为代码已清空，
实际库 API 与测试仍在维护旧路径；两套语义并存时，改动容易只改一边。

## 候选方案（未定案）

1. 彻底删除：移除三个模块与对应测试，`src/ingest.rs` 的调用一起拆掉；改动面最大，
   但语义最干净。
2. 保留但收敛：模块改为 `pub(crate)` 或 feature gate，测试只保留「旧数据可读」的
   最小集合，文档明确「仅 CLI 面删除，库保留兼容」。
3. 只做文档澄清 + 补 `forget_source` 的清理语义。

## 待确认

- 旧库用户是否还有程序化调用这些库函数的需求（没有公开 API 承诺）。
- 删除后旧测试里对「旧版本数据可读」的覆盖要不要保留。

## 验收条件

- [ ] `AGENTS.md` §2.9 的范围表述与代码一致（CLI 面 / 库层分开写）。
- [ ] 若保留，旧模块的文档注释写明「只读兼容，新增代码不得依赖」。
