//! Case-file fields and validation (`docs/testing.md`, "Testing an extension").

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "case parser assertions fail the test with their field context"
)]

use std::path::Path;

use serde_json::{Value, json};

use super::Case;

fn session_case() -> Value {
    json!({
        "script": {"steps": [{"text": "hello"}]},
        "prompt": "hi",
        "expect": [{"kind": "turn_completed"}]
    })
}

fn parse(value: &Value) -> Result<Case, String> {
    Case::parse(Path::new("case.json"), value.to_string().as_bytes())
}

fn case_error(value: &Value) -> String {
    parse(value).err().expect("malformed case is rejected")
}

#[test]
fn every_session_field_and_each_host_reply_shape_is_read() {
    let value = json!({
        "name": "field coverage",
        "script": {"steps": [{"text": "hello"}]},
        "prompt": "hi",
        "config": {"permissions": {"mode": "auto"}},
        "host": {
            "http": [
                {"request": {"method": "GET", "headers": {"Authorization": "Bearer test"}},
                 "reply": {"status": 200, "body": "{\"ok\":true}"}},
                {"request": {"url": "https://example.test/error"},
                 "reply": {"error": {"code": "connection_failed", "message": "offline"}}}
            ],
            "exec": [
                {"request": {"program": "git", "args": ["status"], "cwd": "/workspace"},
                 "reply": {"code": 7, "stdout": "out", "stderr": "err"}},
                {"request": {"program": "git", "args": [], "cwd": null},
                 "reply": {"code": 0, "stdout": "", "stderr": ""}}
            ]
        },
        "clock": [{"after": {"kind": "turn_completed", "nth": 2}, "advance_ms": 500}],
        "until": {"kind": "notice", "nth": 1},
        "expect": [{"kind": "turn_completed", "payload": {"outcome": "completed"}}]
    });
    let parsed = parse(&value).expect("valid session case");
    assert_eq!(parsed.name.as_deref(), Some("field coverage"));
    assert_eq!(parsed.prompt.as_deref(), Some("hi"));
    assert_eq!(
        parsed.config,
        Some(json!({"permissions": {"mode": "auto"}}))
    );
    assert_eq!(parsed.script, Some(json!({"steps": [{"text": "hello"}]})));
    assert_eq!(parsed.clock.len(), 1);
    assert_eq!(parsed.clock[0].advance_ms, 500);
    assert_eq!(
        parsed.clock[0]
            .after
            .as_ref()
            .map(|after| (after.kind.as_str(), after.nth)),
        Some(("turn_completed", 2))
    );
    assert_eq!(parsed.host.http.len(), 2);
    assert_eq!(parsed.host.http[0].request["method"], "GET");
    assert_eq!(parsed.host.http[0].reply.as_ref().unwrap().0, 200);
    assert_eq!(
        parsed.host.http[0].reply.as_ref().unwrap().1,
        br#"{"ok":true}"#
    );
    assert_eq!(
        parsed.host.http[1].reply.as_ref().unwrap_err().0,
        contract::ErrorCode::ConnectionFailed
    );
    assert_eq!(parsed.host.exec.len(), 2);
    assert_eq!(parsed.host.exec[0].request["program"], "git");
    assert_eq!(parsed.host.exec[0].request["cwd"], "/workspace");
    assert_eq!(parsed.host.exec[1].request["cwd"], Value::Null);
    let reply = parsed.host.exec[0].reply.as_ref().unwrap();
    assert_eq!(
        (reply.code, reply.stdout.as_str(), reply.stderr.as_str()),
        (7, "out", "err")
    );
    assert_eq!(
        parsed
            .until
            .as_ref()
            .map(|until| (until.kind.as_str(), until.nth)),
        Some(("notice", 1))
    );
    assert_eq!(parsed.expect.len(), 1);
}

#[test]
fn every_call_field_is_read() {
    let value = json!({
        "call": {"provider": "openrouter", "function": "cost", "arg": {"generation_id": "gen-abc"}},
        "host": {"http": [{"request": {"url": "https://example.test"},
                            "reply": {"status": 200, "body": "{}"}}]},
        "returns": 0.5
    });
    let parsed = parse(&value).expect("valid call case");
    let call = parsed.call.as_ref().unwrap();
    assert_eq!(call.provider, "openrouter");
    assert_eq!(call.function, "cost");
    assert_eq!(call.arg, json!({"generation_id": "gen-abc"}));
    assert_eq!(parsed.returns, Some(json!(0.5)));
    assert!(parsed.error.is_none());

    let error = json!({
        "call": {"provider": "openrouter", "function": "cost", "arg": {}},
        "error": {"code": "extension_failed", "message": "cost failed"}
    });
    let parsed = parse(&error).expect("call case with expected error");
    assert_eq!(
        parsed.error,
        Some(json!({"code": "extension_failed", "message": "cost failed"}))
    );
}

