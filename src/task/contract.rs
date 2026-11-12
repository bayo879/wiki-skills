//! What one task delivers besides the work itself: a guide and a payload
//! contract.
//!
//! A task is only self-explaining if the agent is told what to read, what to
//! decide, what it must not do, when it is done, and what every payload field
//! means. The same contract is handed to `ingest`, `task done` and `task status`
//! through one `TaskView`, so the three surfaces cannot drift apart.
//!
//! Examples are data, not prose: the tests run every `route-v1` example through
//! [`super::route::parse`] and [`super::route::validate_shape`] and every
//! `page-content-v1` example through the real `LeafPayload` reader, so a field
//! rename or a new cross-field rule breaks the contract test instead of the
//! agent.

use serde::Serialize;

/// What the agent needs to know before it can start a task.
///
/// The prose is the deliverable: `must_read` fixes the order of reading,
/// `forbidden` names the shortcuts that produce wrong pages, and the split
/// between [`Guide::cli_can_check`] and [`Guide::agent_owns`] keeps the tool
/// from implying it proved more than it did.
#[derive(Debug, Clone, Serialize)]
pub struct Guide {
    /// One sentence: what this task decides, in the agent's own terms.
    pub goal: String,
    /// What to read, in order, with the field or command that points at it.
    pub must_read: Vec<String>,
    /// The judgements only the agent can make.
    pub must_decide: Vec<String>,
    /// Shortcuts that write a wrong page or a wrong verdict.
    pub forbidden: Vec<String>,
    /// Observable conditions for handing the payload back.
    pub done_when: Vec<String>,
    /// When to stop and ask a human instead of deciding.
    pub human_escalation: String,
    /// Mechanical checks the CLI actually runs.
    pub cli_can_check: Vec<String>,
    /// Semantic work the CLI cannot prove, so the agent stays responsible.
    pub agent_owns: Vec<String>,
}

/// The submission contract of one payload schema.
///
/// [`Contract::constraints`] carries the cross-field rules a flat field table
/// cannot express (which fields each action may carry, which references must
/// resolve). The examples are the same data the tests feed to the real parsers,
/// so the contract cannot quietly disagree with the validator.
#[derive(Debug, Clone, Serialize)]
pub struct Contract {
    /// Schema tag the task names, e.g. `route-v1`.
    pub schema: String,
    /// Every field, nested ones by dot path (`points[].id`).
    pub fields: Vec<Field>,
    /// Rules enforced across fields, not expressible as `required`.
    pub constraints: Vec<String>,
    /// Valid payloads, each run through the real parser by the tests.
    pub examples: Vec<Example>,
    /// What to change after a rejection, and whether resubmission is allowed.
    pub failure_fixes: Vec<String>,
}

/// One field of a payload schema.
#[derive(Debug, Clone, Serialize)]
pub struct Field {
    /// Dot path inside the payload; array items use `[]`.
    pub name: String,
    /// JSON shape, e.g. `string`, `array<object>`, `flag`.
    pub typ: String,
    /// Whether a submission must carry real content here.
    pub required: bool,
    /// What the value means to this task.
    pub meaning: String,
    /// Who produces the value.
    pub filled_by: FilledBy,
    /// What an empty, missing or `null` value means.
    pub empty: String,
    /// Allowed values when the field is an enum; empty otherwise.
    pub allowed: Vec<String>,
}

/// Who fills a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FilledBy {
    /// The agent decides the value.
    Agent,
    /// The tool constructs it; the agent must not invent one.
    Cli,
}

/// One valid payload, named by what it demonstrates.
#[derive(Debug, Clone, Serialize)]
pub struct Example {
    /// Short description of the case the example covers.
    pub title: String,
    /// The payload as the agent would submit it.
    pub payload: serde_json::Value,
}

/// The self-explaining guide for one task kind.
pub(crate) fn guide(kind: &str) -> Guide {
    match kind {
        "route" => route_guide(),
        "page" => page_guide(),
        "reconcile" => reconcile_guide(),
        other => uncovered_guide(other),
    }
}

/// The payload contract for one schema tag.
pub(crate) fn contract(kind: &str, types: &[String]) -> Contract {
    match kind {
        "route-v1" => route_contract(types),
        "page-content-v1" => page_contract(),
        "reconcile-v1" => reconcile_contract(),
        other => uncovered_contract(other),
    }
}

