//! Continuing a turn after `pause_turn` (`docs/loop.md`, "A reply paused by
//! a hosted tool"): the loop resends the request with the paused reply's
//! blocks appended, at most 5 times in a row, then fails `paused_too_long`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use contract::ProviderCallId;
use contract::events::{CallStatus, TextCompleted, ToolCallCompleted, TurnOutcome};
use contract::provider::{Finish, Input, ReplyAction};
use fakes::{Scripted, call_usage};
use serde_json::{Value, json};

use support::{MODEL, Session, delivery};

/// A paused reply of `text` with `content` as its raw blocks.
fn paused(text: &str, content: Vec<Value>) -> Scripted {
    let mut scripted = Scripted::text(text);
    let reply = scripted.end.as_mut().unwrap();
    reply.finish = Finish::Paused { content };
    scripted
}

fn text_block(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

fn call_block(id: &str) -> Value {
    json!({"type": "server_tool_use", "id": id, "name": "web_search",
        "input": {"query": "fiber"}})
}

fn result_block(id: &str) -> Value {
    json!({"type": "web_search_tool_result", "tool_use_id": id, "content": []})
}

/// A conversation item resending `block` unchanged.
fn resent(block: Value) -> Input {
    Input::Assistant {
        model: MODEL.into(),
        text: String::new(),
        provider_item: Some(block),
    }
}

/// A hosted result whose call is not in its reply: the loop joins it
/// against the paused blocks by its id, or drops it.
fn stub(id: &str, result: Value) -> ReplyAction {
    ReplyAction::HostedResult {
        provider_id: ProviderCallId(id.into()),
        completed: ToolCallCompleted {
            status: CallStatus::Completed,
            reason: None,
            error: None,
            process: None,
            content: Vec::new(),
            details: None,
            artifact: None,
            kept: None,
            changes: None,
            control: None,
            changed_by: None,
            protected_paths: None,
            provider_item: Some(result),
            hosts_surfaced: Vec::new(),
            loaded: None,
        },
    }
}

fn text_count(lines: &[contract::Envelope], text: &str) -> usize {
    lines
        .iter()
        .filter(|line| line.kind == "text_completed" && line.payload["text"].as_str() == Some(text))
        .count()
}

fn turn(lines: &[contract::Envelope]) -> &contract::Envelope {
    lines
        .iter()
        .find(|line| line.kind == "turn_completed")
        .unwrap()
}

#[test]
fn a_paused_reply_then_a_completed_one_continues_the_turn() {
    let content = vec![text_block("Searching."), call_block("srvtoolu_01")];
    let mut session = Session::new(
        vec![
            paused("Searching.", content.clone()),
            Scripted::text("Done."),
        ],
        None,
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    // Two requests: the first unchanged, the second with the paused reply's
    // blocks appended as assistant items, equal to the paused content.
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    let (first, second) = (&requests[0], &requests[1]);
    assert_eq!(
        second.conversation[first.conversation.len()..],
        [resent(content[0].clone()), resent(content[1].clone())]
    );
    assert!(
        first
            .conversation
            .iter()
            .all(|input| !matches!(input, Input::Assistant { .. }))
    );
    // The paused reply's text is in the log once, not twice.
    let lines = session.lines();
    assert_eq!(text_count(&lines, "Searching."), 1);
    assert_eq!(text_count(&lines, "Done."), 1);
}

#[test]
fn five_paused_replies_then_a_completed_one_succeeds_on_the_sixth_call() {
    let script: Vec<Scripted> = (0..5)
        .map(|n| paused("Hold.", vec![text_block(&format!("paused {n}"))]))
        .chain(std::iter::once(Scripted::text("Done.")))
        .collect();
    let mut session = Session::new(script, None);
    session.inbox.send(delivery("hi")).unwrap();
    // The boundary: the fifth continuation succeeds.
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(session.requests().len(), 6);
}

#[test]
fn a_sixth_pause_fails_the_turn_as_paused_too_long() {
    let script: Vec<Scripted> = (0..6)
        .map(|_| paused("Hold.", vec![text_block("paused")]))
        .collect();
    let mut session = Session::new(script, None);
    session.inbox.send(delivery("hi")).unwrap();
    // The boundary: the sixth pause fails, with no seventh request.
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    assert_eq!(session.requests().len(), 6);
    let lines = session.lines();
    let end = turn(&lines);
    assert_eq!(end.payload["outcome"], "failed");
    assert_eq!(end.payload["error"]["code"], "paused_too_long");
    assert!(
        end.payload["error"]["message"]
            .as_str()
            .unwrap()
            .contains("pause_turn"),
        "{}",
        end.payload["error"]["message"]
    );
    // The paused replies' text lines are kept.
    assert_eq!(text_count(&lines, "Hold."), 6);
}

#[test]
fn each_continuation_appends_every_paused_reply_so_far_in_order() {
    let first = vec![text_block("one")];
    let second = vec![text_block("two")];
    let third = vec![text_block("three")];
    let mut session = Session::new(
        vec![
            paused("one", first.clone()),
            paused("two", second.clone()),
            paused("three", third.clone()),
            Scripted::text("Done."),
        ],
        None,
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let requests = session.requests();
    assert_eq!(requests.len(), 4);
    let tail = |n: usize, k: usize| requests[n].conversation[k..].to_vec();
    let base = requests[0].conversation.len();
    assert_eq!(tail(1, base), [resent(first[0].clone())]);
    assert_eq!(
        tail(2, base),
        [resent(first[0].clone()), resent(second[0].clone())]
    );
    assert_eq!(
        tail(3, base),
        [
            resent(first[0].clone()),
            resent(second[0].clone()),
            resent(third[0].clone())
        ]
    );
}

#[test]
fn the_continuation_count_ends_with_the_step() {
    let mut session = Session::new(
        vec![
            paused("First.", vec![text_block("first")]),
            Scripted::text("Done."),
            paused("Hold.", vec![text_block("paused")]),
            paused("Hold.", vec![text_block("paused")]),
            paused("Hold.", vec![text_block("paused")]),
            paused("Hold.", vec![text_block("paused")]),
            paused("Hold.", vec![text_block("paused")]),
            Scripted::text("Done again."),
        ],
        None,
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(session.requests().len(), 2);
    // A second turn's step can again take 5 continuations.
    session.inbox.send(delivery("again")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(session.requests().len(), 8);
}

#[test]
fn a_budget_reached_after_a_pause_sends_no_continuation() {
    let mut scripted = paused("Searching.", vec![text_block("Searching.")]);
    scripted.end.as_mut().unwrap().tokens.input = 2_000_000;
    let model = r#loop::Model {
        reference: MODEL.into(),
        cost: Some(contract::provider::Cost {
            input: 1.0,
            output: 0.0,
            cache_read: None,
            cache_write: None,
            tiers: Vec::new(),
        }),
        subscription: false,
    };
    // The scripted spend is far past the default handoff trigger; this turn
    // is about the budget alone.
    let mut session = Session::open(vec![scripted], Vec::new(), Vec::new(), model)
        .handoff(r#loop::HandoffSettings {
            enabled: false,
            ..r#loop::HandoffSettings::default()
        })
        .budget(Some(1.0));
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    assert_eq!(session.requests().len(), 1);
    let lines = session.lines();
    let end = turn(&lines);
    assert_eq!(end.payload["outcome"], "failed");
    assert_eq!(end.payload["error"]["code"], "budget_exceeded");
}

#[test]
fn a_cancel_during_a_continuation_ends_the_turn_interrupted() {
    let mut session = Session::new(
        vec![
            paused("Working.", vec![text_block("Working.")]),
            Scripted::cancelled_after(call_usage("gen_2")),
        ],
        None,
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Interrupted));
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn an_empty_pause_is_a_normal_reply() {
    let mut session = Session::new(
        vec![
            paused("Still here.", Vec::new()),
            paused("Still here.", Vec::new()),
        ],
        None,
    );
    session.inbox.send(delivery("hi")).unwrap();
    // One request, `Completed`, and no `paused_too_long` however many times
    // an empty pause is scripted.
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn a_result_split_from_its_call_by_a_pause_joins_in_the_continuation() {
    let block_a = text_block("A");
    let call = call_block("srvtoolu_01");
    let result = result_block("srvtoolu_01");
    let mut first = paused("A", vec![block_a.clone(), call.clone()]);
    first.end.as_mut().unwrap().actions = vec![ReplyAction::Text(TextCompleted {
        text: "A".into(),
        provider_item: Some(block_a.clone()),
    })];
    let mut second = Scripted::text("B");
    second.end.as_mut().unwrap().actions = vec![
        stub("srvtoolu_01", result.clone()),
        ReplyAction::Text(TextCompleted {
            text: "B".into(),
            provider_item: None,
        }),
    ];
    let mut session = Session::new(vec![first, second, Scripted::text("Again.")], None);
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(session.requests().len(), 2);
    let lines = session.lines();
    // The pair is logged under the continuation's message with the paused
    // call's block as `provider_item`, before its text.
    let order: Vec<&str> = lines
        .iter()
        .filter(|line| {
            matches!(
                line.kind.as_str(),
                "text_completed"
                    | "assistant_message_completed"
                    | "assistant_message_started"
                    | "tool_call_requested"
                    | "tool_call_started"
                    | "tool_call_completed"
            )
        })
        .map(|line| line.kind.as_str())
        .collect();
    assert_eq!(
        order,
        [
            "assistant_message_started",
            "text_completed",
            "assistant_message_completed",
            "assistant_message_started",
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed",
            "text_completed",
            "assistant_message_completed",
        ]
    );
    let requested = lines
        .iter()
        .find(|line| line.kind == "tool_call_requested")
        .unwrap();
    let completed = lines
        .iter()
        .find(|line| line.kind == "tool_call_completed")
        .unwrap();
    let started = lines
        .iter()
        .find(|line| line.kind == "tool_call_started")
        .unwrap();
    assert_eq!(requested.payload["provider_item"], call);
    assert_eq!(completed.payload["provider_item"], result);
    assert_eq!(requested.action_id, started.action_id);
    assert_eq!(requested.action_id, completed.action_id);
    // The continuation request holds A's block and the call block.
    let requests = session.requests();
    let base = requests[0].conversation.len();
    assert_eq!(
        requests[1].conversation[base..],
        [resent(block_a.clone()), resent(call.clone())]
    );
    // A second turn's request holds both raw blocks, in order, between A
    // and B, equal to the blocks sent.
    session.inbox.send(delivery("again")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let requests = session.requests();
    assert_eq!(requests.len(), 3);
    let conversation = &requests[2].conversation;
    let at = conversation
        .iter()
        .position(|input| {
            matches!(input, Input::Assistant { text, provider_item, .. }
                if text == "A" && *provider_item == Some(block_a.clone()))
        })
        .unwrap();
    assert_eq!(conversation[at + 1], resent(call.clone()));
    assert_eq!(conversation[at + 2], resent(result.clone()));
    assert!(matches!(
        &conversation[at + 3],
        Input::Assistant { text, provider_item: None, .. } if text == "B"
    ));
}

#[test]
fn a_stub_with_no_paused_call_writes_nothing() {
    let mut orphan = Scripted::text("");
    orphan.end.as_mut().unwrap().actions = vec![stub("srvtoolu_99", result_block("srvtoolu_99"))];
    let runs = [
        vec![orphan, Scripted::text("Done.")],
        vec![Scripted::text(""), Scripted::text("Done.")],
    ];
    let mut conversations = Vec::new();
    let mut all_kinds = Vec::new();
    for script in runs {
        let mut session = Session::new(script, None);
        session.inbox.send(delivery("hi")).unwrap();
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        session.inbox.send(delivery("Second.")).unwrap();
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let lines = session.lines();
        assert!(
            lines.iter().all(|line| line.kind != "tool_call_requested"),
            "no hosted line is written"
        );
        all_kinds.push(
            lines
                .iter()
                .map(|line| line.kind.clone())
                .collect::<Vec<_>>(),
        );
        conversations.push(session.requests()[1].conversation.clone());
    }
    assert_eq!(all_kinds[0], all_kinds[1]);
    // Its lines and the next conversation equal a reply with no such action.
    // Each session's opening environment text names its own directory, so
    // the comparison starts after it.
    assert_eq!(conversations[0][1..], conversations[1][1..]);
}

/// A stub whose id names no paused call is dropped: the paused list is
/// non-empty but lacks the id, so no hosted pair is logged. With `||`
/// the paused call's `server_tool_use` type alone would match and log a
/// hosted pair under the wrong call.
#[test]
fn a_stub_with_a_different_id_than_the_paused_call_is_dropped() {
    let call = call_block("srvtoolu_01");
    let mut second = Scripted::text("B");
    second.end.as_mut().unwrap().actions = vec![
        stub("srvtoolu_99", result_block("srvtoolu_99")),
        ReplyAction::Text(TextCompleted {
            text: "B".into(),
            provider_item: None,
        }),
    ];
    let mut session = Session::new(
        vec![
            paused("one", vec![call.clone()]),
            second,
            Scripted::text("Done."),
        ],
        None,
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert!(
        lines.iter().all(|line| line.kind != "tool_call_requested"),
        "no hosted line is written"
    );
    assert_eq!(text_count(&lines, "B"), 1);
}

#[test]
fn two_pauses_log_the_pair_once_and_resend_both_replies() {
    let call = call_block("srvtoolu_01");
    let result = result_block("srvtoolu_01");
    let second_text = text_block("two");
    let mut second = Scripted::text("two");
    {
        let reply = second.end.as_mut().unwrap();
        reply.actions = vec![
            stub("srvtoolu_01", result.clone()),
            ReplyAction::Text(TextCompleted {
                text: "two".into(),
                provider_item: None,
            }),
        ];
        reply.finish = Finish::Paused {
            content: vec![result.clone(), second_text.clone()],
        };
    }
    let mut session = Session::new(
        vec![
            paused("one", vec![call.clone()]),
            second,
            Scripted::text("Done."),
        ],
        None,
    );
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let requests = session.requests();
    assert_eq!(requests.len(), 3);
    // Request 3 holds the content of replies 1 and 2, in order.
    let base = requests[0].conversation.len();
    assert_eq!(
        requests[2].conversation[base..],
        [
            resent(call.clone()),
            resent(result.clone()),
            resent(second_text.clone())
        ]
    );
    // The pair is logged once, in the second reply's message.
    let lines = session.lines();
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.kind == "tool_call_requested")
            .count(),
        1
    );
    let requested = lines
        .iter()
        .find(|line| line.kind == "tool_call_requested")
        .unwrap();
    assert_eq!(requested.payload["provider_item"], call);
}
