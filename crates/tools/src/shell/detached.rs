//! A detached shell command (`docs/tools.md`, "Background jobs"): the
//! command starts in its own session and process group, writes its output
//! to a log under Fiber home, and the call returns the process ID and the
//! log path at once. The group is never listed, so a shutdown never
//! signals it; one reaper thread per child owns the `Child` and calls
//! `wait`, so the child never stays a zombie while Fiber runs.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use super::spawn::scrub_env;

/// How many log names one call tries before it fails: stale files from an
/// earlier process that reused the Fiber pid collide, and the person owns
/// deleting them, so 100 consecutive collisions fail the call instead of
/// scanning without bound.
pub(super) const LOG_ATTEMPTS: u64 = 100;

/// The process-wide detached log sequence: one counter per process, so two
/// shells in one process never share a value. `create_log` takes its
/// counter as a parameter, so a test passes a test-local one and never
/// touches this static.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// A started detached command: the child's process ID, which is also its
/// process-group and session ID, and the log its output goes to.
pub(super) struct Started {
    /// The child's process ID, read after `spawn` returns.
    pub pid: u32,
    /// The log the child's standard output and error go to.
    pub log: PathBuf,
}

/// Starts `command` detached: its own session and group, outside the
/// sandbox, writing to a fresh log in `dir`. Standard input is `/dev/null`;
/// standard output and standard error are the log file and its duplicate,
/// one shared offset, so the two streams interleave in arrival order. The
/// child's group is never listed, so a shutdown never signals it. A log
/// created before a failed spawn is removed, and a failed removal is not
/// reported. A path that is not UTF-8 fails before spawn, so no process
/// starts without a path the model can read.
pub(super) fn launch(
    dir: &Path,
    fiber_pid: u32,
    program: &Path,
    command: &str,
    workdir: &Path,
) -> Result<Started, io::Error> {
    // The file name is always UTF-8, so only the directory can refuse
    // this: a log with no path the model can read fails before spawn, so
    // no process starts without a path for the receipt.
    if dir.to_str().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("detached log directory is not UTF-8: {}", dir.display()),
        ));
    }
    let (log, path) = create_log(dir, fiber_pid, &NEXT)?;
    let mut cmd = Command::new(program);
    cmd.arg("-c")
        .arg(command)
        .current_dir(workdir)
        .stdin(Stdio::null());
    let err = log.try_clone()?;
    cmd.stdout(log).stderr(err);
    scrub_env(&mut cmd);
    support::group::detach(&mut cmd, false);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(source) => {
            let _removed = std::fs::remove_file(&path);
            return Err(source);
        }
    };
    let pid = child.id();
    match std::thread::Builder::new()
        .name("detached-reaper".to_owned())
        .spawn(move || {
            let _reaped = child.wait();
        }) {
        Ok(_) => {}
        Err(_) => {
            // No thread owns the child: it runs on, and stays a zombie
            // until Fiber exits and init reaps it. The command itself
            // started, so the receipt still goes back.
        }
    }
    Ok(Started { pid, log: path })
}

/// Creates the detached log directory and the next free log in it: each
/// attempt takes one sequence value and opens `d_<fiber-pid>_<n>.log` with
/// `create_new`, so two concurrent calls never share a file. The directory
/// is mode 0700 and each file mode 0600; a directory that already exists
/// keeps its mode.
pub(super) fn create_log(
    dir: &Path,
    fiber_pid: u32,
    next: &AtomicU64,
) -> io::Result<(File, PathBuf)> {
    DirBuilder::new().mode(0o700).recursive(true).create(dir)?;
    for _ in 0..LOG_ATTEMPTS {
        let n = next.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("d_{fiber_pid}_{n}.log"));
        match OpenOptions::new()
            .create_new(true)
            .append(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((file, path)),
            Err(taken) if taken.kind() == io::ErrorKind::AlreadyExists => {}
            Err(failed) => return Err(failed),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "{LOG_ATTEMPTS} detached logs named d_{fiber_pid}_* already exist in {}",
            dir.display()
        ),
    ))
}

/// The call's receipt: one text part, ending with a newline, with no
/// `job_id` and no completion notice, because a detached command is not a
/// job.
pub(super) fn receipt(started: &Started) -> String {
    format!(
        "Started detached. PID {}. Output: {}. Fiber will not stop it.\n",
        started.pid,
        started.log.display()
    )
}
