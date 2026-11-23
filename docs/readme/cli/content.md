# CLI 契约：内容与检索

`ingest`、`query`、`workflow`、`page`：摄入、检索、工作流与页面维护。

返回 [CLI 契约](../cli.md)；所有命令以会话工作区为根目录。

## ingest

扫描根目录（跳过隐藏目录、`scan.ignore`、旧根 `workflows/`、`topics/` 与观看区 `<view>/`），解析
新文件或内容变化的文件，写入 `<view>/raw/`（默认 `wiki/raw/`），并为每个新来源入队任务，
同一份正文的多个路径合成一条 route 任务。`<view>/raw` 被手改时按整份正文采纳：共享它的
路径一起换摘要，文件留在原名原路径。raw 末尾的 `## 原文来源` 段由引擎生成，链回每个挂载
来源；旧 raw 缺这段时，`ingest` 会补上而不会改正文。`workflows/` 放可执行工作流，`topics/` 是 wiki 的探索
层，都不作为摄入来源，摄入时整目录跳过；观看区目录名即使关了发布也仍跳过。工作流与话题
一样由 `query` 直接扫描文件、返回候选；显式 `workflow search` 也可用。

`--dry-run` 用 `.wiki/tmp/` 下的临时库，退出时清理，不写账本；它只列 `would_ingest`，不预告
末尾补偿入队会补哪些来源。`--force` 忽略哈希重新解析。`--only` 按根目录相对 key
精确匹配，不校验参数形状：传绝对路径或含 `..` 的值只会匹配不到来源，命令仍成功退出 0，
也不会摄入任何文件。

`data`: `{ingested[{key,raw,parser,digest,size,chars}], unchanged, unsupported[{key,suffix,reason,hint}],
oversize[{key,message}], failed[{key,message}], vanished, restored, queued_tasks,
raw_links, pending_total, pending, queue{open,queued,leased,route,page}}`。`raw_links` 是
本轮为旧 raw 补上“原文来源”段的文件数。`queued_tasks` 只列本次
新入队的任务 id；`queue` 是提交后的未完成任务快照，`open` 为 `queued` 与 `leased` 之和。
没有新来源时 `queue.open` 可以为 0，此时再跑 `next` 只会得到空任务列表。

超过 `ingest.max_source_bytes` 的文件进 `oversize`，不解析；无文本层的进
`unsupported`。两者都不会中止整批摄入。

别的会话正持有租约时 `ingest` 不动账本，直接报 `E_INGEST_BUSY`：两个执行体一起改任务目录
和 `.wiki/tmp` 正是载荷被写串的入口。`--dry-run` 用临时库，不受这条限制。

## query

`query <词...> [--limit N] [--task <id>]`：按标题、摘要、正文命中数排序，返回页面子集；同时直接扫描
观看区 `<view>/topics/` 与 `<view>/workflows/`，旧根 `topics/`、`workflows/` 只作兜底；命中时
分别在 `topics`、`workflows` 里返回。工作流候选最多 5 个，每条带 `applies`，但不会自动加载；
是否按流程办事由调用方核对后显式 `workflow load`。
`query --page <id>`：`data.page` 是账本元数据，同层返回 `rev`、
`path`（观看区路径；发布关闭时退回 `page:<id>` 引用）、`sources`（`[{key, raw}]`，`raw`
是账本渲染的观看区副本路径，源缺失时为 `null`）、`outgoing`、`incoming`、`body`，以及
`citing_topics`（引用了该页的话题名，按名字排序；有引用时文本里在 `incoming:` 之后多一行
`citing topics:`）。`query --page` 会顺带解析话题依据，坏引用只告警，不影响这次输出。

普通 `query` / `query --page` 是只读快照：不取工作区锁，不创建 `.wiki/`，不补 schema，
不推送观看区。`.wiki/wiki.db` 不存在或 schema 缺列时报 `E_STATE`（退出码 3），先 `ingest`
或 `init`。`query --task` 会写任务目录，仍按写命令加锁。

