//! One turn without tools, through the loop's public API, against a scripted
//! provider on the provider seam (`docs/loop.md`: "Starting a turn", "One
//! step", "Ending a turn", "What the model is sent").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use contract::events::{CacheLifetime, ReasoningCompleted, TextDelta, TurnOutcome};
use contract::provider::{Delta, Input, ReplyAction};
use contract::shapes::Failure;
use contract::{ActionId, ErrorCode, Seq};
use fakes::{Scripted, reply};
use r#loop::rebuild;
use serde_json::{Value, json};

use support::{MODEL, Session, kinds, message, reasoning_item, reasoning_reply, tool_call_reply};

fn user(text: &str) -> Input {
    Input::User { text: text.into() }
}

fn assistant(text: &str) -> Input {
    Input::Assistant { text: text.into() }
}

/// The durable kinds of `lines`, in order.
fn durable(lines: &[contract::Envelope]) -> Vec<&str> {
    kinds(lines)
        .into_iter()
        .filter(|k| !k.ends_with("_delta"))
        .collect()
}

#[test]
fn messages_waiting_together_start_one_turn_in_arrival_order() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    for text in ["one", "two", "three"] {
        session.inbox.send(message(text)).unwrap();
    }
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let input: Vec<&str> = lines[1].payload["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["content"][0]["text"].as_str().unwrap())
        .collect();
    assert_eq!(input, ["one", "two", "three"]);
    assert_eq!(lines[1].payload["input"][0]["command_id"], "c_one");
    let requests = session.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].conversation,
        [user("one"), user("two"), user("three")]
    );
    assert_eq!(requests[0].system_prompt, "You are terse.");
}