fn route_guide() -> Guide {
    Guide {
        goal: "把来源里值得留下的信息点提出来，为每个点决定去向（修订已有页、按提案新建一页，或明确不写），并处置所有已引用这个来源的页。".into(),
        must_read: vec![
            "先用 read 读 source.read_ref 指向的 raw 全文：它是这个来源的完整内容，不是摘要。".into(),
            "再逐个 read 打开 affected_pages 里的页面正文：这些是已有引用页，不是候选全集，写处置前必须看过。".into(),
            "用 query <关键词> 检索候选；要按页或按来源精确看关系时另起 query --page <页 key> 或 query --source <raw>，多轮迭代到你相信“已有页能不能承载”这个判断为止。".into(),
        ],
        must_decide: vec![
            "来源里有哪几条信息点：一条点是一个可独立转述的结论或做法，不要按段落切。".into(),
            "每个点落在哪个已有页，还是值得独立成页；新页必须先说清读过的近似候选为什么不适合。".into(),
            "每个已引用页要 update 还是 unchanged；unchanged 表示看过且决定不改，不是没看过。".into(),
        ],
        forbidden: vec![
            "不得直接创建或修改 .wiki/pages/ 下的文件：页面只能由本载荷的 create/update 经 page 任务产生。".into(),
            "不得只凭路径或标题相似去重：判断依据是页面正文有没有已经承载这条信息。".into(),
            "不得把 affected_pages 当候选全集：候选可以是从未引用当前来源的页。".into(),
            "不得用 skip 掩盖没读过的页，也不得在 unchanged 里写“未涉及”这类没有判断的理由。".into(),
        ],
        done_when: vec![
            "每个 points[].id 至少有一条 assignments[] 去向（update/create/skip）；同一个点可以指向多个页面，CLI 会在每个目标里重复这个点。引用的点和提案都真实存在。".into(),
            "affected_pages 里的每一页在 maintenance[] 都有 update 或 unchanged，并带非空 reason。".into(),
            "points 为空时 source_reason 写清这个来源为什么不需要任何页面。".into(),
            "每条 create 都能在 proposals[] 找到同 id 的提案，提案有类型、标题、摘要与两条理由。".into(),
        ],
        human_escalation: "来源结论互相矛盾、两个候选页都能承载而取舍会改变页面边界，或者要不要新建页是产品决定时，不要替人决定：把问题和候选写清楚交回用户，并让这个 route 保持未关闭，不要提交一个猜的载荷。".into(),
        cli_can_check: vec![
            "JSON 能否解析、字段是否齐全，point/proposal/affected 页这些引用是否指向载荷内或仓库内真实存在的对象。".into(),
            "每个点是否有去向、affected_pages 是否都有处置、空 points 是否有 source_reason、提案类型是否在 [schema].types 内。".into(),
            "action 与字段是否互斥（skip 不带写入字段，update 不带 proposal/reason/covered_by，create 不带 target/change/basis/reason/covered_by）。".into(),
        ],
        agent_owns: vec![
            "信息点是否提全、来源是否真的读懂；CLI 无法证明遗漏。".into(),
            "归属判断是否成立：已有页能否承载、新页是否值得独立、unchanged 是否真的看过。".into(),
            "检索是否做过、候选看得够不够广；searches[] 只是自述，不是调用证据。".into(),
        ],
    }
}

fn page_guide() -> Guide {
    Guide {
        goal: "把本页要承接的信息点融进目标页的完整正文，提交一整页内容，而不是新来源的片段。".into(),
        must_read: vec![
            "先 read 目标页的当前完整正文（target.reference）；更新已有页时这是融合的起点，新建页时用它确认目标确实不存在。".into(),
            "读 source.read_ref，只把本页要承接的信息点写进去，不要把整个来源抄成一页。".into(),
        ],
        must_decide: vec![
            "新信息放在哪一节、和已有结论是否冲突；冲突要先解决再提交。".into(),
            "哪些旧内容需要改写成与新信息一致；不要留下两段互相矛盾的结论。".into(),
            "status 用 draft 还是 stable；没有把握就不要标 stable。".into(),
        ],
        forbidden: vec![
            "不得只提交新来源的局部片段或 diff：payload.body 会整页渲染，片段等于删掉其余内容。".into(),
            "不得提交你没读过的旧正文：工具不比对正文，覆盖不可撤销。".into(),
            "不得自己写 id、type、sources、updated 或 frontmatter；这些由 CLI 从任务构造。".into(),
        ],
        done_when: vec![
            "payload.body 是融合后的完整正文，既有本页原有内容，也有本来源要承接的信息点。".into(),
            "title 非空；summary/tags/status 描述融合后的整页，而不是只描述新片段。".into(),
            "本页承接的信息点都能在正文里找到对应表述，且没有与来源冲突的旧结论残留。".into(),
        ],
        human_escalation: "新旧结论冲突且需要人决定保留哪一个，或者这次修订会改变页面边界（该拆页、该并页）时，停下来问用户；不要用一次提交替人做取舍。".into(),
        cli_can_check: vec![
            "JSON 能否解析、title 是否非空、status 是否是 draft/stable/stale。".into(),
            "页面渲染后能否被重新解析（frontmatter 与正文格式合法）。".into(),
            "关闭时 pages/ 有没有被任务外的改动动过（E_PAGE_CONFLICT）。".into(),
        ],
        agent_owns: vec![
            "正文是否真的融合完整、有没有漏掉旧内容；CLI 只做格式回读，不比对语义。".into(),
            "信息点取舍、冲突处理与 status 判断。".into(),
        ],
    }
}

fn reconcile_guide() -> Guide {
    Guide {
        goal: "决定一页在任务外被改过的内容算不算数：接受这次改动（accept），或按页面引用的来源重新生成它（regenerate）。".into(),
        must_read: vec![
            "先 read target.reference 的当前正文：target.reference 与任务哈希由 CLI 从任务给出，不在你提交的载荷里；页面正文要和它引用的来源逐条比对，才判断得了改动是否仍然成立。".into(),
            "需要时用 query --page 看这一页的出入链、query --source 看还有谁引用同一个 raw，再裁决。".into(),
        ],
        must_decide: vec![
            "这次改动是有意修订还是误改；accept 意味着把它写进基线，之后不再提示。".into(),
            "regenerate 会按页面引用的每个 raw 重走 route/page，可能整页改写；确认这些来源还在、值得再跑一遍。".into(),
        ],
        forbidden: vec![
            "不得直接改回或删除页面来“消除提示”：那同样是一次任务外改动，只会再生成一个 reconcile 任务。".into(),
            "不得既不 accept 也不 regenerate 而放着不管；reconcile 不关闭，后续任务会一直等它。".into(),
        ],
        done_when: vec![
            "accept 时确认任务登记的这版正文就是想要的版本；regenerate 时确认页面引用的来源足以重建这一页。".into(),
            "裁决后任务关闭，基线推进到任务登记的字节，同一处改动不再提示。".into(),
        ],
        human_escalation: "改动涉及结论取舍，或重新生成会丢掉只有人工知道的内容时，先问用户再裁决。".into(),
        cli_can_check: vec![
            "裁决是否只给了一个：--accept 与 --regenerate 都不给或同时给都会被拒。".into(),
            "regenerate 时页面引用的 raw 是否还在账本里；一个都不在会以 E_TASK_BAD_SOURCE 拒绝。".into(),
        ],
        agent_owns: vec![
            "改动该不该留、重新生成值不值得；CLI 只记录裁决结果与哈希。".into(),
            "页面正文与来源是否仍然一致，CLI 不做语义比对。".into(),
        ],
    }
}

