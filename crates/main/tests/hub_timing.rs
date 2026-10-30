//! Part 14 operation timers at the binary level (`docs/state.md`,
//! "Diagnostic logs"): a debug home times hub start, resume,
//! subscription replay, socket preparation and session listing with only
//! `op` and `ms`; the default and the explicit info homes write no
//! `op_timing` at all. The built `fiber` runs `hub serve` in its own
//! process group with its own `FIBER_HOME`, holding an ordinary provider
//! whose base URL is the fake server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::*;

/// Prompt and workspace text no timer line may carry.
const PROBE: &str = "timing-probe-twelve";

/// The verbatim count rule the multiset calls cite
/// (`docs/testing.md`, "Event streams").
const COUNT_DOC: &str = "docs/testing.md:95: - the event kinds as a set with counts, so a duplicated or missing event fails";

/// Every `op_timing` line in `text`, parsed.
fn op_lines(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|line: &Value| line["code"] == "op_timing")
        .collect()
}

/// The complete stream of the first turn, from the session's start
/// through its first `turn_completed`, asserted as a set: no doc promises
/// this turn prefix an order, and the observer's own `session_status`,
/// `attention` and ephemeral `clients` lines fall where they fall, so none
/// is pinned (as `hub_start_overrides` filters them).
const FIRST_TURN_KINDS: [&str; 14] = [
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
];

/// The complete stream the resumed turn collects, asserted as a set: the
/// exited process's `fiber_exited`, already forwarded before the exit the
/// test closed on, then the resumed process's start through its
/// `turn_completed`. Deltas are live-only lines, so the resumed stream
/// carries none: the hub replays what the log holds.
const SECOND_TURN_KINDS: [&str; 14] = [
    "fiber_exited",
    "stream_reopened",
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
];

/// The `op_timing` lines naming `op`, in file order.
fn ops<'a>(lines: &'a [Value], op: &str) -> Vec<&'a Value> {
    lines
        .iter()
        .filter(|line| line["data"]["op"] == op)
        .collect()
}

