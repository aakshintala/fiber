//! Binary-level test that a hub `start` with `overrides` passes each one
//! to the session it spawns as `-c` (`docs/invocation.md`, "What the hub
//! speaks"): the built `fiber` runs `hub serve` in its own process group
//! with its own `FIBER_HOME`, the session's resolved configuration holds
//! the override, and a malformed override rejects the start with the
//! session's usage sentence.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::sync::{Arc, Mutex};

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::*;

/// The prompt a started session runs.
const PROMPT: &str = "the-volume-of-the-meeting-room";

/// The event kinds of `lines`, in order, without `session_status` or
/// `attention`: an observer thread writes `session_status`, so where it
/// falls among the loop's own lines is not what this test pins (as
/// `tests/session_command.rs` filters it), and the hub's `attention` line
/// derives from that status, so whether it comes and where is not pinned
/// either.
fn kinds(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| line["kind"] != "session_status" && line["kind"] != "attention")
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}

/// The complete, ordered stream from the subscription through `fiber_exited`:
/// the replayed start, the full retried turn, the `close` acknowledgement
/// and the exit. The ephemeral `clients` line is checked apart: the hub sends
/// a `start`'s first prompt once the `full` `subscribe` is accepted or 1
/// second after its answer, so on a loaded machine `clients` can fall after
/// the loop's first lines, and only its presence is pinned.
const EXPECTED_KINDS: [&str; 20] = [
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "usage_recorded",
    "assistant_message_completed",
    "retry_scheduled",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "command_accepted",
    "fiber_exited",
];

#[test]
fn hub_start_with_overrides_runs_the_session_with_them_as_dash_c() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(503, "{}"), hello()]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "retry": {
            "attempts": 0, "initial_delay_ms": 0, "max_delay_ms": 0,
        }}),
    );

    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    client.send(
        &json!({
            "id": "c_start",
            "command": "start",
            "args": {"workspace": workspace, "overrides": ["retry.attempts=2"],
                     "content": [{"type": "text", "text": PROMPT}]},
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
    let mut stream = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    // Attempts 0 in the file would fail the first 503 at once: one
    // `retry_scheduled` reaching attempt 3 proves the override resolved.
    let scheduled: Vec<_> = stream
        .iter()
        .filter(|line| line["kind"] == "retry_scheduled")
        .collect();
    assert_eq!(scheduled.len(), 1, "{stream:?}");
    assert_eq!(scheduled[0]["payload"]["last_attempt"], 3);
    assert!(
        stream
            .iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "the fake model's text arrived through the hub: {stream:?}"
    );

    client.send(&json!({"id": "c_close", "session_id": id, "command": "close"}).to_string());
    let tail = until(&client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    stream.extend(tail);
    let clients = stream
        .iter()
        .filter(|line| line["kind"] == "clients")
        .count();
    assert_eq!(clients, 1, "{stream:?}");
    let without_clients: Vec<&str> = kinds(&stream)
        .into_iter()
        .filter(|kind| *kind != "clients")
        .collect();
    assert_eq!(without_clients, EXPECTED_KINDS);
    guard.wait_gone();
    drop(client);
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
}

#[test]
fn hub_start_with_a_malformed_override_rejects_with_the_session_sentence() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    for (id, overrides) in [("c_1", json!(["nokey"])), ("c_2", json!(["--worktree"]))] {
        client.send(
            &json!({
                "id": id,
                "command": "start",
                "args": {"workspace": workspace, "overrides": overrides},
            })
            .to_string(),
        );
        let rejected = recv_reply(&client, "the start rejection");
        assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
        assert_eq!(rejected["payload"]["command_id"], id);
        assert_eq!(rejected["payload"]["code"], "usage");
        let message = rejected["payload"]["message"].as_str().unwrap();
        assert!(
            message.ends_with(" Run `fiber --help` for usage."),
            "{message}"
        );
        // No session started: no socket, and the workspace is untouched.
        let run = setup.home().join("run");
        if run.is_dir() {
            assert!(
                std::fs::read_dir(&run).unwrap().all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("s_")),
                "no session socket is left"
            );
        }
        assert!(
            std::fs::read_dir(&workspace).unwrap().next().is_none(),
            "the workspace gained no worktree"
        );
        assert!(server.requests().is_empty());
    }
    drop(client);
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
    drop(guard);
}
