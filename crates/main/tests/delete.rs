//! Binary-level tests of `fiber sessions delete` (`docs/invocation.md`,
//! "Deleting and pruning"): the built `fiber` runs with no hub running,
//! starts one, and the hub removes session directories built by hand.
//! Every run and every wait carries a deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use serde_json::json;
use support::*;

const ROOT: &str = "s_00000000000000d1";
const FORK: &str = "s_00000000000000d2";

/// The hub idles out soon after the run's connection closes, so none
/// lingers past the test.
fn setup() -> Setup {
    let setup = Setup::new();
    write_json(
        &setup.home().join("config.json"),
        &json!({"hub": {"idle_exit_ms": 200}}),
    );
    setup
}

/// The workspace's project directory, as the command derives it.
fn project(setup: &Setup) -> PathBuf {
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    log::sessions_dir(&setup.home(), &doors::project(&workspace))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Session `id` in the workspace's project, continuing `from`: its log,
/// an artifact and an unheld lock file. The first line starts the session
/// in this workspace, so `resolve` keeps it.
fn session(setup: &Setup, id: &str, from: Option<&str>) -> PathBuf {
    let dir = project(setup).join("sessions").join(id);
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    let mut payload = json!({"workspace": workspace,
        "variables": {"path": "/usr/bin", "names": [], "source": "inherited"}});
    if let Some(from) = from {
        payload["forked_from"] = json!({"session_id": from, "seq": 1});
    }
    let first = json!({"kind": "session_started", "session_id": id, "ts": 0,
        "schema_version": 1, "seq": 0, "payload": payload});
    fs::write(dir.join("events.jsonl"), format!("{first}\n")).unwrap();
    fs::write(dir.join("artifacts/a_1.txt"), b"bytes").unwrap();
    fs::write(dir.join("session.lock"), b"").unwrap();
    dir
}

/// Runs `fiber sessions delete` with `args` in the workspace, stdin null.
fn delete(setup: &Setup, args: &[&str]) -> std::process::Output {
    let mut all = vec!["sessions", "delete"];
    all.extend_from_slice(args);
    let mut command = setup.fiber(&all);
    command.current_dir(setup.workspace());
    run_to_exit("fiber sessions delete", command)
}

/// Waits under [`DEADLINE`] until `socket` is gone, naming `what`.
fn until_absent(socket: &Path, what: &str) {
    let socket = socket.to_owned();
    let (done, reached) = mpsc::channel();
    thread::spawn(move || {
        while socket.exists() {
            thread::yield_now();
        }
        done.send(()).unwrap_or(());
    });
    assert!(
        reached.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for {what}"
    );
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn yes_starts_a_hub_that_removes_the_session_and_keeps_its_worktree_and_row() {
    let setup = setup();
    let dir = session(&setup, ROOT, None);
    let worktree = project(&setup).join("worktrees").join(ROOT);
    fs::create_dir_all(&worktree).unwrap();
    fs::write(worktree.join("file"), b"kept").unwrap();
    let row = json!({"session_id": ROOT, "ts": 1, "project": "p", "workspace": "/w", "name": "n", "how": "exited"});
    fs::write(setup.home().join("recent.jsonl"), format!("{row}\n")).unwrap();
    let rows = fs::read(setup.home().join("recent.jsonl")).unwrap();
    assert!(!setup.hub_socket().exists(), "no hub runs beforehand");
    let output = delete(&setup, &["--yes", "s_00000000000000d"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), format!("{ROOT}\n"));
    assert!(!dir.exists(), "the log and artifacts are gone");
    assert_eq!(fs::read(worktree.join("file")).unwrap(), b"kept");
    assert_eq!(fs::read(setup.home().join("recent.jsonl")).unwrap(), rows);
    assert!(
        setup.hub_log().contains("hub_started"),
        "{}",
        setup.hub_log()
    );
    until_absent(&setup.hub_socket(), "the hub to idle out");
}

#[test]
fn a_held_session_exits_one_and_stays() {
    let setup = setup();
    let dir = session(&setup, ROOT, None);
    let lock = File::open(dir.join("session.lock")).unwrap();
    lock.try_lock().unwrap();
    let output = delete(&setup, &["--yes", ROOT]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("is held by another process"), "{stderr}");
    assert!(dir.join("events.jsonl").is_file());
    assert_eq!(text(&output.stdout), "");
    drop(lock);
    until_absent(&setup.hub_socket(), "the hub to idle out");
}

#[test]
fn dependents_exit_one_naming_the_fork_and_cascade_removes_both() {
    let setup = setup();
    let root = session(&setup, ROOT, None);
    let fork = session(&setup, FORK, Some(ROOT));
    let output = delete(&setup, &["--yes", ROOT]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.contains(FORK), "{stderr}");
    assert!(stderr.contains("--cascade"), "{stderr}");
    assert!(root.is_dir() && fork.is_dir());
    let output = delete(&setup, &["--cascade", "--yes", ROOT]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), format!("{ROOT}\n{FORK}\n"));
    assert!(!root.exists() && !fork.exists());
    until_absent(&setup.hub_socket(), "the hub to idle out");
}

#[test]
fn without_yes_and_without_a_terminal_it_is_usage_and_removes_nothing() {
    let setup = setup();
    let dir = session(&setup, ROOT, None);
    let output = delete(&setup, &[ROOT]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("--yes"));
    assert!(dir.join("events.jsonl").is_file());
    assert!(
        !setup.home().join("logs").join("hub.log").exists(),
        "no hub was started"
    );
}
