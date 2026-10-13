//! Binary-level tests of `fiber sessions prune` (`docs/invocation.md`,
//! "Deleting and pruning"): the built `fiber` lists and deletes old
//! sessions and diagnostic files, through a hub it starts when one is
//! needed. Every run and every wait carries a deadline.

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
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use support::*;

const FORK: &str = "s_00000000000000d2";
const YOUNG: &str = "s_00000000000000d3";

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

/// A probe file whose mtime times everything: old fixtures are 31 days
/// before it, young fixtures carry its milliseconds as `ts`.
fn probe(setup: &Setup) -> (PathBuf, SystemTime, u64) {
    let path = setup.root.path().join("probe");
    fs::write(&path, b"probe").unwrap();
    let at = fs::metadata(&path).unwrap().modified().unwrap();
    let ms = at
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    (path, at, ms)
}

/// Session `id` in the workspace's project, continuing `from`, its last
/// line carrying `ts`: its log, an artifact and an unheld lock file.
fn session(setup: &Setup, id: &str, from: Option<&str>, ts: u64) -> PathBuf {
    let dir = project(setup).join("sessions").join(id);
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    let mut payload = json!({"workspace": workspace});
    if let Some(from) = from {
        payload["forked_from"] = json!({"session_id": from, "seq": 1});
    }
    let first = json!({"kind": "session_started", "seq": 0, "payload": payload});
    let last = json!({"kind": "x", "ts": ts});
    fs::write(dir.join("events.jsonl"), format!("{first}\n{last}\n")).unwrap();
    fs::write(dir.join("artifacts/a_1.txt"), b"0123456789").unwrap();
    fs::write(dir.join("session.lock"), b"").unwrap();
    dir
}

/// Diagnostic file `name` in `kind` (`logs` or `crashes`) with `bytes`,
/// its mtime set to `at`.
#[track_caller]
fn diag(home: &Path, kind: &str, name: &str, bytes: &[u8], at: SystemTime) {
    let dir = home.join(kind);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), bytes).unwrap();
    File::options()
        .write(true)
        .open(dir.join(name))
        .unwrap()
        .set_modified(at)
        .unwrap();
}

/// Runs `fiber sessions prune` with `args` in the workspace, stdin null.
#[track_caller]
fn prune(setup: &Setup, args: &[&str]) -> std::process::Output {
    let mut all = vec!["sessions", "prune"];
    all.extend_from_slice(args);
    let mut command = setup.fiber(&all);
    command.current_dir(setup.workspace());
    run_to_exit(setup.deadline, "fiber sessions prune", command)
}

