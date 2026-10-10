use std::sync::{Arc, Mutex};

use contract::events::{Git, SessionState, SessionStatus, Waiting, WaitingKind};
use contract::{Envelope, JobId, SessionId};
use serde_json::{Value, json};

use super::Fold;

/// The longest a wait in these tests lasts before it fails.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// A fold over a fake job registry and a fake branch.
struct World {
    fold: Fold,
    running: Arc<Mutex<Vec<JobId>>>,
    branch: Arc<Mutex<Option<Git>>>,
    count: Arc<Mutex<u32>>,
    reads: Arc<Mutex<u32>>,
    ts: u64,
}

fn waiting_of(w: &World) -> Waiting {
    let state = w.status().state;
    let SessionState::Waiting { waiting } = state else {
        panic!("not waiting: {state:?}");
    };
    waiting
}

fn job(id: &str) -> JobId {
    JobId(id.to_owned())
}

fn world() -> World {
    let running = Arc::new(Mutex::new(Vec::new()));
    let branch = Arc::new(Mutex::new(None));
    let count = Arc::new(Mutex::new(0u32));
    let reads = Arc::new(Mutex::new(0u32));
    let (r, b) = (Arc::clone(&running), Arc::clone(&branch));
    let (c, n) = (Arc::clone(&count), Arc::clone(&reads));
    let fold = Fold::new(
        "-w".to_owned(),
        "/w".to_owned(),
        "fake/m".to_owned(),
        None,
        Box::new(move || r.lock().unwrap().clone()),
        Box::new(move || b.lock().unwrap().clone()),
        Box::new(move || {
            *n.lock().unwrap() += 1;
            *c.lock().unwrap()
        }),
    );
    World {
        fold,
        running,
        branch,
        count,
        reads,
        ts: 1000,
    }
}

impl World {
    /// Folds one durable line one tick after the last; true when the status
    /// changed.
    fn feed(&mut self, kind: &str, action: Option<&str>, payload: &Value) -> bool {
        self.ts += 1;
        let line = envelope(kind, self.ts, action, Some(self.ts), payload);
        self.fold.observe(&line)
    }

    fn live(&mut self) {
        self.fold.go_live();
    }

    fn status(&self) -> SessionStatus {
        self.fold.status()
    }

    fn start(&mut self) {
        self.feed("session_started", None, &started());
    }

    fn prompt(&mut self, text: &str) {
        self.feed("turn_started", None, &prompt(text));
    }

    fn call(&mut self, action: &str, tool: &str) {
        self.feed(
            "tool_call_requested",
            Some(action),
            &json!({"name": tool, "arguments": {}}),
        );
        self.feed(
            "tool_call_started",
            Some(action),
            &json!({"effects": [], "reversible": true}),
        );
    }

    fn done(&mut self, action: &str) {
        self.feed(
            "tool_call_completed",
            Some(action),
            &json!({"status": "completed", "content": []}),
        );
    }

    fn ask(&mut self, action: &str, request: &str) {
        self.feed(
            "permission_requested",
            Some(action),
            &json!({"request_id": request, "effects": [], "reversible": true, "step": "review"}),
        );
    }

    fn allow(&mut self, action: &str, request: &str) {
        self.feed(
            "permission_resolved",
            Some(action),
            &json!({"request_id": request, "decision": "allow", "decided_by": "person"}),
        );
    }

    fn finish(&mut self) {
        self.feed("turn_completed", None, &json!({"outcome": "completed"}));
    }
}

fn envelope(
    kind: &str,
    ts: u64,
    action: Option<&str>,
    seq: Option<u64>,
    payload: &Value,
) -> Envelope {
    let mut line = json!({
        "kind": kind,
        "session_id": "s_1",
        "ts": ts,
        "schema_version": 1,
        "payload": payload,
    });
    if let Some(action) = action {
        line["action_id"] = json!(action);
    }
    if let Some(seq) = seq {
        line["seq"] = json!(seq);
    }
    serde_json::from_value(line).unwrap()
}

fn started() -> Value {
    json!({"workspace": "/w", "variables": {"path": "/bin", "names": [], "source": "inherited"}})
}

fn prompt(text: &str) -> Value {
    json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": text}]}]})
}

fn usage(generation: &str, input: u64, cost: Option<f64>) -> Value {
    json!({
        "generation_id": generation,
        "model": "fake/m",
        "tokens": {"input": input, "cache_read": 0, "cache_write": {}, "output": 5},
        "input_bytes": 1,
        "cost": cost,
    })
}

#[test]
fn a_new_fold_is_idle_with_the_loops_own_model_and_workspace() {
    let mut w = world();
    w.start();
    let s = w.status();
    assert_eq!(s.state, SessionState::Idle);
    assert_eq!(s.name, "");
    assert_eq!((s.workspace.as_str(), s.model.as_str()), ("/w", "fake/m"));
    assert_eq!((s.delegates, s.jobs), (0, 0));
    assert_eq!(s.since, 1001);
    assert!(s.context.is_none() && s.git.is_none() && s.parent.is_none());
}

#[test]
fn the_name_is_the_first_prompt_whole_until_a_session_is_named() {
    let mut w = world();
    w.start();
    w.prompt("say hi\nthen bye");
    w.finish();
    w.prompt("second");
    assert_eq!(w.status().name, "say hi\nthen bye");
    w.feed(
        "session_named",
        None,
        &json!({"name": "greeter", "by": "model"}),
    );
    assert_eq!(w.status().name, "greeter");
    w.feed(
        "session_named",
        None,
        &json!({"name": "pinned", "by": "person"}),
    );
    assert_eq!(w.status().name, "pinned");
    // A cleared name falls back to the first prompt again.
    w.feed(
        "session_named",
        None,
        &json!({"name": null, "by": "person"}),
    );
    assert_eq!(w.status().name, "say hi\nthen bye");
}

