//! Step 7 through the loop's public API (`docs/permissions.md`, "The
//! reviewer"): the two stages, failures, counting and escalation, with a
//! scripted reviewer on the provider seam.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use contract::commands::{Remember, RememberScope, Reply, ReplyAnswer};
use contract::events::{CacheLifetime, Decision, TurnOutcome};
use contract::inbox::Delivery;
use contract::provider::{Input, ModelRequest};
use contract::rules::{Rule, RuleDecision, StandingRules};
use contract::shapes::{Effect, Failure};
use contract::tool::Tool;
use contract::{Envelope, ErrorCode, RequestId, SessionId};
use fakes::{Scripted, ScriptedProvider};
use log::Log;
use r#loop::{BlockLimits, Loop, Model, Reviewer};
use serde_json::{Value, json};

use support::{
    DEADLINE, FakeRules, MODEL, REVIEWER_MODEL, Session, TempDir, TestTool, calls_reply, delivery,
    ignore, kinds, on_request,
};

fn paris() -> Value {
    json!({"city": "Paris"})
}

/// A tool whose calls declare `executes`, with `subject` and `prefix` as its
/// tool reads them.
fn shell(subject: Option<&str>, prefix: Option<&str>) -> Arc<TestTool> {
    let mut tool = TestTool::declaring("shell", "Ran it.", vec![Effect::Executes], None);
    tool.subject = subject.map(str::to_owned);
    tool.prefix = prefix.map(str::to_owned);
    Arc::new(tool)
}

fn allow() -> ReplyAnswer {
    ReplyAnswer::Approval {
        decision: Decision::Allow,
        feedback: None,
        remember: None,
    }
}

fn deny(feedback: Option<&str>) -> ReplyAnswer {
    ReplyAnswer::Approval {
        decision: Decision::Deny,
        feedback: feedback.map(str::to_owned),
        remember: None,
    }
}

fn reply(request_id: RequestId, answer: ReplyAnswer) -> Delivery {
    Delivery::Reply(Reply { request_id, answer }, ignore())
}

/// Sends "go", runs one turn, and returns its lines.
fn go(session: &mut Session) -> Vec<Envelope> {
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.lines()
}

fn line<'a>(lines: &'a [Envelope], kind: &str) -> &'a Envelope {
    lines.iter().find(|l| l.kind == kind).unwrap()
}

fn completed(lines: &[Envelope]) -> Vec<&Envelope> {
    lines
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .collect()
}

fn text(line: &Envelope) -> &str {
    line.payload["content"][0]["text"].as_str().unwrap()
}

fn usages(lines: &[Envelope]) -> Vec<&Envelope> {
    lines
        .iter()
        .filter(|l| l.kind == "usage_recorded")
        .collect()
}

/// The full ordered kind list of a first turn whose first reply calls once
/// with no text and whose second says "Done.", with `middle` in place of
/// the decision lines and `reviews` reviewer `usage_recorded` lines after
/// the reply's own.
fn kinds_with(middle: &[&str], reviews: usize) -> Vec<String> {
    let mut kinds = vec!["session_started".to_owned()];
    kinds.extend(kinds_next(middle, reviews));
    kinds
}

/// As [`kinds_with`], for a turn after the first: no `session_started`.
fn kinds_next(middle: &[&str], reviews: usize) -> Vec<String> {
    let mut kinds = vec![
        "turn_started",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "tool_call_arguments_delta",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
    ];
    kinds.extend(vec!["usage_recorded"; reviews]);
    kinds.extend(middle.iter().copied());
    kinds.extend(
        [
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
        .iter()
        .copied(),
    );
    kinds.into_iter().map(str::to_owned).collect()
}

/// A turn whose reviewed call reaches a person: two reviewer usages, the
/// request and its answer, then "Done.". `ran` tells whether the person's
/// allow ran the call, `first` whether the turn opens the session.
fn kinds_escalated(ran: bool, first: bool) -> Vec<String> {
    let mut kinds = Vec::new();
    if first {
        kinds.push("session_started");
    }
    kinds.extend(
        [
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_requested",
            "permission_resolved",
        ]
        .iter()
        .copied(),
    );
    if ran {
        kinds.push("tool_call_started");
    }
    kinds.push("tool_call_completed");
    kinds.extend(
        [
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
        .iter()
        .copied(),
    );
    kinds.into_iter().map(str::to_owned).collect()
}
/// Watches the log for the turn's first `permission_requested`, then runs
/// `send`: the signal the loop is waiting for an answer. Bounded by
/// [`DEADLINE`] like [`on_request`].
fn on_close(session: &Session, send: impl FnOnce() + Send + 'static) -> thread::JoinHandle<()> {
    let mut watcher = session.log.watch();
    thread::spawn(move || {
        let (forward, waiting) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(line)) = watcher.recv() {
                if forward.send(line).is_err() {
                    return;
                }
            }
        });
        loop {
            let line = waiting
                .recv_timeout(DEADLINE)
                .expect("a permission_requested line");
            if line.kind == "permission_requested" {
                send();
                return;
            }
        }
    })
}
/// One turn whose reply calls `shell` once, reviewed with `review`: the
/// session, the tool, the reviewer and the turn's lines.
fn reviewed_turn(
    review: Vec<Scripted>,
) -> (Session, Arc<TestTool>, Arc<ScriptedProvider>, Vec<Envelope>) {
    let tool = shell(None, None);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    let reviewer = session.reviewer(review);
    let lines = go(&mut session);
    (session, tool, reviewer, lines)
}

