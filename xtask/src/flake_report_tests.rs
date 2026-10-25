//! Tests for `super::{decide_with, event_requests}`: one per selection
//! rule, each failing when its comparison, condition or branch flips.
//! Decisions are parsed as JSON objects, never compared as strings, so
//! no test pins key order. Counts appear only where the
//! one-action-per-unique-title contract requires them
//! (`docs/testing/running.md`, "Flaky tests").

use std::collections::HashMap;

use serde_json::{Value, json};

use super::{
    Listed, Run, classify, decide_with, event_requests, failing_file, runner_lists, tested_line,
};

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

const SHA: &str = "1111111111111111111111111111111111111111";
const BASE_SHA: &str = "2222222222222222222222222222222222222222";
const HEAD_SHA: &str = "3333333333333333333333333333333333333333";
const FILE: &str = "crates/main/tests/flaky_probe.rs";
const PANIC: &str = "thread fails_once_then_passes panicked at crates/main/tests/flaky_probe.rs:10:9\\nfirst attempt";

fn pr_run<'a>(base: &'a str, head: &'a str, compare: &'a str, pr: &'a str) -> Run<'a> {
    Run {
        pull_request: true,
        tested_commit: SHA,
        base,
        head,
        compare_files: compare,
        pr_files: pr,
    }
}

fn push_run() -> Run<'static> {
    Run {
        pull_request: false,
        tested_commit: SHA,
        base: BASE_SHA,
        head: HEAD_SHA,
        compare_files: FILE,
        pr_files: FILE,
    }
}

/// The classifications as parsed JSON objects, in output order.
fn classified(lists: &[(&str, &str)], run: &Run) -> Vec<Value> {
    let listed: Vec<Listed> = lists
        .iter()
        .map(|(runner, text)| Listed { runner, text })
        .collect();
    classify(&listed, run)
        .iter()
        .map(|classified| {
            serde_json::from_str::<Value>(&classified.line()).expect("a record is one JSON object")
        })
        .collect()
}

fn field<'a>(record: &'a Value, key: &str) -> Option<&'a str> {
    record.get(key).and_then(Value::as_str)
}

/// The record for test `name`, found by its title: the writer loop handles
/// each record on its own, so record order is not part of the contract.
fn titled<'a>(out: &'a [Value], name: &str) -> &'a Value {
    let title = format!("Flaky test: {name}");
    out.iter()
        .find(|record| field(record, "title") == Some(title.as_str()))
        .unwrap_or_else(|| panic!("no record titled {title}"))
}

#[test]
fn a_panic_with_a_file_goes_on_to_the_lookup() {
    let line = format!("main::t\t{PANIC}");
    let out = classified(
        &[("ubuntu-24.04", &line)],
        &pr_run(BASE_SHA, HEAD_SHA, "other/file.rs", "other/file.rs"),
    );
    assert_eq!(out.len(), 1);
    assert_eq!(field(&out[0], "action"), Some("lookup"));
    assert_eq!(field(&out[0], "failing"), Some(FILE));
    assert_eq!(field(&out[0], "name"), Some("main::t"));
    assert_eq!(field(&out[0], "runner"), Some("ubuntu-24.04"));
    assert_eq!(field(&out[0], "title"), Some("Flaky test: main::t"));
    assert_eq!(field(&out[0], "detail"), Some(PANIC));
    let want_tested = format!("tested {SHA} = main {BASE_SHA} + head {HEAD_SHA}");
    assert_eq!(field(&out[0], "tested"), Some(want_tested.as_str()));
    assert_eq!(field(&out[0], "base"), None);
}

#[test]
fn a_panic_without_a_rs_line_has_no_failing_file() {
    let line = "main::t\tthread fails_once_then_passes panicked at nowhere";
    let out = classified(&[("linux", line)], &pr_run(BASE_SHA, HEAD_SHA, FILE, FILE));
    assert_eq!(out.len(), 1);
    assert_eq!(field(&out[0], "action"), Some("lookup"));
    assert_eq!(field(&out[0], "failing"), None);
}

