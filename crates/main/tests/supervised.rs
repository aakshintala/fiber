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
/// when the local date changes. Other kinds are pinned in order, including
/// ephemeral deltas and `steering_queue` (`docs/events.md`,
/// "`steering_queue`", "`assistant_message_delta`"). `command_accepted` is
/// filtered from ordered comparisons and asserted separately: it is an
/// ephemeral acknowledgement sent only to its client, with no `seq`
/// (`docs/invocation.md`, "Driver commands").
/// `support::kinds` supplies the base projection (`support::*`).
fn ordered_kinds_without_command_accepted(lines: &[Value]) -> Vec<&str> {
    kinds(lines)
        .into_iter()
        .filter(|kind| *kind != "command_accepted")
        .collect()
}

/// A driver command is acknowledged once on its own client stream
/// (`docs/invocation.md`, "Driver commands").
fn assert_command_accepted_once(lines: &[Value], command_id: &str) {
    let accepted = lines
        .iter()
        .filter(|line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == command_id
        })
        .count();
    assert_eq!(
        accepted, 1,
        "one acknowledgement for {command_id}: {lines:?}"
    );
}

fn assert_one_clients_line(lines: &[Value]) {
    // `clients` is "written whenever a `full` connection attaches or
    // leaves" (`docs/events.md`, "`clients`"), so the one attach writes
    // one line. The count is pinned against the raw lines before the
    // ordered projection sets it aside.
    let clients = lines
        .iter()
        .filter(|line| line["kind"] == "clients")
        .count();
    assert_eq!(clients, 1, "one attach writes one clients line: {lines:?}");
}

/// Queries `sessions` once and returns the exited row for `id`. The caller
/// waits for [`SessionGuard::wait_gone`] first: the shutdown writes the
/// `recent.jsonl` row before the session process exits, so a gone process
/// proves the row is listed and one query observes it (`docs/testing.md`,
/// "Waits and timeouts": wait for the signal proving the operation, not a
/// poll for settled state).
fn exited_row(client: &Socket, id: &str) -> Value {
    client.send(r#"{"id":"c_sess","command":"sessions"}"#);
    let listing = until(client, "the sessions answer", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_sess"
    });
    let answer = listing.last().expect("the sessions answer");
    let exited = answer["payload"]["result"]["exited"]
        .as_array()
        .expect("the answer names exited sessions");
    exited
        .iter()
        .find(|row| row["session_id"] == id)
        .unwrap_or_else(|| panic!("the exited session is listed: {exited:?}"))
        .clone()
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
    // Complete ordered kinds (`docs/testing.md`, "Event streams"; set aside
    // `clients`, `session_status`, the hub's `attention` and `date_changed`,
    // so every other kind is pinned in order, ephemeral lines included).
    // Order follows `docs/loop.md`, "One step", with `permission_resolved`
    // standing where `tool_call_started` would (`docs/permissions.md`, "What
    // the log records"); deny-then-continue is "Supervised sessions" ("An
    // escalation is a block at once, and the run carries on") and "What
    // happens on a block". The two deltas stream the closing `Hello.` in
    // fragments (`docs/events.md`, "`assistant_message_delta`").
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
    // The exit is proved by the gone process, not by polling the listing:
    // the shutdown wrote the `recent.jsonl` row carrying the last
    // `session_status` before exiting, so `wait_gone` proves the row is
    // listed and one `sessions` query observes it.
    guard.wait_gone();
    let row = exited_row(&control, &id);
    assert_eq!(row["how"], "exited", "{row}");
    assert_eq!(row["status"]["supervised"], true, "{row}");
    // The wait above consumed the guard; only the hub is left to stop.
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
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
    // Complete ordered kinds (the independently triggered lines set aside as
    // above, and `command_accepted` filtered because its position relative to
    // logged lines is not promised). The first `until` ends before the steer,
    // splitting deny from allow; the allow's `permission_resolved` precedes
    // `tool_call_started` (`docs/permissions.md`, "What the log records").
    // The two `steering_queue` updates stay ordered (`docs/events.md`,
    // "`steering_queue`", and `docs/loop.md`, "One step"). The steer's
    // acknowledgement is asserted separately below (`docs/invocation.md`,
    // "Driver commands"). Deltas stream each turn's `Hello.` in fragments.
    assert_one_clients_line(&lines);
    assert_eq!(
        ordered_kinds_without_command_accepted(&lines),
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
    assert_command_accepted_once(&lines, "c_steer");
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
    // Complete ordered kinds (the same four set aside as above). The
    // `ask_user` turn ends `completed` with the questions on `turn_completed`
    // (`docs/tools.md`, "When a program drives the session"); with no
    // streamed text there are no deltas, and the call runs between the
    // completed request and the turn end (`docs/loop.md`, "One step").
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
    assert_command_accepted_once(&replay, "c_resub");
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
    // Complete ordered kinds of the resumed turn (the same independently
    // triggered lines set aside as above, plus `command_accepted` filtered
    // because its position relative to logged lines is not promised). The
    // prompt's acknowledgement is asserted separately below
    // (`docs/invocation.md`, "Driver commands"). `extensions_loaded` replays
    // on the fresh subscribe. The resumed turn denies as a fresh supervised
    // block (`docs/permissions.md`, "Supervised sessions": "A person driving
    // the session does not change it: its escalations stay blocks"); deltas
    // as above.
    assert_eq!(
        ordered_kinds_without_command_accepted(&lines),
        [
            "extensions_loaded",
            "preamble_built",
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
    assert_command_accepted_once(&lines, "c_again");
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
