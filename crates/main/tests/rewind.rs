//! Binary-level tests of `rewind` (`docs/events.md`, "Rewind" and
//! `docs/invocation.md`, "`rewind` starts a new session process"): the
//! built `fiber` runs `hub serve` in its own process group with its own
//! `FIBER_HOME`, holding an ordinary provider whose base URL is the fake
//! server. A rewind closes the old session with `rewound`, the hub starts
//! the named session, and every relayed client follows it there.

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

use contract::SessionId;
use contract::events::{Event, NamedBy, SessionNamed};
use fakes::ProviderServer;
use serde_json::{Value, json};
use support::hello_item_done as hello;
use support::*;

/// A command for `session` on the hub socket, with its own id.
fn command(id: &str, session: Option<&str>, command: &str, args: Value) -> String {
    let mut line = json!({"id": id, "command": command, "args": args});
    if let Some(session) = session {
        line["session_id"] = session.into();
    }
    serde_json::to_string(&line).unwrap()
}

fn subscribe(id: &str, session: &str) -> String {
    command(id, Some(session), "subscribe", json!({"level": "full"}))
}

fn prompt(id: &str, session: &str, text: &str) -> String {
    command(
        id,
        Some(session),
        "prompt",
        json!({"content": [{"type": "text", "text": text}]}),
    )
}

fn rewind(id: &str, session: &str, args: Value) -> String {
    command(id, Some(session), "rewind", args)
}

/// The acknowledgement of command `id`: every line until it, the last
/// accepted.
fn until_ack(client: &Socket, what: &str, id: &str) -> Vec<Value> {
    let lines = until(client, what, |line| {
        line["payload"].get("command_id").and_then(Value::as_str) == Some(id)
    });
    assert_eq!(
        lines.last().unwrap()["kind"],
        "command_accepted",
        "{lines:?}"
    );
    lines
}

/// The rejection of command `id`.
fn until_rejected(client: &Socket, what: &str, id: &str) -> Value {
    let lines = until(client, what, |line| {
        line["kind"] == "command_rejected"
            && line["payload"].get("command_id").and_then(Value::as_str) == Some(id)
    });
    lines.last().unwrap().clone()
}

/// The log of session `id`, wherever its project is.
fn session_log(setup: &Setup, id: &str) -> Vec<Value> {
    fs::read_to_string(session_log_path(setup, id))
        .map(|text| complete_lines(&text))
        .unwrap()
}

fn session_log_path(setup: &Setup, id: &str) -> PathBuf {
    fs::read_dir(setup.home().join("projects"))
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
        .expect("the session's log exists")
}

/// The `seq` of every `turn_started` in `lines`.
fn turn_starts(lines: &[Value]) -> Vec<u64> {
    lines
        .iter()
        .filter(|line| line["kind"] == "turn_started")
        .map(|line| line["seq"].as_u64().unwrap())
        .collect()
}

/// Starts the hub, the session guard and one client: the caller runs the
/// turns.
struct Hubbed {
    hub: Arc<Mutex<Option<HubProc>>>,
    client: Option<Socket>,
    workspace: String,
    guard: SessionGuard,
}

impl Hubbed {
    fn new(setup: &Setup) -> Self {
        let hub = Arc::new(Mutex::new(None));
        let (client, hello) = connect_hub(setup, &hub);
        assert_eq!(hello["kind"], "hub_hello");
        let workspace = setup.workspace().to_string_lossy().into_owned();
        let guard = SessionGuard::arm(setup.deadline, &workspace);
        Self {
            hub,
            client: Some(client),
            workspace,
            guard,
        }
    }

    fn client(&self) -> &Socket {
        self.client.as_ref().expect("the client is connected")
    }

    /// Drops the hub connection: its relays end, so only the feed can
    /// start the next session after this.
    fn disconnect(&mut self) {
        drop(self.client.take());
    }

    /// Starts a session with `content` under command `id`, subscribes
    /// `sub_id` full to it, and waits for its first turn to complete.
    fn start(&self, id: &str, sub_id: &str, content: &str) -> String {
        self.client().send(&command(
            id,
            None,
            "start",
            json!({"workspace": self.workspace, "content": [{"type": "text", "text": content}]}),
        ));
        let ack = until_ack(self.client(), "the start acknowledgement", id);
        let session = ack.last().unwrap()["payload"]["result"]["session_id"]
            .as_str()
            .expect("the start answers with a session id")
            .to_owned();
        self.client().send(&subscribe(sub_id, &session));
        until_ack(self.client(), "the subscribe acknowledgement", sub_id);
        until(self.client(), "the first turn_completed", |line| {
            line["kind"] == "turn_completed" && line["session_id"] == session
        });
        session
    }

    /// Prompts `session` with `text` under command `id`, and waits for the
    /// acknowledgement and the turn to complete.
    fn prompt(&self, id: &str, session: &str, text: &str) {
        self.client().send(&prompt(id, session, text));
        until_ack(self.client(), "the prompt acknowledgement", id);
        until(self.client(), "the turn_completed", |line| {
            line["kind"] == "turn_completed" && line["session_id"] == session
        });
    }

    /// Closes `session` under command `id`, and waits for its `fiber_exited`.
    fn close(&self, id: &str, session: &str) {
        self.client()
            .send(&command(id, Some(session), "close", json!({})));
        until(self.client(), "the fiber_exited", |line| {
            line["kind"] == "fiber_exited" && line["session_id"] == session
        });
    }

    fn finish(self) {
        let Self { hub, guard, .. } = self;
        guard.wait_gone();
        hub.lock()
            .unwrap()
            .take()
            .expect("the hub ran")
            .kill_and_wait();
    }
}

