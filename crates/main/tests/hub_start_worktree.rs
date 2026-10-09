//! Binary-level test that a hub `start` with `worktree: true` runs the
//! session in a new worktree (`docs/invocation.md`, "What the hub speaks"
//! and "Isolation"): the built `fiber` runs `hub serve` in its own process
//! group with its own `FIBER_HOME`, the session's `session_started` records
//! the worktree, and closing the clean session removes it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{Deadline, HubProc, SessionGuard, Setup, connect_hub, recv_reply, subscribe, until};

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
    let out = support::run_to_exit(deadline, &format!("git {args:?}"), command);
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

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

/// The event kinds of `lines`, in order, without `session_status`: an
/// observer thread writes it, so where it falls among the loop's own
/// lines is not what this test pins (as `tests/session_command.rs`
/// filters it).
fn kinds(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| line["kind"] != "session_status")
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
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
    let without_clients: Vec<&str> = kinds(&stream)
        .into_iter()
        .filter(|kind| *kind != "clients")
        .collect();
    assert_eq!(
        without_clients,
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "command_accepted",
            "fiber_exited",
        ]
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
