//! Worktree create and remove timers at the binary level (`docs/state.md`,
//! "Diagnostic logs"): the built `fiber` runs `ask --worktree` against
//! the fake provider server. At `debug` a clean run writes one
//! `worktree.create` in the pid file and one `worktree.remove` in the
//! session file, each with only `op` and `ms`; at `info` nothing is
//! written. Every wait is the test's one deadline, and `ms` is
//! normalized, never asserted.

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
use std::process::{Command, Stdio};

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{Deadline, Setup, function_call, git, is_status, run_to_exit, stream, write_json};

/// An `openai-responses` stream that calls `write` on the relative path.
fn write_call(path: &str, content: &str) -> fakes::Response {
    stream(&[function_call(
        "w1",
        "write",
        &json!({"path": path, "content": content}),
    )])
}

fn ask_command(setup: &Setup, cwd: &Path, args: &[&str]) -> Command {
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
    command
}

fn init_repo(deadline: Deadline, dir: &Path) {
    git(deadline, dir, &["init", "--quiet"]);
    fs::write(dir.join("file.txt"), "x").unwrap();
    git(deadline, dir, &["add", "."]);
    git(deadline, dir, &["commit", "--quiet", "-m", "first"]);
}

fn lines(out: &std::process::Output) -> Vec<Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !is_status(l))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn started(lines: &[Value]) -> &Value {
    lines
        .iter()
        .find(|line| line["kind"] == "session_started")
        .expect("session_started is on stdout")
}

fn debug_config(setup: &Setup) {
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "diagnostics": {"level": "debug"}}),
    );
}

/// Every diagnostic line under the home's `logs/`, with its file name.
fn log_lines(setup: &Setup) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    let logs = setup.home().join("logs");
    if !logs.exists() {
        return found;
    }
    let mut entries: Vec<_> = fs::read_dir(&logs).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let text = fs::read_to_string(entry.path()).unwrap();
        for line in text.lines() {
            found.push((name.clone(), serde_json::from_str(line).unwrap()));
        }
    }
    found
}

fn op_lines(setup: &Setup, op: &str) -> Vec<(String, Value)> {
    log_lines(setup)
        .into_iter()
        .filter(|(_, line)| line["code"] == "op_timing" && line["data"]["op"] == op)
        .collect()
}

fn all_op_lines(setup: &Setup) -> Vec<(String, Value)> {
    log_lines(setup)
        .into_iter()
        .filter(|(_, line)| line["code"] == "op_timing")
        .collect()
}

/// The wire promise on every worktree line: `debug`, exactly the keys
/// `op` and `ms` with an unsigned `ms`, and no content, path or
/// configuration value in its `data`.
fn assert_worktree_wire(line: &Value, op: &str) {
    assert_eq!(line["code"], "op_timing", "{line}");
    assert_eq!(line["level"], "debug", "{line}");
    assert_eq!(line["data"]["op"], op, "{line}");
    assert_eq!(line["message"], "An operation ended.", "{line}");
    let mut keys: Vec<&str> = line["data"]
        .as_object()
        .expect("data is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["ms", "op"], "{line}");
    assert!(line["data"]["ms"].as_u64().is_some(), "{line}");
}

fn commit_again(deadline: Deadline, repo: &Path, name: &str) -> String {
    fs::write(repo.join(name), "y").unwrap();
    git(deadline, repo, &["add", "."]);
    git(deadline, repo, &["commit", "--quiet", "-m", name]);
    git(deadline, repo, &["rev-parse", "HEAD"])
}

struct First {
    id: String,
    path: PathBuf,
}

#[track_caller]
fn first_run(setup: &Setup) -> First {
    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree` to exit",
        ask_command(setup, &setup.workspace(), &["ask", "--worktree", "hi"]),
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
    }
}

fn clear_logs(setup: &Setup) {
    let logs = setup.home().join("logs");
    if logs.exists() {
        fs::remove_dir_all(&logs).unwrap();
    }
}

#[test]
fn a_debug_worktree_run_writes_one_create_in_the_pid_file() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    debug_config(&setup);
    init_repo(setup.deadline, &setup.workspace());

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree` to exit",
        ask_command(&setup, &setup.workspace(), &["ask", "--worktree", "hi"]),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        server.await_requests(1, setup.deadline),
        "the model request arrived: a passing test really asked"
    );
    let found = lines(&out);
    let id = started(&found)["session_id"].as_str().unwrap().to_owned();

    let creates = op_lines(&setup, "worktree.create");
    assert_eq!(creates.len(), 1, "{creates:?}");
    let (file, line) = &creates[0];
    assert_worktree_wire(line, "worktree.create");
    // Before the session attaches, so no `session_id` and never the
    // session file.
    assert_eq!(line.get("session_id"), None, "{line}");
    assert_eq!(line["process"], "ask", "{line}");
    assert_ne!(file, &format!("ask-{id}.log"), "never the session file");
    assert!(file.starts_with("ask-"), "{file}");
    assert!(file.ends_with(".log"), "{file}");
}

#[test]
fn an_info_worktree_run_writes_no_timing() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree` to exit",
        ask_command(&setup, &setup.workspace(), &["ask", "--worktree", "hi"]),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        server.await_requests(1, setup.deadline),
        "the model request arrived: a passing test really asked"
    );
    assert!(all_op_lines(&setup).is_empty(), "no timing at info");
}

#[test]
fn a_debug_worktree_run_outside_a_repository_writes_no_worktree_line() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    debug_config(&setup);

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree` outside a repository to exit",
        ask_command(&setup, &setup.workspace(), &["ask", "--worktree", "hi"]),
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(op_lines(&setup, "worktree.create").is_empty());
    assert!(op_lines(&setup, "worktree.remove").is_empty());
}

