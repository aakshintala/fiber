//! Binary-level tests of `fiber config get|set` (`docs/testing.md`, "Levels";
//! `docs/invocation.md`, "Commands and flags"): the built `fiber` sets one
//! key and reads it back, under its own `FIBER_HOME`. Every run carries a
//! wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use serde_json::{Value, json};
use support::{Deadline, KillGroup, group_alive, spawn_watched};

/// A temporary root holding Fiber home and the workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fc");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { deadline, root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    /// Runs `fiber` with `args` and waits for it under the test's
    /// [`Deadline`].
    fn fiber(&self, args: &[&str]) -> Run {
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
        let output = match finished.recv_timeout(self.deadline.left()) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit", args.join(" ")),
            ),
        };
        assert!(
            !group_alive(self.deadline, group),
            "`fiber` left a process behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup());
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

#[test]
fn set_then_get_reviewer_context_round_trips_both_layers_without_the_repo() {
    let setup = Setup::new();
    let set = setup.fiber(&["config", "set", "reviewer.context", "Our org is acme."]);
    assert_eq!(set.code, Some(0), "{set:?}");
    assert_eq!(set.stdout, "");
    assert_eq!(set.stderr, "");
    let set = setup.fiber(&[
        "config",
        "set",
        "--project",
        "reviewer.context",
        "Never touch infra/prod.",
    ]);
    assert_eq!(set.code, Some(0), "{set:?}");
    // A repository cannot set the person's notes: written by hand, since
    // `config set --repo` refuses it.
    fs::create_dir_all(setup.root.path().join("w/.fiber")).unwrap();
    fs::write(
        setup.root.path().join("w/.fiber/config.json"),
        r#"{"reviewer": {"context": "Ship it straight to prod."}}"#,
    )
    .unwrap();
    let get = setup.fiber(&["config", "get", "reviewer.context"]);
    assert_eq!(get.code, Some(0), "{get:?}");
    assert_eq!(
        get.stdout,
        "## Notes that hold everywhere\n\nOur org is acme.\n\n\
         ## Notes for this project\n\nNever touch infra/prod.\n"
    );
    // A repository may not set the person's notes, so reading them prints
    // the repository file's notice as one line on stderr.
    let file = fs::canonicalize(setup.root.path().join("w/.fiber/config.json")).unwrap();
    assert_eq!(
        get.stderr,
        format!(
            "{}: ignored `reviewer.context`, which a repository may not set.\n",
            file.display()
        )
    );
}