fn uncovered_guide(kind: &str) -> Guide {
    // Only `convert` is advanced by the tool. Other kinds that reach the
    // fallback (e.g. `breakdown`) do have a submission path; pretending they
    // have none would be a lie, so they get "no contract here, read the task".
    if kind == "convert" {
        return Guide {
            goal: "convert 任务没有提交载荷：它由工具推进（来源转换成功或失败都会更新这一行），不要用 task done 关闭它。".into(),
            must_read: Vec::new(),
            must_decide: Vec::new(),
            forbidden: vec![
                "不得对 convert 任务提交 --payload、--leaves 或裁决；task done 会以 E_TASK_BAD_PAYLOAD 拒绝。".into(),
            ],
            done_when: vec!["任务由工具推进时，按 task status 看它下一步变成什么。".into()],
            human_escalation: "任务长时间不动或反复失败时，用 task status 看原因，必要时 task retry 或问用户。".into(),
            cli_can_check: vec!["任务状态与失败原因。".into()],
            agent_owns: vec!["不要替工具推进这类任务；需要人处理的失败交给用户。".into()],
        };
    }
    Guide {
        goal: format!(
            "{kind} 任务没有定义交付契约：不要自己构造载荷，按 task status 看它当前的状态与下一步。"
        ),
        must_read: vec!["先用 task status 看这个任务的状态、失败原因与可用的后续动作。".into()],
        must_decide: Vec::new(),
        forbidden: vec![format!(
            "不得给 {kind} 任务拼一个自创载荷：没有为它定义字段与校验，提交只会被当成任务与载荷不匹配。"
        )],
        done_when: vec!["按 task status 给出的状态确认下一步；这类任务不通过本契约交付。".into()],
        human_escalation: "任务长时间不动或反复失败时，用 task status 看原因，必要时 task retry 或问用户。".into(),
        cli_can_check: vec!["任务状态与失败原因。".into()],
        agent_owns: vec!["按 task status 处理，不要替工具发明载荷或裁决。".into()],
    }
}

