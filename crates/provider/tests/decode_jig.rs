//! The `decode` jig (`docs/testing.md`, "Jigs"): the built `decode`
//! binary runs a recorded stream through the `openai-responses` decoder
//! and prints what it produced, a `# label` line per exchange and then one
//! line per thing produced.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers; a failure is the test's"
)]

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;

/// How long the decode jig exchange may take once the binary has started.
/// `.config/nextest.toml` kills a slow test after a 30s period times 4
/// (120s total); the test's start bound and its exchange bound each count
/// alone against that limit, and the ticket's margin table gives the total
/// for the pass-then-hang path: `docs/testing.md`, "Waits and timeouts",
/// needs nextest's timeout to be at least twice the test's longest single
/// deadline plus its normal run time, so a hang reports which wait expired.
const JIG_DEADLINE: Duration = Duration::from_secs(20);

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/decode`), so the test starts no cargo and
/// waits on no build lock.
fn example_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.join("examples").join("decode")
}

/// A recorded stream the provider tests replay: the response bytes of one
/// tool-call exchange, compiled into the test so the fixture cannot drift.
static RECORDING: &[u8] = include_bytes!("recordings/opencode-go-tool-call.sse");

#[track_caller]
fn decode(file: &Path) -> Output {
    fakes::warm_up(&example_path(), &[], 2);
    let child = Command::new(example_path())
        .arg(file)
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
        .expect("the decode jig finished")
        .unwrap();
    watchdog.stand_down(fakes::Deadline::after(Duration::from_secs(5)));
    output
}

#[test]
fn it_decodes_a_recorded_stream_into_deltas_actions_and_usage() {
    let dir = fakes::TempDir::new("fiber-decode-jig");
    let file = dir.path().join("opencode-go-tool-call.sse");
    std::fs::write(&file, RECORDING).unwrap();
    let out = decode(&file);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let text = String::from_utf8(out.stdout).unwrap();
    let mut lines = text.lines();
    assert_eq!(lines.next().unwrap(), format!("# {}", file.display()));
    let kinds: Vec<&str> = text
        .lines()
        .skip(1)
        .map(|line| line.split_once(' ').unwrap().0)
        .collect();
    assert_eq!(
        kinds,
        [
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "reasoning_completed",
            "text_completed",
            "tool_call_requested",
            "assistant_message_completed",
            "usage_recorded",
        ]
    );
    let completed = text
        .lines()
        .find(|line| line.starts_with("assistant_message_completed "))
        .unwrap();
    assert!(
        completed.contains(r#""outcome":"completed""#),
        "{completed}"
    );
    let usage = text
        .lines()
        .find(|line| line.starts_with("usage_recorded "))
        .unwrap();
    assert!(usage.contains(r#""generation_id":"resp_"#), "{usage}");
}
