//! Content hashing and change detection.
//!
//! [`read_source`] is the one path current code must use: it opens the file
//! once, enforces `parse.max_source_bytes`, rejects a file that changes while
//! being read, and returns the bytes together with their full digest — so the
//! parse and the hash always describe the same bytes.
//!
//! Only regular files are sources. Symbolic links, FIFOs and device files are
//! rejected instead of being followed: `scan` already skips them, and following
//! them would let an explicit `ingest <KEY>` hash bytes that no scan ever saw
//! (and block forever on a FIFO). The stability check compares the opened
//! descriptor's metadata before and after the read, so every byte and every
//! size/mtime in the result describes the same inode — a replacement that lands
//! during the read is a different file and is simply not the one read.
//!
//! [`hash_file`] with `full = true` digests every byte. The sampled mode
//! (`full = false`, [`fingerprint`]) is legacy: only the retired `version`
//! module calls it with `false`. It digests three windows (head, middle, tail)
//! plus size and mtime, which was a scan-cost trade-off; do not use it for new
//! change detection, because it cannot prove the middle bytes are unchanged.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Every digest is prefixed with the algorithm so stored hashes stay readable.
const PREFIX: &str = "blake3:";

/// Size of each of the three windows used by sampled hashing.
const SAMPLE_WINDOW: u64 = 1024 * 1024;

/// Read granularity for sampling; the windows themselves may be larger.
const READ_BUFFER: usize = 64 * 1024;

/// Why a file could not be hashed.
#[derive(Debug, thiserror::Error)]
pub enum HashError {
    /// The file could not be opened or read.
    #[error("cannot read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The input exceeded the configured byte budget.
    #[error("source {} exceeds {limit} bytes", path.display())]
    Oversize { path: PathBuf, limit: u64 },
    /// The input changed while being read.
    #[error("source {} changed while reading", path.display())]
    Unstable { path: PathBuf },
    /// The path is not a regular file: a symlink, FIFO or device.
    #[error("source {} is not a regular file ({kind})", path.display())]
    NotRegular { path: PathBuf, kind: &'static str },
}

/// Result of the hashing helpers.
pub type Result<T> = std::result::Result<T, HashError>;

/// Cheap stand-in for a full digest: size, mtime and a sampled hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// File length in bytes.
    pub size: u64,
    /// Modification time as seconds since the Unix epoch.
    pub mtime: i64,
    /// `blake3:` digest of the sampled windows.
    pub sample: String,
}

/// A bounded, stable source read used for both parsing and bookkeeping.
pub struct SourceInput {
    pub bytes: Vec<u8>,
    pub hash: String,
    pub size: i64,
    pub mtime: i64,
    pub mtime_ns: Option<i64>,
}

/// Read at most the configured budget plus one byte, rejecting growth or drift.
pub fn read_source(path: &Path, limit: u64) -> Result<SourceInput> {
    read_source_with(path, limit, || {})
}

/// The read itself, with a seam the unit tests use to change the file between
/// the read and the stability check. It is never called with a non-trivial hook
/// outside `#[cfg(test)]`: the instability has to be reachable deterministically.
fn read_source_with(path: &Path, limit: u64, between: impl FnOnce()) -> Result<SourceInput> {
    // Check the type before opening: opening a FIFO without a writer blocks,
    // and `scan` never reports one as a source in the first place.
    let link = std::fs::symlink_metadata(path).map_err(|source| HashError::Io {
        path: path.into(),
        source,
    })?;
    if link.file_type().is_symlink() {
        return Err(HashError::NotRegular {
            path: path.into(),
            kind: "symbolic link",
        });
    }
    if !link.is_file() {
        return Err(HashError::NotRegular {
            path: path.into(),
            kind: "not a regular file",
        });
    }
    let file = open(path)?;
    let before = file.metadata().map_err(|source| HashError::Io {
        path: path.into(),
        source,
    })?;
    if !before.is_file() {
        return Err(HashError::NotRegular {
            path: path.into(),
            kind: "not a regular file",
        });
    }
    if before.len() > limit {
        return Err(HashError::Oversize {
            path: path.into(),
            limit,
        });
    }
    let mut bytes = Vec::new();
    (&file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| HashError::Io {
            path: path.into(),
            source,
        })?;
    if bytes.len() as u64 > limit {
        return Err(HashError::Oversize {
            path: path.into(),
            limit,
        });
    }
    between();
    // The same descriptor, not the path: a rename over the path during the read
    // would describe a file whose bytes are not the ones in `bytes`.
    let after = file.metadata().map_err(|source| HashError::Io {
        path: path.into(),
        source,
    })?;
    if before.len() != bytes.len() as u64
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(HashError::Unstable { path: path.into() });
    }
    Ok(SourceInput {
        hash: hash_bytes(&bytes),
        size: bytes.len() as i64,
        mtime: before.modified().map(unix_seconds).unwrap_or(0),
        mtime_ns: metadata_mtime_ns(&before),
        bytes,
    })
}

