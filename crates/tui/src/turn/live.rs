//! The live turn's wait to retry (`docs/tui.md`, "The working line"): a
//! failed model call's schedule, the attempt about to be made, and when
//! it was scheduled, so the working line counts its wait down.

use contract::events::RetryScheduled;

use super::Turn;

/// A failed model call waiting to retry, with the number of the attempt
/// about to be made and the schedule's wall time.
#[derive(Clone, Debug)]
pub(crate) struct PendingRetry {
    /// The failed call's schedule.
    pub(crate) retry: RetryScheduled,
    /// The attempt about to be made.
    pub(crate) attempt: u32,
    /// The `retry_scheduled` envelope's `ts`, in milliseconds.
    pub(crate) ts: u64,
}

impl Turn {
    /// The wait to retry, while one pends.
    pub(crate) fn pending_retry(&self) -> Option<&PendingRetry> {
        self.retry.as_ref()
    }
}
