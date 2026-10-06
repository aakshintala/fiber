//! The hub's diagnostic log at `logs/hub.log` (`docs/state.md`, "Diagnostic
//! logs"): one JSON object per line, rotated past 10 MiB, pruned at start.
//!
//! Each line is `ts`, `level` (`error`, `warn` or `info`), `process`
//! (`hub`), `session_id` when one is known, `code` and `message`, in that
//! order. For a failure, `code` is its code from `docs/errors.md`. For one
//! of the hub's operations it is the operation's name. Nothing in `logs/`
//! holds a credential or token, prompt or model text, a tool's arguments or
//! a configuration value. A log write failure never stops the hub.

use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use contract::SessionId;
use contract::clock::Clock;
use serde::Serialize;

/// Past this size `hub.log` is renamed to `hub.log.1` before the next write
/// (`docs/state.md`, "Bounds").
const ROTATE_AT: u64 = 10 * 1024 * 1024;

/// Files in `logs/` and `crashes/` older than this are deleted when the hub
/// starts (`docs/state.md`, "Bounds").
const PRUNE_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Besides the old files, each directory keeps this many of its newest
/// (`docs/state.md`, "Bounds").
const PRUNE_KEEP: usize = 100;

/// The hub's diagnostic log. One writer, as a session log has.
pub(crate) struct Diag {
    log: PathBuf,
    clock: Arc<dyn Clock>,
}

impl Diag {
    /// Opens the log in `home`: creates `logs/` mode 0700, prunes `logs/`
    /// and `crashes/`, before the caller writes `hub_started`.
    pub(crate) fn open(home: &Path, clock: Arc<dyn Clock>) -> Self {
        let logs = home.join("logs");
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&logs)
            .unwrap_or(());
        prune(&logs, clock.wall());
        prune(&home.join("crashes"), clock.wall());
        Self {
            log: logs.join("hub.log"),
            clock,
        }
    }

    /// Writes an `info` line for one of the hub's operations.
    pub(crate) fn info(&self, code: &str, message: &str) {
        self.write("info", None, code, message);
    }

    /// Writes an `info` line naming the session, such as `session_started`.
    #[allow(
        dead_code,
        reason = "the relay writes it; it arrives with the connection handling"
    )]
    pub(crate) fn info_session(&self, session: &SessionId, code: &str, message: &str) {
        self.write("info", Some(session), code, message);
    }

    /// Writes a `warn` line, such as a session that failed to start.
    #[allow(
        dead_code,
        reason = "`start` writes it; it arrives with the connection handling"
    )]
    pub(crate) fn warn(&self, code: &str, message: &str) {
        self.write("warn", None, code, message);
    }

    /// Writes a `warn` line naming the session it concerns.
    #[allow(
        dead_code,
        reason = "`start` writes it; it arrives with the connection handling"
    )]
    pub(crate) fn warn_session(&self, session: &SessionId, code: &str, message: &str) {
        self.write("warn", Some(session), code, message);
    }

    fn write(&self, level: &str, session: Option<&SessionId>, code: &str, message: &str) {
        // A log write failure never stops the hub.
        rotate(&self.log);
        let line = Line {
            ts: wall_ms(self.clock.wall()),
            level,
            process: "hub",
            session_id: session,
            code,
            message,
        };
        let mut bytes = serde_json::to_vec(&line).unwrap_or_default();
        bytes.push(b'\n');
        append(&self.log, &bytes);
    }
}

/// One diagnostic line. The fields serialize in the order `docs/state.md`
/// lists them, and `session_id` is absent when none is known, never `null`.
#[derive(Debug, Serialize)]
struct Line<'a> {
    ts: u64,
    level: &'a str,
    process: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a SessionId>,
    code: &'a str,
    message: &'a str,
}

fn wall_ms(wall: SystemTime) -> u64 {
    wall.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Renames a `hub.log` over 10 MiB to `hub.log.1`, replacing any older one.
fn rotate(log: &Path) {
    let over = fs::metadata(log).is_ok_and(|meta| meta.len() > ROTATE_AT);
    if !over {
        return;
    }
    let mut previous = log.as_os_str().to_owned();
    previous.push(".1");
    fs::rename(log, Path::new(&previous)).unwrap_or(());
}

fn append(log: &Path, bytes: &[u8]) {
    use std::fs::OpenOptions;
    use std::io::Write;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .and_then(|mut file| file.write_all(bytes))
        .unwrap_or(());
}

/// Deletes the files in `dir` older than 30 days, then all but its newest
/// 100 files by mtime. A missing directory holds nothing to delete.
fn prune(dir: &Path, now: SystemTime) {
    let entries = fs::read_dir(dir).map(|read| read.filter_map(Result::ok).collect::<Vec<_>>());
    let Ok(entries) = entries else {
        return;
    };
    let mut kept: Vec<(SystemTime, PathBuf)> = Vec::new();
    for entry in entries {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|at| at + PRUNE_AFTER < now);
        if old {
            fs::remove_file(&path).unwrap_or(());
            continue;
        }
        let at = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        kept.push((at, path));
    }
    kept.sort();
    let drop = kept.len().saturating_sub(PRUNE_KEEP);
    for (_, path) in kept.into_iter().take(drop) {
        fs::remove_file(path).unwrap_or(());
    }
}

#[cfg(test)]
#[path = "diag_tests.rs"]
mod tests;
