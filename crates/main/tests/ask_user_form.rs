//! Binary-level tests of `ask_user` in a session the hub started
//! (`docs/tools.md`, "When a person can answer"): the built `fiber` runs
//! `hub serve` in its own process group with its own `FIBER_HOME`, the
//! fake model calls `ask_user`, and two clients on the hub see one `form`
//! that either can answer (`docs/invocation.md`, "Replying"). Every receive
//! waits on the test's one [`Deadline`]; every command on a session has its
//! own id.

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

/// `Base`, a multi-select with two options, and `Name`, free text.
fn base_and_name() -> Value {
    json!([
        {"header": "Base", "question": "Which branch?", "multiSelect": true,
         "options": [{"label": "main (Recommended)"}, {"label": "dev"}]},
        {"header": "Name", "question": "What name?"}
    ])
}

/// `Base` as above, and `Pick`, a single-select with two options.
fn base_and_pick() -> Value {
    json!([
        {"header": "Base", "question": "Which branch?", "multiSelect": true,
         "options": [{"label": "main (Recommended)"}, {"label": "dev"}]},
        {"header": "Pick", "question": "Which one?",
         "options": [{"label": "a"}, {"label": "b"}]}
    ])
}

/// A stream whose one item calls `ask_user` with `questions`.
fn ask_user(questions: &Value) -> fakes::Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": "fc_call_ask",
        "call_id": "call_ask",
        "name": "ask_user",
        "arguments": json!({"questions": questions}).to_string()
    }})])
}

fn command(id: &str, session: &str, name: &str, args: &Value) -> String {
    json!({"id": id, "session_id": session, "command": name, "args": args}).to_string()
}

/// Whether `line` answers command `id`, accepted or rejected.
fn answers(line: &Value, id: &str) -> bool {
    (line["kind"] == "command_accepted" || line["kind"] == "command_rejected")
        && line["payload"]["command_id"] == id
}

fn of_kind<'a>(lines: &'a [Value], kind: &str) -> Vec<&'a Value> {
    lines.iter().filter(|line| line["kind"] == kind).collect()
}

/// A session the hub started on `ask_user`'s first call, with client A and
/// client B both subscribed through the hub.
struct Hubbed {
    a: Socket,
    b: Socket,
    session: String,
    server: ProviderServer,
    guard: SessionGuard,
    hub: Arc<Mutex<Option<HubProc>>>,
    workspace: String,
    setup: Setup,
}

impl Hubbed {
    fn open(questions: &Value) -> Self {
        let setup = Setup::new();
        let server = ProviderServer::start([ask_user(questions), hello()]).unwrap();
        setup.provider(&server);
        let hub = Arc::new(Mutex::new(None));
        let (a, _) = connect_hub(&setup, &hub);
        let workspace = setup.workspace().to_string_lossy().into_owned();
        let guard = SessionGuard::arm(setup.deadline, &workspace);
        let session = start_session(&a, &workspace, "ask-me");
        let (b, _) = connect_hub(&setup, &hub);
        let hubbed = Self {
            a,
            b,
            session,
            server,
            guard,
            hub,
            workspace,
            setup,
        };
        hubbed.send_a("c_sub_a", "subscribe", &json!({"level": "full"}));
        hubbed.answered_a("c_sub_a");
        hubbed.send_b("c_sub_b", "subscribe", &json!({"level": "full"}));
        until(&hubbed.b, "B's subscribe acknowledgement", |line| {
            answers(line, "c_sub_b")
        });
        hubbed
    }

    fn send_a(&self, id: &str, name: &str, args: &Value) {
        self.a.send(&command(id, &self.session, name, args));
    }

    fn send_b(&self, id: &str, name: &str, args: &Value) {
        self.b.send(&command(id, &self.session, name, args));
    }

    /// A's lines up to the answer to its command `id`, which comes last.
    fn answered_a(&self, id: &str) -> Vec<Value> {
        until(&self.a, &format!("the answer to {id}"), |line| {
            answers(line, id)
        })
    }

