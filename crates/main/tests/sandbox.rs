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
        let root = fakes::TempDir::new("fiber-sandbox-binary");
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
