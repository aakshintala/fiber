//! The process extension wire (`docs/extensions.md`, "Process extensions"):
//! driver `call` and `ready` commands, typed process `reply` answers, the
//! call's `command_accepted` wrapper, and the two ephemeral event kinds.
//! Arbitrary JSON positions keep nested null and tell `{}` from `[]`; a
//! present JSON null is a result, never an absent answer.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use contract::commands::{Command, CommandLine, ReplyAnswer};
use contract::events::{CommandResult, Event, ExtensionReason};
use contract::{CommandId, Envelope, RequestId, SessionId};

fn parse(line: &str) -> Result<CommandLine, serde_json::Error> {
    serde_json::from_str(line)
}

fn envelope(kind: &str, payload: serde_json::Value) -> Envelope {
    Envelope {
        kind: kind.into(),
        session_id: SessionId("s".into()),
        ts: 1,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::from_value(payload).unwrap(),
    }
}

#[test]
fn a_call_names_its_request_and_keeps_nested_json_exact() {
    let line = parse(
        r#"{"id":"c_reg","command":"call","args":{"call":"fiber.tool","args":["echo",{"object":{},"list":[],"nothing":null}],"request_id":"r_A"}}"#,
    )
    .unwrap();
    assert_eq!(line.id, CommandId("c_reg".into()));
    let Command::Call(ref call) = line.command else {
        panic!("not a call");
    };
    assert_eq!(call.call, "fiber.tool");
    assert_eq!(
        call.args,
        vec![
            serde_json::Value::String("echo".into()),
            serde_json::json!({"object": {}, "list": [], "nothing": null}),
        ]
    );
    assert_eq!(call.request_id, Some(RequestId("r_A".into())));
    assert_eq!(
        serde_json::to_value(&line).unwrap(),
        serde_json::json!({"id": "c_reg", "command": "call", "args": {"call": "fiber.tool", "args": ["echo", {"object": {}, "list": [], "nothing": null}], "request_id": "r_A"}}),
    );
}

