//! One parser for the two kinds of reference a page carries.
//!
//! Pages declare relations in their body (`[[concept/orders]]`) and their
//! frontmatter (`sources:`). Three consumers used to parse those independently —
//! `query` for search hits, `lint` for dead links, `graph` for the graph view —
//! and they disagreed at the edges (bare ids, `.md` suffixes, code fences).
//! Everything now goes through this module, so the index, the linter and the
//! graph cannot drift apart.
//!
//! This is deliberately *syntactic*: it says what a reference points at, not
//! whether the author was right to write it. Whether a relation should exist is
//! the agent's judgement; resolving the literal is the program's job.

use std::collections::BTreeMap;

/// What a link literal resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Exactly one page has this name.
    Resolved,
    /// No page has this name.
    Missing,
    /// More than one page has this name; a bare id cannot pick one.
    Ambiguous,
}

impl Resolution {
    pub fn as_str(&self) -> &'static str {
        match self {
            Resolution::Resolved => "resolved",
            Resolution::Missing => "missing",
            Resolution::Ambiguous => "ambiguous",
        }
    }
}

/// One page as the resolver sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageIdentity {
    /// Repository-relative key, e.g. `.wiki/pages/concept/orders.md`.
    pub key: String,
    /// Frontmatter `id`.
    pub id: String,
    /// Path under the wiki directory without `.md`, e.g. `concept/orders`.
    pub wiki_rel: String,
}

/// Resolves link literals against a fixed set of pages.
///
/// Built once per refresh from the page list; rebuilding it is cheap and is the
/// only way the alias table stays correct after a page is renamed.
#[derive(Debug, Default)]
pub struct Resolver {
    /// alias -> Some(key) when unique, None when two pages share the alias.
    aliases: BTreeMap<String, Option<String>>,
}

impl Resolver {
    pub fn new(pages: &[PageIdentity]) -> Resolver {
        let mut resolver = Resolver::default();
        for page in pages {
            resolver.insert(&page.key, &page.key);
            resolver.insert(page.key.trim_end_matches(".md"), &page.key);
            resolver.insert(&page.wiki_rel, &page.key);
            resolver.insert(page.wiki_rel.trim_end_matches(".md"), &page.key);
            resolver.insert(&page.id, &page.key);
        }
        resolver
    }

    fn insert(&mut self, alias: &str, key: &str) {
        let alias = alias.trim();
        if alias.is_empty() {
            return;
        }
        match self.aliases.get(alias) {
            Some(Some(existing)) if existing != key => {
                self.aliases.insert(String::from(alias), None);
            }
            Some(_) => {}
            None => {
                self.aliases
                    .insert(String::from(alias), Some(String::from(key)));
            }
        }
    }

    /// Resolves one literal; returns the status and the page key when unique.
    pub fn resolve(&self, literal: &str, wiki_prefix: &str) -> (Resolution, Option<String>) {
        let wanted = normalise_target(literal, wiki_prefix);
        match self.aliases.get(&wanted) {
            Some(Some(key)) => (Resolution::Resolved, Some(key.clone())),
            Some(None) => (Resolution::Ambiguous, None),
            None => (Resolution::Missing, None),
        }
    }
}

/// A link literal reduced to the name the resolver indexes.
pub fn normalise_target(target: &str, wiki_prefix: &str) -> String {
    let mut text = target
        .split_once('#')
        .map_or(target, |(head, _)| head)
        .trim();
    loop {
        let stripped = text.strip_prefix("./").unwrap_or(text);
        if stripped == text {
            break;
        }
        text = stripped;
    }
    let prefix = wiki_prefix.trim_start_matches("./").trim_end_matches('/');
    if !prefix.is_empty() {
        if let Some(rest) = text.strip_prefix(prefix) {
            if let Some(rest) = rest.strip_prefix('/') {
                text = rest;
            }
        }
    }
    text.trim_end_matches(".md").to_string()
}

/// `[[target]]` values in `body`, in source order and with duplicates kept.
///
/// Fenced code blocks are skipped: an example link in documentation is not a
/// claim about this wiki.
pub fn extract_links(body: &str) -> Vec<String> {
    let mut text = String::with_capacity(body.len());
    let mut fence: Option<&'static str> = None;
    for line in body.lines() {
        let trimmed = line.trim_start();
        match fence {
            Some(marker) => {
                if trimmed.starts_with(marker) {
                    fence = None;
                }
                continue;
            }
            None => {
                if trimmed.starts_with("```") {
                    fence = Some("```");
                    continue;
                }
                if trimmed.starts_with("~~~") {
                    fence = Some("~~~");
                    continue;
                }
            }
        }
        text.push_str(line);
        text.push('\n');
    }

    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else {
            break;
        };
        let target = after[..end].trim();
        if !target.is_empty() {
            out.push(String::from(target));
        }
        rest = &after[end + 2..];
    }
    out
}