#[test]
fn history_pages_a_rewound_sessions_chain_back_to_the_root() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("c_start", "c_sub", "one");
    hubbed.prompt("c_two", &old, "two");
    let client = hubbed.client();
    client.send(&rewind("c_middle", &old, json!({})));
    let ack = until_ack(client, "the first rewind acknowledgement", "c_middle");
    let middle = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the middle session's extensions", |line| {
        line["session_id"] == middle && line["kind"] == "extensions_loaded"
    });
    hubbed.prompt("c_three", &middle, "three");
    client.send(&rewind("c_next", &middle, json!({})));
    let ack = until_ack(client, "the second rewind acknowledgement", "c_next");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the next session's extensions", |line| {
        line["session_id"] == next && line["kind"] == "extensions_loaded"
    });
    let other = hubbed.start("c_other", "c_other_sub", "unrelated");
    let snapshots = [&old, &middle].map(|id| fs::read(session_log_path(&setup, id)).unwrap());

    client.send(&command("c_history", Some(&next), "history", json!({})));
    let ack = until_ack(client, "the own history page", "c_history");
    let own = ack.last().unwrap()["payload"]["result"]["lines"]
        .as_array()
        .unwrap();
    assert_eq!(own[0]["kind"], "session_started");
    let mut point = own[0]["payload"]["forked_from"].clone();
    for (index, ancestor) in [&middle, &old].into_iter().enumerate() {
        assert_eq!(point["session_id"], *ancestor);
        let bound = point["seq"].as_u64().unwrap();
        let id = format!("c_ancestor_{index}");
        client.send(&command(
            &id,
            Some(&next),
            "history",
            json!({"session_id":ancestor,"to_seq":bound}),
        ));
        let ack = until_ack(client, "the ancestor history page", &id);
        let page = ack.last().unwrap()["payload"]["result"]["lines"]
            .as_array()
            .unwrap();
        let expected: Vec<Value> = session_log(&setup, ancestor)
            .into_iter()
            .take(usize::try_from(bound).unwrap() + 1)
            .collect();
        assert_eq!(page.len(), expected.len());
        for (offset, (actual, expected)) in page.iter().zip(&expected).enumerate() {
            assert_eq!(actual["kind"], expected["kind"], "{ancestor} line {offset}");
            assert!(actual == expected, "{ancestor} line {offset} differs");
        }
        assert_eq!(page[0]["kind"], "session_started");
        assert_eq!(page[0]["seq"], 0);
        point = page[0]["payload"]
            .get("forked_from")
            .cloned()
            .unwrap_or(Value::Null);
    }
    assert!(point.is_null(), "the root has no forked_from");
    client.send(&command(
        "c_off_chain",
        Some(&next),
        "history",
        json!({"session_id":other}),
    ));
    let rejected = until_rejected(client, "the off-chain rejection", "c_off_chain");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    let next_log = session_log(&setup, &next);
    let bound = next_log[0]["payload"]["forked_from"]["seq"]
        .as_u64()
        .unwrap();
    client.send(&command(
        "c_past",
        Some(&next),
        "history",
        json!({"session_id":middle,"from_seq":bound + 1}),
    ));
    let rejected = until_rejected(client, "the continuation-bound rejection", "c_past");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    for (id, snapshot) in [&old, &middle].into_iter().zip(snapshots) {
        let after = fs::read(session_log_path(&setup, id)).unwrap();
        assert_eq!(after.len(), snapshot.len());
        assert!(after == snapshot, "paging must not resume {id}");
    }
    hubbed.close("c_close_next", &next);
    hubbed.close("c_close_other", &other);
    hubbed.finish();
}

#[test]
fn a_rewind_continues_the_session_from_the_latest_turn() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("c1_start", "c1_sub", "one");
    let client = hubbed.client();
    hubbed.prompt("c1_p2", &old, "two");

    client.send(&rewind("c1_rw", &old, json!({})));
    let ack = until_ack(client, "the rewind acknowledgement", "c1_rw");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .expect("the rewind answers with a session id")
        .to_owned();

    // Without sending anything, the client receives the new session's lines.
    until(client, "the new session's start", |line| {
        line["session_id"] == next && line["kind"] == "session_started"
    });

    // The old log ends `turn_completed, rewound`, with no `fiber_exited`.
    let old_log = session_log(&setup, &old);
    let kinds: Vec<&str> = old_log
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.last().unwrap(), &"rewound", "{kinds:?}");
    let rewound = old_log.last().unwrap();
    let starts = turn_starts(&old_log);
    assert_eq!(starts.len(), 2, "{kinds:?}");
    assert_eq!(rewound["payload"]["seq"], starts[1] - 1);
    assert!(rewound["payload"].get("jobs").is_none());
    assert_eq!(rewound["payload"]["new_session_id"], next);
    assert!(
        rewound
            .get("payload")
            .unwrap()
            .get("from_session_id")
            .is_none()
    );

    // The new session starts on its own: its `fiber_started` arrives live.
    until(client, "the new session's fiber_started", |line| {
        line["session_id"] == next && line["kind"] == "fiber_started"
    });
    until(client, "the new session's extensions_loaded", |line| {
        line["session_id"] == next && line["kind"] == "extensions_loaded"
    });
    // The new log before any prompt is one fresh start continuing the old.
    let new_log = session_log(&setup, &next);
    let new_kinds: Vec<&str> = new_log
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        new_kinds,
        ["session_started", "fiber_started", "extensions_loaded"],
        "{new_kinds:?}"
    );
    let first = &new_log[0];
    assert_eq!(
        first["payload"]["forked_from"],
        json!({"session_id": old, "seq": starts[1] - 1})
    );
    assert!(first["payload"]["rewind"].get("jobs").is_none());
    assert!(first["payload"].get("parent").is_none());
    let note = first["payload"]["rewind"]["note"].as_str().unwrap();
    assert!(note.contains("rewound to this point"), "{note}");
    assert!(note.contains("No file was written"), "{note}");
    assert!(
        new_log.iter().all(|line| line["session_id"] == next),
        "no line names the old session"
    );

    // The new session answers, with the old history before the note.
    hubbed.prompt("c1_p3", &next, "three");
    let requests = server.requests();
    assert_eq!(requests.len(), 3, "one request per turn");
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let third: Value = serde_json::from_slice(&requests[2].body).unwrap();
    let second_keys: Vec<&str> = second
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let third_keys: Vec<&str> = third
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(second_keys, third_keys, "the same top-level keys");
    for key in second_keys {
        if key == "input" {
            continue;
        }
        assert_eq!(
            serde_json::to_string(&second[key]).unwrap(),
            serde_json::to_string(&third[key]).unwrap(),
            "the {key} matches byte for byte"
        );
    }
    // The history rides in `input` behind the opening environment message:
    // the old request's four inputs byte for byte, then the note, then the
    // new prompt.
    let before = second["input"].as_array().unwrap();
    let after = third["input"].as_array().unwrap();
    assert_eq!(after.len(), 5, "{after:?}");
    for (index, item) in before.iter().take(3).enumerate() {
        assert_eq!(
            serde_json::to_string(&after[index]).unwrap(),
            serde_json::to_string(item).unwrap(),
            "input[{index}] matches byte for byte"
        );
    }
    assert!(
        serde_json::to_string(&after[3])
            .unwrap()
            .contains("rewound to this point"),
        "input[3] is the note: {}",
        after[3]
    );
    assert!(
        serde_json::to_string(&after[4]).unwrap().contains("three"),
        "input[4] is the new prompt: {}",
        after[4]
    );

    hubbed.close("c1_close", &next);
    hubbed.finish();
}

