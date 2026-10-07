//! Copying a session's log and artifacts into a new directory
//! (`docs/invocation.md`, "Exporting a session"): `fiber sessions export`.
//! The export is the log as recorded: the complete lines written so far,
//! and nothing but `events.jsonl` and `artifacts/`.

use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use crate::{ARTIFACTS, EVENTS, Error, io_at, read::complete_len};

/// Copies the session in `dir` into `target`: its complete log lines as
/// `events.jsonl` and its `artifacts/` tree. `target` itself is created
/// with mode 0700; missing parents are created as usual. A `target` that already exists is refused,
/// and on any later failure `target` is removed again, so an export never
/// leaves a half-written directory behind.
pub fn export(dir: &Path, target: &Path) -> Result<(), Error> {
    let source = dir.join(EVENTS);
    let bytes = fs::read(&source).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            Error::NotFound(dir.to_owned())
        } else {
            io_at(&source)(e)
        }
    })?;
    let complete = complete_len(&bytes);
    if let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(io_at(parent))?;
    }
    // The refusal is the creation itself failing, so no check races it.
    if let Err(e) = fs::DirBuilder::new().mode(0o700).create(target) {
        if e.kind() == io::ErrorKind::AlreadyExists {
            return Err(Error::Exists(target.to_owned()));
        }
        return Err(io_at(target)(e));
    }
    let excluded = fs::canonicalize(target).map_err(io_at(target))?;
    let copied = write_export(
        bytes.get(..complete).unwrap_or_default(),
        dir,
        target,
        &excluded,
    );
    if copied.is_err() {
        // Best effort: the original error is reported either way.
        fs::remove_dir_all(target).unwrap_or(());
    }
    copied
}

/// Writes the complete log lines and the artifacts tree into `target`,
/// which `export` just created. The log was read before the artifacts are
/// copied, so every artifact a copied line names already exists.
fn write_export(complete: &[u8], dir: &Path, target: &Path, excluded: &Path) -> Result<(), Error> {
    let events = target.join(EVENTS);
    fs::write(&events, complete).map_err(io_at(&events))?;
    copy_artifacts(&dir.join(ARTIFACTS), &target.join(ARTIFACTS), excluded)
}

/// Copies the artifacts tree at `source` into `target`. A symlink is
/// recreated as a link and never read through; a directory recurses;
/// every other entry is copied as bytes. A session with no `artifacts/`
/// gets an empty one in the export.
fn copy_artifacts(source: &Path, target: &Path, excluded: &Path) -> Result<(), Error> {
    let entries = match fs::read_dir(source) {
        Ok(entries) => Some(entries),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_at(source)(e)),
    };
    fs::create_dir(target).map_err(io_at(target))?;
    let Some(entries) = entries else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry.map_err(|e| io_at(source)(e))?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        let file_type = entry.file_type().map_err(|e| io_at(&from)(e))?;
        if file_type.is_symlink() {
            let original = fs::read_link(&from).map_err(|e| io_at(&from)(e))?;
            std::os::unix::fs::symlink(&original, &to).map_err(|e| io_at(&to)(e))?;
        } else if file_type.is_dir() {
            // The export directory is never part of what it exports: a
            // target inside the source tree would otherwise copy itself.
            if fs::canonicalize(&from).map_err(io_at(&from))?.as_path() == excluded {
                continue;
            }
            copy_artifacts(&from, &to, excluded)?;
        } else {
            fs::copy(&from, &to).map_err(|e| io_at(&from)(e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
