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
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use support::*;

const FORK: &str = "s_00000000000000d2";
const MOVED: &str = "s_00000000000000d3";

/// Session `id` in the workspace's project with a `moved` last line
/// naming `next` at `ts`: the moved parent, as prune meets it. Rewrites
/// the log's marker last line, since a real moved log ends at the move.
fn moved_parent(setup: &Setup, id: &str, next: &str, ts: u64) -> PathBuf {
    let dir = session(setup, id, None, ts);
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    let first = json!({"kind": "session_started", "seq": 0,
        "payload": {"workspace": workspace}});
    let last = json!({"kind": "moved", "ts": ts,
        "payload": {"new_session_id": next, "seq": 1, "workspace": "/w2"}});
    fs::write(dir.join("events.jsonl"), format!("{first}\n{last}\n")).unwrap();
    dir
}

/// Session `id` continuing `from` at `ts` in another project's sessions:
/// the cross-project moved child, as prune meets it.
fn moved_child(setup: &Setup, id: &str, from: &str, ts: u64) -> PathBuf {
    let workspace = setup.root.path().join("elsewhere");
    fs::create_dir_all(&workspace).unwrap();
    let dir = log::sessions_dir(&setup.home(), &worktree::project(&workspace)).join(id);
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let first = json!({"kind": "session_started", "seq": 0,
        "payload": {"workspace": workspace,
        "forked_from": {"session_id": from, "seq": 1}}});
    let last = json!({"kind": "x", "ts": ts});
    fs::write(dir.join("events.jsonl"), format!("{first}\n{last}\n")).unwrap();
    fs::write(dir.join("artifacts/a_1.txt"), b"0123456789").unwrap();
    fs::write(dir.join("session.lock"), b"").unwrap();
    dir
}
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
    log::sessions_dir(&setup.home(), &worktree::project(&workspace))
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

