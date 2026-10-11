//! Paired device records (`docs/state.md`, "Paired devices"): one file per
//! device under `hub/devices/`, holding the SHA-256 hash of its device
//! token, never the token. The hub reads only the hash on every
//! `authenticate`, never cached, so a deleted record refuses the next
//! connection.

use std::fs;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use ring::digest;
use serde_json::Value;

/// Whether `hub/devices/` holds a record whose hash is the SHA-256 of
/// `token`. Anything that is not a regular file holding that hash is
/// skipped, fail closed: a missing directory, an unreadable file, a
/// directory or a symlink, non-JSON, a missing or non-string hash, or
/// uppercase hex. Symlinks are never followed: the opened file's own
/// device and inode must equal the entry's.
pub(crate) fn holds(home: &Path, token: &str) -> bool {
    let want = sha256_hex(token.as_bytes());
    let entries = match fs::read_dir(home.join("hub").join("devices")) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let own = match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => meta,
            Ok(_) | Err(_) => continue,
        };
        #[cfg(test)]
        BETWEEN_STAT_AND_OPEN.with(|slot| {
            if let Some(hook) = slot.take() {
                hook();
            }
        });
        let mut file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(_) => continue,
        };
        let opened = match file.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if !opened.is_file() || opened.dev() != own.dev() || opened.ino() != own.ino() {
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
            return true;
        }
    }
    false
}

/// The lowercase hexadecimal SHA-256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    digest::digest(&digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
thread_local! {
    /// Runs once between a record's lstat and its open, so the swap test
    /// can replace the record with a symlink there.
    static BETWEEN_STAT_AND_OPEN: std::cell::Cell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::Cell::new(None) };
}

/// Arms the one-shot hook between the next record's lstat and its open.
/// Test only: production has no seam there.
#[cfg(test)]
pub(crate) fn between_stat_and_open(hook: impl FnOnce() + 'static) {
    BETWEEN_STAT_AND_OPEN.with(|slot| slot.set(Some(Box::new(hook))));
}

#[cfg(test)]
#[path = "devices_tests.rs"]
mod tests;
