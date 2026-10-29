//! Tests for the shell-call read and its narrow action join.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;

fn write_log(dir: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("events.jsonl"), bytes).unwrap();
}

fn session_dir(home: &Path, project: &str, id: &str) -> PathBuf {
    home.join("projects")
        .join(project)
        .join("sessions")
        .join(id)
}

fn started(workspace: &str) -> String {
    json!({"kind": "session_started", "payload": {"workspace": workspace}}).to_string() + "\n"
}

/// One log line for `action`, without `action_id` when `action` is `None`.
fn line(kind: &str, action: Option<&str>, seq: u64, payload: &Value) -> String {
    let mut envelope = json!({
        "kind": kind,
        "session_id": "s_1",
        "ts": seq,
        "schema_version": 1,
        "seq": seq,
        "payload": payload,
    });
    if let Some(action) = action {
        envelope["action_id"] = Value::String(action.to_owned());
    }
    envelope.to_string() + "\n"
}

/// A shell `tool_call_requested` for `action` running `command`.
fn requested(action: &str, seq: u64, command: &str) -> String {
    line(
        "tool_call_requested",
        Some(action),
        seq,
        &json!({"name": "shell", "arguments": {"command": command, "workdir": "/w"}}),
    )
}

/// The same call with a repair: the reviewer saw `repaired`.
fn requested_repaired(action: &str, seq: u64, command: &str, repaired: &str) -> String {
    line(
        "tool_call_requested",
        Some(action),
        seq,
        &json!({
            "name": "shell",
            "arguments": {"command": command, "workdir": "/w"},
            "repaired": {"command": repaired},
            "repairs": [],
        }),
    )
}

/// A `tool_call_requested` for `tool`, which is never a shell candidate.
fn requested_tool(action: &str, seq: u64, tool: &str) -> String {
    line(
        "tool_call_requested",
        Some(action),
        seq,
        &json!({"name": tool, "arguments": {"path": "/w/a"}}),
    )
}

/// A review `permission_requested` for `action`, with a rule and escalation
/// only when given.
fn request(action: &str, seq: u64, id: &str, subject: Option<&str>, escalated: bool) -> String {
    let mut payload = json!({"request_id": id, "step": "review"});
    if let Some(subject) = subject {
        payload["rule"] = json!({"subject": subject, "prefix": subject});
    }
    if escalated {
        payload["escalation"] = json!({"cause": "consecutive_blocks", "reason": "blocked"});
    }
    line("permission_requested", Some(action), seq, &payload)
}

/// A `permission_resolved` for `action`: `reviewed` is the judged
/// `arguments.command`, when a rewrite was judged.
fn resolved(
    action: &str,
    seq: u64,
    id: Option<&str>,
    decision: &str,
    decided_by: &str,
    reviewed: Option<&str>,
) -> String {
    let mut payload = json!({"decision": decision, "decided_by": decided_by});
    if let Some(id) = id {
        payload["request_id"] = Value::String(id.to_owned());
    }
    if let Some(reviewed) = reviewed {
        payload["arguments"] = json!({"command": reviewed});
    }
    line("permission_resolved", Some(action), seq, &payload)
}

/// A session holding one reviewed shell call per action id.
fn seed(home: &Path, key: &str, id: &str, body: &str) {
    let mut log = started("/w");
    log.push_str(body);
    write_log(&session_dir(home, key, id), log.as_bytes());
}

fn calls(home: &Path) -> Vec<ShellCall> {
    reviewer_shell_calls(home)
}

#[test]
fn reviewer_allows_join_with_the_repaired_command_preferred() {
    let home = fakes::TempDir::new("fiber-log-shell-allows");
    let mut body = String::new();
    for (at, action) in ["a_1", "a_2", "a_3", "a_4", "a_5"].iter().enumerate() {
        let seq = u64::try_from(at).unwrap() * 3 + 1;
        body.push_str(&requested_repaired(
            action,
            seq,
            "acme-cli query --xml",
            "acme-cli query --json",
        ));
        body.push_str(&resolved(action, seq + 1, None, "allow", "reviewer", None));
    }
    seed(home.path(), "p", "s_1", &body);
    let found = calls(home.path());
    assert_eq!(found.len(), 5);
    for call in &found {
        assert_eq!(call.requested.as_deref(), Some("acme-cli query --json"));
        assert_eq!(call.reviewed, None);
        assert_eq!(call.raw.as_deref(), Some("acme-cli query --json"));
        assert!(!call.ran_by);
        assert_eq!(call.rule_subject, None);
        assert_eq!(call.decision, "allow");
        assert_eq!(call.decided_by, "reviewer");
        assert!(!call.escalated);
        assert_eq!(call.project, "p");
        assert_eq!(call.identity.as_deref(), Some("/w"));
    }
}

