//! Binary-level tests of `fiber ask --worktree` (`docs/invocation.md`,
//! "Isolation"): the session runs in a new worktree, recorded on
//! `session_started`, and removed at the end when it holds nothing to lose.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs::TryLockError;
use std::fs::{self, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};
use support::{
    Deadline, Setup, function_call, git, group_alive, hello, kinds, run_to_exit, stream,
};

/// A repository with one commit on `main` at `dir`.
#[track_caller]
fn init_repo(deadline: Deadline, dir: &Path) {
    git(deadline, dir, &["init", "--quiet"]);
    fs::write(dir.join("file.txt"), "x").unwrap();
    git(deadline, dir, &["add", "."]);
    git(deadline, dir, &["commit", "--quiet", "-m", "first"]);
}

/// One `fiber` invocation launched from `cwd`, under the test's
/// environment with `extra_env` added: stdio is piped, the caller waits.
fn ask_command(setup: &Setup, cwd: &Path, extra_env: &[(&str, &str)], args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", setup.root.path())
        .env("FIBER_HOME", setup.home())
        .env("FIBER_TEST_FAKE_KEY", "sk-test")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command
}

/// The project's key for the workspace, naming its sessions and worktrees.
/// The blocking `git` runs on a thread under the test's one deadline.
#[track_caller]
fn key_of(deadline: Deadline, workspace: &Path) -> String {
    let workspace = workspace.to_owned();
    support::bounded(deadline, "the project key", move || {
        log::project_key(&doors::project(&workspace))
    })
}

/// The stdout lines parsed as JSON.
fn lines(out: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The `session_started` line's payload.
fn started(lines: &[Value]) -> &Value {
    lines
        .iter()
        .find(|line| line["kind"] == "session_started")
        .expect("session_started is on stdout")
}

/// Whether `refs/heads/<branch>` still exists in the repository.
#[track_caller]
fn branch_exists(deadline: Deadline, repo: &Path, branch: &str) -> bool {
    !git(deadline, repo, &["branch", "--list", branch]).is_empty()
}

#[test]
fn ask_worktree_runs_in_a_new_worktree_and_removes_it_clean() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree` to exit",
        ask_command(
            &setup,
            &setup.workspace(),
            &[],
            &["ask", "--worktree", "hi"],
        ),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).is_empty());
    let lines = lines(&out);
    let first = started(&lines);
    let id = first["session_id"].as_str().unwrap();
    let workspace = first["payload"]["workspace"].as_str().unwrap();
    let worktree = &first["payload"]["worktree"];
    let branch = worktree["branch"].as_str().unwrap();
    assert_eq!(worktree["path"].as_str().unwrap(), workspace);
    let repository = fs::canonicalize(setup.workspace().join(".git")).unwrap();
    assert_eq!(
        worktree["repository"].as_str().unwrap(),
        repository.to_str().unwrap()
    );
    let home = fs::canonicalize(setup.home()).unwrap();
    let expected = home
        .join("projects")
        .join(key_of(setup.deadline, &setup.workspace()))
        .join("worktrees")
        .join(id);
    assert_eq!(workspace, expected.to_str().unwrap());
    assert_eq!(branch, format!("fiber/{id}"));
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    // The session directory is under the same project key.
    assert!(
        home.join("projects")
            .join(key_of(setup.deadline, &setup.workspace()))
            .join("sessions")
            .join(id)
            .join("events.jsonl")
            .is_file()
    );
    // Clean, so both are gone after exit.
    assert!(!Path::new(workspace).exists());
    assert!(!branch_exists(setup.deadline, &setup.workspace(), branch));
}

#[test]
fn ask_worktree_from_a_subdirectory_records_the_worktree_root() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());
    let sub = setup.workspace().join("sub");
    fs::create_dir(&sub).unwrap();

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree` from a subdirectory to exit",
        ask_command(&setup, &sub, &[], &["ask", "--worktree", "hi"]),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = lines(&out);
    let first = started(&lines);
    let workspace = first["payload"]["workspace"].as_str().unwrap();
    assert_eq!(
        first["payload"]["worktree"]["path"].as_str().unwrap(),
        workspace
    );
    // The worktree's root, not the subdirectory launched from: the whole
    // repository was checked out (as `create`'s own test pins file by
    // file), and the session sits under the repository's project.
    assert!(!Path::new(workspace).starts_with(&sub));
    let home = fs::canonicalize(setup.home()).unwrap();
    let worktrees = home
        .join("projects")
        .join(key_of(setup.deadline, &setup.workspace()))
        .join("worktrees");
    assert!(Path::new(workspace).starts_with(&worktrees));
    assert!(
        home.join("projects")
            .join(key_of(setup.deadline, &setup.workspace()))
            .join("sessions")
            .is_dir()
    );
    // Clean, so the worktree is gone after exit.
    assert!(!Path::new(workspace).exists());
}

