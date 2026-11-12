# 摄入、任务推进与来源结算

由根级开发指引迁入；原节号保留，便于识别既有交叉引用。本文记录开发约束与项目特有问题，产品说明见 [架构说明](../../docs/readme/architecture.md)。

### 2.10 忽略列表只有一个解释入口

`src/ignore.rs::IgnoreList` 是唯一读 `.wiki/ignore` 的地方：`ingest` 用它跳过。
不要在调用点自己拼通配，也不要把它和 `[scan].ignore`（只按目录名粗过滤）混用——两者语义不同，混了就会出现「列表里明明写了却还被扫到」。
显式 `ingest <KEY>` 绕开忽略是故意的：先看一眼再决定要不要永久挡住。

### 2.11 来源待办只由根任务收尾清除

一个来源一条待办，转换与入队写在同一个事务里（`Store::register_conversion` 同时写 `sources`、`raws`、`work_items`、`source_facts`）。半路失败时整笔回滚，不会出现「转换成功、待办丢了」的漂移状态；`tests/worklist.rs::failed_enqueue_rolls_back_conversion_baseline_and_can_retry` 用触发器注入失败钉住。

`task::finish_root` 在整批合并成功后持仓库锁调 `worklist::complete`，四道哈希全部成立才删那一条：源文件当前摘要 == 账本记录的源摘要，raw 文件当前摘要 == 待办记录的 raw 摘要 == 根任务创建 route 时登记的 raw 哈希。任何一项不符都是 `E_SOURCE_DRIFT` exit 1，待办与任务都保留，staging 不清。

幂等是刻意的：根已删、待办已清时，`advance` 返回下一个任务或空，不报错；历史上「重复提交报错」的坑不要重演。

根收尾不读页面正文、不验正文质量、不回滚已写内容。写页期间源又变了，它只能保住待办不误清；正文质量与并发覆盖仍由作者负责。`tests/worklist.rs::done_rejects_old_hash_and_only_clears_the_matching_source` 与 `tests/cli_task.rs::the_last_page_settles_the_source` 钉住。

### 2.17 初始化是自动的，不是开场仪式

初始化已经内化到 `open_repo` 与 `run_doctor`：没有 `.wiki/` 或其中仅有安装资源（`cli/`、`skills/`、`readme.md`，且非空）时，第一条命令（包括 `doctor`）会静默建默认契约与目录，然后继续。技能不再提 CLI 初始化流程，也不再有 CLI 侧的后续提示；`init.md` 只保留 agent 侧的两件事。
首次接入只保留两件 agent 侧的事：把「工作区为 wiki，咨询问题默认调用 wiki」写进持久化记忆，以及问用户是否把「wiki自动摄入」设为定时任务；具体实现由各 agent 自己的记忆与调度能力完成，CLI 和技能都不假定实现方式。
除上述纯安装资源目录外，`.wiki/` 已存在但契约或产品目录缺失时才报 `E_NOT_INITIALISED`，让 `doctor` 诊断、`doctor --fix` 修；这条路径是损坏仓库的出口，不是首次使用的入口。
`tests/precondition.rs::every_repository_command_bootstraps_an_empty_directory` 钉住空目录直接可用，`a_missing_product_directory_is_refused_even_when_the_contract_exists` 钉住损坏仓库仍拒绝。

### 2.19 防抖只认 mtime，不记轮次

一轮要不要转换某个源，只看两件事：mtime 距今超过 `[ingest].settle_seconds`（默认 300 秒），并且读到的内容与账本记录不同。没有 `sync_state`、没有连续轮数：跨轮状态只会把「等多久」变复杂。转换失败的计数是另一件事，记在 `tasks` 的 `convert` 行里（2.24），不参与「等多久」的判断。
`--force` 与 `settle_seconds = 0` 是仅有的两个越权口；点名一个 key 不越权，因为等待属于文件，不属于谁在问。

**元数据缓存只用于省读，不用于证明内容没变。** `source_facts` 记上次全文读取的 hash + size + mtime_ns；只有三者全等且该源已经转换过，`scan` 才复用账本摘要，否则全文重读。代价是：大小和 mtime 都不变的改动会被漏掉，只能 `--force` 复核——文档不得写成「每轮全量校验」。
读文件只有一条路径 `hash::read_source`：先按 `parse.max_source_bytes` 预检，再有界读取并前后校验长度与 mtime，超限或不稳定都是错误而不是半份摘要。扫描、预览、转换必须对同一份固定字节做哈希与解析，不允许各读一遍（历史上大文件曾出现「扫描抽样、转换全文」的摘要分裂，`tests/sync.rs::settled_large_source_uses_the_same_full_digest_for_scan_and_conversion` 钉住）。