#[test]
fn a_rewind_stops_a_running_job_and_the_note_names_it() {
    let setup = Setup::new();
    let sleep = function_call(
        "call_1",
        "shell",
        &json!({"command": "sleep 600", "run_in_background": true}),
    );
    let server = ProviderServer::start([stream(&[sleep]), hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    // A standing rule allows the command without asking.
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "sleep"})
        ),
    )
    .unwrap();
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("c3_start", "c3_sub", "one");
    let client = hubbed.client();
    // The background shell started during the first turn: its
    // `job_started` is in the old log.
    let started_log = session_log(&setup, &old);
    let job = started_log
        .iter()
        .find(|line| line["kind"] == "job_started")
        .expect("the turn started a job")["payload"]["job_id"]
        .as_str()
        .expect("the start names the job")
        .to_owned();
    client.send(&rewind("c3_rw", &old, json!({})));
    let ack = until_ack(client, "the rewind acknowledgement", "c3_rw");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .expect("the rewind answers with a session id")
        .to_owned();
    // The old log ends with the stop, then `rewound`.
    let old_log = session_log(&setup, &old);
    let kinds: Vec<&str> = old_log
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    let tail = &kinds[kinds.len() - 2..];
    assert_eq!(tail, ["job_completed", "rewound"], "{kinds:?}");
    let completed = &old_log[old_log.len() - 2];
    assert_eq!(completed["payload"]["job_id"], json!(job));
    assert_eq!(completed["payload"]["status"], json!("cancelled"));
    // The new session's note names the stopped job.
    until(client, "the new session's start", |line| {
        line["session_id"] == next && line["kind"] == "session_started"
    });
    let new_log = session_log(&setup, &next);
    let note = new_log[0]["payload"]["rewind"]["note"]
        .as_str()
        .expect("the start carries the note");
    assert!(note.contains(&job), "{note}");
    assert!(note.contains("Jobs that were running"), "{note}");
    // The new session's first request carries the note.
    hubbed.prompt("c3_p3", &next, "three");
    let requests = server.requests();
    let last: Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
    let input = last["input"].as_array().unwrap();
    assert!(
        input
            .iter()
            .any(|item| serde_json::to_string(item).unwrap().contains(&job)),
        "the first request carries the note: {last:?}"
    );
    hubbed.close("c3_close", &next);
    hubbed.finish();
}

#[test]
fn a_summary_crosses_the_process_boundary_between_history_and_note() {
    let setup = Setup::new();
    let summary = "Tried the cache fix and reverted it.";
    let reply = stream(&[
        json!({"type":"response.output_item.done", "item":{"type":"message", "content":[{"type":"output_text", "text":summary}]}}),
    ]);
    let server = ProviderServer::start([hello(), hello(), reply, hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("sum_start", "sum_sub", "one");
    hubbed.prompt("sum_two", &old, "two");
    let before = session_log(&setup, &old).len();
    let client = hubbed.client();
    client.send(&rewind("sum_rw", &old, json!({"summarise":true})));
    let ack = until_ack(client, "the summary rewind acknowledgement", "sum_rw");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the summary session's extensions", |line| {
        line["session_id"] == next && line["kind"] == "extensions_loaded"
    });
    let old_log = session_log(&setup, &old);
    let tail = &old_log[before..];
    let kinds: Vec<_> = tail
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "rewound"
        ]
    );
    assert_eq!(
        tail[0]["payload"]["input"],
        json!([{"type":"rewind", "command_id":"sum_rw"}])
    );
    assert_eq!(tail[6]["payload"]["outcome"], "completed");
    assert_eq!(tail[7]["payload"]["summary"], summary);
    let new_log = session_log(&setup, &next);
    assert_eq!(new_log[0]["payload"]["rewind"]["summary"], summary);
    assert_eq!(
        new_log
            .iter()
            .filter(|line| line["kind"] == "turn_started")
            .count(),
        0
    );
    hubbed.prompt("sum_three", &next, "three");
    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let next_request: Value = serde_json::from_slice(&requests[3].body).unwrap();
    let before_input = second["input"].as_array().unwrap();
    let input = next_request["input"].as_array().unwrap();
    assert_eq!(input.len(), 6);
    assert_eq!(&input[..3], &before_input[..3]);
    assert_eq!(input[3], json!({"role":"user", "content":summary}));
    let note = new_log[0]["payload"]["rewind"]["note"].as_str().unwrap();
    assert_eq!(input[4], json!({"role":"user", "content":note}));
    assert_eq!(input[5], json!({"role":"user", "content":"three"}));
    hubbed.close("sum_close", &next);
    hubbed.finish();
}

#[test]
fn a_failed_summary_creates_no_session_and_the_old_conversation_continues() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        hello(),
        hello(),
        fakes::Response::status(404, "{}"),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("fail_start", "fail_sub", "one");
    hubbed.prompt("fail_two", &old, "two");
    let before = session_log(&setup, &old).len();
    let directory = session_log_path(&setup, &old)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let sessions: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    hubbed
        .client()
        .send(&rewind("fail_rw", &old, json!({"summarise":true})));
    let rejected = until_rejected(hubbed.client(), "the failed summary", "fail_rw");
    assert_eq!(rejected["payload"]["code"], "summary_failed");
    assert!(
        rejected["payload"]["message"]
            .as_str()
            .unwrap()
            .starts_with("The summary could not be made: ")
    );
    assert_eq!(
        fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        sessions
    );
    let old_log = session_log(&setup, &old);
    let tail = &old_log[before..];
    let kinds: Vec<_> = tail
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed"
        ]
    );
    assert_eq!(tail[5]["payload"]["outcome"], "failed");
    assert_eq!(tail[5]["payload"]["error"]["code"], "invalid_request");
    assert!(old_log.iter().all(|line| line["kind"] != "rewound"));
    hubbed.prompt("fail_three", &old, "three");
    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let next: Value = serde_json::from_slice(&requests[3].body).unwrap();
    let mut expected = second["input"].as_array().unwrap().clone();
    expected.push(json!({"type":"message", "content":[{"type":"output_text", "text":"Hello."}]}));
    expected.push(json!({"role":"user", "content":"three"}));
    assert_eq!(next["input"], json!(expected));
    hubbed.close("fail_close", &old);
    hubbed.finish();
}

