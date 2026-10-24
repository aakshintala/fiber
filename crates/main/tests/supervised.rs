//! Binary-level tests of supervised sessions (`docs/permissions.md`,
//! "Supervised sessions"): the built `fiber` runs `hub serve` with its own
//! `FIBER_HOME`, the supervisor starts the session over the hub socket with
//! `supervised`, and the session stays unanswerable end to end.

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

/// A model reply calling `shell` with `command`.
fn shell_call(command: &str) -> fakes::Response {
    stream(&[function_call(
        "call_1",
        "shell",
        &json!({"command": command}),
    )])
}

/// A reviewer verdict reading as `text`.
fn verdict(text: &str) -> fakes::Response {
    text_reply(text)
}

/// A session the supervisor starts over the hub: `supervised` when true,
/// subscribed `full` once admitted.
fn start_supervised(client: &Socket, workspace: &str, supervised: bool, prompt: &str) -> String {
    client.send(&format!(
        "{{\"id\":\"c_start\",\"command\":\"start\",\"args\":{{\"workspace\":\"{workspace}\",\"supervised\":{supervised},\"content\":[{{\"type\":\"text\",\"text\":\"{prompt}\"}}]}}}}"
    ));
    let ack = recv_answer(client, "c_start", "the start acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    let session = ack["payload"]["result"]["session_id"]
        .as_str()
        .expect("the start answers with a session id")
        .to_owned();
    client.send(&format!(
        "{{\"id\":\"c_sub\",\"session_id\":\"{session}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
    ));
    let ack = recv_answer(client, "c_sub", "the subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    session
}

/// Sends a `prompt` carrying `text` to the session.
fn prompt(client: &Socket, id: &str, name: &str, text: &str) {
    client.send(&format!(
        "{{\"id\":\"{name}\",\"session_id\":\"{id}\",\"command\":\"prompt\",\"args\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}"
    ));
}

/// Sends a supervisor `steer` carrying `text` to the session.
fn steer(client: &Socket, id: &str, name: &str, text: &str) {
    client.send(&format!(
        "{{\"id\":\"{name}\",\"session_id\":\"{id}\",\"command\":\"steer\",\"args\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}"
    ));
}

/// Closes the session and waits for its process to exit.
fn close_and_wait(client: &Socket, id: &str) {
    client.send(&format!(
        "{{\"id\":\"c_close\",\"session_id\":\"{id}\",\"command\":\"close\"}}"
    ));
    until(client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
}

/// The session's log lines, complete ones only.
fn read_log(setup: &Setup, id: &str) -> Vec<Value> {
    let text =
        fs::read_to_string(session_dir(setup, id).join("events.jsonl")).expect("the log reads");
    complete_lines(&text)
}

/// Configures `fake/m` with a reviewer on the same fake server. Shell
/// calls then meet the reviewer with no standing rule in the way: a
/// standing ask would deny at once with nobody to answer, before the
/// reviewer ever judged.
fn reviewing(setup: &Setup, server: &ProviderServer) {
    setup.provider(server);
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "reviewer": {"model": "fake/m"}}),
    );
}

/// Starts the hub for the test, returning the client and the hub slot.
fn started_hub(setup: &Setup, hub: &Arc<Mutex<Option<HubProc>>>) -> Socket {
    let (client, _) = connect_hub(setup, hub);
    client
}

/// The ordered event kinds, setting aside only the lines the contract
/// writes on their own trigger, with no position relative to the session's
/// other lines (`docs/testing.md`, "Event streams"): `clients`, written when
/// a connection attaches; `session_status` and the hub's `attention`,
/// written when the session's summary changes; and `date_changed`, written
/// when the local date changes. Every other kind is pinned in order,
/// including the ones without `seq`: the ephemeral deltas, `command_accepted`
/// and `steering_queue` (`docs/invocation.md`, "Driver commands";
/// `docs/events.md`, "`steering_queue`", "`assistant_message_delta`").
/// The projection itself is `support::kinds`, imported through `support::*`.
fn assert_one_clients_line(lines: &[Value]) {
    // One `clients` line per attach is fixed (`docs/testing.md`, "Event
    // streams"), so the count is pinned against the raw lines before the
    // ordered projection sets it aside.
    let clients = lines
        .iter()
        .filter(|line| line["kind"] == "clients")
        .count();
    assert_eq!(clients, 1, "one attach writes one clients line: {lines:?}");
}

