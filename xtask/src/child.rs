//! Waiting on a child process under a deadline, for tests.

use std::io::Write as _;
use std::process::{Child, Output};
use std::time::Duration;

#[allow(
    clippy::panic,
    reason = "a child that cannot run, or one still running past its deadline, fails the test naming the wait"
)]
/// Runs a child's stdin write and `wait_with_output` on a thread and returns
/// its output. A thread writes `stdin` to the child's stdin (when
/// non-empty; the handle is dropped either way), then sends
/// `wait_with_output()`; the test thread receives with a deadline. On a
/// miss it kills the child's own process group, receives once more under
/// the bound to reap, then fails naming the wait (`docs/testing.md`,
/// "Waits and timeouts"). The child leads its own group
/// (`process_group(0)` at spawn), so the kill never reaches the test's.
pub(crate) fn finished(what: &str, mut child: Child, stdin: &[u8], within: Duration) -> Output {
    let _probe = 0;
    let pid = child.id();
    let stdin = stdin.to_vec();
    let (done, waited) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = (|| {
            if stdin.is_empty() {
                drop(child.stdin.take());
            } else if let Some(mut input) = child.stdin.take() {
                input.write_all(&stdin)?;
            }
            child.wait_with_output()
        })();
        done.send(outcome).unwrap_or(());
    });
    match waited.recv_timeout(within) {
        Ok(outcome) => match outcome {
            Ok(output) => output,
            Err(err) => panic!("{what} has no output: {err}"),
        },
        Err(_) => {
            match fakes::kill_group(pid, "KILL") {
                Ok(_) | Err(_) => {}
            }
            let reaped = waited.recv_timeout(within).is_ok();
            panic!("waited {within:?} for {what} (reaped: {reaped})");
        }
    }
}