    /// A's lines up to the next line of `kind`, which comes last.
    fn until_a(&self, kind: &str) -> Vec<Value> {
        until(&self.a, kind, |line| line["kind"] == kind)
    }

    /// A's `interaction_requested` and the `tool_call_requested` before it.
    fn requested(&self) -> (Value, Value) {
        let lines = self.until_a("interaction_requested");
        let call = of_kind(&lines, "tool_call_requested")
            .last()
            .copied()
            .cloned()
            .expect("the call came first");
        (lines.last().unwrap().clone(), call)
    }

    /// The session's log, parsed.
    fn log(&self) -> Vec<Value> {
        let dir = log::sessions_dir(&self.setup.home(), &doors::project(&self.setup.workspace()))
            .join(&self.session);
        fs::read_to_string(dir.join("events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Sends `close` as `c_close` and returns A's lines up to the relayed
    /// `fiber_exited`, once the session process is gone.
    fn close(&self) -> Vec<Value> {
        self.send_a("c_close", "close", &json!({}));
        let lines = self.until_a("fiber_exited");
        assert!(
            fakes::matching_exits(&self.workspace, self.setup.deadline.left()),
            "waited until the deadline for the session process to exit"
        );
        lines
    }

    /// Stands the guards down and stops the hub.
    fn finish(self) {
        let Self {
            a, b, guard, hub, ..
        } = self;
        guard.wait_gone();
        drop((a, b));
        if let Some(running) = hub.lock().unwrap().take() {
            running.kill_and_wait();
        }
    }
}

/// The request id `line` carries.
fn request_of(line: &Value) -> String {
    line["payload"]["request_id"].as_str().unwrap().to_owned()
}

/// The kinds of `lines` from the first `tool_call_requested` to the first
/// `fiber_exited` after it, both included. A session that exits suspended
/// writes no `turn_completed` between them.
fn kinds_through_exit(lines: &[Value]) -> Vec<&str> {
    let kinds: Vec<&str> = lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    let start = kinds
        .iter()
        .position(|kind| *kind == "tool_call_requested")
        .unwrap();
    let end = start
        + kinds[start..]
            .iter()
            .position(|kind| *kind == "fiber_exited")
            .unwrap();
    kinds[start..=end].to_vec()
}

/// The kinds of `lines` from the first `tool_call_requested` to the first
/// `turn_completed` after it, both included. The log keeps no deltas.
fn kinds_of_the_call(lines: &[Value]) -> Vec<&str> {
    let kinds: Vec<&str> = lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    let start = kinds
        .iter()
        .position(|kind| *kind == "tool_call_requested")
        .unwrap();
    let end = start
        + kinds[start..]
            .iter()
            .position(|kind| *kind == "turn_completed")
            .unwrap();
    kinds[start..=end].to_vec()
}

#[test]
fn a_form_is_answered_by_any_client_and_the_model_gets_one_line_per_question() {
    let hubbed = Hubbed::open(&base_and_name());
    let (requested, call) = hubbed.requested();
    let payload = &requested["payload"];
    assert_eq!(payload["kind"], "form");
    assert_eq!(payload["fields"], base_and_name());
    assert_eq!(payload["action_ids"], json!([call["action_id"]]));
    assert!(requested["action_id"].is_null(), "{requested}");
    let request = request_of(&requested);
    let answered = json!([
        {"labels": ["main (Recommended)", "dev"], "text": "and tags"},
        {"labels": [], "text": "fiber-cli"}
    ]);
    hubbed.send_b(
        "c_reply_b",
        "reply",
        &json!({"request_id": request, "answers": answered, "note": "by friday"}),
    );
    let resolved = hubbed.until_a("interaction_resolved");
    let resolved = &resolved.last().unwrap()["payload"];
    assert_eq!(resolved["request_id"], request.as_str());
    assert_eq!(resolved["by"], "person");
    assert_eq!(resolved["answers"], answered);
    assert_eq!(resolved["note"], "by friday");
    let accepted = until(&hubbed.b, "B's reply acknowledgement", |line| {
        answers(line, "c_reply_b")
    });
    assert_eq!(accepted.last().unwrap()["kind"], "command_accepted");
    hubbed.send_a(
        "c_reply_a",
        "reply",
        &json!({"request_id": request, "declined": true}),
    );
    let (mut stale, mut ended) = (false, false);
    let rest = until(&hubbed.a, "A's late reply and turn_completed", |line| {
        stale |= answers(line, "c_reply_a");
        ended |= line["kind"] == "turn_completed";
        stale && ended
    });
    let rejected = rest.iter().find(|line| answers(line, "c_reply_a")).unwrap();
    assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
    assert_eq!(rejected["payload"]["code"], "stale_request", "{rejected}");
    assert!(
        of_kind(&rest, "interaction_resolved").is_empty(),
        "{rest:?}"
    );
    let text =
        "Base: main (Recommended), dev, \"and tags\"\nName: \"fiber-cli\"\nnote: \"by friday\"";
    let completed = of_kind(&rest, "tool_call_completed")[0];
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["content"][0]["text"], text);
    let requests = hubbed.server.requests();
    let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let output = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert_eq!(output["call_id"], "call_ask");
    assert_eq!(output["output"], text);
    hubbed.close();
    assert_eq!(
        kinds_of_the_call(&hubbed.log()),
        [
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "interaction_requested",
            "interaction_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    hubbed.finish();
}

#[test]
fn a_reply_that_does_not_fit_the_form_is_rejected_and_the_form_stays_pending() {
    let hubbed = Hubbed::open(&base_and_pick());
    let (requested, _) = hubbed.requested();
    let request = request_of(&requested);
    let unfit = [
        (
            "c_extra",
            json!({"request_id": request, "answers": [{"labels": [], "extra": 1}, {"labels": ["a"]}]}),
        ),
        (
            "c_short",
            json!({"request_id": request, "answers": [{"labels": ["dev"]}]}),
        ),
        (
            "c_unoffered",
            json!({"request_id": request, "answers": [{"labels": ["nope"]}, {"labels": ["a"]}]}),
        ),
        (
            "c_two",
            json!({"request_id": request, "answers": [{"labels": ["dev"]}, {"labels": ["a", "b"]}]}),
        ),
        (
            "c_confirmed",
            json!({"request_id": request, "confirmed": true}),
        ),
    ];
    for (id, args) in &unfit {
        hubbed.send_a(id, "reply", args);
        let lines = hubbed.answered_a(id);
        let answer = lines.last().unwrap();
        assert_eq!(answer["kind"], "command_rejected", "{id}: {answer}");
        assert_eq!(
            answer["payload"]["code"], "invalid_arguments",
            "{id}: {answer}"
        );
        assert!(
            of_kind(&lines, "interaction_resolved").is_empty(),
            "{id}: {lines:?}"
        );
    }
    hubbed.send_a(
        "c_nope",
        "reply",
        &json!({"request_id": "r_nope", "declined": true}),
    );
    let lines = hubbed.answered_a("c_nope");
    let answer = lines.last().unwrap();
    assert_eq!(answer["kind"], "command_rejected", "{answer}");
    assert_eq!(answer["payload"]["code"], "stale_request", "{answer}");
    hubbed.send_a(
        "c_fit",
        "reply",
        &json!({"request_id": request, "answers": [{"labels": ["dev"]}, {"labels": ["b"]}]}),
    );
    let (mut accepted, mut ended) = (false, false);
    let rest = until(&hubbed.a, "the fitting reply and turn_completed", |line| {
        accepted |= answers(line, "c_fit");
        ended |= line["kind"] == "turn_completed";
        accepted && ended
    });
    let answer = rest.iter().find(|line| answers(line, "c_fit")).unwrap();
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    let completed = of_kind(&rest, "tool_call_completed")[0];
    assert_eq!(
        completed["payload"]["content"][0]["text"],
        "Base: dev\nPick: b"
    );
    hubbed.close();
    assert_eq!(
        kinds_of_the_call(&hubbed.log()),
        [
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "interaction_requested",
            "interaction_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    hubbed.finish();
}

#[test]
fn a_declined_form_completes_with_declined() {
    let hubbed = Hubbed::open(&base_and_name());
    let (requested, _) = hubbed.requested();
    let request = request_of(&requested);
    hubbed.send_a(
        "c_decline",
        "reply",
        &json!({"request_id": request, "declined": true}),
    );
    let lines = hubbed.until_a("turn_completed");
    let resolved = of_kind(&lines, "interaction_resolved")[0];
    assert_eq!(resolved["payload"]["by"], "person");
    assert_eq!(resolved["payload"]["declined"], true);
    let completed = of_kind(&lines, "tool_call_completed")[0];
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["content"][0]["text"], "declined");
    let ended = lines.last().unwrap();
    assert_eq!(ended["payload"]["outcome"], "completed");
    assert!(ended["payload"].get("questions").is_none(), "{ended}");
    hubbed.close();
    assert_eq!(
        kinds_of_the_call(&hubbed.log()),
        [
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "interaction_requested",
            "interaction_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    hubbed.finish();
}

#[test]
fn a_cancel_while_the_form_is_pending_resolves_it_by_fiber() {
    let hubbed = Hubbed::open(&base_and_name());
    let (requested, _) = hubbed.requested();
    let request = request_of(&requested);
    hubbed.send_a("c_cancel", "cancel", &json!({}));
    let lines = hubbed.until_a("turn_completed");
    let resolved = of_kind(&lines, "interaction_resolved")[0];
    assert_eq!(resolved["payload"]["request_id"], request.as_str());
    assert_eq!(resolved["payload"]["by"], "fiber");
    assert_eq!(resolved["payload"]["declined"], true);
    let completed = of_kind(&lines, "tool_call_completed")[0];
    assert_eq!(completed["payload"]["status"], "cancelled");
    assert_eq!(completed["payload"]["content"][0]["text"], "declined");
    let ended = lines.last().unwrap();
    assert_eq!(ended["payload"]["outcome"], "interrupted");
    assert!(ended["payload"].get("questions").is_none(), "{ended}");
    hubbed.close();
    assert_eq!(
        kinds_of_the_call(&hubbed.log()),
        [
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "interaction_requested",
            "interaction_resolved",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    hubbed.finish();
}

#[test]
fn a_close_while_the_form_is_pending_ends_the_turn_with_its_questions() {
    let hubbed = Hubbed::open(&base_and_name());
    let (requested, _) = hubbed.requested();
    let request = request_of(&requested);
    let lines = hubbed.close();
    let resolved = of_kind(&lines, "interaction_resolved")[0];
    assert_eq!(resolved["payload"]["request_id"], request.as_str());
    assert_eq!(resolved["payload"]["by"], "fiber");
    assert_eq!(resolved["payload"]["declined"], true);
    let completed = of_kind(&lines, "tool_call_completed")[0];
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["content"][0]["text"], SENT);
    assert_eq!(
        completed["payload"]["control"]["questions"],
        base_and_name()
    );
    let ended = of_kind(&lines, "turn_completed")[0];
    assert_eq!(
        ended["payload"],
        json!({"outcome": "completed", "questions": base_and_name()})
    );
    let exited = lines.last().unwrap();
    assert_eq!(exited["payload"]["questions"], base_and_name());
    assert_eq!(hubbed.server.requests().len(), 1, "no second model request");
    assert_eq!(
        kinds_of_the_call(&hubbed.log()),
        [
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "interaction_requested",
            "interaction_resolved",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    hubbed.finish();
}

#[test]
fn a_form_waits_without_a_timeout() {
    let hubbed = Hubbed::open(&base_and_name());
    let (requested, _) = hubbed.requested();
    let request = request_of(&requested);
    hubbed.send_a(
        "c_steer",
        "steer",
        &json!({"content": [{"type": "text", "text": "meanwhile"}]}),
    );
    let lines = hubbed.answered_a("c_steer");
    assert_eq!(lines.last().unwrap()["kind"], "command_accepted");
    assert!(
        of_kind(&lines, "interaction_resolved").is_empty(),
        "the form stays pending: {lines:?}"
    );
    hubbed.send_a(
        "c_reply",
        "reply",
        &json!({"request_id": request, "answers": [{"skipped": true}, {"labels": [], "text": "x"}]}),
    );
    let lines = hubbed.until_a("turn_completed");
    let resolved = of_kind(&lines, "interaction_resolved")[0];
    assert_eq!(resolved["payload"]["by"], "person");
    let completed = of_kind(&lines, "tool_call_completed")[0];
    assert_eq!(
        completed["payload"]["content"][0]["text"],
        "Base: skipped\nName: \"x\""
    );
    hubbed.close();
    assert_eq!(
        kinds_of_the_call(&hubbed.log()),
        [
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "interaction_requested",
            "interaction_resolved",
            "tool_call_completed",
            "step_started",
            "steering_applied",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    hubbed.finish();
}

/// Makes `fake/m` the configured model, with `session.idle_exit_ms` when
/// given.
fn idle(setup: &Setup, ms: Option<u64>) {
    let config = match ms {
        Some(ms) => json!({"model": "fake/m", "session": {"idle_exit_ms": ms}}),
        None => json!({"model": "fake/m"}),
    };
    write_json(&setup.home().join("config.json"), &config);
}

/// The log of session `id` in `setup`'s workspace, parsed.
fn log_of(setup: &Setup, id: &str) -> Vec<Value> {
    let dir = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace())).join(id);
    fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The kinds of `lines` after the last `fiber_exited` that names a
/// suspended request: what the resumed process wrote.
fn kinds_after_suspension(lines: &[Value]) -> Vec<&str> {
    let start = lines
        .iter()
        .rposition(|line| {
            line["kind"] == "fiber_exited" && line["payload"]["suspended_on"].is_string()
        })
        .expect("the session exited suspended");
    lines[start + 1..]
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}

#[test]
fn a_session_idle_on_a_form_exits_suspended_and_a_reply_through_the_hub_resumes_it() {
    let setup = Setup::new();
    let server = ProviderServer::start([ask_user(&base_and_name()), hello()]).unwrap();
    server.hold();
    setup.provider(&server);
    // Long enough to cover the start handshake, short enough to idle out
    // on the form.
    idle(&setup, Some(2000));
    let hub = Arc::new(Mutex::new(None));
    let (a, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&a, &workspace, "ask-me");
    // Subscribed while the model call is held: the form's idle wait starts
    // only after it.
    a.send(&command(
        "c_sub_a",
        &session,
        "subscribe",
        &json!({"level": "full"}),
    ));
    until(&a, "the subscribe acknowledgement", |line| {
        answers(line, "c_sub_a")
    });
    server.release();
    let asked = until(&a, "interaction_requested", |line| {
        line["kind"] == "interaction_requested"
    });
    let requested = asked.last().unwrap().clone();
    let request = request_of(&requested);
    let exited = until(&a, "fiber_exited", |line| line["kind"] == "fiber_exited");
    assert!(
        fakes::matching_exits(&workspace, setup.deadline.left()),
        "waited until the deadline for the session process to exit"
    );
    assert_eq!(
        exited.last().unwrap()["payload"]["suspended_on"],
        request.as_str()
    );
    assert!(
        of_kind(&exited, "interaction_resolved").is_empty(),
        "{exited:?}"
    );
    idle(&setup, None);

    let answered = json!([
        {"labels": ["dev"]},
        {"labels": [], "text": "fiber-cli"}
    ]);
    a.send(&command(
        "c_reply",
        &session,
        "reply",
        &json!({"request_id": request, "answers": answered}),
    ));
    let (mut accepted, mut ended) = (false, false);
    let resumed = until(&a, "the reply and the resumed turn", |line| {
        accepted |= answers(line, "c_reply");
        ended |= line["kind"] == "turn_completed";
        accepted && ended
    });
    let answer = resumed
        .iter()
        .find(|line| answers(line, "c_reply"))
        .unwrap();
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    // What the resumed process wrote, after the hub's replay of the log.
    let start = resumed
        .iter()
        .position(|line| line["kind"] == "fiber_started" && line["payload"]["resumed"] == true)
        .expect("the session resumed");
    let resumed = &resumed[start..];
    let raised = of_kind(resumed, "interaction_requested");
    assert_eq!(raised.len(), 1, "{resumed:?}");
    assert_eq!(
        raised[0]["payload"], requested["payload"],
        "the same request"
    );
    let resolved = of_kind(resumed, "interaction_resolved")[0];
    assert_eq!(resolved["payload"]["request_id"], request.as_str());
    assert_eq!(resolved["payload"]["by"], "person");
    assert_eq!(resolved["payload"]["answers"], answered);
    let completed = of_kind(resumed, "tool_call_completed")[0];
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(
        completed["payload"]["content"][0]["text"],
        "Base: dev\nName: \"fiber-cli\""
    );
    assert_eq!(
        of_kind(resumed, "turn_completed")[0]["payload"]["outcome"],
        "completed"
    );

    a.send(&command("c_close", &session, "close", &json!({})));
    until(&a, "fiber_exited", |line| line["kind"] == "fiber_exited");
    guard.wait_gone();
    drop(a);
    let lines = log_of(&setup, &session);
    assert_one_continued_log(&lines, 2);
    assert_eq!(
        kinds_after_suspension(&lines),
        [
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "interaction_requested",
            "interaction_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    if let Some(running) = hub.lock().unwrap().take() {
        running.kill_and_wait();
    }
}

#[test]
fn fiber_ask_resume_on_a_form_suspended_session_ends_the_turn_with_the_questions() {
    let setup = Setup::new();
    let server = ProviderServer::start([ask_user(&base_and_name()), hello()]).unwrap();
    setup.provider(&server);
    idle(&setup, Some(0));
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let id = doors::mint("s_");
    let suspended = run_to_exit(
        setup.deadline,
        "fiber session",
        setup.fiber(&[
            "session",
            "--id",
            &id,
            "--workspace",
            &workspace,
            "--prompt",
            "ask-me",
        ]),
    );
    assert_eq!(
        suspended.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&suspended.stderr)
    );
    let lines = log_of(&setup, &id);
    let request = request_of(of_kind(&lines, "interaction_requested")[0]);
    assert_eq!(
        lines.last().unwrap()["payload"]["suspended_on"],
        request.as_str()
    );
    idle(&setup, None);

    let mut ask = setup.fiber(&["ask", "--resume", &id, "next"]);
    ask.current_dir(setup.workspace());
    let resumed = run_to_exit(setup.deadline, "fiber ask --resume", ask);
    assert_eq!(
        resumed.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let lines = log_of(&setup, &id);
    assert_one_continued_log(&lines, 2);
    assert_eq!(
        kinds_after_suspension(&lines),
        [
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "interaction_requested",
            "interaction_resolved",
            "tool_call_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let raised = of_kind(&lines, "interaction_requested");
    assert_eq!(raised.len(), 2, "raised again");
    assert_eq!(
        raised[1]["payload"], raised[0]["payload"],
        "the same request"
    );
    let resolved = of_kind(&lines, "interaction_resolved")[0];
    assert_eq!(resolved["payload"]["request_id"], request.as_str());
    assert_eq!(resolved["payload"]["by"], "fiber");
    assert_eq!(resolved["payload"]["declined"], true);
    let completed = of_kind(&lines, "tool_call_completed")[0];
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["content"][0]["text"], SENT);
    assert_eq!(
        completed["payload"]["control"]["questions"],
        base_and_name()
    );
    let ended = of_kind(&lines, "turn_completed");
    assert_eq!(
        ended[0]["payload"],
        json!({"outcome": "completed", "questions": base_and_name()})
    );
    assert_eq!(ended[1]["payload"]["outcome"], "completed");
    assert_eq!(
        server.requests().len(),
        2,
        "the prompt's turn asked the model"
    );
}

#[test]
fn sigterm_on_a_session_pending_on_a_form_exits_143_suspended_and_a_reply_through_the_hub_resumes_it()
 {
    // The default idle delay is 30 minutes, so only the signal ends the
    // session. A question with no timeout lives like a pending
    // approval: the signal exits with it still pending, and resuming
    // raises it again under the same request id.
    let hubbed = Hubbed::open(&base_and_name());
    let (requested, _) = hubbed.requested();
    let request = request_of(&requested);
    // The session's command line carries `--workspace <path>`; the
    // short-lived `git -C <path>` children it spawns carry the bare
    // path, so matching the flag names the session alone. The hub
    // itself carries neither.
    let session_match = format!("--workspace {}", hubbed.workspace);
    let pids = fakes::matching(&session_match).unwrap();
    assert_eq!(pids.len(), 1, "one session process, not {pids:?}");
    assert!(kill_pid(hubbed.setup.deadline, pids[0], "TERM").unwrap());
    let exited = until(&hubbed.a, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    let exit = exited.last().unwrap();
    assert_eq!(exit["payload"]["exit_code"], 143);
    assert_eq!(exit["payload"]["suspended_on"], request.as_str());
    assert!(
        of_kind(&exited, "interaction_resolved").is_empty(),
        "the form stays pending: {exited:?}"
    );
    assert!(
        fakes::matching_exits(&session_match, hubbed.setup.deadline.left()),
        "waited until the deadline for the session process to exit"
    );
    assert_eq!(
        kinds_through_exit(&hubbed.log()),
        [
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "interaction_requested",
            "fiber_exited",
        ]
    );

    let answered = json!([{"labels": ["dev"]}, {"labels": [], "text": "fiber-cli"}]);
    hubbed.send_a(
        "c_reply",
        "reply",
        &json!({"request_id": request.as_str(), "answers": answered}),
    );
    let (mut accepted, mut ended) = (false, false);
    let resumed = until(&hubbed.a, "the reply and the resumed turn", |line| {
        accepted |= answers(line, "c_reply");
        ended |= line["kind"] == "turn_completed";
        accepted && ended
    });
    let answer = resumed
        .iter()
        .find(|line| answers(line, "c_reply"))
        .unwrap();
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    // What the resumed process wrote, after the hub's replay of the log.
    let start = resumed
        .iter()
        .position(|line| line["kind"] == "fiber_started" && line["payload"]["resumed"] == true)
        .expect("the session resumed");
    let resumed = &resumed[start..];
    let raised = of_kind(resumed, "interaction_requested");
    assert_eq!(raised.len(), 1, "{resumed:?}");
    assert_eq!(
        raised[0]["payload"], requested["payload"],
        "the same request"
    );
    let resolved = of_kind(resumed, "interaction_resolved")[0];
    assert_eq!(resolved["payload"]["request_id"], request.as_str());
    assert_eq!(resolved["payload"]["by"], "person");
    assert_eq!(resolved["payload"]["answers"], answered);
    let completed = of_kind(resumed, "tool_call_completed")[0];
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(
        completed["payload"]["content"][0]["text"],
        "Base: dev\nName: \"fiber-cli\""
    );
    assert_eq!(
        of_kind(resumed, "turn_completed")[0]["payload"]["outcome"],
        "completed"
    );

    hubbed.close();
    let lines = hubbed.log();
    assert_one_continued_log(&lines, 2);
    assert_eq!(
        kinds_after_suspension(&lines),
        [
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "interaction_requested",
            "interaction_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    hubbed.finish();
}