/// Turns of one reviewed `shell` call each, answered with "Done.".
fn paired_turns(turns: usize) -> Vec<Scripted> {
    let mut script = Vec::new();
    for _ in 0..turns {
        script.push(calls_reply("", &[("shell", paris())]));
        script.push(Scripted::text("Done."));
    }
    script
}

fn user_text(input: &Input) -> &str {
    match input {
        Input::User { text } => text,
        Input::Assistant { .. }
        | Input::Reasoning { .. }
        | Input::ToolCall { .. }
        | Input::ToolResult { .. } => {
            panic!("not a user item: {input:?}")
        }
    }
}

#[test]
fn a_stage_1_allow_runs_the_call_with_one_token() {
    let (session, tool, reviewer, lines) = reviewed_turn(vec![Scripted::text("allow")]);
    assert_eq!(
        kinds(&lines),
        kinds_with(
            &[
                "permission_resolved",
                "tool_call_started",
                "tool_call_completed",
            ],
            1,
        )
    );
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "allow");
    assert_eq!(resolved.payload["decided_by"], "reviewer");
    assert_eq!(
        resolved.payload["reviewer"],
        json!({"model": REVIEWER_MODEL, "stage": 1})
    );
    assert_eq!(resolved.payload.get("reason"), None);
    assert!(!kinds(&lines).contains(&"permission_requested"));
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "completed");
    assert_eq!(tool.ran().len(), 1);

    let requests = reviewer.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.max_output_tokens, Some(1));
    assert_eq!(request.previous_end, None);
    // The session's own provider never receives a reviewer request.
    assert_eq!(session.requests().len(), 2);

    let recorded = usages(&lines)
        .into_iter()
        .filter(|l| l.payload["model"] == REVIEWER_MODEL)
        .collect::<Vec<_>>();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].action_id, None);
    assert_eq!(usages(&lines).len(), 3);
}

#[test]
fn the_request_body_holds_only_what_the_reviewer_is_shown() {
    let tool = shell(None, None);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    let reviewer = session.reviewer(vec![Scripted::text("allow")]);
    session.inbox.send(delivery("run the tests")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        kinds_with(
            &[
                "permission_resolved",
                "tool_call_started",
                "tool_call_completed",
            ],
            1,
        )
    );

    let requests: Vec<ModelRequest> = reviewer.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.system_prompt.starts_with("## shared\n"));
    assert!(request.tools.is_empty());
    assert_eq!(request.tool_choice, "auto");
    assert_eq!(request.effort, None);
    assert_eq!(request.cache_lifetime, CacheLifetime::OneHour);
    assert_eq!(request.cache_key, "s_test:reviewer");
    assert_eq!(request.conversation.len(), 4);
    assert_eq!(
        user_text(&request.conversation[0]),
        "The person: run the tests"
    );
    assert_eq!(
        user_text(&request.conversation[1]),
        r#"Tool call: {"tool":"shell","arguments":{"city":"Paris"}}"#
    );
    assert_eq!(
        user_text(&request.conversation[2]),
        format!(
            "Declared effects: {{\"effects\":[\"executes\"],\"reversible\":true}}\nWorkspace root: {}",
            session.workspace.display()
        )
    );
    assert!(user_text(&request.conversation[3]).starts_with("## first-pass\n"));
}