#[test]
fn no_panic_text_has_no_failing_file() {
    // The file sits in both lists, yet with no panic the title still
    // goes on to the lookup: only a panic names a failing file.
    let line = format!("main::t\tthread failed at {FILE}:10: some output");
    let out = classified(&[("linux", &line)], &pr_run(BASE_SHA, HEAD_SHA, FILE, FILE));
    assert_eq!(out.len(), 1);
    assert_eq!(field(&out[0], "action"), Some("lookup"));
    assert_eq!(field(&out[0], "failing"), None);
    assert_eq!(failing_file("panicked  at crates/a/b.rs:10"), None);
    assert_eq!(failing_file("Panicked at crates/a/b.rs:10"), None);
}

#[test]
fn a_file_main_changed_is_a_stale_base() {
    let line = format!("main::t\t{PANIC}");
    let out = classified(
        &[("linux", &line)],
        &pr_run(BASE_SHA, HEAD_SHA, FILE, "other/file.rs"),
    );
    assert_eq!(out.len(), 1);
    assert_eq!(field(&out[0], "action"), Some("stale-base"));
    assert_eq!(field(&out[0], "failing"), Some(FILE));
    assert_eq!(field(&out[0], "base"), Some(BASE_SHA));
}

#[test]
fn a_file_the_pull_request_changed_is_pr_owned() {
    let line = format!("main::t\t{PANIC}");
    let out = classified(
        &[("linux", &line)],
        &pr_run(BASE_SHA, HEAD_SHA, "other/file.rs", FILE),
    );
    assert_eq!(out.len(), 1);
    assert_eq!(field(&out[0], "action"), Some("pr-owned"));
    assert_eq!(field(&out[0], "failing"), Some(FILE));
    assert_eq!(field(&out[0], "base"), None);
}

#[test]
fn only_a_stale_base_record_names_the_base() {
    // One pull-request run with a base yields all three classes: the
    // base field appears on the stale-base record and on no other.
    let text = "main::old\tthread old panicked at crates/old.rs:10\n\
                main::new\tthread new panicked at crates/new.rs:3\n\
                main::other\tthread other panicked at crates/other.rs:5\n";
    let out = classified(
        &[("linux", text)],
        &pr_run(BASE_SHA, HEAD_SHA, "crates/old.rs", "crates/new.rs"),
    );
    assert_eq!(out.len(), 3);
    for (name, action, base) in [
        ("main::old", "stale-base", Some(BASE_SHA)),
        ("main::new", "pr-owned", None),
        ("main::other", "lookup", None),
    ] {
        let record = titled(&out, name);
        assert_eq!(field(record, "action"), Some(action), "{record}");
        assert_eq!(field(record, "base"), base, "{record}");
    }
}

#[test]
fn stale_wins_over_pr_owned() {
    let line = format!("main::t\t{PANIC}");
    let out = classified(&[("linux", &line)], &pr_run(BASE_SHA, HEAD_SHA, FILE, FILE));
    assert_eq!(field(&out[0], "action"), Some("stale-base"));
}

#[test]
fn a_line_with_no_tab_has_empty_detail() {
    let out = classified(&[("linux", "main::t")], &push_run());
    assert_eq!(out.len(), 1);
    assert_eq!(field(&out[0], "name"), Some("main::t"));
    assert_eq!(field(&out[0], "detail"), Some(""));
    assert_eq!(field(&out[0], "title"), Some("Flaky test: main::t"));
    assert_eq!(field(&out[0], "failing"), None);
}

#[test]
fn a_line_with_two_tabs_keeps_the_second_in_the_detail() {
    // Only the first TAB splits: the rest of the line is the detail.
    let out = classified(&[("linux", "main::t\tfirst\tsecond")], &push_run());
    assert_eq!(field(&out[0], "detail"), Some("first\tsecond"));
}

#[test]
fn a_line_that_is_only_a_tab_names_the_empty_test() {
    let out = classified(&[("linux", "\t")], &push_run());
    assert_eq!(out.len(), 1);
    assert_eq!(field(&out[0], "name"), Some(""));
    assert_eq!(field(&out[0], "title"), Some("Flaky test: "));
}

#[test]
fn blank_lines_are_skipped() {
    let out = classified(&[("linux", "main::a\n\nmain::b\n")], &push_run());
    assert_eq!(out.len(), 2);
    titled(&out, "main::a");
    titled(&out, "main::b");
}

