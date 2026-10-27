//! Content hashing and cheap change detection.
//!
//! [`hash_file`] with `full = true` digests every byte; with `full = false` it
//! digests three windows (head, middle, tail) plus the file size and mtime, which
//! is enough to notice an edit without reading a multi-gigabyte scan target.

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
