//! Acceptance for the distributable skill package.
//!
//! What another agent actually receives is `skills/` copied verbatim into its
//! own skills root. These tests therefore work on a copy of that directory and
//! never assume the repository's `target/` exists, that cargo is installed, or
//! that the network is reachable. If they pass, the package is self-contained;
//! if they fail, the failure text says what to vendor.

use std::fs;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Command, Output};
#[cfg(unix)]
use std::sync::atomic::{AtomicUsize, Ordering};

// Only the unix cases boot the packaged CLI: Windows has no shell bootstrap, so
// the helpers below would be dead code there (and `-D warnings` in CI is right
// to reject that).
#[cfg(unix)]
static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A PATH with the ordinary system tools but without a globally installed
/// `llm-wiki`, so the test cannot pass by accident on a developer machine.
#[cfg(unix)]
const BARE_PATH: &str = "/usr/bin:/bin";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[cfg(unix)]
fn scratch(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "llm-wiki-install-{}-{}-{}",
        std::process::id(),
        n,
        name
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[cfg(unix)]
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create directory");
    for entry in fs::read_dir(from).expect("read directory") {
        let entry = entry.expect("directory entry");
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst);
        } else {
            fs::copy(&src, &dst).expect("copy file");
        }
    }
}

/// The triple `bin/platform.sh` reports here, kept in sync with that script.
#[cfg(unix)]
fn host_triple() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        ""
    }
}

#[cfg(unix)]
fn run(mut cmd: Command, home: &Path) -> Output {
    cmd.env_clear()
        .env("PATH", BARE_PATH)
        .env("HOME", home.as_os_str());
    cmd.output().expect("spawn command")
}

#[cfg(unix)]
fn run_sh(script: &Path, args: &[&str], home: &Path) -> Output {
    let mut cmd = Command::new("sh");
    cmd.arg(script);
    cmd.args(args);
    run(cmd, home)
}

#[cfg(unix)]
fn run_prog(program: &Path, args: &[&str], home: &Path) -> Output {
    let mut cmd = Command::new(program);
    cmd.args(args);
    run(cmd, home)
}

#[cfg(unix)]
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The whole point of the package: copy it somewhere else and the CLI works
/// without a compiler, a network, or a pre-existing install.
#[cfg(unix)]
#[test]
fn a_copied_package_bootstraps_offline() {
    let triple = host_triple();
    assert!(!triple.is_empty(), "unknown host platform for this test");

    let root = scratch("bootstrap");
    let skills = root.join("skills");
    copy_tree(&repo_root().join("skills"), &skills);

    let vendored = skills
        .join("llm-wiki/bin/vendor")
        .join(triple)
        .join("llm-wiki");
    assert!(
        vendored.is_file(),
        "the package carries no {triple} binary (looked in {}); run scripts/vendor.sh before syncing it",
        vendored.display()
    );

    let home = root.join("home");
    fs::create_dir_all(&home).expect("create home");
    let boot = skills.join("llm-wiki/bin/bootstrap.sh");

    let first = run_sh(&boot, &[], &home);
    assert!(
        first.status.success(),
        "bootstrap failed: {}",
        text(&first.stderr)
    );

    let installed = home.join(".local/bin/llm-wiki");
    assert!(installed.is_file(), "bootstrap did not install the binary");
    let probe = run_prog(&installed, &["version"], &home);
    assert!(
        probe.status.success(),
        "installed binary does not run: {}",
        text(&probe.stderr)
    );

    // Idempotent: a second run leaves the working install alone.
    let second = run_sh(&boot, &[], &home);
    assert!(second.status.success(), "second bootstrap failed");
    assert!(
        text(&second.stdout).contains("already installed"),
        "second bootstrap should report the existing install, got: {}",
        text(&second.stdout)
    );

    // The launcher works with no install at all, which is what the skill
    // documents as the fallback path.
    let launcher = run_sh(&skills.join("llm-wiki/bin/llm-wiki"), &["version"], &home);
    assert!(
        launcher.status.success(),
        "bundled launcher failed: {}",
        text(&launcher.stderr)
    );

    let _ = fs::remove_dir_all(&root);
}

/// A package that lacks the host's binary must say so and name the fix, rather
/// than failing with a stack of shell noise.
#[cfg(unix)]
#[test]
fn a_package_without_the_host_binary_explains_itself() {
    let triple = host_triple();
    assert!(!triple.is_empty(), "unknown host platform for this test");

    let root = scratch("missing");
    let skills = root.join("skills");
    copy_tree(&repo_root().join("skills"), &skills);
    // The vendored binaries are not committed, so on a fresh clone this
    // directory may already be absent: that is the same scenario.
    let vendored = skills.join("llm-wiki/bin/vendor").join(triple);
    if vendored.exists() {
        fs::remove_dir_all(&vendored).expect("remove vendored binary");
    }

    let home = root.join("home");
    fs::create_dir_all(&home).expect("create home");

    let out = run_sh(&skills.join("llm-wiki/bin/bootstrap.sh"), &[], &home);
    assert_eq!(out.status.code(), Some(2), "expected a usage-style exit");
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("scripts/vendor.sh"),
        "the failure should name the fixing command, got: {stderr}"
    );

    let _ = fs::remove_dir_all(&root);
}

/// Every skill is loaded on its own, so each one must state where the command
/// comes from; otherwise a directly-loaded child skill dead-ends.
#[test]
fn every_skill_points_at_the_bundled_cli() {
    let skills = repo_root().join("skills");
    let mut seen = 0;
    for entry in fs::read_dir(&skills).expect("read skills directory") {
        let entry = entry.expect("skill directory");
        if !entry.path().is_dir() {
            continue;
        }
        seen += 1;
        let name = entry.file_name().to_string_lossy().into_owned();
        let body = fs::read_to_string(entry.path().join("SKILL.md")).expect("read SKILL.md");
        assert!(
            body.contains("bootstrap.sh"),
            "{name}/SKILL.md never tells the agent how to install the command"
        );
        assert!(
            body.contains("llm-wiki"),
            "{name}/SKILL.md does not mention the command at all"
        );
    }
    assert_eq!(seen, 6, "expected six skills in the package");
}

/// The Windows side ships launchers instead of a POSIX shell script.
#[test]
fn the_windows_launchers_ship_with_the_package() {
    let bin = repo_root().join("skills/llm-wiki/bin");
    for file in [
        "llm-wiki.cmd",
        "bootstrap.ps1",
        "bootstrap.sh",
        "llm-wiki",
        "platform.sh",
    ] {
        assert!(bin.join(file).is_file(), "package is missing bin/{file}");
    }
}

/// Bootstrapping must not silently fall back to building from source: that
/// would need a compiler the receiving agent is not promised to have.
#[cfg(unix)]
#[test]
fn bootstrap_never_invokes_a_compiler() {
    let body = fs::read_to_string(repo_root().join("skills/llm-wiki/bin/bootstrap.sh"))
        .expect("read bootstrap.sh");
    for forbidden in ["cargo ", "rustc ", "curl ", "wget "] {
        assert!(
            !body.contains(forbidden),
            "bootstrap.sh contains `{forbidden}`; it must only copy the packaged binary"
        );
    }
}
