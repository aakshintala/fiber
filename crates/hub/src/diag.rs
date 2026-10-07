//! The hub's diagnostic log at `logs/hub.log` (`docs/state.md`, "Diagnostic
//! logs"): one JSON object per line, rotated past 10 MiB, pruned at start.
//!
//! Each line is `ts`, `level` (`error`, `warn`, `info` or `debug`),
//! `process` (`hub`), `session_id` when one is known, `code` and `message`,
//! in that order, and `data` on a `debug` line. For a failure, `code` is its
//! code from `docs/errors.md`. For one of the hub's operations it is the
//! operation's name. Nothing in `logs/`
//! holds a credential or token, prompt or model text, a tool's arguments or
//! a configuration value. A log write failure never stops the hub.

use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use contract::SessionId;
use contract::clock::Clock;
use log::diag::{Level, Process, Severity};

/// Past this size `hub.log` is renamed to `hub.log.1` before the next write
/// (`docs/state.md`, "Bounds").
const ROTATE_AT: u64 = 10 * 1024 * 1024;

/// Files in `logs/` and `crashes/` older than this are deleted when the hub
/// starts (`docs/state.md`, "Bounds").
const PRUNE_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Besides the old files, each directory keeps this many of its newest
/// (`docs/state.md`, "Bounds").
const PRUNE_KEEP: usize = 100;

/// The hub's diagnostic log, over the shared writer: one lock serializes
/// rotation and append, so concurrent connections never lose records to two
/// rotations.
pub(crate) struct Diag {
    log: log::diag::Diag,
}

impl Diag {
    /// Opens the log in `home`: creates `logs/` mode 0700, prunes `logs/`
    /// and `crashes/`, before the caller writes `hub_started`. The level is
    /// `info` until [`Diag::with_level`].
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
            log: log::diag::Diag::new(home, Process::Hub, Level::Info, clock).rotating(ROTATE_AT),
        }
    }

    /// Sets the level the configuration names, before the hub shares the log.
    pub(crate) fn with_level(self, level: Level) -> Self {
        Self {
            log: self.log.with_level(level),
        }
    }

    /// Reads the peak memory with `read` instead of the process's own,
    /// before the hub shares the log. Tests use it for a deterministic
    /// `peak_memory` value.
    #[cfg(test)]
    pub(crate) fn with_peak(self, read: fn() -> Option<u64>) -> Self {
        Self {
            log: self.log.with_peak(read),
        }
    }

    /// Runs `between` between the stop lines' two appends, before the hub
    /// shares the log. Test-only: it forces the race the pair closes.
    #[cfg(test)]
    pub(crate) fn with_between(self, between: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            log: self.log.with_between(between),
        }
    }

    /// Writes an `info` line for one of the hub's operations.
    pub(crate) fn info(&self, code: &str, message: &str) {
        self.log.line(Severity::Info, None, code, message);
    }

    /// Writes an `error` line for a failure with no session to hold it,
    /// such as a startup error: `code` is its code from `docs/errors.md`.
    pub(crate) fn error(&self, code: &str, message: &str) {
        self.log.line(Severity::Error, None, code, message);
    }

    /// Writes an `info` line naming the session, such as `session_started`.
    pub(crate) fn info_session(&self, session: &SessionId, code: &str, message: &str) {
        self.log.line(Severity::Info, Some(session), code, message);
    }

    /// Writes a `warn` line naming the session it concerns.
    pub(crate) fn warn_session(&self, session: &SessionId, code: &str, message: &str) {
        self.log.line(Severity::Warn, Some(session), code, message);
    }

    /// Writes `peak_memory` at the debug level immediately followed by
    /// `hub_stopped`, under one lock: a departing client's
    /// `client_disconnected` line cannot come between the pair.
    pub(crate) fn stopped(&self, message: &str) {
        self.log.peak_memory_then_info("hub_stopped", message);
    }
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
