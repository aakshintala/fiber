//! Case-file fields and validation (`docs/testing.md`, "Testing an extension").

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "case parser assertions fail the test with their field context"
)]

use std::path::Path;

use serde_json::{Value, json};

use super::{CallOutcome, Case};

fn single(case: &super::CallCase) -> (&super::Call, &CallOutcome) {
    let super::CallOperation::Invoke { call, outcome } = &case.steps[0].operation else {
        panic!("expected a call");
    };
    (call, outcome)
}

#[test]
fn credential_case_fields_and_ordered_calls_are_read() {
    let value = json!({
        "calls": [
            {"call": {"provider": "p", "function": "login", "arg": {"method": "browser", "label": "work"}}, "returns": {"token": "one"}},
            {"await": "credential_idle"},
            {"call": {"provider": "p", "function": "credential", "arg": {}}, "returns": {"token": "two"}, "clock": [{"advance_ms": 1}]},
            {"call": {"provider": "p", "function": "sign", "arg": {"method": "POST", "url": "https://example.test", "headers": {}}}, "returns": {}}
        ],
        "credentials": {"p/default": {"token": "old"}},
        "expect_credentials": {"p/default": {"token": "new"}},
        "attended": false,
        "clock": [{"advance_ms": 2}],
        "host": {"oauth": [
            {"pkce": {"verifier": "v", "challenge": "c"}},
            {"open": {"url": "https://example.test"}},
            {"show": {"url": "https://example.test", "code": "1234"}},
            {"callback": {"reply": {"query": {"state": "v"}}}},
            {"callback": {"error": {"code": "io_failed", "message": "busy"}}}
        ]}
    });
    let Case::Call(case) = parse(&value).unwrap() else {
        panic!("expected calls");
    };
    assert_eq!(case.steps.len(), 4);
    assert!(matches!(
        case.steps[1].operation,
        super::CallOperation::AwaitCredentialIdle
    ));
    assert_eq!(case.steps[2].clock[0].advance_ms, 1);
    assert_eq!(case.clock[0].advance_ms, 2);
    assert_eq!(case.credentials["p/default"]["token"], "old");
    assert_eq!(case.expect_credentials["p/default"]["token"], "new");
    assert!(!case.attended);
    assert_eq!(case.host.oauth.len(), 5);
}

#[test]
fn new_case_fields_reject_malformed_shapes() {
    let base =
        json!({"call": {"provider": "p", "function": "credential", "arg": {}}, "returns": {}});
    for (field, value, reason) in [
        ("calls", json!([]), "call and calls"),
        ("credentials", json!([]), "credentials"),
        ("credentials", json!({"../default": {}}), "credentials"),
        ("credentials", json!({"p/../default": {}}), "credentials"),
        ("expect_credentials", json!({"p": {}}), "credential/label"),
        ("attended", json!(null), "attended"),
        ("clock", json!([{"advance_ms": 0}]), "positive"),
        (
            "clock",
            json!([{"advance_ms": 1, "after": {"kind": "notice"}}]),
            "clock.after",
        ),
        ("host", json!({"oauth": [{"open": {"url": 1}}]}), "url"),
        (
            "host",
            json!({"oauth": [{"pkce": {"verifier": "v"}}]}),
            "challenge",
        ),
        (
            "host",
            json!({"oauth": [{"callback": {"reply": {"query": {"code": 1}}}}]}),
            "query",
        ),
        (
            "host",
            json!({"oauth": [{"callback": {"error": {"code": "io_failed"}}}]}),
            "message",
        ),
        ("host", json!({"oauth": [{}]}), "exactly one"),
        (
            "host",
            json!({"oauth": [{"pkce": {}, "open": {}}]}),
            "exactly one",
        ),
    ] {
        let mut value_case = base.clone();
        value_case[field] = value;
        assert!(case_error(&value_case).contains(reason), "{value_case}");
    }
    for calls in [
        json!([]),
        json!([{"await": "other"}]),
        json!([{"await": "credential_idle", "call": {}}]),
        json!([{"call": {"provider": "p", "function": "sign", "arg": {}}}]),
    ] {
        assert!(parse(&json!({"calls": calls})).is_err(), "{calls}");
    }
}

