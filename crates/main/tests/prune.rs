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
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use support::*;

const ROOT: &str = "s_00000000000000d1";
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
fn prune(setup: &Setup, args: &[&str]) -> std::process::Output {
    let mut all = vec!["sessions", "prune"];
    all.extend_from_slice(args);
    let mut command = setup.fiber(&all);
    command.current_dir(setup.workspace());
    run_to_exit("fiber sessions prune", command)
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
    until_absent(&setup.hub_socket(), "the hub to idle out");
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
    let output = prune(&setup, &["--cascade", "--yes", "--older-than", "30d"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(!root.exists() && !fork.exists());
    until_absent(&setup.hub_socket(), "the hub to idle out");
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

/// Runs `git` with `args` in `dir`, as the worktree tests do.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} in {}", dir.display());
}

/// Makes the workspace a git repository with one commit: `file.txt` and a
/// `.gitignore` matching `local.secret`.
fn repo(setup: &Setup) {
    let workspace = setup.workspace();
    git(&workspace, &["init", "--quiet"]);
    fs::write(workspace.join("file.txt"), "x").unwrap();
    fs::write(workspace.join(".gitignore"), "local.secret\n").unwrap();
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "--quiet", "-m", "first"]);
}

/// A kept worktree `id` on branch `fiber/<id>` under the project's
/// `worktrees/`, returned canonicalized.
fn kept(setup: &Setup, id: &str) -> PathBuf {
    let path = project(setup).join("worktrees").join(id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let branch = format!("fiber/{id}");
    let target = path.to_string_lossy().into_owned();
    git(
        setup.workspace().as_path(),
        &["worktree", "add", "-b", &branch, &target],
    );
    fs::canonicalize(&path).unwrap()
}

/// Whether `refs/heads/fiber/<id>` still exists in the workspace.
fn branch_exists(setup: &Setup, id: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(setup.workspace())
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/fiber/{id}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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
        out.contains("skipped: removing it would lose uncommitted or ignored files"),
        "{out}"
    );
    assert!(wt.is_dir(), "the worktree stays");
    assert!(
        branch_exists(&setup, "s_00000000000000e2"),
        "the branch stays"
    );
}

#[test]
fn an_ignored_only_worktree_is_skipped() {
    let setup = setup();
    repo(&setup);
    let wt = kept(&setup, "s_00000000000000e3");
    fs::write(wt.join("local.secret"), "secret").unwrap();
    let output = prune(&setup, &["--yes"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(
        out.contains("skipped: removing it would lose uncommitted or ignored files"),
        "{out}"
    );
    assert!(wt.is_dir(), "the worktree stays");
}

#[test]
fn a_unique_commit_worktree_is_skipped_naming_it() {
    let setup = setup();
    repo(&setup);
    let wt = kept(&setup, "s_00000000000000e4");
    fs::write(wt.join("more.txt"), "y").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "--quiet", "-m", "second"]);
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
    assert!(
        out.contains("forced: loses uncommitted or ignored files"),
        "{out}"
    );
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
