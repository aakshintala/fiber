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

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};
use support::{Deadline, Setup, group_alive, run_to_exit};

/// Runs the system `git` in `dir`, in its own process group, to its exit
/// under the test's [`Deadline`].
fn git(deadline: Deadline, dir: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = run_to_exit(deadline, &format!("git {args:?}"), command);
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A repository with one commit on `main` at `dir`.
fn init_repo(deadline: Deadline, dir: &Path) {
    git(deadline, dir, &["init", "--quiet"]);
    fs::write(dir.join("file.txt"), "x").unwrap();
    git(deadline, dir, &["add", "."]);
    git(deadline, dir, &["commit", "--quiet", "-m", "first"]);
}

/// An `openai-responses` stream answering `Hello.` in two fragments.
fn hello() -> Response {
    stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

fn stream(events: &[Value]) -> Response {
    let mut body = String::new();
    for event in events {
        body.push_str(&format!(
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
        ));
    }
    let done = json!({"type": "response.completed", "response": {
        "id": "resp_1", "status": "completed",
        "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
    }});
    body.push_str(&format!("event: response.completed\ndata: {done}\n\n"));
    Response::stream(body)
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
    let home = fs::canonicalize(setup.home()).unwrap();
    let expected = home
        .join("projects")
        .join(key_of(setup.deadline, &setup.workspace()))
        .join("worktrees")
        .join(id);
    assert_eq!(workspace, expected.to_str().unwrap());
    assert_eq!(branch, format!("fiber/{id}"));
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
    let mut mkfifo = Command::new("mkfifo");
    mkfifo
        .arg(&fifo)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let made = run_to_exit(setup.deadline, "mkfifo", mkfifo);
    assert!(made.status.success(), "mkfifo: {made:?}");
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
    let out = match finished.recv_timeout(setup.deadline.left()) {
        Ok(out) => out.unwrap(),
        Err(_) => panic!("waited for `fiber ask --worktree` to exit on SIGTERM"),
    };
    assert_eq!(out.status.code(), Some(143));
    assert!(
        out.stdout.is_empty(),
        "neither the hook nor a line may reach stdout"
    );
    assert!(out.stderr.is_empty(), "a recorded signal writes nothing");
    assert!(
        fakes::group_empties(hook_group, setup.deadline.left()),
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
    watchdog.stand_down(setup.deadline.cleanup());
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
