//! Case-file fields and validation (`docs/testing.md`, "Testing an extension").

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "case parser assertions fail the test with their field context"
)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Value, json};

use super::{CallOutcome, Case};

#[test]
fn a_script_check_error_keeps_its_context_and_path() {
    let path = Path::new("providers/acme/tests/bad-script.json");
    let checked_path = Mutex::new(None::<PathBuf>);
    let check_script = |checked: &Path, _bytes: &[u8]| {
        *checked_path.lock().expect("script checker lock") = Some(checked.to_path_buf());
        Err("bad script".to_owned())
    };
    let error = Case::parse(
        path,
        br#"{"script":{"steps":[false]},"prompt":"hi","expect":[]}"#,
        &check_script,
    )
    .err()
    .expect("a rejected script returns an error");

    assert!(matches!(
        error,
        crate::Error::Case(message) if message == "script: bad script"
    ));
    assert_eq!(
        *checked_path.lock().expect("script checker lock"),
        Some(path.to_path_buf())
    );
}

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
    ] {
        let mut case = session_case();
        case[field] = value;
        assert!(
            case_error(&case).contains("only valid in a call case"),
            "{case}"
        );
    }
}

#[test]
fn session_case_attended_defaults_to_true_and_parses_booleans() {
    // No key reads attended, so the cases written before this field stay
    // with a person attached.
    let Case::Session(parsed) = parse(&session_case()).expect("session case parses") else {
        panic!("session case parsed as a call case");
    };
    assert!(parsed.attended);
    for (value, expected) in [(json!(true), true), (json!(false), false)] {
        let mut case = session_case();
        case["attended"] = value;
        let Case::Session(parsed) = parse(&case).expect("boolean attended parses") else {
            panic!("session case parsed as a call case");
        };
        assert_eq!(parsed.attended, expected);
    }
    for value in [json!(null), json!(0), json!(1), json!("false")] {
        let mut malformed = session_case();
        malformed["attended"] = value;
        let error = case_error(&malformed);
        assert!(error.contains("attended"), "{error}");
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
    parse_with_script_check(value, &|_, _| Ok(()))
}

fn parse_with_script_check(
    value: &Value,
    check_script: &super::ScriptCheck<'_>,
) -> Result<Case, String> {
    Case::parse(
        Path::new("case.json"),
        value.to_string().as_bytes(),
        check_script,
    )
    .map_err(|error| error.to_string())
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
    let super::Input::Prompt(prompt) = &parsed.input else {
        panic!("session case parsed without a prompt");
    };
    assert_eq!(prompt, "hi");
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
    let Ok(crate::ExecReply::Completed {
        end,
        stdout,
        stderr,
    }) = &parsed.host.exec[0].reply
    else {
        panic!("code reply parsed as {:?}", parsed.host.exec[0].reply);
    };
    assert_eq!(
        (*end, stdout.as_str(), stderr.as_str()),
        (crate::ExecEnd::Code(7), "out", "err")
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
    let mut value = session_case();
    value["host"] = json!({"exec": [{"request": {"program": "git", "env": "x"}, "reply": {"code": 0, "stdout": "", "stderr": ""}}]});
    cases.push(("env", value));
    let mut value = session_case();
    value["host"] = json!({"exec": [{"request": {"program": "git", "env": {"A": 1}}, "reply": {"code": 0, "stdout": "", "stderr": ""}}]});
    cases.push(("env", value));

    for (field, value) in cases {
        let error = parse(&value)
            .err()
            .unwrap_or_else(|| panic!("{field}: case was accepted"));
        assert!(error.contains(field), "{field}: {error}");
    }

    let mut invalid_script = session_case();
    invalid_script["script"] = json!({"steps": [false]});
    let error = parse_with_script_check(&invalid_script, &|path, _| {
        Err(format!(
            "The script `{}` is malformed at step 1: the step is not an object.",
            path.display()
        ))
    })
    .err()
    .expect("the script checker rejects its malformed script");
    assert!(error.contains("script"), "{error}");
}

#[test]
fn an_exec_request_with_env_is_read() {
    let value = json!({
        "script": {"steps": [{"text": "hello"}]},
        "prompt": "hi",
        "host": {"exec": [{"request": {"program": "git", "env": {"FIBER_X": "1"}},
                               "reply": {"code": 0, "stdout": "", "stderr": ""}}]},
        "expect": [{"kind": "turn_completed"}]
    });
    let Case::Session(parsed) = parse(&value).expect("valid session case") else {
        panic!("session case parsed as a call case");
    };
    assert_eq!(parsed.host.exec.len(), 1);
    assert_eq!(parsed.host.exec[0].request["env"], json!({"FIBER_X": "1"}));
}

#[test]
fn a_call_case_names_the_supported_provider_functions() {
    let value = json!({
        "call": {"provider": "openrouter", "function": "usage", "arg": {}},
        "returns": 0.5
    });
    let error = case_error(&value);
    assert!(
        error.contains("call.function")
            && error.contains("cost")
            && error.contains("models")
            && error.contains("quota"),
        "{error}"
    );
}

#[test]
fn a_quota_call_case_parses_with_null_or_a_table() {
    for returns in [json!(null), json!({"credit": {"remaining": 1}})] {
        let value = json!({
            "call": {"provider": "openrouter", "function": "quota", "arg": {"key": "sk"}},
            "returns": returns,
        });
        let Case::Call(parsed) = parse(&value).expect("valid quota call case") else {
            panic!("call case parsed as a session case");
        };
        assert_eq!(single(&parsed).0.function, "quota");
    }
    for returns in [json!(0.5), json!([]), json!("x")] {
        let value = json!({
            "call": {"provider": "openrouter", "function": "quota", "arg": {}},
            "returns": returns,
        });
        let error = case_error(&value);
        assert!(error.contains("returns:"), "{error}");
    }
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

#[test]
fn a_signal_exec_reply_parses_to_a_signal_end() {
    let mut value = session_case();
    value["host"] = json!({"exec": [{"request": {"program": "sh"},
        "reply": {"signal": 9, "stdout": "", "stderr": ""}}]});
    let Case::Session(parsed) = parse(&value).expect("signal reply parses") else {
        panic!("signal reply parsed as a call case");
    };
    assert!(
        matches!(
            parsed.host.exec[0].reply,
            Ok(crate::ExecReply::Completed {
                end: crate::ExecEnd::Signal(9),
                ..
            })
        ),
        "{:?}",
        parsed.host.exec[0].reply
    );
    let mut value = session_case();
    value["host"] = json!({"exec": [{"request": {"program": "git", "args": ["status"]},
        "reply": {"code": 7, "stdout": "out", "stderr": "err"}}]});
    let Case::Session(parsed) = parse(&value).expect("code reply still parses") else {
        panic!("code reply parsed as a call case");
    };
    assert!(
        matches!(
            parsed.host.exec[0].reply,
            Ok(crate::ExecReply::Completed {
                end: crate::ExecEnd::Code(7),
                ..
            })
        ),
        "{:?}",
        parsed.host.exec[0].reply
    );
}

#[test]
fn an_exec_reply_with_both_code_and_signal_names_both_fields() {
    let mut value = session_case();
    value["host"] = json!({"exec": [{"request": {"program": "sh"},
        "reply": {"code": 0, "signal": 9, "stdout": "", "stderr": ""}}]});
    let error = case_error(&value);
    assert!(error.contains("host.exec[1].reply"), "{error}");
    assert!(
        error.contains("expected exactly one of code or signal"),
        "{error}"
    );
}

#[test]
fn an_exec_reply_with_neither_code_nor_signal_names_both_fields() {
    let mut value = session_case();
    value["host"] = json!({"exec": [{"request": {"program": "sh"},
        "reply": {"stdout": "", "stderr": ""}}]});
    let error = case_error(&value);
    assert!(error.contains("host.exec[1].reply"), "{error}");
    assert!(
        error.contains("expected exactly one of code or signal"),
        "{error}"
    );
}

#[test]
fn a_hang_reply_parses_to_a_hang_end() {
    let mut value = session_case();
    value["host"] = json!({"exec": [{"request": {"program": "sh"},
        "reply": {"hang": true}}]});
    let Case::Session(parsed) = parse(&value).expect("hang reply parses") else {
        panic!("hang reply parsed as a call case");
    };
    assert!(
        matches!(parsed.host.exec[0].reply, Ok(crate::ExecReply::Hang)),
        "{:?}",
        parsed.host.exec[0].reply
    );
}

#[test]
fn a_hang_reply_with_any_other_key_names_hang() {
    for reply in [
        json!({"hang": true, "code": 0}),
        json!({"hang": true, "signal": 9}),
        json!({"hang": true, "stdout": ""}),
        json!({"hang": true, "stderr": ""}),
        json!({"hang": true, "code": 0, "stdout": "", "stderr": ""}),
    ] {
        let mut value = session_case();
        value["host"] = json!({"exec": [{"request": {"program": "sh"},
            "reply": reply}]});
        let error = case_error(&value);
        assert!(error.contains("host.exec[1].reply.hang"), "{error}");
        assert!(error.contains("takes no other key"), "{error}");
    }
}

#[test]
fn a_hang_reply_that_is_not_true_names_hang() {
    for hang in [json!(false), json!(0), json!(1), json!("true"), json!(null)] {
        let mut value = session_case();
        value["host"] = json!({"exec": [{"request": {"program": "sh"},
            "reply": {"hang": hang}}]});
        let error = case_error(&value);
        assert!(error.contains("host.exec[1].reply.hang"), "{hang}: {error}");
        assert!(error.contains("expected true"), "{hang}: {error}");
    }
}

#[test]
fn a_clock_hang_must_be_a_positive_whole_number() {
    for hang in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!("1"),
        json!(null),
        json!(false),
    ] {
        let mut value = session_case();
        value["host"] = json!({"exec": [{"request": {"program": "sh"},
            "reply": {"hang": true}}]});
        value["clock"] = json!([{"advance_ms": 1000, "hang": hang}]);
        let error = case_error(&value);
        assert!(error.contains("clock[1].hang"), "{hang}: {error}");
        assert!(error.contains("positive whole number"), "{hang}: {error}");
    }
}

#[test]
fn a_clock_hang_must_name_a_hang_exec_entry() {
    let hang_case = || {
        let mut value = session_case();
        value["host"] = json!({"exec": [
            {"request": {"program": "sh"}, "reply": {"hang": true}},
            {"request": {"program": "git"}, "reply": {"code": 0, "stdout": "", "stderr": ""}},
        ]});
        value
    };
    // At the edge: entry 1 hangs and parses.
    let mut value = hang_case();
    value["clock"] = json!([{"advance_ms": 1000, "hang": 1}]);
    let Case::Session(parsed) = parse(&value).expect("hang 1 names the hang entry") else {
        panic!("hang clock parsed as a call case");
    };
    assert_eq!(parsed.clock[0].hang, Some(1));
    // Just above the hanging entries: entry 2 ends with a code.
    // Just above the script: entry 3 does not exist.
    for hang in [2, 3] {
        let mut value = hang_case();
        value["clock"] = json!([{"advance_ms": 1000, "hang": hang}]);
        let error = case_error(&value);
        assert!(error.contains("clock[1].hang"), "{hang}: {error}");
        assert!(error.contains("host.exec"), "{hang}: {error}");
    }
}

#[test]
fn a_clock_hang_in_a_call_case_names_hang() {
    let call = json!({"call": {"provider": "p", "function": "cost", "arg": {}}, "returns": 0.5});
    // No host entry to name: the clock validation names hang.
    let mut value = call.clone();
    value["clock"] = json!([{"advance_ms": 1, "hang": 1}]);
    let error = case_error(&value);
    assert!(error.contains("hang"), "{error}");
    // A hang naming a real hang entry is still refused in a call case.
    value["host"] = json!({"exec": [{"request": {"program": "sh"},
        "reply": {"hang": true}}]});
    let error = case_error(&value);
    assert!(error.contains("clock.hang"), "{error}");
    let mut steps = json!({"calls": [
        {"call": {"provider": "p", "function": "cost", "arg": {}}, "returns": 0.5, "clock": [{"advance_ms": 1, "hang": 1}]},
    ]});
    let error = case_error(&steps);
    assert!(error.contains("hang"), "{error}");
    steps["host"] = json!({"exec": [{"request": {"program": "sh"},
        "reply": {"hang": true}}]});
    let error = case_error(&steps);
    assert!(error.contains("clock.hang"), "{error}");
}

#[test]
fn an_exec_signal_outside_the_i32_range_is_rejected() {
    for signal in [
        json!(i64::from(i32::MAX) + 1),
        json!(i64::from(i32::MIN) - 1),
    ] {
        let mut value = session_case();
        value["host"] = json!({"exec": [{"request": {"program": "sh"},
            "reply": {"signal": signal, "stdout": "", "stderr": ""}}]});
        let error = case_error(&value);
        assert!(
            error.contains("host.exec[1].reply.signal"),
            "{signal}: {error}"
        );
        assert!(
            error.contains("outside the process signal range"),
            "{signal}: {error}"
        );
    }
    for signal in [json!(i32::MAX), json!(i32::MIN)] {
        let mut value = session_case();
        value["host"] = json!({"exec": [{"request": {"program": "sh"},
            "reply": {"signal": signal, "stdout": "", "stderr": ""}}]});
        parse(&value).expect("an i32 signal parses");
    }
}

#[test]
fn an_exec_signal_of_the_wrong_type_is_rejected() {
    let mut value = session_case();
    value["host"] = json!({"exec": [{"request": {"program": "sh"},
        "reply": {"signal": "9", "stdout": "", "stderr": ""}}]});
    let error = case_error(&value);
    assert!(error.contains("host.exec[1].reply.signal"), "{error}");
    assert!(error.contains("expected a whole number"), "{error}");
}

#[test]
fn a_settings_object_is_read_whole() {
    let mut value = session_case();
    value["settings"] =
        json!({"hooks": {"hooks": {"probe": {"point": "session_start", "command": "probe"}}}});
    let Case::Session(parsed) = parse(&value).expect("a settings object parses") else {
        panic!("a settings case parsed as a call case");
    };
    assert_eq!(
        parsed.settings,
        Some(
            json!({"hooks": {"hooks": {"probe": {"point": "session_start", "command": "probe"}}}})
        )
    );
}

#[test]
fn non_object_settings_names_the_field() {
    let mut value = session_case();
    value["settings"] = json!(false);
    let error = case_error(&value);
    assert!(error.contains("settings"), "{error}");
}

#[test]
fn a_call_case_with_settings_is_rejected() {
    let mut value = json!({
        "call": {"provider": "p", "function": "credential", "arg": {}},
        "host": {"http": []},
        "returns": {},
    });
    value["settings"] = json!({"hooks": {}});
    let error = case_error(&value);
    assert!(error.contains("call cases take"), "{error}");
}

fn command_case() -> Value {
    let mut value = session_case();
    value
        .as_object_mut()
        .expect("session case is an object")
        .remove("prompt");
    value["command"] = json!({"name": "export", "text": "out.md"});
    value["until"] = json!({"kind": "extension_ui"});
    value
}

fn command_input(case: &super::SessionCase) -> &super::CaseCommand {
    let super::Input::Command(command) = &case.input else {
        panic!("expected a command input");
    };
    command
}

#[test]
fn a_command_case_parses_its_name_and_text() {
    let Case::Session(parsed) = parse(&command_case()).expect("command case parses") else {
        panic!("command case parsed as a call case");
    };
    let command = command_input(&parsed);
    assert_eq!(command.name, "export");
    assert_eq!(command.text, "out.md");
}

#[test]
fn a_prompt_case_parses_to_a_prompt_input() {
    let Case::Session(parsed) = parse(&session_case()).expect("prompt case parses") else {
        panic!("prompt case parsed as a call case");
    };
    let super::Input::Prompt(prompt) = &parsed.input else {
        panic!("expected a prompt input");
    };
    assert_eq!(prompt, "hi");
}

#[test]
fn a_case_cannot_mix_a_prompt_and_a_command() {
    let mut value = session_case();
    value["command"] = json!({"name": "export"});
    value["until"] = json!({"kind": "extension_ui"});
    let error = case_error(&value);
    assert!(
        error.contains("prompt") && error.contains("command"),
        "{error}"
    );
}

#[test]
fn a_session_case_needs_a_prompt_or_a_command() {
    let mut value = session_case();
    value
        .as_object_mut()
        .expect("session case is an object")
        .remove("prompt");
    let error = case_error(&value);
    assert!(
        error.contains("prompt") && error.contains("command"),
        "{error}"
    );
}

#[test]
fn a_command_case_requires_until() {
    let mut value = command_case();
    value
        .as_object_mut()
        .expect("command case is an object")
        .remove("until");
    let error = case_error(&value);
    assert!(error.contains("until"), "{error}");

    let Case::Session(parsed) = parse(&command_case()).expect("extension_ui until parses") else {
        panic!("command case parsed as a call case");
    };
    assert_eq!(
        parsed.until.as_ref().map(|until| until.kind.as_str()),
        Some("extension_ui")
    );

    let mut value = command_case();
    value["until"] = json!({"kind": "notice", "nth": 2});
    let Case::Session(parsed) = parse(&value).expect("notice until parses") else {
        panic!("command case parsed as a call case");
    };
    assert_eq!(
        parsed
            .until
            .as_ref()
            .map(|until| (until.kind.as_str(), until.nth)),
        Some(("notice", 2))
    );
}

#[test]
fn a_prompt_case_keeps_its_default_until() {
    let Case::Session(parsed) = parse(&session_case()).expect("prompt case parses") else {
        panic!("prompt case parsed as a call case");
    };
    assert!(parsed.until.is_none());
}

#[test]
fn malformed_command_fields_name_command() {
    for (command, reason) in [
        (json!(false), "command"),
        (json!({"text": "out.md"}), "command.name"),
        (json!({"name": 7}), "command.name"),
        (json!({"name": "export", "text": 7}), "command.text"),
        (json!({"name": "export", "surprise": true}), "command"),
    ] {
        let mut value = command_case();
        value["command"] = command.clone();
        let error = case_error(&value);
        assert!(error.contains(reason), "{command}: {error}");
    }
}

#[test]
fn a_case_cannot_mix_a_command_and_a_call() {
    let mut value = command_case();
    value["call"] = json!({"provider": "p", "function": "cost", "arg": {}});
    value["returns"] = json!(0);
    let error = case_error(&value);
    assert!(
        error.contains("command") && error.contains("call"),
        "{error}"
    );
}
