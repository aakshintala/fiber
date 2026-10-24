//! Tests for `super::{decide_with, event_requests}`: one per selection
//! rule, each failing when its comparison, condition or branch flips.
//! Decisions are parsed as JSON objects, never compared as strings, so
//! no test pins key order. Counts appear only where the
//! one-action-per-unique-title contract requires them
//! (`docs/testing/running.md`, "Flaky tests").

use std::collections::HashMap;

use serde_json::{Value, json};

use super::{decide_with, event_requests};

const TITLE: &str = "Flaky test: main::probe fails_once_then_passes";
const FIX: &str = "4444444444444444444444444444444444444444";

/// The decisions as parsed JSON objects, in output order.
fn run(input: &Value, ancestry: &[(&str, bool)]) -> Vec<Value> {
    let known: HashMap<&str, bool> = ancestry.iter().copied().collect();
    decide_with(input, &|fix| known.get(fix).copied())
        .expect("the fixture is valid")
        .iter()
        .map(|decision| {
            serde_json::from_str::<Value>(&decision.line()).expect("a decision is one JSON object")
        })
        .collect()
}

fn action(decision: &Value) -> &str {
    decision
        .get("action")
        .and_then(Value::as_str)
        .expect("a decision names its action")
}

fn issue(decision: &Value) -> Option<u64> {
    decision.get("issue").and_then(Value::as_u64)
}

fn source(decision: &Value) -> Option<&str> {
    decision.get("source").and_then(Value::as_str)
}

fn title(decision: &Value) -> &str {
    decision
        .get("title")
        .and_then(Value::as_str)
        .expect("a decision names its title")
}

fn base() -> Value {
    json!({
        "flakes": [{"title": TITLE}],
        "open": {"ok": true, "issues": []},
        "closed": {"ok": true, "issues": []},
        "events": {},
    })
}

fn closed_issue(number: u64, title: &str, closed_at: &str) -> Value {
    json!({"number": number, "title": title, "closed_at": closed_at})
}

fn closed_event(id: u64, commit: Option<&str>, at: &str) -> Value {
    json!({"event": "closed", "id": id, "commit_id": commit, "created_at": at})
}

fn with_closed(input: &mut Value, issues: Vec<Value>) {
    input["closed"]["issues"] = Value::Array(issues);
}

fn with_events(input: &mut Value, number: u64, events: Vec<Value>) {
    input["events"][number.to_string()] = json!({"ok": true, "events": events});
}

#[test]
fn ancestor_fix_reopens() {
    let mut input = base();
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    with_events(
        &mut input,
        9,
        vec![closed_event(1, Some(FIX), "2026-10-24T11:00:00Z")],
    );
    let out = run(&input, &[(FIX, true)]);
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "reopen");
    assert_eq!(issue(&out[0]), Some(9));
    assert_eq!(title(&out[0]), TITLE);
    assert_eq!(source(&out[0]), None);
}

#[test]
fn stale_fix_skips() {
    let mut input = base();
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    with_events(
        &mut input,
        9,
        vec![closed_event(1, Some(FIX), "2026-10-24T11:00:00Z")],
    );
    let out = run(&input, &[(FIX, false)]);
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "stale");
    assert_eq!(issue(&out[0]), Some(9));
    assert_eq!(title(&out[0]), TITLE);
}

#[test]
fn null_fix_reopens_on_any_failure() {
    let mut input = base();
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    with_events(
        &mut input,
        9,
        vec![closed_event(1, None, "2026-10-24T11:00:00Z")],
    );
    let out = run(&input, &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "reopen");
    assert_eq!(issue(&out[0]), Some(9));
}

#[test]
fn no_closed_issue_files_new() {
    let out = run(&base(), &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "file-new");
    assert_eq!(issue(&out[0]), None);
    assert_eq!(title(&out[0]), TITLE);
}

