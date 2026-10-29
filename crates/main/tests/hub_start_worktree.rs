//! Binary-level test that a hub `start` with `worktree: true` runs the
//! session in a new worktree (`docs/invocation.md`, "What the hub speaks"
//! and "Isolation"): the built `fiber` runs `hub serve` in its own process
//! group with its own `FIBER_HOME`, the session's `session_started` records
//! the worktree, and closing the clean session removes it. A hub resume of
//! a session whose worktree was removed makes it again from the recorded
//! repository before resuming, and ends it by the same rules.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::{
    Deadline, HubProc, SessionGuard, Setup, Socket, assert_one_continued_log, complete_lines,
    connect_hub, function_call, git, recv_reply, run_to_exit, session_dir, stream, subscribe,
    until,
};

/// The verbatim count rule the multiset calls cite
/// (`docs/testing.md`, "Event streams").
const COUNT_DOC: &str = "docs/testing.md:95: - the event kinds as a set with counts, so a duplicated or missing event fails";

/// Verbatim lifecycle sources for the session-boundary assertions below.
const SESSION_FIRST_DOC: &str =
    "docs/events.md:339: Durable. The first line of every session's log.";
const PROCESS_START_DOC: &str = "docs/events.md:285-286: Durable. The first line a process writes when it resumes a session; in a new session it follows `session_started`, which is always the log's first line.";
const PROCESS_END_DOC: &str =
    "docs/events.md:296: Durable. The last line a process writes for a session.";

/// A repository with one commit on `main` at `dir`.
fn init_repo(deadline: Deadline, dir: &Path) {
    git(deadline, dir, &["init", "--quiet"]);
    std::fs::write(dir.join("file.txt"), "x").unwrap();
    git(deadline, dir, &["add", "."]);
    git(deadline, dir, &["commit", "--quiet", "-m", "first"]);
}

fn workspace_text(setup: &Setup) -> String {
    setup.workspace().to_string_lossy().into_owned()
}

#[test]
fn hub_start_with_worktree_runs_in_a_new_worktree_and_removes_it_clean() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());
    let workspace = workspace_text(&setup);

    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    client.send(
        &json!({
            "id": "c_start",
            "command": "start",
            "args": {"workspace": workspace, "worktree": true},
        })
        .to_string(),
    );
    let started = recv_reply(&client, "the start acknowledgement");
    assert_eq!(started["kind"], "command_accepted", "{started}");
    let id = started["payload"]["result"]["session_id"]
        .as_str()
        .expect("the start answers with a session id")
        .to_owned();

    subscribe(&client, &id);
    // The preamble and opening message are read once at the first turn
    // (`crates/loop/src/opening.rs`), so a session started with no
    // content parks after `clients`: closing then races nothing the loop
    // writes, and the stream below is the whole of it.
    let mut stream = until(&client, "session_started", |line| {
        line["kind"] == "session_started"
    });
    let first = stream
        .iter()
        .find(|line| line["kind"] == "session_started")
        .expect("session_started is replayed");
    let worktree = &first["payload"]["worktree"];
    assert_eq!(worktree["branch"].as_str().unwrap(), format!("fiber/{id}"));
    let path = worktree["path"].as_str().unwrap().to_owned();
    assert_eq!(first["payload"]["workspace"].as_str().unwrap(), path);
    assert_ne!(path, workspace);
    assert!(Path::new(&path).exists(), "the worktree exists");

    client.send(&json!({"id": "c_close", "session_id": id, "command": "close"}).to_string());
    let tail = until(&client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    stream.extend(tail);
    // The stream, in order, with the one `clients` line set aside: the hub
    // writes it when the `full` subscribe attaches, which races the
    // session's own `fiber_started` and `extensions_loaded`.
    let clients = stream
        .iter()
        .filter(|line| line["kind"] == "clients")
        .count();
    assert_eq!(clients, 1, "{stream:?}");
    // The stream as a set, with the one `clients` line counted: the hub
    // writes it when the `full` subscribe attaches, which races the
    // session's own `fiber_started` and `extensions_loaded`.
    let mut expected = vec!["clients"];
    expected.extend([
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "command_accepted",
        "fiber_exited",
    ]);
    support::assert_kinds_multiset(&stream, &expected, &["clients"], None, COUNT_DOC);
    // The hub capture above is complete through the exit. The session ran
    // in a new worktree, so its durable log lives under that project: walk
    // for it as the hub timer test does.
    let continued: Vec<Value> = fs::read_dir(setup.home().join("projects"))
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .join("sessions")
                .join(&id)
                .join("events.jsonl")
        })
        .find(|log| log.is_file())
        .map(|log| support::complete_lines(&fs::read_to_string(log).unwrap()))
        .expect("the session's log exists");
    support::assert_session_process_boundaries(
        &continued,
        &id,
        SESSION_FIRST_DOC,
        PROCESS_START_DOC,
        PROCESS_END_DOC,
    );
    guard.wait_gone();
    // Clean, so both are gone after exit.
    assert!(!Path::new(&path).exists());
    assert!(
        git(
            setup.deadline,
            &setup.workspace(),
            &["branch", "--list", &format!("fiber/{id}")]
        )
        .is_empty(),
        "no fiber/ branch remains"
    );
    drop(client);
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
}