/// Waits under the test's [`Deadline`] until `socket` is gone, naming
/// `what`.
#[track_caller]
fn until_absent(deadline: Deadline, socket: &Path, what: &str) {
    let socket = socket.to_owned();
    let (done, reached) = mpsc::channel();
    thread::spawn(move || {
        while socket.exists() {
            thread::yield_now();
        }
        done.send(()).unwrap_or(());
    });
    assert!(
        deadline.recv(&reached).is_ok(),
        "waited until the deadline for {what}"
    );
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn old_at(probe_at: SystemTime) -> SystemTime {
    probe_at - Duration::from_secs(31 * 24 * 60 * 60)
}

#[test]
fn dry_run_lists_and_frees_nothing_without_starting_a_hub() {
    let setup = setup();
    let (_probe, probe_at, probe_ms) = probe(&setup);
    let dir = session(&setup, ROOT, None, 0);
    diag(
        &setup.home(),
        "logs",
        &format!("session-{ROOT}.log"),
        b"0123456789",
        old_at(probe_at),
    );
    diag(
        &setup.home(),
        "crashes",
        &format!("{ROOT}-{probe_ms}.txt"),
        b"crash",
        old_at(probe_at),
    );
    session(&setup, YOUNG, None, probe_ms);
    let output = prune(&setup, &["--dry-run", "--older-than", "30d"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(out.contains(ROOT), "{out}");
    assert!(out.contains(&format!("session-{ROOT}.log")), "{out}");
    assert!(out.contains("would free"), "{out}");
    assert!(dir.join("events.jsonl").is_file(), "nothing is deleted");
    assert!(
        setup
            .home()
            .join("logs")
            .join(format!("session-{ROOT}.log"))
            .is_file()
    );
    assert!(!setup.hub_socket().exists(), "no hub runs");
    assert!(
        !setup.home().join("logs").join("hub.log").exists(),
        "no hub log is written"
    );
}

#[test]
fn yes_starts_a_hub_removes_the_old_and_keeps_the_young() {
    let setup = setup();
    let (_probe, probe_at, probe_ms) = probe(&setup);
    let old = session(&setup, ROOT, None, 0);
    let young = session(&setup, YOUNG, None, probe_ms);
    diag(
        &setup.home(),
        "logs",
        &format!("session-{ROOT}.log"),
        b"0123456789",
        old_at(probe_at),
    );
    diag(&setup.home(), "logs", "fresh.log", b"fresh", probe_at);
    assert!(!setup.hub_socket().exists(), "no hub runs beforehand");
    let output = prune(&setup, &["--yes", "--older-than", "30d"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(out.contains(ROOT), "{out}");
    assert!(out.contains("freed"), "{out}");
    assert!(!old.exists(), "the old session is gone");
    assert!(young.join("events.jsonl").is_file(), "the young stays");
    assert!(
        !setup
            .home()
            .join("logs")
            .join(format!("session-{ROOT}.log"))
            .exists()
    );
    assert!(setup.home().join("logs/fresh.log").is_file());
    assert!(
        setup.hub_log().contains("hub_started"),
        "{}",
        setup.hub_log()
    );
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

#[test]
fn a_young_fork_blocks_then_cascade_removes_both() {
    let setup = setup();
    let (_probe, _probe_at, probe_ms) = probe(&setup);
    let root = session(&setup, ROOT, None, 0);
    let fork = session(&setup, FORK, Some(ROOT), probe_ms);
    let output = prune(&setup, &["--yes", "--older-than", "30d"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(out.contains(ROOT), "{out}");
    assert!(out.contains(FORK), "{out}");
    assert!(root.is_dir() && fork.is_dir(), "the fork blocks the root");
    // The cascade prune needs a ready hub, not a fast cold start: starting
    // the hub here waits for its hello under the test deadline, and the
    // held connection keeps it from idling out before the prune connects.
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let output = prune(&setup, &["--cascade", "--yes", "--older-than", "30d"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(!root.exists() && !fork.exists());
    drop(client);
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

/// Accepts one connection within the test deadline, yielding the thread
/// while none waits. `None` is the stand-in ending: `stop` set or the
/// deadline with no time left.
fn accept_within(
    listener: &UnixListener,
    deadline: Deadline,
    stop: &AtomicBool,
) -> Option<UnixStream> {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => return Some(stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if deadline.left().is_zero() {
                    return None;
                }
                thread::yield_now();
            }
            Err(_) => return None,
        }
    }
    None
}

/// Sets `stop` when dropped, so the stand-in thread ends on every path,
/// including a test panic.
struct StopGuard(Arc<AtomicBool>);

impl Drop for StopGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn a_prune_that_meets_a_silent_hub_fails_then_meets_a_ready_one() {
    // Cold-start contention repro for #2121 (reworked: a stand-in that
    // never answers hello, then the same prune against a held ready hub). A stand-in holds `run/`'s lock and
    // `run/hub`, accepting connections but never answering hello, so a
    // prune that must cold-start against the contended socket sits in
    // its bounded hello wait (`doors::hub::CONNECT_DEADLINE`, read-only)
    // until the wait expires, and exits nonzero with the session still
    // on disk. Releasing the stand-in and holding a ready hub — the fix
    // `a_young_fork_blocks_then_cascade_removes_both` relies on — makes
    // the same prune pass. No sleeps on the real clock, no raised
    // deadlines or budgets, no repeated runs: the 5 s wait is
    // the product's own hello bound, awaited through `run_to_exit`'s
    // deadline, and the `accepted` signal proves the prune spoke to the
    // stand-in before the test asserts on the failure.
    let setup = setup();
    let dir = session(&setup, ROOT, None, 0);
    fs::create_dir_all(setup.home().join("run")).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let _guard = StopGuard(Arc::clone(&stop));
    let (ready_tx, ready_rx) = mpsc::channel();
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (released_tx, released_rx) = mpsc::channel();
    let home = setup.home();
    let deadline = setup.deadline;
    let thread_stop = Arc::clone(&stop);
    thread::spawn(move || {
        // The contended socket: the `run/` lock as `listen::lock` takes
        // it, then `run/hub` bound 0600. Every accepted connection is
        // held open with no bytes written, so the client's hello read
        // always ends in its bounded wait, never in a race retry; the
        // lock drops before the unlink, so the hub the test starts next
        // always gets both.
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
        listener.set_nonblocking(true).unwrap_or(());
        ready_tx.send(()).unwrap_or(());
        let mut held = Vec::new();
        let mut accepted = false;
        while !thread_stop.load(Ordering::SeqCst) {
            let Some(stream) = accept_within(&listener, deadline, &thread_stop) else {
                break;
            };
            held.push(stream);
            if !accepted {
                accepted = true;
                accepted_tx.send(()).unwrap_or(());
            }
        }
        drop(held);
        drop(listener);
        drop(locked);
        fs::remove_file(&socket).unwrap_or(());
        released_tx.send(()).unwrap_or(());
    });
    assert!(
        deadline.recv(&ready_rx).is_ok(),
        "the stand-in holds the socket before the prune runs"
    );
    // Without a ready hub the prune cold-starts against the contended
    // socket: its bounded hello wait expires and it exits nonzero with
    // the session still on disk.
    let output = prune(&setup, &["--yes", "--older-than", "30d"]);
    let stderr = text(&output.stderr);
    assert_ne!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("did not say hub_hello"),
        "the prune failed in its bounded hello wait: {stderr}"
    );
    assert!(text(&output.stdout).contains(ROOT), "{stderr}");
    assert!(
        dir.join("events.jsonl").is_file(),
        "nothing is deleted: {stderr}"
    );
    assert!(
        deadline.recv(&accepted_rx).is_ok(),
        "the prune spoke to the stand-in: {stderr}"
    );
    // Holding a ready hub removes the contention: the same prune passes.
    stop.store(true, Ordering::SeqCst);
    assert!(
        deadline.recv(&released_rx).is_ok(),
        "the stand-in released the socket"
    );
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let output = prune(&setup, &["--yes", "--older-than", "30d"]);
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(!dir.exists(), "the session is gone: {stderr}");
    drop(client);
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

#[test]
fn without_older_than_the_session_stays_and_the_diagnostics_go() {
    let setup = setup();
    let (_probe, probe_at, _probe_ms) = probe(&setup);
    let dir = session(&setup, ROOT, None, 0);
    diag(
        &setup.home(),
        "logs",
        &format!("session-{ROOT}.log"),
        b"0123456789",
        old_at(probe_at),
    );
    let output = prune(&setup, &["--yes"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(dir.join("events.jsonl").is_file(), "the session stays");
    assert!(
        !setup
            .home()
            .join("logs")
            .join(format!("session-{ROOT}.log"))
            .exists(),
        "the diagnostics go"
    );
}

#[test]
fn without_yes_and_without_a_terminal_it_is_usage_and_removes_nothing() {
    let setup = setup();
    let (_probe, probe_at, _probe_ms) = probe(&setup);
    let dir = session(&setup, ROOT, None, 0);
    diag(
        &setup.home(),
        "logs",
        &format!("session-{ROOT}.log"),
        b"0123456789",
        old_at(probe_at),
    );
    let output = prune(&setup, &["--older-than", "30d"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("--yes"));
    assert!(dir.join("events.jsonl").is_file());
    assert!(
        setup
            .home()
            .join("logs")
            .join(format!("session-{ROOT}.log"))
            .is_file()
    );
    assert!(
        !setup.home().join("logs").join("hub.log").exists(),
        "no hub was started"
    );
}

/// A `git` command in its own process group, stdin null and output piped,
/// for [`run_to_exit`].
fn git_command() -> Command {
    let mut command = Command::new("git");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command
}

/// Runs `git` with `args` in `dir`, as the worktree tests do, under the
/// test's [`Deadline`].
#[track_caller]
fn git(deadline: Deadline, dir: &Path, args: &[&str]) {
    let mut command = git_command();
    command
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir);
    let output = run_to_exit(deadline, "git", command);
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        text(&output.stderr)
    );
}

/// Makes the workspace a git repository with one commit: `file.txt` and a
/// `.gitignore` matching `target/`, `.env` and `local.secret`.
#[track_caller]
fn repo(setup: &Setup) {
    let workspace = setup.workspace();
    git(setup.deadline, &workspace, &["init", "--quiet"]);
    fs::write(workspace.join("file.txt"), "x").unwrap();
    fs::write(
        workspace.join(".gitignore"),
        "target/\n.env\nlocal.secret\n",
    )
    .unwrap();
    git(setup.deadline, &workspace, &["add", "."]);
    git(
        setup.deadline,
        &workspace,
        &["commit", "--quiet", "-m", "first"],
    );
}

/// A kept worktree `id` on branch `fiber/<id>` under the project's
/// `worktrees/`, returned canonicalized.
#[track_caller]
fn kept(setup: &Setup, id: &str) -> PathBuf {
    let path = project(setup).join("worktrees").join(id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let branch = format!("fiber/{id}");
    let target = path.to_string_lossy().into_owned();
    git(
        setup.deadline,
        setup.workspace().as_path(),
        &["worktree", "add", "-b", &branch, &target],
    );
    fs::canonicalize(&path).unwrap()
}

/// Whether `refs/heads/fiber/<id>` still exists in the workspace.
#[track_caller]
fn branch_exists(setup: &Setup, id: &str) -> bool {
    let mut command = git_command();
    command.arg("-C").arg(setup.workspace()).args([
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("refs/heads/fiber/{id}"),
    ]);
    run_to_exit(setup.deadline, "git rev-parse", command)
        .status
        .success()
}

/// A session `id` whose workspace is `workspace`: its log and an unheld
/// lock file, like `session` but working anywhere.
fn wt_session(setup: &Setup, id: &str, workspace: &Path) -> PathBuf {
    let dir = project(setup).join("sessions").join(id);
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let first = json!({"kind": "session_started", "seq": 0, "payload": {"workspace": workspace}});
    let last = json!({"kind": "x", "ts": 0});
    fs::write(dir.join("events.jsonl"), format!("{first}\n{last}\n")).unwrap();
    fs::write(dir.join("session.lock"), b"").unwrap();
    dir
}

#[test]
fn a_clean_worktree_is_removed_with_its_branch() {
    let setup = setup();
    repo(&setup);
    let wt = kept(&setup, "s_00000000000000e1");
    let output = prune(&setup, &["--yes"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(
        out.contains("worktree  s_00000000000000e1  fiber/s_00000000000000e1  clean"),
        "{out}"
    );
    assert!(!wt.exists(), "the worktree is gone");
    assert!(
        !branch_exists(&setup, "s_00000000000000e1"),
        "the branch is gone"
    );
}

#[test]
fn a_dirty_worktree_is_skipped_naming_uncommitted_files() {
    let setup = setup();
    repo(&setup);
    let wt = kept(&setup, "s_00000000000000e2");
    fs::write(wt.join("notes.txt"), "scratch").unwrap();
    let output = prune(&setup, &["--yes"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(
        out.contains("skipped: removing it would lose uncommitted files"),
        "{out}"
    );
    assert!(!out.contains("uncommitted or ignored"), "{out}");
    assert!(wt.is_dir(), "the worktree stays");
    assert!(
        branch_exists(&setup, "s_00000000000000e2"),
        "the branch stays"
    );
}

#[test]
fn an_ignored_only_worktree_is_removed() {
    let setup = setup();
    repo(&setup);
    let wt = kept(&setup, "s_00000000000000e3");
    fs::create_dir_all(wt.join("target")).unwrap();
    fs::write(wt.join("target/out.bin"), "1234567890").unwrap();
    fs::write(wt.join(".env"), "123456").unwrap();
    let output = prune(&setup, &["--dry-run"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(out.contains("ignored 16 B: target/, .env"), "{out}");
    assert!(wt.is_dir(), "a dry run removes nothing");
    assert!(
        branch_exists(&setup, "s_00000000000000e3"),
        "the branch stays"
    );
    let output = prune(&setup, &["--yes"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(out.contains("ignored 16 B: target/, .env"), "{out}");
    assert!(!wt.exists(), "the worktree is gone");
    assert!(
        !branch_exists(&setup, "s_00000000000000e3"),
        "the branch is gone"
    );
}

#[test]
fn a_unique_commit_worktree_is_skipped_naming_it() {
    let setup = setup();
    repo(&setup);
    let wt = kept(&setup, "s_00000000000000e4");
    fs::write(wt.join("more.txt"), "y").unwrap();
    git(setup.deadline, &wt, &["add", "."]);
    git(setup.deadline, &wt, &["commit", "--quiet", "-m", "second"]);
    let output = prune(&setup, &["--yes"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(
        out.contains("skipped: removing it would lose commits found nowhere else"),
        "{out}"
    );
    assert!(wt.is_dir(), "the worktree stays");
    assert!(
        branch_exists(&setup, "s_00000000000000e4"),
        "the branch stays"
    );
}

#[test]
fn force_removes_the_dirty_worktree() {
    let setup = setup();
    repo(&setup);
    let wt = kept(&setup, "s_00000000000000e2");
    fs::write(wt.join("notes.txt"), "scratch").unwrap();
    let output = prune(&setup, &["--yes", "--force"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(out.contains("forced: loses uncommitted files"), "{out}");
    assert!(!out.contains("uncommitted or ignored"), "{out}");
    assert!(!wt.exists(), "the worktree is gone");
    assert!(
        !branch_exists(&setup, "s_00000000000000e2"),
        "the branch is gone"
    );
}

#[test]
fn a_running_session_keeps_its_worktree_under_force() {
    let setup = setup();
    repo(&setup);
    let wt = kept(&setup, "s_00000000000000e1");
    let user = wt_session(&setup, "s_00000000000000a1", &wt);
    let held = match log::try_hold(&user) {
        Ok(log::Hold::Held(held)) => held,
        Ok(log::Hold::Busy) | Err(_) => panic!("the user's lock is held"),
    };
    let output = prune(&setup, &["--yes", "--force"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(
        out.contains("worktree  s_00000000000000e1  skipped: a running session works in it"),
        "{out}"
    );
    assert!(wt.is_dir(), "the worktree stays");
    assert!(
        branch_exists(&setup, "s_00000000000000e1"),
        "the branch stays"
    );
    drop(held);
}