#[test]
fn a_check_then_an_allow_sends_two_requests_identical_up_to_the_stage() {
    let (session, tool, reviewer, lines) = reviewed_turn(vec![
        Scripted::text("check"),
        Scripted::text("allow looks fine"),
    ]);
    assert_eq!(
        kinds(&lines),
        kinds_with(
            &[
                "permission_resolved",
                "tool_call_started",
                "tool_call_completed",
            ],
            2,
        )
    );
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "allow");
    assert_eq!(
        resolved.payload["reviewer"],
        json!({"model": REVIEWER_MODEL, "stage": 2})
    );
    assert_eq!(resolved.payload["reason"], "looks fine");
    assert_eq!(tool.ran().len(), 1);

    let requests = reviewer.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].max_output_tokens, Some(1));
    assert_eq!(requests[1].max_output_tokens, None);
    assert_eq!(requests[0].previous_end, None);
    assert_eq!(requests[1].previous_end, Some(2));
    assert_eq!(requests[0].conversation[..3], requests[1].conversation[..3]);
    assert_eq!(requests[0].conversation.len(), 4);
    assert_eq!(requests[1].conversation.len(), 4);
    assert!(user_text(&requests[0].conversation[3]).starts_with("## first-pass\n"));
    assert!(user_text(&requests[1].conversation[3]).starts_with("## second-pass\n"));
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn a_block_returns_the_reason_and_the_turn_continues() {
    let (session, tool, reviewer, lines) = reviewed_turn(vec![
        Scripted::text("check"),
        Scripted::text("block force-pushes to main"),
    ]);
    assert_eq!(
        kinds(&lines),
        kinds_with(&["permission_resolved", "tool_call_completed"], 2)
    );
    assert!(!kinds(&lines).contains(&"permission_requested"));
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "deny");
    assert_eq!(resolved.payload["decided_by"], "reviewer");
    assert_eq!(resolved.payload["reason"], "force-pushes to main");
    assert_eq!(
        resolved.payload["reviewer"],
        json!({"model": REVIEWER_MODEL, "stage": 2})
    );
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "denied");
    assert_eq!(done.payload["reason"], "reviewer");
    assert_eq!(
        text(done),
        "The reviewer blocked this call: force-pushes to main Respect this boundary and \
         find another way to do the task. It did not run."
    );
    assert!(tool.ran().is_empty());
    assert_eq!(reviewer.requests().len(), 2);
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn a_second_call_is_reviewed_with_the_first_as_history() {
    let tool = shell(None, None);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris()), ("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    let reviewer = session.reviewer(vec![Scripted::text("allow"), Scripted::text("allow")]);
    let lines = go(&mut session);
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "permission_resolved",
            "usage_recorded",
            "permission_resolved",
            "tool_call_started",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
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
    assert_eq!(tool.ran().len(), 2);
    assert_eq!(completed(&lines).len(), 2);

    let requests = reviewer.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].conversation.len(), 4);
    assert_eq!(requests[1].conversation.len(), 5);
    // The second request holds the first call as history and ends with
    // itself: the call under review is its own history item, byte-identical
    // in every later request.
    assert_eq!(requests[1].conversation[1], requests[0].conversation[1]);
    assert_eq!(requests[1].conversation[2], requests[0].conversation[1]);
    assert_eq!(requests[1].previous_end, Some(2));
}

#[test]
fn an_unreadable_verdict_is_asked_for_once_more() {
    let (session, tool, reviewer, lines) =
        reviewed_turn(vec![Scripted::text("maybe"), Scripted::text("allow")]);
    assert_eq!(
        kinds(&lines),
        kinds_with(
            &[
                "permission_resolved",
                "tool_call_started",
                "tool_call_completed",
            ],
            2,
        )
    );
    assert_eq!(tool.ran().len(), 1);
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "allow");
    assert_eq!(
        resolved.payload["reviewer"],
        json!({"model": REVIEWER_MODEL, "stage": 1})
    );

    let requests = reviewer.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].conversation.len(), 5);
    assert!(user_text(&requests[1].conversation[4]).starts_with("Your reply could not be read: "));
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn a_verdict_still_unreadable_escalates() {
    let tool = shell(None, None);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    let reviewer = session.reviewer(vec![Scripted::text("maybe"), Scripted::text("perhaps")]);
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| inbox.send(reply(id, allow())).unwrap()
    });
    let lines = go(&mut session);
    answered.join().unwrap();
    assert_eq!(kinds(&lines), kinds_escalated(true, true));
    assert_eq!(tool.ran().len(), 1);

    let requested = line(&lines, "permission_requested");
    assert_eq!(requested.payload["step"], "review");
    assert_eq!(requested.payload["escalation"]["cause"], "reviewer_failed");
    assert_eq!(
        requested.payload["escalation"]["error"]["code"],
        "unreadable_reply"
    );
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "allow");
    assert_eq!(resolved.payload["decided_by"], "person");
    assert_eq!(reviewer.requests().len(), 2);
    assert!(kinds(&lines).contains(&"tool_call_started"));
}