#[test]
fn refusals_leave_the_session_running_and_its_log_unchanged() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("c2_start", "c2_sub", "one");
    let client = hubbed.client();
    let other = hubbed.start("c2_start_other", "c2_sub_other", "other");
    let before = fs::read(session_log_path(&setup, &old)).unwrap();

    // Before the first line's turn: not a step boundary.
    client.send(&rewind("c2_rw_seq", &old, json!({"seq": 0})));
    let refused = until_rejected(client, "the seq refusal", "c2_rw_seq");
    assert_eq!(refused["payload"]["code"], "not_step_boundary");
    assert_eq!(
        fs::read(session_log_path(&setup, &old)).unwrap(),
        before,
        "a refused rewind writes nothing"
    );

    // A session off the chain rewinds through the hub.
    client.send(&rewind(
        "c2_rw_from",
        &old,
        json!({"from_session_id": other}),
    ));
    let refused = until_rejected(client, "the from refusal", "c2_rw_from");
    assert_eq!(refused["payload"]["code"], "invalid_arguments");
    assert!(
        refused["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("through the hub"),
        "{}",
        refused["payload"]["message"]
    );
    assert_eq!(
        fs::read(session_log_path(&setup, &old)).unwrap(),
        before,
        "a refused rewind writes nothing"
    );

    // The session is still running: its next prompt turns.
    hubbed.prompt("c2_p2", &old, "two");
    hubbed.close("c2_close_old", &old);
    hubbed.close("c2_close_other", &other);
    hubbed.finish();
}

#[test]
fn a_rewind_through_the_hub_resumes_an_exited_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("c3_start", "c3_sub", "one");
    let client = hubbed.client();
    hubbed.close("c3_close", &old);
    fakes::try_matching_exits(&hubbed.workspace, setup.deadline).unwrap_or_else(|err| {
        panic!("waited until the deadline for the old session process to exit: {err}")
    });
    assert!(
        !setup.session_socket(&old).exists(),
        "the closed session unlinked its socket"
    );

    // No process holds the session: the hub resumes it, and it rewinds.
    client.send(&rewind("c3_rw", &old, json!({})));
    let ack = until_ack(client, "the rewind acknowledgement", "c3_rw");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the new session's start", |line| {
        line["session_id"] == next && line["kind"] == "session_started"
    });
    until(client, "the new session's fiber_started", |line| {
        line["session_id"] == next && line["kind"] == "fiber_started"
    });
    let new_log = session_log(&setup, &next);
    assert_eq!(new_log[0]["kind"], "session_started");
    assert_eq!(new_log[0]["payload"]["forked_from"]["session_id"], old);

    hubbed.close("c3_close_next", &next);
    hubbed.finish();
}

#[test]
fn the_feed_starts_a_rewind_no_client_relayed() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let mut hubbed = Hubbed::new(&setup);
    let old = hubbed.start("c4_start", "c4_sub", "one");
    hubbed.prompt("c4_p2", &old, "two");

    // A second hub client holds a feed subscription only: it relays no
    // session, so only the feed can start the next session. Its status
    // for the old session proves the feed follows it.
    let hub = Arc::clone(&hubbed.hub);
    let (watcher, _) = connect_hub(&setup, &hub);
    watcher.send(&command("c4_watch", None, "feed", json!({})));
    until(&watcher, "the feed following the session", |line| {
        line["kind"] == "session_status" && line["session_id"] == old
    });
    hubbed.disconnect();

    // A client on the session's own socket, not the hub's, rewinds it.
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&old));
    direct.send(&command(
        "d4_sub",
        None,
        "subscribe",
        json!({"level": "full"}),
    ));
    until(&direct, "the direct subscribe acknowledgement", |line| {
        line["payload"].get("command_id").and_then(Value::as_str) == Some("d4_sub")
    });
    direct.send(&command("d4_rw", None, "rewind", json!({})));
    let ack = until(&direct, "the direct rewind acknowledgement", |line| {
        line["payload"].get("command_id").and_then(Value::as_str) == Some("d4_rw")
    });
    assert_eq!(ack.last().unwrap()["kind"], "command_accepted");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    drop(direct);

    // The feed sees the old session leave, then starts the named session:
    // its status arrives, though no hub client relayed the rewind.
    until(&watcher, "the old session's leaving", |line| {
        line["kind"] == "session_left" && line["payload"]["session_id"] == old
    });
    until(&watcher, "the new session's status", |line| {
        line["kind"] == "session_status" && line["session_id"] == next
    });
    let new_log = session_log(&setup, &next);
    assert_eq!(
        new_log[0]["payload"]["forked_from"]["session_id"], old,
        "the feed started the rewind's session"
    );

    // A client on the new session's own socket closes it: the feed sees
    // that too.
    let direct_next = Socket::connect(setup.deadline, &setup.session_socket(&next));
    direct_next.send(&command(
        "d4_sub",
        None,
        "subscribe",
        json!({"level": "full"}),
    ));
    until(
        &direct_next,
        "the direct subscribe acknowledgement",
        |line| line["payload"].get("command_id").and_then(Value::as_str) == Some("d4_sub"),
    );
    direct_next.send(&command("d4_close", None, "close", json!({})));
    until(&direct_next, "the direct close acknowledgement", |line| {
        line["payload"].get("command_id").and_then(Value::as_str) == Some("d4_close")
    });
    drop(direct_next);
    until(&watcher, "the new session's leaving", |line| {
        line["kind"] == "session_left" && line["payload"]["session_id"] == next
    });
    drop(watcher);
    hubbed.finish();
}