#[test]
fn the_first_prompt_joins_its_text_parts_and_skips_other_items() {
    let mut w = world();
    w.start();
    w.feed(
        "turn_started",
        None,
        &json!({"input": [
            {"type": "jobs", "job_ids": ["j1"]},
            {"type": "message", "source": "driver", "content": [
                {"type": "text", "text": "ab"},
                {"type": "image", "path": "a.png", "mime_type": "image/png", "width": 1, "height": 1},
                {"type": "text", "text": "cd"}
            ]},
            {"type": "message", "source": "driver", "content": [{"type": "text", "text": "later"}]}
        ]}),
    );
    assert_eq!(w.status().name, "abcd");
}

#[test]
fn a_first_turn_without_a_message_leaves_no_name_to_fall_back_on() {
    let mut w = world();
    w.start();
    w.feed(
        "turn_started",
        None,
        &json!({"input": [{"type": "jobs", "job_ids": ["j1"]}]}),
    );
    w.finish();
    w.prompt("later");
    assert_eq!(w.status().name, "");
}

#[test]
fn a_delegate_names_its_parent_and_another_session_does_not() {
    let mut w = world();
    let mut payload = started();
    payload["parent"] = json!({"session_id": "s_parent", "delegate_id": "j1"});
    w.feed("session_started", None, &payload);
    assert_eq!(w.status().parent, Some(SessionId("s_parent".to_owned())));
    let mut plain = world();
    plain.start();
    assert_eq!(plain.status().parent, None);
}

#[test]
fn the_workspace_is_the_one_session_started_records() {
    let mut w = world();
    let mut payload = started();
    payload["workspace"] = json!("/elsewhere");
    w.feed("session_started", None, &payload);
    assert_eq!(w.status().workspace, "/elsewhere");
}

#[test]
fn the_model_and_window_follow_the_preamble_and_a_switch() {
    let mut w = world();
    w.start();
    w.feed(
        "preamble_built",
        None,
        &json!({
            "reason": "start", "model": "fake/built", "context_window": 1000,
            "tool_choice": "auto", "cache_lifetime": "5m", "system_prompt": "", "tools": []
        }),
    );
    assert_eq!(w.status().model, "fake/built");
    w.feed("usage_recorded", Some("a1"), &usage("g1", 100, None));
    let fill = w.status().context.unwrap();
    assert_eq!((fill.tokens, fill.window), (105, 1000));
    let settings = |model: &str| json!({"model": model, "cache_lifetime": "5m"});
    w.feed(
        "model_changed",
        None,
        &json!({"before": settings("fake/built"), "after": settings("fake/next"), "source": "driver"}),
    );
    let s = w.status();
    assert_eq!(s.model, "fake/next");
    assert_eq!(s.context.unwrap().window, 1000);
}

#[test]
fn context_needs_a_window_and_a_window_of_zero_is_none() {
    let mut w = world();
    w.start();
    w.feed("usage_recorded", Some("a1"), &usage("g1", 100, None));
    assert!(w.status().context.is_none());
    w.feed(
        "preamble_built",
        None,
        &json!({
            "reason": "start", "model": "fake/m", "context_window": 0,
            "tool_choice": "auto", "cache_lifetime": "5m", "system_prompt": "", "tools": []
        }),
    );
    assert!(w.status().context.is_none());
}

#[test]
fn only_a_session_model_reply_sets_the_context() {
    let mut w = world();
    w.start();
    w.fold = {
        let running = Arc::clone(&w.running);
        let branch = Arc::clone(&w.branch);
        Fold::new(
            "-w".to_owned(),
            "/w".to_owned(),
            "fake/m".to_owned(),
            Some(1000),
            Box::new(move || running.lock().unwrap().clone()),
            Box::new(move || branch.lock().unwrap().clone()),
            Box::new(|| 0),
        )
    };
    // The reviewer's line has no action; a copy names its origin; an
    // extension's call names the extension.
    w.feed("usage_recorded", None, &usage("g1", 1, None));
    let mut copy = usage("g2", 2, None);
    copy["origin_session_id"] = json!("s_delegate");
    w.feed("usage_recorded", Some("a1"), &copy);
    let mut extension = usage("g3", 3, None);
    extension["extension"] = json!("ext");
    w.feed("usage_recorded", Some("a1"), &extension);
    assert!(w.status().context.is_none());
    w.feed("usage_recorded", Some("a1"), &usage("g4", 40, None));
    assert_eq!(w.status().context.unwrap().tokens, 45);
    // The latest wins.
    w.feed("usage_recorded", Some("a2"), &usage("g5", 7, None));
    assert_eq!(w.status().context.unwrap().tokens, 12);
    // A handoff clears it until the next reply.
    w.feed(
        "handoff_completed",
        None,
        &json!({"outcome": "completed", "tokens_before": 12}),
    );
    assert!(w.status().context.is_none());
    w.feed("usage_recorded", Some("a3"), &usage("g6", 1, None));
    assert_eq!(w.status().context.unwrap().tokens, 6);
}