fn route_contract(types: &[String]) -> Contract {
    let fields = vec![
        field(
            "points",
            "array<object>",
            false,
            "来源里可独立转述的信息点；按语义切，不按段落切。",
            FilledBy::Agent,
            "缺省或 [] 表示这个来源没有可提取的信息点，此时必须给非空 source_reason。",
        ),
        field(
            "points[].id",
            "string",
            true,
            "只在本次载荷内有效的点 id；assignments[].point 用它引用。",
            FilledBy::Agent,
            "不允许空；空 id 会被拒。",
        ),
        field(
            "points[].finding",
            "string",
            true,
            "这条信息点说了什么，用可转述的结论表述。",
            FilledBy::Agent,
            "不允许空。",
        ),
        field(
            "points[].evidence",
            "string",
            true,
            "它在来源里的位置（小节、片段或原话），供人核对。",
            FilledBy::Agent,
            "不允许空。",
        ),
        field(
            "points[].boundary",
            "string",
            false,
            "超出来源就不该被声称的边界。",
            FilledBy::Agent,
            "null 或缺省表示不需要额外声明边界。",
        ),
        field(
            "assignments",
            "array<object>",
            false,
            "每个信息点的去向，一个点一行。",
            FilledBy::Agent,
            "只在 points 为空时可以缺省；否则每个点至少有一条，同一个点指向多个页面时每个页面各写一条。",
        ),
        field(
            "assignments[].point",
            "string",
            true,
            "对应 points[].id，必须指向载荷里真实存在的点。",
            FilledBy::Agent,
            "不允许空，也不允许引用不存在的点。",
        ),
        enum_field(
            "assignments[].action",
            "string",
            true,
            "这条去向做什么：改已有页、按提案新建，还是明确不写。",
            FilledBy::Agent,
            "不允许空；只能是这三个值。",
            &["update", "create", "skip"],
        ),
        field(
            "assignments[].target",
            "string (page reference)",
            false,
            "要修订的已有页，仓库相对路径；update 必填。",
            FilledBy::Agent,
            "update 之外的 action 必须缺省，不得携带。",
        ),
        field(
            "assignments[].change",
            "string",
            false,
            "这次改动新增或改写什么；update 必填。",
            FilledBy::Agent,
            "update 之外必须缺省。",
        ),
        field(
            "assignments[].basis",
            "string",
            false,
            "这项改动为什么成立（来源依据）；update 必填。",
            FilledBy::Agent,
            "update 之外必须缺省。",
        ),
        field(
            "assignments[].proposal",
            "string",
            false,
            "create 引用的 proposals[].id；页面路径由 CLI 分配。",
            FilledBy::Agent,
            "create 之外必须缺省；create 时必须指向真实提案。",
        ),
        field(
            "assignments[].reason",
            "string",
            false,
            "skip 为什么可以不写；不能是“未涉及”这类空话。",
            FilledBy::Agent,
            "skip 时必填非空；其他 action 必须缺省。",
        ),
        field(
            "assignments[].covered_by",
            "string (page reference)",
            false,
            "skip 时说明哪个已有页已经承载这条信息。",
            FilledBy::Agent,
            "缺省表示没有单一已有页覆盖；只允许出现在 skip 上。",
        ),
        field(
            "maintenance",
            "array<object>",
            false,
            "affected_pages 里每个引用页的处置。",
            FilledBy::Agent,
            "affected_pages 为空时可以缺省；否则每个引用页都必须有一行。",
        ),
        field(
            "maintenance[].page",
            "string (page reference)",
            true,
            "必须是 affected_pages 里的仓库相对路径。",
            FilledBy::Agent,
            "不允许空；写 affected_pages 之外的页会被拒。",
        ),
        enum_field(
            "maintenance[].action",
            "string",
            true,
            "这页改还是不改；unchanged 表示看过且决定不改。",
            FilledBy::Agent,
            "不允许空。",
            &["update", "unchanged"],
        ),
        field(
            "maintenance[].reason",
            "string",
            true,
            "为什么 update、为什么 unchanged。",
            FilledBy::Agent,
            "不允许空。",
        ),
        field(
            "proposals",
            "array<object>",
            false,
            "create 需要的临时提案；一个提案只生成一个目标页。",
            FilledBy::Agent,
            "没有 create 时缺省或 []；每个提案都必须被至少一条 create 引用。",
        ),
        field(
            "proposals[].id",
            "string",
            true,
            "只在本次载荷内有效的提案 id。",
            FilledBy::Agent,
            "不允许空。",
        ),
        enum_field(
            "proposals[].type",
            "string",
            true,
            "新页的类型，决定 CLI 把它分配到哪个目录。",
            FilledBy::Agent,
            "不允许空；必须是 [schema].types 之一。",
            types,
        ),
        field(
            "proposals[].title",
            "string",
            true,
            "页面语义标题；CLI 由它派生 slug，不要自己写路径或 id。",
            FilledBy::Agent,
            "不允许空。",
        ),
        field(
            "proposals[].summary",
            "string",
            true,
            "这页要说什么。",
            FilledBy::Agent,
            "不允许空。",
        ),
        field(
            "proposals[].why_new",
            "string",
            true,
            "这条信息为什么值得独立成页。",
            FilledBy::Agent,
            "不允许空。",
        ),
        field(
            "proposals[].why_not_existing",
            "string",
            true,
            "读过的近似候选为什么不能承载这条信息，而不是只写“没有”。",
            FilledBy::Agent,
            "不允许空。",
        ),
        field(
            "searches",
            "array<object>",
            false,
            "实际考虑过的检索，用来记录判断依据。",
            FilledBy::Agent,
            "缺省表示不记录；记录了也不等于证明用过 query。",
        ),
        field(
            "searches[].query",
            "string",
            true,
            "试过的检索词。",
            FilledBy::Agent,
            "不允许空。",
        ),
        field(
            "searches[].candidates",
            "array<string>",
            false,
            "这次检索出现的候选页路径。",
            FilledBy::Agent,
            "缺省或 [] 表示没有候选。",
        ),
        field(
            "searches[].reason",
            "string",
            true,
            "候选为什么合适或不合适。",
            FilledBy::Agent,
            "不允许空。",
        ),
        field(
            "source_reason",
            "string",
            false,
            "没有任何信息点时，这个来源为什么不需要页面。",
            FilledBy::Agent,
            "points 非空时可以缺省；points 为空时必须非空。",
        ),
    ];
    Contract {
        schema: "route-v1".into(),
        fields,
        constraints: vec![
            "每个 points[].id 至少有一条 assignments[]：同一个点可以指向多个页面（CLI 在每个目标里重复这个点），但一条都没有会被拒；assignments[].point 必须指向 points 里的 id，没提到点的会被拒。".into(),
            "assignments[].action=skip 必须给出非空 assignments[].reason，且不得携带 target、change、basis、proposal；covered_by 可选。".into(),
            "assignments[].action=update 必须给出 target、change、basis，且不得携带 proposal、reason、covered_by；target 必须是一张可解析的已有页。".into(),
            "assignments[].action=create 必须给出指向 proposals[].id 的 proposal，且不得携带 target、change、basis、reason、covered_by；页面路径与页面 id 由 CLI 分配。".into(),
            "proposals[].id 在载荷内唯一，proposals[].type 必须在 [schema].types 内；每个提案都必须被至少一条 create 引用，否则会被拒。".into(),
            "maintenance 必须给 affected_pages 的每一页一条 update 或 unchanged；写进 affected_pages 之外的页、或理由为空，都会被拒。".into(),
            "affected_pages 是从共享索引反查出来的“已有引用页”，不是候选全集；候选取自 query，可以是从未引用当前来源的页。".into(),
            "affected_pages 为空时 maintenance 可以缺省，但仍有信息点要处置时不能省。".into(),
            "points 为空时 source_reason 必须非空；有信息点时它可以缺省。".into(),
            "searches 只用于自述；每条都要有非空 query 与 reason，但 CLI 不据此证明 agent 真的检索过。".into(),
            "点 id 与提案 id 只在本次载荷内有效，不是正式页面 id；正式路径由 CLI 在 plan 阶段分配。".into(),
            "载荷里缺省的数组按空数组处理；required=false 只表示允许缺省，不表示可以省掉这些判断。".into(),
        ],
        examples: vec![
            Example {
                title: "update：把两个信息点融进同一张已有页".into(),
                payload: serde_json::json!({
                    "points": [
                        {"id": "p1", "finding": "基线是全局一份，不是每棵树各一份。", "evidence": "设计文档 2.25 节"},
                        {"id": "p2", "finding": "任务外的改动会进 reconcile 任务，不会被静默吸收。", "evidence": "设计文档 2.25 节"}
                    ],
                    "assignments": [
                        {"point": "p1", "action": "update", "target": ".wiki/pages/concept/task-tree.md", "change": "补充基线为什么必须全局一份", "basis": "同一页可能被先收敛的树合并，后收敛的树必须把它当正常状态"},
                        {"point": "p2", "action": "update", "target": ".wiki/pages/concept/task-tree.md", "change": "补充 reconcile 的触发条件", "basis": "advance 开头用全局基线比对 pages/"}
                    ],
                    "maintenance": [
                        {"page": ".wiki/pages/concept/task-tree.md", "action": "update", "reason": "两条新增信息都落在这页"}
                    ],
                    "searches": [
                        {"query": "任务树 基线", "candidates": [".wiki/pages/concept/task-tree.md"], "reason": "这页已经在讲任务树，能承载这两点"}
                    ]
                }),
            },
            Example {
                title: "create：为一条独立结论新建一页".into(),
                payload: serde_json::json!({
                    "points": [
                        {"id": "p1", "finding": "交付示例必须走真实解析器，否则契约会和校验器漂移。", "evidence": "自解释契约一节"}
                    ],
                    "assignments": [{"point": "p1", "action": "create", "proposal": "n1"}],
                    "proposals": [
                        {"id": "n1", "type": "concept", "title": "契约示例测试", "summary": "用真实解析器校验交付示例", "why_new": "这是一条可独立复用的结论，已有页都在讲流程", "why_not_existing": ".wiki/pages/concept/task-tree.md 只讲任务推进，没有契约维护的边界"}
                    ],
                    "maintenance": [],
                    "searches": [
                        {"query": "契约 示例", "candidates": [".wiki/pages/concept/task-tree.md"], "reason": "候选页只讲任务推进，不覆盖示例维护"}
                    ]
                }),
            },
            Example {
                title: "skip：信息点已被别的页承载，不写入".into(),
                payload: serde_json::json!({
                    "points": [
                        {"id": "p1", "finding": "一个来源一棵根任务。", "evidence": "设计文档 2.22 节"}
                    ],
                    "assignments": [
                        {"point": "p1", "action": "skip", "reason": "这句已经在任务树页里说清楚，没有新增结论。", "covered_by": ".wiki/pages/concept/task-tree.md"}
                    ],
                    "maintenance": []
                }),
            },
            Example {
                title: "零写页：来源没有可保留的信息点".into(),
                payload: serde_json::json!({
                    "points": [],
                    "maintenance": [],
                    "source_reason": "这是一条一次性的发布时间通知，没有可复用的结论；affected_pages 为空，不需要写页。"
                }),
            },
        ],
        failure_fixes: vec![
            "E_TASK_BAD_PAYLOAD 且消息点名 point '<id>'：改 assignments[] 里该 point 的行，或改 points[].id，让两边一致；改完可以重交，任务还在。".into(),
            "消息说 point '...' has no assignment：给该 points[].id 补一条 assignments[]；消息说 create refers to proposal '...' not in proposals：补 proposals[] 或改 proposal 引用。".into(),
            "消息说 skip assignment needs a non-empty reason 或 cannot carry target：skip 只保留 point/action/reason（有单一覆盖页可加 covered_by），删掉写入字段后重交。".into(),
            "消息说 update assignment needs a non-empty target/change/basis：补齐这三个字段；update 不能带 proposal/reason/covered_by。".into(),
            "消息说 target '<path>' does not exist：query 后把 assignments[].target 改成仓库里真实存在的页面路径；任务还在，改完可以重交。".into(),
            "消息说 create assignment cannot carry ...：create 只保留 point/action/proposal，页面路径交给 proposals[] 由 CLI 分配。".into(),
            "消息说某页 is an affected page but maintenance has no verdict：补上 update 或 unchanged 加理由；maintenance 里 affected_pages 之外的页一律不要写。".into(),
            "消息说 a route with no points needs a non-empty source_reason：要么补信息点，要么写清这个来源为什么不需要页面。".into(),
            "以上都是载荷完整性与引用问题，改完可以重交；CLI 不判断信息点是否提全、归属是否成立，那些语义责任仍在你身上。".into(),
        ],
    }
}

