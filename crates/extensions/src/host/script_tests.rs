//! Scripted host-call matching (`docs/testing.md`, "Testing an extension").

use contract::ErrorCode;
use serde_json::{Value, json};

use super::{ExecEntry, ExecReply, HostScript, HttpEntry, json_matches};

#[test]
fn oauth_entries_match_in_order_and_misses_consume_nothing() {
    let script = HostScript::new(
        vec![],
        vec![],
        vec![
            json!({"pkce": {"verifier": "fixed", "challenge": "challenge"}}),
            json!({"open": {"url": "https://example.test"}}),
            json!({"show": {"url": "https://example.test/device", "code": "1234"}}),
            json!({"callback": {"reply": {"query": {"code": "yes"}}}}),
            json!({"callback": {"error": {"code": "io_failed", "message": "busy"}}}),
        ],
    );
    assert_eq!(script.unmet().len(), 5);
    assert!(
        script
            .oauth("open", &json!({"url": "https://example.test"}))
            .is_err()
    );
    assert_eq!(
        script.oauth("pkce", &json!({})).unwrap()["verifier"],
        "fixed"
    );
    assert!(script.oauth("open", &json!({"url": "wrong"})).is_err());
    assert!(
        script
            .oauth("open", &json!({"url": "https://example.test"}))
            .is_ok()
    );
    assert!(
        script
            .oauth(
                "show",
                &json!({"url": "https://example.test/device", "code": "1234"})
            )
            .is_ok()
    );
    assert_eq!(
        script.oauth("callback", &json!({})).unwrap()["query"]["code"],
        "yes"
    );
    assert_eq!(
        script.oauth("callback", &json!({})).unwrap_err(),
        (ErrorCode::IoFailed, "busy".to_owned())
    );
    assert!(script.oauth("pkce", &json!({})).is_err());
    assert_eq!(script.unmet().len(), 3, "only the three misses remain");
}

fn http(request: Value) -> HttpEntry {
    HttpEntry {
        request,
        reply: Ok((200, b"ok".to_vec())),
    }
}

fn exec(request: Value) -> ExecEntry {
    ExecEntry {
        request,
        reply: Ok(ExecReply {
            code: 0,
            stdout: "ok".to_owned(),
            stderr: String::new(),
        }),
    }
}

#[test]
fn json_matches_the_expected_subset_and_reports_the_first_path() {
    let cases = [
        (
            "object subset, nested and extra keys",
            json!({"payload": {"cost": 0.5}}),
            json!({"payload": {"cost": 0.5, "other": true}, "extra": 1}),
            true,
        ),
        (
            "null matches a missing key",
            json!({"payload": {"cost": null}}),
            json!({"payload": {}}),
            true,
        ),
        (
            "a non-null expected key does not match a missing key",
            json!({"cost": 1}),
            json!({}),
            false,
        ),
        (
            "null matches null",
            json!({"cost": null}),
            json!({"cost": null}),
            true,
        ),
        (
            "null does not match a present value",
            json!({"cost": null}),
            json!({"cost": 1}),
            false,
        ),
        (
            "equal arrays match by subset and number rules",
            json!([{"cost": 1}, {"nested": ["two"]}]),
            json!([{"cost": 1.0, "extra": true}, {"nested": ["two"]}]),
            true,
        ),
        (
            "unequal arrays of the same length do not match",
            json!([1, 2]),
            json!([1, 3]),
            false,
        ),
        (
            "arrays of different lengths do not match",
            json!([1]),
            json!([1, 2]),
            false,
        ),
        (
            "nested arrays match recursively",
            json!({"items": [[{"cost": 1}]]}),
            json!({"items": [[{"cost": 1.0, "extra": true}]]}),
            true,
        ),
        (
            "nested unequal arrays do not match",
            json!({"items": [[1, 2]]}),
            json!({"items": [[1, 3]]}),
            false,
        ),
        ("integer matches float", json!(1), json!(1.0), true),
        ("float matches integer", json!(1.0), json!(1), true),
        ("equal integers match", json!(2), json!(2), true),
        ("different integers do not match", json!(2), json!(3), false),
        (
            "equal large integers match",
            json!(9007199254740993u64),
            json!(9007199254740993u64),
            true,
        ),
        (
            "different large integers do not match",
            json!(9007199254740993u64),
            json!(9007199254740992u64),
            false,
        ),
        ("string does not match number", json!("1"), json!(1), false),
    ];

    for (name, expected, actual, matches) in cases {
        assert_eq!(json_matches(&expected, &actual).is_ok(), matches, "{name}");
    }

    let error = json_matches(
        &json!({"payload": {"cost": 0.5}}),
        &json!({"payload": {"cost": 0.4}}),
    )
    .unwrap_err();
    assert!(error.contains("payload.cost"), "{error}");
}

