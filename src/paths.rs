//! Path identity keys.
//!
//! A key is a `/`-separated path relative to the wiki root, with every component
//! in Unicode NFC. Two paths that differ only in normalisation form or in the
//! platform's native separator therefore produce the same key, which is exactly
//! what `state.db` stores and what links between pages are written against.

use std::path::{Component, Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

/// Why a path could not be turned into an identity key.
#[derive(Debug, thiserror::Error)]
pub enum PathError {
    /// The path does not live under the wiki root.
    #[error("{} is not inside {}", path.display(), root.display())]
    OutsideRoot { path: PathBuf, root: PathBuf },
    /// A `..` component would let the key escape the root.
    #[error("{} contains a parent-directory component", path.display())]
    ParentComponent { path: PathBuf },
    /// A component is not UTF-8, so it cannot be stored as a key.
    #[error("{} is not valid UTF-8", path.display())]
    NonUtf8 { path: PathBuf },
}

/// Result of the path helpers.
pub type Result<T> = std::result::Result<T, PathError>;

/// Key for `p` relative to `root`, NFC-normalised and `/`-separated.
///
/// Normalisation is not an error: a name that is already NFC is returned as is,
/// and any other form is folded onto its NFC spelling.
pub fn identity_key(root: &Path, p: &Path) -> Result<String> {
    let rel = p.strip_prefix(root).map_err(|_| PathError::OutsideRoot {
        path: p.to_path_buf(),
        root: root.to_path_buf(),
    })?;

    let mut parts: Vec<String> = Vec::new();
    for component in rel.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(PathError::ParentComponent {
                    path: p.to_path_buf(),
                })
            }
            Component::Normal(text) => {
                let text = text.to_str().ok_or_else(|| PathError::NonUtf8 {
                    path: p.to_path_buf(),
                })?;
                parts.push(text.nfc().collect());
            }
            // A path stripped of its root cannot still carry one; report it
            // rather than silently build a key that points nowhere.
            Component::RootDir | Component::Prefix(_) => {
                return Err(PathError::OutsideRoot {
                    path: p.to_path_buf(),
                    root: root.to_path_buf(),
                })
            }
        }
    }
    Ok(parts.join("/"))
}

/// Whether a `/`-separated relative path belongs to a dot-directory or is a
/// dot-file: any component starting with `.` marks it ignored.
pub fn is_ignored(rel: &str) -> bool {
    rel.split('/').any(|component| component.starts_with('.'))
}

/// Renders a path with `/` separators on every platform.
pub fn to_slash(p: &Path) -> String {
    let mut out = String::new();
    for component in p.components() {
        match component {
            Component::RootDir => out.push('/'),
            Component::Prefix(prefix) => out.push_str(&prefix.as_os_str().to_string_lossy()),
            Component::CurDir => {
                push_separator(&mut out);
                out.push('.');
            }
            Component::ParentDir => {
                push_separator(&mut out);
                out.push_str("..");
            }
            Component::Normal(text) => {
                push_separator(&mut out);
                out.push_str(&text.to_string_lossy());
            }
        }
    }
    out
}

fn push_separator(out: &mut String) {
    if !out.is_empty() && !out.ends_with('/') {
        out.push('/');
    }
}

/// Inverse of [`identity_key`]: rebuilds a native path from `root` and `key`.
///
/// Keys produced by [`identity_key`] never contain empty or `.` components, and
/// never `..`; those are dropped here so that a hand-written key cannot escape
/// the root through string surgery.
pub fn join_key(root: &Path, key: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for component in key
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
    {
        if component == ".." {
            continue;
        }
        out.push(component);
    }
    out
}
