//! Per-mode invocation facts for the shared output context.
//!
//! Every builder here is fed facts the command already holds: no database
//! access, no inference from serialized keys. Summaries state scope and what
//! the report cannot prove; continuations are commands this build can run.

use llm_wiki::output_context::{shell_quote, Context, Continuation};

fn help(cmd: &str) -> String {
    format!("llm-wiki {cmd} --help")
}

fn action(purpose: &str, command: String) -> Continuation {
    Continuation {
        purpose: purpose.to_string(),
        command,
    }
}

pub fn ignore_list(count: usize) -> Context {
    Context::new(
        "ignore.list",
        &help("ignore"),
        format!(
            "This lists {count} ordered ignore rule(s) only; it does not prove any source or page was cleaned up."
        ),
    )
}

pub fn ignore_change(added: usize, removed: usize, count: usize) -> Context {
    Context::new(
        "ignore.change",
        &help("ignore"),
        format!(
            "Rule list changed: {added} added, {removed} removed, {count} rule(s) now. This command changed rules only; no source or page was deleted."
        ),
    )
}

pub fn ignore_check(key: &str, ignored: bool, pattern: Option<&str>) -> Context {
    let summary = match (ignored, pattern) {
        (true, Some(pattern)) => format!("`{key}` is ignored; the deciding rule is `{pattern}`."),
        (false, Some(pattern)) => format!(
            "`{key}` is not ignored; the deciding rule is the `!` negation `{pattern}`, which brings it back."
        ),
        (false, None) => format!("`{key}` is not ignored; no rule matches it."),
        (true, None) => format!(
            "`{key}` is ignored, but no deciding rule was found; this answer is inconsistent and should be reported."
        ),
    };
    Context::new("ignore.check", &help("ignore"), summary)
}

pub fn doctor(ok: usize, warn: usize, fail: usize, fix: bool, scope: &str) -> Context {
    let summary = format!(
        "Doctor ran {scope}: {ok} ok, {warn} warn, {fail} fail. Warnings do not fail the run; --fix repairs mechanical defects only, not page-content correctness."
    );
    let mut context = Context::new("doctor", &help("doctor"), summary);
    if fix {
        context.continuations.push(action(
            "re-check after the repair",
            String::from("llm-wiki doctor"),
        ));
    }
    context
}

pub fn read_window(
    reference: &str,
    offset: usize,
    end: usize,
    total: usize,
    truncated: bool,
    limit: usize,
) -> Context {
    let summary = if total == 0 {
        format!("Read `{reference}`: line window 0/0; the file is empty.")
    } else if offset > total {
        format!(
            "Read `{reference}`: line window is empty because offset {offset} is beyond the last line ({total}); this is not a read failure."
        )
    } else {
        format!(
            "Read `{reference}`: line window {offset}-{end} of {total}{}.",
            if truncated {
                "; more lines remain"
            } else {
                "; this window reaches the end of the file"
            }
        )
    };
    let mut context = Context::new("read", &help("read"), summary);
    if truncated {
        context.continuations.push(action(
            "read the next lines",
            format!(
                "llm-wiki read {} --offset {} --limit {limit}",
                shell_quote(reference),
                end + 1
            ),
        ));
    }
    context
}

pub fn query_text(count: usize, limit: usize, scope: &str) -> Context {
    Context::new(
        "query.text",
        &help("query"),
        format!(
            "Text search over indexed pages (scope {scope}): {count} hit(s) returned, limit {limit}. count is the returned number, not the total number of matches; snippets, summaries and scores are not evidence of correctness."
        ),
    )
}

/// One page/source group, copied verbatim from the report so the summary
/// cannot drift from the payload.
#[derive(Debug, Clone, Default)]
pub struct GroupFacts {
    pub requested: bool,
    pub total: usize,
    pub returned: usize,
    pub next_offset: Option<usize>,
    pub next_command: Option<String>,
}

impl GroupFacts {
    pub fn page(group: &llm_wiki::query::PageGroup) -> Self {
        Self {
            requested: group.requested,
            total: group.total,
            returned: group.items.len(),
            next_offset: group.next_offset,
            next_command: group.next_command.clone(),
        }
    }

    pub fn sources(group: &llm_wiki::query::SourceGroup) -> Self {
        Self {
            requested: group.requested,
            total: group.total,
            returned: group.items.len(),
            next_offset: None,
            next_command: None,
        }
    }

    fn describe(&self, label: &str) -> String {
        if !self.requested {
            return format!("{label} not requested");
        }
        if self.total == 0 {
            return format!("{label} queried and empty");
        }
        match self.next_offset {
            Some(next) => format!(
                "{label} returned {} of {}, next offset {next}",
                self.returned, self.total
            ),
            None => format!("{label} returned {} of {}", self.returned, self.total),
        }
    }
}