/// What the first `ask --worktree` run recorded on its `session_started`.
struct First {
    id: String,
    path: PathBuf,
    branch: String,
    repository: String,
}

/// Runs `ask --worktree hi` from the workspace to exit 0 and returns what
/// its `session_started` recorded.
#[track_caller]
fn first_run(setup: &Setup) -> First {
    let mut command = setup.fiber(&["ask", "--worktree", "hi"]);
    command.current_dir(setup.workspace());
    let out = run_to_exit(setup.deadline, "`fiber ask --worktree` to exit", command);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let first = lines
        .iter()
        .find(|line| line["kind"] == "session_started")
        .expect("session_started is on stdout");
    First {
        id: first["session_id"].as_str().unwrap().to_owned(),
        path: PathBuf::from(first["payload"]["workspace"].as_str().unwrap()),
        branch: first["payload"]["worktree"]["branch"]
            .as_str()
            .unwrap()
            .to_owned(),
        repository: first["payload"]["worktree"]["repository"]
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

/// An `openai-responses` stream that calls `write` on the relative path.
fn write_call(path: &str, content: &str) -> Response {
    stream(&[function_call(
        "w1",
        "write",
        &json!({"path": path, "content": content}),
    )])
}

/// Sends a `prompt` for `session` through the hub.
fn send_prompt(client: &Socket, id: &str, session: &str, text: &str) {
    client.send(
        &json!({"id": id, "session_id": session, "command": "prompt",
            "args": {"content": [{"type": "text", "text": text}]}})
        .to_string(),
    );
}

/// Sends a `close` for `session` through the hub.
fn send_close(client: &Socket, session: &str) {
    client.send(&json!({"id": "c_close", "session_id": session, "command": "close"}).to_string());
}

/// Collects hub lines until the resumed `fiber_started`, skipping the
/// first run's replayed lines: only the resumed `fiber_started` on is
/// returned.
#[track_caller]
fn until_resumed(client: &Socket, what: &str) -> Vec<Value> {
    let lines = until(client, what, |line| {
        line["kind"] == "fiber_started" && line["payload"]["resumed"] == true
    });
    let resumed = lines
        .iter()
        .rposition(|line| line["kind"] == "fiber_started" && line["payload"]["resumed"] == true)
        .expect("the resumed fiber_started is collected");
    lines[resumed..].to_vec()
}

/// The session log's lines and its first line's bytes.
fn session_log(setup: &Setup, id: &str) -> (Vec<Value>, Vec<u8>) {
    let events = session_dir(setup, id).join("events.jsonl");
    let bytes = fs::read(&events).unwrap();
    let lines = complete_lines(&String::from_utf8_lossy(&bytes));
    let first = bytes.split(|byte| *byte == b'\n').next().unwrap().to_vec();
    (lines, first)
}

/// The lock file beside the recorded worktree path.
fn lock_of(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    path.with_file_name(format!("{name}.lock"))
}

#[test]
fn a_hub_resume_makes_a_removed_worktree_again_and_keeps_it_when_dirty() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        support::hello(),
        write_call("note.txt", "hi\n"),
        support::hello(),
    ])
    .unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());
    let workspace = workspace_text(&setup);
    let _ask_guard = SessionGuard::arm(setup.deadline, &workspace);

    // Text only, so the path and branch are gone after exit.
    let first = first_run(&setup);
    assert!(!first.path.exists());
    assert_eq!(
        first.repository,
        fs::canonicalize(setup.workspace().join(".git"))
            .unwrap()
            .to_string_lossy()
    );
    let (_, first_line) = session_log(&setup, &first.id);

    let head = commit_again(setup.deadline, &setup.workspace(), "second.txt");

    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let repo_guard = SessionGuard::arm(setup.deadline, &first.repository);
    subscribe(&client, &first.id);
    let mut stream = until_resumed(&client, "the resumed fiber_started");
    // Made again at the same path and branch, from the new HEAD.
    assert!(first.path.exists(), "the worktree exists again");
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

    send_prompt(&client, "c_prompt", &first.id, "write it");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    assert_eq!(
        fs::read_to_string(first.path.join("note.txt")).unwrap(),
        "hi\n"
    );
    send_close(&client, &first.id);
    stream.extend(until(&client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    }));
    let clients = stream
        .iter()
        .filter(|line| line["kind"] == "clients")
        .count();
    assert_eq!(clients, 1, "{stream:?}");
    // The resumed stream as a set, with the one `clients` line
    // counted: the capture starts at the resumed process, so the
    // lifecycle edges are asserted on the complete durable log.
    let mut expected = vec!["clients"];
    expected.extend([
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "turn_started",
        "command_accepted",
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
        "command_accepted",
        "fiber_exited",
    ]);
    support::assert_kinds_multiset(&stream, &expected, &["clients"], None, COUNT_DOC);
    // The hub capture above starts at the resumed process, so the
    // lifecycle edges are asserted on the complete continued log below.
    repo_guard.wait_gone();
    // Dirty, so the path and branch are kept.
    assert!(first.path.exists());
    assert!(
        !git(
            setup.deadline,
            &setup.workspace(),
            &["branch", "--list", &first.branch]
        )
        .is_empty(),
        "the branch is kept"
    );
    // One continued log, and the first line's bytes are unchanged.
    let (lines, now_first) = session_log(&setup, &first.id);
    assert_one_continued_log(&lines, 2);
    support::assert_session_process_boundaries(
        &lines,
        &first.id,
        SESSION_FIRST_DOC,
        PROCESS_START_DOC,
        PROCESS_END_DOC,
    );
    assert_eq!(now_first, first_line);
    let log = setup.hub_log();
    assert!(!log.contains(&first.repository), "{log}");
    assert!(
        !log.contains(first.path.to_string_lossy().as_ref()),
        "{log}"
    );
    drop(client);
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
}

