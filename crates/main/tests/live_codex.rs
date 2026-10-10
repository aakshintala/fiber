//! The opt-in live test for the codex package (`docs/testing.md`, "Live
//! calls and evals"): one turn against the real endpoint. It returns at once
//! unless `FIBER_LIVE_CODEX_HOME` names a Fiber home where the owner
//! installed the package and ran `fiber login codex`; it never runs in CI
//! and never tries to reach a usage limit. The child runs in its own
//! process group under a watchdog, and a timeout kills the group and reaps
//! it before failing (`docs/testing.md`, "Running tests").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How long the live turn may take.
const LIVE_DEADLINE: Duration = Duration::from_secs(300);

/// How long a killed child may take to report after its group is killed.
const REAP_DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn a_live_codex_turn_replies() {
    let Ok(home) = std::env::var("FIBER_LIVE_CODEX_HOME") else {
        return;
    };
    let model = std::env::var("FIBER_LIVE_CODEX_MODEL").unwrap_or("gpt-6-luna".to_owned());
    let child = Command::new(env!("CARGO_BIN_EXE_fiber"))
        .args([
            "ask",
            "--model",
            &format!("codex/{model}"),
            "Reply with the word pong.",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .env("FIBER_HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let group = child.id();
    let watchdog = fakes::Watchdog::group(group);
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || done.send(child.wait_with_output()));
    let first = finished.recv_timeout(LIVE_DEADLINE);
    let timed_out = first.is_err();
    let mut received = match first {
        Ok(output) => Some(output),
        Err(_) => {
            fakes::kill_group(group, "KILL").unwrap_or(false);
            finished.recv_timeout(REAP_DEADLINE).ok()
        }
    };
    if received.is_none() {
        fakes::kill_group(group, "KILL").unwrap_or(false);
        received = finished.recv_timeout(REAP_DEADLINE).ok();
    }

    let empty = fakes::group_empties(group, REAP_DEADLINE);
    if !empty {
        fakes::kill_group(group, "KILL").unwrap_or(false);
    }
    let cleaned = empty || fakes::group_empties(group, REAP_DEADLINE);
    watchdog.stand_down(REAP_DEADLINE);
    assert!(
        cleaned,
        "the live turn left process group {group} behind after cleanup"
    );

    if timed_out {
        let killed = received
            .as_ref()
            .and_then(|output| output.as_ref().ok())
            .map(|output| output.status.signal());
        assert!(
            matches!(killed, Some(Some(9))),
            "the live turn was not reaped as killed within {REAP_DEADLINE:?}: {killed:?}"
        );
        panic!("the live turn did not exit within {LIVE_DEADLINE:?}");
    }
    let output = match received {
        Some(Ok(output)) => output,
        Some(Err(error)) => panic!("waiting for the live turn failed: {error}"),
        None => panic!("the live turn was not reaped within {REAP_DEADLINE:?}"),
    };
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{stdout}");
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    // A live stream varies in its deltas and reasoning events, so this pins
    // only the order of the kinds it relies on (docs/testing.md, "Live calls
    // and evals"). Lines written on their own trigger are set aside.
    let kinds: Vec<&str> = lines
        .iter()
        .filter_map(|line| line["kind"].as_str())
        .filter(|kind| !matches!(*kind, "session_status" | "clients" | "attention"))
        .collect();
    let started = kinds.iter().position(|kind| *kind == "turn_started");
    let first_turn_line = lines
        .iter()
        .position(|line| line.get("turn_id").is_some())
        .map(|at| lines[at]["kind"].as_str());
    assert_eq!(first_turn_line, Some(Some("turn_started")), "{stdout}");
    assert_eq!(kinds.last(), Some(&"turn_completed"), "{stdout}");
    let text_at = kinds.iter().rposition(|kind| *kind == "text_completed");
    assert!(
        matches!((started, text_at), (Some(s), Some(t)) if s < t),
        "{stdout}"
    );
    let completed = lines
        .iter()
        .rfind(|line| line["kind"] == "turn_completed")
        .unwrap();
    assert_eq!(completed["payload"]["outcome"], "completed", "{stdout}");
    let text = lines
        .iter()
        .rfind(|line| line["kind"] == "text_completed")
        .and_then(|line| line["payload"]["text"].as_str())
        .unwrap_or_else(|| panic!("no text_completed with text in {stdout}"));
    assert!(!text.is_empty(), "{stdout}");
}