#[test]
fn malformed_case_fields_name_the_field() {
    let mut cases = Vec::new();
    let mut value = session_case();
    value["name"] = json!(12);
    cases.push(("name", value));
    let mut value = session_case();
    value["script"] = json!({"steps": [false]});
    cases.push(("script", value));
    let mut value = session_case();
    value["prompt"] = json!(false);
    cases.push(("prompt", value));
    let mut value = session_case();
    value["config"] = json!(false);
    cases.push(("config", value));
    let mut value = session_case();
    value["host"] = json!({"http": [{"reply": {"status": 200, "body": ""}}]});
    cases.push(("request", value));
    let mut value = session_case();
    value["host"] = json!({"http": [{"request": {}, "reply": {"status": 200}}]});
    cases.push(("body", value));
    let mut value = session_case();
    value["host"] = json!({"http": [{"request": {}, "reply": {"status": 99, "body": ""}}]});
    cases.push(("status", value));
    let mut value = session_case();
    value["host"] =
        json!({"http": [{"request": {}, "reply": {"error": {"code": 7, "message": "x"}}}]});
    cases.push(("code", value));
    let mut value = session_case();
    value["host"] =
        json!({"exec": [{"request": {"args": []}, "reply": {"code": 0, "stdout": ""}}]});
    cases.push(("program", value));
    let mut value = session_case();
    value["host"] =
        json!({"exec": [{"request": {"program": "git"}, "reply": {"code": 0, "stdout": ""}}]});
    cases.push(("stderr", value));
    let mut value = session_case();
    value["clock"] = json!([{"advance_ms": 0}]);
    cases.push(("advance_ms", value));
    let mut value = session_case();
    value["clock"] = json!([{"advance_ms": 1, "after": {"kind": 3}}]);
    cases.push(("after.kind", value));
    let mut value = session_case();
    value["until"] = json!({"kind": "notice", "nth": 0});
    cases.push(("nth", value));
    let mut value = session_case();
    value["expect"] = json!({"kind": "turn_completed"});
    cases.push(("expect", value));
    let value = json!({"call": {"provider": "p", "function": "cost", "arg": {}}, "returns": "not a number"});
    cases.push(("returns", value));
    let value = json!({"call": {"provider": "p", "function": "cost", "arg": {}}, "returns": -0.1});
    cases.push(("returns", value));
    let value = json!({"call": {"provider": "p", "function": "cost", "arg": {}}, "error": "bad"});
    cases.push(("error", value));
    let value = json!({"call": {"provider": "p", "function": "cost", "arg": {}}, "error": {"code": "extension_failed", "message": 7}});
    cases.push(("error.message", value));
    let mut value = session_case();
    value["returns"] = json!(0.5);
    cases.push(("returns and error", value));
    let mut value = session_case();
    value["error"] = json!({"code": "extension_failed"});
    cases.push(("returns and error", value));
    let mut value = session_case();
    value["host"] = json!({"exec": [{"request": {"program": "git", "args": "status"}, "reply": {"code": 0, "stdout": "", "stderr": ""}}]});
    cases.push(("args", value));
    let mut value = session_case();
    value["host"] = json!({"exec": [{"request": {"program": "git", "args": [1]}, "reply": {"code": 0, "stdout": "", "stderr": ""}}]});
    cases.push(("args", value));
    let mut value = session_case();
    value["host"] = json!({"exec": [{"request": {"program": "git", "cwd": true}, "reply": {"code": 0, "stdout": "", "stderr": ""}}]});
    cases.push(("cwd", value));

    for (field, value) in cases {
        let error = parse(&value)
            .err()
            .unwrap_or_else(|| panic!("{field}: case was accepted"));
        assert!(error.contains(field), "{field}: {error}");
    }
}

#[test]
fn a_call_case_names_the_supported_provider_functions() {
    let value = json!({
        "call": {"provider": "openrouter", "function": "models", "arg": {}},
        "returns": 0.5
    });
    let error = case_error(&value);
    assert!(
        error.contains("call.function") && error.contains("cost"),
        "{error}"
    );
}

#[test]
fn a_call_case_accepts_null_returns_and_requires_one_outcome() {
    let call = json!({"call": {"provider": "p", "function": "cost", "arg": {}}, "returns": null});
    assert_eq!(parse(&call).unwrap().returns, Some(Value::Null));

    for value in [
        json!({"call": {"provider": "p", "function": "cost", "arg": {}}, "returns": 0.5, "error": {"code": "extension_failed"}}),
        json!({"call": {"provider": "p", "function": "cost", "arg": {}}}),
    ] {
        assert!(case_error(&value).contains("exactly one"));
    }
}

#[test]
fn a_case_cannot_mix_a_prompt_and_a_call() {
    let mut value = session_case();
    value["call"] = json!({"provider": "p", "function": "cost", "arg": {}});
    value["returns"] = json!(0);
    let error = case_error(&value);
    assert!(
        error.contains("prompt") && error.contains("call"),
        "{error}"
    );
}

#[test]
fn unknown_keys_and_zero_nth_are_refused() {
    let mut value = session_case();
    value["surprise"] = json!(true);
    let error = case_error(&value);
    assert!(error.contains("surprise"), "{error}");

    let mut value = session_case();
    value["clock"] = json!([{"after": {"kind": "turn_completed", "nth": 0}, "advance_ms": 1}]);
    let error = case_error(&value);
    assert!(error.contains("nth"), "{error}");
}