/// Queries `sessions` until the hub lists the session as exited. The
/// shutdown writes the `recent.jsonl` row before the session process
/// exits, so the loop converges instead of racing the feed's scan: each
/// query blocks for its own answer, and the socket's deadline bounds the
/// whole wait.
fn exited_row(client: &Socket, id: &str) -> Value {
    let mut n = 0;
    loop {
        n += 1;
        let command = format!("c_sess_{n}");
        client.send(&format!(
            "{{\"id\":\"{command}\",\"command\":\"sessions\"}}"
        ));
        let listing = until(client, "the sessions answer", |line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == command
        });
        let answer = listing.last().expect("the sessions answer");
        let exited = answer["payload"]["result"]["exited"]
            .as_array()
            .expect("the answer names exited sessions");
        if let Some(row) = exited.iter().find(|row| row["session_id"] == id) {
            return row.clone();
        }
    }
}

/// Stops the hub and waits for the session watchdog to stand down.
fn stop_hub(hub: &Arc<Mutex<Option<HubProc>>>, guard: SessionGuard) {
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
    guard.wait_gone();
}

#[test]
fn a_supervised_start_denies_without_asking_and_sends_no_attention() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        shell_call("true"),
        verdict("maybe SUP-FIRST"),
        verdict("perhaps SUP-LAST"),
        hello(),
    ])
    .unwrap();
    reviewing(&setup, &server);
    let hub = Arc::new(Mutex::new(None));
    let control = started_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let id = start_supervised(&control, &workspace, true, "run it");
    let lines = until(&control, "the turn end", |line| {
        line["kind"] == "turn_completed"
    });
    // Complete ordered kinds (`docs/testing.md`, "Event streams"): only
    // `clients`, `session_status`, the hub's `attention` and `date_changed`
    // are set aside, so a duplicated, missing or reordered event of any
    // other kind fails, ephemeral deltas included. Order follows
    // `docs/loop.md`, "One step" with `permission_resolved` standing where
    // `tool_call_started` would (`docs/permissions.md`, "What the log
    // records"), and the deny-then-continue from `docs/permissions.md`,
    // "Supervised sessions" ("An escalation is a block at once, and the run
    // carries on") and "What happens on a block" ("A block is not the end
    // of the turn. ... The turn continues."). The two
    // `assistant_message_delta` lines are the closing `Hello.` streaming in
    // two fragments (`docs/loop.md`, "One step": the loop streams the reply,
    // emitting its actions as they arrive; `docs/events.md`,
    // "`assistant_message_delta`").
    assert_one_clients_line(&lines);
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
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ],
        "complete ordered kinds: {lines:?}"
    );
    let started = lines
        .iter()
        .find(|line| line["kind"] == "session_started")
        .expect("a session_started line");
    // Fixed at start and recorded (`docs/permissions.md`, "Supervised
    // sessions": "`supervised` is fixed at `start`, recorded on
    // `session_started`, and kept across a resume").
    assert_eq!(started["payload"]["supervised"], true);
    // The escalation became a block at once: nothing waited on anyone
    // (`docs/permissions.md`, "Supervised sessions": "Nothing waits on a
    // person and no request stays open"; "What the log records": an
    // escalation where no answer is possible writes no
    // `permission_requested`, recorded as a `permission_resolved` with
    // `decided_by: reviewer`).
    assert!(
        !lines
            .iter()
            .any(|line| line["kind"] == "permission_requested"),
        "no approval waits on anyone: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line["kind"] == "notice"),
        "no notice explains the block: {lines:?}"
    );
    // Sends no `attention` line (`docs/permissions.md`, "Supervised
    // sessions": "The block is on the session's event stream, and sends no
    // `attention` line.").
    assert!(
        !lines.iter().any(|line| line["kind"] == "attention"),
        "a supervised turn sends no attention: {lines:?}"
    );
    let resolved: Vec<&Value> = lines
        .iter()
        .filter(|line| line["kind"] == "permission_resolved")
        .collect();
    // One judged call, one block (`docs/permissions.md`, "Supervised
    // sessions": "Its own reviewer judges every call. An escalation is a
    // block at once").
    assert_eq!(resolved.len(), 1, "{lines:?}");
    assert_eq!(resolved[0]["payload"]["decision"], "deny");
    assert_eq!(resolved[0]["payload"]["decided_by"], "reviewer");
    // The turn carries on past the block to the model's close
    // (`docs/permissions.md`, "What happens on a block": "The refusal is
    // returned to the model ... The turn continues.").
    let end = lines.last().expect("the turn end");
    assert_eq!(end["payload"]["outcome"], "completed");
    close_and_wait(&control, &id);
    // The exited session lists with its flag: the shutdown wrote the
    // `recent.jsonl` row carrying the last `session_status`.
    let row = exited_row(&control, &id);
    assert_eq!(row["how"], "exited", "{row}");
    assert_eq!(row["status"]["supervised"], true, "{row}");
    stop_hub(&hub, guard);
}