#[test]
fn only_exact_titles_match() {
    let mut input = base();
    input["open"]["issues"] =
        json!([{"number": 7, "title": format!("{TITLE} extra")}, {"number": 8, "title": TITLE}]);
    assert_eq!(run(&input, &[]), Vec::<Value>::new());
    let mut input = base();
    with_closed(
        &mut input,
        vec![
            closed_issue(9, &format!("{TITLE} extra"), "2026-10-24T12:00:00Z"),
            closed_issue(10, TITLE, "2026-10-24T11:00:00Z"),
        ],
    );
    with_events(
        &mut input,
        10,
        vec![closed_event(1, None, "2026-10-24T11:00:00Z")],
    );
    let out = run(&input, &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "reopen");
    assert_eq!(issue(&out[0]), Some(10));
}

#[test]
fn open_match_emits_nothing() {
    let mut input = base();
    input["open"]["issues"] = json!([{"number": 7, "title": TITLE}]);
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    assert_eq!(run(&input, &[]), Vec::<Value>::new());
}

#[test]
fn latest_closed_issue_wins_then_higher_number() {
    let mut input = base();
    with_closed(
        &mut input,
        vec![
            closed_issue(9, TITLE, "2026-10-24T12:00:00Z"),
            closed_issue(10, TITLE, "2026-10-24T11:00:00Z"),
        ],
    );
    with_events(
        &mut input,
        9,
        vec![closed_event(1, None, "2026-10-24T12:00:00Z")],
    );
    let out = run(&input, &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(issue(&out[0]), Some(9));
    let mut input = base();
    with_closed(
        &mut input,
        vec![
            closed_issue(9, TITLE, "2026-10-24T11:00:00Z"),
            closed_issue(10, TITLE, "2026-10-24T11:00:00Z"),
        ],
    );
    with_events(
        &mut input,
        10,
        vec![closed_event(1, None, "2026-10-24T11:00:00Z")],
    );
    let out = run(&input, &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(issue(&out[0]), Some(10));
}

#[test]
fn latest_closed_event_wins_then_greater_id() {
    let mut input = base();
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    with_events(
        &mut input,
        9,
        vec![
            closed_event(1, Some(FIX), "2026-10-24T12:00:00Z"),
            closed_event(2, None, "2026-10-24T11:00:00Z"),
        ],
    );
    // The older null close is not the fix, so the probe answer for the
    // latest fix decides: here it is stale.
    let out = run(&input, &[(FIX, false)]);
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "stale");
    let mut input = base();
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    with_events(
        &mut input,
        9,
        vec![
            closed_event(1, Some(FIX), "2026-10-24T11:00:00Z"),
            closed_event(2, None, "2026-10-24T11:00:00Z"),
        ],
    );
    let out = run(&input, &[(FIX, false)]);
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "reopen");
    assert_eq!(issue(&out[0]), Some(9));
}

#[test]
fn only_the_latest_fix_is_probed() {
    let mut input = base();
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    with_events(
        &mut input,
        9,
        vec![
            closed_event(
                1,
                Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                "2026-10-24T10:00:00Z",
            ),
            closed_event(2, Some(FIX), "2026-10-24T12:00:00Z"),
        ],
    );
    // The probe sees only the latest fix: an answer for the older commit
    // alone decides nothing.
    let probed: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    let out = decide_with(&input, &|fix| {
        probed.borrow_mut().push(fix.to_owned());
        (fix == FIX).then_some(true)
    })
    .expect("the fixture is valid");
    assert_eq!(probed.borrow().as_slice(), [FIX]);
    assert_eq!(out.len(), 1);
    let line: Value = serde_json::from_str(&out[0].line()).expect("one JSON object");
    assert_eq!(action(&line), "reopen");
}

#[test]
fn failed_open_lookup_is_unknown_with_its_source() {
    let mut input = base();
    input["open"]["ok"] = json!(false);
    let out = run(&input, &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "unknown");
    assert_eq!(source(&out[0]), Some("open"));
    assert_eq!(issue(&out[0]), None);
}