#[test]
fn a_last_line_without_a_newline_is_kept() {
    let out = classified(&[("linux", "main::a\nmain::b")], &push_run());
    assert_eq!(out.len(), 2);
    titled(&out, "main::b");
}

#[test]
fn a_carriage_return_is_data() {
    // Lines split on `\n` only: a `\r` stays in the name and detail.
    let out = classified(&[("linux", "main::a\r\tpanicked at x.rs:1\r")], &push_run());
    assert_eq!(out.len(), 1);
    assert_eq!(field(&out[0], "name"), Some("main::a\r"));
    assert_eq!(field(&out[0], "detail"), Some("panicked at x.rs:1\r"));
}

#[test]
fn a_repeated_title_is_written_once_from_its_first_occurrence() {
    let first = format!("main::t\t{PANIC}");
    let second = "main::t\tthread other panicked at crates/other.rs:3";
    let text = format!("{first}\n{second}\n");
    let out = classified(&[("linux", &text)], &push_run());
    assert_eq!(out.len(), 1);
    assert_eq!(field(&out[0], "detail"), Some(PANIC));
    let out = classified(
        &[("zeta", second), ("alpha", &first)],
        &Run {
            pull_request: false,
            tested_commit: SHA,
            base: "",
            head: "",
            compare_files: "",
            pr_files: "",
        },
    );
    assert_eq!(out.len(), 1);
    assert_eq!(field(&out[0], "runner"), Some("zeta"));
    assert_eq!(
        field(&out[0], "detail"),
        Some("thread other panicked at crates/other.rs:3")
    );
}

#[test]
fn list_files_are_read_in_name_byte_order() {
    // docs/testing/running.md, "Flaky tests", promises this order: "a
    // repeated title takes its runner and detail from its first
    // occurrence in file-name byte order". `-` (0x2d) sorts
    // before `.` (0x2e), so the arm file is read first and its
    // occurrence wins the dedupe.
    let names = vec![
        "flaky-tests-ubuntu-24.04.txt".to_owned(),
        "flaky-tests-ubuntu-24.04-arm.txt".to_owned(),
    ];
    let ordered = runner_lists(&names);
    assert_eq!(
        ordered,
        vec![
            (
                "ubuntu-24.04-arm".to_owned(),
                "flaky-tests-ubuntu-24.04-arm.txt".to_owned()
            ),
            (
                "ubuntu-24.04".to_owned(),
                "flaky-tests-ubuntu-24.04.txt".to_owned()
            ),
        ]
    );
    let other = vec![
        "collection-complete-x.txt".to_owned(),
        "flaky-tests-x.json".to_owned(),
        "flaky-tests-x.txt.bak".to_owned(),
        "flaky-tests-".to_owned(),
    ];
    assert!(runner_lists(&other).is_empty());
    let empty = vec!["flaky-tests-.txt".to_owned()];
    assert_eq!(
        runner_lists(&empty),
        vec![("".to_owned(), "flaky-tests-.txt".to_owned())]
    );
}

#[test]
fn classification_takes_no_issue_data() {
    // The classification consults only the file lists: the same flake
    // list still classifies stale-base with no issue data anywhere, and
    // an open exact-title match decides nothing, so the two files never
    // collapse into one comment.
    let line = format!("main::t\t{PANIC}");
    let context = pr_run(BASE_SHA, HEAD_SHA, FILE, "other/file.rs");
    let out = classified(&[("linux", &line)], &context);
    assert_eq!(field(&out[0], "action"), Some("stale-base"));
    let mut input = base();
    input["flakes"] = json!([{"title": "Flaky test: main::t"}]);
    input["open"]["issues"] = json!([{"number": 7, "title": "Flaky test: main::t"}]);
    let out = run(&input, &[]);
    assert!(out.is_empty());
}

#[test]
fn a_push_event_ignores_both_lists() {
    let line = format!("main::t\t{PANIC}");
    let out = classified(&[("linux", &line)], &push_run());
    assert_eq!(field(&out[0], "action"), Some("lookup"));
    assert_eq!(field(&out[0], "failing"), Some(FILE));
}