/// The `sessions` answer to command `id`: a hub command without a
/// `session_id` is for the hub.
#[track_caller]
fn sessions(client: &Socket, id: &str, seen: &mut Vec<Value>) -> Value {
    client.send(&format!(
        "{{\"id\":\"{id}\",\"command\":\"sessions\",\"args\":{{}}}}"
    ));
    let ack = recv_answer(client, id, "the sessions acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert_eq!(ack["payload"]["command_id"], json!(id), "{ack}");
    seen.push(ack.clone());
    ack["payload"]["result"].clone()
}

/// The wire promise on every Part 14 line: debug, exactly the keys `op`
/// and `ms` with an unsigned `ms`, and none of the session's prompt,
/// workspace or credential anywhere in the line.
#[track_caller]
fn assert_wire(lines: &[&Value], op: &str, workspace: &str) {
    assert!(!lines.is_empty(), "the test proved its lines exist first");
    for line in lines {
        assert_eq!(line["code"], "op_timing", "{line}");
        assert_eq!(line["level"], "debug", "{line}");
        let data = &line["data"];
        let mut keys: Vec<&str> = data
            .as_object()
            .expect("the data is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort();
        assert_eq!(keys, ["ms", "op"], "{line}");
        assert_eq!(data["op"], json!(op), "{line}");
        assert!(data["ms"].is_u64(), "{line}");
        let text = serde_json::to_string(line).unwrap();
        for sentinel in [PROBE, workspace, "sk-test"] {
            assert!(!text.contains(sentinel), "{line}");
        }
    }
}

/// The durable log of session `id`, wherever its project is.
fn session_log(setup: &Setup, id: &str) -> String {
    let path = fs::read_dir(setup.home().join("projects"))
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
    fs::read_to_string(path).unwrap()
}

#[test]
fn debug_times_hub_start_resume_listing_replay_and_socket() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "diagnostics": {"level": "debug"}}),
    );
    let hub = Arc::new(Mutex::new(None));
    let (client, hello) = connect_hub(&setup, &hub);
    assert_eq!(hello["kind"], "hub_hello");
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let mut seen: Vec<Value> = Vec::new();

    // Hub start, accepted subscription and first turn.
    let session = start_session(&client, &workspace, PROBE);
    subscribe(&client, &session);
    let first = until(&client, "the first turn to complete", |line| {
        line["kind"] == "turn_completed"
    });
    seen.extend(first.iter().cloned());

    // A listing while the session runs.
    let listed_live = sessions(&client, "c_ses1", &mut seen);

    // Exit through the session's own socket: its EOF is the completion
    // signal the hub's listing after it follows.
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    close_session(&direct);
    drop(direct);

    // A listing after the exit, then a prompt that resumes the session
    // and replays the kept subscription for its second turn.
    let listed_exited = sessions(&client, "c_ses2", &mut seen);
    client.send(&format!(
        "{{\"id\":\"c_p2\",\"session_id\":\"{session}\",\"command\":\"prompt\",\"args\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{PROBE}-again\"}}]}}}}"
    ));
    let resumed_ack = recv_answer(&client, "c_p2", "the resumed prompt acknowledgement");
    assert_eq!(resumed_ack["kind"], "command_accepted", "{resumed_ack}");
    seen.push(resumed_ack);
    let second = until(&client, "the resumed turn to complete", |line| {
        line["kind"] == "turn_completed"
    });
    seen.extend(second.iter().cloned());

    // Exit again, so no session holds the workspace, then stop the hub:
    // both logs are complete once their processes are gone.
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    close_session(&direct);
    drop(direct);
    drop(client);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill_and_wait();
    guard.wait_gone();

    // One line per entered operation, and nothing else timed in the hub.
    let hub_timings = op_lines(&setup.hub_log());
    assert_eq!(hub_timings.len(), 5, "{hub_timings:?}");
    assert_wire(
        &ops(&hub_timings, "hub.session_start"),
        "hub.session_start",
        &workspace,
    );
    assert_wire(&ops(&hub_timings, "hub.resume"), "hub.resume", &workspace);
    assert_wire(
        &ops(&hub_timings, "sessions.list"),
        "sessions.list",
        &workspace,
    );
    assert_wire(
        &ops(&hub_timings, "hub.subscribe_replay"),
        "hub.subscribe_replay",
        &workspace,
    );
    assert_eq!(ops(&hub_timings, "sessions.list").len(), 2);

    // Both turns ran to completion over the socket, with nothing
    // missing or duplicated; the turn prefixes hold no promised order, so
    // each is asserted as a set.
    support::assert_kinds_multiset(&first, &FIRST_TURN_KINDS, &[], None, COUNT_DOC);
    support::assert_kinds_multiset(&second, &SECOND_TURN_KINDS, &[], None, COUNT_DOC);
    // The resumed stream opens with the exited process's goodbye.
    let exited = second
        .iter()
        .find(|line| line["kind"] == "fiber_exited")
        .expect("the exited process says goodbye");
    assert_eq!(exited["payload"]["exit_code"], json!(0), "{exited}");
    // The fake model's text arrived on both turns, and every line
    // naming a session names this one.
    for (turn, what) in [(&first, "the first turn"), (&second, "the resumed turn")] {
        let text = turn
            .iter()
            .find(|line| line["kind"] == "text_completed")
            .expect("the turn completes its text");
        assert_eq!(text["payload"]["text"], json!("Hello."), "{text}");
        for line in turn {
            if let Some(id) = line.get("session_id").and_then(Value::as_str) {
                assert_eq!(id, session, "{what} stays on its session: {line}");
            }
        }
    }
    // The listings answered with the stable shape, whatever the feed's
    // scan had followed when each ran: pinning followed contents would
    // pin scan timing, which is independently triggered.
    for (listing, id) in [(&listed_live, "c_ses1"), (&listed_exited, "c_ses2")] {
        let mut keys: Vec<&str> = listing
            .as_object()
            .expect("the listing answers with an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort();
        assert_eq!(keys, ["exited", "live"], "{id}: {listing}");
    }

    // Both session processes timed their socket preparation into the
    // session's own diagnostic file.
    let socket_timings: Vec<Value> = op_lines(
        &fs::read_to_string(
            setup
                .home()
                .join("logs")
                .join(format!("session-{session}.log")),
        )
        .unwrap(),
    )
    .into_iter()
    .filter(|line| line["data"]["op"] == "startup.socket")
    .collect();
    assert_wire(
        &socket_timings.iter().collect::<Vec<_>>(),
        "startup.socket",
        &workspace,
    );
    assert_eq!(socket_timings.len(), 2, "the new and the resumed process");

    // No timer leaks into the durable session log or the command output.
    assert!(
        !session_log(&setup, &session).contains("op_timing"),
        "the session log holds events, never diagnostics"
    );
    for line in &seen {
        let text = serde_json::to_string(line).unwrap();
        assert!(!text.contains("op_timing"), "{line}");
    }
}

/// A home with `debug` false runs start, subscription, turn and listing,
/// then asserts no timer was written anywhere under `logs/`.
#[track_caller]
fn info_home_writes_no_operation_timing(explicit: bool) {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    if explicit {
        support::write_json(
            &setup.home().join("config.json"),
            &json!({"model": "fake/m", "diagnostics": {"level": "info"}}),
        );
    }
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, PROBE);
    subscribe(&client, &session);
    until(&client, "the turn to complete", |line| {
        line["kind"] == "turn_completed"
    });
    let mut seen = Vec::new();
    sessions(&client, "c_ses1", &mut seen);
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    close_session(&direct);
    drop(direct);
    drop(client);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill_and_wait();
    guard.wait_gone();

    // The operations ran — the hub logged them — but no timer did.
    let log = setup.hub_log();
    assert!(log.contains("\"code\":\"session_started\""), "{log}");
    let timings = op_lines(&log);
    assert!(timings.is_empty(), "{timings:?}");
    let logs = setup.home().join("logs");
    if logs.exists() {
        for entry in fs::read_dir(&logs).unwrap() {
            let text = fs::read_to_string(entry.unwrap().path()).unwrap();
            assert!(
                op_lines(&text).is_empty(),
                "no diagnostic file times an operation"
            );
        }
    }
}

#[test]
fn default_info_writes_no_operation_timing() {
    info_home_writes_no_operation_timing(false);
}

#[test]
fn explicit_info_writes_no_operation_timing() {
    info_home_writes_no_operation_timing(true);
}