#[test]
fn a_failed_reviewer_call_escalates_with_its_failure() {
    let tool = shell(None, None);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    let reviewer = session.reviewer(vec![Scripted::failed(Failure {
        code: ErrorCode::Timeout,
        message: "the reviewer timed out".into(),
        retry_after: None,
        provider: None,
    })]);
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| inbox.send(reply(id, allow())).unwrap()
    });
    let lines = go(&mut session);
    answered.join().unwrap();
    // The failed call writes no usage: only the reply's own line.
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_requested",
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
        ]
    );

    let requested = line(&lines, "permission_requested");
    assert_eq!(requested.payload["step"], "review");
    assert_eq!(
        requested.payload["escalation"],
        json!({
            "cause": "reviewer_failed",
            "error": {"code": "timeout", "message": "the reviewer timed out"},
        })
    );
    // The escalation offers the rule an allow can remember.
    assert_eq!(requested.payload.get("rule"), None);
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "allow");
    assert_eq!(resolved.payload["decided_by"], "person");
    assert_eq!(tool.ran().len(), 1);
    assert_eq!(reviewer.requests().len(), 1);
    assert!(
        usages(&lines)
            .iter()
            .all(|l| l.payload["model"] != REVIEWER_MODEL)
    );
}

#[test]
fn with_no_reviewer_every_reviewed_call_goes_to_a_person_with_one_notice() {
    let tool = shell(None, None);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris()), ("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    )
    .answerable(false);
    let lines = go(&mut session);
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "notice",
            "permission_resolved",
            "permission_resolved",
            "tool_call_completed",
            "tool_call_completed",
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

    let notices: Vec<&Envelope> = lines.iter().filter(|l| l.kind == "notice").collect();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].payload["code"], "no_model");
    assert_eq!(
        notices[0].payload["message"],
        "No reviewer model is set, so every reviewed call goes to a person. Set reviewer.model."
    );
    assert!(!kinds(&lines).contains(&"permission_requested"));
    let resolved: Vec<&Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_resolved")
        .collect();
    assert_eq!(resolved.len(), 2);
    for line in &resolved {
        assert_eq!(line.payload["decision"], "deny");
        assert_eq!(line.payload["decided_by"], "reviewer");
    }
    for done in completed(&lines) {
        assert_eq!(done.payload["status"], "denied");
        assert_eq!(done.payload["reason"], "reviewer");
    }
    assert!(!kinds(&lines).contains(&"tool_call_started"));
    assert!(tool.ran().is_empty());
}

#[test]
fn the_third_consecutive_block_asks_a_person() {
    let tool = shell(None, None);
    let mut session =
        Session::with_tools(paired_turns(4), None, vec![tool.clone() as Arc<dyn Tool>]);
    let reviewer = session.reviewer(vec![
        Scripted::text("check"),
        Scripted::text("block first reason"),
        Scripted::text("check"),
        Scripted::text("block second reason"),
        Scripted::text("check"),
        Scripted::text("block third reason"),
        Scripted::text("check"),
        Scripted::text("block fourth reason"),
    ]);
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        kinds(&session.lines()),
        kinds_with(&["permission_resolved", "tool_call_completed"], 2)
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        kinds(&session.lines()),
        kinds_next(&["permission_resolved", "tool_call_completed"], 2)
    );
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| inbox.send(reply(id, deny(Some("not on main")))).unwrap()
    });
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    answered.join().unwrap();
    assert_eq!(kinds(&lines), kinds_escalated(false, false));

    // The block that reaches the count is not returned to the model.
    let requested = line(&lines, "permission_requested");
    assert_eq!(requested.payload["step"], "review");
    assert_eq!(
        requested.payload["escalation"],
        json!({"cause": "consecutive_blocks", "reason": "third reason"})
    );
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "denied");
    assert_eq!(done.payload["reason"], "person");

    // The person's answer ends the run of consecutive blocks.
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        kinds_next(&["permission_resolved", "tool_call_completed"], 2)
    );
    assert_eq!(reviewer.requests().len(), 8);
    assert!(tool.ran().is_empty());
}

#[test]
fn an_allow_between_blocks_resets_the_consecutive_count() {
    let tool = shell(None, None);
    let mut session =
        Session::with_tools(paired_turns(5), None, vec![tool.clone() as Arc<dyn Tool>]);
    let reviewer = session.reviewer(vec![
        Scripted::text("check"),
        Scripted::text("block one"),
        Scripted::text("allow"),
        Scripted::text("check"),
        Scripted::text("block two"),
        Scripted::text("check"),
        Scripted::text("block three"),
        Scripted::text("check"),
        Scripted::text("block four"),
    ]);
    // T2's allow runs the call; every other turn denies without asking. The
    // flag tells whether the turn opens the session.
    let turns = [
        (vec!["permission_resolved", "tool_call_completed"], 2, true),
        (
            vec![
                "permission_resolved",
                "tool_call_started",
                "tool_call_completed",
            ],
            1,
            false,
        ),
        (vec!["permission_resolved", "tool_call_completed"], 2, false),
        (vec!["permission_resolved", "tool_call_completed"], 2, false),
    ];
    for (middle, reviews, first) in turns {
        session.inbox.send(delivery("go")).unwrap();
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let lines = session.lines();
        let expected = if first {
            kinds_with(&middle, reviews)
        } else {
            kinds_next(&middle, reviews)
        };
        assert_eq!(kinds(&lines), expected);
    }
    assert_eq!(tool.ran().len(), 1);
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| inbox.send(reply(id, deny(None))).unwrap()
    });
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    answered.join().unwrap();
    assert_eq!(kinds(&lines), kinds_escalated(false, false));
    let requested = line(&lines, "permission_requested");
    assert_eq!(
        requested.payload["escalation"],
        json!({"cause": "consecutive_blocks", "reason": "four"})
    );
    assert_eq!(reviewer.requests().len(), 9);
}

