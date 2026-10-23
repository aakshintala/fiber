//! Paired device records (`docs/state.md`, "Paired devices"): one file per
//! device under `hub/devices/`, holding the SHA-256 hash of its device
//! token, never the token. The hub reads only the hash on every
//! `authenticate`, never cached, so a deleted record refuses the next
//! connection.

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::Path;

use serde_json::Value;

pub(crate) use support::sha256_hex;

/// The open flags for a record: no symlink, and no wait for a FIFO's writer.
// `O_NOFOLLOW` and `O_NONBLOCK` are distinct bits, so OR and XOR give the same
// flags; the symlink and FIFO tests pin each flag's behaviour.
#[cfg_attr(false, mutants::skip)]
fn open_flags() -> i32 {
    libc::O_NOFOLLOW | libc::O_NONBLOCK
}

/// The file name of a record whose hash is the SHA-256 of `token` in
/// `hub/devices/`. Anything that is not a regular file holding that hash is
/// skipped, fail closed: a missing directory, an unreadable file, a
/// directory or a symlink, non-JSON, a missing or non-string hash, or
/// uppercase hex. Records open with `O_NOFOLLOW`, so a symlink is refused
/// at open even when an entry is swapped between the directory read and
/// the open. `O_NONBLOCK` prevents a FIFO from stalling authentication
/// before its opened-file metadata can reject it.
pub(crate) fn holder(home: &Path, token: &str) -> Option<String> {
    let want = sha256_hex(token.as_bytes());
    let entries = match fs::read_dir(home.join("hub").join("devices")) {
        Ok(entries) => entries,
        Err(_) => return None,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        #[cfg(test)]
        BEFORE_OPEN.with(|slot| {
            if let Some(hook) = slot.take() {
                hook();
            }
        });
        let mut file = match fs::OpenOptions::new()
            .read(true)
            .custom_flags(open_flags())
            .open(&path)
        {
            Ok(file) => file,
            Err(_) => continue,
        };
        let opened = match file.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if !opened.is_file() {
            continue;
        }
        let mut contents = String::new();
        if file.read_to_string(&mut contents).is_err() {
            continue;
        }
        let record: Value = match serde_json::from_str(&contents) {
            Ok(record) => record,
            Err(_) => continue,
        };
        if record.get("token_sha256").and_then(Value::as_str) == Some(want.as_str()) {
            return Some(entry.file_name().to_string_lossy().into_owned());
        }
    }
    None
}

/// The record's hash, read without following symlinks or opening special files.
pub(crate) fn token_hash(home: &Path, name: &std::ffi::OsStr) -> io::Result<String> {
    read_record(&home.join("hub/devices").join(name))?
        .get("token_sha256")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("The device record has no token hash."))
}

/// The complete record created by pairing.
#[derive(serde::Serialize)]
pub(crate) struct Record {
    pub(crate) name: String,
    pub(crate) paired_at: u64,
    pub(crate) last_connected_at: u64,
    pub(crate) token_sha256: String,
}

/// A listed device, identified by its file name rather than its JSON name.
pub(crate) struct Listed {
    pub(crate) name: String,
    pub(crate) paired_at: Option<u64>,
    pub(crate) last_connected_at: Option<u64>,
}

fn read_record(path: &Path) -> io::Result<Value> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(open_flags())
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("The device record is not a regular file."));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn write_value(home: &Path, name: &str, value: &Value) -> io::Result<()> {
    let dir = home.join("hub/devices");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    // A temporary outside devices cannot authenticate after a crash.
    let temporary = home.join("hub").join(format!(".{name}.tmp"));
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(value).map_err(io::Error::other)?)?;
        file.sync_all()?;
        fs::rename(&temporary, dir.join(name))
    })();
    if result.is_err() {
        fs::remove_file(&temporary).unwrap_or(());
    }
    result
}

/// Writes and file-syncs a record; the rename commits it.
pub(crate) fn write(home: &Path, record: &Record) -> io::Result<()> {
    write_value(
        home,
        &record.name,
        &serde_json::to_value(record).map_err(io::Error::other)?,
    )
}

/// Makes a committed record change durable across a crash.
pub(crate) fn sync_dir(home: &Path) -> io::Result<()> {
    #[cfg(test)]
    check_fault(Fault::Sync)?;
    fs::File::open(home.join("hub/devices"))?.sync_all()
}

/// Rewrites the connection time while preserving every other key.
pub(crate) fn touch(home: &Path, name: &str, at_ms: u64) -> io::Result<()> {
    let mut value = read_record(&home.join("hub/devices").join(name))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| io::Error::other("The device record is not an object."))?;
    object.insert("last_connected_at".to_owned(), Value::from(at_ms));
    write_value(home, name, &value)
}

/// Unlinks a regular record only; absence and special files are not found.
pub(crate) fn remove(home: &Path, name: impl AsRef<std::ffi::OsStr>) -> io::Result<bool> {
    let path = home.join("hub/devices").join(name.as_ref());
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Ok(false);
    }
    #[cfg(test)]
    check_fault(Fault::Unlink)?;
    fs::remove_file(path)?;
    Ok(true)
}

/// Whether any entry already occupies the name, without following symlinks.
pub(crate) fn exists(home: &Path, name: &str) -> io::Result<bool> {
    match fs::symlink_metadata(home.join("hub/devices").join(name)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Lists regular device records in file-name order, without hashes.
pub(crate) fn list(home: &Path) -> io::Result<Vec<Listed>> {
    let entries = match fs::read_dir(home.join("hub/devices")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut listed = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let record = read_record(&entry.path()).unwrap_or(Value::Null);
        listed.push(Listed {
            name: entry.file_name().to_string_lossy().into_owned(),
            paired_at: record.get("paired_at").and_then(Value::as_u64),
            last_connected_at: record.get("last_connected_at").and_then(Value::as_u64),
        });
    }
    listed.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(listed)
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    Unlink,
    Sync,
}

#[cfg(test)]
thread_local! {
    static FAULT: std::cell::Cell<Option<Fault>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn fault(fault: Fault) {
    FAULT.with(|slot| slot.set(Some(fault)));
}

#[cfg(test)]
fn check_fault(fault: Fault) -> io::Result<()> {
    FAULT.with(|slot| {
        if slot.get() == Some(fault) {
            slot.set(None);
            Err(io::Error::other("Injected device persistence failure."))
        } else {
            Ok(())
        }
    })
}

#[cfg(test)]
thread_local! {
    /// Runs once before a record's open, so the swap test can replace the
    /// record with a symlink there.
    static BEFORE_OPEN: std::cell::Cell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::Cell::new(None) };
}

/// Arms the one-shot hook before the next record's open.
/// Test only: production has no seam there.
#[cfg(test)]
pub(crate) fn before_open(hook: impl FnOnce() + 'static) {
    BEFORE_OPEN.with(|slot| slot.set(Some(Box::new(hook))));
}

#[cfg(test)]
#[path = "devices_tests.rs"]
mod tests;