`data`: `{query, matches, returned, results, topic_matches, topics,
workflow_matches, workflows, boundary}`；`query --page` 的 `data` 另带 `citing_topics`。
`matches` 只数页面，`topic_matches` 只数话题，`workflow_matches` 只数工作流；`topics[]` 为 `{name, title, description, status, updated, path, origin, score, snippet,
refs}`，`refs`（`pages`/`raws`/`topics`/`workflows`/`missing`）；
`origin` 为 `view` 或 `legacy`，按 `path` 直接打开文件；`--page` 与普通文本 `query`（不含
`query --task`）的每页结果都带 `citing_topics`（按名字排序，文本只列前 3 条，其余折算成
`+N`）。`refs.missing` 的元素是 `{kind, target}`，前四组是字符串数组。`workflows[]` 的字段与
`workflow search` 的候选相同。

`query --task <id> 词1 词2 ...`：把这次检索登记到任务上，route 任务提交前必须至少登记一次，
否则 `submit` 报 `E_NO_QUERY`。每个词单独检索、各返上限 5 条，按词分组返回，不并成一次
排序；这次检索只查页面，不带工作流候选。重复的词不重复计配额。配额是 5 个词/1000 字（正文不足 1000 字也给 5 个），超出报
`E_QUERY_BUDGET`。登记要求任务归本会话：别的会话持有的任务报 `E_TASK_FOREIGN`。每次调用
追加一行到 `.wiki/tasks/<id>/queries.jsonl`（`{ts,words,hits}`），
任务关闭时随目录一起删除，不保留审计历史。`data`:
`{task, queries[{word,matches,returned,results}], budget, boundary}`。

检索结果是定位工具，不是覆盖证明。

## workflow

`workflow list`：先列观看区 `<view>/workflows/` 下的一级工作流目录，再列旧根
`<root>/workflows/` 的兼容兜底，最后列技能内置工作流，包括结构不合法的项；同 id 时观看区
优先、旧根其次、内置最后，只列一次。

`data`: `{root, roots, count, valid, warned, workflows[]}`；`root` 是首选工作流目录（观看区
存在时如 `wiki/workflows`，否则 `workflows`），`roots` 是实际存在的目录（观看区在前，旧根
兜底，如 `["wiki/workflows", "workflows"]`），`warned` 是带 warning 的项数；每项为
`{id, source, origin, name, alias_name, description, tags, path, main, skill_path, valid,
error, resources, tools, human_steps, agent_steps, verify_steps, warnings[]}`；冲突项另带
`legacy_path`、`legacy_conflict`。`tools`、`human_steps` 等是各类型步骤的数量，`warnings[]`
为 `{code, line, text, message}`；`name` 是工作流机器名、`alias_name` 是可选的中文展示名
（人类可读行优先显示它）；`tags` 按 Obsidian 规则规范化（去 `#`、空白切分、保留 `a/b`
层级、丢弃纯数字），`source` 取 `project` 或 `builtin`，`origin` 取 `view`、`legacy` 或
`builtin`。`path`/`main` 只给项目工作流（工作区根目录相对），观看区版以 `wiki/` 开头，旧根
兜底以 `workflows/` 开头；内置项改给 `skill_path`（技能目录相对，形如
`workflows/<id>/WORKFLOW.md`），两者互斥。同 id 的观看区版与旧根版 `WORKFLOW.md` 不同时，
`legacy_conflict` 为 `true`，`legacy_path` 指向旧根主文件；人类输出打印“观看区版本生效”，
加载仍用观看区版。
`resources[]` 为 `{path, kind, size}`，`kind` 取 `script`（`scripts/` 下）、`reference`
（`references/` 下）或 `other`。

`workflow search <词...> [--limit N]`：显式检索工作流，返回命中的项目工作流与内置工作流；
最多 5 个，按分数排序；不传词返回 `E_USAGE`。候选带 `applies`（`## 适用` 原文），0 命中时
data 带 `message: "无可用workflow"`。只有核对 `applies` 并 `load` 后，才按工作流的分支流程
执行；候选不会自动加载。