#[test]
fn the_session_limit_escalates_with_a_raised_consecutive_limit() {
    let tool = shell(None, None);
    let calls: Vec<(&str, Value)> =
        vec![("shell", paris()), ("shell", paris()), ("shell", paris())];
    let mut session = Session::with_tools(
        vec![calls_reply("", &calls), Scripted::text("Done.")],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    let reviewer = session.reviewer_limits(
        vec![
            Scripted::text("check"),
            Scripted::text("block one"),
            Scripted::text("check"),
            Scripted::text("block two"),
            Scripted::text("check"),
            Scripted::text("block three"),
        ],
        r#loop::BlockLimits {
            consecutive: 1000,
            session: 3,
        },
    );
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| inbox.send(reply(id, allow())).unwrap()
    });
    let lines = go(&mut session);
    answered.join().unwrap();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "usage_recorded",
            "usage_recorded",
            "permission_requested",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "tool_call_completed",
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

    let done = completed(&lines);
    assert_eq!(done.len(), 3);
    assert_eq!(done[0].payload["status"], "denied");
    assert_eq!(done[1].payload["status"], "denied");
    assert_eq!(done[2].payload["status"], "completed");
    let requested = line(&lines, "permission_requested");
    assert_eq!(
        requested.payload["escalation"],
        json!({"cause": "session_blocks", "reason": "three"})
    );
    assert_eq!(reviewer.requests().len(), 6);
}

#[test]
fn a_persons_allow_with_remember_adds_a_grant_later_calls_match() {
    let tool = shell(Some("npm test"), Some("npm"));
    let mut session =
        Session::with_tools(paired_turns(4), None, vec![tool.clone() as Arc<dyn Tool>]);
    let reviewer = session.reviewer(vec![
        Scripted::text("check"),
        Scripted::text("block one"),
        Scripted::text("check"),
        Scripted::text("block two"),
        Scripted::text("check"),
        Scripted::text("block three"),
    ]);
    for turn in 0..2 {
        session.inbox.send(delivery("go")).unwrap();
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let lines = session.lines();
        let expected = if turn == 0 {
            kinds_with(&["permission_resolved", "tool_call_completed"], 2)
        } else {
            kinds_next(&["permission_resolved", "tool_call_completed"], 2)
        };
        assert_eq!(kinds(&lines), expected);
    }
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| {
            inbox
                .send(reply(
                    id,
                    ReplyAnswer::Approval {
                        decision: Decision::Allow,
                        feedback: None,
                        remember: Some(Remember {
                            scope: RememberScope::Session,
                            prefix: "npm test".into(),
                        }),
                    },
                ))
                .unwrap();
        }
    });
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    answered.join().unwrap();
    assert_eq!(kinds(&lines), kinds_escalated(true, false));
    let requested = line(&lines, "permission_requested");
    assert_eq!(
        requested.payload["rule"],
        json!({"subject": "npm test", "prefix": "npm"})
    );
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "allow");
    assert_eq!(
        resolved.payload["grant"],
        json!({"tool": "shell", "prefix": "npm test"})
    );
    assert_eq!(reviewer.requests().len(), 6);

    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    // The grant answers the call with no reviewer request and no usage.
    assert_eq!(
        kinds(&lines),
        [
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
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
        ]
    );
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "allow");
    assert_eq!(resolved.payload["decided_by"], "session_grant");
    // The grant answers the call: no new reviewer request.
    assert_eq!(reviewer.requests().len(), 6);
    assert_eq!(tool.ran().len(), 2);
}

