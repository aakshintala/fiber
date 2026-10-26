//! The terminal's operation timers (`docs/state.md`, "Diagnostic logs"): the
//! diagnostic writer the loop's timed scopes share. Every timed place goes
//! through `log::diag::timed`, so the terminal's lines match every other
//! process's.

use std::sync::Arc;

/// The loop's timing state: the diagnostic writer its scopes time through.
/// `tui.log_read` and `tui.transcript` each write at most once per process;
/// the latches that enforce it land with the scopes.
pub(crate) struct Timing {
    /// The process's diagnostic writer. No scope reads it yet; the attach
    /// and frame scopes do.
    #[allow(
        dead_code,
        reason = "stored for the timing scopes, which do not read it yet"
    )]
    diag: Arc<log::diag::Diag>,
}

impl Timing {
    /// Shares `diag` with the loop's scopes.
    pub(crate) fn new(diag: Arc<log::diag::Diag>) -> Self {
        Self { diag }
    }

    /// A timing state whose writer records nothing: at `info` every timed
    /// call is one level check that returns `None`, so tests that assert no
    /// timing keep their signatures.
    #[cfg(test)]
    pub(crate) fn info() -> Self {
        Self::new(info_diag())
    }
}

/// An info-level `tui` writer that records nothing, for callers that pass a
/// writer but assert no timing.
#[cfg(test)]
pub(crate) fn info_diag() -> Arc<log::diag::Diag> {
    Arc::new(log::diag::Diag::new(
        &std::env::temp_dir(),
        log::diag::Process::Tui,
        log::diag::Level::Info,
        Arc::new(support::clock::System),
    ))
}