#[test]
fn a_record_that_reported_no_input_tokens_never_moves_the_context() {
    let mut w = world();
    w.start();
    w.fold = {
        let running = Arc::clone(&w.running);
        let branch = Arc::clone(&w.branch);
        Fold::new(
            "-w".to_owned(),
            "/w".to_owned(),
            "fake/m".to_owned(),
            Some(1000),
            Box::new(move || running.lock().unwrap().clone()),
            Box::new(move || branch.lock().unwrap().clone()),
            Box::new(|| 0),
        )
    };
    w.feed("usage_recorded", Some("a1"), &usage("g1", 500, None));
    assert_eq!(w.status().context.unwrap().tokens, 505);
    // A call that failed before its provider named a generation, and a
    // named one cut before its usage: no input tokens, output 5 each.
    w.feed(
        "usage_recorded",
        Some("a2"),
        &usage("fiber-0123456789abcdef", 0, None),
    );
    assert_eq!(w.status().context.unwrap().tokens, 505);
    w.feed("usage_recorded", Some("a3"), &usage("g3", 0, None));
    assert_eq!(w.status().context.unwrap().tokens, 505);
    // After a handoff it waits for the next record that reports its input.
    w.feed(
        "handoff_completed",
        None,
        &json!({"outcome": "completed", "tokens_before": 505}),
    );
    w.feed("usage_recorded", Some("a4"), &usage("g4", 0, None));
    assert!(w.status().context.is_none());
    w.feed("usage_recorded", Some("a5"), &usage("g5", 30, None));
    assert_eq!(w.status().context.unwrap().tokens, 35);
}

#[test]
fn a_late_correction_of_an_earlier_call_never_moves_the_context() {
    let mut w = world();
    w.start();
    w.fold = {
        let running = Arc::clone(&w.running);
        let branch = Arc::clone(&w.branch);
        Fold::new(
            "-w".to_owned(),
            "/w".to_owned(),
            "fake/m".to_owned(),
            Some(1000),
            Box::new(move || running.lock().unwrap().clone()),
            Box::new(move || branch.lock().unwrap().clone()),
            Box::new(|| 0),
        )
    };
    w.feed("usage_recorded", Some("a1"), &usage("g1", 40, None));
    w.feed("usage_recorded", Some("a2"), &usage("g2", 7, None));
    assert_eq!(w.status().context.unwrap().tokens, 12);
    // `g1`'s cost settles after a newer call: the context stays the newer
    // call's.
    w.feed("usage_recorded", Some("a1"), &usage("g1", 40, Some(0.5)));
    assert_eq!(w.status().context.unwrap().tokens, 12);
    assert_eq!(w.status().spend.cost, Some(0.5));
    // After a handoff, a correction of a call before it leaves the context
    // unknown until the next reply.
    w.feed(
        "handoff_completed",
        None,
        &json!({"outcome": "completed", "tokens_before": 12}),
    );
    w.feed("usage_recorded", Some("a2"), &usage("g2", 7, Some(0.25)));
    assert!(w.status().context.is_none());
}

#[test]
fn spend_counts_every_usage_line_and_a_repeated_generation_once() {
    let mut w = world();
    w.start();
    w.feed("usage_recorded", Some("a1"), &usage("g1", 10, Some(1.0)));
    w.feed("usage_recorded", None, &usage("g2", 20, Some(2.0)));
    let mut copy = usage("g3", 30, Some(4.0));
    copy["origin_session_id"] = json!("s_delegate");
    w.feed("usage_recorded", Some("a1"), &copy);
    w.feed("usage_recorded", Some("a1"), &usage("g1", 10, Some(1.0)));
    let spend = w.status().spend;
    assert_eq!(spend.tokens.input, 60);
    assert_eq!(spend.tokens.output, 15);
    assert_eq!(spend.cost, Some(7.0));
}

#[test]
fn a_turn_streams_and_a_started_call_is_a_tool_until_it_completes() {
    let mut w = world();
    w.start();
    assert!(w.feed("turn_started", None, &prompt("go")));
    assert_eq!(w.status().state, SessionState::Streaming);
    w.feed(
        "tool_call_requested",
        Some("a1"),
        &json!({"name": "shell", "arguments": {}}),
    );
    // Requested, not started: still streaming.
    assert_eq!(w.status().state, SessionState::Streaming);
    w.feed(
        "tool_call_started",
        Some("a1"),
        &json!({"effects": [], "reversible": true}),
    );
    assert_eq!(
        w.status().state,
        SessionState::Tool {
            tool: "shell".into()
        }
    );
    w.done("a1");
    assert_eq!(w.status().state, SessionState::Streaming);
    w.finish();
    assert_eq!(w.status().state, SessionState::Idle);
}

/// A crash leaves a call started and never completed; the resumed process
/// is not running it, but an approval the crash left pending still waits.
#[test]
fn a_resumed_process_does_not_report_the_crashed_process_s_running_call() {
    let mut w = world();
    w.start();
    w.prompt("go");
    w.call("a1", "shell");
    assert_eq!(
        w.status().state,
        SessionState::Tool {
            tool: "shell".into()
        }
    );
    assert!(w.feed(
        "fiber_started",
        None,
        &json!({"version": "0.0.1", "resumed": true})
    ));
    assert_eq!(w.status().state, SessionState::Streaming);
    w.call("a2", "read");
    w.ask("a3", "r1");
    w.feed(
        "fiber_started",
        None,
        &json!({"version": "0.0.1", "resumed": true}),
    );
    assert_eq!(waiting_of(&w).request_id.0, "r1");
}

#[test]
fn the_latest_started_call_names_the_tool_and_an_earlier_one_returns() {
    let mut w = world();
    w.start();
    w.prompt("go");
    w.call("a1", "read");
    w.call("a2", "shell");
    assert_eq!(
        w.status().state,
        SessionState::Tool {
            tool: "shell".into()
        }
    );
    w.done("a2");
    assert_eq!(
        w.status().state,
        SessionState::Tool {
            tool: "read".into()
        }
    );
    w.done("a1");
    assert_eq!(w.status().state, SessionState::Streaming);
}