#[test]
fn a_headless_session_ends_the_turn_once_the_block_budget_runs_out() {
    let tool = shell(None, None);
    let calls: Vec<(&str, Value)> = vec![("shell", paris()), ("shell", paris())];
    let mut session = Session::with_tools(
        vec![calls_reply("", &calls)],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    )
    .answerable(false);
    let reviewer = session.reviewer_limits(
        vec![
            Scripted::text("check"),
            Scripted::text("block one"),
            Scripted::text("check"),
            Scripted::text("block two"),
        ],
        r#loop::BlockLimits {
            consecutive: 1000,
            session: 2,
        },
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "tool_call_completed",
            "tool_call_completed",
            "turn_completed",
        ]
    );

    assert!(!kinds(&lines).contains(&"permission_requested"));
    for done in completed(&lines) {
        assert_eq!(done.payload["status"], "denied");
        assert_eq!(done.payload["reason"], "reviewer");
    }
    let end = line(&lines, "turn_completed");
    assert_eq!(end.payload["outcome"], "failed");
    assert_eq!(end.payload["error"]["code"], "blocked");
    assert_eq!(
        end.payload["error"]["message"],
        "The reviewer blocked 2 calls and no person can answer."
    );
    assert_eq!(reviewer.requests().len(), 4);
    assert!(tool.ran().is_empty());
}

#[test]
fn a_colon_block_is_handled_as_a_block() {
    let (session, tool, reviewer, lines) = reviewed_turn(vec![
        Scripted::text("check"),
        Scripted::text("block: force-pushes to main"),
    ]);
    assert_eq!(
        kinds(&lines),
        kinds_with(&["permission_resolved", "tool_call_completed"], 2)
    );
    assert!(!kinds(&lines).contains(&"permission_requested"));
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "deny");
    assert_eq!(resolved.payload["decided_by"], "reviewer");
    assert_eq!(resolved.payload["reason"], "force-pushes to main");
    assert_eq!(
        resolved.payload["reviewer"],
        json!({"model": REVIEWER_MODEL, "stage": 2})
    );
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "denied");
    assert_eq!(done.payload["reason"], "reviewer");
    assert!(tool.ran().is_empty());
    assert_eq!(reviewer.requests().len(), 2);
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn a_colon_allow_carries_its_reason() {
    let (session, tool, reviewer, lines) = reviewed_turn(vec![
        Scripted::text("check"),
        Scripted::text("allow: looks fine"),
    ]);
    assert_eq!(
        kinds(&lines),
        kinds_with(
            &[
                "permission_resolved",
                "tool_call_started",
                "tool_call_completed",
            ],
            2,
        )
    );
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "allow");
    assert_eq!(
        resolved.payload["reviewer"],
        json!({"model": REVIEWER_MODEL, "stage": 2})
    );
    assert_eq!(resolved.payload["reason"], "looks fine");
    assert_eq!(tool.ran().len(), 1);
    assert_eq!(reviewer.requests().len(), 2);
    assert_eq!(session.requests().len(), 2);
}

#[test]
fn headless_failures_count_toward_the_block_budget() {
    let tool = shell(None, None);
    let calls: Vec<(&str, Value)> = vec![("shell", paris()), ("shell", paris())];
    let mut session = Session::with_tools(
        vec![calls_reply("", &calls)],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    )
    .answerable(false);
    let reviewer = session.reviewer_limits(
        vec![
            Scripted::failed(Failure {
                code: ErrorCode::Timeout,
                message: "the reviewer timed out".into(),
                retry_after: None,
                provider: None,
            }),
            Scripted::failed(Failure {
                code: ErrorCode::Timeout,
                message: "the reviewer timed out".into(),
                retry_after: None,
                provider: None,
            }),
        ],
        r#loop::BlockLimits {
            consecutive: 1000,
            session: 2,
        },
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let lines = session.lines();
    // Failed reviewer calls write no usage and raise no request.
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_resolved",
            "permission_resolved",
            "tool_call_completed",
            "tool_call_completed",
            "turn_completed",
        ]
    );
    for line in lines.iter().filter(|l| l.kind == "permission_resolved") {
        assert_eq!(line.payload["decision"], "deny");
        assert_eq!(line.payload["decided_by"], "reviewer");
        assert_eq!(line.payload["reason"], "the reviewer timed out");
    }
    let end = line(&lines, "turn_completed");
    assert_eq!(end.payload["outcome"], "failed");
    assert_eq!(end.payload["error"]["code"], "blocked");
    assert_eq!(
        end.payload["error"]["message"],
        "The reviewer blocked 2 calls and no person can answer."
    );
    assert_eq!(reviewer.requests().len(), 2);
    assert!(tool.ran().is_empty());
}

