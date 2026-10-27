//! The hard gate in front of every repository command.
//!
//! Commands used to start by loading `wiki.toml`, which meant an uninitialised
//! repository failed somewhere in the middle: a missing contract surfaced as a
//! database error, a missing engine directory as an IO error, and a missing
//! `wiki/` as an empty result. Those messages describe the symptom, not the
//! cause, and an agent reading them cannot tell "run `init`" from "this repo is
//! damaged".
//!
//! So the layout is split by who owns it:
//!
//! - **Authored and load-bearing**: `wiki.toml` and `wiki/`. They live in git,
//!   every command reads one or both, and their absence means the repository was
//!   never initialised. Every command refuses with [`CODE`] and the hint in
//!   [`HINT`]; nothing is guessed or repaired. (`SCHEMA.md` is the third file
//!   `init` writes, but no code reads it, so it is `doctor`'s business rather
//!   than a hard stop.)
//! - **Provisioned**: `source/`, `raw/`, `wiki/audit/`, `wiki/archive/` and
//!   everything under the engine directory. These are inputs, outputs and
//!   derived state; `init` creates them, but a fresh clone has no engine
//!   directory at all because it is ignored. Refusing there would make
//!   "clone and run" impossible, so [`provision`] creates them on demand.
//!
//! The distinction is the point: a missing contract is a decision the user has
//! not made yet, and the tool must not make it for them.

use std::path::Path;

/// Error code for a repository that has not been initialised.
pub const CODE: &str = "E_NOT_INITIALISED";

/// What the caller should do about it.
pub const HINT: &str = "run `llm-wiki init` in the repository root, then retry";

/// The contract every command loads; `init` is the only thing that writes it.
const REQUIRED_FILES: [&str; 1] = ["wiki.toml"];

/// Directories that hold the product and are in git.
const REQUIRED_DIRS: [&str; 1] = ["wiki"];

/// Directories that are inputs, outputs or derived state.
///
/// Parents come before children so a single `create_dir_all` per entry works
/// even on a fresh checkout.
const PROVISIONED: [&str; 9] = [
    "source",
    "raw",
    "wiki/archive",
    "wiki/audit",
    ".wiki",
    ".wiki/cache",
    ".wiki/sessions",
    ".wiki/staging",
    ".wiki/versions",
];

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
    for name in REQUIRED_DIRS {
        if !root.join(name).is_dir() {
            return Err(Missing::Dir(name));
        }
    }
    Ok(())
}

/// Creates the provisioned directories that are absent.
///
/// Existing directories are left alone, so this is safe on every call and does
/// not touch the mtime of anything already there.
pub fn provision(root: &Path) -> std::io::Result<()> {
    for name in PROVISIONED {
        let path = root.join(name);
        if !path.is_dir() {
            std::fs::create_dir_all(&path)?;
        }
    }
    Ok(())
}
