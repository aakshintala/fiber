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

use contract::events::{
    CacheLifetime, ReasoningCompleted, TextCompleted, TextDelta, ToolCallRequested, TurnOutcome,
};
use contract::provider::{Cost, Delta, Input, InputSize, ReplyAction};
use contract::shapes::Failure;
use contract::{ActionId, ErrorCode, Seq};
use fakes::{Scripted, reply};
use r#loop::rebuild;
use serde_json::{Value, json};

use support::{
    MODEL, Session, TestTool, delivery, kinds, message, reasoning_item, reasoning_reply, steer,
    tool_call_reply,
};

fn user(text: &str) -> Input {
    Input::User {
        text: text.into(),
        images: Vec::new(),
    }
}

fn assistant(text: &str) -> Input {
    Input::Assistant {
        model: MODEL.into(),
        text: text.into(),
        provider_item: None,
    }
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
    // The first is the prompt. The others are steers taken while the loop is
    // still idle, so they join this turn's input (`docs/loop.md`, "Starting
    // a turn").
    session.inbox.send(delivery("one")).unwrap();
    session.inbox.send(steer("two")).unwrap();
    session.inbox.send(steer("three")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
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
        ]
    );
    let input: Vec<&str> = lines[3].payload["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["content"][0]["text"].as_str().unwrap())
        .collect();
    assert_eq!(input, ["one", "two", "three"]);
    assert_eq!(lines[3].payload["input"][0]["command_id"], "c_one");
    let requests = session.requests();
    assert_eq!(requests.len(), 1);
    // `conversation[0]` is the opening message.
    assert_eq!(
        requests[0].conversation[1..],
        [user("one"), user("two"), user("three")]
    );
    assert!(
        requests[0].system_prompt.contains("operating inside Fiber"),
        "{}",
        requests[0].system_prompt
    );
    assert!(
        requests[0].system_prompt.contains("fake/model-1"),
        "{}",
        requests[0].system_prompt
    );
}

#[test]
fn a_step_writes_its_events_and_ephemeral_deltas_carry_no_seq() {
    let mut session = Session::new(vec![Scripted::text("Hello there.")], None);
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    let turn = lines[3].turn_id.clone().unwrap();
    let action = lines[5].action_id.clone().unwrap();
    for line in &lines[3..] {
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
    let part = lines.iter().find(|l| l.kind == "text_completed").unwrap();
    assert_eq!(part.action_id.as_ref(), Some(&action));
    assert_eq!(part.payload["text"], "Hello there.");
    let completed = lines
        .iter()
        .find(|l| l.kind == "assistant_message_completed")
        .unwrap();
    assert_eq!(completed.action_id.as_ref(), Some(&action));
    assert_eq!(completed.payload["outcome"], "completed");
    let usage = lines.iter().find(|l| l.kind == "usage_recorded").unwrap();
    assert_eq!(usage.action_id.as_ref(), Some(&action));
    assert_eq!(usage.payload["model"], MODEL);
    assert_eq!(usage.payload["generation_id"], "gen_1");
    assert_eq!(usage.payload["tokens"]["input"], 10);
    assert_eq!(usage.payload["input_bytes"], 1000);
    assert!(usage.payload.get("input_media").is_none());
    assert_eq!(usage.payload["cost"], Value::Null);
    assert!(usage.payload.get("subscription").is_none());
    assert_eq!(lines.last().unwrap().payload["outcome"], "completed");

    // The durable lines are the log, byte for byte.
    let durable: Vec<_> = lines.into_iter().filter(|l| l.seq.is_some()).collect();
    assert_eq!(log::read(&session.dir).unwrap(), durable);
    let seqs: Vec<Seq> = durable.iter().map(|l| l.seq.unwrap()).collect();
    assert_eq!(seqs, (0..10).map(Seq).collect::<Vec<_>>());
}

#[test]
fn a_call_that_sent_media_records_input_media() {
    let mut scripted = Scripted::text("Done.");
    scripted.end.as_mut().unwrap().input_size = InputSize {
        bytes: 2048,
        media: true,
    };
    let mut session = Session::new(vec![scripted], None);
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
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
        ]
    );
    let usage = lines.iter().find(|l| l.kind == "usage_recorded").unwrap();
    assert_eq!(usage.payload["input_bytes"], 2048);
    assert_eq!(usage.payload["input_media"], true);
}