fn page_contract() -> Contract {
    Contract {
        schema: "page-content-v1".into(),
        fields: vec![
            field(
                "title",
                "string",
                true,
                "页面标题；CLI 用它渲染 frontmatter 的 title。",
                FilledBy::Agent,
                "不允许空白；空白会被拒。",
            ),
            field(
                "summary",
                "string",
                false,
                "页面摘要，供索引与列表显示。",
                FilledBy::Agent,
                "缺省表示空摘要，工具照写。",
            ),
            field(
                "tags",
                "array<string>",
                false,
                "标签，渲染进 frontmatter。",
                FilledBy::Agent,
                "缺省或 [] 表示无标签。",
            ),
            enum_field(
                "status",
                "string",
                false,
                "页面状态。",
                FilledBy::Agent,
                "缺省按 draft 处理。",
                &["draft", "stable", "stale"],
            ),
            field(
                "body",
                "string (markdown)",
                true,
                "融合后的整页正文，不含 frontmatter。",
                FilledBy::Agent,
                "必填；空正文会被拒绝。",
            ),
        ],
        constraints: vec![
            "payload.body 是整页正文：工具不比对正文，片段就是整页覆盖，旧内容会丢。".into(),
            "title 必须非空；空 title 会被拒，重交前先补。".into(),
            "status 只能是 draft、stable、stale；缺省按 draft 处理。".into(),
            "载荷不包含 id、type、sources、updated，也不包含 frontmatter 本身；这些由 CLI 从任务与已有页构造。".into(),
            "pages/ 在任务外被改过（E_PAGE_CONFLICT）时先处理冲突再重交；工具不会替你合并。".into(),
            "工具只做格式回读（渲染后能否重新解析），不判断正文是否融合完整、结论是否正确。".into(),
        ],
        examples: vec![Example {
            title: "完整正文：在已有页上补两段，提交整页".into(),
            payload: serde_json::json!({
                "title": "任务树",
                "summary": "来源、根任务、子叶与全局基线的推进规则",
                "tags": ["ingest", "task"],
                "status": "stable",
                "body": "# 任务树\n\n一个来源一棵根任务：根拆成子叶，子叶逐个交付、逐个关闭；全部关闭后根收尾合并，并清除来源待办。\n\n## 基线\n\n基线是全局一份，不是每棵树各一份：先收敛的树合并出的页，必须被后收敛的树当成正常状态。\n\n## 复核\n\n任务外的改动不会被静默吸收。有来源的页进入 reconcile 任务，由作者裁决接受（accept）还是按来源重新生成（regenerate）。\n"
            }),
        }],
        failure_fixes: vec![
            "E_TASK_BAD_PAYLOAD 且消息说 payload is not valid JSON：按本契约的字段重建 JSON 文件后重交，任务还在。".into(),
            "消息说 payload needs a non-empty title：补 title；它是页面标题，不是正文摘要。".into(),
            "消息说 status '...' is not draft, stable or stale：改成合法值，或删掉该字段（缺省按 draft）。".into(),
            "消息点名某页 does not cite this source 或 not under ...：那是任务目标与载荷不匹配，按 target.reference 重写，不要改任务。".into(),
            "E_PAGE_CONFLICT：pages/ 在任务外被改过；先 read 冲突页并决定保留哪份，再关闭，工具不会替你合并。".into(),
            "消息说 payload body is empty：补整页正文后重交；空白正文不会写进 staging，任务还在。".into(),
            "以上改完都可以重交；CLI 只验格式与引用，不评判正文质量，语义质量仍由你负责。".into(),
        ],
    }
}

