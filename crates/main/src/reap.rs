//! The hub child's reap (`docs/invocation.md`, "The hub"): the handed-off
//! child is waited for on a thread of its own, so it never stays a zombie.

use std::io;
use std::process::{Child, ExitStatus};
use std::thread::JoinHandle;

/// Waits for the handed-off hub child on a thread of its own and reports
/// its status: the caller keeps serving while the thread owns the wait.
pub(crate) fn on_thread(mut child: Child) -> io::Result<JoinHandle<io::Result<ExitStatus>>> {
    std::thread::Builder::new()
        .name("fiber-hub-reap".to_owned())
        .spawn(move || child.wait())
}

#[cfg(test)]
#[path = "reap_tests.rs"]
mod tests;
