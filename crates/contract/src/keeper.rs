//! Shell command hosting and the keeper's internal lines (`docs/tools.md`,
//! "The keeper").

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use serde::{Deserialize, Serialize};

use crate::JobId;

/// Runs shell processes outside the session process.
pub trait CommandHost: Send + Sync {
    /// Starts a process and delivers its output and ending through `events`.
    fn start(&self, spec: Spawn, events: Sender<HostEvent>) -> Result<Started, HostError>;
    /// Asks a running process to stop, returning whether it was known.
    fn stop(&self, id: &str) -> bool;
    /// Writes text into a process's terminal.
    fn write(&self, id: &str, text: &str) -> Result<(), HostError>;
    /// Names the job and files that survive the session's death.
    fn adopt(
        &self,
        id: &str,
        job_id: &JobId,
        output_path: &Path,
        errors_path: Option<&Path>,
    ) -> Result<(), HostError>;
}

/// A shell process to start. Arguments and environment may contain secrets,
/// so this type deliberately has no `Debug` implementation.
#[derive(Serialize, Deserialize)]
pub struct Spawn {
    /// The session's identifier for this hosted call.
    pub id: String,
    /// The executable.
    pub program: PathBuf,
    /// Arguments following the executable name.
    pub args: Vec<String>,
    /// The working directory.
    pub cwd: PathBuf,
    /// Explicit environment sets, applied after environment scrubbing.
    pub env: Vec<(String, String)>,
    /// Input and terminal setup.
    pub stdin: HostStdin,
    /// The deadline from process start, in milliseconds; no bound when absent.
    pub timeout_ms: Option<u64>,
    /// Maximum bytes across the process's output streams.
    pub output_cap: u64,
    /// Confinement settings and the session-owned protection baseline.
    pub sandbox: Option<Sandbox>,
}

/// Confinement carried to the keeper's command builder.
#[derive(Serialize, Deserialize)]
pub struct Sandbox {
    /// Whether operating-system confinement is enabled.
    pub enabled: bool,
    /// Extra writable paths.
    pub allow_write: Vec<PathBuf>,
    /// Paths hidden from the command.
    pub deny_read: Vec<PathBuf>,
    /// Fiber home, excluded from ordinary command access.
    pub fiber_home: PathBuf,
    /// The workspace confinement root.
    pub workspace: PathBuf,
    /// The session's protection baseline, when one was acquired.
    pub baseline: Option<PathBuf>,
}

/// The only input modes a hosted shell command accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostStdin {
    /// No input pipe.
    Null,
    /// A pseudo-terminal owned by the keeper.
    Pty,
}

/// A started process's group, for read-only liveness probes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Started {
    /// The process group id.
    pub pgid: u32,
}

/// A hosted operation could not reach or start its process.
#[derive(Debug, thiserror::Error)]
#[error("hosted call {id}: {io}")]
pub struct HostError {
    /// The hosted call identifier.
    pub id: String,
    /// The underlying failure's text.
    pub io: String,
}

/// One output stream from a hosted process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    /// Standard output, or a combined ordinary shell stream.
    Out,
    /// Standard error, kept separately for monitors.
    Err,
}

/// A hosted process's reaped ending.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exit {
    /// The shell's exit code, when it exited normally.
    pub code: Option<i32>,
    /// The terminating signal, when it was signalled.
    pub signal: Option<i32>,
    /// The keeper stopped it at its deadline.
    pub timed_out: bool,
    /// The keeper stopped it at its output cap.
    pub output_cap: bool,
}

/// Output and endings delivered to the session's shell driver.
#[derive(Debug)]
pub enum HostEvent {
    /// Bytes read before the process's ending.
    Output {
        /// The stream that produced these bytes.
        stream: Stream,
        /// Raw bytes; decoding belongs to the shell driver.
        bytes: Vec<u8>,
    },
    /// The process ended and its output readers drained.
    Exit(Exit),
}

/// Session-to-keeper control lines. The stdin pipe is a separate lifeline.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToKeeper {
    /// Start a tool call.
    Start(Spawn),
    /// Stop a tool call or adopted job.
    Stop(String),
    /// Write to a terminal.
    Write {
        /// The hosted call identifier.
        id: String,
        /// Text to write.
        text: String,
    },
    /// Hand the process to a job after `job_started` is durable.
    Adopt {
        /// The hosted call identifier.
        id: String,
        /// The job the session opened.
        job_id: JobId,
        /// Absolute path to the job's output file.
        output_path: PathBuf,
        /// Absolute path to a monitor's error file, when separate.
        errors_path: Option<PathBuf>,
    },
    /// The session has durably written this call's job ending.
    Confirmed(String),
    /// A normal session shutdown, rather than a crash.
    Shutdown(bool),
}

/// Keeper-to-session control lines. Output encoding happens at the process
/// boundary, not in the shell driver.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FromKeeper {
    /// Process creation succeeded.
    Started {
        /// The hosted call identifier.
        id: String,
        /// The new process group id.
        pgid: u32,
    },
    /// Process creation failed.
    Failed {
        /// The hosted call identifier.
        id: String,
        /// The spawn failure's text.
        error: String,
    },
    /// Bytes read from a process pipe or terminal.
    Output {
        /// The hosted call identifier.
        id: String,
        /// The originating stream.
        stream: Stream,
        /// Base64-encoded bytes.
        bytes: String,
    },
    /// A reaped process whose pipe readers have drained.
    Exit {
        /// The hosted call identifier.
        id: String,
        /// The process ending, flattened into the wire object.
        #[serde(flatten)]
        exit: Exit,
    },
}

#[cfg(test)]
#[path = "keeper_tests.rs"]
mod tests;
