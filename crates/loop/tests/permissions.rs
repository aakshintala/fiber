//! Steps 1 to 6 of the permission order through the loop's public API
//! (`docs/permissions.md`, "The order a call is judged in").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::sync::Arc;

use contract::commands::{Reply, ReplyAnswer};
use contract::events::{Decision, TurnOutcome};
use contract::inbox::Delivery;
use contract::provider::Input;
use contract::rules::{Rule, RuleDecision, RulesError, StandingRules};
use contract::shapes::Effect;
use contract::tool::Tool;
use contract::{Envelope, RequestId};
use fakes::Scripted;
use serde_json::{Value, json};

use support::{Session, TestTool, calls_reply, delivery, ignore, kinds, message, on_request};

fn paris() -> Value {
    json!({"city": "Paris"})
}

fn rule(decision: RuleDecision, tool: &str, prefix: &str) -> Rule {
    Rule {
        decision,
        tool: tool.into(),
        prefix: prefix.into(),
        added: None,
        session_id: None,
    }
}

fn standing(global: Vec<Rule>, project: Vec<Rule>) -> StandingRules {
    StandingRules { global, project }
}

/// A tool whose calls execute with `subject`.
fn shell(subject: Option<&str>) -> Arc<TestTool> {
    let mut tool = TestTool::declaring("shell", "Ran it.", vec![Effect::Executes], None);
    tool.subject = subject.map(str::to_owned);
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

/// The full ordered kind list of a first turn whose first reply calls once
/// with no text and whose second says "Done.", with `middle` in place of
/// the decision lines.
fn kinds_with(middle: &[&str]) -> Vec<String> {
    let mut kinds = vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "tool_call_arguments_delta",
        // No `text_completed`: the first reply carries no text.
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
    ];
    kinds.extend(middle.iter().copied());
    kinds.extend(
        [
            "step_started",
            "assistant_message_started",
            // "Done." streams as two deltas.
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

#[test]
fn a_call_touching_the_credentials_is_refused_and_never_runs() {
    // `../credentials` from the workspace: the session's credentials
    // directory, which the session created.
    let sneaky = TestTool::declaring(
        "sneaky",
        "Read it.",
        vec![Effect::Reads],
        Some(vec!["../credentials".into()]),
    );
    let tool = Arc::new(sneaky);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("sneaky", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    let lines = go(&mut session);
    assert_eq!(
        kinds(&lines),
        kinds_with(&["permission_resolved", "tool_call_completed",])
    );
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "deny");
    assert_eq!(resolved.payload["decided_by"], "credential_deny");
    assert_eq!(
        resolved.payload["reason"],
        "The call touches Fiber's credential directory."
    );
    assert_eq!(resolved.payload.get("request_id"), None);
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "denied");
    assert_eq!(done.payload["reason"], "credentials");
    assert_eq!(resolved.action_id, done.action_id);
    assert!(
        !kinds(&lines).contains(&"tool_call_started"),
        "a denied call never starts"
    );
    assert!(tool.ran().is_empty(), "a denied call never runs");
}

/// Runs one read-only call of `subject` declaring `paths` (relative to the
/// workspace, which sits in Fiber home beside `credentials/`), and asserts
/// the credential deny refused it before it ran.
fn assert_refused_as_credentials(subject: &str, paths: &[&str]) {
    let mut search = TestTool::declaring(
        "shell",
        "Found it.",
        vec![Effect::Reads],
        Some(paths.iter().map(|p| (*p).to_owned()).collect()),
    );
    search.subject = Some(subject.to_owned());
    let tool = Arc::new(search);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    let lines = go(&mut session);
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "deny", "{subject}");
    assert_eq!(
        resolved.payload["decided_by"], "credential_deny",
        "{subject}"
    );
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "denied", "{subject}");
    assert_eq!(done.payload["reason"], "credentials", "{subject}");
    assert!(
        !kinds(&lines).contains(&"tool_call_started"),
        "a denied call never starts: {subject}"
    );
    assert!(tool.ran().is_empty(), "a denied call never runs: {subject}");
}

#[test]
fn a_recursive_grep_of_fiber_home_is_refused() {
    // `..` from the workspace is Fiber home.
    assert_refused_as_credentials("grep -r '' ..", &[".."]);
}

#[test]
fn an_rg_with_no_operand_above_fiber_home_is_refused() {
    // With no operand the search declares its workdir, here the directory
    // that holds Fiber home.
    assert_refused_as_credentials("rg sk-", &["../.."]);
}

#[test]
fn a_git_diff_of_fiber_home_is_refused() {
    assert_refused_as_credentials("git diff .. .", &["..", "."]);
}

