//! The terminal's operation timers (`docs/state.md`, "Diagnostic logs"): the
//! diagnostic writer the loop's timed scopes share. Every timed place goes
//! through `log::diag::timed`, so the terminal's lines match every other
//! process's.

use std::sync::Arc;

use contract::diag::Op;

/// The loop's timing state: the diagnostic writer its scopes share, and
/// the attach latches. `tui.log_read` and `tui.transcript` each write at
/// most once per process: a reconnect, a delegate swap or a second attach
/// never writes a second line.
pub(crate) struct Timing {
    diag: Arc<log::diag::Diag>,
    log_read: bool,
    transcript: bool,
    attach_summary: bool,
    tail_wait: bool,
    tail_install: bool,
}

impl Timing {
    /// Shares `diag` with the loop's scopes; both attach latches start
    /// unset.
    pub(crate) fn new(diag: Arc<log::diag::Diag>) -> Self {
        Self {
            diag,
            log_read: false,
            transcript: false,
            attach_summary: false,
            tail_wait: false,
            tail_install: false,
        }
    }

    /// A timing state whose writer records nothing: at `info` every timed
    /// call is one level check that returns `None`, so tests that assert no
    /// timing keep their signatures.
    #[cfg(test)]
    pub(crate) fn info() -> Self {
        Self::new(info_diag())
    }

    /// Starts the first render's guard, from entry to the first draw. The
    /// caller discards it on every pre-draw return and drops it after the
    /// draw: a line there times work that happened.
    pub(crate) fn first_render(&self) -> Option<log::diag::Timer> {
        log::diag::timed(&self.diag, Op::TuiFirstRender, None, None)
    }

    /// Starts the first attach's first fetched page guard, once per
    /// process. The caller holds it across the first actual `page_in`
    /// `hub.fetch` attempt to completion, whatever its outcome: an
    /// attempted read is work.
    pub(crate) fn log_read(&mut self) -> Option<log::diag::Timer> {
        if std::mem::replace(&mut self.log_read, true) {
            return None;
        }
        log::diag::timed(&self.diag, Op::TuiLogRead, None, None)
    }

    /// Starts the first successful page-load guard, once per process.
    /// The caller holds it across the first successful `app.load`; a
    /// failed fetch never takes it, and an attach with no stream returns
    /// before it, so either leaves the latch for the next load.
    pub(crate) fn transcript(&mut self) -> Option<log::diag::Timer> {
        if std::mem::replace(&mut self.transcript, true) {
            return None;
        }
        log::diag::timed(&self.diag, Op::TuiTranscript, None, None)
    }

    /// Starts a frame's guard. A frame within its 16 ms budget writes no
    /// line; the suppression lives in the timer's drop.
    pub(crate) fn frame(&self) -> Option<log::diag::Timer> {
        log::diag::timed(&self.diag, Op::TuiFrame, None, None)
    }

    /// Starts the extension load's guard. Loading runs once before the
    /// first frame, so no latch: one line per process.
    pub(crate) fn extensions(&self) -> Option<log::diag::Timer> {
        log::diag::timed(&self.diag, Op::TuiExtensions, None, None)
    }

    /// Starts the opening summary subscribe's guard, once per process.
    /// The loop drops it once the open leaves the summary stage, so one
    /// line covers the send through the acceptance that queues the
    /// session's commands line.
    pub(crate) fn attach_summary(&mut self) -> Option<log::diag::Timer> {
        if std::mem::replace(&mut self.attach_summary, true) {
            return None;
        }
        log::diag::timed(&self.diag, Op::TuiAttachSummary, None, None)
    }

    /// Starts the open's pre-frame tail read guard, once per process.
    /// The caller holds it across the whole tail phase to completion,
    /// whatever its outcome; the post-frame count never takes it.
    pub(crate) fn tail_wait(&mut self) -> Option<log::diag::Timer> {
        if std::mem::replace(&mut self.tail_wait, true) {
            return None;
        }
        log::diag::timed(&self.diag, Op::TuiTailWait, None, None)
    }

    /// Starts one preview fold's guard. Each tail answer folds the
    /// candidate separately, so no latch: every fold writes its own
    /// line.
    pub(crate) fn preview_fold(&self) -> Option<log::diag::Timer> {
        log::diag::timed(&self.diag, Op::TuiPreviewFold, None, None)
    }

    /// Starts the open's pre-frame install guard, once per process.
    /// The caller holds it across the preview install to completion,
    /// whatever its outcome. The install puts the newest candidate on
    /// screen without another fold (`docs/tui.md`, "History and
    /// paging"); when no answer folded one, the install folds the
    /// leftover candidate first, under this same guard.
    pub(crate) fn tail_install(&mut self) -> Option<log::diag::Timer> {
        if std::mem::replace(&mut self.tail_install, true) {
            return None;
        }
        log::diag::timed(&self.diag, Op::TuiTailInstall, None, None)
    }
}

/// Drops `timer` without writing: setup work before a frame that never
/// drew must never land as that frame's line.
pub(crate) fn discard(timer: Option<log::diag::Timer>) {
    if let Some(timer) = timer {
        timer.discard();
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
