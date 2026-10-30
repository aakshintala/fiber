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

/// The verbatim count rules the multiset calls cite
/// (`docs/testing.md`, "Event streams"): the general set-with-counts rule,
/// plus the one-`clients`-line-per-attach promise for the counted `clients`
/// line.
const COUNT_DOC: &str = "docs/testing.md:95: - the event kinds as a set with counts, so a duplicated or missing event fails; docs/testing.md:109-111: Where its count is fixed, such as one `clients` line per attach, the test asserts the count, so a duplicate or a missing line still fails";

/// Verbatim lifecycle sources for the session-boundary assertions below.
const SESSION_FIRST_DOC: &str =
    "docs/events.md:339: Durable. The first line of every session's log.";
const PROCESS_START_DOC: &str = "docs/events.md:285-286: Durable. The first line a process writes when it resumes a session; in a new session it follows `session_started`, which is always the log's first line.";
const PROCESS_END_DOC: &str =
    "docs/events.md:296: Durable. The last line a process writes for a session.";

/// The durable log of session `id`, wherever its project is.
fn session_log(setup: &Setup, id: &str) -> Vec<Value> {
    let path = std::fs::read_dir(setup.home().join("projects"))
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .join("sessions")
                .join(id)
                .join("events.jsonl")
        })
        .find(|log| log.is_file())
        .expect("the session's log exists");
    support::complete_lines(&std::fs::read_to_string(path).unwrap())
}

/// The complete stream from the subscription through `fiber_exited`,
/// asserted as a set: the replayed start, the full retried turn, the
/// `close` acknowledgement and the exit. The ephemeral `clients` line is
/// counted too: the hub sends a `start`'s first prompt once the `full`
/// `subscribe` is accepted or 1 second after its answer, so on a loaded
/// machine `clients` can fall after the loop's first lines, and only its
/// presence is pinned.
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
    let mut expected = vec!["clients"];
    expected.extend(EXPECTED_KINDS);
    support::assert_kinds_multiset(&stream, &expected, &["clients"], None, COUNT_DOC);
    support::assert_session_process_boundaries(
        &session_log(&setup, &id),
        &id,
        SESSION_FIRST_DOC,
        PROCESS_START_DOC,
        PROCESS_END_DOC,
    );
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
