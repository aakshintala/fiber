//! The `call` jig (`docs/testing.md`, "Jigs"): the built `call` binary
//! runs one tool call and prints its result as JSON.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers; a failure is the test's"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;
use serde_json::{Value, json};

/// How long the call jig exchange may take once the binary has started.
/// This file's exchange bound, the start bound (`fakes::START_BOUND`) and the
/// stand-down sum under nextest's 120 s limit (`docs/testing.md`, "Waits and
/// timeouts").
const JIG_DEADLINE: Duration = Duration::from_secs(20);

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/call`), so the test starts no cargo and
/// waits on no build lock.
fn example_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.join("examples").join("call")
}

#[track_caller]
fn call(tool: &str, arguments: &str) -> Output {
    fakes::warm_up(&example_path(), &[], 2);
    let child = Command::new(example_path())
        .args([tool, arguments])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let watchdog = fakes::Watchdog::group(child.id());
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = Deadline::after(JIG_DEADLINE)
        .recv(&finished)
        .expect("the call jig finished")
        .unwrap();
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    output
}

#[test]
fn it_runs_a_shell_command_and_prints_its_result_as_json() {
    let out = call("shell", r#"{"command":"echo hi"}"#);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(printed["content"][0]["text"], json!("hi\nExit code 0.\n"));
    assert_eq!(printed["details"]["command"], json!("echo hi"));
    assert_eq!(printed["process"]["exit_code"], json!(0));
    assert!(printed.get("error").is_none(), "{printed}");
}

/// An ordinary command spells its limit with `timeout_ms`, every optional
/// argument present, and succeeds.
#[test]
fn it_runs_an_ordinary_command_with_an_explicit_timeout_ms() {
    let out = call(
        "shell",
        r#"{"command":"true","monitor":false,"run_in_background":false,"timeout_ms":10000,"tty":false,"workdir":null}"#,
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(printed["content"][0]["text"], json!("Exit code 0.\n"));
    assert_eq!(printed["details"]["command"], json!("true"));
    assert_eq!(printed["process"]["exit_code"], json!(0));
    assert_eq!(printed["process"]["timed_out"], json!(false));
    assert!(printed.get("error").is_none(), "{printed}");
}
