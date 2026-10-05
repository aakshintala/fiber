//! The wire encoding and decoding, pinned byte for byte.

use serde_json::json;

use super::{
    Incoming, Outcome, decode_line, encode_error, encode_notification, encode_request,
    encode_result,
};

#[test]
fn a_request_encodes_with_its_id_method_and_params() {
    assert_eq!(
        encode_request(3, "tools/call", Some(&json!({"name": "echo"}))),
        r#"{"id":3,"jsonrpc":"2.0","method":"tools/call","params":{"name":"echo"}}"#,
    );
}

#[test]
fn absent_params_encode_as_an_empty_object() {
    assert_eq!(
        encode_request(2, "tools/list", None),
        r#"{"id":2,"jsonrpc":"2.0","method":"tools/list","params":{}}"#,
    );
}

#[test]
fn a_notification_without_params_carries_no_params_key() {
    assert_eq!(
        encode_notification("notifications/initialized", None),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );
}

#[test]
fn a_notification_with_params_carries_them() {
    assert_eq!(
        encode_notification("notifications/cancelled", Some(&json!({"requestId": 3}))),
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":3}}"#,
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