pub fn query_page(
    page: &str,
    outgoing: &GroupFacts,
    incoming: &GroupFacts,
    sources: &GroupFacts,
    incomplete: bool,
) -> Context {
    let mut summary = format!(
        "Exact page `{page}`: {}, {}, {}. Links are frontmatter/wikilink literals, not support or causality; unresolved links are listed but never paginated. sources are frontmatter references and are not paginated.",
        outgoing.describe("outgoing"),
        incoming.describe("incoming"),
        sources.describe("sources")
    );
    if incomplete {
        summary.push_str(" The index for this page is incomplete.");
    }
    let mut context = Context::new("query.page", &help("query"), summary);
    for (label, group) in [("outgoing", outgoing), ("incoming", incoming)] {
        if let Some(command) = &group.next_command {
            context.continuations.push(action(
                &format!("read the next page of {label} links"),
                command.clone(),
            ));
        }
    }
    context
}

pub fn query_source(raw_key: &str, available: bool, citing: &GroupFacts) -> Context {
    let summary = format!(
        "Reverse lookup for raw `{raw_key}` ({}): {}. The lookup covers the whole raw file, not one fragment; presence does not prove the file is readable or that the pages support it.",
        if available { "present" } else { "missing" },
        citing.describe("citing pages")
    );
    let mut context = Context::new("query.source", &help("query"), summary);
    if let Some(command) = &citing.next_command {
        context.continuations.push(action(
            "read the next page of citing pages",
            command.clone(),
        ));
    }
    context
}

pub fn query_graph(nodes: usize, orphans: usize, broken: usize) -> Context {
    Context::new(
        "query.graph",
        &help("query"),
        format!(
            "Whole-index wikilink structure: {nodes} page node(s), {orphans} orphan(s), {broken} broken link(s). This is the stored link graph, not an image search, a semantic graph, or a complete evidence network."
        ),
    )
}

/// Facts one ingest round reports, already reduced to what the summary states.
pub struct IngestFacts<'a> {
    pub mode: &'a str,
    pub converted: usize,
    pub queued: usize,
    pub waiting: usize,
    pub failures: usize,
    pub merged_pages: usize,
    pub confirmed_sources: usize,
    pub removed_pages: usize,
    pub actionable: usize,
    pub failed: usize,
    pub blocked: usize,
    pub has_task: bool,
}

pub fn ingest(facts: &IngestFacts<'_>) -> Context {
    let mut summary = if facts.mode == "dry_run" {
        String::from(
            "Dry run: this round applies no conversion or page merge; it previews the effects a real round would have. Opening the repository may already have created or repaired engine files before the preview.",
        )
    } else {
        String::from("One ingest round: this is not proof that the whole repository is complete.")
    };
    summary.push_str(&format!(
        " converted {}, queued {}, waiting {}; merged pages {}, confirmed sources {}, removed pages {}; {} failure(s), {} actionable, {} failed, {} blocked task(s); next task: {}.",
        facts.converted,
        facts.queued,
        facts.waiting,
        facts.merged_pages,
        facts.confirmed_sources,
        facts.removed_pages,
        facts.failures,
        facts.actionable,
        facts.failed,
        facts.blocked,
        if facts.has_task {
            "delivered"
        } else {
            "none"
        }
    ));
    let mut context = Context::new(
        if facts.mode == "dry_run" {
            "ingest.dry_run"
        } else {
            "ingest.round"
        },
        &help("ingest"),
        summary,
    );
    context.continuations.push(action(
        "inspect the whole task tree",
        String::from("llm-wiki task status"),
    ));
    context
}

pub fn preview_head(
    key: &str,
    backend: &str,
    total_chars: usize,
    returned_chars: usize,
    truncated: bool,
    chunks: usize,
) -> Context {
    let summary = if truncated {
        format!(
            "Parsed `{key}` with the {backend} backend: {total_chars} characters total, {returned_chars} returned in markdown_head (truncated). This is a head, not the full parsed text."
        )
    } else {
        format!(
            "Parsed `{key}` with the {backend} backend: {total_chars} characters total, all {returned_chars} returned in markdown_head."
        )
    };
    let mut context = Context::new("ingest.preview_head", &help("ingest"), summary);
    if truncated && chunks > 0 {
        context.continuations.push(action(
            "read the whole parsed text in chunks",
            format!(
                "llm-wiki ingest --key {} --range 1-{chunks}",
                shell_quote(key)
            ),
        ));
    }
    context
}