#[test]
fn a_step_writes_its_events_and_ephemeral_deltas_carry_no_seq() {
    let mut session = Session::new(vec![Scripted::text("Hello there.")], None);
    session.inbox.send(message("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    let turn = lines[1].turn_id.clone().unwrap();
    let action = lines[3].action_id.clone().unwrap();
    for line in &lines[1..] {
        assert_eq!(line.turn_id.as_ref(), Some(&turn), "{}", line.kind);
    }
    let deltas: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == "assistant_message_delta")
        .collect();
    let text: String = deltas
        .iter()
        .map(|l| l.payload["text"].as_str().unwrap())
        .collect();
    assert_eq!(text, "Hello there.");
    for delta in &deltas {
        assert_eq!(delta.seq, None);
        assert_eq!(delta.action_id.as_ref(), Some(&action));
    }
    let completed = lines
        .iter()
        .find(|l| l.kind == "assistant_message_completed")
        .unwrap();
    assert_eq!(completed.action_id.as_ref(), Some(&action));
    assert_eq!(completed.payload["outcome"], "completed");
    assert_eq!(completed.payload["text"], "Hello there.");
    let usage = lines.iter().find(|l| l.kind == "usage_recorded").unwrap();
    assert_eq!(usage.action_id.as_ref(), Some(&action));
    assert_eq!(usage.payload["model"], MODEL);
    assert_eq!(usage.payload["generation_id"], "gen_1");
    assert_eq!(usage.payload["tokens"]["input"], 10);
    assert_eq!(usage.payload["cost"], Value::Null);
    assert_eq!(lines.last().unwrap().payload["outcome"], "completed");

    // The durable lines are the log, byte for byte.
    let durable: Vec<_> = lines.into_iter().filter(|l| l.seq.is_some()).collect();
    assert_eq!(log::read(&session.dir).unwrap(), durable);
    let seqs: Vec<Seq> = durable.iter().map(|l| l.seq.unwrap()).collect();
    assert_eq!(seqs, (0..7).map(Seq).collect::<Vec<_>>());
}

#[test]
fn a_message_sent_during_the_final_reply_continues_the_turn() {
    let mut session = Session::new(
        vec![Scripted::text("First."), Scripted::text("Second.")],
        Some(message("also this")),
    );
    session.inbox.send(message("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        durable(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "step_started",
            "steering_applied",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let steering = lines.iter().find(|l| l.kind == "steering_applied").unwrap();
    assert_eq!(steering.payload["content"][0]["text"], "also this");
    assert_eq!(steering.payload["command_id"], "c_also this");
    assert_eq!(steering.payload["source"], "driver");
    assert_eq!(
        session.requests()[1].conversation,
        [user("hi"), assistant("First."), user("also this")]
    );
}

#[test]
fn a_message_waiting_at_a_step_boundary_is_applied_by_that_step() {
    // The second message arrives while the first reply streams; the reply
    // calls a tool, so the next step's drain applies it.
    let mut session = Session::new(
        vec![
            tool_call_reply("", &["get_weather"]),
            Scripted::text("Done."),
        ],
        Some(message("steer")),
    );
    session.inbox.send(message("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    let kinds = kinds(&lines);
    let second_step = kinds.iter().rposition(|k| *k == "step_started").unwrap();
    assert_eq!(kinds[second_step + 1], "steering_applied");
    assert_eq!(
        kinds.iter().filter(|k| **k == "steering_applied").count(),
        1
    );
}

#[test]
fn two_fsyncs_per_model_request() {
    let mut session = Session::new(
        vec![
            Scripted::text("First."),
            Scripted::text("Second."),
            Scripted::text("Third."),
        ],
        Some(message("more")),
    );
    session.inbox.send(message("hi")).unwrap();
    let before = session.log.fsyncs();
    session.turn();
    assert_eq!(session.requests().len(), 2);
    assert_eq!(session.log.fsyncs() - before, 4);

    session.inbox.send(message("again")).unwrap();
    let before = session.log.fsyncs();
    session.turn();
    assert_eq!(session.requests().len(), 3);
    assert_eq!(session.log.fsyncs() - before, 2);
}

#[test]
fn the_conversation_is_kept_in_memory_not_reread_from_the_log() {
    let mut session = Session::new(
        vec![Scripted::text("First."), Scripted::text("Second.")],
        None,
    );
    session.inbox.send(message("one")).unwrap();
    session.turn();
    // The log's file is gone; only memory can supply the first turn.
    std::fs::remove_file(session.dir.join("events.jsonl")).unwrap();
    session.inbox.send(message("two")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let requests = session.requests();
    assert_eq!(
        requests[1].conversation,
        [user("one"), assistant("First."), user("two")]
    );
    // The second request starts where the first ended, and both carry the
    // session's own id as the cache key and the preamble's settings.
    assert_eq!(requests[0].previous_end, None);
    assert_eq!(requests[1].previous_end, Some(1));
    for request in &requests {
        assert_eq!(request.cache_key, "s_test");
        assert_eq!(request.tool_choice, "auto");
        assert_eq!(request.cache_lifetime, CacheLifetime::OneHour);
        assert_eq!(request.tools, []);
    }
}

#[test]
fn the_conversation_in_memory_is_the_one_rebuilt_from_the_log() {
    let mut session = Session::new(
        vec![
            reasoning_reply("Think.", "Hi."),
            tool_call_reply("Checking.", &["get_weather", "get_time"]),
            Scripted::text("Done."),
        ],
        Some(message("steer")),
    );
    session.inbox.send(message("one")).unwrap();
    session.turn();
    session.inbox.send(message("two")).unwrap();
    session.turn();
    let lines = log::read(&session.dir).unwrap();
    // The log up to the last request is what that request was built from.
    let last = lines
        .iter()
        .rposition(|l| l.kind == "assistant_message_started")
        .unwrap();
    let sent = session.requests().pop().unwrap().conversation;
    assert_eq!(rebuild(&lines[..last], MODEL).unwrap(), sent);

    // In log order: the calls come before the message's text completes,
    // and their results after it.
    let call = |n: usize| {
        let Input::ToolCall { action_id, call } = &sent[n] else {
            panic!("{:?}", sent[n]);
        };
        (action_id.clone(), call.name.clone())
    };
    let result = |n: usize| {
        let Input::ToolResult {
            action_id,
            text,
            is_error,
        } = &sent[n]
        else {
            panic!("{:?}", sent[n]);
        };
        assert!(text.contains("No tool is named"), "{text}");
        assert!(is_error);
        action_id.clone()
    };
    assert_eq!(sent[0], user("one"));
    assert!(matches!(sent[1], Input::Reasoning { .. }));
    assert_eq!(sent[2], assistant("Hi."));
    assert_eq!(sent[3], user("steer"));
    let (first, first_name) = call(4);
    let (second, second_name) = call(5);
    assert_eq!(
        (first_name.as_str(), second_name.as_str()),
        ("get_weather", "get_time")
    );
    assert_eq!(sent[6], assistant("Checking."));
    assert_eq!(result(7), first);
    assert_eq!(result(8), second);
    assert_eq!(sent[9], assistant("Done."));
    assert_eq!(sent[10], user("two"));
    assert_eq!(sent.len(), 11);
}

#[test]
fn rebuild_reads_only_durable_lines_and_refuses_a_bad_one() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    session.inbox.send(message("one")).unwrap();
    session.turn();
    let mut lines = session.lines();
    assert_eq!(
        rebuild(&lines, MODEL).unwrap(),
        [user("one"), assistant("Hi.")]
    );
    let turn = lines.iter().position(|l| l.kind == "turn_started").unwrap();
    lines[turn].payload.insert("input".into(), json!(3));
    let error = rebuild(&lines, MODEL).unwrap_err();
    assert_eq!(error.code(), ErrorCode::LogCorrupt);
}

#[test]
fn reasoning_is_logged_and_goes_back_only_to_the_model_that_produced_it() {
    let mut session = Session::new(
        vec![reasoning_reply("Think.", "Hi."), Scripted::text("Again.")],
        None,
    );
    session.inbox.send(message("one")).unwrap();
    session.turn();
    let lines = session.lines();
    let started = lines
        .iter()
        .find(|l| l.kind == "reasoning_started")
        .unwrap();
    let delta = lines.iter().find(|l| l.kind == "reasoning_delta").unwrap();
    let completed = lines
        .iter()
        .find(|l| l.kind == "reasoning_completed")
        .unwrap();
    assert_eq!(delta.seq, None);
    assert_eq!(delta.payload["text"], "Think.");
    assert_eq!(delta.action_id, started.action_id);
    assert_eq!(completed.action_id, started.action_id);
    assert_ne!(started.action_id, lines[3].action_id);
    assert_eq!(completed.payload["provider_item"], reasoning_item("Think."));

    session.inbox.send(message("two")).unwrap();
    session.turn();
    assert_eq!(
        session.requests()[1].conversation[1],
        Input::Reasoning {
            model: MODEL.into(),
            text: "Think.".into(),
            provider_item: Some(reasoning_item("Think.")),
        }
    );
}

#[test]
fn each_reasoning_action_keeps_the_id_it_was_first_seen_with() {
    let reasoning = |text: &str| {
        ReplyAction::Reasoning(ReasoningCompleted {
            text: text.into(),
            provider_item: Some(reasoning_item(text)),
        })
    };
    let fragment = |text: &str| Delta::Reasoning(TextDelta { text: text.into() });
    let mut end = reply("Done.");
    // An item with no readable text streams nothing; the two readable ones
    // stream with text between them.
    end.actions = vec![reasoning(""), reasoning("First."), reasoning("Second.")];
    let script = Scripted {
        deltas: vec![
            fragment("Fir"),
            fragment("st."),
            Delta::Text(TextDelta {
                text: "Done.".into(),
            }),
            fragment("Second."),
        ],
        end: Ok(end),
    };
    let mut session = Session::new(vec![script], None);
    session.inbox.send(message("one")).unwrap();
    session.turn();
    let lines = session.lines();
    let ids = |kind: &str| -> Vec<ActionId> {
        lines
            .iter()
            .filter(|l| l.kind == kind)
            .map(|l| l.action_id.clone().unwrap())
            .collect()
    };
    let started = ids("reasoning_started");
    let deltas = ids("reasoning_delta");
    let completed = ids("reasoning_completed");
    assert_eq!(started.len(), 3);
    // Streamed: the first readable action, then the second.
    assert_eq!(
        deltas,
        [started[0].clone(), started[0].clone(), started[1].clone()]
    );
    // Completed in reply order: the unreadable one opened last.
    assert_eq!(
        completed,
        [started[2].clone(), started[0].clone(), started[1].clone()]
    );
    let texts: Vec<&str> = lines
        .iter()
        .filter(|l| l.kind == "reasoning_completed")
        .map(|l| l.payload["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["", "First.", "Second."]);
}

#[test]
fn a_tool_call_with_no_tool_fails_unknown_tool_and_the_turn_continues() {
    let mut session = Session::new(
        vec![
            tool_call_reply("", &["get_weather"]),
            Scripted::text("Sorry."),
        ],
        None,
    );
    session.inbox.send(message("weather?")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        durable(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let arguments = lines
        .iter()
        .find(|l| l.kind == "tool_call_arguments_delta")
        .unwrap();
    assert_eq!(arguments.action_id, lines[3].action_id);
    assert_eq!(arguments.seq, None);
    let requested = lines
        .iter()
        .find(|l| l.kind == "tool_call_requested")
        .unwrap();
    let completed = lines
        .iter()
        .find(|l| l.kind == "tool_call_completed")
        .unwrap();
    assert_eq!(requested.payload["name"], "get_weather");
    assert_ne!(requested.action_id, lines[3].action_id);
    assert_eq!(completed.action_id, requested.action_id);
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "unknown_tool");
    let sent = &session.requests()[1].conversation;
    assert!(matches!(&sent[1], Input::ToolCall { call, .. } if call.name == "get_weather"));
    assert_eq!(sent[2], assistant(""));
    assert_eq!(
        sent[3],
        Input::ToolResult {
            action_id: requested.action_id.clone().unwrap(),
            text: completed.payload["content"][0]["text"]
                .as_str()
                .unwrap()
                .into(),
            is_error: true,
        }
    );
}

#[test]
fn a_failed_model_call_fails_the_turn_with_its_code() {
    let failure = Failure {
        code: ErrorCode::ConnectionFailed,
        message: "The connection to the provider failed.".into(),
        retry_after: None,
        provider: None,
    };
    let mut session = Session::new(vec![Scripted::failed(failure)], None);
    session.inbox.send(message("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let call = &lines[4].payload;
    assert_eq!(call["outcome"], "failed");
    assert_eq!(call["attempt"], 1);
    assert_eq!(call["text"], "");
    assert_eq!(call["error"]["code"], "connection_failed");
    assert_eq!(lines[5].payload["outcome"], "failed");
    assert_eq!(lines[5].payload["error"], call["error"]);
    // A failed call sends nothing to the model.
    assert_eq!(
        rebuild(&log::read(&session.dir).unwrap(), MODEL).unwrap(),
        [user("hi")]
    );
}

#[test]
fn the_loop_runs_turns_until_every_sender_is_gone() {
    let mut session = Session::new(vec![Scripted::text("Hi."), Scripted::text("Bye.")], None);
    session.inbox.send(message("hi")).unwrap();
    session.inbox.send(message("and")).unwrap();
    let looped = session.looped.take().unwrap();
    let (done, finished) = std::sync::mpsc::channel();
    let ran = std::thread::spawn(move || done.send(looped.run().is_ok()).unwrap());
    let lines = session.lines();
    assert_eq!(lines[1].payload["input"].as_array().unwrap().len(), 2);
    session.inbox.send(message("bye")).unwrap();
    assert_eq!(
        session.lines().last().unwrap().payload["outcome"],
        "completed"
    );
    drop(session.inbox);
    assert!(
        finished
            .recv_timeout(support::DEADLINE)
            .expect("waited for the loop to finish")
    );
    ran.join().unwrap();
    assert_eq!(session.provider.requests().len(), 2);
}