#[test]
fn an_unprompted_rewound_session_is_kept_and_resumes() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("c5_start", "c5_sub", "one");
    let client = hubbed.client();
    hubbed.prompt("c5_p2", &old, "two");
    client.send(&rewind("c5_rw", &old, json!({})));
    let ack = until_ack(client, "the rewind acknowledgement", "c5_rw");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the new session's start", |line| {
        line["session_id"] == next && line["kind"] == "session_started"
    });

    // Never prompted: closing it still keeps its directory.
    hubbed.close("c5_close", &next);
    let dir = session_log_path(&setup, &next).parent().unwrap().to_owned();
    assert!(dir.is_dir(), "the rewound session is kept");
    // The prompt must reach a gone session, not one still shutting down:
    // its socket accepts until the process releases it.
    fakes::try_matching_exits(&hubbed.workspace, setup.deadline).unwrap_or_else(|err| {
        panic!("waited until the deadline for the closed session process to exit: {err}")
    });

    // A prompt through the hub resumes it like any exited session.
    hubbed.prompt("c5_p3", &next, "three");
    hubbed.close("c5_close_again", &next);
    hubbed.finish();
}

#[test]
fn a_rewound_session_rewinds_to_an_ancestor_point() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("c6_start", "c6_sub", "one");
    let client = hubbed.client();
    hubbed.prompt("c6_p2", &old, "two");
    client.send(&rewind("c6_rw", &old, json!({})));
    let ack = until_ack(client, "the rewind acknowledgement", "c6_rw");
    let middle = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the middle session's start", |line| {
        line["session_id"] == middle && line["kind"] == "session_started"
    });
    until(client, "the middle session's fiber_started", |line| {
        line["session_id"] == middle && line["kind"] == "fiber_started"
    });
    hubbed.prompt("c6_p3", &middle, "three");

    // The ancestor's first turn boundary, through the middle session.
    let old_log = session_log(&setup, &old);
    let point = turn_starts(&old_log)[0] - 1;
    client.send(&rewind(
        "c6_rw_again",
        &middle,
        json!({"from_session_id": old, "seq": point}),
    ));
    let ack = until_ack(client, "the second rewind acknowledgement", "c6_rw_again");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the third session's start", |line| {
        line["session_id"] == next && line["kind"] == "session_started"
    });
    until(client, "the third session's fiber_started", |line| {
        line["session_id"] == next && line["kind"] == "fiber_started"
    });
    let new_log = session_log(&setup, &next);
    assert_eq!(
        new_log[0]["payload"]["forked_from"],
        json!({"session_id": old, "seq": point}),
        "the new session continues the ancestor"
    );

    // Its first request replays the ancestor's first inputs, then the note.
    hubbed.prompt("c6_p4", &next, "four");
    // The point is before the ancestor turn's own input, which rides in
    // its `turn_started`: the new session replays only what the point
    // holds, then the note, then the new prompt.
    let requests = server.requests();
    assert_eq!(requests.len(), 4, "one request per turn");
    let first: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let last: Value = serde_json::from_slice(&requests[3].body).unwrap();
    let first_input = first["input"].as_array().unwrap();
    let input = last["input"].as_array().unwrap();
    assert_eq!(input.len(), 3, "{input:?}");
    assert_eq!(
        serde_json::to_string(&input[0]).unwrap(),
        serde_json::to_string(&first_input[0]).unwrap(),
        "the ancestor's input at the point"
    );
    assert!(
        serde_json::to_string(&input[1])
            .unwrap()
            .contains("rewound to this point"),
        "input[1] is the note: {}",
        input[1]
    );
    assert!(
        serde_json::to_string(&input[2]).unwrap().contains("four"),
        "input[2] is the new prompt: {}",
        input[2]
    );

    hubbed.close("c6_close_old", &old);
    hubbed.close("c6_close_middle", &middle);
    hubbed.close("c6_close_next", &next);
    hubbed.finish();
}

/// The `session_named` lines in `log`, in order.
fn named_lines(log: &[Value]) -> Vec<&Value> {
    log.iter()
        .filter(|line| line["kind"] == "session_named")
        .collect()
}

/// The `name` cell `fiber sessions --json` prints for `id`
/// (`docs/invocation.md`, "`sessions [--all]`"): the session's latest
/// name, live from the hub.
fn listed_name(setup: &Setup, id: &str) -> String {
    let command = setup.fiber(&["sessions", "--json", "--all"]);
    let output = run_to_exit(setup.deadline, "fiber sessions --json --all", command);
    assert_eq!(
        output.status.code(),
        Some(0),
        "fiber sessions --json --all: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    for line in stdout.lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        if row["id"].as_str() == Some(id) {
            return row["name"].as_str().unwrap().to_owned();
        }
    }
    panic!("fiber sessions --json --all lists {id}: {stdout}");
}