#[test]
fn a_message_sent_during_the_final_reply_continues_the_turn() {
    let mut session = Session::new(
        vec![Scripted::text("First."), Scripted::text("Second.")],
        Some(message("also this")),
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        durable(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "steering_queue",
            "step_started",
            "steering_applied",
            "steering_queue",
            "assistant_message_started",
            "text_completed",
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
        session.requests()[1].conversation[1..],
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
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    let kinds = kinds(&lines);
    let second_step = kinds.iter().rposition(|k| *k == "step_started").unwrap();
    assert_eq!(kinds[second_step + 1], "steering_queue");
    assert_eq!(kinds[second_step + 2], "steering_applied");
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
    session.inbox.send(delivery("hi")).unwrap();
    let before = session.log.fsyncs();
    session.turn();
    assert_eq!(session.requests().len(), 2);
    assert_eq!(session.log.fsyncs() - before, 4);

    session.inbox.send(delivery("again")).unwrap();
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
    session.inbox.send(delivery("one")).unwrap();
    session.turn();
    // The log's file is gone; only memory can supply the first turn.
    std::fs::remove_file(session.dir.join("events.jsonl")).unwrap();
    session.inbox.send(delivery("two")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let requests = session.requests();
    assert_eq!(
        requests[1].conversation[1..],
        [user("one"), assistant("First."), user("two")]
    );
    // The second request starts where the first ended, and both carry the
    // session's own id as the cache key and the preamble's settings.
    assert_eq!(requests[0].previous_end, None);
    assert_eq!(requests[1].previous_end, Some(2));
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
    session.inbox.send(delivery("one")).unwrap();
    session.turn();
    session.inbox.send(delivery("two")).unwrap();
    session.turn();
    let lines = log::read(&session.dir).unwrap();
    // The log up to the last request is what that request was built from.
    let last = lines
        .iter()
        .rposition(|l| l.kind == "assistant_message_started")
        .unwrap();
    let sent = session.requests().pop().unwrap().conversation;
    assert_eq!(rebuild(&lines[..last], MODEL).unwrap(), sent);

    // In log order: a reply's text parts sit among its calls, and the
    // calls' results follow the reply.
    let call = |n: usize| {
        let Input::ToolCall {
            action_id, call, ..
        } = &sent[n]
        else {
            panic!("{:?}", sent[n]);
        };
        (action_id.clone(), call.name.clone())
    };
    let result = |n: usize| {
        let Input::ToolResult {
            action_id,
            text,
            is_error,
            ..
        } = &sent[n]
        else {
            panic!("{:?}", sent[n]);
        };
        assert!(text.contains("No tool is named"), "{text}");
        assert!(is_error);
        action_id.clone()
    };
    assert_eq!(sent[1], user("one"));
    assert!(matches!(sent[2], Input::Reasoning { .. }));
    assert_eq!(sent[3], assistant("Hi."));
    assert_eq!(sent[4], user("steer"));
    assert_eq!(sent[5], assistant("Checking."));
    let (first, first_name) = call(6);
    let (second, second_name) = call(7);
    assert_eq!(
        (first_name.as_str(), second_name.as_str()),
        ("get_weather", "get_time")
    );
    assert_eq!(result(8), first);
    assert_eq!(result(9), second);
    assert_eq!(sent[10], assistant("Done."));
    assert_eq!(sent[11], user("two"));
    assert_eq!(sent.len(), 12);
}

#[test]
fn rebuild_reads_only_durable_lines_and_refuses_a_bad_one() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    session.inbox.send(delivery("one")).unwrap();
    session.turn();
    let mut lines = session.lines();
    assert_eq!(
        rebuild(&lines, MODEL).unwrap()[1..],
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
    session.inbox.send(delivery("one")).unwrap();
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
    assert_ne!(started.action_id, lines[5].action_id);
    assert_eq!(completed.payload["provider_item"], reasoning_item("Think."));

    session.inbox.send(delivery("two")).unwrap();
    session.turn();
    assert_eq!(
        session.requests()[1].conversation[2],
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
    // stream with text between them. The text part stays after them: the
    // reply's actions are replaced below, and `reply` had put it first.
    let text = std::mem::take(&mut end.actions);
    end.actions = vec![reasoning(""), reasoning("First."), reasoning("Second.")];
    end.actions.extend(text);
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
    session.inbox.send(delivery("one")).unwrap();
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
    session.inbox.send(delivery("weather?")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        durable(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let arguments = lines
        .iter()
        .find(|l| l.kind == "tool_call_arguments_delta")
        .unwrap();
    assert_eq!(arguments.action_id, lines[5].action_id);
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
    assert_ne!(requested.action_id, lines[5].action_id);
    assert_eq!(completed.action_id, requested.action_id);
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "unknown_tool");
    let sent = &session.requests()[1].conversation;
    assert!(matches!(&sent[2], Input::ToolCall { call, .. } if call.name == "get_weather"));
    assert_eq!(
        sent[3],
        Input::ToolResult {
            action_id: requested.action_id.clone().unwrap(),
            text: completed.payload["content"][0]["text"]
                .as_str()
                .unwrap()
                .into(),
            is_error: true,
            images: Vec::new()
        }
    );
}

#[test]
fn a_failed_model_call_fails_the_turn_with_its_code() {
    let failure = Failure {
        code: ErrorCode::InvalidRequest,
        message: "The provider rejected the request.".into(),
        retry_after_ms: None,
        provider: None,
    };
    let mut session = Session::new(vec![Scripted::failed(failure)], None);
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let call = &lines[6].payload;
    assert_eq!(call["outcome"], "failed");
    assert!(call.get("attempt").is_none());
    assert_eq!(call["error"]["code"], "invalid_request");
    assert_eq!(lines[7].payload["outcome"], "failed");
    assert_eq!(lines[7].payload["error"], call["error"]);
    // A failed call sends nothing to the model.
    assert_eq!(
        rebuild(&log::read(&session.dir).unwrap(), MODEL).unwrap()[1..],
        [user("hi")]
    );
}

#[test]
fn a_reply_logs_each_text_part_among_its_other_items() {
    let part = |text: &str| {
        ReplyAction::Text(TextCompleted {
            text: text.into(),
            provider_item: None,
        })
    };
    let mut first = reply("");
    first.actions = vec![
        part("A"),
        ReplyAction::ToolCall(ToolCallRequested {
            name: "get_weather".into(),
            arguments: json!({"city": "Paris"}),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        }),
        part("B"),
    ];
    let mut last = reply("");
    last.actions = vec![part("A"), part("B")];
    let mut session = Session::with_tools(
        vec![
            Scripted {
                deltas: Vec::new(),
                end: Ok(first),
            },
            Scripted {
                deltas: Vec::new(),
                end: Ok(last),
            },
        ],
        None,
        vec![std::sync::Arc::new(TestTool::reads(
            "get_weather",
            "Sunny.",
        ))],
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));

    let lines = session.lines();
    let started: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.kind == "assistant_message_started")
        .map(|(i, _)| i)
        .collect();
    let span = &lines[started[0]..started[1]];
    let message_id = span[0].action_id.clone();
    assert_eq!(
        durable(span),
        [
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
        ]
    );
    let texts: Vec<&str> = span
        .iter()
        .filter(|l| l.kind == "text_completed")
        .map(|l| {
            assert_eq!(l.action_id, message_id);
            l.payload["text"].as_str().unwrap()
        })
        .collect();
    assert_eq!(texts, ["A", "B"]);

    let sent = session.requests()[1].conversation.clone();
    let logged = log::read(&session.dir).unwrap();
    let opened = logged
        .iter()
        .rposition(|l| l.kind == "assistant_message_started")
        .unwrap();
    assert_eq!(rebuild(&logged[..opened], MODEL).unwrap(), sent);
    assert_eq!(sent[1], user("hi"));
    assert_eq!(sent[2], assistant("A"));
    assert!(matches!(&sent[3], Input::ToolCall { call, .. } if call.name == "get_weather"));
    assert_eq!(sent[4], assistant("B"));
    assert!(matches!(&sent[5], Input::ToolResult { text, .. } if text == "Sunny."));

    r#loop::fiber_exited(&session.log, &session.dir, Ok(()), true, None).unwrap();
    let exited = log::read(&session.dir).unwrap();
    assert_eq!(exited.last().unwrap().kind, "fiber_exited");
    assert_eq!(exited.last().unwrap().payload["text"], "AB");
}

#[test]
fn the_loop_runs_turns_until_every_sender_is_gone() {
    let mut session = Session::new(vec![Scripted::text("Hi."), Scripted::text("Bye.")], None);
    session.inbox.send(delivery("hi")).unwrap();
    session.inbox.send(steer("and")).unwrap();
    let looped = session.looped.take().unwrap();
    let (done, finished) = std::sync::mpsc::channel();
    let ran = std::thread::spawn(move || done.send(looped.run().is_ok()).unwrap());
    let lines = session.lines();
    assert_eq!(lines[3].payload["input"].as_array().unwrap().len(), 2);
    session.inbox.send(delivery("bye")).unwrap();
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

/// Prices whose tier at 100 tokens differs from the base, so a loop that
/// priced only the base input would log a different cost.
fn tiered(subscription: bool) -> r#loop::Model {
    r#loop::Model {
        reference: MODEL.into(),
        cost: Some(contract::provider::Cost {
            input: 2.0,
            output: 8.0,
            cache_read: Some(0.5),
            cache_write: Some(4.0),
            tiers: vec![contract::provider::Tier {
                input_tokens_above: 100,
                input: 6.0,
                output: 20.0,
                cache_read: 1.0,
                cache_write: 8.0,
            }],
        }),
        subscription,
    }
}

/// A reply whose whole prompt is 105 tokens, over the tier above.
fn counted(text: &str) -> Scripted {
    let mut scripted = Scripted::text(text);
    let reply = scripted.end.as_mut().unwrap();
    reply.tokens = contract::shapes::Tokens {
        input: 40,
        cache_read: 50,
        cache_write: [("5m".into(), 15)].into(),
        output: 7,
    };
    scripted
}

fn tier_cost() -> f64 {
    (40.0 * 6.0 + 50.0 * 1.0 + 15.0 * 8.0 + 7.0 * 20.0) / 1_000_000.0
}

#[test]
fn a_priced_model_logs_the_cost_its_prices_give() {
    let mut session = Session::open(
        vec![counted("Hello.")],
        Vec::new(),
        Vec::new(),
        tiered(false),
    );
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let usage = session
        .lines()
        .into_iter()
        .find(|line| line.kind == "usage_recorded")
        .unwrap();
    assert_eq!(usage.payload["cost"].as_f64(), Some(tier_cost()));
    assert!(usage.payload.get("subscription").is_none());
}

#[test]
fn a_subscription_model_logs_an_estimate_apart_from_billed_spend() {
    let mut session = Session::open(
        vec![counted("Hello.")],
        Vec::new(),
        Vec::new(),
        tiered(true),
    );
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let usage = session
        .lines()
        .into_iter()
        .find(|line| line.kind == "usage_recorded")
        .unwrap();
    assert_eq!(usage.payload["subscription"], true);
    assert_eq!(usage.payload["cost"].as_f64(), Some(tier_cost()));

    r#loop::fiber_exited(&session.log, &session.dir, Ok(()), true, None).unwrap();
    let exited = log::read(&session.dir).unwrap();
    let totals = &exited.last().unwrap().payload["usage"];
    assert_eq!(totals["cost"], 0.0);
    assert_eq!(totals["subscription_cost"].as_f64(), Some(tier_cost()));
}

/// A model priced at $1 per million input tokens and nothing else, so a
/// reply's cost is its input-token count over a million.
fn per_token(subscription: bool) -> r#loop::Model {
    r#loop::Model {
        reference: MODEL.into(),
        cost: Some(contract::provider::Cost {
            input: 1.0,
            output: 0.0,
            cache_read: None,
            cache_write: None,
            tiers: Vec::new(),
        }),
        subscription,
    }
}

