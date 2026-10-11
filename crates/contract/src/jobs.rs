//! The job seam (`docs/architecture.md`, "The call rules"): what crosses
//! between `jobs` and `tools`, as data and closures, the way [`crate::inbox::Ack`]
//! does. `jobs` and `tools` do not depend on each other.

use std::fmt;
use std::path::PathBuf;

use crate::ErrorCode;
use crate::events::{DelegateFinished, DelegateStarted, JobCompleted, JobLine, JobStarted};

/// What opening a job asks for. The opener supplies [`Stop`]; the registry
/// mints the id and the output file.
pub struct Opening {
    /// The tool that started the job, recorded on `job_started`.
    pub tool: String,
    /// A short description, recorded on `job_started`.
    pub description: String,
    /// Asks the job to stop. The registry calls it at most once.
    pub stop: Stop,
    /// Types into the job; `Some` only for a job started with `tty`.
    pub input: Option<Input>,
    /// The job delivers lines to the model: true only for a monitor
    /// (`docs/tools.md`, "Background jobs").
    pub lines: bool,
}

/// Sends one batch of a monitor's lines to the model. The job's drive
/// thread calls it, and calls every one before the job's [`End`], so the
/// lines reach the inbox before the job's completion notice.
pub struct Lines(
    /// The registry's send.
    pub Box<dyn Fn(JobLine) + Send + Sync>,
);

impl fmt::Debug for Lines {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Lines(..)")
    }
}

/// Writes bytes to a job's terminal (`docs/tools.md`, "Terminal (`tty`)").
/// It writes in chunks and checks the cancel between them, waiting on the
/// clock when the terminal's queue is full. It returns how many bytes it
/// wrote: fewer than given means the cancel fired.
pub struct Input(
    /// The opener's write to the terminal.
    pub Box<Writes>,
);

type Writes = dyn Fn(&[u8], &dyn crate::clock::Clock, &dyn crate::tool::Cancel) -> std::io::Result<usize>
    + Send
    + Sync;

impl fmt::Debug for Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Input(..)")
    }
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
    /// Where the job's `job_delta` lines go (`docs/events.md`, `job_delta`).
    pub emit: std::sync::Arc<dyn crate::emit::Emit>,
    /// Where a monitor's batches go; `Some` only when [`Opening::lines`]
    /// was true.
    pub lines: Option<Lines>,
}

/// Reports how the job ended, once. The registry records a job whose `End`
/// is dropped uncalled as failed `indeterminate` (`jobs::Registry::open`).
pub struct End(
    /// The registry's report.
    pub Box<dyn FnOnce(JobCompleted) + Send>,
);

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

    /// The jobs still running, in start order. A job's end is sent to the
    /// inbox before this stops listing it, so once a job is gone from here
    /// its notice is already waiting there.
    fn running(&self) -> Vec<crate::JobId>;

    /// Sends each running Fiber delegate its stop, once, and returns how
    /// many were sent one. Ordinary jobs keep running. The loop calls it
    /// once when a turn ends `budget_exceeded`, through its one
    /// budget-end function, and nothing else calls it
    /// (`docs/loop.md`, "Spending budget"). Returns at once.
    fn stop_delegates(&self) -> usize;

    /// Sends each later job end to `inbox` as a
    /// [`crate::inbox::Delivery::Job`], and each later monitor batch as a
    /// [`crate::inbox::Delivery::JobLine`], so the loop can wake the model
    /// with it (`docs/tools.md`, "Background jobs"). Before this, an end or
    /// a batch sends nothing.
    fn deliver_to(&self, inbox: std::sync::mpsc::Sender<crate::inbox::Delivery>);

    /// Writes `text` to a running job started with `tty`, as the `jobs`
    /// action `write` does for the model (`docs/tools.md`, "Background
    /// jobs"), and is accepted once written. The job's output reaches the
    /// client on its stream as usual. Test fakes keep the default, which
    /// reports every job as not running.
    fn write(&self, job_id: &crate::JobId, text: &str) -> Result<(), WriteError> {
        let _ = (job_id, text);
        Err(WriteError::NotRunning)
    }
}

/// `write` could not reach the job.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// No job has this id, or it already ended.
    #[error("that job is not running")]
    NotRunning,
    /// The job was not started with `tty`.
    #[error("that job was not started with `tty`")]
    NotTty,
    /// Writing to the terminal failed.
    #[error("writing to the job's terminal failed: {0}")]
    Io(#[from] std::io::Error),
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
    /// `delegate_started`: a Fiber delegate's run began.
    DelegateStarted(DelegateStarted),
    /// `delegate_finished`: a Fiber delegate's run ended, written just
    /// before `job_completed`.
    DelegateFinished(DelegateFinished),
    /// `job_completed`.
    Completed(JobCompleted),
}
