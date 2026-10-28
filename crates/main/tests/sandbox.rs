//! Binary-level sandbox key wiring (`docs/configuration.md`, "Keys"): the
//! built `fiber` reads person-only `sandbox.*` keys through the binary
//! boundary. Observable JSON events (`confined: true`) and fail-closed spawn
//! are covered through the public shell API in `crates/tools/tests/shell.rs`
//! and the wire shape in `crates/contract/src/events_tests.rs`; the existing
//! `resume.rs` binary tests cover `confined: false` events.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use support::{KillGroup, Output, Setup, spawn_watched};

impl Setup {
    fn new_with_sandbox_root() -> Self {
        // Root the session under the owner's home: the sandbox grants the
        // temp dir as writable, so a fixture beneath it would overlap and
        // report unavailable instead of confining.
        let home_base = std::env::var_os("HOME").expect("HOME names the owner's home");
        let root = fakes::TempDir::under(std::path::Path::new(&home_base), "fiber-sandbox-binary");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self {
            deadline: support::Deadline::start(),
            root,
        }
    }

    fn fiber_run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path().join("w"))
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
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
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit", args.join(" ")),
            ),
        };
        assert!(!fakes::group_alive(group), "`fiber` left a process behind");
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        Output::from(output)
    }
}

#[test]
fn sandbox_keys_round_trip_through_the_binary() {
    let setup = Setup::new_with_sandbox_root();
    let out = setup.fiber_run(&["config", "set", "sandbox.enabled", "true"]);
    assert_eq!(out.code, Some(0));
    let out = setup.fiber_run(&["config", "get", "sandbox.enabled"]);
    assert_eq!(out.code, Some(0));
    assert!(out.stdout.contains("true"));
    let out = setup.fiber_run(&["config", "set", "sandbox.allow_write", r#"["~/.pi/agent"]"#]);
    assert_eq!(out.code, Some(0));
    let out = setup.fiber_run(&["config", "get", "sandbox.allow_write"]);
    assert_eq!(out.code, Some(0));
    assert!(out.stdout.contains(".pi/agent"));
}

/// A session confined through the binary denies the configured path: the
/// model reads the secret and the sibling, and the log shows the ordered
/// events with `confined: true`, no denied secret and a readable sibling.
#[cfg(target_os = "linux")]
#[test]
fn a_confined_session_denies_the_configured_path() {
    use fakes::ProviderServer;
    use serde_json::{Value, json};
    use support::{function_call, hello, stream};

    let setup = Setup::new_with_sandbox_root();
    // The denied home sits beside the session workspace under the same
    // owner-home root: a deny beneath the workspace would overlap it.
    let fixture = setup.root.path().join("fh");
    fs::create_dir_all(&fixture).unwrap();
    fs::write(fixture.join("secret"), "shh\n").unwrap();
    fs::write(fixture.join("sibling"), "ok\n").unwrap();
    let secret = fixture.join("secret");
    let sibling = fixture.join("sibling");

    let server = ProviderServer::start([
        stream(&[function_call(
            "secret_1",
            "shell",
            &json!({"command": format!("cat {}", secret.display())}),
        )]),
        stream(&[function_call(
            "sibling_1",
            "shell",
            &json!({"command": format!("cat {}", sibling.display())}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    // The person-only sandbox config enables confinement with the deny.
    fs::write(
        setup.home().join("config.json"),
        serde_json::to_string(&json!({
            "model": "fake/m",
            "sandbox": {"enabled": true, "deny_read": [secret.display().to_string()]}
        }))
        .unwrap(),
    )
    .unwrap();
    // A standing rule allows the shell command, so review approves it.
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "cat "})
        ),
    )
    .unwrap();

    let run = setup.fiber_run(&["ask", "read both files"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let lines: Vec<Value> = run
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert!(!lines.is_empty(), "the session writes event lines");
    // The action ids are minted per call: tell the calls apart by the
    // requested command, then follow each id through its events.
    let action = |command: &str| {
        lines
            .iter()
            .find(|line| {
                line["kind"] == "tool_call_requested"
                    && line["payload"]["arguments"].to_string().contains(command)
            })
            .and_then(|line| line["action_id"].as_str())
            .unwrap_or_else(|| panic!("a request for {command}"))
            .to_owned()
    };
    let secret_id = action(secret.display().to_string().as_str());
    let sibling_id = action(sibling.display().to_string().as_str());
    // The two calls run in order, each requested before it starts and
    // completed after.
    let positions = |kind: &str, id: &str| {
        lines
            .iter()
            .position(|line| line["kind"] == kind && line["action_id"] == id)
            .unwrap_or_else(|| panic!("a {kind} line for {id}"))
    };
    let order = [
        positions("tool_call_requested", &secret_id),
        positions("tool_call_started", &secret_id),
        positions("tool_call_completed", &secret_id),
        positions("tool_call_requested", &sibling_id),
        positions("tool_call_started", &sibling_id),
        positions("tool_call_completed", &sibling_id),
    ];
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "ordered events"
    );
    for (name, id) in [("secret", &secret_id), ("sibling", &sibling_id)] {
        let started = &lines[positions("tool_call_started", id)];
        assert_eq!(
            started["payload"]["confined"], true,
            "{name} starts confined"
        );
    }
    let secret_end = &lines[positions("tool_call_completed", &secret_id)];
    let secret_text = secret_end["payload"]["content"]
        .as_array()
        .map(|content| {
            content
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    assert!(
        secret_text.contains("Permission denied"),
        "the denied read fails: {secret_text}"
    );
    assert!(
        !secret_text.contains("shh"),
        "no denied secret in the output: {secret_text}"
    );
    let sibling_end = &lines[positions("tool_call_completed", &sibling_id)];
    let sibling_text = sibling_end["payload"]["content"]
        .as_array()
        .map(|content| {
            content
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    assert!(
        sibling_text.contains("ok"),
        "the sibling stays readable: {sibling_text}"
    );
}

#[test]
fn a_repository_sandbox_key_is_ignored_through_the_binary() {
    let setup = Setup::new_with_sandbox_root();
    fs::create_dir_all(setup.root.path().join("w/.fiber")).unwrap();
    fs::write(
        setup.root.path().join("w/.fiber/config.json"),
        r#"{"sandbox": {"enabled": true}}"#,
    )
    .unwrap();
    let out = setup.fiber_run(&["config", "get", "sandbox.enabled"]);
    assert_eq!(out.code, Some(0));
    assert!(out.stdout.contains("false"));
}