#[test]
fn ask_worktree_outside_a_repository_is_usage() {
    let setup = Setup::new();

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree` outside a repository to exit",
        ask_command(
            &setup,
            &setup.workspace(),
            &[],
            &["ask", "--worktree", "hi"],
        ),
    );
    assert_eq!(out.status.code(), Some(2));
    let lines = lines(&out);
    let last = lines.last().expect("a fiber_exited line");
    assert_eq!(last["kind"], "fiber_exited");
    assert_eq!(last.get("session_id"), None);
    assert_eq!(last["payload"]["exit_code"], 2);
    assert_eq!(last["payload"]["error"]["code"], "usage");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.lines().count(), 1);
    assert!(stderr.starts_with("fiber: "), "{stderr}");
    assert!(!setup.home().join("projects").exists());
}

#[test]
fn ask_worktree_with_resume_is_usage_and_creates_nothing() {
    let setup = Setup::new();

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree --resume` to exit",
        ask_command(
            &setup,
            &setup.workspace(),
            &[],
            &["ask", "--resume", "s_0123456789abcdef", "--worktree", "hi"],
        ),
    );
    assert_eq!(out.status.code(), Some(2));
    let lines = lines(&out);
    let last = lines.last().expect("a fiber_exited line");
    assert_eq!(last["kind"], "fiber_exited");
    assert_eq!(last["payload"]["error"]["code"], "usage");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.lines().count(), 1);
    assert!(!setup.home().join("projects").exists());
}