#[test]
fn a_hub_resume_removes_a_clean_recreated_worktree_at_the_end() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());
    let workspace = workspace_text(&setup);
    let _ask_guard = SessionGuard::arm(setup.deadline, &workspace);

    let first = first_run(&setup);
    assert!(!first.path.exists());
    let head = commit_again(setup.deadline, &setup.workspace(), "second.txt");

    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let repo_guard = SessionGuard::arm(setup.deadline, &first.repository);
    subscribe(&client, &first.id);
    let mut stream = until_resumed(&client, "the resumed fiber_started");
    assert!(first.path.exists(), "the worktree exists again");
    assert_eq!(
        git(setup.deadline, &first.path, &["rev-parse", "HEAD"]),
        head
    );
    send_close(&client, &first.id);
    stream.extend(until(&client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    }));
    let clients = stream
        .iter()
        .filter(|line| line["kind"] == "clients")
        .count();
    assert_eq!(clients, 1, "{stream:?}");
    // The resumed stream as a set, with the one `clients` line
    // counted: the capture starts at the resumed process, so the
    // lifecycle edges are asserted on the complete durable log.
    let mut expected = vec!["clients"];
    expected.extend([
        "fiber_started",
        "extensions_loaded",
        "command_accepted",
        "fiber_exited",
    ]);
    support::assert_kinds_multiset(&stream, &expected, &["clients"], None, COUNT_DOC);
    // The hub capture above starts at the resumed process, so the
    // lifecycle edges are asserted on the complete durable log.
    let (continued, _) = session_log(&setup, &first.id);
    support::assert_session_process_boundaries(
        &continued,
        &first.id,
        SESSION_FIRST_DOC,
        PROCESS_START_DOC,
        PROCESS_END_DOC,
    );
    repo_guard.wait_gone();
    // Clean, so the path, the branch and the lock are gone again: a stray
    // hold on the session would have kept them.
    assert!(!first.path.exists());
    assert!(
        git(
            setup.deadline,
            &setup.workspace(),
            &["branch", "--list", &first.branch]
        )
        .is_empty(),
        "no fiber/ branch remains"
    );
    assert!(!lock_of(&first.path).exists());
    drop(client);
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
}

