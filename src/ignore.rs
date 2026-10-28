//! `.wiki/ignore`: repository paths the tool must never read.
//!
//! The syntax follows `.gitignore`: a blank line or a leading `#` is a comment,
//! a leading `!` re-includes, a trailing `/` matches directories only, a pattern
//! containing `/` is anchored at the repository root, and `*`, `?`, `**` keep
//! their usual meaning. The last matching line wins, and a pattern that matches
//! a directory also covers everything below it.
//!
//! The list exists because a scan is not always desirable. After a rollback the
//! same sources would reproduce the same pages, so the user decides which of
//! them caused the churn, adds them here, and the next round stops resurrecting
//! the work they just undid.
//!
//! Ignored paths are invisible to `scan` and `convert`, and `archive purge`
//! refuses to clear them. An explicit `--key` still reads a single file: the
//! list guides the automatic rounds, it does not lock the tool.

use std::fs;
use std::path::{Path, PathBuf};

/// Engine-relative location of the ignore list.
pub const IGNORE_FILE: &str = ".wiki/ignore";

/// Why the list could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum IgnoreError {
    /// The list could not be opened or written.
    #[error("cannot access {}: {source}", path.display())]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
}

impl IgnoreError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            IgnoreError::Io { .. } => "E_IO",
        }
    }
}

/// One parsed line, with the matching order preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    /// The line as written, without the `!` and the trailing `/`.
    pattern: String,
    /// Whether a match re-includes the path.
    negated: bool,
    /// Whether the rule may only match a directory.
    dir_only: bool,
    /// Whether the pattern is pinned to the repository root.
    anchored: bool,
    /// The pattern split on `/`, ready for segment matching.
    segments: Vec<String>,
}

impl Rule {
    /// The pattern as written, `!` and trailing `/` restored.
    fn render(&self) -> String {
        let mut out = String::new();
        if self.negated {
            out.push('!');
        }
        out.push_str(&self.pattern);
        if self.dir_only {
            out.push('/');
        }
        out
    }

    /// Parses one line, or returns `None` for a blank line or a comment.
    fn parse(line: &str) -> Option<Self> {
        let line = line.trim_end_matches('\r');
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (negated, rest) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        let (dir_only, rest) = match rest.strip_suffix('/') {
            Some(rest) => (true, rest),
            None => (false, rest),
        };
        let rooted = rest.starts_with('/');
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        let mut segments: Vec<String> = rest
            .split('/')
            .filter(|part| !part.is_empty())
            .map(String::from)
            .collect();
        if segments.is_empty() {
            return None;
        }
        // A pattern that names more than one segment is pinned to the root, the
        // same rule `.gitignore` uses; a bare name floats at any depth.
        let anchored = segments.len() > 1 || rooted;
        segments.shrink_to_fit();
        Some(Rule {
            pattern: rest.to_string(),
            negated,
            dir_only,
            anchored,
            segments,
        })
    }

    /// The number of path segments this rule consumes when it matches a prefix.
    fn match_prefix(&self, path: &[String]) -> Option<usize> {
        if self.anchored {
            return match_at(&self.segments, path);
        }
        for start in 0..=path.len() {
            if let Some(len) = match_at(&self.segments, &path[start..]) {
                return Some(start + len);
            }
        }
        None
    }
}

/// Matches `pat` against the front of `path`; returns the segments consumed.
fn match_at(pat: &[String], path: &[String]) -> Option<usize> {
    let Some(first) = pat.first() else {
        return Some(0);
    };
    if first == "**" {
        for skip in 0..=path.len() {
            if let Some(rest) = match_at(&pat[1..], &path[skip..]) {
                return Some(skip + rest);
            }
        }
        return None;
    }
    let candidate = path.first()?;
    if !segment_match(first, candidate) {
        return None;
    }
    match_at(&pat[1..], &path[1..]).map(|consumed| consumed + 1)
}

/// Matches one path segment against one pattern segment (`*` and `?` only).
fn segment_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut marker) = (None, 0usize);
    while ti < txt.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi] == txt[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star = Some(pi);
            marker = ti;
            pi += 1;
        } else if let Some(position) = star {
            pi = position + 1;
            marker += 1;
            ti = marker;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == '*' {
        pi += 1;
    }
    pi == pat.len()
}

/// The parsed ignore list of one repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IgnoreList {
    rules: Vec<Rule>,
}

impl IgnoreList {
    /// Parses the text of an ignore file.
    pub fn parse(text: &str) -> Self {
        let rules = text.lines().filter_map(Rule::parse).collect();
        Self { rules }
    }

    /// Reads `.wiki/ignore`; a missing file is an empty list, not an error.
    pub fn load(root: &Path) -> Result<Self, IgnoreError> {
        let path = root.join(IGNORE_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(IgnoreError::Io { path, source }),
        }
    }

    /// Every pattern in file order, `!` and `/` included, comments excluded.
    pub fn patterns(&self) -> Vec<String> {
        self.rules.iter().map(Rule::render).collect()
    }