#[test]
fn the_reviewed_command_wins_over_the_requested_one() {
    let home = fakes::TempDir::new("fiber-log-shell-reviewed");
    let mut body = requested_repaired("a_1", 1, "acme-cli query --xml", "acme-cli query --json");
    body.push_str(&resolved(
        "a_1",
        2,
        None,
        "allow",
        "reviewer",
        Some("acme-cli query --yaml"),
    ));
    seed(home.path(), "p", "s_1", &body);
    let found = calls(home.path());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].requested.as_deref(), Some("acme-cli query --json"));
    assert_eq!(found[0].reviewed.as_deref(), Some("acme-cli query --yaml"));
    assert_eq!(found[0].raw.as_deref(), Some("acme-cli query --yaml"));
}

#[test]
fn a_hook_rewritten_deny_carries_the_judged_subject() {
    let home = fakes::TempDir::new("fiber-log-shell-hook-deny");
    let mut body = requested("a_1", 1, "acme-cli query --xml");
    body.push_str(&request(
        "a_1",
        2,
        "r_1",
        Some("acme-cli query --json"),
        false,
    ));
    body.push_str(&resolved(
        "a_1",
        3,
        Some("r_1"),
        "deny",
        "reviewer",
        Some("acme-cli query --json"),
    ));
    seed(home.path(), "p", "s_1", &body);
    let found = calls(home.path());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].requested.as_deref(), Some("acme-cli query --xml"));
    assert_eq!(
        found[0].rule_subject.as_deref(),
        Some("acme-cli query --json")
    );
    assert_eq!(found[0].decision, "deny");
    assert_eq!(found[0].decided_by, "reviewer");
}

#[test]
fn multi_part_and_non_shell_allows_without_one_command_are_skipped() {
    let home = fakes::TempDir::new("fiber-log-shell-skipped");
    let mut body = String::new();
    // A non-shell call never joins, even allowed by the reviewer.
    body.push_str(&requested_tool("a_1", 1, "read"));
    body.push_str(&resolved("a_1", 2, Some("r_1"), "allow", "reviewer", None));
    // A shell call whose command comes in parts, not one string, proves
    // no single command: the multi-part call is skipped.
    body.push_str(&line(
        "tool_call_requested",
        Some("a_2"),
        3,
        &json!({"name": "shell", "arguments": {"command": ["acme-cli", "query"]}}),
    ));
    body.push_str(&resolved("a_2", 4, None, "allow", "reviewer", None));
    // A shell call naming no command at all is skipped too.
    body.push_str(&line(
        "tool_call_requested",
        Some("a_3"),
        5,
        &json!({"name": "shell", "arguments": {"workdir": "/w"}}),
    ));
    body.push_str(&resolved("a_3", 6, None, "allow", "reviewer", None));
    seed(home.path(), "p", "s_1", &body);
    assert!(calls(home.path()).is_empty());
}

#[test]
fn reviewer_allows_without_a_shell_requester_are_skipped() {
    let home = fakes::TempDir::new("fiber-log-shell-unprovable");
    let mut body = String::new();
    // No requester at all: fail closed, even with a judged command.
    body.push_str(&resolved(
        "a_1",
        1,
        None,
        "allow",
        "reviewer",
        Some("acme-cli query --json"),
    ));
    // A requester for another action does not join this resolution.
    body.push_str(&requested("a_2", 2, "acme-cli query --json"));
    seed(home.path(), "p", "s_1", &body);
    assert!(calls(home.path()).is_empty());
}

#[test]
fn denies_escalations_and_blocks_stay_attributable() {
    let home = fakes::TempDir::new("fiber-log-shell-attributable");
    let mut body = String::new();
    // A reviewer deny with no request: the requested text attributes it.
    body.push_str(&requested("a_1", 1, "acme-cli push"));
    body.push_str(&resolved("a_1", 2, None, "deny", "reviewer", None));
    // A person's deny by request: the request line joins it.
    body.push_str(&requested("a_2", 3, "acme-cli push --force"));
    body.push_str(&request(
        "a_2",
        4,
        "r_2",
        Some("acme-cli push --force"),
        false,
    ));
    body.push_str(&resolved("a_2", 5, Some("r_2"), "deny", "person", None));
    // An escalation the person allowed still escalated.
    body.push_str(&requested("a_3", 6, "acme-cli query --json"));
    body.push_str(&request("a_3", 7, "r_3", Some("acme-cli query"), true));
    body.push_str(&resolved("a_3", 8, Some("r_3"), "allow", "person", None));
    seed(home.path(), "p", "s_1", &body);
    let found = calls(home.path());
    assert_eq!(found.len(), 3);
    assert_eq!(found[0].raw.as_deref(), Some("acme-cli push"));
    assert_eq!(
        (found[0].decision.as_str(), found[0].decided_by.as_str()),
        ("deny", "reviewer")
    );
    assert!(!found[0].escalated);
    assert_eq!(
        found[1].rule_subject.as_deref(),
        Some("acme-cli push --force")
    );
    assert_eq!(
        (found[1].decision.as_str(), found[1].decided_by.as_str()),
        ("deny", "person")
    );
    assert!(!found[1].escalated);
    assert!(found[2].escalated);
    assert_eq!(
        (found[2].decision.as_str(), found[2].decided_by.as_str()),
        ("allow", "person")
    );
}

