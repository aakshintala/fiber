//! Waiting on a child process under a deadline, for tests.

use std::io::Write as _;
use std::process::{Child, Output};

use crate::deadline::Deadline;

#[allow(
    clippy::panic,
    reason = "a child that cannot run, or one still running past its deadline, fails the test naming the wait"
)]
/// Runs a child's stdin write and `wait_with_output` on a thread and returns
/// its output. A thread writes `stdin` to the child's stdin (when
/// non-empty; the handle is dropped either way), then sends
/// `wait_with_output()`; the test thread receives with a deadline. A stdin
/// write error kills the child's own process group and reaps the child
/// before the error is sent, so the `Child` stays owned until then. After a
/// normal exit the test thread kills the group too: a descendant the child
/// left behind, holding no pipe, must not survive. On a missed deadline it
/// kills the child's own process group, receives once more under the bound
/// to reap, then fails naming the wait (`docs/testing.md`, "Waits and
/// timeouts"). The child leads its own group (`process_group(0)` at spawn),
/// so the kill never reaches the test's.
#[track_caller]
pub fn finished(what: &str, mut child: Child, stdin: &[u8], deadline: Deadline) -> Output {
    let within = deadline.left();
    let pid = child.id();
    let stdin = stdin.to_vec();
    let (done, waited) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = (|| {
            if stdin.is_empty() {
                drop(child.stdin.take());
            } else if let Some(mut input) = child.stdin.take() {
                let write = input.write_all(&stdin);
                drop(input);
                if let Err(err) = write {
                    match crate::kill_group(pid, "KILL") {
                        Ok(_) | Err(_) => {}
                    }
                    match child.wait() {
                        Ok(_) | Err(_) => {}
                    }
                    return Err(err);
                }
            }
            child.wait_with_output()
        })();
        done.send(outcome).unwrap_or(());
    });
    match Deadline::after(within).recv(&waited) {
        Ok(outcome) => match outcome {
            Ok(output) => {
                match crate::kill_group(pid, "KILL") {
                    Ok(_) | Err(_) => {}
                }
                assert!(
                    crate::group_empties(pid, Deadline::after(within)),
                    "{what} left its process group {pid} alive"
                );
                output
            }
            Err(err) => {
                match crate::kill_group(pid, "KILL") {
                    Ok(_) | Err(_) => {}
                }
                let emptied = crate::group_empties(pid, Deadline::after(within));
                panic!("{what} has no output: {err} (group emptied: {emptied})");
            }
        },
        Err(_) => {
            match crate::kill_group(pid, "KILL") {
                Ok(_) | Err(_) => {}
            }
            let reaped = Deadline::after(within).recv(&waited).is_ok();
            panic!("waited {within:?} for {what} (reaped: {reaped})");
        }
    }
}

#[cfg(test)]
#[path = "finished_tests.rs"]
mod tests;
