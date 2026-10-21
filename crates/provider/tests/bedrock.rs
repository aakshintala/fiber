//! Claude on AWS Bedrock's Invoke API through the provider crate's public
//! API (`docs/model-routing.md`, "Protocols and providers"): the probe's
//! recorded event streams served by the fake provider server, the request
//! a model with `compat.bedrock_invoke` sends, and the failures Bedrock
//! reports in its headers and inside the stream
//! (`research/bedrock-invoke-probe`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

#[path = "support/harness.rs"]
mod harness;

use contract::ErrorCode;
use contract::events::ToolCallRequested;
use contract::provider::{CallError, Delta, Finish, ModelRequest, Provider, Reply, ReplyAction};
use contract::{GenerationId, ProviderCallId, ThinkingLevel};
use fakes::{ProviderServer, Response, fingerprint};
use provider::anthropic_messages::Messages;
use provider::{Compat, Endpoint};
use serde_json::{Map, Value, json};

use harness::{failed, run, sent_body};

const MODEL: &str = "global.anthropic.claude-haiku-4-5-20251001-v1:0";

const PLAIN: &[u8] = include_bytes!("../../../research/bedrock-invoke-probe/raw/plain.eventstream");
const TOOL: &[u8] = include_bytes!("../../../research/bedrock-invoke-probe/raw/tool.eventstream");
const THINKING: &[u8] =
    include_bytes!("../../../research/bedrock-invoke-probe/raw/thinking.eventstream");
const CACHE_WRITE: &[u8] =
    include_bytes!("../../../research/bedrock-invoke-probe/raw/cache-write.eventstream");

fn endpoint(server: &ProviderServer) -> Endpoint {
    Endpoint {
        model: MODEL.into(),
        base_url: server.url(),
        compat: Compat {
            bedrock_invoke: true,
            ..Compat::default()
        },
        ..harness::endpoint("bedrock", server)
    }
}

/// A 200 reply carrying `body` as Bedrock frames it.
fn stream(body: &[u8]) -> Response {
    Response {
        status: 200,
        headers: vec![(
            "content-type".to_owned(),
            "application/vnd.amazon.eventstream".to_owned(),
        )],
        body: body.to_vec(),
        drop_connection: false,
        stall: false,
    }
}

/// The reply `body` decodes into, with its deltas, through one call.
fn served(body: &[u8]) -> (Result<Reply, CallError>, Vec<Delta>) {
    let server = ProviderServer::start([stream(body)]).unwrap();
    run(Messages::new(endpoint(&server)).call(&harness::request()))
}

/// CRC-32/ISO-HDLC, bit by bit: the test's own, so a wrong table in the
/// crate cannot agree with it.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// One message framed as AWS frames it, its headers all strings.
fn frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    for (name, value) in headers {
        encoded.push(u8::try_from(name.len()).unwrap());
        encoded.extend_from_slice(name.as_bytes());
        encoded.push(7);
        encoded.extend_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
        encoded.extend_from_slice(value.as_bytes());
    }
    let total = u32::try_from(16 + encoded.len() + payload.len()).unwrap();
    let mut out = total.to_be_bytes().to_vec();
    out.extend_from_slice(&u32::try_from(encoded.len()).unwrap().to_be_bytes());
    let prelude = crc32(&out);
    out.extend_from_slice(&prelude.to_be_bytes());
    out.extend_from_slice(&encoded);
    out.extend_from_slice(payload);
    let message = crc32(&out);
    out.extend_from_slice(&message.to_be_bytes());
    out
}

/// One `chunk` event carrying `event`, as Bedrock wraps it.
fn chunk(event: &Value) -> Vec<u8> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.encode(event.to_string());
    frame(
        &[
            (":event-type", "chunk"),
            (":content-type", "application/json"),
            (":message-type", "event"),
        ],
        json!({"bytes": bytes, "p": "abcdefgh"})
            .to_string()
            .as_bytes(),
    )
}

/// The first `n` messages of a recorded stream.
fn first_messages(recorded: &[u8], n: usize) -> Vec<u8> {
    let mut at = 0;
    for _ in 0..n {
        let total = u32::from_be_bytes(recorded[at..at + 4].try_into().unwrap());
        at += usize::try_from(total).unwrap();
    }
    recorded[..at].to_vec()
}

/// The recording's decoded events, from the probe's record beside it.
fn recorded_events(record: &str) -> Vec<Value> {
    let record: Value = serde_json::from_str(record).unwrap();
    record["events"].as_array().unwrap().clone()
}