#[test]
fn a_supervised_steer_grants_a_fresh_review_and_allows() {
    const MARKER: &str = "grant-marker-7f3a";
    let setup = Setup::new();
    let server = ProviderServer::start([
        shell_call("true"),
        verdict("maybe SUP-FIRST"),
        verdict("perhaps SUP-LAST"),
        hello(),
        shell_call("true"),
        verdict("check"),
        verdict("allow the supervisor granted it"),
        hello(),
    ])
    .unwrap();
    reviewing(&setup, &server);
    let hub = Arc::new(Mutex::new(None));
    let control = started_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let id = start_supervised(&control, &workspace, true, "run it");
    // Reviewer requests are the provider calls without tools: the session
    // model always offers its tools, the reviewer offers none.
    let reviewer_bodies = || {
        server
            .requests()
            .iter()
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .filter(|body| {
                serde_json::from_str::<Value>(body)
                    .ok()
                    .and_then(|json| json.get("tools").cloned())
                    .is_none_or(|tools| tools.as_array().is_none_or(|tools| tools.is_empty()))
            })
            .collect::<Vec<String>>()
    };
    let mut lines = until(&control, "the first turn end", |line| {
        line["kind"] == "turn_completed"
    });
    // The block was judged (`docs/permissions.md`, "Supervised sessions":
    // "Its own reviewer judges every call").
    let first_reviewed = reviewer_bodies().len();
    assert!(
        !reviewer_bodies().is_empty(),
        "the block was judged: {:?}",
        reviewer_bodies()
    );
    // The supervisor steers its grant; the person's own prompt would arrive
    // the same way (`docs/permissions.md`, "Supervised sessions": "The
    // reviewer reads the supervisor's prompts and steers as the person's
    // words").
    steer(
        &control,
        &id,
        "c_steer",
        &format!("{MARKER}: you may run shell"),
    );
    lines.extend(until(&control, "the second turn end", |line| {
        line["kind"] == "turn_completed"
    }));
    // Complete ordered kinds (`docs/testing.md`, "Event streams"): only
    // `clients`, `session_status`, the hub's `attention` and `date_changed`
    // are set aside, so a duplicated, missing or reordered event of any
    // other kind fails, ephemeral lines included. The first turn denies as
    // in the block test, then the steered second turn allows. The
    // deny-then-allow order is forced by the pause point between them: the
    // first `until` completes before the steer is sent. The allow turn's
    // `permission_resolved` precedes its `tool_call_started`
    // (`docs/permissions.md`, "What the log records"), and the run carries
    // on (`docs/permissions.md`, "Supervised sessions": "An escalation is a
    // block at once, and the run carries on"; "the model's retry passes").
    // Between the turns the steer queues, is answered, and drains:
    // `steering_queue` is written whenever the queue changes (`docs/events.md`,
    // "`steering_queue`"), the steer's `command_accepted` answers it exactly
    // once (`docs/invocation.md`, "Driver commands"), and the second
    // `steering_queue` is the emptied queue the next turn drains
    // (`docs/loop.md`, "One step": the loop drains the inbox at the step
    // boundary). The closing `assistant_message_delta` pairs stream each
    // turn's `Hello.` in two fragments (`docs/loop.md`, "One step";
    // `docs/events.md`, "`assistant_message_delta`").
    assert_one_clients_line(&lines);
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
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "steering_queue",
            "command_accepted",
            "steering_queue",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
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
        ],
        "complete ordered kinds: {lines:?}"
    );
    // The steer never answers an approval (`docs/permissions.md`,
    // "Supervised sessions": "The supervisor never answers an approval").
    assert!(
        !lines
            .iter()
            .any(|line| line["kind"] == "permission_requested"),
        "the steer never answers an approval: {lines:?}"
    );
    let resolved: Vec<&Value> = lines
        .iter()
        .filter(|line| line["kind"] == "permission_resolved")
        .collect();
    // Two judged calls: the block, then the allowed retry
    // (`docs/permissions.md`, "Supervised sessions": "Its own reviewer
    // judges every call. An escalation is a block at once" and "the
    // model's retry passes"). The order follows the turns: the first
    // `until` ends before the steer, so the deny predates it and the allow
    // follows it.
    assert_eq!(resolved.len(), 2, "{lines:?}");
    assert_eq!(resolved[0]["payload"]["decision"], "deny");
    assert_eq!(resolved[0]["payload"]["decided_by"], "reviewer");
    assert_eq!(resolved[1]["payload"]["decision"], "allow", "{lines:?}");
    assert_eq!(
        resolved[1]["payload"]["decided_by"], "reviewer",
        "{lines:?}"
    );
    // The allowed retry ran (`docs/permissions.md`, "Supervised sessions":
    // "the model's retry passes").
    assert!(
        lines
            .iter()
            .any(|line| line["kind"] == "tool_call_completed"),
        "the allowed retry ran: {lines:?}"
    );
    // The retry was judged again on the steered words
    // (`docs/permissions.md`, "Supervised sessions": the reviewer "judges
    // the call again when the model retries it"): more reviewer requests
    // arrive after the steer, the later ones carry the grant, and the
    // earlier ones predate it. The split sits on the pause point above:
    // every request before the steer belongs to the block, every request
    // after to the retry.
    let reviewer = reviewer_bodies();
    assert!(
        reviewer.len() > first_reviewed,
        "the retry was judged again: {reviewer:?}"
    );
    let (blocked, granted) = reviewer.split_at(first_reviewed);
    assert!(
        granted.iter().all(|body| body.contains(MARKER)),
        "the retry's reviews read the steer: {granted:?}"
    );
    assert!(
        blocked.iter().all(|body| !body.contains(MARKER)),
        "the block's reviews predate the steer: {blocked:?}"
    );
    close_and_wait(&control, &id);
    stop_hub(&hub, guard);
}