#[test]
fn a_pending_approval_outranks_a_running_call_and_a_tool_follows_its_clearing() {
    let mut w = world();
    w.start();
    w.prompt("go");
    w.call("a1", "read");
    w.feed(
        "tool_call_requested",
        Some("a2"),
        &json!({"name": "shell", "arguments": {}}),
    );
    w.ask("a2", "r1");
    let waiting = |request: &str, kind, summary: &str| SessionState::Waiting {
        waiting: Waiting {
            request_id: contract::RequestId(request.to_owned()),
            kind,
            summary: summary.to_owned(),
        },
    };
    assert_eq!(
        w.status().state,
        waiting("r1", WaitingKind::Approval, "shell")
    );
    w.allow("a2", "r1");
    assert_eq!(
        w.status().state,
        SessionState::Tool {
            tool: "read".into()
        }
    );
}

#[test]
fn an_unmatched_resolve_clears_nothing() {
    let mut w = world();
    w.start();
    w.prompt("go");
    w.feed(
        "tool_call_requested",
        Some("a1"),
        &json!({"name": "shell", "arguments": {}}),
    );
    w.ask("a1", "r1");
    w.feed(
        "permission_resolved",
        Some("a1"),
        &json!({"decision": "deny", "decided_by": "standing_rule"}),
    );
    w.allow("a1", "other");
    assert!(matches!(w.status().state, SessionState::Waiting { .. }));
    w.allow("a1", "r1");
    assert_eq!(w.status().state, SessionState::Streaming);
}

#[test]
fn two_pending_requests_name_the_earlier_and_resolving_it_names_the_other() {
    let mut w = world();
    w.start();
    w.prompt("go");
    w.feed(
        "tool_call_requested",
        Some("a1"),
        &json!({"name": "shell", "arguments": {}}),
    );
    w.feed(
        "tool_call_requested",
        Some("a2"),
        &json!({"name": "write", "arguments": {}}),
    );
    w.ask("a1", "r1");
    let first_since = w.status().since;
    w.ask("a2", "r2");
    let s = w.status();
    assert_eq!(s.since, first_since);
    let named = waiting_of(&w);
    let (request_id, summary) = (named.request_id, named.summary);
    assert_eq!((request_id.0.as_str(), summary.as_str()), ("r1", "shell"));
    // The earlier resolved first: `waiting` changes and `since` restarts.
    w.allow("a1", "r1");
    let waiting = waiting_of(&w);
    assert_eq!(
        (waiting.request_id.0.as_str(), waiting.summary.as_str()),
        ("r2", "write")
    );
    assert_eq!(w.status().since, w.ts);
}

#[test]
fn a_question_summarises_its_first_prompt_on_one_line() {
    let mut w = world();
    w.start();
    w.prompt("go");
    let waiting = waiting_of;
    w.feed(
        "interaction_requested",
        None,
        &json!({"request_id": "q1", "kind": "form", "fields": [
            {"header": "h", "question": "Which\none?", "options": []},
            {"header": "h2", "question": "Second", "options": []}
        ]}),
    );
    let got = waiting(&w);
    assert_eq!(got.kind, WaitingKind::Question);
    assert_eq!(got.summary, "Which one?");
    w.feed(
        "interaction_resolved",
        None,
        &json!({"request_id": "q1", "by": "person", "declined": true}),
    );
    assert_eq!(w.status().state, SessionState::Streaming);
    w.feed(
        "interaction_requested",
        None,
        &json!({"request_id": "q2", "kind": "confirm", "prompt": "Sure?"}),
    );
    assert_eq!(waiting(&w).summary, "Sure?");
    // An empty prompt is the tool name of the call that raised it.
    w.feed(
        "interaction_resolved",
        None,
        &json!({"request_id": "q2", "by": "person", "declined": true}),
    );
    w.feed(
        "tool_call_requested",
        Some("a1"),
        &json!({"name": "ask_user", "arguments": {}}),
    );
    w.feed(
        "interaction_requested",
        None,
        &json!({"request_id": "q3", "kind": "text_input", "prompt": "", "action_ids": ["a1"]}),
    );
    assert_eq!(waiting(&w).summary, "ask_user");
}

#[test]
fn a_retry_notice_is_retrying_until_the_next_message_or_step() {
    let mut w = world();
    w.start();
    w.prompt("go");
    w.feed("step_started", None, &json!({}));
    assert!(w.feed(
        "retry_scheduled",
        None,
        &json!({"code": "rate_limited", "attempt": 2, "delay_ms": 1, "last_attempt": 4})
    ));
    assert_eq!(w.status().state, SessionState::Retrying);
    w.feed("assistant_message_started", Some("a1"), &json!({}));
    assert_eq!(w.status().state, SessionState::Streaming);
    w.feed(
        "retry_scheduled",
        None,
        &json!({"code": "rate_limited", "attempt": 2, "delay_ms": 1, "last_attempt": 4}),
    );
    w.feed("step_started", None, &json!({}));
    assert_eq!(w.status().state, SessionState::Streaming);
    w.feed(
        "retry_scheduled",
        None,
        &json!({"code": "rate_limited", "attempt": 2, "delay_ms": 1, "last_attempt": 4}),
    );
    w.finish();
    assert_eq!(w.status().state, SessionState::Idle);
}

#[test]
fn an_ephemeral_delta_and_an_unknown_or_unreadable_line_change_nothing() {
    let mut w = world();
    w.start();
    w.prompt("go");
    let before = w.status();
    let delta = envelope(
        "assistant_message_delta",
        5000,
        Some("a1"),
        None,
        &json!({"text": "hi"}),
    );
    assert!(!w.fold.observe(&delta));
    let unknown = envelope("from_the_future", 5001, None, Some(90), &json!({"x": 1}));
    assert!(!w.fold.observe(&unknown));
    let unreadable = envelope(
        "turn_started",
        5002,
        None,
        Some(91),
        &json!({"input": "no"}),
    );
    assert!(!w.fold.observe(&unreadable));
    assert_eq!(w.status(), before);
}