fn reply_of(text: &str, id: &str, input: u64) -> Scripted {
    let mut scripted = Scripted::text(text);
    let reply = scripted.end.as_mut().unwrap();
    reply.generation_id = contract::GenerationId(id.into());
    reply.tokens.input = input;
    reply.tokens.output = 0;
    scripted
}

fn tool_of(id: &str, input: u64) -> Scripted {
    let mut scripted = tool_call_reply("", &["get_weather"]);
    let reply = scripted.end.as_mut().unwrap();
    reply.generation_id = contract::GenerationId(id.into());
    reply.tokens.input = input;
    reply.tokens.output = 0;
    scripted
}

fn weather() -> std::sync::Arc<TestTool> {
    std::sync::Arc::new(TestTool::reads("get_weather", "Sunny."))
}

fn budgeted(
    script: Vec<Scripted>,
    during: Option<contract::inbox::Message>,
    model: r#loop::Model,
    usd: Option<f64>,
) -> Session {
    let during = during
        .into_iter()
        .map(|message| contract::inbox::Delivery::Steer(message, support::ignore()))
        .collect();
    // The scripted spend is far past the default handoff trigger; these
    // tests are about the budget alone.
    let mut session =
        Session::open(script, during, vec![weather()], model).handoff(r#loop::HandoffSettings {
            enabled: false,
            ..r#loop::HandoffSettings::default()
        });
    if let Some(usd) = usd {
        session = session.budget(Some(usd));
    }
    session
}

