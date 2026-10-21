use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::finished;
use crate::deadline::Deadline;

/// How long a test waits for a child that already exited, and for the
/// killed group to empty: a passing run never waits on either, they only
/// bound a hang.
const WITHIN: Duration = Duration::from_secs(10);
/// How long the timeout test lets its `sleep` run before giving up on it.
const SHORT: Duration = Duration::from_secs(1);

fn spawn(program: &str, args: &[&str]) -> std::process::Child {
    Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap()
}

#[test]
fn an_exited_child_returns_its_output_with_stdout() {
    let child = spawn("sh", &["-c", "printf hello"]);
    let output = finished("exited child", child, &[], Deadline::after(WITHIN));
    assert!(output.status.success());
    assert_eq!(output.stdout, b"hello");
}

#[test]
fn stdin_bytes_reach_the_child() {
    let child = spawn("cat", &[]);
    let output = finished("cat stdin", child, b"input-bytes", Deadline::after(WITHIN));
    assert!(output.status.success());
    assert_eq!(output.stdout, b"input-bytes");
}

#[test]
fn a_child_past_its_deadline_fails_naming_the_wait_and_leaves_no_group() {
    let child = spawn("sleep", &["30"]);
    let pid = child.id();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        finished("sleepy", child, &[], Deadline::after(SHORT))
    }));
    let message = panic_message(caught.unwrap_err());
    assert!(message.contains("sleepy"), "{message}");
    assert!(
        crate::group_empties(pid, Deadline::after(WITHIN)),
        "the killed group {pid} is still alive"
    );
}

#[test]
fn a_backgrounded_descendant_leaves_no_group() {
    let child = spawn("sh", &["-c", "sleep 30 >/dev/null 2>&1 & echo ok"]);
    let pid = child.id();
    let output = finished(
        "backgrounded descendant",
        child,
        &[],
        Deadline::after(WITHIN),
    );
    assert!(output.status.success());
    assert_eq!(output.stdout, b"ok\n");
    assert!(
        crate::group_empties(pid, Deadline::after(WITHIN)),
        "the backgrounded group {pid} is still alive"
    );
}

#[test]
fn a_stdin_write_error_kills_and_reaps_the_group() {
    let child = spawn("sh", &["-c", "exec 0<&-; sleep 30"]);
    let pid = child.id();
    let stdin = vec![b'x'; 1 << 20];
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        finished("closed stdin", child, &stdin, Deadline::after(WITHIN))
    }));
    let message = panic_message(caught.unwrap_err());
    assert!(message.contains("closed stdin"), "{message}");
    assert!(
        crate::group_empties(pid, Deadline::after(WITHIN)),
        "the killed group {pid} is still alive"
    );
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "panic".to_owned())
}