#[test]
fn a_hub_resume_of_a_kept_worktree_runs_in_it_and_never_removes_it() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        write_call("kept.txt", "x"),
        support::hello(),
        write_call("again.txt", "y"),
        support::hello(),
    ])
    .unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());
    let workspace = workspace_text(&setup);
    let _ask_guard = SessionGuard::arm(setup.deadline, &workspace);

    // The reply writes, so the worktree is kept.
    let first = first_run(&setup);
    assert!(first.path.join("kept.txt").is_file());
    // Back to clean, so only the new HEAD could move it.
    fs::remove_file(first.path.join("kept.txt")).unwrap();
    let base = git(setup.deadline, &first.path, &["rev-parse", "HEAD"]);
    let head = commit_again(setup.deadline, &setup.workspace(), "second.txt");
    assert_ne!(base, head);
    let (_, first_line) = session_log(&setup, &first.id);

    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let path_text = first.path.to_string_lossy().into_owned();
    let path_guard = SessionGuard::arm(setup.deadline, &path_text);
    subscribe(&client, &first.id);
    let mut stream = until_resumed(&client, "the resumed fiber_started");
    send_prompt(&client, "c_prompt", &first.id, "write again");
    stream.extend(until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    }));
    assert_eq!(
        fs::read_to_string(first.path.join("again.txt")).unwrap(),
        "y"
    );
    assert!(
        !setup.workspace().join("again.txt").exists(),
        "the write ran in the worktree, not the repository"
    );
    // Never recreated: still at the recorded HEAD, not the new one.
    assert_eq!(
        git(setup.deadline, &first.path, &["rev-parse", "HEAD"]),
        base
    );
    // Back to clean again.
    fs::remove_file(first.path.join("again.txt")).unwrap();
    send_close(&client, &first.id);
    stream.extend(until(&client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    }));
    let clients = stream
        .iter()
        .filter(|line| line["kind"] == "clients")
        .count();
    assert_eq!(clients, 1, "{stream:?}");
    // The resumed stream as a set, with the one `clients` line
    // counted: the capture starts at the resumed process, so the
    // lifecycle edges are asserted on the complete durable log.
    let mut expected = vec!["clients"];
    expected.extend([
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "turn_started",
        "command_accepted",
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
        "command_accepted",
        "fiber_exited",
    ]);
    support::assert_kinds_multiset(&stream, &expected, &["clients"], None, COUNT_DOC);
    path_guard.wait_gone();
    // A resumed process of a kept worktree never removes it.
    assert!(first.path.exists());
    assert!(
        !git(
            setup.deadline,
            &setup.workspace(),
            &["branch", "--list", &first.branch]
        )
        .is_empty(),
        "the branch is kept"
    );
    let (continued, now_first) = session_log(&setup, &first.id);
    support::assert_session_process_boundaries(
        &continued,
        &first.id,
        SESSION_FIRST_DOC,
        PROCESS_START_DOC,
        PROCESS_END_DOC,
    );
    assert_eq!(now_first, first_line);
    drop(client);
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
}

#[test]
fn a_hub_resume_with_the_repository_gone_is_io_failed_naming_it_and_changes_nothing() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());
    let workspace = workspace_text(&setup);
    let _ask_guard = SessionGuard::arm(setup.deadline, &workspace);

    let first = first_run(&setup);
    assert!(!first.path.exists());
    let events = session_dir(&setup, &first.id).join("events.jsonl");
    let before = fs::read(&events).unwrap();

    fs::rename(
        setup.workspace().join(".git"),
        setup.workspace().join(".git-moved"),
    )
    .unwrap();
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let repo_guard = SessionGuard::arm(setup.deadline, &first.repository);
    client.send(
        &json!({"id": "c_sub", "session_id": first.id, "command": "subscribe",
            "args": {"level": "full"}})
        .to_string(),
    );
    let stream = until(&client, "command_rejected", |line| {
        line["kind"] == "command_rejected"
    });
    support::assert_kinds_multiset(&stream, &["command_rejected"], &[], None, COUNT_DOC);
    let rejected = stream.last().unwrap();
    assert_eq!(rejected["payload"]["command_id"], "c_sub");
    assert_eq!(rejected["payload"]["code"], "io_failed");
    let message = rejected["payload"]["message"].as_str().unwrap().to_owned();
    // The confirmed launch-target failure takes the ruled shape: the hub
    // names it without the starter-error wrapper's period.
    assert_eq!(
        message,
        format!(
            "Session {} could not resume: {} no longer exists",
            first.id, first.repository
        ),
        "{message}"
    );
    assert!(
        !message.contains(first.path.to_string_lossy().as_ref()),
        "{message}"
    );
    // Nothing created, the log untouched, no lock left.
    assert!(!first.path.exists());
    assert_eq!(fs::read(&events).unwrap(), before);
    assert!(!lock_of(&first.path).exists());
    repo_guard.wait_gone();
    fs::rename(
        setup.workspace().join(".git-moved"),
        setup.workspace().join(".git"),
    )
    .unwrap();
    assert!(
        git(
            setup.deadline,
            &setup.workspace(),
            &["branch", "--list", &format!("fiber/{}", first.id)]
        )
        .is_empty(),
        "no fiber/ branch was created"
    );
    let log = setup.hub_log();
    assert!(!log.contains(&first.repository), "{log}");
    assert!(
        !log.contains(first.path.to_string_lossy().as_ref()),
        "{log}"
    );
    drop(client);
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
}
