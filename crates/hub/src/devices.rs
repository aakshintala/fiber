//! Paired device records (`docs/state.md`, "Paired devices"): one file per
//! device under `hub/devices/`, holding the SHA-256 hash of its device
//! token, never the token. The hub reads only the hash on every
//! `authenticate`, never cached, so a deleted record refuses the next
//! connection.

use std::fs;
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use ring::digest;
use serde_json::Value;

/// Whether `hub/devices/` holds a record whose hash is the SHA-256 of
/// `token`. Anything that is not a regular file holding that hash is
/// skipped, fail closed: a missing directory, an unreadable file, a
/// directory or a symlink, non-JSON, a missing or non-string hash, or
/// uppercase hex. Records open with `O_NOFOLLOW`, so a symlink is refused
/// at open even when an entry is swapped between the directory read and
/// the open. `O_NONBLOCK` prevents a FIFO from stalling authentication
/// before its opened-file metadata can reject it.
#[cfg_attr(
    false,
    mutants::skip,
    reason = "On Unix, O_NOFOLLOW and O_NONBLOCK are distinct flag bits, so OR and XOR produce the same flags; the symlink and FIFO tests separately pin each flag's behavior."
)]
pub(crate) fn holds(home: &Path, token: &str) -> bool {
    let want = sha256_hex(token.as_bytes());
    let entries = match fs::read_dir(home.join("hub").join("devices")) {
        Ok(entries) => entries,
        Err(_) => return false,
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
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
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
