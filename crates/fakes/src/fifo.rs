//! Making a FIFO by one function, for tests.

use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// Makes a FIFO at `path` with `mkfifo`'s default mode (0666 minus umask).
/// One shared `mkfifo` child, so no test file spawns its own: the child
/// leads its own process group and runs under
/// [`MUST_SUCCEED_WITHIN`](crate::MUST_SUCCEED_WITHIN), so a stall fails
/// naming the wait instead of hanging the runner.
///
/// # Panics
///
/// When `mkfifo` cannot start, exits non-zero (the path exists, the parent
/// is missing), runs past its deadline, or leaves its process group behind.
/// Never removes an existing path and never retries.
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test helper; a failure is the test's"
)]
#[track_caller]
pub fn fifo(path: impl AsRef<Path>) {
    let path = path.as_ref();
    let what = format!("mkfifo {}", path.display());
    let child = match Command::new("mkfifo")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(err) => panic!("mkfifo {}: {err}", path.display()),
    };
    let output = crate::finished(
        &what,
        child,
        &[],
        crate::Deadline::after(crate::MUST_SUCCEED_WITHIN),
    );
    assert!(
        output.status.success(),
        "mkfifo {} failed ({}): {}",
        path.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(test)]
#[path = "fifo_tests.rs"]
mod tests;