安静的一轮不是错误：没有新源时 `ingest` 照常跑完收尾（重建索引、写审计），不因为没有待办而拒绝执行。
旧配置里的 `auto_ingest` / `stable_streak` 已删除。`IngestCfg` 带 `deny_unknown_fields`，带这两个键的 `wiki.toml` 会加载失败（`doctor --fix` 修不了配置），删掉那两行即可。
`tests/sync.rs` 钉住：窗口内的文件等下一轮、mtime 够久就转换、`--force` 越权、大小与 mtime 不变的改动不被缓存掩盖、旧但存活的锁不可回收、掉电留下的锁文件原地复用。

超过 `parse.max_source_bytes` 的源在 `scan` 里是 `Oversize`，既不是「待转换」也不是失败：不转换、不进待办、退出码不变，但 `sync` 每轮必须把它写成 `oversize:` warning（键名 + 上限）。静默跳过会被读成「已经整理过」，`tests/cli_ingest.rs::an_oversize_source_is_named_in_the_warnings_of_every_round` 钉住人类输出与 JSON 两侧。

### 2.21 消失的源自动收口，删页是不可撤销的那一步

`[ingest].on_missing` 有三个值，默认 `forget`：`forget` 去掉页面里那一条引用（Stable 转 Stale）、一条来源都不剩的页整页删除、删 raw 文件、删账本行（`sources`/`raws`/`work_items`/`source_facts` 同一次事务）；`mark_stale` 只把引用它的 Stable 页转 Stale；`ignore` 直接返回空。
顺序是「先页与 raw、后账本行」，所以任一步失败只是本轮多一条 `failures` 与一条 `vanished: ... could not be settled` warning，账本行留着下一轮重试。**删页与删 raw 不可撤销**，这是 `mark_stale` 仍然存在的唯一理由——不要给「页被删了怎么恢复」发明工具内退路（2.9 已说明没有）。
判据只认页面自己的 `sources:`，不认账本的映射：页面可能引用一个账本已不认识的 raw，照样要清。`tests/sync.rs::forget_removes_the_page_the_raw_and_the_ledger_row`、`forget_keeps_a_page_that_still_has_another_source`、`a_failed_forget_keeps_the_ledger_row_for_the_next_round` 钉住三件事。
页质量闸门 `lint::page_quality`（`L12` 页面不引用任何来源、`L13` 正文留着 `TODO`/`FIXME`/`待补`/`占位`，代码围栏内不算）既是 `lint` 的规则，也是 `sync` 对 `affected_pages` 复跑的那一份——直接写进轮次 warning，不等 `doctor`。两处共用同一个函数，不要各写一遍。

### 2.22 任务树取代批次与工作租约

`.wiki/work.lock` 与 `src/lease.rs` 已删除。它服务的概念是「一次只开放一批来源」，现在由 SQLite 的 `tasks` 表承担：一个来源一棵根任务（`breakdown`），根下先有唯一的 `route`；route 关闭后才按归属建 `page`，page 逐个交付、逐个关闭，全部关闭后根收尾合并；route 判定没有写页目标时零写页直接进入收尾。这个单位是**结算单位**，不是锁：它不阻止任何人写文件，只在任务关闭与根收尾时比对基线。

不许以任何形式把它变回「锁人」的门禁：

- `ingest` 与 `task done` 都经 `task::advance` 交付**一个** `next_task`；整棵树用 `task status` 看。交付清单不截断指的是树本身完整，不是一次塞一堆任务。
- `done` 已移除；来源确认由根收尾的四道哈希取代；没有 `E_WORK_LEASE`。
- `moves::apply` 与消失源收口每轮都跑，不因为「批内页可能正在被写」而推迟。

当初的担忧（改名或删页撞上正在写的页）仍然成立，但保护位置变了：`page::write_bytes` 落盘前重读比对，不一致就跳过并写进 warning；任务树的基线比对只做事后越界发现（`E_PAGE_CONFLICT`），不是锁，也抵不了手工写页。

### 2.24 失败的源让位，但不被永久排除

`source_attempts`（migration 8）记「这个源上次转换失败的时间与错误」，只用于排序；同一份失败还会写进 `tasks` 的 `convert` 行（`kind='convert'`，`attempts` 自增，`last_error` 记录原因），那才是熔断依据。`sync::order_pending` 按 `(上次失败时间, key)` 升序排：从未失败过的源键是空串、排在最前，刚失败的源时间最新、沉到队尾。理由是把预算先让给能顺畅处理的文件（用户原话：「失败了，优先级可以放到最低」），同时坏文件不会消失——队列里只剩它时照样会被取到。

写入只有一处：`sync.rs` 转换失败的 `Err` 分支调 `Ledger::record_failure`。清除有三个出口，都是为了「文件不在了笔记也不留」：`register_conversion`（转换成功）与 `delete_source`（源被遗忘）在各自事务里删，文件悄悄消失时由 `sync::run` 的 3c 段按「`source_attempts` 里的键还有没有对应文件」清掉——`book.md` 这类从未转换成功的源没有 `sources` 行，`settle_missing` 根本看不见它。`src/store.rs` 的 `a_failure_note_lives_until_the_source_converts_or_goes_away` 与 `tests/sync.rs` 的 `a_failed_source_yields_the_budget_and_its_note_dies_with_the_file` 钉住这几条出口，`src/sync.rs` 的 `a_failed_source_sinks_behind_untried_ones_but_keeps_its_turn` 钉住排序本身。

