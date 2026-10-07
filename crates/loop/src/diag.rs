//! The session's diagnostic log at `logs/session-<id>.log`
//! (`docs/state.md`, "Diagnostic logs"): one JSON object per line for each
//! `host.log` line. A session with nothing to record writes no file.

use std::path::Path;
use std::sync::Arc;

use contract::SessionId;
use contract::clock::Clock;
use log::diag::{Diag, Level, Process, Severity};

/// Writes `host.log` lines to the session's diagnostic file, over the
/// shared writer. One writer per file, owned by the session's loop.
pub(crate) struct SessionDiag {
    log: Arc<Diag>,
}

impl SessionDiag {
    pub(crate) fn new(home: &Path, session: SessionId, clock: Arc<dyn Clock>) -> Self {
        let log = Diag::new(home, Process::Session, Level::Info, clock);
        log.attach(&session);
        Self { log: Arc::new(log) }
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