/// Nanosecond timestamp, absent rather than guessed when unavailable.
pub fn metadata_mtime_ns(meta: &std::fs::Metadata) -> Option<i64> {
    let time = meta.modified().ok()?;
    let duration = time.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_nanos()).ok()
}

/// `blake3:<hex>` digest of an in-memory buffer.
pub fn hash_bytes(b: &[u8]) -> String {
    format!("{PREFIX}{}", blake3::hash(b).to_hex())
}

/// Content digest of `p`.
///
/// `full = true` streams the whole file through blake3; `full = false` returns
/// the sampled digest from [`fingerprint`], which equals the full digest for
/// files whose windows overlap and cover it entirely.
pub fn hash_file(p: &Path, full: bool) -> Result<String> {
    if full {
        hash_whole(p)
    } else {
        Ok(fingerprint(p)?.sample)
    }
}

/// Size, mtime and sampled digest of `p`.
pub fn fingerprint(p: &Path) -> Result<Fingerprint> {
    let file = open(p)?;
    let meta = file.metadata().map_err(|source| HashError::Io {
        path: p.to_path_buf(),
        source,
    })?;
    let size = meta.len();
    // Platforms without a modification time report 0 rather than failing the
    // whole scan; the sampled digest still changes when the bytes do.
    let mtime = meta.modified().map(unix_seconds).unwrap_or(0);
    let sample = hash_sample(&file, size).map_err(|source| HashError::Io {
        path: p.to_path_buf(),
        source,
    })?;
    Ok(Fingerprint {
        size,
        mtime,
        sample,
    })
}

fn open(p: &Path) -> Result<File> {
    File::open(p).map_err(|source| HashError::Io {
        path: p.to_path_buf(),
        source,
    })
}

fn hash_whole(p: &Path) -> Result<String> {
    let file = open(p)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(file).map_err(|source| HashError::Io {
        path: p.to_path_buf(),
        source,
    })?;
    Ok(format!("{PREFIX}{}", hasher.finalize().to_hex()))
}

fn hash_sample(file: &File, size: u64) -> io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut reader = file;
    let mut buffer = vec![0u8; READ_BUFFER];
    // `sample_ranges` yields half-open `(start, end)` offsets, not lengths.
    for (start, end) in sample_ranges(size) {
        reader.seek(SeekFrom::Start(start))?;
        let mut remaining = end - start;
        while remaining > 0 {
            let want = remaining.min(buffer.len() as u64) as usize;
            reader.read_exact(&mut buffer[..want])?;
            hasher.update(&buffer[..want]);
            remaining -= want as u64;
        }
    }
    Ok(format!("{PREFIX}{}", hasher.finalize().to_hex()))
}

/// Byte ranges hashed in sampled mode: head, middle and tail, merged so that
/// overlapping windows are not hashed twice. Files up to twice the window size
/// are covered entirely, which makes sampled and full hashes agree there.
fn sample_ranges(size: u64) -> Vec<(u64, u64)> {
    if size == 0 {
        return Vec::new();
    }
    let window = SAMPLE_WINDOW.min(size);
    let starts = [0, size / 2 - window / 2, size.saturating_sub(window)];
    let mut ranges: Vec<(u64, u64)> = starts
        .iter()
        .map(|start| (*start, (*start + window).min(size)))
        .collect();
    ranges.sort_unstable();

    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => since.as_secs() as i64,
        Err(before) => -(before.duration().as_secs() as i64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_source_that_grows_during_the_read_is_unstable() {
        let dir =
            std::env::temp_dir().join(format!("llm-wiki-hash-unstable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.md");
        std::fs::write(&path, "短文\n").unwrap();

        let Err(err) = read_source_with(&path, 1 << 20, || {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(b"more\n").unwrap();
        }) else {
            panic!("a read that raced a writer must not report a stable digest");
        };
        assert!(matches!(err, HashError::Unstable { .. }), "{err}");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
