//! `hash::read_source`: only regular files, and every returned field describes
//! the bytes that were actually read.
//!
//! The byte-identity contract is "one descriptor, one inode": the size, the mtime
//! and the digest all come from the same opened file, so a path that is replaced
//! during the read cannot lend its metadata to someone else's bytes.

use std::fs;

use llm_wiki::hash::{self, HashError};

fn temp_file(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    fs::write(&path, "正文\n").unwrap();
    (dir, path)
}

#[test]
fn every_returned_field_describes_the_bytes() {
    let (_dir, path) = temp_file("a.md");
    let input = hash::read_source(&path, 1024).unwrap();

    assert_eq!(input.bytes, "正文\n".as_bytes());
    assert_eq!(input.hash, hash::hash_bytes(&input.bytes));
    assert_eq!(input.size, input.bytes.len() as i64);
    let on_disk = fs::metadata(&path).unwrap();
    assert_eq!(
        input.mtime,
        on_disk
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    );
}

#[test]
fn a_directory_is_not_a_source() {
    let dir = tempfile::tempdir().unwrap();
    let Err(err) = hash::read_source(dir.path(), 1024) else {
        panic!("a directory must not be a source")
    };
    assert!(matches!(err, HashError::NotRegular { .. }), "{err}");
}

#[cfg(unix)]
#[test]
fn a_symlink_is_not_a_source() {
    let (dir, target) = temp_file("a.md");
    let link = dir.path().join("link.md");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let Err(err) = hash::read_source(&link, 1024) else {
        panic!("a symlink must not be a source")
    };
    assert!(matches!(err, HashError::NotRegular { .. }), "{err}");
}

#[cfg(unix)]
#[test]
fn a_fifo_is_rejected_instead_of_waited_on() {
    use std::process::Command;

    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("pipe.md");
    let status = Command::new("mkfifo").arg(&fifo).status().unwrap();
    assert!(status.success(), "mkfifo is required on Unix test hosts");

    // Would block forever if the type check did not run before the open.
    let Err(err) = hash::read_source(&fifo, 1024) else {
        panic!("a fifo must not be a source")
    };
    assert!(matches!(err, HashError::NotRegular { .. }), "{err}");
}

#[test]
fn a_missing_file_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let Err(err) = hash::read_source(&dir.path().join("gone.md"), 1024) else {
        panic!("a missing file must fail")
    };
    assert!(matches!(err, HashError::Io { .. }), "{err}");
}

#[test]
fn the_budget_still_rejects_an_oversize_file() {
    let (_dir, path) = temp_file("big.md");
    let Err(err) = hash::read_source(&path, 3) else {
        panic!("an oversize file must fail")
    };
    assert!(matches!(err, HashError::Oversize { .. }), "{err}");
}
