//! The session's diagnostic log at `logs/session-<id>.log`
//! (`docs/state.md`, "Diagnostic logs"): one JSON object per line for each
//! `host.log` line. A session with nothing to record writes no file.

use std::path::Path;
use std::sync::Arc;

use contract::SessionId;
use contract::clock::Clock;
use contract::diag::Op;
use log::diag::{Diag, Level, Process, Severity, Timer};

/// Writes `host.log` lines to the session's diagnostic file, over the
/// shared writer. One writer per file, owned by the session's loop.
pub(crate) struct SessionDiag {
    log: Arc<Diag>,
    /// Whether the preamble scope has been emitted in this process: the
    /// first actual construction writes one `startup.preamble` line, and a
    /// same-session rebuild writes none.
    preamble_done: bool,
}

impl SessionDiag {
    pub(crate) fn new(home: &Path, session: SessionId, clock: Arc<dyn Clock>) -> Self {
        let log = Diag::new(home, Process::Session, Level::Info, clock);
        log.attach(&session);
        Self {
            log: Arc::new(log),
            preamble_done: false,
        }
    }

    /// Shares the session's attached writer: `extension_log`, request and
    /// memory lines go to one file with one session id. The caller sets
    /// the loop's factories first, so every line shares its identity.
    pub(crate) fn shared(log: Arc<Diag>) -> Self {
        Self {
            log,
            preamble_done: false,
        }
    }

    /// The shared writer, to construct the one operation timer every timed
    /// place goes through. Borrowing it ends at construction: the guard
    /// owns all emission state.
    pub(crate) fn writer(&self) -> &Diag {
        &self.log
    }

    /// Starts the one timer for `op`, or nothing at `info` before any
    /// clock read, through the writer above.
    pub(crate) fn timed(&self, op: Op) -> Option<Timer> {
        log::diag::timed(self.writer(), op, None, None)
    }

    /// Whether the first preamble construction already emitted its scope.
    pub(crate) fn preamble_done(&self) -> bool {
        self.preamble_done
    }

    /// Records that the first preamble construction entered its scope,
    /// so a same-session rebuild stays silent, including after a failure.
    pub(crate) fn set_preamble_done(&mut self) {
        self.preamble_done = true;
    }

    /// Writes the `peak_memory` line when debugging: best-effort, like
    /// every diagnostic write. A failed read writes nothing.
    pub(crate) fn peak_memory(&self) {
        self.log.peak_memory();
    }

    /// Appends one `extension_log` line. A write failure is ignored: it
    /// never stops the session.
    pub(crate) fn extension_log(&self, extension: &str, message: &str) {
        self.log.line(
            Severity::Info,
            None,
            "extension_log",
            &format!("{extension}: {message}"),
        );
    }
}

#[cfg(test)]
#[path = "diag_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "wire_tests.rs"]
mod wire_tests;