    /// Whether `key` is ignored; `key` is `/`-separated and root-relative.
    pub fn is_ignored(&self, key: &str, is_dir: bool) -> bool {
        self.deciding_rule(key, is_dir)
            .is_some_and(|rule| !rule.negated)
    }

    /// The pattern that decided the verdict, for explaining it to a human.
    ///
    /// `None` means no rule matched; `Some` may name a `!` rule, which is why
    /// `is_ignored` can be false while this answers.
    pub fn deciding_pattern(&self, key: &str, is_dir: bool) -> Option<String> {
        self.deciding_rule(key, is_dir).map(Rule::render)
    }

    /// The rule that decided the last match.
    fn deciding_rule(&self, key: &str, is_dir: bool) -> Option<&Rule> {
        let segments: Vec<String> = key
            .split('/')
            .filter(|part| !part.is_empty())
            .map(String::from)
            .collect();
        let mut decided: Option<&Rule> = None;
        for rule in &self.rules {
            let Some(len) = rule.match_prefix(&segments) else {
                continue;
            };
            // A pattern that ends before the path names a directory, so it
            // covers everything below it. A directory-only rule that matches
            // the whole path describes a directory, so it does not apply to a
            // file of the same name; any other rule does.
            let covers = !rule.dir_only || len < segments.len() || is_dir;
            if covers {
                decided = Some(rule);
            }
        }
        decided
    }
}

/// Appends `patterns` to `.wiki/ignore`, skipping ones already present.
pub fn add(root: &Path, patterns: &[String]) -> Result<Vec<String>, IgnoreError> {
    let path = root.join(IGNORE_FILE);
    let mut text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(IgnoreError::Io { path, source }),
    };
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    let existing: Vec<String> = text.lines().map(|line| line.trim().to_string()).collect();
    let mut added = Vec::new();
    for pattern in patterns {
        let pattern = pattern.trim();
        if pattern.is_empty() || existing.iter().any(|line| line == pattern) {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(pattern);
        added.push(pattern.to_string());
    }
    if !added.is_empty() {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        fs::write(&path, text).map_err(|source| IgnoreError::Io { path, source })?;
    }
    Ok(added)
}

/// Removes `patterns` from `.wiki/ignore`, returning the ones that were there.
pub fn remove(root: &Path, patterns: &[String]) -> Result<Vec<String>, IgnoreError> {
    let path = root.join(IGNORE_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(IgnoreError::Io { path, source }),
    };
    let wanted: Vec<&str> = patterns.iter().map(|pattern| pattern.trim()).collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    for line in text.lines() {
        if wanted.contains(&line.trim()) {
            if !removed.iter().any(|seen| seen == line.trim()) {
                removed.push(line.trim().to_string());
            }
            continue;
        }
        kept.push(line);
    }
    if removed.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    fs::write(&path, out).map_err(|source| IgnoreError::Io { path, source })?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(pattern: &str) -> IgnoreList {
        IgnoreList::parse(pattern)
    }

    #[test]
    fn a_bare_name_matches_at_any_depth() {
        let ignore = list("draft.md");
        assert!(ignore.is_ignored("draft.md", false));
        assert!(ignore.is_ignored("notes/draft.md", false));
        assert!(!ignore.is_ignored("notes/draft.txt", false));
    }

    #[test]
    fn a_slash_pins_the_pattern_to_the_root() {
        let ignore = list("notes/draft.md");
        assert!(ignore.is_ignored("notes/draft.md", false));
        assert!(!ignore.is_ignored("deep/notes/draft.md", false));
    }

    #[test]
    fn a_trailing_slash_matches_only_directories() {
        let ignore = list("build/");
        assert!(ignore.is_ignored("build", true));
        assert!(ignore.is_ignored("build/out.md", false));
        assert!(!ignore.is_ignored("build", false));
    }

    #[test]
    fn the_last_matching_line_wins() {
        let ignore = list("*.md\n!keep.md");
        assert!(ignore.is_ignored("draft.md", false));
        assert!(!ignore.is_ignored("keep.md", false));
        let ignore = list("!keep.md\n*.md");
        assert!(ignore.is_ignored("keep.md", false));
    }

    #[test]
    fn double_star_spans_directories() {
        let ignore = list("docs/**/tmp");
        assert!(ignore.is_ignored("docs/tmp", true));
        assert!(ignore.is_ignored("docs/a/b/tmp", true));
        assert!(ignore.is_ignored("docs/a/b/tmp/x.md", false));
    }

    #[test]
    fn wildcards_stay_inside_one_segment() {
        let ignore = list("notes/*.md");
        assert!(ignore.is_ignored("notes/a.md", false));
        assert!(!ignore.is_ignored("notes/sub/a.md", false));
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let ignore = list("# a comment\n\n  \nreal.md\n");
        assert_eq!(ignore.patterns(), vec!["real.md"]);
    }

    #[test]
    fn a_later_rule_explains_the_verdict() {
        let ignore = list("*.md\n!keep.md");
        let rule = ignore.deciding_rule("keep.md", false).expect("a rule");
        assert!(rule.negated);
    }
}