#[test]
fn http_misses_do_not_consume_entries_and_unmet_lists_misses_before_unused_entries() {
    let script = HostScript::new(
        vec![
            http(json!({"url": "https://example.test/one"})),
            http(json!({"url": "https://example.test/two"})),
        ],
        vec![],
        Vec::new(),
    );

    let mismatch = script.http(json!({"url": "https://example.test/wrong"}));
    assert_eq!(
        mismatch,
        Err((
            ErrorCode::ConnectionFailed,
            "host.http: the case scripts no reply for this request".to_owned(),
        ))
    );
    assert_eq!(
        script.http(json!({"url": "https://example.test/one"})),
        Ok((200, b"ok".to_vec()))
    );

    let unmet = script.unmet();
    assert_eq!(unmet.len(), 2);
    assert!(unmet[0].contains("host.http[1]"), "{}", unmet[0]);
    assert!(
        unmet[0].contains("https://example.test/wrong"),
        "{}",
        unmet[0]
    );
    assert!(unmet[1].contains("host.http[2]"), "{}", unmet[1]);
    assert!(unmet[1].contains("unused"), "{}", unmet[1]);
}

#[test]
fn http_call_past_the_script_is_a_miss_at_the_next_index() {
    let script = HostScript::new(
        vec![http(json!({"url": "https://example.test/one"}))],
        vec![],
        Vec::new(),
    );
    assert!(
        script
            .http(json!({"url": "https://example.test/one"}))
            .is_ok()
    );
    assert!(
        script
            .http(json!({"url": "https://example.test/two"}))
            .is_err()
    );
    let unmet = script.unmet();
    assert_eq!(unmet.len(), 1);
    assert!(unmet[0].contains("host.http[2]"), "{}", unmet[0]);
    assert!(
        unmet[0].contains("https://example.test/two"),
        "{}",
        unmet[0]
    );
}

#[test]
fn http_header_names_are_matched_without_case() {
    let script = HostScript::new(
        vec![http(json!({
            "method": "GET",
            "url": "https://example.test/",
            "headers": {"X-Case": "value"},
            "body": null
        }))],
        vec![],
        Vec::new(),
    );
    assert!(
        script
            .http(json!({
                "method": "GET",
                "url": "https://example.test/",
                "headers": {"x-case": "value"},
                "body": null
            }))
            .is_ok()
    );
    assert!(script.unmet().is_empty());
}

#[test]
fn exec_matches_program_arguments_and_working_directory_in_order() {
    let request = json!({
        "program": "git",
        "args": ["status", "--short"],
        "cwd": "/workspace"
    });
    let script = HostScript::new(vec![], vec![exec(request.clone())], Vec::new());

    assert!(
        script
            .exec(json!({"program": "other", "args": ["status", "--short"], "cwd": "/workspace"}))
            .is_err()
    );
    assert_eq!(
        script.exec(request),
        Ok(ExecReply {
            code: 0,
            stdout: "ok".to_owned(),
            stderr: String::new(),
        })
    );

    let unmet = script.unmet();
    assert_eq!(unmet.len(), 1);
    assert!(unmet[0].contains("host.exec[1]"), "{}", unmet[0]);
    assert!(unmet[0].contains("program"), "{}", unmet[0]);
    assert!(unmet[0].contains("other"), "{}", unmet[0]);
}

#[test]
fn exec_call_past_the_script_is_a_miss_at_the_next_index() {
    let script = HostScript::new(
        vec![],
        vec![exec(json!({
            "program": "git",
            "args": [],
            "cwd": "/workspace"
        }))],
        Vec::new(),
    );
    assert!(
        script
            .exec(json!({"program": "git", "args": [], "cwd": "/workspace"}))
            .is_ok()
    );
    assert_eq!(
        script.exec(json!({"program": "git", "args": ["status"], "cwd": "/workspace"})),
        Err((
            ErrorCode::IoFailed,
            "host.exec: the case scripts no reply for this request".to_owned(),
        ))
    );
    let unmet = script.unmet();
    assert_eq!(unmet.len(), 1);
    assert!(unmet[0].contains("host.exec[2]"), "{}", unmet[0]);
    assert!(unmet[0].contains("status"), "{}", unmet[0]);
}