#[test]
fn a_review_at_the_spending_budget_denies_without_sending() {
    let tool = shell(None, None);
    let calls: Vec<(&str, Value)> = vec![("shell", paris()), ("shell", paris())];
    let mut session = Session::with_tools(
        vec![calls_reply("", &calls)],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    )
    .answerable(false)
    .budget(Some(0.00001));
    // One priced reviewer call costs (10 + 3) / 1e6: the first sends, the
    // second is denied at the budget.
    let reviewer = session.reviewer_priced(
        vec![Scripted::text("allow")],
        r#loop::BlockLimits::default(),
        Some(contract::provider::Cost {
            input: 1.0,
            output: 1.0,
            cache_read: None,
            cache_write: None,
            tiers: Vec::new(),
        }),
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "permission_resolved",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "step_started",
            "turn_completed",
        ]
    );
    assert_eq!(reviewer.requests().len(), 1);
    let resolved: Vec<&Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_resolved")
        .collect();
    assert_eq!(resolved[0].payload["decision"], "allow");
    assert_eq!(resolved[1].payload["decision"], "deny");
    assert_eq!(resolved[1].payload["decided_by"], "reviewer");
    assert_eq!(
        resolved[1].payload["reason"],
        "The session reached its spending budget."
    );
    let done = completed(&lines);
    assert_eq!(done[0].payload["status"], "completed");
    assert_eq!(done[1].payload["status"], "denied");
    assert_eq!(done[1].payload["reason"], "budget_exceeded");
    // The next step's own budget check fails the turn.
    let end = line(&lines, "turn_completed");
    assert_eq!(end.payload["outcome"], "failed");
    assert_eq!(end.payload["error"]["code"], "budget_exceeded");
    assert_eq!(tool.ran().len(), 1);
}

#[test]
fn failures_without_an_answer_count_toward_the_consecutive_limit() {
    let tool = shell(None, None);
    let home = TempDir::new();
    let workspace = home.0.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let credentials = home.0.join("credentials");
    std::fs::create_dir_all(&credentials).unwrap();
    let log = Arc::new(
        Log::create(
            &home.0,
            SessionId("s_test".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap(),
    );
    let mut watcher = log.watch();
    let (forward, waiting) = mpsc::channel();
    thread::spawn(move || {
        while let Ok(Some(line)) = watcher.recv() {
            if forward.send(line).is_err() {
                return;
            }
        }
    });
    let (inbox, rx) = mpsc::channel();
    let session: Arc<dyn contract::provider::Provider> = Arc::new(ScriptedProvider::new(vec![
        calls_reply(
            "",
            &[("shell", paris()), ("shell", paris()), ("shell", paris())],
        ),
        Scripted::text("Done."),
    ]));
    let reviewer = Arc::new(ScriptedProvider::new(vec![
        Scripted::failed(Failure {
            code: ErrorCode::Timeout,
            message: "the reviewer timed out".into(),
            retry_after: None,
            provider: None,
        }),
        Scripted::failed(Failure {
            code: ErrorCode::Timeout,
            message: "the reviewer timed out".into(),
            retry_after: None,
            provider: None,
        }),
        Scripted::text("check"),
        Scripted::text("block third reason"),
    ]));
    let rules = Arc::new(FakeRules::empty());
    let mut looped = Loop::start(
        Arc::clone(&log),
        session,
        Model {
            reference: MODEL.into(),
            cost: None,
            subscription: false,
        },
        "You are terse.".into(),
        rx,
        vec![("builtin".to_owned(), tool.clone() as Arc<dyn Tool>)],
        r#loop::Permissions {
            workspace: workspace.display().to_string(),
            credentials: credentials.clone(),
            rules,
        },
    )
    .unwrap()
    .reviewer(
        Ok(Reviewer {
            provider: reviewer.clone(),
            model: Model {
                reference: REVIEWER_MODEL.into(),
                cost: None,
                subscription: false,
            },
        }),
        BlockLimits::default(),
    );
    inbox.send(delivery("go")).unwrap();
    // Every inbox sender is gone: each escalation raises its request and
    // ends with no answer, so nothing resets the consecutive count.
    drop(inbox);
    assert_eq!(looped.turn().unwrap(), Some(TurnOutcome::Completed));
    let mut lines = Vec::new();
    loop {
        let line = waiting
            .recv_timeout(DEADLINE)
            .expect("a turn_completed line");
        let last = line.kind == "turn_completed";
        lines.push(line);
        if last {
            break;
        }
    }
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_requested",
            "permission_resolved",
            "permission_requested",
            "permission_resolved",
            "usage_recorded",
            "usage_recorded",
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
            "tool_call_completed",
            "tool_call_completed",
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
    let requested: Vec<&Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_requested")
        .collect();
    assert_eq!(requested.len(), 3);
    assert_eq!(
        requested[0].payload["escalation"],
        json!({
            "cause": "reviewer_failed",
            "error": {"code": "timeout", "message": "the reviewer timed out"},
        })
    );
    // The third call's block reaches the consecutive limit because the two
    // failures counted and no answer reset them.
    assert_eq!(
        requested[2].payload["escalation"],
        json!({"cause": "consecutive_blocks", "reason": "third reason"})
    );
    let resolved: Vec<&Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_resolved")
        .collect();
    assert_eq!(resolved.len(), 3);
    for line in &resolved[..2] {
        assert_eq!(line.payload["decision"], "deny");
        assert_eq!(line.payload["decided_by"], "reviewer");
        assert_eq!(
            line.payload["reason"],
            "The session ended while waiting for an answer."
        );
    }
    for done in completed(&lines) {
        assert_eq!(done.payload["status"], "denied");
        assert_eq!(done.payload["reason"], "no_person");
    }
    assert_eq!(reviewer.requests().len(), 4);
    assert!(tool.ran().is_empty());
}

/// `close` taken while an escalation waits ends the wait as its step's
/// unanswerable denial, and every later escalation in the turn denies
/// without raising a request: `close` needs no `answerable` check of its
/// own, because taking it already denies every later approval.
#[test]
fn close_taken_during_an_escalation_leaves_later_calls_unanswerable() {
    let tool = shell(None, None);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris()), ("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    let reviewer = session.reviewer_limits(
        vec![
            Scripted::text("check"),
            Scripted::text("block first reason"),
            Scripted::text("check"),
            Scripted::text("block second reason"),
        ],
        r#loop::BlockLimits {
            consecutive: 1,
            session: 1000,
        },
    );
    let closer = on_close(&session, {
        let inbox = session.inbox.clone();
        move || {
            inbox.send(Delivery::Close(ignore())).unwrap();
        }
    });
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    closer.join().unwrap();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_requested",
            "permission_resolved",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "tool_call_completed",
            "tool_call_completed",
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
    // One request, for the first call: the second escalation denies without
    // raising one.
    let requested: Vec<&Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_requested")
        .collect();
    assert_eq!(requested.len(), 1);
    assert_eq!(
        requested[0].payload["escalation"],
        json!({"cause": "consecutive_blocks", "reason": "first reason"})
    );
    let resolved: Vec<&Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_resolved")
        .collect();
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].payload["reason"], "first reason");
    assert_eq!(
        resolved[0].payload["request_id"],
        requested[0].payload["request_id"]
    );
    assert_eq!(resolved[1].payload["reason"], "second reason");
    assert_eq!(resolved[1].payload.get("request_id"), None);
    for done in completed(&lines) {
        assert_eq!(done.payload["status"], "denied");
        assert_eq!(done.payload["reason"], "reviewer");
    }
    assert_eq!(reviewer.requests().len(), 4);
    assert!(tool.ran().is_empty());
}