#[test]
fn ran_by_marks_inner_calls_for_allows_denies_and_blocks() {
    let home = fakes::TempDir::new("fiber-log-shell-ran-by");
    let mut body = String::new();
    for (at, (action, decision, decided_by)) in [
        ("a_1", "allow", "reviewer"),
        ("a_2", "deny", "reviewer"),
        ("a_3", "deny", "person"),
    ]
    .iter()
    .enumerate()
    {
        let seq = u64::try_from(at).unwrap() * 3 + 1;
        body.push_str(&line(
            "tool_call_requested",
            Some(action),
            seq,
            &json!({
                "name": "shell",
                "arguments": {"command": "acme-cli query --json"},
                "ran_by": {"extension": "acme", "command_id": "c_1"},
            }),
        ));
        body.push_str(&resolved(action, seq + 1, None, decision, decided_by, None));
    }
    seed(home.path(), "p", "s_1", &body);
    let found = calls(home.path());
    assert_eq!(found.len(), 3);
    for call in &found {
        assert!(call.ran_by, "an inner call stays marked");
        assert_eq!(call.raw.as_deref(), Some("acme-cli query --json"));
    }
    assert_eq!(found[0].decision, "allow");
    assert_eq!(found[1].decision, "deny");
    assert_eq!(found[2].decided_by, "person");
}

#[test]
fn raw_command_text_survives_what_the_classifier_cannot_parse() {
    let home = fakes::TempDir::new("fiber-log-shell-raw");
    // A deny whose command no classifier parses still joins whole: the
    // raw text is the containment fallback's input.
    let mut body = requested("a_1", 1, "((acme-cli query --json");
    body.push_str(&resolved("a_1", 2, None, "deny", "reviewer", None));
    seed(home.path(), "p", "s_1", &body);
    let found = calls(home.path());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].raw.as_deref(), Some("((acme-cli query --json"));
    assert_eq!(
        found[0].requested.as_deref(),
        Some("((acme-cli query --json")
    );
}

#[test]
fn a_torn_tail_and_an_unreadable_line_keep_what_came_before() {
    let home = fakes::TempDir::new("fiber-log-shell-torn");
    let mut body = requested("a_1", 1, "acme-cli query --json");
    body.push_str(&resolved("a_1", 2, None, "allow", "reviewer", None));
    // A torn tail: the cut line keeps the joined record before it.
    let dir = session_dir(home.path(), "p", "s_1");
    write_log(&dir, format!("{}{}", started("/w"), body).as_bytes());
    assert_eq!(calls(home.path()).len(), 1);
    // An unreadable complete line ends the walk with the earlier kept.
    let mut cut = format!("{}{}", started("/w"), body);
    cut.push_str("{not json}\n");
    cut.push_str(&requested("a_2", 3, "acme-cli push"));
    cut.push_str(&resolved("a_2", 4, None, "allow", "reviewer", None));
    write_log(&dir, cut.as_bytes());
    let found = calls(home.path());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].raw.as_deref(), Some("acme-cli query --json"));
    // A torn tail mid-log: the cut line keeps the joined record before it.
    let mut torn = format!("{}{}", started("/w"), body);
    torn.push_str(&requested("a_2", 3, "acme-cli push"));
    write_log(&dir, torn.as_bytes());
    let found = calls(home.path());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].raw.as_deref(), Some("acme-cli query --json"));
}

#[test]
fn calls_read_in_session_id_then_log_order_across_projects() {
    let home = fakes::TempDir::new("fiber-log-shell-order");
    seed(
        home.path(),
        "q",
        "s_b",
        &format!(
            "{}{}",
            requested("a_1", 1, "acme-cli query --json"),
            resolved("a_1", 2, None, "allow", "reviewer", None)
        ),
    );
    seed(
        home.path(),
        "p",
        "s_a",
        &format!(
            "{}{}",
            requested("a_2", 1, "acme-cli query --json"),
            resolved("a_2", 2, None, "allow", "reviewer", None)
        ),
    );
    let found = calls(home.path());
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].project, "p");
    assert_eq!(found[1].project, "q");
}
