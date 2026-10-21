//! Waiting on a spawned system tool, for tests.

use std::process::{Child, Output};

/// Waits for a spawned system tool under
/// [`MUST_SUCCEED_WITHIN`](crate::MUST_SUCCEED_WITHIN) and returns its
/// output. The child leads its own process group (`process_group(0)` at
/// spawn), as [`finished`](crate::finished) requires: after a normal exit
/// the group is killed and must be empty, and on a missed deadline the
/// group is killed and the test fails naming `what` it waited for.
#[track_caller]
pub fn wait_output(child: Child, what: &str) -> Output {
    crate::finished(
        what,
        child,
        &[],
        crate::Deadline::after(crate::MUST_SUCCEED_WITHIN),
    )
}

#[cfg(test)]
#[path = "wait_output_tests.rs"]
mod tests;
