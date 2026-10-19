//! Removes a session directory (`docs/invocation.md`, "Deleting and
//! pruning"). Only `log` removes the session directory, holds its lock,
//! or mints `seq` (`docs/architecture.md`).

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, fstat, open, openat, statat, unlinkat};

use super::{Error, SessionLock};

/// Whether `dir` is a plain directory: `symlink_metadata` succeeds and is
/// a directory. A link to a directory is not plain, so the hub's lookup
/// refuses it before taking a lock.
pub fn is_plain_dir(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir())
}

/// Removes `dir`: its log first, then every other entry, then the
/// directory itself. The caller holds `dir`'s lock through `lock` and
/// keeps it until after this returns; the lock itself is never taken or
/// released here. A symlink, a non-directory or a missing path is
/// [`Error::NotFound`] and nothing is touched.
pub fn delete_session(dir: &Path, _lock: &SessionLock) -> Result<(), Error> {
    let (fd, identity) = open_session_dir(dir)?;
    delete_opened(dir, &fd, identity)
}

/// Opens `dir` without following a link and records its `(dev, ino)`.
/// Anything but a plain directory is [`Error::NotFound`].
fn open_session_dir(dir: &Path) -> Result<(OwnedFd, (u64, u64)), Error> {
    let fd: OwnedFd = open(
        dir,
        OFlags::RDONLY
            .union(OFlags::DIRECTORY)
            .union(OFlags::NOFOLLOW)
            .union(OFlags::CLOEXEC),
        Mode::empty(),
    )
    .map_err(|_| Error::NotFound(dir.to_owned()))?;
    let stat = fstat(&fd).map_err(|_| Error::NotFound(dir.to_owned()))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Err(Error::NotFound(dir.to_owned()));
    }
    Ok((fd, (stat.st_dev, stat.st_ino)))
}

/// Whether `dir` still names the opened directory: its link metadata
/// carries the recorded `(dev, ino)`. A rename-and-symlink swap after the
/// open fails this, so nothing further is removed.
fn identity_holds(dir: &Path, expected: (u64, u64)) -> bool {
    std::fs::symlink_metadata(dir)
        .ok()
        .is_some_and(|meta| (meta.dev(), meta.ino()) == expected)
}

/// Removes everything under the opened directory and the directory
/// itself, checking the recorded identity before each unlink. Every
/// removal is relative to the opened fd, never by path, so a swapped
/// path cannot reach another directory's files.
fn delete_opened(dir: &Path, fd: &OwnedFd, expected: (u64, u64)) -> Result<(), Error> {
    if !identity_holds(dir, expected) {
        return Err(Error::NotFound(dir.to_owned()));
    }
    let log = dir.join(super::EVENTS);
    unlinkat(fd, "events.jsonl", AtFlags::empty()).map_err(|source| io_at(&log, source))?;
    for name in entry_names(dir, fd)? {
        if name.as_os_str() == OsStr::new(super::EVENTS) {
            continue;
        }
        if !identity_holds(dir, expected) {
            return Err(Error::NotFound(dir.to_owned()));
        }
        remove_entry(dir, fd, name.as_os_str(), dir, expected)?;
    }
    if !identity_holds(dir, expected) {
        return Err(Error::NotFound(dir.to_owned()));
    }
    remove_dir_itself(dir)
}

/// The entry names of the opened directory, in name order, without
/// `.` and `..`.
fn entry_names(dir: &Path, fd: &OwnedFd) -> Result<Vec<OsString>, Error> {
    let listing = fd.try_clone().map_err(|source| Error::Io {
        path: dir.to_owned(),
        source,
    })?;
    let listing = Dir::read_from(listing).map_err(|source| Error::Io {
        path: dir.to_owned(),
        source: io::Error::from(source),
    })?;
    let mut names: Vec<OsString> = listing
        .flatten()
        .map(|entry| OsStr::from_bytes(entry.file_name().to_bytes()).to_os_string())
        .filter(|name| name != "." && name != "..")
        .collect();
    names.sort();
    Ok(names)
}

/// Removes `name` under the opened directory: a subdirectory is emptied
/// without following links and then removed, anything else is unlinked
/// without being followed. `dir` is only for errors; the removal never
/// goes by path. Every identity check is against the session directory
/// `root`, never a subdirectory.
fn remove_entry(
    dir: &Path,
    fd: &OwnedFd,
    name: &OsStr,
    root: &Path,
    expected: (u64, u64),
) -> Result<(), Error> {
    let path = dir.join(Path::new(name));
    let stat = statat(fd, Path::new(name), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|source| io_at(&path, source))?;
    if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
        remove_children(&path, fd, name, root, expected)?;
        if !identity_holds(root, expected) {
            return Err(Error::NotFound(root.to_owned()));
        }
        unlinkat(fd, Path::new(name), AtFlags::REMOVEDIR).map_err(|source| io_at(&path, source))?;
    } else {
        unlinkat(fd, Path::new(name), AtFlags::empty()).map_err(|source| io_at(&path, source))?;
    }
    Ok(())
}

/// Empties the subdirectory `name` under `fd`, opening it without
/// following a link. A link met where a directory stood is unlinked by
/// the caller instead of being opened.
fn remove_children(
    path: &Path,
    fd: &OwnedFd,
    name: &OsStr,
    root: &Path,
    expected: (u64, u64),
) -> Result<(), Error> {
    let child: OwnedFd = openat(
        fd,
        Path::new(name),
        OFlags::RDONLY
            .union(OFlags::DIRECTORY)
            .union(OFlags::NOFOLLOW)
            .union(OFlags::CLOEXEC),
        Mode::empty(),
    )
    .map_err(|source| io_at(path, source))?;
    for sub in entry_names(path, &child)? {
        if !identity_holds(root, expected) {
            return Err(Error::NotFound(root.to_owned()));
        }
        remove_entry(path, &child, sub.as_os_str(), root, expected)?;
    }
    Ok(())
}

/// Removes the opened directory itself through its parent, after the same
/// identity check. A swap of a component above `dir` between the open
/// and this unlink is not guarded; only the session directory's own
/// identity is.
fn remove_dir_itself(dir: &Path) -> Result<(), Error> {
    let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
        return Err(Error::Io {
            path: dir.to_owned(),
            source: io::Error::other("the session directory has no parent"),
        });
    };
    let parent_fd: OwnedFd = open(
        parent,
        OFlags::RDONLY
            .union(OFlags::DIRECTORY)
            .union(OFlags::CLOEXEC),
        Mode::empty(),
    )
    .map_err(|source| io_at(dir, source))?;
    unlinkat(parent_fd, Path::new(name), AtFlags::REMOVEDIR)
        .map_err(|source| io_at(dir, source))?;
    Ok(())
}

/// An I/O failure at `path`, from a `rustix` `Errno`.
fn io_at(path: &Path, source: rustix::io::Errno) -> Error {
    Error::Io {
        path: path.to_owned(),
        source: io::Error::from(source),
    }
}

#[cfg(test)]
#[path = "delete_tests.rs"]
mod tests;
