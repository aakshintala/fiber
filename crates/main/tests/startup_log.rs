//! Binary-level tests of the startup diagnostic log (`docs/state.md`,
//! "Diagnostic logs"): the built `fiber ask` records a failure that has no
//! session in `logs/ask-<pid>.log`. Every run carries a wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use serde_json::Value;
use support::{KillGroup, Setup, spawn_watched};

const SECRET: &str = "sk-planted-secret-4242";

impl Setup {
    /// Runs `fiber` with `args` under the test's deadline and returns its
    /// exit code.
    #[track_caller]
    fn run_ask(&self, args: &[&str]) -> Option<i32> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(self.deadline, group, &finished, "`fiber ask` to exit"),
        };
        assert!(!fakes::group_alive(group), "`fiber` left a process behind");
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        output.status.code()
    }

    /// The names in `logs/`, sorted; empty when the directory is absent.
    fn logs(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.home().join("logs"))
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

#[test]
fn a_config_error_before_the_session_writes_one_ask_line() {
    let setup = Setup::new();
    let code = setup.run_ask(&["ask", "-c", &format!("handoff.tokens=\"{SECRET}\""), "hi"]);
    assert_eq!(code, Some(1));

    let logs = setup.logs();
    assert_eq!(logs.len(), 1, "{logs:?}");
    let name = &logs[0];
    let pid = name
        .strip_prefix("ask-")
        .and_then(|rest| rest.strip_suffix(".log"))
        .unwrap_or_else(|| panic!("not ask-<pid>.log: {name}"));
    assert!(pid.parse::<u32>().is_ok(), "{name}");

    let text = fs::read_to_string(setup.home().join("logs").join(name)).unwrap();
    assert!(!text.contains(SECRET), "{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    let line: Value = serde_json::from_str(lines[0]).unwrap();
    let object = line.as_object().unwrap();
    // The documented fields in the documented order: no `session_id` (none
    // exists) and no `data` (not a debug line).
    let mut at = 0;
    for key in ["ts", "level", "process", "code", "message"] {
        let found = lines[0][at..]
            .find(&format!("\"{key}\":"))
            .unwrap_or_else(|| panic!("{key} missing or out of order: {text}"));
        at += found;
    }
    let mut fields: Vec<&str> = object.keys().map(String::as_str).collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        ["code", "level", "message", "process", "ts"],
        "{text}"
    );
    assert!(line["ts"].as_u64().is_some_and(|ts| ts > 0), "{text}");
    assert_eq!(line["level"], "error");
    assert_eq!(line["process"], "ask");
    assert_eq!(line["code"], "config_invalid");
    let message = line["message"].as_str().unwrap();
    assert!(
        message.ends_with('.') && !message.contains('\n'),
        "{message}"
    );
}

#[test]
fn a_usage_error_creates_no_log() {
    let setup = Setup::new();
    assert_eq!(setup.run_ask(&["ask", "one", "two"]), Some(2));
    assert!(!setup.home().join("logs").exists(), "{:?}", setup.logs());
}

#[test]
fn an_internal_session_failure_writes_a_session_line() {
    let setup = Setup::new();
    let missing = setup.root.path().join("missing");
    let code = setup.run_ask(&[
        "session",
        "--id",
        "s_00000000000000a1",
        "--workspace",
        missing.to_str().unwrap(),
    ]);
    assert_eq!(code, Some(1));

    let logs = setup.logs();
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert!(
        logs[0].starts_with("session-") && logs[0].ends_with(".log"),
        "{logs:?}"
    );
    let text = fs::read_to_string(setup.home().join("logs").join(&logs[0])).unwrap();
    let line: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(line["process"], "session");
    assert_eq!(line["code"], "io_failed");
}
