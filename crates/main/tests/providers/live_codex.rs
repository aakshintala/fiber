//! The opt-in live test for the codex package (`docs/testing.md`, "Live
//! calls and evals"): one turn against the real endpoint. It is `#[ignore]`d
//! and panics without `FIBER_LIVE_CODEX_HOME` naming a Fiber home where the
//! owner installed the package and ran `fiber login codex`; it never runs in CI
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
use std::sync::{Arc, mpsc};
use std::time::Duration;

use fakes::Deadline;

/// How long the live turn may take.
const LIVE_DEADLINE: Duration = Duration::from_secs(300);

/// How long a killed child may take to report after its group is killed.
const REAP_DEADLINE: Duration = Duration::from_secs(10);

/// How long the live quota fetch may take.
const QUOTA_DEADLINE: Duration = Duration::from_secs(30);

#[test]
#[ignore = "live/needs FIBER_LIVE_CODEX_HOME; run with --run-ignored only"]
fn a_live_codex_quota_fetch_reports_windows() {
    let Ok(home) = std::env::var("FIBER_LIVE_CODEX_HOME") else {
        panic!("the live codex test needs FIBER_LIVE_CODEX_HOME set");
    };
    let home = std::path::PathBuf::from(home);
    // The owner's stored login: `default` where one is stored under it,
    // else the home's only codex label.
    let mut labels: Vec<String> = std::fs::read_dir(home.join("credentials").join("codex"))
        .expect("the live home holds no codex login")
        .map(|entry| {
            entry
                .expect("the live home holds an unreadable codex login")
                .file_name()
                .into_string()
                .expect("the live home holds a codex login with an unreadable name")
        })
        .collect();
    labels.sort();
    let label = if labels.iter().any(|label| label == "default") {
        "default".to_owned()
    } else if labels.len() == 1 {
        labels.pop().expect("the live home holds one codex login")
    } else {
        panic!(
            "the live home holds {} codex logins and none is `default`: {}",
            labels.len(),
            labels.join(", ")
        );
    };
    let dir = home.join("extensions").join(config::dir_name(
        "github.com/aakshintala/fiber/providers/codex",
    ));
    let clock = Arc::new(fakes::clock::SystemClock);
    let extension = Arc::new(extensions::LuaExtension::new(
        "github.com/aakshintala/fiber/providers/codex",
        dir,
        home,
        clock,
    ));
    let provider = extensions::LuaProvider::new(extension, "codex");
    let pair = extensions::CredentialPair {
        credential: "codex".to_owned(),
        label,
    };
    let (done, fetched) = mpsc::channel();
    std::thread::spawn(move || done.send(provider.fetch_quota(&pair, None)));
    let (quota, _) = fakes::Deadline::after(QUOTA_DEADLINE)
        .recv(&fetched)
        .expect("the live quota fetch never returned")
        .expect("the live quota fetch failed");
    let Some(quota) = quota else {
        panic!("the live quota reported nothing");
    };
    let extensions::Quota::Windows(windows) = quota else {
        panic!("the live quota is not windows: {quota:?}");
    };
    assert!(
        (1..=2).contains(&windows.len()),
        "the live quota holds {} windows: {windows:?}",
        windows.len()
    );
    for window in &windows {
        assert!(
            ["primary", "secondary"].contains(&window.name.as_str()),
            "the live window has an unexpected name: {window:?}"
        );
        let extensions::WindowUse::Percent { used, .. } = &window.used else {
            panic!("the live window is already reset: {window:?}");
        };
        assert!(
            used.is_finite() && (0.0..=100.0).contains(used),
            "the live window percent is out of range: {window:?}"
        );
    }
}

#[test]
#[ignore = "live/needs FIBER_LIVE_CODEX_HOME; run with --run-ignored only"]
fn a_live_codex_turn_replies() {
    let Ok(home) = std::env::var("FIBER_LIVE_CODEX_HOME") else {
        panic!("the live codex test needs FIBER_LIVE_CODEX_HOME set");
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
        .envs(fakes::check_run())
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
    let first = Deadline::after(LIVE_DEADLINE).recv(&finished);
    let timed_out = first.is_err();
    let mut received = match first {
        Ok(output) => Some(output),
        Err(_) => {
            fakes::kill_group(group, "KILL").unwrap_or(false);
            Deadline::after(REAP_DEADLINE).recv(&finished).ok()
        }
    };
    if received.is_none() {
        fakes::kill_group(group, "KILL").unwrap_or(false);
        received = Deadline::after(REAP_DEADLINE).recv(&finished).ok();
    }

    let empty = fakes::group_empties(group, fakes::Deadline::after(REAP_DEADLINE));
    if !empty {
        fakes::kill_group(group, "KILL").unwrap_or(false);
    }
    let cleaned = empty || fakes::group_empties(group, fakes::Deadline::after(REAP_DEADLINE));
    watchdog.stand_down(fakes::Deadline::after(REAP_DEADLINE));
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