#[test]
fn spend_below_the_budget_sends_the_next_request() {
    let mut session = budgeted(
        vec![tool_of("gen_1", 500_000), reply_of("Done.", "gen_2", 100)],
        None,
        per_token(false),
        Some(1.0),
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn spend_at_the_budget_refuses_the_next_request() {
    let mut session = budgeted(
        vec![tool_of("gen_1", 1_000_000), reply_of("Done.", "gen_2", 1)],
        Some(message("later")),
        per_token(false),
        Some(1.0),
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    assert_eq!(session.requests().len(), 1);
    let lines = session.lines();
    assert_eq!(
        kinds(&lines)
            .iter()
            .filter(|kind| **kind == "assistant_message_started")
            .count(),
        1
    );
    assert!(kinds(&lines).contains(&"steering_applied"));
    let completed = lines.last().unwrap();
    assert_eq!(completed.payload["outcome"], "failed");
    assert_eq!(completed.payload["error"]["code"], "budget_exceeded");
    assert_eq!(
        completed.payload["error"]["message"],
        "The session reached its spending budget of $1.00 (budget.usd)."
    );
}

#[test]
fn a_call_that_crosses_the_budget_completes_and_the_next_is_refused() {
    let mut session = budgeted(
        vec![
            tool_of("gen_1", 600_000),
            tool_of("gen_2", 600_000),
            reply_of("Done.", "gen_3", 1),
        ],
        None,
        per_token(false),
        Some(1.0),
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    assert_eq!(session.requests().len(), 2);
    let lines = session.lines();
    assert_eq!(
        kinds(&lines)
            .iter()
            .filter(|kind| **kind == "usage_recorded")
            .count(),
        2
    );
    assert_eq!(
        kinds(&lines)
            .iter()
            .filter(|kind| **kind == "assistant_message_started")
            .count(),
        2
    );
    assert_eq!(
        lines.last().unwrap().payload["error"]["code"],
        "budget_exceeded"
    );
}

#[test]
fn subscription_spend_never_reaches_the_budget() {
    let mut session = budgeted(
        vec![
            tool_of("gen_1", 5_000_000),
            reply_of("Done.", "gen_2", 5_000_000),
        ],
        None,
        per_token(true),
        Some(0.01),
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn a_model_with_no_cost_never_reaches_the_budget() {
    let mut session = budgeted(
        vec![
            tool_call_reply("", &["get_weather"]),
            Scripted::text("Done."),
        ],
        None,
        r#loop::Model {
            reference: MODEL.into(),
            cost: None,
            subscription: false,
        },
        Some(0.01),
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn an_unset_budget_never_refuses() {
    let mut session = budgeted(
        vec![
            tool_of("gen_1", 5_000_000),
            reply_of("Done.", "gen_2", 5_000_000),
        ],
        None,
        per_token(false),
        None,
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn a_zero_budget_refuses_the_first_request() {
    let mut session = Session::open(
        vec![Scripted::text("Hello.")],
        Vec::new(),
        Vec::new(),
        per_token(false),
    )
    .budget(Some(0.0));
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    assert!(session.requests().is_empty());
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "turn_completed",
        ]
    );
    assert_eq!(
        lines.last().unwrap().payload["error"]["message"],
        "The session reached its spending budget of $0.00 (budget.usd)."
    );
}

#[test]
fn a_negative_budget_refuses_the_first_request() {
    let mut session = Session::open(
        vec![Scripted::text("Hello.")],
        Vec::new(),
        Vec::new(),
        per_token(false),
    )
    .budget(Some(-1.0));
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    assert!(session.requests().is_empty());
    assert_eq!(
        session.lines().last().unwrap().payload["error"]["message"],
        "The session reached its spending budget of $-1.00 (budget.usd)."
    );
}

#[test]
fn session_started_records_the_path_and_the_other_names_without_values() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    session.inbox.send(delivery("one")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let variables = &lines[0].payload["variables"];
    // No hub starts this session, so it keeps the environment it was given
    // (`docs/invocation.md`, "A session's environment").
    assert_eq!(variables["source"], "inherited");
    assert_eq!(variables["path"], std::env::var("PATH").unwrap());
    let names: Vec<&str> = variables["names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    assert!(!names.contains(&"PATH"));
    let mut expected: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .filter(|name| name != "PATH")
        .collect();
    expected.sort();
    assert_eq!(names, expected);
}

/// The texts of a `steering_queue` line's messages, oldest first.
fn queued(line: &contract::Envelope) -> Vec<&str> {
    line.payload["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["content"][0]["text"].as_str().unwrap())
        .collect()
}

#[test]
fn a_steer_arriving_during_a_failed_reply_starts_the_next_turn() {
    let failure = Failure {
        code: ErrorCode::InvalidRequest,
        message: "The provider rejected the request.".into(),
        retry_after_ms: None,
        provider: None,
    };
    let mut session = Session::new(
        vec![Scripted::failed(failure), Scripted::text("Recovered.")],
        Some(message("meanwhile")),
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let failed = session.lines();
    assert_eq!(
        kinds(&failed),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    // The failed turn ends without a last drain, so the steer is still
    // waiting: no `steering_applied` names it.
    assert!(failed.iter().all(|l| l.kind != "steering_applied"));
    // The next turn starts without a new prompt, carrying the steer on its
    // `turn_started`.
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let started = lines.iter().find(|l| l.kind == "turn_started").unwrap();
    let input: Vec<&str> = started.payload["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["content"][0]["text"].as_str().unwrap())
        .collect();
    assert_eq!(input, ["meanwhile"]);
}

#[test]
fn queued_steers_are_each_applied_in_order_and_listed_while_queued() {
    let tool: std::sync::Arc<dyn contract::tool::Tool> =
        std::sync::Arc::new(TestTool::reads("get_weather", "Sunny."));
    let mut session = Session::open(
        vec![
            tool_call_reply("", &["get_weather"]),
            Scripted::text("Done."),
        ],
        vec![steer("one"), steer("two")],
        vec![tool],
        r#loop::Model {
            reference: MODEL.into(),
            cost: None,
            subscription: false,
        },
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "steering_queue",
            "steering_queue",
            "steering_applied",
            "steering_applied",
            "steering_queue",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let queues: Vec<Vec<&str>> = lines
        .iter()
        .filter(|l| l.kind == "steering_queue")
        .map(queued)
        .collect();
    assert_eq!(queues, [vec!["one"], vec!["one", "two"], Vec::new()]);
    let applied: Vec<&str> = lines
        .iter()
        .filter(|l| l.kind == "steering_applied")
        .map(|l| l.payload["content"][0]["text"].as_str().unwrap())
        .collect();
    assert_eq!(applied, ["one", "two"]);
}

#[test]
fn a_dropped_steer_leaves_the_queue_listing() {
    let tool: std::sync::Arc<dyn contract::tool::Tool> =
        std::sync::Arc::new(TestTool::reads("get_weather", "Sunny."));
    let mut session = Session::open(
        vec![
            tool_call_reply("", &["get_weather"]),
            Scripted::text("Done."),
        ],
        vec![
            steer("keep"),
            steer("drop"),
            contract::inbox::Delivery::SteerDrop(
                contract::CommandId("c_drop".into()),
                support::ignore(),
            ),
        ],
        vec![tool],
        r#loop::Model {
            reference: MODEL.into(),
            cost: None,
            subscription: false,
        },
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "steering_queue",
            "steering_queue",
            "steering_queue",
            "steering_applied",
            "steering_queue",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let queues: Vec<Vec<&str>> = lines
        .iter()
        .filter(|l| l.kind == "steering_queue")
        .map(queued)
        .collect();
    assert_eq!(
        queues,
        [vec!["keep"], vec!["keep", "drop"], vec!["keep"], Vec::new()]
    );
    let applied: Vec<&str> = lines
        .iter()
        .filter(|l| l.kind == "steering_applied")
        .map(|l| l.payload["content"][0]["text"].as_str().unwrap())
        .collect();
    assert_eq!(applied, ["keep"]);
}

#[test]
fn a_reply_reporting_searches_records_their_count() {
    let mut end = reply("Done.");
    end.web_searches = Some(3);
    let mut scripted = Scripted::text("Done.");
    scripted.end = Ok(end);
    let mut session = Session::new(vec![scripted], None);
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
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
        ]
    );
    let usage = lines.iter().find(|l| l.kind == "usage_recorded").unwrap();
    assert_eq!(usage.payload["web_searches"], 3);

    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
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
        ]
    );
    let usage = lines.iter().find(|l| l.kind == "usage_recorded").unwrap();
    assert!(usage.payload.get("web_searches").is_none());
}

#[test]
fn a_reply_carrying_an_inline_cost_records_it_instead_of_the_declared_price() {
    let priced = r#loop::Model {
        reference: MODEL.into(),
        cost: Some(Cost {
            input: 100.0,
            output: 200.0,
            cache_read: None,
            cache_write: None,
            tiers: Vec::new(),
        }),
        subscription: false,
    };
    // The declared prices give 0.0016 for the reply's tokens; the
    // vendor's own figure stands instead.
    let mut end = reply("Done.");
    end.cost = Some(0.000123);
    let mut scripted = Scripted::text("Done.");
    scripted.end = Ok(end);
    let mut session = Session::open(vec![scripted], Vec::new(), Vec::new(), priced);
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
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
        ]
    );
    let usages: Vec<_> = lines
        .iter()
        .filter(|line| line.kind == "usage_recorded")
        .collect();
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0].payload["cost"], json!(0.000123));
}