pub fn preview_chunked(
    key: &str,
    backend: &str,
    total_chars: usize,
    chunks: usize,
    chunks_dir: &str,
) -> Context {
    Context::new(
        "ingest.preview_chunked",
        &help("ingest"),
        format!(
            "Parsed `{key}` with the {backend} backend: {total_chars} characters, materialized as {chunks} chunk file(s) under `{chunks_dir}`. This preview writes that chunk cache; chunk numbers are 1-based."
        ),
    )
}

pub fn preview_chunk(
    key: &str,
    backend: &str,
    chunk: usize,
    chunks: usize,
    returned_chars: usize,
    total_chars: usize,
) -> Context {
    Context::new(
        "ingest.preview_chunk",
        &help("ingest"),
        format!(
            "Parsed `{key}` with the {backend} backend: chunk {chunk} of {chunks}, {returned_chars} of {total_chars} characters. Chunk numbers are 1-based; this is one window, not the whole document."
        ),
    )
}

pub fn preview_range(
    key: &str,
    backend: &str,
    from: usize,
    to: usize,
    chunks: usize,
    returned_chars: usize,
    total_chars: usize,
) -> Context {
    Context::new(
        "ingest.preview_range",
        &help("ingest"),
        format!(
            "Parsed `{key}` with the {backend} backend: chunks {from}-{to} of {chunks} (1-based inclusive), {returned_chars} of {total_chars} characters. This is one window, not the whole document."
        ),
    )
}

pub struct DoneFacts<'a> {
    pub closed: i64,
    pub outcome: &'a str,
    pub merged: usize,
    pub confirmed: usize,
    pub removed: usize,
    pub failed: usize,
    pub blocked: usize,
    pub next: Option<i64>,
}

pub fn task_done(facts: &DoneFacts) -> Context {
    let DoneFacts {
        closed,
        outcome,
        merged,
        confirmed,
        removed,
        failed,
        blocked,
        next,
    } = *facts;
    let summary = format!(
        "Closed task {closed} with outcome `{outcome}`: {merged} merged page(s), {confirmed} settled source(s), {removed} removed page(s); {failed} failed and {blocked} blocked task(s) remain; next task: {}.",
        match next {
            Some(id) => format!("{id}"),
            None => String::from("none"),
        }
    );
    let mut context = Context::new("task.done", &help("task done"), summary);
    context.continuations.push(action(
        "inspect the whole task tree",
        String::from("llm-wiki task status"),
    ));
    context
}

pub fn task_status(roots: usize, current: Option<i64>, failed: usize, blocked: usize) -> Context {
    Context::new(
        "task.status",
        &help("task status"),
        format!(
            "Task tree observed without advancing it: {roots} source root(s), current task {}, {failed} failed, {blocked} blocked. No current task does not prove there is no failed or blocked work.",
            current.map_or_else(|| String::from("none"), |id| format!("{id}"))
        ),
    )
}

pub fn task_retry(kind: &str, id: i64, status: &str) -> Context {
    Context::new(
        "task.retry",
        &help("task retry"),
        format!(
            "Retry returned the previous row for task {id} ({kind}, {status}). This is a human recovery action, not re-execution: it does not prove the failure cause is fixed, and for a convert task the returned old row may already be gone."
        ),
    )
}

pub fn task_replan(id: i64, status: &str) -> Context {
    Context::new(
        "task.replan",
        &help("task retry"),
        format!(
            "Replanned root task {id} ({status}). The route was rebuilt; pages already merged were not reverted, and no work was executed."
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_done_names_partial_settlement_and_the_next_task() {
        let context = task_done(&DoneFacts {
            closed: 7,
            outcome: "partial",
            merged: 2,
            confirmed: 0,
            removed: 1,
            failed: 1,
            blocked: 0,
            next: None,
        });
        assert!(context.summary.contains("`partial`"), "{}", context.summary);
        assert!(context.summary.contains("2 merged"), "{}", context.summary);
        assert!(context.summary.contains("0 settled"), "{}", context.summary);
        assert!(
            context.summary.contains("next task: none"),
            "{}",
            context.summary
        );
    }

    #[test]
    fn an_unrequested_group_is_not_described_as_empty() {
        let absent = GroupFacts::default();
        assert_eq!(absent.describe("incoming"), "incoming not requested");
        let empty = GroupFacts {
            requested: true,
            ..GroupFacts::default()
        };
        assert_eq!(empty.describe("incoming"), "incoming queried and empty");
        let paged = GroupFacts {
            requested: true,
            total: 25,
            returned: 20,
            next_offset: Some(20),
            next_command: None,
        };
        assert_eq!(
            paged.describe("outgoing"),
            "outgoing returned 20 of 25, next offset 20"
        );
    }
}
