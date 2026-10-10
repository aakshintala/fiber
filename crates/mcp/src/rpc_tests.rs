//! The wire encoding and decoding, pinned byte for byte.

use serde_json::json;

use super::{
    Incoming, Outcome, decode_line, encode_error, encode_notification, encode_request,
    encode_result,
};

#[test]
fn a_call_line_keeps_the_top_level_name_last_when_arguments_hold_a_name() {
    assert_eq!(
        encode_request(
            3,
            "tools/call",
            &json!({"name": "echo", "arguments": {"name": "inner"}}),
        ),
        r#"{"id":3,"jsonrpc":"2.0","method":"tools/call","params":{"arguments":{"name":"inner"},"name":"echo"}}"#,
    );
}

#[test]
fn a_request_and_a_notification_round_trip_through_decode() {
    assert_eq!(
        decode_line(&encode_request(3, "tools/call", &json!({"name": "echo"}))),
        Incoming::ServerRequest(super::ServerRequest {
            id: json!(3),
            method: "tools/call".to_owned(),
        }),
    );
    assert_eq!(
        decode_line(&encode_notification(
            "notifications/cancelled",
            &json!({"requestId": 3})
        )),
        Incoming::Ignored,
    );
}

#[test]
fn a_result_decodes_with_its_id() {
    assert_eq!(
        decode_line(r#"{"jsonrpc":"2.0","id":3,"result":{"tools":[]}}"#),
        Incoming::Response(super::Response {
            id: 3,
            outcome: Outcome::Result(json!({"tools": []})),
        }),
    );
}

#[test]
fn an_error_decodes_with_its_code_and_message() {
    assert_eq!(
        decode_line(r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"Unknown tool"}}"#),
        Incoming::Response(super::Response {
            id: 3,
            outcome: Outcome::Error {
                code: -32602,
                message: "Unknown tool".to_owned(),
            },
        }),
    );
}

#[test]
fn a_server_ping_decodes_as_a_request() {
    assert_eq!(
        decode_line(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#),
        Incoming::ServerRequest(super::ServerRequest {
            id: json!(7),
            method: "ping".to_owned(),
        }),
    );
}

#[test]
fn a_server_ping_with_a_string_id_decodes_as_a_request() {
    assert_eq!(
        decode_line(r#"{"jsonrpc":"2.0","id":"probe","method":"ping"}"#),
        Incoming::ServerRequest(super::ServerRequest {
            id: json!("probe"),
            method: "ping".to_owned(),
        }),
    );
}

#[test]
fn a_server_notification_is_ignored() {
    assert_eq!(
        decode_line(r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#),
        Incoming::Ignored,
    );
}

#[test]
fn garbage_is_ignored() {
    for line in [
        "not json at all",
        "[1, 2, 3]",
        "42",
        r#"{"id": 3}"#,
        r#"{"jsonrpc":"2.0","id":3}"#,
    ] {
        assert_eq!(decode_line(line), Incoming::Ignored, "line: {line}");
    }
}

#[test]
fn answers_echo_a_numeric_id() {
    let id = json!(9);
    assert_eq!(
        encode_result(&id, &json!({})),
        r#"{"id":9,"jsonrpc":"2.0","result":{}}"#,
    );
    assert_eq!(
        encode_error(&id, -32601, "Method not found"),
        r#"{"error":{"code":-32601,"message":"Method not found"},"id":9,"jsonrpc":"2.0"}"#,
    );
}

#[test]
fn answers_echo_a_string_id_unchanged() {
    let id = json!("probe");
    assert_eq!(
        encode_result(&id, &json!({})),
        r#"{"id":"probe","jsonrpc":"2.0","result":{}}"#,
    );
    assert_eq!(
        encode_error(&id, -32601, "Method not found"),
        r#"{"error":{"code":-32601,"message":"Method not found"},"id":"probe","jsonrpc":"2.0"}"#,
    );
}

#[test]
fn a_server_request_with_a_negative_or_fractional_id_is_ignored() {
    // The `as_u64` guard admits only non-negative integers: `true` in its
    // place would answer these as server requests.
    for line in [
        r#"{"jsonrpc":"2.0","id":-1,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":3.5,"method":"ping"}"#,
    ] {
        assert_eq!(decode_line(line), Incoming::Ignored, "line: {line}");
    }
}
