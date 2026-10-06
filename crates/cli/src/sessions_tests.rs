//! `fiber sessions export`'s resolver and printer, with the session
//! directories built by hand.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::ErrorCode;

use super::{export, run};

const FIRST: &str = "{\"seq\":0,\"kind\":\"a\"}\n";
const SECOND: &str = "{\"seq\":1,\"kind\":\"b\"}\n";
const TORN: &str = "{\"seq\":2,\"kin";

/// Fiber home and a workspace in a temporary directory, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let setup = Self {
            root: fakes::TempDir::new("fiber-sessions"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        fs::create_dir_all(setup.root.path().join("workspace")).unwrap();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// The workspace as the command sees it: resolved, so it names the
    /// same project as `doors::project` derives inside `run`.
    fn workspace(&self) -> PathBuf {
        fs::canonicalize(self.root.path().join("workspace")).unwrap()
    }

    /// The project's sessions directory, as `run` derives it: a workspace
    /// outside any repository is its own project.
    fn sessions(&self) -> PathBuf {
        log::sessions_dir(&self.home(), &self.workspace())
    }

    /// A session directory with two complete lines, a torn tail, one
    /// artifact and a lock file.
    fn session(&self, id: &str) {
        let dir = self.sessions().join(id);
        fs::create_dir_all(dir.join("artifacts")).unwrap();
        fs::write(dir.join("events.jsonl"), format!("{FIRST}{SECOND}{TORN}")).unwrap();
        fs::write(dir.join("artifacts/a_1.txt"), "artifact\n").unwrap();
        fs::write(dir.join("session.lock"), "held").unwrap();
    }
}

#[test]
fn a_unique_prefix_exports_the_complete_lines_and_the_artifact() {
    let setup = Setup::new();
    setup.session("s_exportaa01");
    let mut out = Vec::new();
    let target = run(
        &setup.home(),
        &setup.workspace(),
        "s_exportaa",
        None,
        &mut out,
    )
    .unwrap();
    assert_eq!(target, setup.workspace().join("s_exportaa01"));
    assert_eq!(out, format!("{}\n", target.display()).into_bytes());
    assert_eq!(
        fs::read(target.join("events.jsonl")).unwrap(),
        format!("{FIRST}{SECOND}").into_bytes()
    );
    assert_eq!(
        fs::read(target.join("artifacts/a_1.txt")).unwrap(),
        b"artifact\n"
    );
    assert!(!target.join("session.lock").exists());
}

#[test]
fn an_explicit_relative_path_lands_under_the_workspace() {
    let setup = Setup::new();
    setup.session("s_exportbb01");
    let mut out = Vec::new();
    let target = run(
        &setup.home(),
        &setup.workspace(),
        "s_exportbb01",
        Some(Path::new("deep/out")),
        &mut out,
    )
    .unwrap();
    assert_eq!(target, setup.workspace().join("deep/out"));
    assert_eq!(out, format!("{}\n", target.display()).into_bytes());
    assert_eq!(
        fs::read(target.join("events.jsonl")).unwrap(),
        format!("{FIRST}{SECOND}").into_bytes()
    );
    assert_eq!(
        fs::read(target.join("artifacts/a_1.txt")).unwrap(),
        b"artifact\n"
    );
}

#[test]
fn an_unknown_id_is_not_found() {
    let setup = Setup::new();
    setup.session("s_exportdd01");
    let e = run(
        &setup.home(),
        &setup.workspace(),
        "s_missing",
        None,
        &mut Vec::new(),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::SessionNotFound);
}

#[test]
fn an_ambiguous_prefix_names_both_matches() {
    let setup = Setup::new();
    setup.session("s_amb11111");
    setup.session("s_amb22222");
    let e = run(
        &setup.home(),
        &setup.workspace(),
        "s_amb",
        None,
        &mut Vec::new(),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::Usage);
    assert!(e.message.contains("s_amb11111"), "{}", e.message);
    assert!(e.message.contains("s_amb22222"), "{}", e.message);
}

/// The child's marker: set, the test runs `export` and exits with its code.
const CHILD: &str = "FIBER_CLI_TEST_CHILD";

/// How long the child may run before the test kills it and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(60);

/// The session the child exports.
const CHILD_ID: &str = "s_childexport01";

#[test]
fn export_exits_zero_and_writes_the_export() {
    // Runs in a child with the parent's Fiber home and workspace, so the
    // exit code is `export`'s own. The parent checks the code and the
    // export: a code alone would not catch an `export` that returns 0
    // without copying.
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(export(CHILD_ID, None));
    }
    let setup = Setup::new();
    setup.session(CHILD_ID);
    let name = module_path!().split_once("::").unwrap().1;
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{name}::export_exits_zero_and_writes_the_export"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FIBER_HOME", setup.home())
        .env(CHILD, "1")
        .current_dir(setup.workspace())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(child.wait().unwrap()));
    let Ok(status) = rx.recv_timeout(CHILD_DEADLINE) else {
        fakes::kill_pid(pid, "KILL").unwrap();
        panic!("waited {CHILD_DEADLINE:?} for `fiber sessions export` to exit");
    };
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert_eq!(
        fs::read(setup.workspace().join(CHILD_ID).join("events.jsonl")).unwrap(),
        format!("{FIRST}{SECOND}").into_bytes()
    );
}