#[test]
fn a_young_moved_child_blocks_then_cascade_removes_both() {
    let setup = setup();
    let (_probe, _probe_at, probe_ms) = probe(&setup);
    let root = moved_parent(&setup, ROOT, MOVED, 0);
    let moved = moved_child(&setup, MOVED, ROOT, probe_ms);
    let output = prune(&setup, &["--yes", "--older-than", "30d"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(out.contains(ROOT), "{out}");
    assert!(out.contains(MOVED), "{out}");
    assert!(
        root.is_dir() && moved.is_dir(),
        "the moved child blocks the root"
    );
    // The cascade prune needs a ready hub, not a fast cold start: starting
    // the hub here waits for its hello under the test deadline, and the
    // held connection keeps it from idling out before the prune connects.
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let output = prune(&setup, &["--cascade", "--yes", "--older-than", "30d"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(!root.exists() && !moved.exists());
    drop(client);
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

/// Restores the run directory's writable mode if the test exits early.
struct RunDirPermissions(PathBuf);

impl Drop for RunDirPermissions {
    fn drop(&mut self) {
        fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700)).unwrap_or(());
    }
}

#[test]
fn a_prune_that_cannot_bind_fails_then_meets_a_ready_one() {
    // Spawn-sabotage repro for #2121: with the same `run/` mode and prune
    // inputs, a cold-start cannot bind `run/hub`, while a held hub already
    // bound there lets the prune pass. `doors::hub::connect` retries the
    // absent socket only until its existing 5 s bind deadline, so the
    // cold run fails deterministically; the `did not bind` error is the
    // signal that the product reached that bounded failure. No stand-in,
    // sleeps, raised deadlines or budgets.
    let setup = setup();
    let dir = session(&setup, ROOT, None, 0);
    let run = setup.home().join("run");
    fs::create_dir(&run).unwrap();
    let _restore_run_permissions = RunDirPermissions(run.clone());
    fs::set_permissions(&run, fs::Permissions::from_mode(0o500)).unwrap();

    let output = prune(&setup, &["--yes", "--older-than", "30d"]);
    let stderr = text(&output.stderr);
    assert_ne!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("did not bind"),
        "the cold-start hub could not bind run/hub: {stderr}"
    );
    assert!(dir.join("events.jsonl").is_file(), "nothing is deleted");

    // Let the test start and hold a real hub, then restore the same
    // unwritable run-directory condition before the identical prune.
    fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    fs::set_permissions(&run, fs::Permissions::from_mode(0o500)).unwrap();
    let output = prune(&setup, &["--yes", "--older-than", "30d"]);
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(!dir.exists(), "the session is gone: {stderr}");

    // The hub needs write permission to unlink its socket on idle exit.
    fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
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
/// test's [`Deadline`]: the failure names `dir`, and there is no output
/// to return.
#[track_caller]
fn git_in_dir(deadline: Deadline, dir: &Path, args: &[&str]) {
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
    git_in_dir(setup.deadline, &workspace, &["init", "--quiet"]);
    fs::write(workspace.join("file.txt"), "x").unwrap();
    fs::write(
        workspace.join(".gitignore"),
        "target/\n.env\nlocal.secret\n",
    )
    .unwrap();
    git_in_dir(setup.deadline, &workspace, &["add", "."]);
    git_in_dir(
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
    git_in_dir(
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

/// A session `id` whose workspace is `workspace` in the project at
/// `project_dir`: its log and an unheld lock file, like `session` but
/// working anywhere.
fn wt_session(id: &str, workspace: &Path, project_dir: &Path) -> PathBuf {
    let dir = project_dir.join("sessions").join(id);
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
    git_in_dir(setup.deadline, &wt, &["add", "."]);
    git_in_dir(setup.deadline, &wt, &["commit", "--quiet", "-m", "second"]);
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
    let user = wt_session("s_00000000000000a1", &wt, &project(&setup));
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

/// Hook `fmt` running `script` in `dir`.
#[track_caller]
fn declare_hook(dir: &Path, script: &str) {
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("scripts/fmt.sh"), script).unwrap();
    fs::create_dir_all(dir.join(".fiber/config")).unwrap();
    fs::write(
        dir.join(".fiber/config/hooks.json"),
        r#"{"hooks":{"fmt":{"point":"after_tool","command":"sh","args":["scripts/fmt.sh"]}}}"#,
    )
    .unwrap();
}

/// A linked worktree of the workspace on `branch` at `name` beside it,
/// with its own hook script committed. Returned canonicalized.
#[track_caller]
fn linked(setup: &Setup, branch: &str, name: &str, script: &str) -> PathBuf {
    let path = setup.root.path().join(name);
    git_in_dir(
        setup.deadline,
        &setup.workspace(),
        &["worktree", "add", "-b", branch, path.to_str().unwrap()],
    );
    declare_hook(&path, script);
    git_in_dir(setup.deadline, &path, &["add", "."]);
    git_in_dir(setup.deadline, &path, &["commit", "--quiet", "-m", branch]);
    fs::canonicalize(&path).unwrap()
}

/// Runs `fiber approve --yes` in `dir`, under the test's [`Deadline`].
#[track_caller]
fn approve_in(setup: &Setup, dir: &Path) -> std::process::Output {
    let mut command = setup.fiber(&["approve", "--yes"]);
    command.current_dir(dir);
    run_to_exit(setup.deadline, "fiber approve", command)
}

/// The hash prefix the approval run approved for `label`, such as
/// `hook fmt` or `mcp_server shared`.
#[track_caller]
fn approved_prefix(output: &std::process::Output, label: &str) -> String {
    let stderr = text(&output.stderr);
    assert!(output.status.success(), "fiber approve: {stderr}");
    stderr
        .lines()
        .find_map(|line| line.strip_prefix(&format!("approved {label} ")))
        .expect("one approval")
        .to_owned()
}

/// The names under `dir`, sorted; none when it is absent.
fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// The approval file matching `prefix`.
#[track_caller]
fn approval(setup: &Setup, prefix: &str) -> String {
    let hits: Vec<String> = names(&project(setup).join("approvals"))
        .into_iter()
        .filter(|name| name.starts_with(prefix))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "{prefix} in {:?}",
        names(&project(setup).join("approvals"))
    );
    hits.into_iter().next().unwrap()
}

/// The copy directories under `pinned/`, sorted.
fn copies(setup: &Setup) -> Vec<String> {
    names(&setup.home().join("pinned"))
        .into_iter()
        .filter(|name| setup.home().join("pinned").join(name).is_dir())
        .collect()
}

/// The workspace with hook `fmt` on `main` and linked worktrees `wa` on
/// `a` and `wb` on `b` beside it, each with its own hook script, all
/// three approved. Returns each run's hash prefix.
#[track_caller]
fn approved_repo(setup: &Setup) -> (String, String, String) {
    repo(setup);
    declare_hook(&setup.workspace(), "main\n");
    git_in_dir(setup.deadline, &setup.workspace(), &["add", "."]);
    git_in_dir(
        setup.deadline,
        &setup.workspace(),
        &["commit", "--quiet", "-m", "hook"],
    );
    linked(setup, "a", "wa", "a\n");
    linked(setup, "b", "wb", "b\n");
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    let main = approved_prefix(&approve_in(setup, &workspace), "hook fmt");
    let a = approved_prefix(
        &approve_in(setup, &setup.root.path().join("wa")),
        "hook fmt",
    );
    let b = approved_prefix(
        &approve_in(setup, &setup.root.path().join("wb")),
        "hook fmt",
    );
    (main, a, b)
}

/// The `pinned  ` lines of prune's stdout.
fn pinned_lines(out: &str) -> Vec<&str> {
    out.lines()
        .filter(|line| line.starts_with("pinned  "))
        .collect()
}

#[test]
fn prune_removes_pinned_copies_no_worktree_matches() {
    let setup = setup();
    let (main, _, b) = approved_repo(&setup);
    let hash_b = approval(&setup, &b);
    let hash_main = approval(&setup, &main);
    git_in_dir(
        setup.deadline,
        &setup.workspace(),
        &[
            "worktree",
            "remove",
            setup.root.path().join("wb").to_str().unwrap(),
        ],
    );
    let dry = prune(&setup, &["--dry-run"]);
    assert_eq!(dry.status.code(), Some(0), "{}", text(&dry.stderr));
    let out = text(&dry.stdout);
    assert_eq!(pinned_lines(&out).len(), 1, "{out}");
    let fields: Vec<_> = pinned_lines(&out)[0].split("  ").collect();
    assert_eq!(fields.len(), 3, "{out}");
    let (hash, size) = (fields[1], fields[2]);
    assert_eq!(hash, hash_b);
    assert!(out.contains(&format!("would free {size}")), "{out}");
    assert!(
        setup.home().join("pinned").join(&hash_b).is_dir(),
        "dry-run deletes nothing"
    );
    let yes = prune(&setup, &["--yes"]);
    assert_eq!(yes.status.code(), Some(0), "{}", text(&yes.stderr));
    let out = text(&yes.stdout);
    assert_eq!(pinned_lines(&out).len(), 1, "{out}");
    assert!(out.contains(&format!("pinned  {hash_b}  {size}")), "{out}");
    assert!(out.contains(&format!("freed {size}")), "{out}");
    assert!(
        !setup.home().join("pinned").join(&hash_b).exists(),
        "the copy is gone"
    );
    assert!(
        !setup
            .home()
            .join("pinned")
            .join(format!("{hash_b}.ready"))
            .exists()
    );
    // The other copies stay with their bytes, and every approval stays.
    let hash_a = copies(&setup)
        .into_iter()
        .find(|name| {
            fs::read_to_string(
                setup
                    .home()
                    .join("pinned")
                    .join(name)
                    .join("scripts/fmt.sh"),
            )
            .unwrap()
                == "a\n"
        })
        .unwrap();
    assert_eq!(
        fs::read_to_string(
            setup
                .home()
                .join("pinned")
                .join(&hash_a)
                .join("scripts/fmt.sh")
        )
        .unwrap(),
        "a\n"
    );
    assert!(setup.home().join("pinned").join(&hash_main).is_dir());
    assert_eq!(names(&project(&setup).join("approvals")).len(), 3);
}

#[test]
fn prune_keeps_a_held_pinned_copy() {
    let setup = setup();
    let (main, a, _) = approved_repo(&setup);
    let hash_a = approval(&setup, &a);
    let hash_main = approval(&setup, &main);
    assert_ne!(hash_a, hash_main);
    // A shared hold on the marker, as a running session keeps.
    let ready = setup.home().join("pinned").join(format!("{hash_a}.ready"));
    let hold = File::options().read(true).write(true).open(&ready).unwrap();
    hold.lock_shared().unwrap();
    git_in_dir(
        setup.deadline,
        &setup.workspace(),
        &[
            "worktree",
            "remove",
            setup.root.path().join("wa").to_str().unwrap(),
        ],
    );
    let held = prune(&setup, &["--yes"]);
    assert_eq!(held.status.code(), Some(0), "{}", text(&held.stderr));
    let out = text(&held.stdout);
    assert_eq!(
        pinned_lines(&out),
        vec![format!("pinned  {hash_a}  skipped: a session holds it")],
        "{out}"
    );
    assert!(out.contains("freed 0 B"), "{out}");
    assert!(
        setup.home().join("pinned").join(&hash_a).is_dir(),
        "the copy stays"
    );
    drop(hold);
    let yes = prune(&setup, &["--yes"]);
    assert_eq!(yes.status.code(), Some(0), "{}", text(&yes.stderr));
    let out = text(&yes.stdout);
    assert_eq!(pinned_lines(&out).len(), 1, "{out}");
    assert!(
        !setup.home().join("pinned").join(&hash_a).exists(),
        "removed"
    );
}

#[test]
fn prune_under_a_git_redirect_judges_the_workspace_repository() {
    let setup = setup();
    repo(&setup);
    let old = session(&setup, ROOT, None, 0);
    // A decoy repository elsewhere, with one old exited session in its own
    // project and one approved copy.
    let decoy = setup.root.path().join("decoy");
    fs::create_dir_all(&decoy).unwrap();
    git_in_dir(setup.deadline, &decoy, &["init", "--quiet"]);
    fs::write(decoy.join("file.txt"), "x").unwrap();
    git_in_dir(setup.deadline, &decoy, &["add", "."]);
    git_in_dir(
        setup.deadline,
        &decoy,
        &["commit", "--quiet", "-m", "first"],
    );
    let decoy_canon = fs::canonicalize(&decoy).unwrap();
    let decoy_project = log::sessions_dir(&setup.home(), &worktree::project(&decoy_canon))
        .parent()
        .unwrap()
        .to_path_buf();
    let decoy_session = wt_session("s_00000000000000e5", &decoy_canon, &decoy_project);
    declare_hook(&decoy_canon, "decoy\n");
    git_in_dir(setup.deadline, &decoy_canon, &["add", "."]);
    git_in_dir(
        setup.deadline,
        &decoy_canon,
        &["commit", "--quiet", "-m", "hook"],
    );
    let approved = approve_in(&setup, &decoy_canon);
    assert!(
        approved.status.success(),
        "fiber approve: {}",
        text(&approved.stderr)
    );
    let decoy_copy = copies(&setup)
        .into_iter()
        .find(|name| {
            fs::read_to_string(
                setup
                    .home()
                    .join("pinned")
                    .join(name)
                    .join("scripts/fmt.sh"),
            )
            .unwrap()
                == "decoy\n"
        })
        .expect("one decoy copy");
    // Prune with the git environment redirected at the decoy repository.
    let mut command = setup.fiber(&["sessions", "prune", "--older-than", "30d", "--yes"]);
    command.current_dir(setup.workspace());
    command.env("GIT_DIR", decoy_canon.join(".git"));
    command.env("GIT_WORK_TREE", &decoy_canon);
    let output = run_to_exit(setup.deadline, "fiber sessions prune", command);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(!old.exists(), "the workspace session is gone");
    assert!(
        decoy_session.join("events.jsonl").is_file(),
        "the decoy session stays"
    );
    assert!(
        setup.home().join("pinned").join(&decoy_copy).is_dir(),
        "the decoy copy stays"
    );
    assert_eq!(
        pinned_lines(&out),
        ["pinned  skipped: GIT_DIR is set, so git may read another repository"],
        "{out}"
    );
}

/// Another git repository at `other` beside the workspace, declaring only
/// the `shared` server with the same bytes, committed. Returned
/// canonicalized: a different project from the workspace's.
#[track_caller]
fn other_repo(setup: &Setup) -> PathBuf {
    let dir = setup.root.path().join("other");
    fs::create_dir_all(dir.join("srv")).unwrap();
    fs::create_dir_all(dir.join(".fiber")).unwrap();
    fs::write(dir.join("srv/shared.js"), "shared\n").unwrap();
    fs::write(
        dir.join(".fiber/config.json"),
        r#"{"mcp":{"servers":{"shared":{"command":"node","args":["srv/shared.js"]}}}}"#,
    )
    .unwrap();
    git_in_dir(setup.deadline, &dir, &["init", "--quiet"]);
    fs::write(dir.join("file.txt"), "x").unwrap();
    git_in_dir(setup.deadline, &dir, &["add", "."]);
    git_in_dir(setup.deadline, &dir, &["commit", "--quiet", "-m", "first"]);
    fs::canonicalize(&dir).unwrap()
}

/// The content hash the per-machine approval for server `name` holds.
#[track_caller]
fn server_hash(setup: &Setup, name: &str) -> String {
    fs::read_dir(setup.home().join("approvals"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find_map(|path| {
            let body: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            (body.get("name").and_then(|name| name.as_str()) == Some(name))
                .then(|| path.file_name().unwrap().to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| panic!("no machine approval for server {name}"))
}

#[test]
fn prune_lists_and_removes_a_server_copy_no_project_record_names() {
    let setup = setup();
    repo(&setup);
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    // Three servers declared in the workspace, each with its own file.
    fs::create_dir_all(workspace.join("srv")).unwrap();
    fs::create_dir_all(workspace.join(".fiber")).unwrap();
    for name in ["gone", "shared", "legacy"] {
        fs::write(
            workspace.join("srv").join(format!("{name}.js")),
            format!("{name}\n"),
        )
        .unwrap();
    }
    fs::write(
        workspace.join(".fiber/config.json"),
        r#"{"mcp":{"servers":{
            "gone":{"command":"node","args":["srv/gone.js"]},
            "shared":{"command":"node","args":["srv/shared.js"]},
            "legacy":{"command":"node","args":["srv/legacy.js"]}}}}"#,
    )
    .unwrap();
    git_in_dir(setup.deadline, &workspace, &["add", "."]);
    git_in_dir(
        setup.deadline,
        &workspace,
        &["commit", "--quiet", "-m", "servers"],
    );
    // Another repository approves the same `shared` declaration first:
    // its project record names the copy, from another project.
    let other = other_repo(&setup);
    let other_project = log::sessions_dir(&setup.home(), &worktree::project(&other))
        .parent()
        .unwrap()
        .to_path_buf();
    assert_ne!(other_project, project(&setup));
    let approved_other = approve_in(&setup, &other);
    // The same declaration is the same copy, whichever project approves
    // it first.
    let shared_prefix = approved_prefix(&approved_other, "mcp_server shared");
    let approved = approve_in(&setup, &workspace);
    for server in ["gone", "legacy"] {
        approved_prefix(&approved, &format!("mcp_server {server}"));
    }
    // `shared` is already approved machine-wide, so this repository is
    // not asked about it again and holds no project record for it.
    assert!(
        !text(&approved.stderr).contains("shared"),
        "{}",
        text(&approved.stderr)
    );
    let gone = server_hash(&setup, "gone");
    let shared = server_hash(&setup, "shared");
    let legacy = server_hash(&setup, "legacy");
    assert!(shared.starts_with(&shared_prefix));
    assert_ne!(gone, shared);
    assert_ne!(gone, legacy);
    assert_ne!(shared, legacy);
    // `legacy` keeps its machine record but loses its project record: a
    // copy approved before project records existed.
    fs::remove_file(project(&setup).join("servers").join(&legacy)).unwrap();
    // Every declaration changes, so no worktree matches an approved copy.
    for name in ["gone", "shared", "legacy"] {
        fs::write(
            workspace.join("srv").join(format!("{name}.js")),
            format!("{name} changed\n"),
        )
        .unwrap();
    }
    let dry = prune(&setup, &["--dry-run"]);
    assert_eq!(dry.status.code(), Some(0), "{}", text(&dry.stderr));
    let out = text(&dry.stdout);
    assert_eq!(pinned_lines(&out).len(), 1, "{out}");
    let fields: Vec<_> = pinned_lines(&out)[0].split("  ").collect();
    assert_eq!(fields.len(), 3, "{out}");
    assert_eq!(fields[1], gone);
    let size = fields[2];
    assert!(out.contains(&format!("would free {size}")), "{out}");
    assert!(
        setup.home().join("pinned").join(&gone).is_dir(),
        "a dry run deletes nothing"
    );
    let yes = prune(&setup, &["--yes"]);
    assert_eq!(yes.status.code(), Some(0), "{}", text(&yes.stderr));
    let out = text(&yes.stdout);
    assert_eq!(pinned_lines(&out).len(), 1, "{out}");
    assert!(out.contains(&format!("pinned  {gone}  {size}")), "{out}");
    assert!(out.contains(&format!("freed {size}")), "{out}");
    assert!(
        !setup.home().join("pinned").join(&gone).exists(),
        "the copy is gone"
    );
    assert!(
        !setup
            .home()
            .join("pinned")
            .join(format!("{gone}.ready"))
            .exists(),
        "its marker is gone"
    );
    // The other copies stay: one named by another project's record, one
    // with no project record at all. Every approval record stays.
    assert!(setup.home().join("pinned").join(&shared).is_dir());
    assert!(setup.home().join("pinned").join(&legacy).is_dir());
    assert_eq!(names(&setup.home().join("approvals")).len(), 3);
}
