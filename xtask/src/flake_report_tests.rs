//! Tests for `super::decide`: one per selection rule, each failing when
//! its comparison, condition or branch flips.

use serde_json::json;

use super::decide;

const TITLE: &str = "Flaky test: main::probe fails_once_then_passes";
const FIX: &str = "4444444444444444444444444444444444444444";

fn lines(input: &serde_json::Value) -> Vec<String> {
    decide(input)
        .expect("the fixture is valid")
        .iter()
        .map(super::Decision::line)
        .collect()
}

fn base() -> serde_json::Value {
    json!({
        "flakes": [{"title": TITLE}],
        "open": {"ok": true, "issues": []},
        "closed": {"ok": true, "issues": []},
        "events": {},
        "ancestry": {},
    })
}

fn closed_issue(number: u64, title: &str, closed_at: &str) -> serde_json::Value {
    json!({"number": number, "title": title, "closed_at": closed_at})
}

fn closed_event(id: u64, commit: Option<&str>, at: &str) -> serde_json::Value {
    json!({"event": "closed", "id": id, "commit_id": commit, "created_at": at})
}

fn with_closed(input: &mut serde_json::Value, issues: Vec<serde_json::Value>) {
    input["closed"]["issues"] = serde_json::Value::Array(issues);
}

fn with_events(input: &mut serde_json::Value, number: u64, events: Vec<serde_json::Value>) {
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
    input["ancestry"][FIX] = json!(true);
    assert_eq!(
        lines(&input),
        [format!(
            r#"{{"action":"reopen","issue":9,"title":"{TITLE}"}}"#
        )]
    );
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
    input["ancestry"][FIX] = json!(false);
    assert_eq!(
        lines(&input),
        [format!(
            r#"{{"action":"stale","issue":9,"title":"{TITLE}"}}"#
        )]
    );
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
    assert_eq!(
        lines(&input),
        [format!(
            r#"{{"action":"reopen","issue":9,"title":"{TITLE}"}}"#
        )]
    );
}

#[test]
fn no_closed_issue_files_new() {
    let input = base();
    assert_eq!(
        lines(&input),
        [format!(r#"{{"action":"file-new","title":"{TITLE}"}}"#)]
    );
}

#[test]
fn only_exact_titles_match() {
    let mut input = base();
    input["open"]["issues"] =
        json!([{"number": 7, "title": format!("{TITLE} extra")}, {"number": 8, "title": TITLE}]);
    assert_eq!(lines(&input), Vec::<String>::new());
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
    assert_eq!(
        lines(&input),
        [format!(
            r#"{{"action":"reopen","issue":10,"title":"{TITLE}"}}"#
        )]
    );
}

#[test]
fn open_match_emits_nothing() {
    let mut input = base();
    input["open"]["issues"] = json!([{"number": 7, "title": TITLE}]);
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    assert_eq!(lines(&input), Vec::<String>::new());
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
    assert_eq!(
        lines(&input),
        [format!(
            r#"{{"action":"reopen","issue":9,"title":"{TITLE}"}}"#
        )]
    );
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
    assert_eq!(
        lines(&input),
        [format!(
            r#"{{"action":"reopen","issue":10,"title":"{TITLE}"}}"#
        )]
    );
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
    input["ancestry"][FIX] = json!(false);
    assert_eq!(
        lines(&input),
        [format!(
            r#"{{"action":"stale","issue":9,"title":"{TITLE}"}}"#
        )]
    );
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
    assert_eq!(
        lines(&input),
        [format!(
            r#"{{"action":"reopen","issue":9,"title":"{TITLE}"}}"#
        )]
    );
}

#[test]
fn failed_open_lookup_is_unknown() {
    let mut input = base();
    input["open"]["ok"] = json!(false);
    assert_eq!(
        lines(&input),
        [format!(r#"{{"action":"unknown","title":"{TITLE}"}}"#)]
    );
}

#[test]
fn failed_closed_lookup_is_unknown() {
    let mut input = base();
    input["closed"]["ok"] = json!(false);
    assert_eq!(
        lines(&input),
        [format!(r#"{{"action":"unknown","title":"{TITLE}"}}"#)]
    );
}

#[test]
fn failed_events_lookup_is_unknown() {
    let mut input = base();
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    input["events"]["9"] = json!({"ok": false, "events": []});
    assert_eq!(
        lines(&input),
        [format!(
            r#"{{"action":"unknown","issue":9,"title":"{TITLE}"}}"#
        )]
    );
}

#[test]
fn missing_events_entry_is_unknown() {
    let mut input = base();
    with_closed(
        &mut input,
        vec![closed_issue(9, TITLE, "2026-10-24T11:00:00Z")],
    );
    assert_eq!(
        lines(&input),
        [format!(
            r#"{{"action":"unknown","issue":9,"title":"{TITLE}"}}"#
        )]
    );
}

#[test]
fn no_closed_event_is_unknown() {
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
    assert_eq!(
        lines(&input),
        [format!(
            r#"{{"action":"unknown","issue":9,"title":"{TITLE}"}}"#
        )]
    );
}

#[test]
fn missing_ancestry_result_is_unknown() {
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
    assert_eq!(
        lines(&input),
        [format!(
            r#"{{"action":"unknown","issue":9,"title":"{TITLE}"}}"#
        )]
    );
}

#[test]
fn duplicate_titles_decide_once() {
    let mut input = base();
    input["flakes"] = json!([{"title": TITLE}, {"title": TITLE}]);
    assert_eq!(
        lines(&input),
        [format!(r#"{{"action":"file-new","title":"{TITLE}"}}"#)]
    );
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
    assert_eq!(
        lines(&input),
        [format!(r#"{{"action":"file-new","title":"{TITLE}"}}"#)]
    );
}

#[test]
fn malformed_input_is_an_error() {
    assert!(decide(&json!({})).is_err());
    let mut input = base();
    input["flakes"] = json!({});
    assert!(decide(&input).is_err());
    let mut input = base();
    input["open"] = json!({"ok": "yes", "issues": []});
    assert!(decide(&input).is_err());
}
