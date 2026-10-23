//! Warming a jig's built binary past its first-exec stall, for tests.
//!
//! The first exec of a freshly linked binary can stall for seconds on macOS
//! (`docs/testing.md`:289 states the stall), so every jig test runs its
//! binary once on fast-failing arguments under [`START_BOUND`] before the
//! exchange keeps its own deadline. No retry and no wider exchange deadline:
//! the warm-up either absorbs the stall or fails naming the start.

use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// How long a jig binary's first start may take: at least 30 s, above
/// `MUST_SUCCEED_WITHIN`, and twice it fits nextest's 4 x 30 s timeout
/// (`docs/testing.md`, "Waits and timeouts"). Sized from the worst observed
/// stand-in stall of 28.36 s with margin, and capped so a missed warm-up
/// with its reap still reports before the harness kills the test.
pub const START_BOUND: Duration = Duration::from_secs(50);

/// Runs `binary args` once under [`START_BOUND`] on every call and asserts
/// its exit is `exit`: no state, so a failed warm-up affects no later call.
/// The child leads its own process group and a watchdog guards it, as the
/// sibling jig wrappers do (`docs/testing/running.md`).
///
/// # Panics
///
/// When the binary cannot spawn, misses the bound, or exits otherwise. Each
/// names the start of the jig's path, never the exchange.
#[allow(
    clippy::panic,
    reason = "a jig binary that cannot start fails the test naming the start"
)]
#[track_caller]
pub fn warm_up(binary: &Path, args: &[&str], exit: i32) {
    warm_up_within(binary, args, exit, START_BOUND);
}

/// The same run with the bound injected: the bound only sets how soon a
/// stalled start fails by design.
#[allow(
    clippy::panic,
    reason = "a jig binary that cannot start fails the test naming the start"
)]
#[track_caller]
pub(crate) fn warm_up_within(binary: &Path, args: &[&str], exit: i32, bound: Duration) {
    let what = format!("the start of {}", binary.display());
    let mut command = Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => panic!("{what} could not spawn: {err}"),
    };
    let watchdog = crate::Watchdog::group(child.id());
    let output = crate::finished(&what, child, &[], crate::Deadline::after(bound));
    watchdog.stand_down(crate::Deadline::after(crate::MUST_SUCCEED_WITHIN));
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{what} exited {} but the warm-up needs {exit}",
        output.status
    );
}

#[cfg(test)]
#[path = "jig_start_tests.rs"]
mod tests;
