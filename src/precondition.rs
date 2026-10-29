//! The hard gate in front of every repository command.
//!
//! Commands used to start by loading `wiki.toml`, which meant an uninitialised
//! repository failed somewhere in the middle: a missing contract surfaced as a
//! database error, a missing engine directory as an IO error, and a missing
//! `wiki/` as an empty result. Those messages describe the symptom, not the
//! cause, and an agent reading them cannot tell "run `init`" from "this repo is
//! damaged".
//!
//! So the layout is one table, [`crate::layout::LAYOUT`], and each entry says
//! who owns it:
//!
//! - **Authored and load-bearing**: `.wiki/wiki.toml` and `.wiki/pages/`. The
//!   contract is the decision the user made, and the page tree is the product;
//!   their absence means the repository was never initialised. Every command
//!   refuses with [`CODE`] and the hint in [`HINT`]; nothing is guessed or
//!   repaired. (`.wiki/SCHEMA.md` is the third file `init` writes, but no code
//!   reads it, so it is `doctor`'s business rather than a hard stop.)
//! - **Provisioned**: everything else under `.wiki/`. These are inputs, outputs
//!   and derived state; `init` creates them, but a fresh clone has no engine
//!   directory at all because it is not committed. Refusing there would make
//!   "clone and run" impossible, so [`provision`] creates them on demand.
//!
//! `provision` never creates a missing authored directory, not even through a
//! child: `.wiki/pages/archive` must not resurrect `.wiki/pages`. The gate runs
//! before provision, so that state usually never reaches this code; the rule
//! keeps "not initialised" from silently becoming "empty wiki" if it ever does.
//!
//! Nothing outside `.wiki/` is ever created: the repository itself is the source
//! tree, and the tool treats it as read-only.
//!
//! The distinction is the point: a missing contract is a decision the user has
//! not made yet, and the tool must not make it for them.

use std::path::Path;

use crate::layout::LAYOUT;

/// Error code for a repository that has not been initialised.
pub const CODE: &str = "E_NOT_INITIALISED";

/// What the caller should do about it.
pub const HINT: &str = "run `llm-wiki doctor --fix` in the repository root, then retry";

/// The contract every command loads; `init` is the only thing that writes it.
const REQUIRED_FILES: [&str; 1] = [".wiki/wiki.toml"];

/// The first authored path that is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// A contract file that `init` writes.
    File(&'static str),
    /// A directory that holds the product.
    Dir(&'static str),
}

impl Missing {
    /// Path relative to the repository root.
    pub fn path(self) -> &'static str {
        match self {
            Missing::File(path) | Missing::Dir(path) => path,
        }
    }

    /// `file` or `dir`.
    pub fn kind(self) -> &'static str {
        match self {
            Missing::File(_) => "file",
            Missing::Dir(_) => "dir",
        }
    }
}

/// Reports the first authored path that is absent.
///
/// Presence is all this checks: a `wiki.toml` that exists but does not parse is
/// [`crate::config::ConfigError`]'s business, and reporting it here would
/// duplicate the validation and its messages.
pub fn check(root: &Path) -> Result<(), Missing> {
    for name in REQUIRED_FILES {
        if !root.join(name).is_file() {
            return Err(Missing::File(name));
        }
    }
    for entry in LAYOUT.iter().filter(|entry| entry.authored) {
        if !root.join(entry.path).is_dir() {
            return Err(Missing::Dir(entry.path));
        }
    }
    Ok(())
}

/// Creates the provisioned directories that are absent.
///
/// A derived directory under a missing authored parent is skipped: creating
/// `.wiki/pages/archive` would also create `.wiki/pages`, which is exactly the
/// product the gate refuses to invent. Existing directories are left alone, so
/// this is safe on every call and does not touch the mtime of anything already
/// there.
pub fn provision(root: &Path) -> std::io::Result<()> {
    for entry in LAYOUT.iter().filter(|entry| !entry.authored) {
        if missing_authored_ancestor(root, entry.path) {
            continue;
        }
        let path = root.join(entry.path);
        if !path.is_dir() {
            std::fs::create_dir_all(&path)?;
        }
    }
    Ok(())
}

/// True when `path` sits under an authored directory that is absent.
fn missing_authored_ancestor(root: &Path, path: &str) -> bool {
    LAYOUT.iter().any(|entry| {
        entry.authored
            && path
                .strip_prefix(entry.path)
                .is_some_and(|rest| rest.starts_with('/'))
            && !root.join(entry.path).is_dir()
    })
}