#[test]
// debt: this clean-setup repeats the test at :205. Upgrade trigger: fold the two into one helper when a third clean-setup test is added.
fn a_debug_worktree_run_writes_one_remove_in_the_session_file() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    debug_config(&setup);
    init_repo(setup.deadline, &setup.workspace());

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree` to exit",
        ask_command(&setup, &setup.workspace(), &["ask", "--worktree", "hi"]),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let found = lines(&out);
    let id = started(&found)["session_id"].as_str().unwrap().to_owned();

    let creates = op_lines(&setup, "worktree.create");
    let removes = op_lines(&setup, "worktree.remove");
    assert_eq!(creates.len(), 1, "{creates:?}");
    assert_eq!(removes.len(), 1, "{removes:?}");
    let (_, create) = &creates[0];
    let (remove_file, remove) = &removes[0];
    assert_worktree_wire(remove, "worktree.remove");
    // After the session attaches, so the session file and id.
    assert_eq!(remove_file, &format!("ask-{id}.log"), "{remove_file}");
    assert_eq!(remove["session_id"], id.as_str(), "{remove}");
    // The two lines are in different files; only the `ts` order is
    // pinned, never the files' relative timing beyond it.
    let create_ts = create["ts"].as_u64().unwrap();
    let remove_ts = remove["ts"].as_u64().unwrap();
    assert!(
        remove_ts >= create_ts,
        "remove at {remove_ts} after create at {create_ts}"
    );
}

#[test]
fn a_dirty_debug_worktree_run_writes_no_remove_line() {
    let setup = Setup::new();
    let server = ProviderServer::start([write_call("note.txt", "hi\n"), support::hello()]).unwrap();
    setup.provider(&server);
    debug_config(&setup);
    init_repo(setup.deadline, &setup.workspace());

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --worktree` to exit",
        ask_command(&setup, &setup.workspace(), &["ask", "--worktree", "hi"]),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let found = lines(&out);
    let first = started(&found);
    let workspace = first["payload"]["workspace"].as_str().unwrap();
    assert!(
        Path::new(workspace).join("note.txt").is_file(),
        "the worktree is kept dirty"
    );
    assert_eq!(op_lines(&setup, "worktree.create").len(), 1);
    assert!(
        op_lines(&setup, "worktree.remove").is_empty(),
        "a kept worktree writes no remove"
    );
}

#[test]
fn a_debug_resume_recreates_and_removes() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello(), support::hello()]).unwrap();
    setup.provider(&server);
    debug_config(&setup);
    init_repo(setup.deadline, &setup.workspace());

    let first = first_run(&setup);
    assert!(!first.path.exists(), "the clean worktree is gone");
    // Only the resume's lines are pinned below.
    clear_logs(&setup);
    commit_again(setup.deadline, &setup.workspace(), "second.txt");

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --resume` of a removed worktree to exit",
        ask_command(
            &setup,
            &setup.workspace(),
            &["ask", "--resume", &first.id, "hi"],
        ),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let creates = op_lines(&setup, "worktree.create");
    let removes = op_lines(&setup, "worktree.remove");
    assert_eq!(creates.len(), 1, "{creates:?}");
    assert_eq!(removes.len(), 1, "{removes:?}");
    let (create_file, create) = &creates[0];
    let (remove_file, remove) = &removes[0];
    assert_worktree_wire(create, "worktree.create");
    assert_worktree_wire(remove, "worktree.remove");
    assert_eq!(create.get("session_id"), None, "{create}");
    assert_ne!(create_file, &format!("ask-{}.log", first.id));
    assert_eq!(
        remove_file,
        &format!("ask-{}.log", first.id),
        "{remove_file}"
    );
    assert_eq!(remove["session_id"], first.id.as_str(), "{remove}");
    assert!(
        !first.path.exists(),
        "the clean recreated worktree is gone again"
    );
}

#[test]
fn a_debug_resume_of_a_kept_present_worktree_writes_neither() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        write_call("note.txt", "hi\n"),
        support::hello(),
        support::hello(),
    ])
    .unwrap();
    setup.provider(&server);
    debug_config(&setup);
    init_repo(setup.deadline, &setup.workspace());

    let first = first_run(&setup);
    assert!(
        first.path.join("note.txt").is_file(),
        "the worktree is kept"
    );
    // Back to clean, so only the new HEAD could move it; the path stays
    // present, so the resume shares it instead of remaking it.
    fs::remove_file(first.path.join("note.txt")).unwrap();
    commit_again(setup.deadline, &setup.workspace(), "second.txt");
    clear_logs(&setup);

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --resume` of a kept worktree to exit",
        ask_command(
            &setup,
            &setup.workspace(),
            &["ask", "--resume", &first.id, "hi"],
        ),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        op_lines(&setup, "worktree.create").is_empty(),
        "a present worktree is never made"
    );
    assert!(
        op_lines(&setup, "worktree.remove").is_empty(),
        "a shared worktree never ends"
    );
    assert!(first.path.exists(), "the worktree stands");
}

#[test]
fn an_info_resume_writes_neither() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello(), support::hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());

    let first = first_run(&setup);
    assert!(!first.path.exists());
    commit_again(setup.deadline, &setup.workspace(), "second.txt");

    let out = run_to_exit(
        setup.deadline,
        "`fiber ask --resume` to exit",
        ask_command(
            &setup,
            &setup.workspace(),
            &["ask", "--resume", &first.id, "hi"],
        ),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all_op_lines(&setup).is_empty(), "no timing at info");
}