#[test]
fn since_is_the_line_that_began_the_state_and_a_no_op_line_leaves_it() {
    let mut w = world();
    w.start();
    w.prompt("go");
    let began = w.ts;
    assert_eq!(w.status().since, began);
    // A line that leaves the state as it was.
    assert!(!w.feed("step_started", None, &json!({})));
    assert_eq!(w.status().since, began);
    w.call("a1", "read");
    let tool_began = w.ts;
    assert_eq!(w.status().since, tool_began);
    w.feed("usage_recorded", Some("a1"), &usage("g1", 1, None));
    assert_eq!(w.status().since, tool_began);
    // A tool to tool change of name restarts it.
    w.call("a2", "shell");
    assert_eq!(w.status().since, w.ts);
    w.done("a2");
    assert_eq!(w.status().since, w.ts);
}

#[test]
fn a_changed_field_is_reported_and_an_unchanged_one_is_not() {
    let mut w = world();
    w.start();
    assert!(w.feed("session_named", None, &json!({"name": "a", "by": "model"})));
    assert!(!w.feed("session_named", None, &json!({"name": "a", "by": "person"})));
    assert!(w.feed("session_named", None, &json!({"name": "b", "by": "person"})));
}

fn job_started(id: &str) -> Value {
    json!({"job_id": id, "description": "d", "output_path": "/o"})
}

fn delegate_started(id: &str) -> Value {
    json!({
        "job_id": id, "delegate_session_id": "s_d", "harness": "fiber",
        "model": "fake/m", "workspace": "/w"
    })
}

#[test]
fn jobs_run_after_a_turn_and_the_state_flips_with_no_turn_running() {
    let mut w = world();
    w.start();
    w.live();
    assert_eq!(w.status().state, SessionState::Idle);
    *w.running.lock().unwrap() = vec![job("j1")];
    assert!(w.feed("job_started", None, &job_started("j1")));
    let s = w.status();
    assert_eq!((s.state, s.jobs, s.delegates), (SessionState::Jobs, 1, 0));
    w.running.lock().unwrap().clear();
    w.feed(
        "job_completed",
        None,
        &json!({"job_id": "j1", "status": "completed"}),
    );
    let s = w.status();
    assert_eq!((s.state, s.jobs), (SessionState::Idle, 0));
}

#[test]
fn a_turn_in_flight_outranks_jobs_and_ends_in_jobs_when_one_runs() {
    let mut w = world();
    w.start();
    w.live();
    *w.running.lock().unwrap() = vec![job("j1")];
    w.prompt("go");
    assert_eq!(w.status().state, SessionState::Streaming);
    w.finish();
    assert_eq!(w.status().state, SessionState::Jobs);
}

#[test]
fn delegates_are_counted_apart_from_jobs() {
    let mut w = world();
    w.start();
    w.live();
    *w.running.lock().unwrap() = vec![job("j1"), job("j2")];
    w.feed("job_started", None, &job_started("j1"));
    w.feed("delegate_started", None, &delegate_started("j2"));
    w.feed("job_started", None, &job_started("j2"));
    let s = w.status();
    assert_eq!((s.jobs, s.delegates), (1, 1));
    // Only a delegate running: the state is still `jobs`.
    *w.running.lock().unwrap() = vec![job("j2")];
    w.feed(
        "job_completed",
        None,
        &json!({"job_id": "j1", "status": "completed"}),
    );
    let s = w.status();
    assert_eq!((s.state, s.jobs, s.delegates), (SessionState::Jobs, 0, 1));
    w.feed(
        "delegate_finished",
        None,
        &json!({
            "job_id": "j2", "text": "",
            "usage": {"tokens": {"input": 0, "cache_read": 0, "cache_write": {}, "output": 0},
                      "cost": 0.0, "subscription_cost": 0.0}
        }),
    );
    let s = w.status();
    assert_eq!((s.jobs, s.delegates), (1, 0));
}

#[test]
fn a_delegate_is_dropped_on_its_end_even_without_its_finish() {
    // An orphaned delegate resumed without its finish leaves no
    // `delegate_finished`: its bare `job_completed` still removes it.
    let mut w = world();
    w.start();
    w.live();
    *w.running.lock().unwrap() = vec![job("j2")];
    w.feed("delegate_started", None, &delegate_started("j2"));
    assert_eq!(w.status().delegates, 1);
    w.feed(
        "job_completed",
        None,
        &json!({"job_id": "j2", "status": "completed"}),
    );
    assert_eq!(w.status().delegates, 0);
}

#[test]
fn history_is_folded_without_reading_the_jobs_or_the_branch_until_live() {
    let mut w = world();
    *w.running.lock().unwrap() = vec![job("j1")];
    *w.branch.lock().unwrap() = Some(Git {
        branch: Some("main".into()),
    });
    *w.count.lock().unwrap() = 2;
    w.start();
    w.prompt("go");
    w.feed("job_started", None, &job_started("j1"));
    w.finish();
    let s = w.status();
    assert_eq!((s.state, s.jobs, s.git), (SessionState::Idle, 0, None));
    assert_eq!((s.clients, *w.reads.lock().unwrap()), (0, 0));
    w.live();
    let s = w.status();
    assert_eq!((s.state, s.jobs), (SessionState::Jobs, 1));
    assert_eq!(s.clients, 2);
    assert_eq!(
        s.git,
        Some(Git {
            branch: Some("main".into())
        })
    );
}

#[test]
fn a_clients_line_reads_the_count_and_leaves_since() {
    let mut w = world();
    w.start();
    w.prompt("go");
    w.live();
    let since = w.status().since;
    *w.count.lock().unwrap() = 2;
    w.ts += 1;
    let line = envelope("clients", w.ts, None, None, &json!({"count": 2}));
    assert!(w.fold.observe(&line));
    let s = w.status();
    assert_eq!(s.clients, 2);
    assert_eq!(s.since, since);
    // The same count again changes nothing.
    w.ts += 1;
    let line = envelope("clients", w.ts, None, None, &json!({"count": 2}));
    assert!(!w.fold.observe(&line));
}

