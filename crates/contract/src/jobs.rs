//! The job seam (`docs/architecture.md`, "The call rules"): what crosses
//! between `jobs` and `tools`, as data and closures, the way [`crate::inbox::Ack`]
//! does. `jobs` and `tools` do not depend on each other.

use std::fmt;
use std::path::PathBuf;

use crate::ErrorCode;
use crate::events::{JobCompleted, JobStarted};
use crate::shapes::Failure;

/// What opening a job asks for. The opener supplies [`Stop`]; the registry
/// mints the id and the output file.
pub struct Opening {
    /// The tool that started the job, recorded on `job_started`.
    pub tool: String,
    /// A short description, recorded on `job_started`.
    pub description: String,
    /// Asks the job to stop. The registry calls it at most once.
    pub stop: Stop,
}

/// Asks the job to stop. It returns at once. A second call is a no-op for
/// the job: the opener's closure is what makes the second call do nothing.
pub struct Stop(
    /// The opener's stop. Returns at once.
    pub Box<dyn Fn() + Send + Sync>,
);

impl fmt::Debug for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Stop(..)")
    }
}

/// A job the registry opened: its `job_started` payload, the output file,
/// and the [`End`] that reports how it finished.
pub struct Opened {
    /// The `job_started` payload. `output_path` is relative to the session
    /// directory.
    pub started: JobStarted,
    /// The output file, absolute.
    pub path: PathBuf,
    /// The output file, open for writing.
    pub file: std::fs::File,
    /// Reports how the job ended, once.
    pub end: End,
}

/// Reports how the job ended, once. Dropped uncalled, the job is recorded
/// failed: a runner that returns on an error path without reporting would
/// otherwise leave the job running. Calling [`End::end`] disarms the drop,
/// so a job is recorded once. A panic aborts the process; this is not a
/// panic handler.
pub struct End {
    job_id: crate::JobId,
    report: Option<Box<dyn FnOnce(JobCompleted) + Send>>,
}

impl End {
    /// An end that reports through `report`. The registry builds one per job.
    pub fn new(job_id: crate::JobId, report: Box<dyn FnOnce(JobCompleted) + Send>) -> Self {
        Self {
            job_id,
            report: Some(report),
        }
    }

    /// Reports `completed` and disarms the drop.
    pub fn end(mut self, completed: JobCompleted) {
        if let Some(report) = self.report.take() {
            report(completed);
        }
    }
}

impl Drop for End {
    fn drop(&mut self) {
        let Some(report) = self.report.take() else {
            return;
        };
        report(JobCompleted {
            job_id: self.job_id.clone(),
            status: crate::events::Outcome::Failed,
            error: Some(Failure {
                code: ErrorCode::ToolError,
                message: "The job ended without a result.".to_owned(),
                retry_after: None,
                provider: None,
            }),
            process: None,
            output_tail: None,
        });
    }
}

impl fmt::Debug for End {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("End(..)")
    }
}

/// Opens a running job. `jobs::Registry` and test fakes both implement it,
/// so `tools` can move a command without depending on `jobs`
/// (`docs/architecture.md`, "The call rules").
pub trait Jobs: Send + Sync {
    /// Opens a running job; see `jobs::Registry::open`.
    fn open(&self, opening: Opening) -> Result<Opened, OpenError>;

    /// Sends the job's [`Stop`] unless it was already sent. True when the
    /// job is running, false when it is unknown or has ended. Returns at
    /// once; the job ends on its own thread.
    fn stop(&self, job_id: &crate::JobId) -> bool;

    /// Asks every registered foreground call to move to the background
    /// (`docs/tools.md`, "Moving to the background"). Returns how many will.
    /// Returns at once.
    fn background(&self) -> usize;

    /// Registers a running foreground call. Held weakly: the call holds the
    /// strong reference while it runs in the foreground.
    fn foreground(&self, call: Foreground);
}

/// A running foreground call, as the `background` command reaches it. The
/// call holds the `Arc` while it runs in the foreground and drops it when it
/// finishes or moves. The closure asks the call to move and returns true
/// when it was still in the foreground and will now move.
pub struct Foreground(
    /// The call's move request, held weakly.
    pub std::sync::Weak<dyn Fn() -> bool + Send + Sync>,
);

impl fmt::Debug for Foreground {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Foreground(..)")
    }
}

/// The output file could not be created. Nothing was recorded.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// Creating the file failed.
    #[error("the job's output file {path} could not be created: {source}")]
    Io {
        /// The file that could not be created.
        path: PathBuf,
        /// The filesystem error.
        source: std::io::Error,
    },
}

impl OpenError {
    /// The stable code (`docs/errors.md`). The output file could not be
    /// created, which is the tool's failure.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Io { .. } => ErrorCode::ToolError,
        }
    }
}

/// One durable job line a call returns. The loop writes them, in order,
/// under the call's action, immediately before that call's
/// `tool_call_completed`.
#[derive(Debug, Clone, PartialEq)]
pub enum JobRecord {
    /// `job_started`.
    Started(JobStarted),
    /// `job_completed`.
    Completed(JobCompleted),
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;