/// A `fiber ask`-style `ask_user` call the supervisor's program drives.
fn ask_user_call() -> fakes::Response {
    stream(&[function_call(
        "call_ask",
        "ask_user",
        &json!({"questions": [{"header": "Pick", "question": "Which one?",
            "options": [{"label": "a"}, {"label": "b"}]}]}),
    )])
}

#[test]
fn a_supervised_ask_user_completes_with_its_questions() {
    let setup = Setup::new();
    let server = ProviderServer::start([ask_user_call(), hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let control = started_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let id = start_supervised(&control, &workspace, true, "pick one");
    let lines = until(&control, "the turn end", |line| {
        line["kind"] == "turn_completed"
    });
    // Complete ordered kinds (`docs/testing.md`, "Event streams"): only
    // `clients`, `session_status`, the hub's `attention` and `date_changed`
    // are set aside, so a duplicated, missing or reordered event of any
    // other kind fails. The `ask_user` turn ends `completed` with the
    // questions on `turn_completed` (`docs/tools.md`, "When a program drives
    // the session"). The `ask_user` call carries no streamed text, so no
    // `assistant_message_delta` appears: the request runs (`tool_call_started`)
    // and completes (`tool_call_completed`) between the completed request
    // (`assistant_message_completed`, `docs/loop.md`, "One step") and the
    // turn end.
    assert_one_clients_line(&lines);
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
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "turn_completed",
        ],
        "complete ordered kinds: {lines:?}"
    );
    let end = lines.last().expect("the turn end");
    assert_eq!(end["payload"]["outcome"], "completed");
    let questions = end["payload"]["questions"]
        .as_array()
        .expect("the questions ride the turn end: {end}");
    assert_eq!(questions.len(), 1, "{end}");
    assert_eq!(questions[0]["question"], "Which one?");
    // The program drives: the call's result says the questions went to
    // the driver, and nobody was asked (`docs/tools.md`, "When a program
    // drives the session": "The turn ends `completed`, with the questions
    // on `turn_completed`. The call's result is one line saying the
    // questions went to the driver."; `docs/permissions.md`, "Supervised
    // sessions": "Nothing waits on a person and no request stays open").
    assert!(
        lines
            .iter()
            .any(|line| line["kind"] == "tool_call_completed"),
        "the ask_user call completed: {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line["kind"] == "permission_requested"),
        "nobody was asked: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line["kind"] == "attention"),
        "a supervised turn sends no attention: {lines:?}"
    );
    close_and_wait(&control, &id);
    stop_hub(&hub, guard);
}