fn reconcile_contract() -> Contract {
    Contract {
        schema: "reconcile-v1".into(),
        fields: vec![
            field(
                "page_hash",
                "string",
                true,
                "任务登记时页面的摘要；accept 据此推进基线。",
                FilledBy::Cli,
                "由 CLI 填写，不会为空。",
            ),
            field(
                "raw_keys",
                "array<string>",
                true,
                "页面引用的 raw key；regenerate 逐个重新排队。",
                FilledBy::Cli,
                "不会为空：没有 sources 的页在生成 reconcile 任务前就被吸收，因此这里至少有页面引用的一个 raw。",
            ),
            enum_field(
                "verdict",
                "flag",
                true,
                "这次裁决；作为命令行开关提交，不是 JSON 字段。",
                FilledBy::Agent,
                "不给或同时给都会被拒。",
                &["accept", "regenerate"],
            ),
        ],
        constraints: vec![
            "裁决只能二选一：--accept 或 --regenerate；两者都不给或同时给都会被拒。".into(),
            "accept 把任务登记的 page_hash 写进基线；如果之后页面又被改过，会生成新的 reconcile 任务，接受不等于忽略后续改动。".into(),
            "regenerate 为页面引用的每个 raw 重建来源待办与根任务；一个 raw 都不在账本里时以 E_TASK_BAD_SOURCE 拒绝。".into(),
            "两种裁决都把基线推进到任务登记的字节；裁决后同一处改动不再提示。".into(),
            "页面的改动内容对不对不由 CLI 判断；它只记录裁决与哈希。".into(),
        ],
        examples: vec![
            Example {
                title: "accept：这次改动就是要保留的版本".into(),
                payload: serde_json::json!({"verdict": "accept"}),
            },
            Example {
                title: "regenerate：改动不可信，按来源重建".into(),
                payload: serde_json::json!({"verdict": "regenerate"}),
            },
        ],
        failure_fixes: vec![
            "消息说 a reconcile task needs exactly one of --accept or --regenerate：只给一个开关后重交，任务还在。".into(),
            "消息说页面 has no target / has no payload：任务行损坏，用 task status 确认后交给用户，不要自己改数据库。".into(),
            "regenerate 报 E_TASK_BAD_SOURCE：页面引用的 raw 已不在账本里；需要人决定删页还是补来源，重复提交绕不过去。".into(),
            "同一页反复出现 reconcile：说明裁决后页面又被改过；确认以哪一版为准再裁决，不要用 accept 追着自己改。".into(),
            "这些检查只覆盖引用与哈希，不判断改动内容对不对；裁决对错由你负责。".into(),
        ],
    }
}

fn uncovered_contract(kind: &str) -> Contract {
    Contract {
        schema: kind.to_string(),
        fields: Vec::new(),
        constraints: vec![format!(
            "没有为 {kind} 定义提交契约：不要自己构造载荷，按 task status 确认当前任务的交付方式与状态。"
        )],
        examples: Vec::new(),
        failure_fixes: vec![
            "如果收到这个 schema，说明任务类型与交付不匹配；用 task status 看当前任务，不要自己构造载荷。".into(),
        ],
    }
}

fn field(
    name: &str,
    typ: &str,
    required: bool,
    meaning: &str,
    filled_by: FilledBy,
    empty: &str,
) -> Field {
    Field {
        name: name.to_string(),
        typ: typ.to_string(),
        required,
        meaning: meaning.to_string(),
        filled_by,
        empty: empty.to_string(),
        allowed: Vec::new(),
    }
}