/// `close` taken while a review escalation waits leaves a later standing
/// ask in the same reply unanswerable: it denies as the standing rule, with
/// no request raised.
#[test]
fn close_taken_during_an_escalation_leaves_a_later_standing_ask_unanswerable() {
    let reviewed = shell(None, None);
    let mut asked = TestTool::declaring("publish", "Ran it.", vec![Effect::Executes], None);
    asked.subject = Some("npm publish".to_owned());
    let asked = Arc::new(asked);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris()), ("publish", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![
            reviewed.clone() as Arc<dyn Tool>,
            asked.clone() as Arc<dyn Tool>,
        ],
    );
    session.rules.set(StandingRules {
        global: vec![Rule {
            decision: RuleDecision::Ask,
            tool: "publish".into(),
            prefix: "npm publish".into(),
            added: None,
            session_id: None,
        }],
        project: Vec::new(),
    });
    let reviewer = session.reviewer_limits(
        vec![
            Scripted::text("check"),
            Scripted::text("block first reason"),
        ],
        r#loop::BlockLimits {
            consecutive: 1,
            session: 1000,
        },
    );
    let closer = on_close(&session, {
        let inbox = session.inbox.clone();
        move || {
            inbox.send(Delivery::Close(ignore())).unwrap();
        }
    });
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    closer.join().unwrap();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_requested",
            "permission_resolved",
            "permission_resolved",
            "tool_call_completed",
            "tool_call_completed",
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
    let requested: Vec<&Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_requested")
        .collect();
    assert_eq!(requested.len(), 1);
    assert_eq!(requested[0].payload["step"], "review");
    let resolved: Vec<&Envelope> = lines
        .iter()
        .filter(|l| l.kind == "permission_resolved")
        .collect();
    assert_eq!(resolved[0].payload["decided_by"], "reviewer");
    assert_eq!(
        resolved[0].payload["request_id"],
        requested[0].payload["request_id"]
    );
    assert_eq!(resolved[1].payload["decision"], "deny");
    assert_eq!(resolved[1].payload["decided_by"], "standing_rule");
    assert_eq!(resolved[1].payload.get("request_id"), None);
    let done = completed(&lines);
    assert_eq!(done[0].payload["reason"], "reviewer");
    assert_eq!(done[1].payload["status"], "denied");
    assert_eq!(done[1].payload["reason"], "no_person");
    assert_eq!(reviewer.requests().len(), 2);
    assert!(reviewed.ran().is_empty());
    assert!(asked.ran().is_empty());
}
