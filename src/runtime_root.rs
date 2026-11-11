//! A distributed executable owns the root above its `.wiki/cli` directory.
use std::path::{Path, PathBuf};

/// Resolve symlinks before recognising the distribution layout. Standalone
/// development binaries retain the current-directory contract.
pub fn resolve_root(executable: &Path, cwd: &Path) -> PathBuf {
    let executable = executable
        .canonicalize()
        .unwrap_or_else(|_| executable.into());
    let Some(cli) = executable.parent() else {
        return cwd.into();
    };
    let Some(engine) = cli.parent() else {
        return cwd.into();
    };
    if cli.file_name().is_some_and(|name| name == "cli")
        && engine.file_name().is_some_and(|name| name == ".wiki")
    {
        if let Some(root) = engine.parent() {
            return root.into();
        }
    }
    cwd.into()
}
