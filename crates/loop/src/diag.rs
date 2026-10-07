//! The session's diagnostic log at `logs/session-<id>.log`
//! (`docs/state.md`, "Diagnostic logs"): one JSON object per line for each
//! `host.log` line. A session with nothing to record writes no file.

use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::SessionId;
use contract::clock::Clock;

/// Writes `host.log` lines to the session's diagnostic file. One writer
/// per file, owned by the session's loop.
pub(crate) struct SessionDiag {
    log: PathBuf,
    session: SessionId,
    clock: Arc<dyn Clock>,
}

impl SessionDiag {
    pub(crate) fn new(home: &Path, session: SessionId, clock: Arc<dyn Clock>) -> Self {
        Self {
            log: home.join("logs").join(format!("session-{}.log", session.0)),
            session,
            clock,
        }
    }

    /// Appends one `extension_log` line. A write failure is ignored: it
    /// never stops the session.
    pub(crate) fn extension_log(&self, extension: &str, message: &str) {
        if let Some(parent) = self.log.parent() {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .unwrap_or(());
        }
        // The fields are in the order `docs/state.md` lists them. They
        // are formatted by hand: `loop` has no `serde` dependency, and a
        // `serde_json::Map` would order them alphabetically.
        let line = format!(
            "{{\"ts\":{},\"level\":{},\"process\":{},\"session_id\":{},\"code\":{},\"message\":{}}}\n",
            wall_ms(self.clock.wall()),
            quoted("info"),
            quoted("session"),
            quoted(&self.session.0),
            quoted("extension_log"),
            quoted(&format!("{extension}: {message}")),
        );
        append(&self.log, line.as_bytes());
    }
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn wall_ms(wall: std::time::SystemTime) -> u64 {
    wall.duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        .try_into()
        .unwrap_or(u64::MAX)
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

#[cfg(test)]
#[path = "diag_tests.rs"]
mod tests;
