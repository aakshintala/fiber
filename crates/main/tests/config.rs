//! Binary-level tests of `fiber config get|set` (`docs/testing.md`, "Levels";
//! `docs/invocation.md`, "Commands and flags"): the built `fiber` sets one
//! key and reads it back, under its own `FIBER_HOME`. Every run carries a
//! wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Watchdog;
use serde_json::{Value, json};

/// How long one `fiber` run may take.
const DEADLINE: Duration = Duration::from_secs(20);

/// A temporary root holding Fiber home and the workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fc");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    /// Runs `fiber` with `args` and waits for it under [`DEADLINE`].
    fn fiber(&self, args: &[&str]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path().join("w"))
            .env_clear()
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
        let output = match finished.recv_timeout(DEADLINE) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                fakes::kill_group(group, "KILL").unwrap();
                let reaped = finished.recv_timeout(DEADLINE).is_ok();
                assert!(!group_alive(group), "`fiber` left a process behind");
                panic!(
                    "waited {DEADLINE:?} for `fiber {}` to exit (reaped after the kill: {reaped})",
                    args.join(" ")
                );
            }
        };
        assert!(!group_alive(group), "`fiber` left a process behind");
        std::mem::forget(guard);
        watchdog.stand_down(DEADLINE);
        Run {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

#[derive(Debug)]
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    std::mem::forget(guard);
    (child, watchdog)
}

/// Kills process group `group` on drop. After the child is reaped and the
/// group is empty, [`std::mem::forget`] skips that kill.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        match fakes::kill_group(self.0, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
}

/// Whether any process remains in process group `group`.
fn group_alive(group: u32) -> bool {
    fakes::kill_group(group, "0").unwrap()
}

#[test]
fn set_then_get_round_trips_the_value_and_its_layer() {
    let setup = Setup::new();
    let set = setup.fiber(&["config", "set", "model", "a/b"]);
    assert_eq!(set.code, Some(0), "{set:?}");
    assert_eq!(set.stdout, "");
    assert_eq!(set.stderr, "");
    let written: Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(written, json!({"model": "a/b"}));
    let get = setup.fiber(&["config", "get", "model"]);
    assert_eq!(get.code, Some(0), "{get:?}");
    assert_eq!(
        get.stdout,
        format!(
            "\"a/b\" from {}\n",
            setup.home().join("config.json").display()
        )
    );
    assert_eq!(get.stderr, "");
}

#[test]
fn set_repo_with_a_person_only_key_fails_and_writes_nothing() {
    let setup = Setup::new();
    let set = setup.fiber(&["config", "set", "--repo", "session.idle_exit_ms", "60000"]);
    assert_eq!(set.code, Some(2), "{set:?}");
    assert!(set.stderr.contains("`session.idle_exit_ms`"), "{set:?}");
    assert!(!setup.root.path().join("w/.fiber/config.json").exists());
}
