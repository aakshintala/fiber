//! The `turn` jig (`docs/testing.md`, "Jigs"): the built `turn` binary
//! runs one turn against the scripted fake provider and prints its events,
//! one JSON line each, ephemeral ones included.

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

/// How long the turn jig exchange may take once the binary has started.
/// `.config/nextest.toml` kills a slow test after a 30s period times 4
/// (120s total); the test's start bound and its exchange bound each count
/// alone against that limit, and the ticket's margin table gives the total
/// for the pass-then-hang path: `docs/testing.md`, "Waits and timeouts",
/// needs nextest's timeout to be at least twice the test's longest single
/// deadline plus its normal run time, so a hang reports which wait expired.
const JIG_DEADLINE: Duration = Duration::from_secs(20);

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/turn`), so the test starts no cargo and
/// waits on no build lock.
fn example_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.join("examples").join("turn")
}

#[track_caller]
fn turn(args: &[&str]) -> Output {
    fakes::warm_up(&example_path(), &["--bogus"], 2);
    let child = Command::new(example_path())
        .args(args)
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
        .expect("the turn jig finished")
        .unwrap();
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    output
}

/// The jig's stdout, one parsed JSON value per line.
fn stdout_lines(output: &Output) -> Vec<Value> {
    assert!(
        output.status.success(),
        "the jig exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "the jig is JSONL on stdout only: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn it_runs_one_turn_and_prints_its_events_in_order() {
    let lines = stdout_lines(&turn(&[]));
    let kinds: Vec<&str> = lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "reasoning_started",
            "reasoning_delta",
            "assistant_message_delta",
            "reasoning_completed",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let delta = lines
        .iter()
        .find(|line| line["kind"] == json!("reasoning_delta"))
        .unwrap();
    assert_eq!(
        delta["payload"]["text"],
        json!("The person wants a greeting.")
    );
    let text = lines
        .iter()
        .find(|line| line["kind"] == json!("assistant_message_delta"))
        .unwrap();
    assert_eq!(text["payload"]["text"], json!("Hello."));
    assert_eq!(
        lines.last().unwrap()["payload"]["outcome"],
        json!("completed")
    );
}
