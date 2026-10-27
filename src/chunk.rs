//! Splitting long text into UTF-8-safe chunks and reading them back on demand.
//!
//! A chunk is a byte range into the source text, never a copy: [`split`] only
//! returns ranges, and every cut is a character boundary, so a multibyte
//! character is never broken. [`materialize`] writes one markdown file per range
//! under `<cache_dir>/<hash>/`, and [`read_range`] reads only the chunk files it
//! was asked for, so a large source never has to be loaded whole again.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

/// Narrowest backward search for a newline at a cut, in bytes.
///
/// The design asks for 10% of the target. That window can be shorter than one
/// line — the plan's own `"abc\n"` case with a 10-byte target needs a 3-byte
/// look-back — so this floor keeps line-aligned cuts useful for small targets
/// without letting a large target scan far away from the ideal position.
const MIN_LOOKBACK: usize = 8;

/// Minimum zero-padding of the chunk file names.
///
/// Four digits keep lexicographic order equal to numeric order up to 9999
/// chunks, which is well above what `parse.max_source_bytes` allows at the
/// default target. Wider counts widen the name, so the order never breaks.
const NAME_DIGITS: usize = 4;

/// Failure while splitting, materializing, or reading chunks.
#[derive(Debug, thiserror::Error)]
pub enum ChunkError {
    /// The request cannot be served as asked: a zero target, an empty or
    /// multi-component hash, an invalid range, or a chunk that was never
    /// materialized.
    #[error("{message}")]
    BadArgs { message: String },
    /// `from`/`to` are not a 1-based inclusive range.
    #[error("chunk range {from}-{to} is invalid: chunks are numbered from 1 and {to} must not be smaller than {from}")]
    BadRange { from: usize, to: usize },
    /// The requested chunk is not in the cache directory.
    #[error("chunk {index} is not materialized under {}", dir.display())]
    Missing { dir: PathBuf, index: usize },
    /// Reading or writing the cache failed.
    #[error("cannot {action} {}: {source}", path.display())]
    Io {
        path: PathBuf,
        action: &'static str,
        #[source]
        source: io::Error,
    },
}

impl ChunkError {
    /// Stable CLI code for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            ChunkError::BadArgs { .. }
            | ChunkError::BadRange { .. }
            | ChunkError::Missing { .. } => "E_BAD_ARGS",
            ChunkError::Io { .. } => "E_IO",
        }
    }
}

/// Result alias for chunk operations.
pub type Result<T> = std::result::Result<T, ChunkError>;

/// Splits `text` into byte ranges of about `target` bytes.
///
/// Ranges are half-open and cover `text` exactly once, in order. A zero target
/// cannot produce any range, so it yields an empty vector. Cuts prefer the
/// nearest preceding `\n` within the look-back window, then the nearest
/// character boundary; a CRLF pair is never split.
pub fn split(text: &str, target: usize) -> Vec<(usize, usize)> {
    if text.is_empty() || target == 0 {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = cut(text, start, target);
        ranges.push((start, end));
        start = end;
    }
    ranges
}

/// The end offset of the chunk that starts at `start`.
fn cut(text: &str, start: usize, target: usize) -> usize {
    let bytes = text.as_bytes();
    let len = text.len();
    // The remaining text fits in one chunk: there is nothing to prefer.
    if start + target >= len {
        return len;
    }

    let mut candidate = start + target;
    while !text.is_char_boundary(candidate) {
        candidate -= 1;
    }

    // Searching backwards from the cut (inclusive: a newline right at the cut is
    // the ideal place to break after) keeps lines whole when possible.
    let lookback = (target / 10).max(MIN_LOOKBACK);
    let search_from = candidate.saturating_sub(lookback).max(start);
    for pos in (search_from..=candidate).rev() {
        if bytes[pos] == b'\n' {
            return pos + 1;
        }
    }

    let mut end = candidate;
    if end == start {
        // The target is smaller than the first character: take it whole.
        end = start + text[start..].chars().next().map_or(1, char::len_utf8);
    }
    // A fallback cut may land between CR and LF; keep the pair together so no
    // chunk starts or ends with half of it.
    if end < len && bytes[end] == b'\n' && bytes[end - 1] == b'\r' {
        end += 1;
    } else if end + 1 < len && bytes[end] == b'\r' && bytes[end + 1] == b'\n' {
        end += 2;
    }
    end
}