#[test]
fn a_signal_while_the_post_checkout_hook_runs_stops_it_and_leaves_nothing() {
    let setup = Setup::new();
    init_repo(setup.deadline, &setup.workspace());
    let hooks = Path::new(env!("CARGO_MANIFEST_DIR")).join("../worktree/fixtures/holding-hook");
    git(
        setup.deadline,
        &setup.workspace(),
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    let fifo = setup.root.path().join("hook.fifo");
    fakes::fifo(&fifo);
    let child = ask_command(
        &setup,
        &setup.workspace(),
        &[("FIBER_TEST_HOOK_FIFO", fifo.to_str().unwrap())],
        &["ask", "--worktree", "hi"],
    )
    .spawn()
    .unwrap();
    let fiber_pid = child.id();
    let watchdog = Watchdog::group(fiber_pid);
    // The hook writes its pid and process group once it runs: only then
    // is the signal's target the hook's group.
    let hook = support::bounded(setup.deadline, "the hook's pid and group", move || {
        fs::read_to_string(&fifo)
    })
    .unwrap();
    let mut words = hook.split_whitespace();
    let hook_pid: u32 = words.next().unwrap().parse().unwrap();
    let hook_group: u32 = words.next().unwrap().parse().unwrap();
    assert!(hook_pid > 1 && hook_group > 1);
    support::kill_pid(setup.deadline, fiber_pid, "TERM").unwrap();
    let (done, finished) = std::sync::mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let out = match setup.deadline.recv(&finished) {
        Ok(out) => out.unwrap(),
        Err(_) => panic!("waited for `fiber ask --worktree` to exit on SIGTERM"),
    };
    assert_eq!(out.status.code(), Some(143));
    assert!(
        out.stdout.is_empty(),
        "neither the hook nor a line may reach stdout"
    );
    assert!(
        out.stderr.is_empty(),
        "a recorded signal writes nothing to stderr"
    );
    assert!(
        fakes::group_empties(hook_group, setup.deadline),
        "the hook's group is gone"
    );
    let key = key_of(setup.deadline, &setup.workspace());
    let worktrees = setup.home().join("projects").join(&key).join("worktrees");
    let empty = !worktrees.exists() || fs::read_dir(&worktrees).unwrap().count() == 0;
    assert!(empty, "no worktree entry remains");
    assert!(
        git(
            setup.deadline,
            &setup.workspace(),
            &["branch", "--list", "fiber/*"]
        )
        .is_empty(),
        "no fiber/ branch remains"
    );
    assert!(
        !setup
            .home()
            .join("projects")
            .join(&key)
            .join("sessions")
            .exists(),
        "no session directory exists"
    );
    watchdog.stand_down(setup.deadline.cleanup_phase());
    assert!(!group_alive(setup.deadline, fiber_pid));
}

#[test]
fn hook_output_never_reaches_fiber_s_streams() {
    let setup = Setup::new();
    init_repo(setup.deadline, &setup.workspace());
    let hooks = Path::new(env!("CARGO_MANIFEST_DIR")).join("../worktree/fixtures/failing-hook");
    git(
        setup.deadline,
        &setup.workspace(),
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree` with a failing hook to exit",
        ask_command(
            &setup,
            &setup.workspace(),
            &[],
            &["ask", "--worktree", "hi"],
        ),
    );
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines = lines(&out);
    assert!(!lines.is_empty(), "every stdout line parses as JSON");
    let last = lines.last().unwrap();
    assert_eq!(last["kind"], "fiber_exited");
    assert_eq!(last.get("session_id"), None);
    assert_eq!(last["payload"]["error"]["code"], "io_failed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.lines().count(), 1);
    assert!(stderr.starts_with("fiber: "), "{stderr}");
    for marker in ["hook-marker-out", "hook-marker-err"] {
        assert!(!stdout.contains(marker), "{marker} reached stdout");
        assert!(!stderr.contains(marker), "{marker} reached stderr");
    }
    let key = key_of(setup.deadline, &setup.workspace());
    let worktrees = setup.home().join("projects").join(&key).join("worktrees");
    let empty = !worktrees.exists() || fs::read_dir(&worktrees).unwrap().count() == 0;
    assert!(empty, "no worktree remains");
    assert!(
        git(
            setup.deadline,
            &setup.workspace(),
            &["branch", "--list", "fiber/*"]
        )
        .is_empty(),
        "no fiber/ branch remains"
    );
}

#[test]
fn a_startup_failure_after_creating_the_worktree_removes_it() {
    let setup = Setup::new();
    init_repo(setup.deadline, &setup.workspace());

    // No provider installed: the model cannot resolve, after the
    // worktree was created.
    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree` with no model to exit",
        ask_command(
            &setup,
            &setup.workspace(),
            &[],
            &["ask", "--worktree", "hi"],
        ),
    );
    assert_eq!(out.status.code(), Some(1));
    let lines = lines(&out);
    let last = lines.last().expect("a fiber_exited line");
    assert_eq!(last["kind"], "fiber_exited");
    assert_eq!(last["payload"]["error"]["code"], "no_model");
    let key = key_of(setup.deadline, &setup.workspace());
    let worktrees = setup.home().join("projects").join(&key).join("worktrees");
    let empty = !worktrees.exists() || fs::read_dir(&worktrees).unwrap().count() == 0;
    assert!(empty, "the clean worktree was removed");
    assert!(
        git(
            setup.deadline,
            &setup.workspace(),
            &["branch", "--list", "fiber/*"]
        )
        .is_empty(),
        "no fiber/ branch remains"
    );
}

/// A finished first `ask --worktree` run: what its `session_started`
/// recorded.
struct First {
    id: String,
    path: PathBuf,
    branch: String,
}

/// Runs `ask --worktree hi` to exit 0 and returns what its
/// `session_started` recorded.
#[track_caller]
fn first_run(setup: &Setup) -> First {
    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree` to exit",
        ask_command(setup, &setup.workspace(), &[], &["ask", "--worktree", "hi"]),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let found = lines(&out);
    let first = started(&found);
    First {
        id: first["session_id"].as_str().unwrap().to_owned(),
        path: PathBuf::from(first["payload"]["workspace"].as_str().unwrap()),
        branch: first["payload"]["worktree"]["branch"]
            .as_str()
            .unwrap()
            .to_owned(),
    }
}

/// Commits one more file on the repository and returns its new HEAD.
#[track_caller]
fn commit_again(deadline: Deadline, repo: &Path, name: &str) -> String {
    fs::write(repo.join(name), "y").unwrap();
    git(deadline, repo, &["add", "."]);
    git(deadline, repo, &["commit", "--quiet", "-m", name]);
    git(deadline, repo, &["rev-parse", "HEAD"])
}

/// The session's log file, its bytes, and its first line's bytes.
#[track_caller]
fn session_log(setup: &Setup, id: &str) -> (PathBuf, Vec<u8>, Vec<u8>) {
    let events = fs::canonicalize(setup.home())
        .unwrap()
        .join("projects")
        .join(key_of(setup.deadline, &setup.workspace()))
        .join("sessions")
        .join(id)
        .join("events.jsonl");
    let bytes = fs::read(&events).unwrap();
    let first = bytes.split(|byte| *byte == b'\n').next().unwrap().to_vec();
    (events, bytes, first)
}

/// The lock file beside the recorded worktree path.
fn lock_of(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    path.with_file_name(format!("{name}.lock"))
}

/// The worktree's lock file is held: an exclusive take waits.
#[track_caller]
fn lock_held(path: &Path) {
    let lock = lock_of(path);
    assert!(lock.is_file(), "the lock file exists");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock)
        .unwrap();
    assert!(
        matches!(file.try_lock(), Err(TryLockError::WouldBlock)),
        "the process holds its share"
    );
}

/// Spawns `ask --resume`, waits for `requests` provider requests in
/// total, checks the resumed process holds the worktree's lock, then
/// releases the provider and waits for exit.
#[track_caller]
fn resume_held(
    setup: &Setup,
    server: &ProviderServer,
    first: &First,
    prompt: &str,
    requests: usize,
) -> Output {
    server.hold();
    let child = ask_command(
        setup,
        &setup.workspace(),
        &[],
        &["ask", "--resume", &first.id, prompt],
    )
    .spawn()
    .unwrap();
    assert!(
        server.await_requests(requests, setup.deadline),
        "the resume reached the provider"
    );
    lock_held(&first.path);
    server.release();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    match setup.deadline.recv(&finished) {
        Ok(out) => out.unwrap(),
        Err(_) => panic!("waited for `fiber ask --resume` to exit"),
    }
}

/// An `openai-responses` stream that calls `write` on the relative path.
fn write_call(path: &str, content: &str) -> Response {
    stream(&[function_call(
        "w1",
        "write",
        &json!({"path": path, "content": content}),
    )])
}

#[test]
fn ask_resume_recreates_a_removed_worktree_at_the_new_head() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), write_call("note.txt", "hi\n"), hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());

    // Text only, so the worktree and its branch are gone after exit.
    let first = first_run(&setup);
    assert!(!first.path.exists());
    assert!(!branch_exists(
        setup.deadline,
        &setup.workspace(),
        &first.branch
    ));
    let (_, _, first_line) = session_log(&setup, &first.id);

    let head = commit_again(setup.deadline, &setup.workspace(), "second.txt");

    // The resume writes, so it is kept: while the provider holds the
    // request, the recreating process holds the lock.
    let out = resume_held(&setup, &server, &first, "write it", 2);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let resumed = lines(&out);
    assert_eq!(resumed[0]["kind"], "fiber_started");
    assert_eq!(resumed[0]["payload"]["resumed"], true);
    assert_eq!(
        kinds(&resumed),
        [
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    assert_eq!(
        fs::read_to_string(first.path.join("note.txt")).unwrap(),
        "hi\n"
    );
    assert_eq!(
        git(setup.deadline, &first.path, &["rev-parse", "HEAD"]),
        head
    );
    assert_eq!(
        git(
            setup.deadline,
            &first.path,
            &["rev-parse", "--abbrev-ref", "HEAD"]
        ),
        first.branch
    );
    assert!(first.path.exists(), "the dirty worktree is kept");
    assert!(branch_exists(
        setup.deadline,
        &setup.workspace(),
        &first.branch
    ));
    assert_eq!(session_log(&setup, &first.id).2, first_line);
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn a_clean_recreated_worktree_is_removed_again() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());

    let first = first_run(&setup);
    assert!(!first.path.exists());
    commit_again(setup.deadline, &setup.workspace(), "second.txt");

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --resume` of a removed worktree to exit",
        ask_command(
            &setup,
            &setup.workspace(),
            &[],
            &["ask", "--resume", &first.id, "hi"],
        ),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let resumed = lines(&out);
    assert_eq!(resumed[0]["kind"], "fiber_started");
    assert_eq!(resumed[0]["payload"]["resumed"], true);
    assert_eq!(
        kinds(&resumed),
        [
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    // Removed against the new base: the path, the branch and the lock
    // file are gone again, which a strong log hold would have kept.
    assert!(!first.path.exists());
    assert!(!branch_exists(
        setup.deadline,
        &setup.workspace(),
        &first.branch
    ));
    assert!(!lock_of(&first.path).exists());
}

#[test]
fn ask_resume_of_a_kept_worktree_neither_recreates_nor_removes() {
    let setup = Setup::new();
    let server = ProviderServer::start([write_call("note.txt", "hi\n"), hello(), hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());

    // The reply writes, so the worktree is kept.
    let first = first_run(&setup);
    assert!(first.path.join("note.txt").is_file());
    let base = git(setup.deadline, &first.path, &["rev-parse", "HEAD"]);
    // Back to clean, so only the new HEAD could move it.
    fs::remove_file(first.path.join("note.txt")).unwrap();
    let head = commit_again(setup.deadline, &setup.workspace(), "second.txt");
    assert_ne!(base, head);

    let out = resume_held(&setup, &server, &first, "hi", 3);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let resumed = lines(&out);
    assert_eq!(resumed[0]["kind"], "fiber_started");
    assert_eq!(resumed[0]["payload"]["resumed"], true);
    assert_eq!(
        kinds(&resumed),
        [
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    // Never recreated and never removed: the path and branch stand at
    // the original base, not the new HEAD.
    assert!(first.path.exists());
    assert!(branch_exists(
        setup.deadline,
        &setup.workspace(),
        &first.branch
    ));
    assert_eq!(
        git(setup.deadline, &first.path, &["rev-parse", "HEAD"]),
        base
    );
}

#[test]
fn ask_resume_without_a_recorded_repository_is_session_not_found() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());

    let first = first_run(&setup);
    assert!(!first.path.exists());
    // The recorded worktree without its repository: the lookup falls
    // back to the removed workspace, which matches no project.
    let (events, before, _) = session_log(&setup, &first.id);
    let mut rewritten: Value =
        serde_json::from_slice(before.split(|byte| *byte == b'\n').next().unwrap()).unwrap();
    rewritten["payload"]["worktree"]
        .as_object_mut()
        .unwrap()
        .remove("repository");
    let mut changed = serde_json::to_string(&rewritten).unwrap().into_bytes();
    changed.extend_from_slice(&before[before.iter().position(|byte| *byte == b'\n').unwrap()..]);
    fs::write(&events, &changed).unwrap();
    let copied = fs::read(&events).unwrap();

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --resume` with no recorded repository to exit",
        ask_command(
            &setup,
            &setup.workspace(),
            &[],
            &["ask", "--resume", &first.id, "x"],
        ),
    );
    assert_eq!(out.status.code(), Some(1));
    let refused = lines(&out);
    assert_eq!(kinds(&refused), ["fiber_exited"]);
    let last = refused.last().unwrap();
    assert_eq!(last.get("session_id"), None);
    assert_eq!(last["payload"]["error"]["code"], "session_not_found");
    assert_eq!(fs::read(&events).unwrap(), copied);
    assert!(!first.path.exists());
    assert!(!lock_of(&first.path).exists());
}

#[test]
fn ask_resume_with_a_branch_left_behind_is_io_failed() {
    let setup = Setup::new();
    let server = ProviderServer::start([write_call("note.txt", "hi\n"), hello(), hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());

    let first = first_run(&setup);
    assert!(first.path.join("note.txt").is_file());
    // A commit beyond the base, then the directory deleted by hand: the
    // branch is left behind.
    git(setup.deadline, &first.path, &["add", "."]);
    git(
        setup.deadline,
        &first.path,
        &["commit", "--quiet", "-m", "more"],
    );
    let sha = git(
        setup.deadline,
        &setup.workspace(),
        &["rev-parse", &first.branch],
    );
    fs::remove_dir_all(&first.path).unwrap();
    let (_, before, _) = session_log(&setup, &first.id);

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --resume` with a branch left behind to exit",
        ask_command(
            &setup,
            &setup.workspace(),
            &[],
            &["ask", "--resume", &first.id, "hi"],
        ),
    );
    assert_eq!(out.status.code(), Some(1));
    let refused = lines(&out);
    assert_eq!(kinds(&refused), ["fiber_exited"]);
    assert_eq!(refused[0]["payload"]["error"]["code"], "io_failed");
    assert_eq!(session_log(&setup, &first.id).1, before);
    assert_eq!(
        git(
            setup.deadline,
            &setup.workspace(),
            &["rev-parse", &first.branch]
        ),
        sha,
        "the branch stays at its commit"
    );
    assert!(!first.path.exists());
}

#[test]
fn a_failure_after_recreation_is_reported_after_the_end() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());

    let first = first_run(&setup);
    assert!(!first.path.exists());
    commit_again(setup.deadline, &setup.workspace(), "second.txt");
    let (_, before, _) = session_log(&setup, &first.id);

    // No credential: the failure lands after the worktree was made.
    let mut resume = ask_command(
        &setup,
        &setup.workspace(),
        &[],
        &["ask", "--resume", &first.id, "hi"],
    );
    resume.env_remove("FIBER_TEST_FAKE_KEY");
    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --resume` with no credential to exit",
        resume,
    );
    assert_eq!(out.status.code(), Some(1));
    let refused = lines(&out);
    assert_eq!(kinds(&refused), ["fiber_exited"]);
    assert_eq!(refused[0]["payload"]["error"]["code"], "credential_missing");
    assert_eq!(session_log(&setup, &first.id).1, before);
    // Clean, so the end removed it before the failure was reported.
    assert!(!first.path.exists());
    assert!(!branch_exists(
        setup.deadline,
        &setup.workspace(),
        &first.branch
    ));
}

#[test]
fn a_signal_during_the_recreating_add_leaves_nothing() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());

    let first = first_run(&setup);
    assert!(!first.path.exists());
    let (_, before, _) = session_log(&setup, &first.id);
    // The hook holds the recreating add open, as the creation test's does.
    let hooks = Path::new(env!("CARGO_MANIFEST_DIR")).join("../worktree/fixtures/holding-hook");
    git(
        setup.deadline,
        &setup.workspace(),
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    let fifo = setup.root.path().join("hook.fifo");
    fakes::fifo(&fifo);
    let child = ask_command(
        &setup,
        &setup.workspace(),
        &[("FIBER_TEST_HOOK_FIFO", fifo.to_str().unwrap())],
        &["ask", "--resume", &first.id, "hi"],
    )
    .spawn()
    .unwrap();
    let fiber_pid = child.id();
    let watchdog = Watchdog::group(fiber_pid);
    // The hook writes its pid and process group once it runs: only then
    // is the signal's target the hook's group.
    let hook = support::bounded(setup.deadline, "the hook's pid and group", move || {
        fs::read_to_string(&fifo)
    })
    .unwrap();
    let mut words = hook.split_whitespace();
    let hook_pid: u32 = words.next().unwrap().parse().unwrap();
    let hook_group: u32 = words.next().unwrap().parse().unwrap();
    assert!(hook_pid > 1 && hook_group > 1);
    support::kill_pid(setup.deadline, fiber_pid, "TERM").unwrap();
    let (done, finished) = std::sync::mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let out = match setup.deadline.recv(&finished) {
        Ok(out) => out.unwrap(),
        Err(_) => panic!("waited for `fiber ask --resume` to exit on SIGTERM"),
    };
    assert_eq!(out.status.code(), Some(143));
    assert!(
        out.stdout.is_empty(),
        "neither the hook nor a line may reach stdout"
    );
    assert!(
        out.stderr.is_empty(),
        "a recorded signal writes nothing to stderr"
    );
    assert!(
        fakes::group_empties(hook_group, setup.deadline),
        "the hook's group is gone"
    );
    assert!(!first.path.exists());
    assert!(!branch_exists(
        setup.deadline,
        &setup.workspace(),
        &first.branch
    ));
    assert_eq!(session_log(&setup, &first.id).1, before);
    watchdog.stand_down(setup.deadline.cleanup_phase());
    assert!(!group_alive(setup.deadline, fiber_pid));
}

#[test]
fn ask_resume_of_a_worktree_replaced_by_another_repository_is_session_not_found() {
    let setup = Setup::new();
    let server = ProviderServer::start([write_call("note.txt", "hi\n"), hello(), hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());

    let first = first_run(&setup);
    assert!(first.path.join("note.txt").is_file());
    // The first run's end keeps its lock file beside the kept worktree:
    // remove it, so what follows pins only what the refused resume
    // creates, which is nothing.
    fs::remove_file(lock_of(&first.path)).unwrap();
    // The kept worktree replaced by another repository: present, but in
    // no project the launch knows.
    fs::remove_dir_all(&first.path).unwrap();
    fs::create_dir(&first.path).unwrap();
    init_repo(setup.deadline, &first.path);
    let (_, before, _) = session_log(&setup, &first.id);

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --resume` of a replaced worktree to exit",
        ask_command(
            &setup,
            &setup.workspace(),
            &[],
            &["ask", "--resume", &first.id, "x"],
        ),
    );
    assert_eq!(out.status.code(), Some(1));
    let refused = lines(&out);
    assert_eq!(kinds(&refused), ["fiber_exited"]);
    let last = refused.last().unwrap();
    assert_eq!(last.get("session_id"), None);
    assert_eq!(last["payload"]["error"]["code"], "session_not_found");
    assert_eq!(session_log(&setup, &first.id).1, before);
    // The other repository is untouched, and no lock was taken.
    assert_eq!(
        fs::read_to_string(first.path.join("file.txt")).unwrap(),
        "x"
    );
    assert!(
        git(
            setup.deadline,
            &first.path,
            &["branch", "--list", "fiber/*"]
        )
        .is_empty()
    );
    assert!(!lock_of(&first.path).exists());
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn ask_resume_of_a_kept_worktree_runs_in_it() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());
    // A checked-in hook that leaves an untracked file behind, so the
    // worktree is kept.
    let hooks = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/keeping-hook");
    git(
        setup.deadline,
        &setup.workspace(),
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );

    let first = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree` to exit",
        ask_command(
            &setup,
            &setup.workspace(),
            &[],
            &["ask", "--worktree", "hi"],
        ),
    );
    assert_eq!(
        first.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_lines = lines(&first);
    let id = started(&first_lines)["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let workspace = started(&first_lines)["payload"]["workspace"]
        .as_str()
        .unwrap()
        .to_owned();
    let branch = started(&first_lines)["payload"]["worktree"]["branch"]
        .as_str()
        .unwrap()
        .to_owned();
    // Kept: the untracked file holds the worktree and its branch.
    assert!(Path::new(&workspace).join("keep.txt").is_file());

    let second = run_to_exit(
        setup.deadline,
        "`fiber ask --resume` of a kept worktree to exit",
        ask_command(
            &setup,
            &setup.workspace(),
            &[],
            &["ask", "--resume", &id, "x"],
        ),
    );
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_lines = lines(&second);
    assert_eq!(second_lines[0]["kind"], "fiber_started");
    assert_eq!(second_lines[0]["payload"]["resumed"], true);
    assert_eq!(
        kinds(&second_lines),
        [
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    assert!(Path::new(&workspace).exists());
    assert!(branch_exists(setup.deadline, &setup.workspace(), &branch));
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn a_missing_key_in_a_worktree_carries_the_doctor_line_first() {
    let setup = Setup::new();
    init_repo(setup.deadline, &setup.workspace());
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let mut ask = ask_command(
        &setup,
        &setup.workspace(),
        &[],
        &["ask", "--worktree", "hi"],
    );
    ask.env_remove("FIBER_TEST_FAKE_KEY");
    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree` with no key to exit",
        ask,
    );
    assert_eq!(out.status.code(), Some(1));
    let events = lines(&out);
    let last = events.last().expect("a fiber_exited line");
    assert_eq!(last["payload"]["error"]["code"], "credential_missing");
    let message = last["payload"]["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("no key for fake: run `fiber login fake`\n"),
        "{message}"
    );
}