/// A frontmatter `sources:` entry split into its addressable parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceParts {
    /// The entry as written, fragment included.
    pub raw_ref: String,
    /// Raw file path, `./` stripped and fragment removed.
    pub raw_key: String,
    /// Everything after `#`, when non-empty.
    pub fragment: Option<String>,
}

/// Splits `.wiki/raw/a.md#p=12-40`; returns `None` for an empty entry.
pub fn split_source_ref(source: &str) -> Option<SourceParts> {
    let raw_ref = source.trim();
    if raw_ref.is_empty() {
        return None;
    }
    let (raw, fragment) = match raw_ref.split_once('#') {
        Some((raw, fragment)) => {
            let fragment = fragment.trim();
            (
                raw.trim(),
                if fragment.is_empty() {
                    None
                } else {
                    Some(String::from(fragment))
                },
            )
        }
        None => (raw_ref, None),
    };
    let mut raw_key = raw;
    loop {
        let stripped = raw_key.strip_prefix("./").unwrap_or(raw_key);
        if stripped == raw_key {
            break;
        }
        raw_key = stripped;
    }
    if raw_key.is_empty() {
        return None;
    }
    Some(SourceParts {
        raw_ref: String::from(raw_ref),
        raw_key: String::from(raw_key),
        fragment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(key: &str, id: &str, wiki_rel: &str) -> PageIdentity {
        PageIdentity {
            key: String::from(key),
            id: String::from(id),
            wiki_rel: String::from(wiki_rel),
        }
    }

    #[test]
    fn fences_and_md_suffix_do_not_form_edges() {
        let body = "见 [[concept/b]] 与 [[b.md]]\n```\n[[in-fence]]\n```\n~~~\n[[also]]\n~~~\n";
        assert_eq!(extract_links(body), vec!["concept/b", "b.md"]);
    }

    #[test]
    fn a_bare_id_shared_by_two_types_is_ambiguous() {
        let pages = vec![
            page(".wiki/pages/concept/b.md", "b", "concept/b"),
            page(".wiki/pages/entity/b.md", "b", "entity/b"),
        ];
        let resolver = Resolver::new(&pages);
        assert_eq!(
            resolver.resolve("b", ".wiki/pages"),
            (Resolution::Ambiguous, None)
        );
        assert_eq!(
            resolver.resolve("concept/b", ".wiki/pages"),
            (
                Resolution::Resolved,
                Some(String::from(".wiki/pages/concept/b.md"))
            )
        );
    }

    #[test]
    fn every_spelling_of_one_page_resolves_to_it() {
        let pages = vec![page(".wiki/pages/concept/b.md", "b", "concept/b")];
        let resolver = Resolver::new(&pages);
        for literal in [
            "b",
            "concept/b",
            "concept/b.md",
            ".wiki/pages/concept/b",
            ".wiki/pages/concept/b.md",
            "./concept/b",
        ] {
            assert_eq!(
                resolver.resolve(literal, ".wiki/pages").0,
                Resolution::Resolved,
                "{literal}"
            );
        }
        assert_eq!(
            resolver.resolve("nope", ".wiki/pages"),
            (Resolution::Missing, None)
        );
    }

    #[test]
    fn source_ref_splits_fragment_and_raw_key() {
        let parts = split_source_ref("./.wiki/raw/a.md#p=12-40").unwrap();
        assert_eq!(parts.raw_key, ".wiki/raw/a.md");
        assert_eq!(parts.fragment.as_deref(), Some("p=12-40"));
        assert_eq!(parts.raw_ref, "./.wiki/raw/a.md#p=12-40");

        let parts = split_source_ref(".wiki/raw/a.md").unwrap();
        assert_eq!(parts.fragment, None);
        assert_eq!(split_source_ref("  "), None);
    }

    #[test]
    fn a_fence_closes_only_on_its_own_marker() {
        let body = "```\n[[no]]\n~~~\n[[no]]\n```\n[[yes]]\n";
        assert_eq!(extract_links(body), vec!["yes"]);
    }
}
