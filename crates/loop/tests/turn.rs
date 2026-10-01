//! One turn without tools, through the loop's public API, against the fake
//! provider server (`docs/loop.md`: "Starting a turn", "One step", "Ending a
//! turn", "What the model is sent").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use contract::Seq;
use contract::events::TurnOutcome;
use contract::provider::Input;
use fakes::Response;
use serde_json::{Value, json};

use support::{MODEL, Session, kinds, message, reasoning_item, reasoning_reply, text_reply};

fn body(session: &Session, n: usize) -> Value {
    serde_json::from_slice(&session.server.requests()[n].body).unwrap()
}

#[test]
fn messages_waiting_together_start_one_turn_in_arrival_order() {
    let mut session = Session::new(vec![text_reply("Done.")], None);
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
    assert_eq!(
        body(&session, 0)["input"],
        json!([
            {"role": "user", "content": "one"},
            {"role": "user", "content": "two"},
            {"role": "user", "content": "three"},
        ])
    );
    assert_eq!(session.server.requests().len(), 1);
}

#[test]
fn a_step_writes_its_events_and_ephemeral_deltas_carry_no_seq() {
    let mut session = Session::new(vec![text_reply("Hello there.")], None);
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
    assert_eq!(usage.payload["generation_id"], "resp_1");
    assert_eq!(usage.payload["tokens"]["input"], 6);
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
        vec![text_reply("First."), text_reply("Second.")],
        Some(message("also this")),
    );
    session.inbox.send(message("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let durable: Vec<&str> = kinds(&lines)
        .into_iter()
        .filter(|k| !k.ends_with("_delta"))
        .collect();
    assert_eq!(
        durable,
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
        body(&session, 1)["input"],
        json!([
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "First."},
            {"role": "user", "content": "also this"},
        ])
    );
}

#[test]
fn a_message_waiting_at_a_step_boundary_is_applied_by_that_step() {
    // The second message arrives while the first reply streams; the reply
    // calls a tool, so the next step's drain applies it.
    let mut session = Session::new(
        vec![support::tool_call_reply(), text_reply("Done.")],
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
            text_reply("First."),
            text_reply("Second."),
            text_reply("Third."),
        ],
        Some(message("more")),
    );
    session.inbox.send(message("hi")).unwrap();
    let before = session.log.fsyncs();
    session.turn();
    assert_eq!(session.server.requests().len(), 2);
    assert_eq!(session.log.fsyncs() - before, 4);

    session.inbox.send(message("again")).unwrap();
    let before = session.log.fsyncs();
    session.turn();
    assert_eq!(session.server.requests().len(), 3);
    assert_eq!(session.log.fsyncs() - before, 2);
}

#[test]
fn the_conversation_is_kept_in_memory_not_reread_from_the_log() {
    let mut session = Session::new(vec![text_reply("First."), text_reply("Second.")], None);
    session.inbox.send(message("one")).unwrap();
    session.turn();
    // The log's file is gone; only memory can supply the first turn.
    std::fs::remove_file(session.dir.join("events.jsonl")).unwrap();
    session.inbox.send(message("two")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        body(&session, 1)["input"],
        json!([
            {"role": "user", "content": "one"},
            {"role": "assistant", "content": "First."},
            {"role": "user", "content": "two"},
        ])
    );
}

#[test]
fn reasoning_is_logged_and_goes_back_only_to_the_model_that_produced_it() {
    let mut session = Session::new(
        vec![reasoning_reply("Think.", "Hi."), text_reply("Again.")],
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
    let sent = session.requests.lock().unwrap()[1].conversation.clone();
    assert_eq!(
        sent[1],
        Input::Reasoning {
            model: MODEL.into(),
            text: "Think.".into(),
            provider_item: Some(reasoning_item("Think.")),
        }
    );
    // Sent back unchanged to the model that produced it.
    assert_eq!(body(&session, 1)["input"][1], reasoning_item("Think."));
}

#[test]
fn reasoning_with_no_readable_text_still_opens_its_action() {
    let item = json!({"type": "reasoning", "id": "rs_2", "encrypted_content": "x", "summary": []});
    let mut session = Session::new(
        vec![support::stream(&[
            json!({"type": "response.output_item.done", "item": item}),
            json!({"type": "response.completed", "response": {"id": "r", "status": "completed", "usage": {}}}),
        ])],
        None,
    );
    session.inbox.send(message("one")).unwrap();
    session.turn();
    let lines = session.lines();
    let started = lines
        .iter()
        .position(|l| l.kind == "reasoning_started")
        .unwrap();
    assert_eq!(lines[started + 1].kind, "reasoning_completed");
    assert_eq!(lines[started + 1].action_id, lines[started].action_id);
    assert_eq!(lines[started + 1].payload["provider_item"], item);
}

#[test]
fn a_tool_call_with_no_tool_fails_unknown_tool_and_the_turn_continues() {
    let mut session = Session::new(vec![support::tool_call_reply(), text_reply("Sorry.")], None);
    session.inbox.send(message("weather?")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let durable: Vec<&str> = kinds(&lines)
        .into_iter()
        .filter(|k| !k.ends_with("_delta"))
        .collect();
    assert_eq!(
        durable,
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
    let input = &body(&session, 1)["input"];
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[2]["type"], "function_call_output");
    assert_eq!(input[2]["output"], completed.payload["content"][0]["text"]);
}

#[test]
fn a_failed_model_call_fails_the_turn_with_its_code() {
    let mut session = Session::new(
        vec![Response::status(400, r#"{"error":{"message":"bad"}}"#)],
        None,
    );
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
    let code = call["error"]["code"].clone();
    assert_ne!(code, Value::Null);
    assert_eq!(lines[5].payload["outcome"], "failed");
    assert_eq!(lines[5].payload["error"], call["error"]);
}

#[test]
fn the_loop_runs_turns_until_every_sender_is_gone() {
    let mut session = Session::new(vec![text_reply("Hi."), text_reply("Bye.")], None);
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
    assert!(finished.recv_timeout(support::DEADLINE).unwrap());
    ran.join().unwrap();
    assert_eq!(session.server.requests().len(), 2);
}