#[test]
fn a_supervised_resume_stays_unanswerable() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        shell_call("true"),
        verdict("maybe SUP-FIRST"),
        verdict("perhaps SUP-LAST"),
        hello(),
        shell_call("true"),
        verdict("maybe SUP-AGAIN"),
        verdict("perhaps SUP-LAST-AGAIN"),
        hello(),
    ])
    .unwrap();
    reviewing(&setup, &server);
    let hub = Arc::new(Mutex::new(None));
    let control = started_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let id = start_supervised(&control, &workspace, true, "run it");
    until(&control, "the first turn end", |line| {
        line["kind"] == "turn_completed"
    });
    close_and_wait(&control, &id);
    // A fresh client subscribing to the exited session resumes it; the
    // resumed session keeps the first line's flag. The old socket stays
    // subscribed, so re-subscribing on it would be rejected.
    let (resumed, _) = connect_hub(&setup, &hub);
    resumed.send(&format!(
        "{{\"id\":\"c_resub\",\"session_id\":\"{id}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
    ));
    let replay = until(&resumed, "the resumed run", |line| {
        line["kind"] == "fiber_started" && line["payload"]["resumed"] == true
    });
    assert!(
        replay
            .iter()
            .any(|line| line["kind"] == "command_accepted"
                && line["payload"]["command_id"] == "c_resub"),
        "the subscribe was acknowledged: {replay:?}"
    );
    let log = read_log(&setup, &id);
    let started = log
        .iter()
        .find(|line| line["kind"] == "session_started")
        .expect("a session_started line");
    // Kept across a resume (`docs/permissions.md`, "Supervised sessions":
    // "`supervised` is fixed at `start`, recorded on `session_started`,
    // and kept across a resume").
    assert_eq!(started["payload"]["supervised"], true);
    prompt(&resumed, &id, "c_again", "run it again");
    let lines = until(&resumed, "the resumed turn end", |line| {
        line["kind"] == "turn_completed"
    });
    // Complete ordered kinds for the resumed turn (`docs/testing.md`,
    // "Event streams"): only `clients`, `session_status`, the hub's
    // `attention` and `date_changed` are set aside, so a duplicated, missing
    // or reordered event of any other kind fails, ephemeral lines included.
    // `extensions_loaded` replays on the fresh subscribe before the turn's
    // own lines; the prompt's `command_accepted` answers it exactly once
    // (`docs/invocation.md`, "Driver commands") after the prompt starts the
    // turn (`turn_started`, same page) and before the first step runs
    // (`docs/loop.md`, "One step"). The resumed turn denies as a fresh
    // supervised block (`docs/permissions.md`, "Supervised sessions": "A
    // person driving the session does not change it: its escalations stay
    // blocks"), with the closing `Hello.` streaming as two
    // `assistant_message_delta` lines (`docs/loop.md`, "One step";
    // `docs/events.md`, "`assistant_message_delta`").
    assert_one_clients_line(&lines);
    assert_eq!(
        kinds(&lines),
        [
            "extensions_loaded",
            "preamble_built",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ],
        "complete ordered kinds: {lines:?}"
    );
    let resolved: Vec<&Value> = lines
        .iter()
        .filter(|line| line["kind"] == "permission_resolved")
        .collect();
    // One judged call, one block, still decided by the reviewer after the
    // resume (`docs/permissions.md`, "Supervised sessions": kept across a
    // resume; "Its own reviewer judges every call. An escalation is a block
    // at once").
    assert_eq!(resolved.len(), 1, "{lines:?}");
    assert_eq!(resolved[0]["payload"]["decision"], "deny");
    assert_eq!(resolved[0]["payload"]["decided_by"], "reviewer");
    // Still nobody to answer (`docs/permissions.md`, "Supervised sessions":
    // "Nothing waits on a person and no request stays open"; "What the log
    // records": no `permission_requested` where no answer is possible).
    assert!(
        !lines
            .iter()
            .any(|line| line["kind"] == "permission_requested"),
        "the resumed session still asks nobody: {lines:?}"
    );
    // Still no `attention` line (`docs/permissions.md`, "Supervised
    // sessions": "The block is on the session's event stream, and sends no
    // `attention` line.").
    assert!(
        !lines.iter().any(|line| line["kind"] == "attention"),
        "a resumed supervised turn sends no attention: {lines:?}"
    );
    close_and_wait(&resumed, &id);
    stop_hub(&hub, guard);
}
