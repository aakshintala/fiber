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
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use serde_json::json;
use support::*;

const FORK: &str = "s_00000000000000d2";
const MOVED: &str = "s_00000000000000d3";

/// Appends a `moved` last line to `dir`'s log naming `next`: the parent's
/// post-move state, as `fiber sessions delete` meets it.
fn append_moved(dir: &Path, old: &str, next: &str) {
    let line = json!({"kind": "moved", "session_id": old, "ts": 1,
        "schema_version": 1, "seq": 3,
        "payload": {"new_session_id": next, "seq": 1, "workspace": "/w2"}});
    let mut log = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    log.push_str(&format!("{line}\n"));
    fs::write(dir.join("events.jsonl"), log).unwrap();
}

/// Session `id` in another project's sessions, continuing `from`: the
/// cross-project moved child, as `fiber sessions delete` meets it.
fn moved_child(setup: &Setup, id: &str, from: &str) -> PathBuf {
    let workspace = setup.root.path().join("elsewhere");
    fs::create_dir_all(&workspace).unwrap();
    let dir = log::sessions_dir(&setup.home(), &doors::project(&workspace)).join(id);
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let first = json!({"kind": "session_started", "session_id": id, "ts": 0,
        "schema_version": 1, "seq": 0,
        "payload": {"workspace": workspace,
        "variables": {"path": "/usr/bin", "names": [], "source": "inherited"},
        "forked_from": {"session_id": from, "seq": 1}}});
    fs::write(dir.join("events.jsonl"), format!("{first}\n")).unwrap();
    fs::write(dir.join("artifacts/a_1.txt"), b"bytes").unwrap();
    fs::write(dir.join("session.lock"), b"").unwrap();
    dir
}

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
#[track_caller]
fn delete(setup: &Setup, args: &[&str]) -> std::process::Output {
    let mut all = vec!["sessions", "delete"];
    all.extend_from_slice(args);
    let mut command = setup.fiber(&all);
    command.current_dir(setup.workspace());
    run_to_exit(setup.deadline, "fiber sessions delete", command)
}

/// Waits under the test's [`Deadline`] until `socket` is gone, naming
/// `what`: the hub holds the `run/` flock until `serve` returns, after
/// unlinking the socket, so a returned flock means the hub is gone.
#[track_caller]
fn until_absent(deadline: Deadline, socket: &Path, what: &str) {
    let run = socket.parent().map(Path::to_owned);
    let socket = socket.to_owned();
    let what = what.to_owned();
    bounded(deadline, &what.clone(), move || {
        if let Some(run) = run
            && let Ok(file) = File::open(&run)
        {
            file.lock().unwrap();
        }
        assert!(
            !socket.exists(),
            "{what}: the hub exited but left its socket"
        );
    });
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Accepts one connection, blocking: `None` is the stand-in ending,
/// `stop` set before a client connected.
fn accept_unless_stopped(listener: &UnixListener, stop: &AtomicBool) -> Option<UnixStream> {
    match listener.accept() {
        Ok((stream, _)) if !stop.load(Ordering::SeqCst) => Some(stream),
        _ => None,
    }
}

/// Sets `stop` when dropped, then connects to the socket so a thread
/// blocked in `accept` wakes and ends on every path, including a test
/// panic. The stream drops at once.
struct StopGuard {
    stop: Arc<AtomicBool>,
    socket: PathBuf,
}

impl Drop for StopGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        match UnixStream::connect(&self.socket) {
            Ok(_) | Err(_) => {}
        }
    }
}

#[test]
fn a_delete_that_meets_an_exiting_hub_waits_and_starts_a_new_one() {
    let setup = setup();
    let dir = session(&setup, ROOT, None);
    fs::create_dir_all(setup.home().join("run")).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let _guard = StopGuard {
        stop: Arc::clone(&stop),
        socket: setup.hub_socket(),
    };
    let (ready_tx, ready_rx) = mpsc::channel();
    let (released_tx, released_rx) = mpsc::channel();
    let home = setup.home();
    let deadline = setup.deadline;
    let thread_stop = Arc::clone(&stop);
    thread::spawn(move || {
        // The exiting hub: the `run/` lock as `listen::lock` takes it,
        // then `run/hub` bound 0600. The lock drops before the unlink,
        // the unlink before the held stream drops, so the hub the client
        // starts always gets the lock.
        let run = home.join("run");
        let lock = File::open(&run).ok();
        let locked = lock.filter(|file| file.try_lock().is_ok());
        let socket = run.join("hub");
        fs::remove_file(&socket).unwrap_or(());
        let Ok(listener) = UnixListener::bind(&socket) else {
            fs::remove_file(&socket).unwrap_or(());
            return;
        };
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap_or(());
        ready_tx.send(()).unwrap_or(());
        let Some(first) = accept_unless_stopped(&listener, &thread_stop) else {
            drop(locked);
            fs::remove_file(&socket).unwrap_or(());
            return;
        };
        drop(first);
        let Some(held) = accept_unless_stopped(&listener, &thread_stop) else {
            drop(locked);
            fs::remove_file(&socket).unwrap_or(());
            return;
        };
        drop(locked);
        fs::remove_file(&socket).unwrap_or(());
        drop(held);
        drop(listener);
        released_tx.send(()).unwrap_or(());
    });
    assert!(
        deadline.recv(&ready_rx).is_ok(),
        "the exiting hub is ready before the delete runs"
    );
    let output = delete(&setup, &["--yes", ROOT]);
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(text(&output.stdout), format!("{ROOT}\n"), "{stderr}");
    assert!(!dir.exists(), "the session is gone: {stderr}");
    assert!(
        deadline.recv(&released_rx).is_ok(),
        "the client outlasted both of the exiting hub's connections: {stderr}"
    );
    assert!(
        setup.hub_log().contains("hub_started"),
        "a real hub started: {stderr}"
    );
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
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
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
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
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
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
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

#[test]
fn dependents_exit_one_naming_the_moved_child_and_cascade_removes_both() {
    let setup = setup();
    let root = session(&setup, ROOT, None);
    append_moved(&root, ROOT, MOVED);
    let moved = moved_child(&setup, MOVED, ROOT);
    // The moved child continues the parent across projects: deleting the
    // parent names it and refuses without `--cascade`.
    let output = delete(&setup, &["--yes", ROOT]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.contains(MOVED), "{stderr}");
    assert!(stderr.contains("--cascade"), "{stderr}");
    assert!(root.is_dir() && moved.is_dir());
    let output = delete(&setup, &["--cascade", "--yes", ROOT]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), format!("{ROOT}\n{MOVED}\n"));
    assert!(!root.exists() && !moved.exists());
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
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
