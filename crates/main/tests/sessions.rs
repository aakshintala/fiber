//! Binary-level tests of `fiber sessions export` (`docs/invocation.md`,
//! "Deleting and pruning"; `docs/testing.md`, "Levels"): the built `fiber`
//! runs in a temporary workspace with its own `FIBER_HOME`, over
//! session directories built by hand. Every run carries a wall-clock
//! deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::Watchdog;
use serde_json::json;
use support::Deadline;

/// The `session_started` first line of session `id` in `workspace`: a full
/// envelope, so `resolve` keeps it, in this project's workspace.
fn first(id: &str, workspace: &Path) -> String {
    format!(
        "{}\n",
        json!({"kind": "session_started", "session_id": id, "ts": 0,
            "schema_version": 1, "seq": 0,
            "payload": {"workspace": workspace,
                "variables": {"path": "/usr/bin", "names": [],
                    "source": "inherited"}}})
    )
}

const SECOND: &str = "{\"seq\":1,\"kind\":\"b\"}\n";
const TORN: &str = "{\"seq\":2,\"kin";

/// Fiber home and a workspace in a temporary directory, removed on
/// drop.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        let setup = Self {
            deadline,
            root: fakes::TempDir::new("fiber-export"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        fs::create_dir_all(setup.workspace()).unwrap();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    /// The project's sessions directory, as the command derives it: a
    /// workspace outside any repository is its own project.
    fn sessions(&self) -> PathBuf {
        log::sessions_dir(&self.home(), &fs::canonicalize(self.workspace()).unwrap())
    }

    /// A session directory with two complete lines, a torn tail, one
    /// artifact and a lock file. The first line starts the session in
    /// this workspace, so `resolve` keeps it.
    fn session(&self, id: &str) {
        let dir = self.sessions().join(id);
        fs::create_dir_all(dir.join("artifacts")).unwrap();
        fs::write(
            dir.join("events.jsonl"),
            format!("{}{SECOND}{TORN}", self.first_line(id)),
        )
        .unwrap();
        fs::write(dir.join("artifacts/a_1.txt"), "artifact\n").unwrap();
        fs::write(dir.join("session.lock"), "held").unwrap();
    }

    /// The `session_started` first line `session` writes for `id`.
    fn first_line(&self, id: &str) -> String {
        first(id, &fs::canonicalize(self.workspace()).unwrap())
    }

    /// Runs `fiber sessions export` with `args` in the workspace.
    fn export(&self, args: &[&str]) -> Run {
        let mut all = vec!["sessions", "export"];
        all.extend_from_slice(args);
        self.fiber(&all)
    }

    /// Runs `fiber` with `args` in the workspace.
    fn fiber(&self, args: &[&str]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(self.deadline.left()) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(self.deadline, group, &finished, "`fiber` to exit"),
        };
        watchdog.stand_down(self.deadline.cleanup());
        Run {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

// debt: `spawn_watched` copies `approve.rs`'s, as the other binary test files
// in this crate each do; move every copy into `fakes` together when a
// change to one has to be made in all.

/// Spawns `command` in a new process group, then a watchdog in its own
/// group, which kills the group if this process dies first.
fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let watchdog = Watchdog::group(child.id());
    (child, watchdog)
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// The workspace as the child saw it: the current directory, resolved.
fn canonical(setup: &Setup) -> PathBuf {
    fs::canonicalize(setup.workspace()).unwrap()
}

#[test]
fn a_unique_prefix_exports_the_complete_lines_and_the_artifacts() {
    let setup = Setup::new();
    setup.session("s_exportaa01");
    let run = setup.export(&["s_exportaa"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let target = canonical(&setup).join("s_exportaa01");
    assert_eq!(run.stdout, format!("{}\n", target.display()), "stdout");
    assert_eq!(
        fs::read(target.join("events.jsonl")).unwrap(),
        format!("{}{SECOND}", setup.first_line("s_exportaa01")).as_bytes()
    );
    assert_eq!(
        fs::read(target.join("artifacts/a_1.txt")).unwrap(),
        b"artifact\n"
    );
    assert!(!target.join("session.lock").exists());
}

#[test]
fn an_explicit_relative_path_is_taken_from_the_workspace() {
    let setup = Setup::new();
    setup.session("s_exportbb01");
    let run = setup.export(&["s_exportbb01", "deep/out"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let target = canonical(&setup).join("deep/out");
    assert_eq!(run.stdout, format!("{}\n", target.display()), "stdout");
    assert_eq!(
        fs::read(target.join("events.jsonl")).unwrap(),
        format!("{}{SECOND}", setup.first_line("s_exportbb01")).as_bytes()
    );
    assert_eq!(
        fs::read(target.join("artifacts/a_1.txt")).unwrap(),
        b"artifact\n"
    );
}

#[test]
fn an_existing_target_is_refused_and_left_alone() {
    let setup = Setup::new();
    setup.session("s_exportcc01");
    let target = setup.workspace().join("out");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("keep"), "untouched").unwrap();
    let run = setup.export(&["s_exportcc01", "out"]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(run.stderr.contains("already exists"), "{}", run.stderr);
    assert!(run.stderr.contains("out"), "{}", run.stderr);
    assert_eq!(fs::read(target.join("keep")).unwrap(), b"untouched");
    assert!(!target.join("events.jsonl").exists());
}

#[test]
fn an_ambiguous_prefix_names_every_match() {
    let setup = Setup::new();
    setup.session("s_amb11111");
    setup.session("s_amb22222");
    let run = setup.export(&["s_amb"]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(run.stderr.contains("s_amb11111"), "{}", run.stderr);
    assert!(run.stderr.contains("s_amb22222"), "{}", run.stderr);
}

#[test]
fn an_unknown_id_exits_1() {
    let setup = Setup::new();
    setup.session("s_exportdd01");
    let run = setup.export(&["s_missing"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert!(run.stderr.contains("no session"), "{}", run.stderr);
}

#[test]
fn sessions_help_and_export_help_print() {
    let setup = Setup::new();
    for args in [
        &["help", "sessions"][..],
        &["sessions", "--help"],
        &["sessions", "export", "--help"],
    ] {
        let run = setup.fiber(args);
        assert_eq!(run.code, Some(0), "{args:?}: {}", run.stderr);
        assert_eq!(run.stderr, "", "{args:?}");
        assert!(run.stdout.contains("export"), "{args:?}: {}", run.stdout);
    }
    let help = setup.fiber(&["help", "sessions"]);
    let flag = setup.fiber(&["sessions", "--help"]);
    assert_eq!(help.stdout, flag.stdout);
    let export_help = setup.fiber(&["sessions", "export", "--help"]);
    assert!(
        export_help.stdout.contains("Usage: fiber sessions export"),
        "{}",
        export_help.stdout
    );
}