fn enum_field<S: AsRef<str>>(
    name: &str,
    typ: &str,
    required: bool,
    meaning: &str,
    filled_by: FilledBy,
    empty: &str,
    allowed: &[S],
) -> Field {
    Field {
        allowed: allowed
            .iter()
            .map(|value| value.as_ref().to_string())
            .collect(),
        ..field(name, typ, required, meaning, filled_by, empty)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::route;
    use super::super::LeafPayload;
    use super::*;

    #[test]
    fn examples_run_through_the_real_parsers() {
        let route = contract("route-v1", &["concept".into()]);
        // One fixed affected list per example, in example order. Deriving it
        // from the example's own `maintenance` would make the validator's
        // "affected page has no verdict" and "maintenance names a page outside
        // affected_pages" branches unreachable.
        let affected_by_example: [&[&str]; 4] =
            [&[".wiki/pages/concept/task-tree.md"], &[], &[], &[]];
        assert_eq!(
            route.examples.len(),
            affected_by_example.len(),
            "each route example needs its own fixed affected list"
        );
        let mut actions = BTreeSet::new();
        let mut zero_page = false;
        for (example, affected) in route.examples.iter().zip(affected_by_example) {
            let bytes = serde_json::to_vec(&example.payload).unwrap();
            let doc = route::parse(&bytes)
                .unwrap_or_else(|err| panic!("{} does not parse: {err}", example.title));
            let affected: Vec<String> = affected.iter().map(|page| page.to_string()).collect();
            route::validate_shape(&doc, &affected, &["concept".to_string()])
                .unwrap_or_else(|err| panic!("{} does not validate: {err}", example.title));
            // The validator already proves both directions; state the agreement
            // here too so a wrong fixture names the example it belongs to.
            let verdicted: BTreeSet<&str> = doc
                .maintenance
                .iter()
                .map(|item| item.page.as_str())
                .collect();
            assert_eq!(
                verdicted,
                affected.iter().map(String::as_str).collect::<BTreeSet<_>>(),
                "{} and its affected pages disagree",
                example.title
            );
            for assignment in &doc.assignments {
                actions.insert(assignment.action.clone());
            }
            zero_page |= doc.points.is_empty();
        }
        for action in ["update", "create", "skip"] {
            assert!(actions.contains(action), "no route example uses {action}");
        }
        assert!(zero_page, "no route example shows a zero-page route");

        let page = contract("page-content-v1", &["concept".into()]);
        for example in &page.examples {
            let bytes = serde_json::to_vec(&example.payload).unwrap();
            let doc: LeafPayload = serde_json::from_slice(&bytes)
                .unwrap_or_else(|err| panic!("{} does not parse: {err}", example.title));
            assert!(
                !doc.title.trim().is_empty(),
                "{} has no title",
                example.title
            );
            assert!(
                !doc.body.trim().is_empty(),
                "{} shows a snippet instead of a full body",
                example.title
            );
            assert!(doc
                .status
                .as_deref()
                .is_none_or(|status| { ["draft", "stable", "stale"].contains(&status) }));
        }
    }

    #[test]
    fn the_route_contract_names_every_top_level_field() {
        let c = contract("route-v1", &["concept".into()]);
        for name in [
            "points",
            "assignments",
            "maintenance",
            "proposals",
            "searches",
            "source_reason",
        ] {
            assert!(c.fields.iter().any(|f| f.name == name), "missing {name}");
        }
    }

    #[test]
    fn the_route_contract_uses_dot_paths_for_nested_fields() {
        let c = contract("route-v1", &["concept".into()]);
        for name in [
            "points[].id",
            "points[].finding",
            "points[].evidence",
            "points[].boundary",
            "assignments[].point",
            "assignments[].action",
            "assignments[].covered_by",
            "maintenance[].page",
            "maintenance[].action",
            "proposals[].type",
            "proposals[].why_not_existing",
            "searches[].candidates",
        ] {
            assert!(c.fields.iter().any(|f| f.name == name), "missing {name}");
        }
    }

    #[test]
    fn the_proposal_type_list_comes_from_the_caller() {
        let c = contract("route-v1", &["concept".into(), "entity".into()]);
        let field = c
            .fields
            .iter()
            .find(|f| f.name == "proposals[].type")
            .expect("proposals[].type is documented");
        assert_eq!(field.allowed, vec!["concept", "entity"]);
        let action = c
            .fields
            .iter()
            .find(|f| f.name == "assignments[].action")
            .expect("assignments[].action is documented");
        assert_eq!(action.allowed, vec!["update", "create", "skip"]);
        // The enum is serialized the way the CLI prints it.
        assert_eq!(
            serde_json::to_string(&FilledBy::Agent).unwrap(),
            "\"agent\""
        );
        assert_eq!(serde_json::to_string(&FilledBy::Cli).unwrap(), "\"cli\"");
    }

    #[test]
    fn the_route_contract_states_the_action_specific_rules() {
        let c = contract("route-v1", &["concept".into()]);
        let text = c.constraints.join("\n");
        assert!(
            text.contains("skip") && text.contains("reason"),
            "skip's required reason is not a stated constraint"
        );
        assert!(
            text.contains("update") && text.contains("不得携带 proposal、reason、covered_by"),
            "update's rejected fields are not stated"
        );
        assert!(
            text.contains("create")
                && text.contains("不得携带 target、change、basis、reason、covered_by"),
            "create's rejected fields are not stated"
        );
        for rule in [
            "source_reason",
            "affected_pages",
            "proposals[]",
            "maintenance",
        ] {
            assert!(text.contains(rule), "constraints never mention {rule}");
        }
        assert!(
            text.contains("至少有一条"),
            "constraints still demand exactly one assignment per point"
        );
        assert!(
            !text.contains("有且只有"),
            "constraints contradict the validator's at-least-one rule"
        );
        let fixes = c.failure_fixes.join("\n");
        assert!(
            fixes.contains("does not exist") && fixes.contains("query"),
            "the missing update target has no recovery instruction"
        );
    }

    #[test]
    fn the_page_contract_names_every_payload_field() {
        let c = contract("page-content-v1", &["concept".into()]);
        for name in ["title", "summary", "tags", "status", "body"] {
            assert!(c.fields.iter().any(|f| f.name == name), "missing {name}");
        }
        let status = c
            .fields
            .iter()
            .find(|f| f.name == "status")
            .expect("status is documented");
        assert_eq!(status.allowed, vec!["draft", "stable", "stale"]);
        let body = c
            .fields
            .iter()
            .find(|f| f.name == "body")
            .expect("body is documented");
        assert!(body.required, "body must stay required");
        assert!(
            body.empty.contains("必填"),
            "body's empty text does not say it is required"
        );
    }

    #[test]
    fn the_reconcile_contract_covers_both_verdicts() {
        let c = contract("reconcile-v1", &["concept".into()]);
        let verdicts: Vec<&str> = c
            .examples
            .iter()
            .filter_map(|example| example.payload.get("verdict"))
            .filter_map(|value| value.as_str())
            .collect();
        assert!(verdicts.contains(&"accept"), "no accept example");
        assert!(verdicts.contains(&"regenerate"), "no regenerate example");
        for name in ["page_hash", "raw_keys"] {
            let field = c
                .fields
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(
                field.filled_by,
                FilledBy::Cli,
                "{name} is filled by the tool, not the agent"
            );
        }
        assert!(
            c.fields.iter().all(|f| f.name != "target"),
            "target is task context, not a ReconcilePayload field"
        );
        let raw_keys = c
            .fields
            .iter()
            .find(|f| f.name == "raw_keys")
            .expect("raw_keys is documented");
        assert!(
            raw_keys.empty.contains("不会为空"),
            "raw_keys describes an empty array that a reconcile task cannot have"
        );
        let verdict = c
            .fields
            .iter()
            .find(|f| f.name == "verdict")
            .expect("verdict is documented");
        assert_eq!(verdict.allowed, vec!["accept", "regenerate"]);
    }

    #[test]
    fn the_route_guide_names_the_read_order_and_the_hard_stops() {
        let g = guide("route");
        assert!(
            g.goal.contains("去向"),
            "the goal does not name the decision"
        );
        let read = g.must_read.join("\n");
        assert!(read.contains("source.read_ref"), "read order omits the raw");
        assert!(
            read.contains("affected_pages"),
            "read order omits the cited pages"
        );
        assert!(
            read.contains("query --page") && read.contains("query --source"),
            "the guide stacks --page/--source on a keyword query, which clap rejects"
        );
        let forbidden = g.forbidden.join("\n");
        assert!(
            forbidden.contains("不得直接创建"),
            "the guide does not forbid writing pages directly"
        );
        assert!(
            forbidden.contains("路径"),
            "the guide does not forbid path-only dedup"
        );
        assert_eq!(g.done_when.len(), 4);
        assert!(
            g.done_when.iter().any(|item| item.contains("至少有一条")),
            "done_when still demands exactly one assignment per point"
        );
        assert!(
            g.done_when.iter().all(|item| !item.contains("有且只有")),
            "done_when contradicts the validator's at-least-one rule"
        );
        assert!(
            g.done_when
                .iter()
                .any(|item| item.contains("source_reason")),
            "the empty-route condition is not in done_when"
        );
        assert!(
            !g.cli_can_check.is_empty() && !g.agent_owns.is_empty(),
            "the guide does not separate what the CLI checks from what the agent owns"
        );
        assert!(
            g.agent_owns.iter().any(|item| item.contains("无法证明")),
            "the guide does not admit what the CLI cannot prove"
        );
        assert!(
            g.human_escalation.contains("不要提交"),
            "the guide does not keep the route open for a human decision"
        );
    }

    #[test]
    fn the_page_guide_requires_the_whole_body() {
        let g = guide("page");
        let read = g.must_read.join("\n");
        assert!(
            read.contains("完整正文"),
            "the page guide does not require reading the page"
        );
        let forbidden = g.forbidden.join("\n");
        assert!(
            forbidden.contains("局部片段"),
            "the page guide does not forbid snippets"
        );
        let done = g.done_when.join("\n");
        assert!(
            done.contains("完整正文"),
            "the page guide does not require submitting the fused body"
        );
        assert!(
            g.agent_owns.iter().any(|item| item.contains("语义")),
            "the page guide does not say semantics stay with the agent"
        );
    }

    #[test]
    fn the_reconcile_guide_names_both_verdicts() {
        let g = guide("reconcile");
        let text = [
            g.goal.clone(),
            g.must_read.join("\n"),
            g.must_decide.join("\n"),
            g.done_when.join("\n"),
            g.cli_can_check.join("\n"),
        ]
        .join("\n");
        assert!(text.contains("accept"), "the reconcile guide omits accept");
        assert!(
            text.contains("regenerate"),
            "the reconcile guide omits regenerate"
        );
        assert!(
            g.forbidden.iter().any(|item| item.contains("不关闭")),
            "the guide lets a reconcile task sit open"
        );
        let read = g.must_read.join("\n");
        assert!(
            read.contains("逐条比对"),
            "the guide does not say what to compare the body against"
        );
        assert!(
            !read.contains("看清改了什么"),
            "the guide asks for page history the tool does not keep"
        );
    }

    #[test]
    fn failure_fixes_say_what_to_change_and_whether_to_resubmit() {
        // The positive wording matters: a schema must say out loud that the
        // judgement stays with the agent, not merely avoid one bad phrase.
        let owned = [
            ("route-v1", "语义责任仍在你身上"),
            ("page-content-v1", "语义质量仍由你负责"),
            ("reconcile-v1", "裁决对错由你负责"),
        ];
        for (kind, agent_owns) in owned {
            let c = contract(kind, &["concept".into()]);
            assert!(!c.failure_fixes.is_empty(), "{kind} has no failure fixes");
            let text = c.failure_fixes.join("\n");
            assert!(
                text.contains("重交"),
                "{kind} does not say whether to resubmit"
            );
            assert!(
                text.contains(agent_owns),
                "{kind} does not say in positive terms that the semantic judgement stays with the agent"
            );
            assert!(
                !text.contains("语义质量已"),
                "{kind} claims a semantic check that never ran"
            );
        }
    }

    #[test]
    fn an_unknown_kind_gets_an_honest_empty_contract() {
        let convert = guide("convert");
        assert!(
            convert.goal.contains("工具"),
            "convert must say the tool advances it"
        );
        let breakdown = guide("breakdown");
        assert!(
            !breakdown.goal.contains("由工具推进"),
            "breakdown is closed with --leaves, so it is not tool-advanced"
        );
        assert!(
            breakdown.goal.contains("没有定义") && breakdown.goal.contains("task status"),
            "an uncovered kind must say no contract is defined and point at task status"
        );
        assert!(
            breakdown
                .forbidden
                .iter()
                .all(|item| !item.contains("--leaves")),
            "the fallback still claims breakdown cannot be closed with --leaves"
        );
        let c = contract("mystery-v1", &["concept".into()]);
        assert!(c.fields.is_empty());
        assert!(c.examples.is_empty());
        assert!(
            c.constraints.iter().any(|rule| rule.contains("mystery-v1")),
            "the contract does not name the schema it has no rules for"
        );
        assert!(
            !c.constraints.join("\n").contains("E_TASK_BAD_PAYLOAD"),
            "the unknown-schema fallback promises a rejection it cannot know"
        );
    }
}