#[test]
fn the_recorded_plain_stream_decodes_into_its_text_and_usage() {
    let (reply, deltas) = served(PLAIN);
    let reply = reply.unwrap();
    assert_eq!(reply.finish, Finish::Completed);
    let [ReplyAction::Text(text)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(text.text, "Hello, how are you today?");
    let streamed: String = deltas
        .iter()
        .map(|delta| {
            let Delta::Text(d) = delta else {
                panic!("{delta:?}");
            };
            d.text.clone()
        })
        .collect();
    assert_eq!(streamed, text.text);
    assert_eq!(
        reply.generation_id,
        Some(GenerationId(
            "msg_bdrk_wo53fgumh24epnbvvdongfrupvcopsptfh6v6pmfag7knuem3k3a".into()
        ))
    );
    assert_eq!(reply.tokens.input, 13);
    assert_eq!(reply.tokens.output, 10);
    assert_eq!(reply.tokens.cache_read, 0);
    assert!(reply.tokens.cache_write.is_empty());
}

#[test]
fn the_recorded_tool_stream_decodes_into_both_calls_in_order() {
    let (reply, deltas) = served(TOOL);
    let reply = reply.unwrap();
    assert_eq!(reply.finish, Finish::Completed);
    let calls: Vec<&ToolCallRequested> = reply
        .actions
        .iter()
        .map(|action| {
            let ReplyAction::ToolCall(call) = action else {
                panic!("{action:?}");
            };
            call
        })
        .collect();
    assert_eq!(calls.len(), 2);
    for (call, (city, id)) in calls.iter().zip([
        ("Paris", "toolu_bdrk_016BjcwULmFAJLq2JVoUrN6F"),
        ("Rome", "toolu_bdrk_01UhLdMn8cSBDFYVMmRMZnky"),
    ]) {
        assert_eq!(call.name, "get_weather");
        assert_eq!(call.arguments, json!({"city": city}));
        assert_eq!(call.provider_id, Some(ProviderCallId(id.into())));
    }
    let indices: Vec<u32> = deltas
        .iter()
        .filter_map(|delta| {
            if let Delta::ToolCallArguments(d) = delta
                && !d.text.is_empty()
            {
                return Some(d.index);
            }
            None
        })
        .collect();
    assert_eq!(indices, [0, 0, 1, 1, 1, 1]);
    assert_eq!((reply.tokens.input, reply.tokens.output), (584, 90));
}

#[test]
fn the_recorded_thinking_stream_keeps_its_reasoning_and_signature() {
    let (reply, deltas) = served(THINKING);
    let reply = reply.unwrap();
    let events = recorded_events(include_str!(
        "../../../research/bedrock-invoke-probe/raw/thinking.json"
    ));
    let signature: String = events
        .iter()
        .filter_map(|event| event.pointer("/delta/signature")?.as_str())
        .collect();
    assert!(!signature.is_empty());
    let [ReplyAction::Reasoning(reasoning), ReplyAction::Text(text)] = reply.actions.as_slice()
    else {
        panic!("{:?}", reply.actions);
    };
    assert!(reasoning.text.ends_with("= 391"), "{}", reasoning.text);
    assert_eq!(
        reasoning.provider_item,
        Some(json!({"type": "thinking", "thinking": reasoning.text, "signature": signature}))
    );
    assert_eq!(text.text, "391");
    assert!(
        deltas
            .iter()
            .any(|delta| matches!(delta, Delta::Reasoning(_)))
    );
    assert_eq!((reply.tokens.input, reply.tokens.output), (50, 73));
}

#[test]
fn the_recorded_cache_write_reports_the_write_by_lifetime() {
    let (reply, _) = served(CACHE_WRITE);
    let tokens = reply.unwrap().tokens;
    assert_eq!(tokens.input, 9);
    assert_eq!(tokens.cache_read, 0);
    assert_eq!(
        tokens.cache_write.into_iter().collect::<Vec<_>>(),
        [("5m".to_owned(), 5874)]
    );
}

#[test]
fn a_request_goes_to_the_invoke_path_in_bedrocks_shape() {
    let server = ProviderServer::start([stream(PLAIN)]).unwrap();
    let mut extra_body = Map::new();
    extra_body.insert("anthropic_beta".into(), json!(["context-1m-2025-08-07"]));
    let endpoint = Endpoint {
        extra_body,
        max_output_tokens: Some(64_000),
        ..endpoint(&server)
    };
    run(Messages::new(endpoint).call(&harness::request()))
        .0
        .unwrap();
    let sent = &server.requests()[0];
    assert_eq!(sent.method, "POST");
    // The id's `:` is percent-encoded, as AWS's SDKs send it.
    assert_eq!(
        sent.path,
        "/model/global.anthropic.claude-haiku-4-5-20251001-v1%3A0/invoke-with-response-stream"
    );
    assert_eq!(
        sent.header("accept"),
        Some("application/vnd.amazon.eventstream")
    );
    assert_eq!(sent.header("content-type"), Some("application/json"));
    // A Bedrock API key is a bearer token; the version rides in the body.
    assert_eq!(
        sent.header("authorization"),
        Some(fingerprint("Bearer sk-secret").as_str())
    );
    assert_eq!(sent.header("x-api-key"), None);
    assert_eq!(sent.header("anthropic-version"), None);
    assert_eq!(sent.header("anthropic-beta"), None);
    let body = sent_body(&server, 0);
    assert_eq!(body["anthropic_version"], "bedrock-2023-05-31");
    assert_eq!(body.get("model"), None);
    assert_eq!(body.get("stream"), None);
    assert_eq!(body["anthropic_beta"], json!(["context-1m-2025-08-07"]));
    assert_eq!(body["max_tokens"], 64_000);
    // Prompt caching keeps its markers: Bedrock caches Claude's prompts.
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
    assert_eq!(
        body["messages"],
        json!([{"role": "user", "content": [
            {"type": "text", "text": "hi", "cache_control": {"type": "ephemeral", "ttl": "1h"}}
        ]}])
    );
}

#[test]
fn a_budget_thinking_level_rides_in_the_body_as_on_anthropic() {
    let server = ProviderServer::start([stream(THINKING)]).unwrap();
    let endpoint = Endpoint {
        compat: Compat {
            bedrock_invoke: true,
            thinking_budget: true,
            ..Compat::default()
        },
        max_output_tokens: Some(64_000),
        ..endpoint(&server)
    };
    let request = ModelRequest {
        thinking: Some(ThinkingLevel::Low),
        ..harness::request()
    };
    run(Messages::new(endpoint).call(&request)).0.unwrap();
    let body = sent_body(&server, 0);
    assert_eq!(
        body["thinking"],
        json!({"type": "enabled", "budget_tokens": 2048})
    );
    assert_eq!(body.get("output_config"), None);
}

#[test]
fn an_exception_inside_the_stream_fails_the_call_with_its_code() {
    for (exception, code) in [
        ("throttlingException", ErrorCode::RateLimited),
        ("modelStreamErrorException", ErrorCode::ProviderUnavailable),
        ("validationException", ErrorCode::InvalidRequest),
    ] {
        let mut body = first_messages(PLAIN, 3);
        body.extend(frame(
            &[
                (":exception-type", exception),
                (":content-type", "application/json"),
                (":message-type", "exception"),
            ],
            br#"{"message":"Bedrock said no."}"#,
        ));
        let server = ProviderServer::start([stream(&body)]).unwrap();
        let (failure, should_retry) =
            failed(Messages::new(endpoint(&server)).call(&harness::request()));
        assert_eq!(failure.code, code, "{exception}");
        assert_eq!(should_retry, None, "{exception}");
        let said = failure.provider.unwrap();
        assert_eq!(said.status, Some(200), "{exception}");
        assert_eq!(said.message, "Bedrock said no.", "{exception}");
    }
}

#[test]
fn chunks_framed_by_the_test_decode_as_the_recording_does() {
    // The crate's framing against the test's own encoder, CRCs included.
    let events = recorded_events(include_str!(
        "../../../research/bedrock-invoke-probe/raw/plain.json"
    ));
    let body: Vec<u8> = events.iter().flat_map(chunk).collect();
    let (rebuilt, _) = served(&body);
    let (recorded, _) = served(PLAIN);
    assert_eq!(rebuilt.unwrap(), recorded.unwrap());
}

#[test]
fn a_message_that_is_no_chunk_is_passed_over() {
    let mut body = first_messages(PLAIN, 2);
    body.extend(frame(
        &[
            (":event-type", "metrics"),
            (":content-type", "application/json"),
            (":message-type", "event"),
        ],
        br#"{"latency":1}"#,
    ));
    body.extend_from_slice(&PLAIN[first_messages(PLAIN, 2).len()..]);
    let (reply, _) = served(&body);
    let (recorded, _) = served(PLAIN);
    assert_eq!(reply.unwrap(), recorded.unwrap());
}

#[test]
fn a_chunk_that_cannot_be_read_fails_the_call_as_stream_incomplete() {
    let headers = [
        (":event-type", "chunk"),
        (":content-type", "application/json"),
        (":message-type", "event"),
    ];
    for (payload, said) in [
        (&b"not json"[..], "a chunk is not JSON"),
        (&br#"{"p":"abc"}"#[..], "a chunk holds no `bytes`"),
        (
            &br#"{"bytes":"!!!"}"#[..],
            "a chunk holds `bytes` that are not base64",
        ),
        (&br#"{"bytes":"bm90IGpzb24="}"#[..], "an event is not JSON"),
    ] {
        let mut body = first_messages(PLAIN, 1);
        body.extend(frame(&headers, payload));
        let server = ProviderServer::start([stream(&body)]).unwrap();
        let (failure, _) = failed(Messages::new(endpoint(&server)).call(&harness::request()));
        assert_eq!(failure.code, ErrorCode::StreamIncomplete, "{said}");
        assert!(failure.message.contains(said), "{}", failure.message);
    }
}

#[test]
fn a_frame_that_fails_its_checksum_fails_the_call_as_stream_incomplete() {
    let mut body = PLAIN.to_vec();
    // A byte of the second message's payload.
    let first = usize::try_from(u32::from_be_bytes(body[0..4].try_into().unwrap())).unwrap();
    body[first + 100] ^= 0x01;
    let server = ProviderServer::start([stream(&body)]).unwrap();
    let (failure, _) = failed(Messages::new(endpoint(&server)).call(&harness::request()));
    assert_eq!(failure.code, ErrorCode::StreamIncomplete);
    assert!(failure.message.contains("checksum"), "{}", failure.message);
}

#[test]
fn a_stream_that_ends_before_message_stop_is_incomplete() {
    let body = first_messages(PLAIN, 4);
    let server = ProviderServer::start([stream(&body)]).unwrap();
    let (failure, _) = failed(Messages::new(endpoint(&server)).call(&harness::request()));
    assert_eq!(failure.code, ErrorCode::StreamIncomplete);
    assert!(
        failure.message.contains("message_stop"),
        "{}",
        failure.message
    );
}

/// The recorded error reply `record` holds, served as it was answered.
fn answered(record: &str) -> Response {
    let record: Value = serde_json::from_str(record).unwrap();
    let mut response = Response::status(
        u16::try_from(record["status"].as_u64().unwrap()).unwrap(),
        record["body"].as_str().unwrap(),
    );
    for (name, value) in record["headers"].as_object().unwrap() {
        if !name.eq_ignore_ascii_case("content-type") {
            response = response.header(name, value.as_str().unwrap());
        }
    }
    response
}

#[test]
fn a_recorded_error_reply_is_classified_by_its_x_amzn_errortype() {
    for (record, code, said) in [
        (
            include_str!("../../../research/bedrock-invoke-probe/raw/bad-model.json"),
            ErrorCode::ModelNotFound,
            "The provided model identifier is invalid.",
        ),
        (
            include_str!("../../../research/bedrock-invoke-probe/raw/bad-key-id.json"),
            ErrorCode::AuthenticationFailed,
            "The security token included in the request is invalid.",
        ),
        (
            include_str!("../../../research/bedrock-invoke-probe/raw/bad-bearer.json"),
            ErrorCode::AuthenticationFailed,
            "Invalid API Key format: Delimiter ':' not found",
        ),
        (
            include_str!("../../../research/bedrock-invoke-probe/raw/validation.json"),
            ErrorCode::InvalidRequest,
            "stream cannot be true when max_tokens is 0",
        ),
    ] {
        let server = ProviderServer::start([answered(record)]).unwrap();
        let (failure, _) = failed(Messages::new(endpoint(&server)).call(&harness::request()));
        assert_eq!(failure.code, code, "{said}");
        assert_eq!(failure.provider.unwrap().message, said);
    }
}

#[test]
fn a_throttled_reply_is_rate_limited_and_a_quota_reply_keeps_its_status_code() {
    let throttled = Response::status(
        429,
        r#"{"message":"Too many requests, please wait before trying again."}"#,
    )
    .header(
        "x-amzn-ErrorType",
        "ThrottlingException:http://internal.amazon.com/coral/com.amazon.bedrock/",
    );
    let server = ProviderServer::start([throttled]).unwrap();
    let (failure, _) = failed(Messages::new(endpoint(&server)).call(&harness::request()));
    assert_eq!(failure.code, ErrorCode::RateLimited);
    let quota = Response::status(
        400,
        r#"{"message":"Your request exceeds the service quota for your account."}"#,
    )
    .header(
        "x-amzn-ErrorType",
        "ServiceQuotaExceededException:http://internal.amazon.com/coral/com.amazon.bedrock/",
    );
    let server = ProviderServer::start([quota]).unwrap();
    let (failure, _) = failed(Messages::new(endpoint(&server)).call(&harness::request()));
    assert_eq!(failure.code, ErrorCode::InvalidRequest);
}