#[test]
fn ordered_outcomes_and_clock_entries_stay_with_their_call() {
    let entry =
        json!({"call": {"provider": "p", "function": "credential", "arg": {}}, "returns": {}});
    for (field, value) in [
        ("returns", json!({})),
        ("error", json!({"code": "credential_failed"})),
    ] {
        let mut case = json!({"calls": [entry]});
        case[field] = value;
        assert!(
            case_error(&case).contains("their own returns or error"),
            "{case}"
        );
    }
    let mut entry = entry;
    entry["clock"] = json!([{"advance_ms": 1, "after": {"kind": "notice"}}]);
    assert!(case_error(&json!({"calls": [entry]})).contains("clock.after"));
    for (field, value) in [
        ("credentials", json!({})),
        ("expect_credentials", json!({})),
        ("exact_credentials", json!(true)),
        ("attended", json!(false)),
    ] {
        let mut case = session_case();
        case[field] = value;
        assert!(
            case_error(&case).contains("only valid in a call case"),
            "{case}"
        );
    }
}

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
fn exact_credentials_parses_boolean_values_and_defaults_to_false() {
    for (value, expected) in [(json!(true), true), (json!(false), false)] {
        let case = json!({
            "call": {"provider": "p", "function": "credential", "arg": {}},
            "returns": {},
            "exact_credentials": value
        });
        let Case::Call(parsed) = parse(&case).expect("boolean exact_credentials is valid") else {
            panic!("call case parsed as a session case");
        };
        assert_eq!(parsed.exact_credentials, expected);
    }

    let default = json!({
        "call": {"provider": "p", "function": "credential", "arg": {}},
        "returns": {}
    });
    let Case::Call(parsed) = parse(&default).expect("exact_credentials defaults to false") else {
        panic!("call case parsed as a session case");
    };
    assert!(!parsed.exact_credentials);

    let mut malformed = default;
    malformed["exact_credentials"] = json!(null);
    let error = case_error(&malformed);
    assert!(error.contains("exact_credentials"), "{error}");
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
    let Case::Session(parsed) = parse(&value).expect("valid session case") else {
        panic!("session case parsed as a call case");
    };
    assert_eq!(parsed.name.as_deref(), Some("field coverage"));
    assert_eq!(parsed.prompt, "hi");
    assert_eq!(
        parsed.config,
        Some(json!({"permissions": {"mode": "auto"}}))
    );
    assert_eq!(parsed.script, json!({"steps": [{"text": "hello"}]}));
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
    let Case::Call(parsed) = parse(&value).expect("valid call case") else {
        panic!("call case parsed as a session case");
    };
    assert_eq!(single(&parsed).0.provider, "openrouter");
    assert_eq!(single(&parsed).0.function, "cost");
    assert_eq!(single(&parsed).0.arg, json!({"generation_id": "gen-abc"}));
    assert!(matches!(
        single(&parsed).1,
        CallOutcome::Returns(returns) if *returns == json!(0.5)
    ));

    let error = json!({
        "call": {"provider": "openrouter", "function": "cost", "arg": {}},
        "error": {"code": "extension_failed", "message": "cost failed"}
    });
    let Case::Call(parsed) = parse(&error).expect("call case with expected error") else {
        panic!("call case parsed as a session case");
    };
    assert!(matches!(
        single(&parsed).1,
        CallOutcome::Error(error)
            if *error == json!({"code": "extension_failed", "message": "cost failed"})
    ));
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
        "call": {"provider": "openrouter", "function": "quota", "arg": {}},
        "returns": 0.5
    });
    let error = case_error(&value);
    assert!(
        error.contains("call.function") && error.contains("cost") && error.contains("models"),
        "{error}"
    );
}

#[test]
fn a_models_call_case_parses_with_a_list_return() {
    let value = json!({
        "call": {"provider": "openrouter", "function": "models", "arg": {}},
        "returns": [{"id": "m1"}]
    });
    let Case::Call(parsed) = parse(&value).expect("valid models call case") else {
        panic!("call case parsed as a session case");
    };
    assert_eq!(single(&parsed).0.function, "models");
    assert!(matches!(
        single(&parsed).1,
        CallOutcome::Returns(returns) if *returns == json!([{"id": "m1"}])
    ));
}

#[test]
fn a_models_call_case_rejects_a_non_list_return() {
    for returns in [json!(0.5), json!(null)] {
        let value = json!({
            "call": {"provider": "openrouter", "function": "models", "arg": {}},
            "returns": returns
        });
        let error = case_error(&value);
        assert!(error.contains("returns"), "{returns}: {error}");
    }
}

#[test]
fn a_call_case_accepts_null_returns_and_requires_one_outcome() {
    let call = json!({"call": {"provider": "p", "function": "cost", "arg": {}}, "returns": null});
    let Case::Call(parsed) = parse(&call).unwrap() else {
        panic!("call case parsed as a session case");
    };
    assert!(matches!(
        single(&parsed).1,
        CallOutcome::Returns(Value::Null)
    ));

    for value in [
        json!({"call": {"provider": "p", "function": "cost", "arg": {}}, "returns": 0.5, "error": {"code": "extension_failed"}}),
        json!({"call": {"provider": "p", "function": "cost", "arg": {}}}),
    ] {
        assert!(case_error(&value).contains("exactly one"));
    }
}

#[test]
fn a_call_return_at_zero_is_accepted_and_a_negative_return_is_refused() {
    let value = json!({
        "call": {"provider": "openrouter", "function": "cost", "arg": {}},
        "returns": 0
    });
    let Case::Call(parsed) = parse(&value).expect("zero is an allowed return") else {
        panic!("call case parsed as a session case");
    };
    assert!(matches!(single(&parsed).1, CallOutcome::Returns(value) if *value == json!(0)));

    let mut beyond = value;
    beyond["returns"] = json!(-0.1);
    let error = case_error(&beyond);
    assert!(error.contains("at or above 0"), "{error}");
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