`data`: `{query, matches, workflows[], message?}`；`matches` 只数工作流，`workflows[]` 为
`{id, source, origin, name, alias_name, description, tags, applies, path, main, skill_path,
load, score, resources, tools, human_steps, verify_steps, warnings}`，`load` 是建议的加载
命令，`path`/`skill_path` 按来源二选一（规则同 `workflow list`）；0 命中时 `message` 为
`"无可用workflow"`。非法工作流不进 `workflow search` 结果。

`workflow load <id>`：加载一个合法工作流（项目或内置），返回主文件全文、资源清单与按类型
分好的步骤。

`data`: `{workflow, path, content, meta, resources, tools, human_steps, agent_steps,
verify_steps, warnings}`。`content` 是 `WORKFLOW.md` 全文（含 frontmatter），`path` 是主
文件的根目录相对路径（项目工作流；观看区版以 `wiki/` 开头，旧根版以 `workflows/` 开头），
`workflow.origin` 标明来源。`tools`、
`human_steps`、`agent_steps`、`verify_steps` 每项为 `{line, text}`，`warnings[]` 同上。
内置工作流不给 `path`/`main`，改给 `skill_path`（技能目录相对，形如
`workflows/<id>/WORKFLOW.md`）；其 `resources` 恒为空。`workflow list` 与 `workflow search`
的条目按同一规则给 `path` 或 `skill_path`。

三个命令都只读：不创建 `.wiki/`，不写账本，不要求先 `init` 或 `ingest`。工作流文件改动后
下一次 `workflow list` / `workflow load` 立即生效。`id` 必须是工作流目录下的一级目录名
（或内置 id），包含路径分隔符或试图跳出目录返回 `E_USAGE`；工作流不存在返回
`E_NOT_FOUND`；缺 `WORKFLOW.md`、缺 `description`、正文为空、文件过大、工作流目录或主
文件指向区外，或步骤里没有 `tool:` 步骤，返回 `E_WORKFLOW_INVALID`。没有 `## 步骤`、
这一节没有列表项、或整份没有任何类型标记的工作流在 0.5.0 仍可加载但带 `warnings`，
`0.6.0` 起返回 `E_WORKFLOW_INVALID`。`workflow list` 不因个别工作流不合法而失败。

## page

- `page import <id> --from <file>`：采用人工编辑的页面文件（可带 frontmatter），
  生成新修订（`origin=import`），不强制 evidence。只允许已存在的页面。frontmatter 的
  `title/type/summary/sources/links` 会并入；`links` 必须是块列表，与旧链接取并集，
  未知目标报 `E_LINK_UNRESOLVED`；`aliases/status/updated` 忽略。
- `page link <id> <target...>`：给已存在页面补关联，生成新修订（`origin=link`），只增不删。
  目标必须是已存在页；未知页报 `E_LINK_UNRESOLVED`，空目标或指向自己报 `E_USAGE`；已链过
  的目标不产生新修订。成功后立即推送观看区，告警写 stderr。单页模式可加 `--dry-run`
  只预览、不写修订。`data`: `{mode:"single", dry_run, page, rev, links, added, view}`，
  预览时没有 `links`/`view`。
- `page link --repair [--dry-run]`：按“共享同一 raw 来源”推断缺失的边。同一来源下的页
  两两配对，已有任一方向的边就跳过，每对只加一条（页 id 小的指向大的，观看区两边都会
  显示）；不传页面 id 和目标。`--dry-run` 只输出计划。`data`:
  `{mode:"repair", dry_run, rules:["shared-source"], edges, pages, view}`，`pages` 每项为
  `{page, rev, added}`。只补不删，不改变已有边的方向。
- `page restore <id> --rev N`：把第 N 版内容写成新修订（`origin=restore`）。该页在观看区
  有未收割手改时，推送跳过它，恢复内容不会出现；此时跑 `sync` 反而会把那份手改收成更新一版
  并盖过 restore。先 `sync` 处理手改，再 `restore`。