#[test]
fn failed_closed_lookup_is_unknown_with_its_source() {
    let mut input = base();
    input["closed"]["ok"] = json!(false);
    let out = run(&input, &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "unknown");
    assert_eq!(source(&out[0]), Some("closed"));
}

#[test]
fn failed_events_lookup_is_unknown_with_its_source() {
    let mut input = base();
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    input["events"]["9"] = json!({"ok": false, "events": []});
    let out = run(&input, &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "unknown");
    assert_eq!(source(&out[0]), Some("events"));
    assert_eq!(issue(&out[0]), Some(9));
}

#[test]
fn missing_events_entry_is_unknown_with_its_source() {
    let mut input = base();
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    let out = run(&input, &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "unknown");
    assert_eq!(source(&out[0]), Some("events"));
}

#[test]
fn no_closed_event_is_unknown_with_its_source() {
    let mut input = base();
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    with_events(
        &mut input,
        9,
        vec![
            json!({"event": "reopened", "id": 1, "commit_id": None::<String>, "created_at": "2026-10-24T11:00:00Z"}),
        ],
    );
    let out = run(&input, &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "unknown");
    assert_eq!(source(&out[0]), Some("events"));
}

#[test]
fn missing_ancestry_answer_is_unknown_with_its_source() {
    let mut input = base();
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    with_events(
        &mut input,
        9,
        vec![closed_event(1, Some(FIX), "2026-10-24T11:00:00Z")],
    );
    let out = run(&input, &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "unknown");
    assert_eq!(source(&out[0]), Some("ancestry"));
    assert_eq!(issue(&out[0]), Some(9));
}

#[test]
fn duplicate_titles_decide_once() {
    // The one-action-per-unique-title contract
    // (`docs/testing/running.md`, "Flaky tests"): two identical titles
    // emit one line, and two distinct titles emit two.
    let mut input = base();
    input["flakes"] = json!([{"title": TITLE}, {"title": TITLE}]);
    assert_eq!(run(&input, &[]).len(), 1);
    let mut input = base();
    input["flakes"] = json!([{"title": TITLE}, {"title": "Flaky test: other"}]);
    assert_eq!(run(&input, &[]).len(), 2);
}

#[test]
fn pull_requests_never_match() {
    let mut input = base();
    input["open"]["issues"] = json!([{"number": 7, "title": TITLE, "pull_request": {}}]);
    with_closed(
        &mut input,
        vec![
            json!({"number": 9, "title": TITLE, "closed_at": "2026-10-24T11:00:00Z", "pull_request": {}}),
        ],
    );
    let out = run(&input, &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(action(&out[0]), "file-new");
}

#[test]
fn malformed_input_is_an_error() {
    let probe = |_: &str| None;
    assert!(decide_with(&json!({}), &probe).is_err());
    let mut input = base();
    input["flakes"] = json!({});
    assert!(decide_with(&input, &probe).is_err());
    let mut input = base();
    input["open"] = json!({"ok": "yes", "issues": []});
    assert!(decide_with(&input, &probe).is_err());
    assert!(event_requests(&json!({})).is_err());
}

#[test]
fn event_requests_select_only_titles_needing_events() {
    // An open match and a title with no closed issue need no fetch; a
    // failed open lookup decides unknown without events.
    let mut input = base();
    input["flakes"] = json!([{"title": TITLE}, {"title": "Flaky test: other"}]);
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    let requests = event_requests(&input).expect("the fixture is valid");
    assert_eq!(requests, vec![super::IssueNumber(9)]);
    let mut input = base();
    input["open"]["issues"] = json!([{"number": 7, "title": TITLE}]);
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    assert_eq!(
        event_requests(&input).expect("the fixture is valid"),
        Vec::<super::IssueNumber>::new()
    );
    let mut input = base();
    input["open"]["ok"] = json!(false);
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    assert_eq!(
        event_requests(&input).expect("the fixture is valid"),
        Vec::<super::IssueNumber>::new()
    );
}
