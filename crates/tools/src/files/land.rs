//! How a file tool's bytes land (`docs/tools.md`, "How a change lands").
//!
//! A temporary file in the same directory is renamed over the target, so a
//! crash leaves the old file or the new one. A hard link is written in place,
//! because a rename would replace only one name.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use std::os::unix::fs::MetadataExt;

const BOM: &[u8] = b"\xEF\xBB\xBF";

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// Line ending taken from the first newline in a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    /// The first newline is `\n`.
    Lf,
    /// The first newline is `\r\n`. A file with no newline is this too: a new
    /// line the replacement adds uses `\n`.
    Crlf,
}

/// The existing file's first line ending. No newline means [`Ending::Lf`].
pub(crate) fn ending_of(bytes: &[u8]) -> Ending {
    match bytes.iter().position(|byte| *byte == b'\n') {
        Some(index) if index > 0 && bytes.get(index - 1) == Some(&b'\r') => Ending::Crlf,
        Some(_) | None => Ending::Lf,
    }
}

/// `content` stored with `existing`'s line endings and byte order mark.
///
/// A `\r\n` pair is a CRLF ending. A lone `\r` is a byte of the line, not an
/// ending, so it is kept.
pub(crate) fn shape_replacement(existing: &[u8], content: &str) -> Vec<u8> {
    let body = content.strip_prefix('\u{feff}').unwrap_or(content);
    let lf = body.replace("\r\n", "\n");
    let mut out = Vec::new();
    if existing.starts_with(BOM) {
        out.extend_from_slice(BOM);
    }
    match ending_of(existing) {
        Ending::Lf => out.extend_from_slice(lf.as_bytes()),
        Ending::Crlf => {
            for piece in lf.split_inclusive('\n') {
                if let Some(stripped) = piece.strip_suffix('\n') {
                    out.extend_from_slice(stripped.as_bytes());
                    out.extend_from_slice(b"\r\n");
                } else {
                    out.extend_from_slice(piece.as_bytes());
                }
            }
        }
    }
    out
}

/// Writes `bytes` to `target`, creating missing parents. A hard-linked file
/// is truncated in place. Anything else is written to a temp file and renamed.
pub(crate) fn land(target: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = target.parent()
        && parent != Path::new("")
    {
        fs::create_dir_all(parent)?;
    }
    match fs::symlink_metadata(target) {
        Ok(meta) if meta.file_type().is_file() && meta.nlink() > 1 => write_in_place(target, bytes),
        Ok(meta) => replace(target, bytes, Some(meta.permissions())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => replace(target, bytes, None),
        Err(err) => Err(err),
    }
}

/// `.<name>.fiber-<pid>-<n>.tmp` beside `target`.
pub(crate) fn temporary_name(target: &Path, pid: u32, n: u64) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());
    let file_name = format!(".{name}.fiber-{pid}-{n}.tmp");
    // An empty parent (`foo.txt`) joins as the name alone, same as no parent.
    match target.parent() {
        Some(parent) => parent.join(file_name),
        None => PathBuf::from(file_name),
    }
}

fn write_in_place(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).truncate(true).open(target)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn replace(
    target: &Path,
    bytes: &[u8],
    permissions: Option<std::fs::Permissions>,
) -> io::Result<()> {
    let path = fresh_temp(target)?;
    let mut temp = TempFile { path, armed: true };
    {
        let mut file = OpenOptions::new().write(true).open(&temp.path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    if let Some(permissions) = permissions {
        fs::set_permissions(&temp.path, permissions)?;
    }
    fs::rename(&temp.path, target)?;
    temp.armed = false;
    Ok(())
}

fn fresh_temp(target: &Path) -> io::Result<PathBuf> {
    let pid = std::process::id();
    for _ in 0..64 {
        let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = temporary_name(target, pid, n);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                drop(file);
                return Ok(path);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::other(
        "a temporary file could not be created beside the target",
    ))
}

/// Removes the temp file unless [`replace`] renamed it into place.
struct TempFile {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        remove_quietly(&self.path);
    }
}

/// Removes `path`, ignoring every error: Drop cannot report one, and the
/// caller already has the landing error.
// The NotFound arm and the catch-all do the same thing, so a mutant of the
// guard changes nothing.
#[cfg_attr(false, mutants::skip)]
fn remove_quietly(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(_err) => {}
    }
}

#[cfg(test)]
#[path = "land_tests.rs"]
mod tests;
