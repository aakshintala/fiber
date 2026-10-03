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

use contract::commands::{Remember, RememberScope, Reply, ReplyAnswer};
use contract::events::{CacheLifetime, Decision, TurnOutcome};
use contract::inbox::Delivery;
use contract::provider::{Input, ModelRequest};
use contract::shapes::{Effect, Failure};
use contract::tool::Tool;
use contract::{Envelope, ErrorCode, RequestId};
use fakes::{Scripted, ScriptedProvider};
use serde_json::{Value, json};

use support::{
    REVIEWER_MODEL, Session, TestTool, calls_reply, delivery, ignore, kinds, on_request,
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
    let mut kinds = vec![
        "session_started",
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
    let _ = session.lines();

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
    for _ in 0..2 {
        session.inbox.send(delivery("go")).unwrap();
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let _ = session.lines();
    }
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| inbox.send(reply(id, deny(Some("not on main")))).unwrap()
    });
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    answered.join().unwrap();

    // The block that reaches the count is not returned to the model.
    assert!(kinds(&lines).contains(&"permission_requested"));
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
    assert!(!kinds(&lines).contains(&"permission_requested"));
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
    for _ in 0..4 {
        session.inbox.send(delivery("go")).unwrap();
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let lines = session.lines();
        assert!(!kinds(&lines).contains(&"permission_requested"));
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
    for _ in 0..2 {
        session.inbox.send(delivery("go")).unwrap();
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let _ = session.lines();
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