#[test]
fn a_settled_clients_count_emits_no_status_line() {
    // Red on base if the fold ever reads the line's own count instead of the settled one.
    let mut w = world();
    w.start();
    w.prompt("go");
    w.live();
    assert_eq!(w.status().clients, 0);
    // The 0 -> 1 -> 0 change settles to 0 before the fold sees the lagging line.
    *w.count.lock().unwrap() = 1;
    *w.count.lock().unwrap() = 0;
    w.ts += 1;
    let line = envelope("clients", w.ts, None, None, &json!({"count": 1}));
    assert!(!w.fold.observe(&line));
    assert_eq!(w.status().clients, 0);
    // A count still held at 1 while the fold reads does emit a line.
    *w.count.lock().unwrap() = 1;
    w.ts += 1;
    let line = envelope("clients", w.ts, None, None, &json!({"count": 1}));
    assert!(w.fold.observe(&line));
    assert_eq!(w.status().clients, 1);
}

#[test]
fn the_count_is_read_from_live_on_at_each_durable_line_and_not_on_a_delta() {
    let mut w = world();
    *w.count.lock().unwrap() = 3;
    w.start();
    w.prompt("go");
    assert_eq!(w.status().clients, 0);
    assert_eq!(*w.reads.lock().unwrap(), 0);
    w.live();
    assert_eq!(w.status().clients, 3);
    *w.count.lock().unwrap() = 1;
    assert!(w.feed("step_started", None, &json!({})));
    assert_eq!(w.status().clients, 1);
    let reads = *w.reads.lock().unwrap();
    let delta = envelope(
        "assistant_message_delta",
        5000,
        Some("a1"),
        None,
        &json!({"text": "hi"}),
    );
    assert!(!w.fold.observe(&delta));
    assert_eq!(*w.reads.lock().unwrap(), reads);
}

#[test]
fn project_is_the_session_directorys_grandparent() {
    use std::path::Path;
    assert_eq!(
        super::project_of(Path::new("/h/projects/-a-b/sessions/s_1")),
        "-a-b"
    );
    assert_eq!(super::project_of(Path::new("/s_1")), "");
    let mut w = world();
    w.fold = Fold::new(
        "-p".to_owned(),
        "/w".to_owned(),
        "fake/m".to_owned(),
        None,
        {
            let running = Arc::clone(&w.running);
            Box::new(move || running.lock().unwrap().clone())
        },
        {
            let branch = Arc::clone(&w.branch);
            Box::new(move || branch.lock().unwrap().clone())
        },
        {
            let (count, reads) = (Arc::clone(&w.count), Arc::clone(&w.reads));
            Box::new(move || {
                *reads.lock().unwrap() += 1;
                *count.lock().unwrap()
            })
        },
    );
    assert_eq!(w.status().project, "-p");
    w.start();
    w.prompt("go");
    w.live();
    assert_eq!(w.status().project, "-p");
}