#[test]
fn a_standing_deny_refuses_and_never_runs() {
    let tool = shell(Some("npm publish"));
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    session.rules.set(standing(
        vec![rule(RuleDecision::Deny, "shell", "npm publish")],
        Vec::new(),
    ));
    let lines = go(&mut session);
    assert_eq!(
        kinds(&lines),
        kinds_with(&["permission_resolved", "tool_call_completed"])
    );
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "deny");
    assert_eq!(resolved.payload["decided_by"], "standing_rule");
    assert_eq!(
        resolved.payload["reason"],
        "A standing rule refuses this call."
    );
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "denied");
    assert_eq!(done.payload["reason"], "standing_rule");
    assert_eq!(
        text(done),
        "A standing rule refuses this call. It did not run."
    );
    assert!(!kinds(&lines).contains(&"tool_call_started"));
    assert!(tool.ran().is_empty());
}

#[test]
fn a_standing_allow_runs_an_executes_call() {
    let tool = shell(Some("npm test --watch"));
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    session.rules.set(standing(
        vec![rule(RuleDecision::Allow, "shell", "npm test")],
        Vec::new(),
    ));
    let lines = go(&mut session);
    assert_eq!(
        kinds(&lines),
        kinds_with(&[
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
        ])
    );
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "allow");
    assert_eq!(resolved.payload["decided_by"], "standing_rule");
    assert_eq!(resolved.payload.get("request_id"), None);
    let started = line(&lines, "tool_call_started");
    assert_eq!(resolved.action_id, started.action_id);
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "completed");
    assert_eq!(text(done), "Ran it.");
    assert_eq!(tool.ran().len(), 1);
}

#[test]
fn a_call_no_rule_can_match_is_not_matched_by_a_deny() {
    let tool = shell(None);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    )
    // No reviewer is configured, so the call escalates `no_model`, and no
    // person can answer it: the call ends a reviewer deny.
    .answerable(false);
    session.rules.set(standing(
        vec![rule(RuleDecision::Deny, "shell", "npm test")],
        Vec::new(),
    ));
    let lines = go(&mut session);
    assert_eq!(
        kinds(&lines),
        kinds_with(&["notice", "permission_resolved", "tool_call_completed"])
    );
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "deny");
    assert_eq!(resolved.payload["decided_by"], "reviewer");
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "denied");
    assert_eq!(done.payload["reason"], "reviewer");
    assert!(tool.ran().is_empty());
}

#[test]
fn a_standing_ask_runs_on_a_persons_allow() {
    let tool = shell(Some("npm publish"));
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    session.rules.set(standing(
        vec![rule(RuleDecision::Ask, "shell", "npm publish")],
        Vec::new(),
    ));
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| inbox.send(reply(id, allow())).unwrap()
    });
    let lines = go(&mut session);
    answered.join().unwrap();
    assert_eq!(
        kinds(&lines),
        kinds_with(&[
            "permission_requested",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
        ])
    );
    let requested = line(&lines, "permission_requested");
    assert_eq!(requested.payload["step"], "standing_ask");
    assert_eq!(
        requested.payload["standing_rule"],
        json!({"scope": "global", "prefix": "npm publish"})
    );
    assert_eq!(requested.payload["effects"], json!(["executes"]));
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "allow");
    assert_eq!(resolved.payload["decided_by"], "person");
    assert_eq!(
        resolved.payload["request_id"],
        requested.payload["request_id"]
    );
    assert_eq!(requested.action_id, resolved.action_id);
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "completed");
    assert_eq!(tool.ran().len(), 1);
}

#[test]
fn a_reply_naming_a_wrong_request_is_ignored_and_the_right_one_answers() {
    let tool = shell(Some("npm publish"));
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    session.rules.set(standing(
        vec![rule(RuleDecision::Ask, "shell", "npm publish")],
        Vec::new(),
    ));
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| {
            inbox
                .send(reply(RequestId("r_wrong".into()), allow()))
                .unwrap();
            inbox.send(reply(id, allow())).unwrap();
        }
    });
    let lines = go(&mut session);
    answered.join().unwrap();
    assert_eq!(
        kinds(&lines),
        kinds_with(&[
            "permission_requested",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
        ])
    );
    // One request, one answer: the wrong id wrote nothing.
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.kind == "permission_requested")
            .count(),
        1
    );
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.kind == "permission_resolved")
            .count(),
        1
    );
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "completed");
    assert_eq!(tool.ran().len(), 1);
}

