use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use super::atomic::{locked, write_atomic};
use crate::error::ConfigError;
use crate::home::plain;

/// Appends `line` to a line-based file under its lock, creating the
/// directory: reads the file, adds the line, and renames a temporary file
/// over it, so a reader sees the old file or the new one, never half
/// (`docs/state.md`, "Concurrent access").
pub(crate) fn append_line(file: &Path, line: &str) -> Result<(), ConfigError> {
    let io = |source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    };
    let _lock = locked(file)?;
    let mut current = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
        Err(source) => return Err(io(source)),
    };
    if !current.is_empty() && !current.ends_with(b"\n") {
        current.push(b'\n');
    }
    current.extend_from_slice(line.as_bytes());
    current.push(b'\n');
    write_atomic(file, &current, 0o666)
}

/// Deletes physical line `line` of `file`, counted from 1, when it still
/// reads `text`: `true` when it removed. Every other byte is kept,
/// including blank lines, unknown keys, CRLF endings and a final line
/// with no newline. A line is cut as `str::lines` cuts it. A missing
/// file, a line number of 0 or past the end, or a line whose text
/// changed removes nothing and answers `false`, writing nothing; a
/// missing file gets no directory or lock file. The lock is held from
/// the read to the rename (`docs/state.md`, "Concurrent access").
pub(crate) fn remove_line(file: &Path, line: usize, text: &str) -> Result<bool, ConfigError> {
    let io = |source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    };
    if !plain(file, false)? {
        return Ok(false);
    }
    let _lock = locked(file)?;
    // A file deleted after the check above reports the missing read as an I/O error.
    let current = fs::read(file).map_err(io)?;
    let segments: Vec<&[u8]> = current.split_inclusive(|b| *b == b'\n').collect();
    let Some(segment) = line.checked_sub(1).and_then(|index| segments.get(index)) else {
        return Ok(false);
    };
    let mut held = *segment;
    let mut newline = false;
    if let Some(rest) = held.strip_suffix(b"\n") {
        held = rest;
        newline = true;
    }
    if newline {
        held = held.strip_suffix(b"\r").unwrap_or(held);
    }
    if held != text.as_bytes() {
        return Ok(false);
    }
    let mut rest = Vec::new();
    for (index, other) in segments.iter().enumerate() {
        if index + 1 != line {
            rest.extend_from_slice(other);
        }
    }
    write_atomic(file, &rest, 0o666)?;
    Ok(true)
}