#[test]
fn a_call_before_ready_names_no_request() {
    let line =
        parse(r#"{"id":"c_reg","command":"call","args":{"call":"host.sha256","args":["YWJj"]}}"#)
            .unwrap();
    let Command::Call(ref call) = line.command else {
        panic!("not a call");
    };
    assert_eq!(call.request_id, None);
    assert_eq!(
        serde_json::to_value(&line).unwrap(),
        serde_json::json!({"id": "c_reg", "command": "call", "args": {"call": "host.sha256", "args": ["YWJj"]}}),
    );
}

#[test]
fn ready_takes_no_args() {
    let line = parse(r#"{"id":"c_ready","command":"ready"}"#).unwrap();
    assert_eq!(line.command, Command::Ready);
    assert_eq!(
        serde_json::to_value(&line).unwrap(),
        serde_json::json!({"id": "c_ready", "command": "ready"}),
    );
}

#[test]
fn a_call_refuses_an_unknown_key_and_a_null_control_key() {
    assert!(
        parse(r#"{"id":"c","command":"call","args":{"call":"fiber.tool","args":[],"extra":true}}"#)
            .is_err()
    );
    assert!(parse(r#"{"id":"c","command":"call","args":{"call":null,"args":[]}}"#).is_err());
    assert!(
        parse(r#"{"id":"c","command":"call","args":{"call":"fiber.tool","args":null}}"#).is_err()
    );
}

#[test]
fn a_process_reply_carries_a_result_of_any_json() {
    let line = parse(
        r#"{"id":"c_answer","command":"reply","args":{"request_id":"r_A","result":{"object":{},"list":[],"nothing":null}}}"#,
    )
    .unwrap();
    let Command::Reply(reply) = line.command else {
        panic!("not a reply");
    };
    assert_eq!(reply.request_id, RequestId("r_A".into()));
    let ReplyAnswer::Value { result } = reply.answer else {
        panic!("not a value answer: {:?}", reply.answer);
    };
    assert_eq!(
        result,
        serde_json::json!({"object": {}, "list": [], "nothing": null}),
    );
}

#[test]
fn a_present_json_null_is_a_result_not_an_absent_answer() {
    let line =
        parse(r#"{"id":"c","command":"reply","args":{"request_id":"r","result":null}}"#).unwrap();
    let Command::Reply(reply) = line.command else {
        panic!("not a reply");
    };
    assert_eq!(
        reply.answer,
        ReplyAnswer::Value {
            result: serde_json::Value::Null
        },
    );
}

#[test]
fn a_process_reply_carries_a_typed_error() {
    let line = parse(
        r#"{"id":"c_error","command":"reply","args":{"request_id":"r_B","error":{"code":"tool_error","message":"example failure"}}}"#,
    )
    .unwrap();
    let Command::Reply(reply) = line.command else {
        panic!("not a reply");
    };
    let ReplyAnswer::Failure { error } = reply.answer else {
        panic!("not a failure answer");
    };
    assert_eq!(error.code, contract::ErrorCode::ToolError);
    assert_eq!(error.message, "example failure");
}

#[test]
fn a_process_reply_needs_exactly_one_of_result_and_error() {
    assert!(parse(r#"{"id":"c","command":"reply","args":{"request_id":"r"}}"#).is_err());
    assert!(
        parse(
            r#"{"id":"c","command":"reply","args":{"request_id":"r","result":1,"error":{"code":"tool_error","message":"m"}}}"#
        )
        .is_err()
    );
    assert!(
        parse(r#"{"id":"c","command":"reply","args":{"request_id":"r","result":1,"decision":"allow"}}"#)
            .is_err()
    );
    assert!(
        parse(
            r#"{"id":"c","command":"reply","args":{"request_id":"r","error":{"code":"tool_error"}}}"#
        )
        .is_err()
    );
}

#[test]
fn an_ordinary_declined_reply_still_reads() {
    let line =
        parse(r#"{"id":"c","command":"reply","args":{"request_id":"r","declined":true}}"#).unwrap();
    let Command::Reply(reply) = line.command else {
        panic!("not a reply");
    };
    assert!(matches!(reply.answer, ReplyAnswer::Declined { .. }));
}

#[test]
fn a_call_answer_wraps_the_return_with_presence() {
    let absent: CommandResult = serde_json::from_value(serde_json::json!({})).unwrap();
    let CommandResult::Call(empty) = &absent else {
        panic!("not a call result");
    };
    assert_eq!(empty.value, None);
    let present: CommandResult =
        serde_json::from_value(serde_json::json!({"value": {"content": "ok"}})).unwrap();
    let CommandResult::Call(answer) = &present else {
        panic!("not a call result");
    };
    assert_eq!(answer.value, Some(serde_json::json!({"content": "ok"})));
    let null: CommandResult = serde_json::from_value(serde_json::json!({"value": null})).unwrap();
    let CommandResult::Call(nothing) = &null else {
        panic!("not a call result");
    };
    assert_eq!(nothing.value, Some(serde_json::Value::Null));
    assert_eq!(
        serde_json::to_value(&null).unwrap(),
        serde_json::json!({"value": null})
    );
}

#[test]
fn a_call_answer_refuses_unknown_objects() {
    assert!(serde_json::from_value::<CommandResult>(serde_json::json!({"bogus": 1})).is_err());
    assert!(
        serde_json::from_value::<CommandResult>(serde_json::json!({"value": 1, "extra": 2}))
            .is_err()
    );
}

#[test]
fn other_call_answers_still_read_as_their_own_shape() {
    let read: CommandResult = serde_json::from_value(serde_json::json!({"last_seq": 41})).unwrap();
    assert!(matches!(read, CommandResult::Subscribe { .. }));
}

#[test]
fn hello_and_request_read_as_ephemeral_process_kinds() {
    for (reason, wire) in [
        (ExtensionReason::New, "new"),
        (ExtensionReason::Resume, "resume"),
        (ExtensionReason::Fork, "fork"),
        (ExtensionReason::Rewind, "rewind"),
        (ExtensionReason::Handoff, "handoff"),
    ] {
        assert_eq!(serde_json::to_value(reason).unwrap(), wire);
        let event = Event::from_envelope(&envelope(
            "extension_hello",
            serde_json::json!({"session_id": "s", "reason": wire, "workspace": "/w", "data_dirs": {"machine": "/m", "project": "/p"}, "state": {}}),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(event.kind(), "extension_hello");
        assert_eq!(event.class(), contract::events::Class::Ephemeral);
    }
    assert!(Event::from_envelope(&envelope(
        "extension_hello",
        serde_json::json!({"session_id": "s", "reason": "restart", "workspace": "/w", "data_dirs": {"machine": "/m", "project": "/p"}, "state": {}}),
    ))
    .is_err());
    let event = Event::from_envelope(&envelope(
        "extension_request",
        serde_json::json!({"request_id": "r_A", "registration": "c_reg", "key": "run", "args": []}),
    ))
    .unwrap()
    .unwrap();
    assert_eq!(event.kind(), "extension_request");
    assert_eq!(event.class(), contract::events::Class::Ephemeral);
    assert!(
        Event::from_envelope(&envelope(
            "extension_request",
            serde_json::json!({"request_id": "r_A", "registration": "c_reg", "key": "run"}),
        ))
        .is_err()
    );
}
