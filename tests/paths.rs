//! Path identity keys and content hashing.
//!
//! These are the two primitives every later command leans on: a key identifies a
//! source file independently of the filesystem's chosen Unicode normalisation
//! and path separator, and a hash identifies its bytes.

use std::fs;
use std::path::Path;

use llm_wiki::hash::{fingerprint, hash_bytes, hash_file};
use llm_wiki::paths::{identity_key, is_ignored, join_key, to_slash};

/// Bytes that are stable on every platform: no `\n`, so no CRLF translation.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

#[test]
fn nfd_and_nfc_inputs_produce_same_key() {
    let root = Path::new("/root");
    let nfc = identity_key(root, Path::new("/root/资料/café.md")).unwrap();
    let nfd = identity_key(root, Path::new("/root/资料/cafe\u{0301}.md")).unwrap();
    assert_eq!(nfc, "资料/café.md");
    assert_eq!(nfc, nfd);
}

#[test]
fn case_is_preserved_and_separators_are_slashes() {
    let k = identity_key(Path::new("/r"), Path::new("/r/a/b.MD")).unwrap();
    assert_eq!(k, "a/b.MD");
}

#[test]
fn dot_prefixed_components_are_ignored() {
    assert!(is_ignored(".git/config"));
    assert!(is_ignored("a/.hidden/x.md"));
    assert!(!is_ignored("a/b.md"));
}

#[test]
fn path_outside_root_is_an_error() {
    assert!(identity_key(Path::new("/r"), Path::new("/other/x.md")).is_err());
}

#[test]
fn parent_components_are_rejected() {
    assert!(identity_key(Path::new("/r"), Path::new("/r/a/../b.md")).is_err());
}

#[test]
fn slash_form_is_stable_across_platforms() {
    assert_eq!(to_slash(Path::new("/r/a/b.md")), "/r/a/b.md");
    assert_eq!(to_slash(Path::new("a/b.md")), "a/b.md");
}

#[test]
fn key_round_trips_through_join_key() {
    let root = Path::new("/r");
    assert_eq!(join_key(root, "a/b.md"), root.join("a").join("b.md"));
    assert_eq!(join_key(root, "资料/café.md"), root.join("资料/café.md"));
}

#[test]
fn hash_bytes_matches_the_known_empty_digest() {
    assert_eq!(
        hash_bytes(b""),
        "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
    assert_ne!(hash_bytes(b"abc"), hash_bytes(b"abd"));
}

/// A file well inside the sampled windows must hash identically in both modes:
/// the sampled bytes are the whole file, so the digests coincide.
#[test]
fn full_and_sampled_hashes_agree_for_a_small_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small.bin");
    fs::write(&path, pattern(64 * 1024)).unwrap();

    let full = hash_file(&path, true).unwrap();
    let sampled = hash_file(&path, false).unwrap();

    assert!(full.starts_with("blake3:"));
    assert_eq!(full, sampled);
}

/// Sampling must actually look at the middle of a large file, not just its ends.
#[test]
fn sampled_hash_notices_a_change_in_the_sampled_middle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.bin");
    let mut bytes = pattern(6 * 1024 * 1024);
    fs::write(&path, &bytes).unwrap();
    let before = hash_file(&path, false).unwrap();

    bytes[3 * 1024 * 1024] ^= 0xff;
    fs::write(&path, &bytes).unwrap();
    let after = hash_file(&path, false).unwrap();

    assert_ne!(before, after);
}

#[test]
fn fingerprint_reports_size_mtime_and_sample() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.bin");
    let bytes = pattern(5 * 1024 * 1024);
    fs::write(&path, &bytes).unwrap();

    let fp = fingerprint(&path).unwrap();

    assert_eq!(fp.size, bytes.len() as u64);
    assert!(fp.mtime > 0);
    assert!(fp.sample.starts_with("blake3:"));
    assert_eq!(fp.sample, hash_file(&path, false).unwrap());
}