#[test]
fn a_persons_deny_with_feedback_reaches_the_models_next_request() {
    let tool = shell(Some("npm publish"));
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    session.rules.set(standing(
        vec![rule(RuleDecision::Ask, "shell", "npm publish")],
        Vec::new(),
    ));
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| inbox.send(reply(id, deny(Some("not on main")))).unwrap()
    });
    let lines = go(&mut session);
    answered.join().unwrap();
    assert_eq!(
        kinds(&lines),
        kinds_with(&[
            "permission_requested",
            "permission_resolved",
            "tool_call_completed",
        ])
    );
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "deny");
    assert_eq!(resolved.payload["decided_by"], "person");
    assert_eq!(resolved.payload["feedback"], "not on main");
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "denied");
    assert_eq!(done.payload["reason"], "person");
    assert_eq!(
        text(done),
        "A person refused this call: not on main. It did not run."
    );
    assert!(tool.ran().is_empty());
    let requests = session.requests();
    let Some(Input::ToolResult { text, is_error, .. }) = requests[1]
        .conversation
        .iter()
        .rev()
        .find(|input| matches!(input, Input::ToolResult { .. }))
    else {
        panic!("the denial goes back to the model");
    };
    assert!(text.contains("not on main"), "{text}");
    assert!(!is_error);
}

#[test]
fn a_message_sent_while_waiting_steers_the_next_step() {
    let tool = shell(Some("npm publish"));
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    session.rules.set(standing(
        vec![rule(RuleDecision::Ask, "shell", "npm publish")],
        Vec::new(),
    ));
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| {
            inbox
                .send(Delivery::Steer(message("wait, actually"), ignore()))
                .unwrap();
            inbox.send(reply(id, allow())).unwrap();
        }
    });
    let lines = go(&mut session);
    answered.join().unwrap();
    let sequence = kinds(&lines);
    let resolved_at = sequence
        .iter()
        .position(|k| *k == "permission_resolved")
        .unwrap();
    let steering_at = sequence
        .iter()
        .position(|k| *k == "steering_applied")
        .unwrap();
    assert!(
        steering_at > resolved_at,
        "the held message steers the next step: {sequence:?}"
    );
    let steering = line(&lines, "steering_applied");
    assert_eq!(steering.payload["content"][0]["text"], "wait, actually");
    assert_eq!(completed(&lines)[0].payload["status"], "completed");
}

#[test]
fn an_unanswerable_session_denies_a_standing_ask_without_asking() {
    let tool = shell(Some("npm publish"));
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    )
    .answerable(false);
    session.rules.set(standing(
        vec![rule(RuleDecision::Ask, "shell", "npm publish")],
        Vec::new(),
    ));
    let lines = go(&mut session);
    assert_eq!(
        kinds(&lines),
        kinds_with(&["permission_resolved", "tool_call_completed"])
    );
    assert!(!kinds(&lines).contains(&"permission_requested"));
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "deny");
    assert_eq!(resolved.payload["decided_by"], "standing_rule");
    assert_eq!(resolved.payload.get("request_id"), None);
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "denied");
    assert_eq!(done.payload["reason"], "no_person");
    assert!(tool.ran().is_empty());
}

#[test]
fn unreadable_rules_deny_with_the_file_and_line() {
    let tool = shell(Some("npm publish"));
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    session
        .rules
        .fail(RulesError("/home/rules:2: expected value".into()));
    let lines = go(&mut session);
    assert_eq!(
        kinds(&lines),
        kinds_with(&["permission_resolved", "tool_call_completed"])
    );
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "deny");
    assert_eq!(resolved.payload["decided_by"], "standing_rule");
    assert_eq!(resolved.payload["reason"], "/home/rules:2: expected value");
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "denied");
    assert_eq!(done.payload["reason"], "rules_unreadable");
    assert!(tool.ran().is_empty());
}

#[test]
fn the_projects_rule_is_reported_over_a_global_one() {
    let tool = shell(Some("npm test --watch"));
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    session.rules.set(standing(
        vec![rule(RuleDecision::Ask, "shell", "npm")],
        vec![rule(RuleDecision::Ask, "shell", "npm test")],
    ));
    let answered = on_request(&session, {
        let inbox = session.inbox.clone();
        move |id| inbox.send(reply(id, allow())).unwrap()
    });
    let lines = go(&mut session);
    answered.join().unwrap();
    let requested = line(&lines, "permission_requested");
    assert_eq!(
        requested.payload["standing_rule"],
        json!({"scope": "project", "prefix": "npm test"})
    );
    assert_eq!(completed(&lines)[0].payload["status"], "completed");
}
