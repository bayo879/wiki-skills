//! The work lease: what counts as progress, and when a batch is abandoned.
//!
//! Expiry is the one rule a crashed agent cannot defend itself against, so the
//! fixtures write the file by hand and ask the same question `ingest` asks:
//! has this batch moved within `LEASE_SECONDS`?

use std::fs;
use std::path::Path;

use llm_wiki::lease::{self, Item, Lease};
use tempfile::tempdir;

fn item(key: &str) -> Item {
    Item {
        source_key: key.to_string(),
        raw_key: format!(".wiki/raw/{key}"),
        raw_hash: "digest".to_string(),
    }
}

/// RFC 3339 in the shape `lease::stamp` writes.
fn ago(hours: i64) -> String {
    let when = chrono::Utc::now() - chrono::Duration::hours(hours);
    when.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn write_lease(root: &Path, body: &str) {
    fs::create_dir_all(root.join(".wiki")).unwrap();
    fs::write(root.join(".wiki/work.lock"), body).unwrap();
}

/// A lease file as this version or the previous one would have written it.
fn lease_json(started: &str, refreshed: Option<&str>) -> String {
    let refreshed = match refreshed {
        Some(value) => format!(",\"refreshed_at\":\"{value}\""),
        None => String::new(),
    };
    format!(
        "{{\"started_at\":\"{started}\"{refreshed},\"pid\":1,\"run_id\":\"test\",\
         \"items\":[{{\"source_key\":\"a.md\",\"raw_key\":\".wiki/raw/a.md\",\
         \"raw_hash\":\"digest\"}}]}}"
    )
}

#[test]
fn an_acknowledgement_keeps_a_long_batch_alive() {
    let dir = tempdir().unwrap();
    let opened = ago(5);
    write_lease(dir.path(), &lease_json(&opened, Some(&ago(0))));

    let open = lease::read(dir.path()).unwrap().unwrap();
    assert!(
        !open.expired(),
        "a batch opened hours ago but acknowledged just now is still live"
    );
    assert_eq!(
        open.started_at, opened,
        "the audit trail keeps the open time"
    );
}

#[test]
fn a_batch_that_stopped_moving_expires() {
    let dir = tempdir().unwrap();
    write_lease(dir.path(), &lease_json(&ago(0), Some(&ago(5))));

    let open = lease::read(dir.path()).unwrap().unwrap();
    assert!(open.expired(), "the last acknowledgement is what expires");
}

#[test]
fn a_file_without_the_heartbeat_falls_back_to_when_it_opened() {
    let dir = tempdir().unwrap();

    write_lease(dir.path(), &lease_json(&ago(5), None));
    let stale = lease::read(dir.path()).unwrap().unwrap();
    assert!(stale.expired(), "old files are judged by started_at");

    write_lease(dir.path(), &lease_json(&ago(0), None));
    let fresh = lease::read(dir.path()).unwrap().unwrap();
    assert!(!fresh.expired(), "a file written a moment ago is live");
}

#[test]
fn an_unparsable_timestamp_is_treated_as_abandoned() {
    let dir = tempdir().unwrap();
    write_lease(dir.path(), &lease_json("yesterday-ish", Some("later")));

    let open = lease::read(dir.path()).unwrap().unwrap();
    assert!(
        open.expired(),
        "a lease nobody can date must not block the repository forever"
    );
}

#[test]
fn refreshing_keeps_the_moment_the_batch_opened() {
    let dir = tempdir().unwrap();
    let mut open = Lease::open("run-1", vec![item("a.md")]);
    open.started_at = "2020-01-01T00:00:00Z".to_string();

    open.refresh();
    open.save(dir.path()).unwrap();

    let saved = lease::read(dir.path()).unwrap().unwrap();
    assert_eq!(saved.started_at, "2020-01-01T00:00:00Z");
    assert_ne!(saved.refreshed_at, "2020-01-01T00:00:00Z");
    assert!(!saved.expired(), "a refreshed batch is live again");
}

#[test]
fn the_last_acknowledgement_deletes_the_file() {
    let dir = tempdir().unwrap();
    let mut open = Lease::open("run-1", vec![item("a.md")]);
    open.save(dir.path()).unwrap();

    assert!(!open.settle("a.md"));
    open.save(dir.path()).unwrap();

    assert!(lease::read(dir.path()).unwrap().is_none());
}