#[test]
fn a_rewind_keeps_the_person_name_and_its_pin() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        hello(),
        stream(&[function_call(
            "n1",
            "name_session",
            &json!({"name": "Theirs"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("n1_start", "n1_sub", "one");
    let client = hubbed.client();
    hubbed.close("n1_close_old", &old);
    fakes::try_matching_exits(&hubbed.workspace, setup.deadline).unwrap_or_else(|err| {
        panic!("waited until the deadline for the old session process to exit: {err}")
    });

    // The person names the closed session, as `tests/name_session.rs`
    // does: the session is closed and no process holds it, so the test
    // appends to its log directly.
    {
        let dir = session_log_path(&setup, &old);
        let sessions = dir.parent().unwrap().parent().unwrap();
        let log = log::Log::open(sessions, SessionId(old.clone()), Arc::new(SystemClock)).unwrap();
        log.append(
            &Event::SessionNamed(SessionNamed {
                name: Some("Mine".into()),
                by: NamedBy::Person,
            }),
            None,
            None,
        )
        .unwrap();
    }

    // No process holds the session: the hub resumes it, and it rewinds.
    client.send(&rewind("n1_rw", &old, json!({})));
    let ack = until_ack(client, "the rewind acknowledgement", "n1_rw");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .expect("the rewind answers with a session id")
        .to_owned();
    until(client, "the new session's start", |line| {
        line["session_id"] == next && line["kind"] == "session_started"
    });
    until(client, "the new session's fiber_started", |line| {
        line["session_id"] == next && line["kind"] == "fiber_started"
    });
    until(client, "the new session's extensions_loaded", |line| {
        line["session_id"] == next && line["kind"] == "extensions_loaded"
    });

    // The name rides with the conversation: `session_named` right after
    // `session_started`, unchanged.
    let new_log = session_log(&setup, &next);
    let new_kinds: Vec<&str> = new_log
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        new_kinds,
        [
            "session_started",
            "session_named",
            "fiber_started",
            "extensions_loaded"
        ],
        "{new_kinds:?}"
    );
    assert_eq!(
        new_log[1]["payload"],
        json!({"name": "Mine", "by": "person"})
    );
    // The listing follows the feed's `run/` scan: wait for the new
    // session's feed entry before listing it, as the feed test does.
    let (watcher, _) = connect_hub(&setup, &hubbed.hub);
    watcher.send(&command("n1_watch", None, "feed", json!({})));
    until(&watcher, "the new session's feed entry", |line| {
        line["kind"] == "session_status" && line["session_id"] == next
    });
    assert_eq!(listed_name(&setup, &next), "Mine");

    // The pin holds in the new session: its model's `name_session` fails
    // `name_pinned` and writes no further name.
    hubbed.prompt("n1_p2", &next, "two");
    let after = session_log(&setup, &next);
    let named = named_lines(&after);
    assert_eq!(named.len(), 1, "{after:?}");
    assert_eq!(named[0]["payload"], json!({"name": "Mine", "by": "person"}));
    let completed: Vec<&Value> = after
        .iter()
        .filter(|line| line["kind"] == "tool_call_completed")
        .collect();
    let last = completed.last().unwrap();
    assert_eq!(last["payload"]["status"], "failed");
    assert_eq!(last["payload"]["error"]["code"], "name_pinned");
    assert_eq!(listed_name(&setup, &next), "Mine");

    hubbed.close("n1_close", &next);
    hubbed.finish();
}

#[test]
fn a_rewind_to_an_ancestor_point_keeps_the_rewound_sessions_person_name() {
    // The name is the rewound session's, not the point's session's: the
    // middle session is person-named "Mine" while the ancestor never
    // is, and rewinding the middle to an ancestor point still carries
    // "Mine" with its pin.
    let setup = Setup::new();
    let server = ProviderServer::start([
        hello(),
        hello(),
        hello(),
        stream(&[function_call(
            "a1",
            "name_session",
            &json!({"name": "Theirs"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("a1_start", "a1_sub", "one");
    let client = hubbed.client();
    hubbed.prompt("a1_p2", &old, "two");
    client.send(&rewind("a1_rw", &old, json!({})));
    let ack = until_ack(client, "the rewind acknowledgement", "a1_rw");
    let middle = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the middle session's start", |line| {
        line["session_id"] == middle && line["kind"] == "session_started"
    });
    until(client, "the middle session's fiber_started", |line| {
        line["session_id"] == middle && line["kind"] == "fiber_started"
    });
    hubbed.prompt("a1_p3", &middle, "three");
    hubbed.close("a1_close_middle", &middle);
    fakes::try_matching_exits(&hubbed.workspace, setup.deadline).unwrap_or_else(|err| {
        panic!("waited until the deadline for the middle session process to exit: {err}")
    });

    // The person names the closed middle session, as `tests/name_session.rs`
    // does; the ancestor stays unnamed.
    {
        let dir = session_log_path(&setup, &middle);
        let sessions = dir.parent().unwrap().parent().unwrap();
        let log =
            log::Log::open(sessions, SessionId(middle.clone()), Arc::new(SystemClock)).unwrap();
        log.append(
            &Event::SessionNamed(SessionNamed {
                name: Some("Mine".into()),
                by: NamedBy::Person,
            }),
            None,
            None,
        )
        .unwrap();
    }

    // No process holds the middle session: the hub resumes it, and it
    // rewinds to the ancestor's first turn boundary.
    let old_log = session_log(&setup, &old);
    let point = turn_starts(&old_log)[0] - 1;
    client.send(&rewind(
        "a1_rw_again",
        &middle,
        json!({"from_session_id": old, "seq": point}),
    ));
    let ack = until_ack(client, "the second rewind acknowledgement", "a1_rw_again");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the third session's start", |line| {
        line["session_id"] == next && line["kind"] == "session_started"
    });
    until(client, "the third session's fiber_started", |line| {
        line["session_id"] == next && line["kind"] == "fiber_started"
    });
    until(client, "the third session's extensions_loaded", |line| {
        line["session_id"] == next && line["kind"] == "extensions_loaded"
    });

    // The rewound session's name rides with the conversation, not the
    // ancestor's: `session_named` right after `session_started`.
    let new_log = session_log(&setup, &next);
    let new_kinds: Vec<&str> = new_log
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        new_kinds,
        [
            "session_started",
            "session_named",
            "fiber_started",
            "extensions_loaded"
        ],
        "{new_kinds:?}"
    );
    assert_eq!(
        new_log[1]["payload"],
        json!({"name": "Mine", "by": "person"})
    );

    // The pin holds in the new session: its model's `name_session` fails
    // `name_pinned` and writes no further name.
    hubbed.prompt("a1_p4", &next, "four");
    let after = session_log(&setup, &next);
    let named = named_lines(&after);
    assert_eq!(named.len(), 1, "{after:?}");
    assert_eq!(named[0]["payload"], json!({"name": "Mine", "by": "person"}));
    let completed: Vec<&Value> = after
        .iter()
        .filter(|line| line["kind"] == "tool_call_completed")
        .collect();
    let last = completed.last().unwrap();
    assert_eq!(last["payload"]["status"], "failed");
    assert_eq!(last["payload"]["error"]["code"], "name_pinned");

    hubbed.close("a1_close_old", &old);
    hubbed.close("a1_close_next", &next);
    hubbed.finish();
}

#[test]
fn a_rewind_carries_the_model_name_and_the_new_session_renames_it() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[function_call(
            "m1",
            "name_session",
            &json!({"name": "Model name"}),
        )]),
        hello(),
        stream(&[function_call(
            "m2",
            "name_session",
            &json!({"name": "New name"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("m1_start", "m1_sub", "one");
    let client = hubbed.client();
    let old_log = session_log(&setup, &old);
    let old_named = named_lines(&old_log);
    assert_eq!(old_named.len(), 1, "{old_log:?}");
    assert_eq!(
        old_named[0]["payload"],
        json!({"name": "Model name", "by": "model"})
    );

    client.send(&rewind("m1_rw", &old, json!({})));
    let ack = until_ack(client, "the rewind acknowledgement", "m1_rw");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .expect("the rewind answers with a session id")
        .to_owned();
    until(client, "the new session's start", |line| {
        line["session_id"] == next && line["kind"] == "session_started"
    });
    until(client, "the new session's fiber_started", |line| {
        line["session_id"] == next && line["kind"] == "fiber_started"
    });
    until(client, "the new session's extensions_loaded", |line| {
        line["session_id"] == next && line["kind"] == "extensions_loaded"
    });

    let new_log = session_log(&setup, &next);
    let new_kinds: Vec<&str> = new_log
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        new_kinds,
        [
            "session_started",
            "session_named",
            "fiber_started",
            "extensions_loaded"
        ],
        "{new_kinds:?}"
    );
    assert_eq!(
        new_log[1]["payload"],
        json!({"name": "Model name", "by": "model"})
    );
    assert_eq!(
        log::session_name(&setup.home(), &SessionId(next.clone())),
        Some("Model name".to_owned())
    );

    // No pin holds against a model's name: its next `name_session` renames it.
    hubbed.prompt("m1_p2", &next, "two");
    let after = session_log(&setup, &next);
    let named = named_lines(&after);
    assert_eq!(named.len(), 2, "{after:?}");
    assert_eq!(
        named[1]["payload"],
        json!({"name": "New name", "by": "model"})
    );
    assert_eq!(
        log::session_name(&setup.home(), &SessionId(next.clone())),
        Some("New name".to_owned())
    );

    hubbed.close("m1_close", &next);
    hubbed.finish();
}

/// Runs the system `git` in `dir` with a clean environment, in its own
/// process group, to its exit under the test's [`Deadline`].
fn git_clean_env(deadline: Deadline, dir: &Path, args: &[&str]) -> String {
    use std::os::unix::process::CommandExt as _;
    let mut command = std::process::Command::new("git");
    command
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);
    let out = run_to_exit(deadline, &format!("git {args:?}"), command);
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A repository with one commit on `main` at `dir`.
fn init_repo(deadline: Deadline, dir: &Path) {
    git_clean_env(deadline, dir, &["init", "--quiet"]);
    fs::write(dir.join("file.txt"), "x").unwrap();
    git_clean_env(deadline, dir, &["add", "."]);
    git_clean_env(deadline, dir, &["commit", "--quiet", "-m", "first"]);
}

#[test]
fn a_rewind_keeps_the_worktree_for_the_next_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    init_repo(setup.deadline, &setup.workspace());
    let repo = setup.workspace().to_string_lossy().into_owned();
    let old = "s_7111111111111111";

    // The session runs in a new worktree, as `ask --worktree` does.
    let mut session = setup.fiber(&["session", "--id", old, "--workspace", &repo, "--worktree"]);
    session.current_dir(setup.workspace());
    let mut child = session.spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let first: String = support::bounded(setup.deadline, "the session's first line", move || {
        use std::io::BufRead;
        let mut read = std::io::BufReader::new(stdout);
        let mut line = String::new();
        read.read_line(&mut line).unwrap();
        line
    });
    let started: Value = serde_json::from_str(first.trim_end()).unwrap();
    assert_eq!(started["kind"], "session_started", "{started}");
    let worktree = started["payload"]["worktree"]["path"]
        .as_str()
        .expect("the session runs in a worktree")
        .to_owned();
    let branch = started["payload"]["worktree"]["branch"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        started["payload"]["workspace"].as_str().unwrap(),
        worktree,
        "the worktree is the workspace"
    );
    let sessions_guard = SessionGuard::arm(setup.deadline, &worktree);

    let hubbed = Hubbed::new(&setup);
    let client = hubbed.client();
    client.send(&subscribe("c7_sub", old));
    until_ack(client, "the subscribe acknowledgement", "c7_sub");
    hubbed.prompt("c7_p1", old, "hi");
    client.send(&rewind("c7_rw", old, json!({})));
    let ack = until_ack(client, "the rewind acknowledgement", "c7_rw");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until(client, "the new session's start", |line| {
        line["session_id"] == next && line["kind"] == "session_started"
    });
    until(client, "the new session's fiber_started", |line| {
        line["session_id"] == next && line["kind"] == "fiber_started"
    });

    // The old process is gone, and the worktree is still there.
    fakes::try_matching_exits(&repo, setup.deadline).unwrap_or_else(|err| {
        panic!("waited until the deadline for the old session process to exit: {err}")
    });
    support::bounded(setup.deadline, "the old session to be reaped", move || {
        child.wait().unwrap();
    });
    assert!(Path::new(&worktree).is_dir(), "the worktree is kept");
    assert!(
        !git_clean_env(
            setup.deadline,
            &setup.workspace(),
            &["branch", "--list", &branch]
        )
        .is_empty(),
        "its branch is kept"
    );

    // The new session records the same workspace and worktree, and runs.
    let new_log = session_log(&setup, &next);
    assert_eq!(new_log[0]["payload"]["workspace"], worktree);
    assert_eq!(new_log[0]["payload"]["worktree"]["path"], worktree);
    assert_eq!(new_log[0]["payload"]["worktree"]["branch"], branch);
    drop(Socket::connect(
        setup.deadline,
        &setup.session_socket(&next),
    ));
    // Already subscribed at `full` through the redirect: the new session's
    // status arrives with the worktree it runs in.
    let status = until(client, "the new session's status", |line| {
        line["kind"] == "session_status" && line["session_id"] == next
    });
    assert_eq!(
        status.last().unwrap()["payload"]["workspace"],
        worktree,
        "the new session runs in the worktree"
    );
    // The continuing process shares the worktree's lock for its life:
    // the lock file exists, and an exclusive take waits behind it.
    let lock = Path::new(&worktree).with_file_name(format!(
        "{}.lock",
        Path::new(&worktree).file_name().unwrap().to_string_lossy()
    ));
    assert!(lock.is_file(), "the continuation holds the lock file");
    let probe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock)
        .unwrap();
    assert!(
        matches!(probe.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "the continuation holds its share"
    );
    drop(probe);

    // Closing the new session never removes the worktree either.
    hubbed.close("c7_close", &next);
    assert!(Path::new(&worktree).is_dir(), "the worktree is still there");
    assert!(
        !git_clean_env(
            setup.deadline,
            &setup.workspace(),
            &["branch", "--list", &branch]
        )
        .is_empty(),
        "its branch is still there"
    );
    sessions_guard.wait_gone();
    hubbed.finish();
}

#[test]
fn a_rewind_inherits_a_handoff_by_position() {
    // A handoff completed in the old session applies to a rewind at or
    // past it, and not to one before it (`docs/events.md`, "Rewind"). The
    // handoff is the person's: turn 2 runs, the `handoff` command carries
    // on, and turn 3 answers from the new context.
    let setup = Setup::new();
    let server = ProviderServer::start([
        hello(),
        hello(),
        text_reply("The note."),
        hello(),
        hello(),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup);
    let old = hubbed.start("h1_start", "h1_sub", "one");
    let client = hubbed.client();
    hubbed.prompt("h1_p2", &old, "two");

    client.send(&command("h1_hand", Some(&old), "handoff", json!({})));
    until_ack(client, "the handoff acknowledgement", "h1_hand");
    until(client, "the handoff turn_completed", |line| {
        line["kind"] == "turn_completed" && line["session_id"] == old
    });
    hubbed.prompt("h1_p3", &old, "three");

    // The old log holds the completed handoff in turn 2, and turn 3 runs
    // from its context.
    let old_log = session_log(&setup, &old);
    let kinds: Vec<&str> = old_log
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    let completed = kinds
        .iter()
        .position(|kind| *kind == "handoff_completed")
        .unwrap();
    assert_eq!(old_log[completed]["payload"]["outcome"], "completed");
    assert_eq!(kinds[completed + 1], "opening_message");
    let starts = turn_starts(&old_log);
    assert!(starts.len() >= 3, "{kinds:?}");
    let requests = server.requests();
    assert_eq!(requests.len(), 4, "two turns, the note, the third turn");
    let third: Value = serde_json::from_slice(&requests[3].body).unwrap();
    let input = third["input"].as_array().unwrap();
    // The handoff turn began as the command and held no message from the
    // person, so the new context is the opening message and the note.
    assert_eq!(input.len(), 3, "{input:?}");
    assert!(
        input[1].to_string().contains("The note."),
        "the handoff note: {input:?}"
    );
    assert!(
        input[2].to_string().contains("three"),
        "the new prompt: {input:?}"
    );

    // Past the handoff, at the default point: the new session's next
    // request carries the handoff's context, then the note, then the
    // prompt, with the old request's other keys.
    client.send(&rewind("h1_rw_after", &old, json!({})));
    let ack = until_ack(client, "the after rewind acknowledgement", "h1_rw_after");
    let after = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .expect("the rewind answers with a session id")
        .to_owned();
    until(client, "the after session's start", |line| {
        line["session_id"] == after && line["kind"] == "session_started"
    });
    hubbed.prompt("h1_p4", &after, "four");
    let requests = server.requests();
    assert_eq!(requests.len(), 5);
    let past: Value = serde_json::from_slice(&requests[4].body).unwrap();
    for key in third
        .as_object()
        .unwrap()
        .keys()
        .filter(|key| key.as_str() != "input")
    {
        assert_eq!(
            serde_json::to_string(&past[key]).unwrap(),
            serde_json::to_string(&third[key]).unwrap(),
            "the {key} matches byte for byte"
        );
    }
    let past_input = past["input"].as_array().unwrap();
    // Turn 3's prompt is past the point: the shared prefix is the handoff's
    // context, then the rewind note, then the new prompt.
    let k = input.len() - 1;
    assert_eq!(past_input.len(), k + 2, "{past_input:?}");
    for (index, item) in input[..k].iter().enumerate() {
        assert_eq!(
            serde_json::to_string(&past_input[index]).unwrap(),
            serde_json::to_string(item).unwrap(),
            "input[{index}] matches byte for byte"
        );
    }
    assert!(
        past_input[k].to_string().contains("rewound to this point"),
        "input[{k}] is the note: {}",
        past_input[k]
    );
    assert!(
        past_input[k + 1].to_string().contains("four"),
        "input[{k}] is the new prompt: {}",
        past_input[k + 1]
    );

    // Before the handoff, at turn 2's start minus one: no handoff note
    // reaches the new session.
    client.send(&rewind(
        "h1_rw_before",
        &old,
        json!({"from_session_id": old, "seq": starts[1] - 1}),
    ));
    let ack = until_ack(client, "the before rewind acknowledgement", "h1_rw_before");
    let before = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .expect("the rewind answers with a session id")
        .to_owned();
    until(client, "the before session's start", |line| {
        line["session_id"] == before && line["kind"] == "session_started"
    });
    hubbed.prompt("h1_p5", &before, "five");
    let requests = server.requests();
    assert_eq!(requests.len(), 6);
    let earlier: Value = serde_json::from_slice(&requests[5].body).unwrap();
    let earlier_input = earlier["input"].as_array().unwrap();
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let second_input = second["input"].as_array().unwrap();
    // Turn 2's prompt is past the point: the shared prefix is the history
    // before it, then the rewind note, then the new prompt.
    let k = second_input.len() - 1;
    assert_eq!(earlier_input.len(), k + 2, "{earlier_input:?}");
    for (index, item) in second_input[..k].iter().enumerate() {
        assert_eq!(
            serde_json::to_string(&earlier_input[index]).unwrap(),
            serde_json::to_string(item).unwrap(),
            "input[{index}] matches byte for byte"
        );
    }
    assert!(
        earlier_input
            .iter()
            .all(|item| !item.to_string().contains("The note.")),
        "no handoff note reaches the rewound session: {earlier_input:?}"
    );
    assert!(
        earlier_input[k]
            .to_string()
            .contains("rewound to this point"),
        "the note follows the history: {}",
        earlier_input[k]
    );
    assert!(
        earlier_input[k + 1].to_string().contains("five"),
        "the new prompt is last: {}",
        earlier_input[k + 1]
    );

    hubbed.close("h1_close_after", &after);
    hubbed.close("h1_close_before", &before);
    hubbed.finish();
}