/// A lagging status watcher drops the `clients` line, but the fold reads
/// the latest count at the next durable line it folds while live, so the
/// count shows in `session_status` by the next durable line at the latest.
/// The hold is in the count reader's second call, which returns the value
/// it read before the hold, so neither `go_live` nor the held line can
/// supply the 2: only a later durable line can.
#[test]
fn a_dropped_clients_line_is_read_at_the_next_durable_line() {
    use contract::events::{
        Clients, Empty, Event, InputItem, TurnCompleted, TurnOutcome, TurnStarted,
    };
    use contract::shapes::{ContentPart, Origin, Sender};
    use contract::{CommandId, TurnId};

    let root = fakes::TempDir::new("status-clients-lag");
    let log = Arc::new(
        log::Log::create(
            root.path(),
            SessionId("s_1".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap(),
    );
    let weak = Arc::downgrade(&log);
    let (held, observer_held) = std::sync::mpsc::channel::<()>();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let held_read = Mutex::new(Some((held, released)));
    let calls = Mutex::new(0u32);
    let status = super::start(
        &log,
        Fold::new(
            "-w".to_owned(),
            "/w".to_owned(),
            "fake/m".to_owned(),
            None,
            Box::new(Vec::new),
            Box::new(|| None),
            Box::new(move || {
                let mut calls = calls.lock().unwrap();
                *calls += 1;
                match *calls {
                    1 => super::clients_of(&weak),
                    2 => {
                        let seen = super::clients_of(&weak);
                        if let Some((held, released)) = held_read.lock().unwrap().take() {
                            held.send(()).unwrap();
                            // The test's own wait fails first and drops the sender, which releases this hold.
                            released.recv().unwrap();
                        }
                        seen
                    }
                    _ => super::clients_of(&weak),
                }
            }),
        ),
    )
    .expect("an observer");
    let turn = Some(TurnId("t_1".into()));
    log.append(
        &Event::TurnStarted(TurnStarted {
            input: vec![InputItem::Message {
                content: vec![ContentPart::Text { text: "go".into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: Some(CommandId("c_1".into())),
                },
                changed_by: None,
            }],
        }),
        turn.clone(),
        None,
    )
    .unwrap();
    observer_held
        .recv_timeout(DEADLINE)
        .expect("the observer is held");
    // More lines than a watcher's queue holds: the rest are dropped.
    for _ in 0..2_000 {
        log.append(&Event::StepStarted(Empty {}), turn.clone(), None)
            .unwrap();
    }
    log.append(&Event::Clients(Clients { count: 2 }), None, None)
        .unwrap();
    log.append(
        &Event::TurnCompleted(TurnCompleted {
            outcome: TurnOutcome::Completed,
            error: None,
            questions: None,
        }),
        turn,
        None,
    )
    .unwrap();

    status.signal();
    release.send(()).unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        status.join();
        done_tx.send(()).unwrap();
    });
    done_rx.recv_timeout(DEADLINE).expect("the observer ended");

    // The log keeps the latest status it was handed.
    let last = log.latest("session_status").expect("a session_status");
    assert_eq!(last.payload["clients"], json!(2));
    assert_eq!(last.payload["state"], "idle");
}

#[test]
fn the_branch_is_read_at_each_turn_start_and_a_change_is_a_change() {
    let mut w = world();
    w.start();
    *w.branch.lock().unwrap() = Some(Git {
        branch: Some("main".into()),
    });
    w.live();
    assert_eq!(
        w.status().git,
        Some(Git {
            branch: Some("main".into())
        })
    );
    w.prompt("one");
    w.finish();
    *w.branch.lock().unwrap() = Some(Git { branch: None });
    assert!(w.feed("turn_started", None, &prompt("two")));
    assert_eq!(w.status().git, Some(Git { branch: None }));
    *w.branch.lock().unwrap() = None;
    w.finish();
    assert_eq!(w.status().git, Some(Git { branch: None }));
    w.feed("turn_started", None, &prompt("three"));
    assert_eq!(w.status().git, None);
}

/// A running observer held on its first jobs read while the log is written
/// past its queue's capacity and the stop line is queued: released, it still
/// folds the lines the queue dropped, since the kept stop line comes ahead of
/// them, so the last status is the final one. The observer signals that it
/// is held, and the test releases it only after the stop line is queued.
#[test]
fn a_lagging_observer_folds_every_written_line_before_it_stops() {
    use contract::events::{
        Empty, Event, InputItem, TurnCompleted, TurnOutcome, TurnStarted, UsageRecorded,
    };
    use contract::shapes::{ContentPart, Origin, Sender, Tokens};
    use contract::{CommandId, GenerationId, TurnId};

    let root = fakes::TempDir::new("status-lag");
    let log = Arc::new(
        log::Log::create(
            root.path(),
            SessionId("s_1".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap(),
    );
    let (held, observer_held) = std::sync::mpsc::channel::<()>();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let held_read = Mutex::new(Some((held, released)));
    let status = super::start(
        &log,
        Fold::new(
            "-w".to_owned(),
            "/w".to_owned(),
            "fake/m".to_owned(),
            None,
            Box::new(move || {
                if let Some((held, released)) = held_read.lock().unwrap().take() {
                    held.send(()).unwrap();
                    // The test's own wait fails first and drops the sender, which releases this hold.
                    released.recv().unwrap();
                }
                Vec::new()
            }),
            Box::new(|| None),
            Box::new(|| 0),
        ),
    )
    .expect("an observer");
    observer_held
        .recv_timeout(DEADLINE)
        .expect("the observer is held");
    let turn = Some(TurnId("t_1".into()));
    let append = |event: Event| log.append(&event, turn.clone(), None).unwrap();
    append(Event::TurnStarted(TurnStarted {
        input: vec![InputItem::Message {
            content: vec![ContentPart::Text { text: "go".into() }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_1".into())),
            },
            changed_by: None,
        }],
    }));
    // More lines than a watcher's queue holds: the rest are dropped.
    for _ in 0..2_000 {
        append(Event::StepStarted(Empty {}));
    }
    append(Event::UsageRecorded(UsageRecorded {
        generation_id: GenerationId("g_1".into()),
        model: "fake/m".into(),
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: std::collections::BTreeMap::new(),
            output: 3,
        },
        web_searches: None,
        cost: Some(1.5),
        subscription: None,
        extension: None,
        origin_session_id: None,
        reviewer: None,
        input_bytes: 1,
        input_media: None,
    }));
    append(Event::TurnCompleted(TurnCompleted {
        outcome: TurnOutcome::Completed,
        error: None,
        questions: None,
    }));

    status.signal();
    release.send(()).unwrap();
    status.join();

    // The log keeps the latest status it was handed.
    let last = log.latest("session_status").expect("a session_status");
    assert_eq!(last.payload["state"], "idle");
    assert_eq!(last.payload["name"], "go");
    assert_eq!(last.payload["spend"]["cost"], 1.5);
    assert_eq!(last.payload["spend"]["tokens"]["input"], 10);
}

/// An ephemeral line is a delta and changes nothing, except a retry notice,
/// which is what `retrying` is read from.
#[test]
fn an_ephemeral_retry_notice_is_read_and_another_ephemeral_line_is_not() {
    let mut w = world();
    w.start();
    w.prompt("go");
    let ephemeral = |kind: &str, payload: &Value| envelope(kind, 99, None, None, payload);
    let delta = ephemeral("assistant_message_delta", &json!({"text": "hi"}));
    assert!(!w.fold.observe(&delta));
    assert_eq!(w.status().state, SessionState::Streaming);
    let retry = ephemeral(
        "retry_scheduled",
        &json!({"code": "rate_limited", "attempt": 2, "delay_ms": 1000, "last_attempt": 4}),
    );
    assert!(w.fold.observe(&retry));
    assert_eq!(w.status().state, SessionState::Retrying);
    w.feed("assistant_message_started", None, &json!({}));
    assert_eq!(w.status().state, SessionState::Streaming);
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// The branch read: none outside a repository, the branch's name on one,
/// and a detached head as a `null` branch.
#[test]
fn the_branch_is_read_from_git_and_a_detached_head_is_null() {
    let root = fakes::TempDir::new("status-git");
    let plain = root.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(super::branch(&plain), None);

    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "trunk"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    assert_eq!(
        super::branch(&repo),
        Some(Git {
            branch: Some("trunk".into())
        })
    );
    git(&repo, &["checkout", "-q", "--detach"]);
    assert_eq!(super::branch(&repo), Some(Git { branch: None }));
}

/// `Status::stop` returns only after the observer thread ended, having
/// folded every line already written: the observer owns a sender, so
/// `stop` returning means it was dropped.
#[test]
fn stop_returns_after_the_observer_folded_what_was_written() {
    use contract::events::{Event, InputItem, TurnCompleted, TurnOutcome, TurnStarted};
    use contract::shapes::{ContentPart, Origin, Sender};
    use contract::{CommandId, TurnId};

    let root = fakes::TempDir::new("status-stop");
    let log = Arc::new(
        log::Log::create(
            root.path(),
            SessionId("s_1".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap(),
    );
    let (held, observer_held) = std::sync::mpsc::channel::<()>();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let held_read = Mutex::new(Some((held, released)));
    let (exit_tx, exit_rx) = std::sync::mpsc::channel::<()>();
    let status = super::start(
        &log,
        Fold::new(
            "-w".to_owned(),
            "/w".to_owned(),
            "fake/m".to_owned(),
            None,
            Box::new(move || {
                let _owned = &exit_tx;
                if let Some((held, released)) = held_read.lock().unwrap().take() {
                    held.send(()).unwrap();
                    released.recv().unwrap();
                }
                Vec::new()
            }),
            Box::new(|| None),
            Box::new(|| 0),
        ),
    )
    .expect("an observer");
    observer_held
        .recv_timeout(DEADLINE)
        .expect("the observer is held");
    let turn = Some(TurnId("t_1".into()));
    log.append(
        &Event::TurnStarted(TurnStarted {
            input: vec![InputItem::Message {
                content: vec![ContentPart::Text { text: "go".into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: Some(CommandId("c_1".into())),
                },
                changed_by: None,
            }],
        }),
        turn.clone(),
        None,
    )
    .unwrap();
    log.append(
        &Event::TurnCompleted(TurnCompleted {
            outcome: TurnOutcome::Completed,
            error: None,
            questions: None,
        }),
        turn,
        None,
    )
    .unwrap();
    // `stop` runs on a worker so the test can prove it joined the thread:
    // the observer owns a sender, so `stop` returning means it was dropped.
    let (done_tx, done) = std::sync::mpsc::channel::<Result<(), std::sync::mpsc::TryRecvError>>();
    let _stopper = std::thread::spawn(move || {
        status.stop();
        done_tx.send(exit_rx.try_recv()).unwrap();
    });
    release.send(()).unwrap();
    let that = done.recv_timeout(DEADLINE).expect("stop returned");
    assert_eq!(
        that,
        Err(std::sync::mpsc::TryRecvError::Disconnected),
        "stop returned before the observer thread ended"
    );
    let last = log.latest("session_status").expect("a session_status");
    assert_eq!(last.payload["state"], "idle");
    assert_eq!(last.payload["name"], "go");
}

fn offer_items(n: usize) -> Value {
    let item = json!({"kind": "mcp_server", "name": "db", "hash": "h", "required": false,
        "summary": "MCP server: db"});
    Value::Array(vec![item; n])
}

impl World {
    fn offer(&mut self, request: &str, items: usize) {
        self.feed(
            "repository_code_offered",
            None,
            &json!({"request_id": request, "items": offer_items(items)}),
        );
    }

    fn resolve_offer(&mut self, request: &str) {
        self.feed(
            "repository_code_resolved",
            None,
            &json!({"request_id": request, "decisions": []}),
        );
    }

    fn preamble(&mut self) {
        self.feed(
            "preamble_built",
            None,
            &json!({
                "reason": "start", "model": "fake/built", "context_window": 1000,
                "tool_choice": "auto", "cache_lifetime": "5m", "system_prompt": "", "tools": []
            }),
        );
    }
}

#[test]
fn an_offer_waits_with_its_item_count_until_it_resolves() {
    let mut w = world();
    w.start();
    w.offer("r_o", 3);
    let waiting = waiting_of(&w);
    assert_eq!(waiting.kind, WaitingKind::Offer);
    assert_eq!(waiting.request_id.0, "r_o");
    assert_eq!(waiting.summary, "3 items from the repository");
    w.resolve_offer("r_o");
    assert_eq!(w.status().state, SessionState::Idle);
    w.offer("r_p", 1);
    assert_eq!(waiting_of(&w).summary, "1 item from the repository");
}

#[test]
fn an_offer_left_unanswered_stops_waiting_at_the_preamble() {
    let mut w = world();
    w.start();
    w.offer("r_o", 2);
    w.preamble();
    assert_eq!(w.status().state, SessionState::Idle);
}

#[test]
fn an_offer_shows_before_an_approval_an_earlier_process_left() {
    let mut w = world();
    w.start();
    w.prompt("go");
    w.ask("a1", "r1");
    w.feed(
        "fiber_started",
        None,
        &json!({"version": "0.0.1", "resumed": true}),
    );
    w.offer("r_o", 1);
    assert_eq!(waiting_of(&w).request_id.0, "r_o");
    w.resolve_offer("r_o");
    let waiting = waiting_of(&w);
    assert_eq!(waiting.request_id.0, "r1");
    assert_eq!(waiting.kind, WaitingKind::Approval);
}

#[test]
fn an_offer_raised_again_leaves_one_entry() {
    let mut w = world();
    w.start();
    w.offer("r_o", 1);
    w.offer("r_o", 1);
    w.resolve_offer("r_o");
    assert_eq!(w.status().state, SessionState::Idle);
}

#[test]
fn the_preamble_leaves_a_pending_approval_waiting() {
    let mut w = world();
    w.start();
    w.prompt("go");
    w.ask("a1", "r1");
    w.preamble();
    assert_eq!(waiting_of(&w).request_id.0, "r1");
}