/// Writes `text` as chunk files under `<cache_dir>/<hash>/` and returns them.
///
/// Existing files are left untouched, which makes a second call idempotent and
/// preserves a chunk the human edited by hand. Empty text has no chunks and
/// creates no directory.
pub fn materialize(
    cache_dir: &Path,
    hash: &str,
    text: &str,
    target: usize,
) -> Result<Vec<PathBuf>> {
    let dir = cache_subdir(cache_dir, hash)?;
    if target == 0 {
        return Err(ChunkError::BadArgs {
            message: String::from("chunk target must be greater than zero"),
        });
    }
    let ranges = split(text, target);
    if ranges.is_empty() {
        return Ok(Vec::new());
    }

    fs::create_dir_all(&dir).map_err(|err| io_error(&dir, "create directory", err))?;
    let width = name_width(ranges.len());
    let mut paths = Vec::with_capacity(ranges.len());
    for (index, &(start, end)) in ranges.iter().enumerate() {
        let path = dir.join(chunk_name(index + 1, width));
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                fs::write(&path, &text[start..end])
                    .map_err(|err| io_error(&path, "write chunk", err))?;
            }
            Err(err) => return Err(io_error(&path, "inspect chunk", err)),
        }
        paths.push(path);
    }
    Ok(paths)
}

/// Joins chunks `from..=to` (1-based, inclusive) into one string.
///
/// Only the requested chunk files are opened; the directory is listed for their
/// names, but no other chunk's content is read.
pub fn read_range(cache_dir: &Path, hash: &str, from: usize, to: usize) -> Result<String> {
    let dir = cache_subdir(cache_dir, hash)?;
    if from == 0 || to < from {
        return Err(ChunkError::BadRange { from, to });
    }
    let files = chunk_index(&dir)?;
    let mut text = String::new();
    for index in from..=to {
        let path = files.get(&index).ok_or_else(|| ChunkError::Missing {
            dir: dir.clone(),
            index,
        })?;
        append_chunk(path, &mut text)?;
    }
    Ok(text)
}

/// `<cache_dir>/<hash>`, rejecting a hash that could escape the cache directory.
fn cache_subdir(cache_dir: &Path, hash: &str) -> Result<PathBuf> {
    let mut components = Path::new(hash).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(name)), None) if !name.is_empty() => Ok(cache_dir.join(name)),
        _ => Err(ChunkError::BadArgs {
            message: format!("chunk hash {hash:?} must be a single path component"),
        }),
    }
}

/// Zero-pads chunk names to at least [`NAME_DIGITS`], wider for larger counts.
fn name_width(count: usize) -> usize {
    count.to_string().len().max(NAME_DIGITS)
}

/// `chunk-0001.md` for index 1 with a four-digit width.
fn chunk_name(index: usize, width: usize) -> String {
    format!("chunk-{index:0width$}.md")
}

/// Every materialized chunk under `dir`, keyed by its 1-based number.
///
/// A missing directory is an empty cache, not an I/O failure: nothing was
/// materialized yet, so the requested chunk is missing (a bad argument).
fn chunk_index(dir: &Path) -> Result<BTreeMap<usize, PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(err) => return Err(io_error(dir, "list chunks", err)),
    };
    let mut files = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|err| io_error(dir, "list chunks", err))?;
        let path = entry.path();
        if let Some(index) = chunk_number(&path) {
            if path.is_file() {
                files.insert(index, path);
            }
        }
    }
    Ok(files)
}

/// The 1-based number encoded in a `chunk-<digits>.md` name.
fn chunk_number(path: &Path) -> Option<usize> {
    let name = path.file_name()?.to_str()?;
    let digits = name.strip_prefix("chunk-")?.strip_suffix(".md")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Appends one chunk file through `File::seek` + `take`.
fn append_chunk(path: &Path, out: &mut String) -> Result<()> {
    let mut file = File::open(path).map_err(|err| io_error(path, "open chunk", err))?;
    let len = file
        .metadata()
        .map_err(|err| io_error(path, "inspect chunk", err))?
        .len();
    file.seek(SeekFrom::Start(0))
        .map_err(|err| io_error(path, "seek chunk", err))?;
    let mut chunk = String::new();
    file.take(len)
        .read_to_string(&mut chunk)
        .map_err(|err| io_error(path, "read chunk", err))?;
    out.push_str(&chunk);
    Ok(())
}

/// Wraps an I/O failure with the path and the action that failed.
fn io_error(path: &Path, action: &'static str, source: io::Error) -> ChunkError {
    ChunkError::Io {
        path: path.to_path_buf(),
        action,
        source,
    }
}