#[test]
fn a_push_event_ignores_the_pr_list() {
    // Only the pull-request list holds the file, yet a push run still
    // goes on to the lookup: the list checks need a pull request.
    let line = format!("main::t\t{PANIC}");
    let run = Run {
        pull_request: false,
        tested_commit: SHA,
        base: BASE_SHA,
        head: HEAD_SHA,
        compare_files: "",
        pr_files: FILE,
    };
    let out = classified(&[("linux", &line)], &run);
    assert_eq!(field(&out[0], "action"), Some("lookup"));
}

#[test]
fn a_push_event_has_the_short_tested_line() {
    assert_eq!(tested_line(&push_run()), format!("tested {SHA}"));
    let pr = pr_run(BASE_SHA, HEAD_SHA, "", "");
    assert_eq!(
        tested_line(&pr),
        format!("tested {SHA} = main {BASE_SHA} + head {HEAD_SHA}")
    );
}

#[test]
fn no_base_means_no_stale_check() {
    // An empty base never goes stale, even with the file in the
    // compare list: it falls through to the pull-request check.
    let line = format!("main::t\t{PANIC}");
    let out = classified(&[("linux", &line)], &pr_run("", HEAD_SHA, FILE, ""));
    assert_eq!(field(&out[0], "action"), Some("lookup"));
    let out = classified(&[("linux", &line)], &pr_run("", HEAD_SHA, FILE, FILE));
    assert_eq!(field(&out[0], "action"), Some("pr-owned"));
}

#[test]
fn tested_needs_both_parents() {
    let base_only = pr_run(BASE_SHA, "", "", "");
    assert_eq!(tested_line(&base_only), format!("tested {SHA}"));
    let head_only = pr_run("", HEAD_SHA, "", "");
    assert_eq!(tested_line(&head_only), format!("tested {SHA}"));
}

#[test]
fn list_membership_is_whole_line() {
    // A substring or a suffix of a listed path is not a match: only a
    // whole line counts.
    let line = "main::t\tthread t panicked at crates/a/b.rs:10";
    let run = pr_run(BASE_SHA, HEAD_SHA, "crates/a/b.rs", "crates/a/b.rs");
    let out = classified(&[("linux", line)], &run);
    assert_eq!(field(&out[0], "action"), Some("stale-base"));
    let short = pr_run(BASE_SHA, HEAD_SHA, "b.rs", "b.rs");
    let out = classified(&[("linux", line)], &short);
    assert_eq!(field(&out[0], "action"), Some("lookup"));
    let prefixed = pr_run(BASE_SHA, HEAD_SHA, "xcrates/a/b.rs", "xcrates/a/b.rs");
    let out = classified(&[("linux", line)], &prefixed);
    assert_eq!(field(&out[0], "action"), Some("lookup"));
}

#[test]
fn failing_file_scan_finds_the_first_rs_line() {
    assert_eq!(failing_file("panicked at a.rs:5"), Some("a.rs"));
    assert_eq!(failing_file("panicked at a.rs:1 b.rs:2"), Some("a.rs"));
    assert_eq!(
        failing_file("panicked at x.rs: crates/a/b.rs:9"),
        Some("crates/a/b.rs")
    );
    assert_eq!(failing_file("panicked at ./a.rs:5"), Some("./a.rs"));
    assert_eq!(
        failing_file("panicked at A-Z_0./-9.rs:42"),
        Some("A-Z_0./-9.rs")
    );
    assert_eq!(failing_file("panicked at a.rs:12"), Some("a.rs"));
    assert_eq!(failing_file("panicked at a.rs:12:9"), Some("a.rs"));
    assert_eq!(failing_file("panicked at x.rs.rs:5"), Some("x.rs.rs"));
    assert_eq!(failing_file("panicked at foo bar/a.rs:3"), Some("bar/a.rs"));
}

#[test]
fn failing_file_scan_rejects_a_digitless_or_bare_rs() {
    assert_eq!(failing_file("panicked at .rs:5"), None);
    assert_eq!(failing_file("panicked at a.rs:"), None);
    assert_eq!(failing_file("panicked at a.rs:x"), None);
    assert_eq!(failing_file("panicked at nowhere"), None);
    assert_eq!(failing_file("no panic here a.rs:5"), None);
}

#[test]
fn failing_file_scan_stops_the_run_at_punctuation() {
    assert_eq!(failing_file("panicked at (a.rs:3)"), Some("a.rs"));
    assert_eq!(failing_file("panicked at 'a.rs:3'"), Some("a.rs"));
}