连续 3 次转换失败把 `convert` 行置 `failed`：之后每轮只写 `failed: <key> ...` warning，不再给它预算、不再为它建根。它没有自动出口——源改了也不自动重置，只认 `task retry <id>` 删行并清 `source_attempts`。这是唯一允许的熔断，不要再往上加失败计数、退避时间或「N 次自动忽略」；具体放弃与否仍由人用 `task retry` 或 `doctor` 判断。`queued` 只是数字，超出预算的源还必须以 `waiting:` 逐条列出——只报数字等于没让人看见。

### 2.25 任务树的推进只有一个函数

`task::advance` 是唯一决定「下一个任务是谁」的地方：`ingest` 与 `task done` 都调它，别在两处各写一套排序，否则两边的「当前任务」会分叉。顺序是：有 `pending` page 先交付 page；没有 page 时先交付 `reconcile`；再取最早一棵待路由的根；根的子任务全清后在 `advance` 内部收尾。route 完成前不存在 page 任务。

基线是**全局一份**（`GLOBAL_BASELINE = 0`），不是每棵树一份：先收敛的树合并出的页，必须被后收敛的树当成正常状态。`audit/` 与 `index.md` 是派生文件，不进基线；`tasks.id` 用 `AUTOINCREMENT`，删掉的任务 id 不能被复用，否则 staging 或基线会串页。

任务外的改动**不静默吸收**：`advance` 开头调 `task::reconcile_scan`，把当前 `pages/` 与全局基线比对。有 `sources` 的差异页建 `reconcile` 任务，agent 用 `--accept`（承认改动、把该页当前哈希写进基线）或 `--regenerate`（重建来源待办；没有根时 `ensure_root`，已有根走 `--replan`，按当前 raw 重建 route，已写页保留）关闭；没有 `sources` 的页直接写进基线；基线里已消失的页删行（源也没了由 `on_missing` 收口）。空闲轮不再无条件 `refresh_global_baseline`——那正是手工改动被吞掉的原因。

### 2.26 没有可用正文不算转换成功

解析结果必须过一道统一闸门（`parse::validate_extracted`）：去空白后非空、且不是资源链接占位。此前 PDF 抽样探测的假阴性会把有字文档写成附件链接并登记成功，图片后端也靠 `asset` 标志绕过阈值；现在这类结果统一报 `E_BAD_SOURCE`（`text_unavailable` / `partial_extraction` / `lossy_decode`），不进 `raws`、不建根，按既有失败记录让位并在收尾逐份点名。扫描件与图片没有 OCR，属于未处理，不能汇报成已取得正文。

PDF 后端（`src/parse/pdf.rs` + `src/parse/pdf/validate.rs`）只负责“读全、读准、把失败说清楚”：手工遍历页树、逐页严格解压 `Contents` 与 Form XObject、空口令加密自动解密、只重写能精确映射的 Unicode CMap；提取按页进行（每页一个单页副本），页号由循环给出而不是靠分隔符；某页画了可见文字（`Tj`/`TJ` 的非空白字符串）却提取为空时，整份报 `partial_extraction` 并点名页号；只画空白字符串的页不算失败，正常空白页也不算失败。任一页明确失败则整份不提交。扩展名没有后端的文件不进待办、不算失败，每轮以 `unsupported:` 点名。

### 2.27 重转是显式入口，保护写在提交路径上

`ingest --reconvert <KEY>...` 是唯一能重转内容未变来源的入口，只处理显式列出的键，冲突于 `--auto`/位置参数/`--key`/预览选项，支持 `--dry-run` 与 `--force`。`--dry-run` 只做保护检查，不解析、不联网、不写盘。保护逻辑集中在 `src/convert/protection.rs`，普通转换与重转共用：来源摘要、raw 当前摘要、账本记录的 raw 摘要、归属与 pin 必须一致；缺账本记录或摘要不符报 `E_RAW_COLLISION`，不按文本形似猜测。`src/convert/commit.rs` 文件先行、账本随后，登记失败时回写旧字节或删除新文件，恢复也失败则留下 `.*.recovery` 材料并报 `E_IO`。

重转成功时，`Store::register_conversion` 在同一事务里把该源尚未完成的根（pending/active/blocked/failed）标成 `E_SOURCE_DRIFT` blocked，保留载荷、子任务与 staging；只有用户显式 `task retry <根> --replan`（或用户在 reconcile 中显式选择 regenerate，`task done ... --regenerate` 走同一条 replan 路径）才丢弃旧子任务与 staging 并重建 route，已写页按冲突规则保留。普通 `task retry` 对被 `E_SOURCE_DRIFT` 阻断的根返回同码错误，避免旧页混进合并。不要把这个阻断当成可以绕过的锁，也不要自动 replan。
