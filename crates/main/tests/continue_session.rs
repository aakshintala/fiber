//! Binary-level test that a hub `start` with `continues` runs the new
//! session where the old one did, and the worktree stands in both exit
//! orders (`docs/invocation.md`, "`start` `continues`" and "Isolation"):
//! the built `fiber` runs `hub serve` in its own process group with its
//! own `FIBER_HOME`, the fake provider server, and a `support::git`
//! temporary repository. The old session exits first while the new one
//! runs, then a second pair exits new-first while the old still runs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::path::Path;
use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{
    Deadline, HubProc, SessionGuard, Setup, Socket, complete_lines, connect_hub, git,
    hello_item_done as hello, session_dir, until,
};

/// A repository with one commit on `main` at `dir`.
fn init_repo(deadline: Deadline, dir: &Path) {
    git(deadline, dir, &["init", "--quiet"]);
    std::fs::write(dir.join("file.txt"), "x").unwrap();
    git(deadline, dir, &["add", "."]);
    git(deadline, dir, &["commit", "--quiet", "-m", "first"]);
}

/// Sends command `id` for `session` (`None` for hub-level commands).
fn send(client: &Socket, id: &str, session: Option<&str>, command: &str, args: Value) {
    let mut line = json!({"id": id, "command": command, "args": args});
    if let Some(session) = session {
        line["session_id"] = session.into();
    }
    client.send(&line.to_string());
}

/// The acknowledgement of command `id`: every line until it, the last
/// accepted.
fn until_ack(client: &Socket, what: &str, id: &str) -> Vec<Value> {
    until(client, what, |line| {
        line["payload"].get("command_id").and_then(Value::as_str) == Some(id)
    })
}

/// Starts the old session in a new worktree and returns its id and the
/// worktree's path.
fn start_old(client: &Socket, workspace: &str, tag: &str) -> (String, String) {
    send(
        client,
        &format!("c_old_{tag}"),
        None,
        "start",
        json!({"workspace": workspace, "worktree": true}),
    );
    let ack = until_ack(
        client,
        "the old start acknowledgement",
        &format!("c_old_{tag}"),
    );
    let id = ack.last().unwrap()["payload"]["result"]["session_id"]
        .as_str()
        .expect("the start answers with a session id")
        .to_owned();
    send(
        client,
        &format!("s_old_{tag}"),
        Some(&id),
        "subscribe",
        json!({"level": "full"}),
    );
    until_ack(
        client,
        "the old subscribe acknowledgement",
        &format!("s_old_{tag}"),
    );
    let stream = until(client, "the old session_started", |line| {
        line["kind"] == "session_started" && line["session_id"] == id
    });
    let first = stream
        .iter()
        .find(|line| line["kind"] == "session_started")
        .expect("session_started is replayed");
    let path = first["payload"]["worktree"]["path"]
        .as_str()
        .expect("the old session records its worktree")
        .to_owned();
    (id, path)
}

/// Starts the new session continuing `old` in `path` and returns its id.
/// The start carries a prompt, so the new session runs one turn: a
/// session that never runs a turn deletes its own directory on close
/// (`docs/invocation.md`, "Lifecycle"), and the keep needs the exited
/// new session's `session_started` row.
fn start_new(client: &Socket, path: &str, old: &str, tag: &str) -> String {
    send(
        client,
        &format!("c_new_{tag}"),
        None,
        "start",
        json!({
            "workspace": path,
            "continues": old,
            "content": [{"type": "text", "text": "Say hello."}],
        }),
    );
    let ack = until_ack(
        client,
        "the new start acknowledgement",
        &format!("c_new_{tag}"),
    );
    assert_eq!(ack.last().unwrap()["kind"], "command_accepted", "{ack:?}");
    let id = ack.last().unwrap()["payload"]["result"]["session_id"]
        .as_str()
        .expect("the start answers with a session id")
        .to_owned();
    send(
        client,
        &format!("s_new_{tag}"),
        Some(&id),
        "subscribe",
        json!({"level": "full"}),
    );
    until_ack(
        client,
        "the new subscribe acknowledgement",
        &format!("s_new_{tag}"),
    );
    until(client, "the new session_started", |line| {
        line["kind"] == "session_started" && line["session_id"] == id
    });
    until(client, "the new turn_completed", |line| {
        line["kind"] == "turn_completed" && line["session_id"] == id
    });
    id
}

/// The worktree path the session's own log first line records.
fn logged_worktree(setup: &Setup, id: &str) -> String {
    let text =
        std::fs::read_to_string(session_dir(setup, id).join("events.jsonl")).expect("the log");
    let first = complete_lines(&text)
        .into_iter()
        .next()
        .expect("the log has a first line");
    assert_eq!(first["kind"], "session_started", "{first:?}");
    first["payload"]["worktree"]["path"]
        .as_str()
        .expect("the log records its worktree")
        .to_owned()
}

/// Closes `session` and waits for its `fiber_exited`.
fn close_wait(client: &Socket, session: &str, tag: &str) {
    send(
        client,
        &format!("x_{tag}"),
        Some(session),
        "close",
        json!({}),
    );
    until(client, "the fiber_exited", |line| {
        line["kind"] == "fiber_exited" && line["session_id"] == session
    });
}

/// Waits for the session process `id` to exit: its `isolation.end`
/// runs after its `fiber_exited`, so the worktree's fate is settled
/// only once the process is gone.
#[track_caller]
fn wait_gone(setup: &Setup, id: &str) {
    fakes::try_matching_exits(id, setup.deadline).expect("the session process exits");
}

#[test]
fn continues_keeps_the_worktree_in_both_exit_orders() {
    let setup = Setup::new();
    let server = ProviderServer::start_with_fallback([], hello()).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());
    let workspace = setup.workspace().to_string_lossy().into_owned();

    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let guard_ws = SessionGuard::arm(setup.deadline, &workspace);

    // The old session exits first while the new one runs.
    let (old1, path1) = start_old(&client, &workspace, "one");
    let new1 = start_new(&client, &path1, &old1, "one");
    assert_eq!(
        logged_worktree(&setup, &new1),
        path1,
        "the new session records the same worktree path"
    );
    let guard_w1 = SessionGuard::arm(setup.deadline, &path1);
    close_wait(&client, &old1, "old1");
    assert!(Path::new(&path1).exists(), "the worktree stands");
    close_wait(&client, &new1, "new1");
    assert!(Path::new(&path1).exists(), "the worktree stands");

    // The new session exits first while the old still runs.
    let (old2, path2) = start_old(&client, &workspace, "two");
    let new2 = start_new(&client, &path2, &old2, "two");
    assert_eq!(
        logged_worktree(&setup, &new2),
        path2,
        "the new session records the same worktree path"
    );
    let guard_w2 = SessionGuard::arm(setup.deadline, &path2);
    close_wait(&client, &new2, "new2");
    assert!(Path::new(&path2).exists(), "the worktree stands");
    close_wait(&client, &old2, "old2");
    // The old session's `end` runs after its exit line: only the gone
    // process proves the worktree was kept rather than removed.
    wait_gone(&setup, &old2);
    assert!(Path::new(&path2).exists(), "the worktree stands");

    drop(client);
    guard_ws.wait_gone();
    guard_w1.wait_gone();
    guard_w2.wait_gone();
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
}
