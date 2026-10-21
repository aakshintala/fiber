use std::io::{BufRead, BufReader};

use contract::ErrorCode;
use contract::provider::{CallUsage, Delta, Finish, Reply, ReplyAction};
use serde_json::json;

use super::decode_tracked;
use crate::Error;
use crate::event_stream::tests::{Trickle, Value, frame};

const PLAIN: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/plain.eventstream");
const TOOL: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/tool-ministral.eventstream");
const SIGNED: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/signature.eventstream");
const REDACTED: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/redacted-first.eventstream");
const CACHE_WRITE: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/cache-write.eventstream");

type Decoded = (Result<Reply, (Error, Option<CallUsage>)>, Vec<Delta>);

fn decoded(stream: impl BufRead) -> Decoded {
    let mut deltas = Vec::new();
    let reply = decode_tracked(stream, &mut |delta| deltas.push(delta));
    (reply, deltas)
}

/// The byte offset at which each message of `recorded` ends.
fn ends(recorded: &[u8]) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut at = 0;
    while at < recorded.len() {
        let total = u32::from_be_bytes(recorded[at..at + 4].try_into().unwrap());
        at += usize::try_from(total).unwrap();
        ends.push(at);
    }
    ends
}

/// One event framed as ConverseStream frames it.
fn event(kind: &str, payload: &serde_json::Value) -> Vec<u8> {
    frame(
        &[
            (":event-type", Value::Str(kind)),
            (":content-type", Value::Str("application/json")),
            (":message-type", Value::Str("event")),
        ],
        payload.to_string().as_bytes(),
    )
}

/// A one-text reply that stops with `reason`, then its `metadata`.
fn stopping_with(reason: &str) -> Vec<u8> {
    [
        event("messageStart", &json!({"role": "assistant"})),
        event(
            "contentBlockDelta",
            &json!({"contentBlockIndex": 0, "delta": {"text": "hi"}}),
        ),
        event("contentBlockStop", &json!({"contentBlockIndex": 0})),
        event("messageStop", &json!({"stopReason": reason})),
        event(
            "metadata",
            &json!({"usage": {"inputTokens": 3, "outputTokens": 1}}),
        ),
    ]
    .concat()
}

#[test]
fn every_recording_decodes_the_same_however_its_bytes_are_split() {
    for recording in [PLAIN, TOOL, SIGNED, REDACTED, CACHE_WRITE] {
        let (whole, whole_deltas) = decoded(recording);
        let whole = whole.unwrap();
        assert!(!whole.actions.is_empty());
        for step in [1, 3, 7, 64] {
            let (split, deltas) = decoded(BufReader::new(Trickle {
                bytes: recording.to_vec(),
                at: 0,
                step,
            }));
            assert_eq!(split.unwrap(), whole, "step {step}");
            assert_eq!(deltas, whole_deltas, "step {step}");
        }
    }
}

#[test]
fn a_recording_cut_before_its_metadata_fails_as_stream_incomplete() {
    for recording in [PLAIN, TOOL, SIGNED] {
        let ends = ends(recording);
        assert_eq!(ends.last(), Some(&recording.len()));
        // After every whole message but the last, then inside the last one.
        let cuts = ends[..ends.len() - 1]
            .iter()
            .copied()
            .chain([recording.len() - 3]);
        for cut in cuts {
            let (reply, _) = decoded(&recording[..cut]);
            let (error, partial) = reply.unwrap_err();
            assert_eq!(error.code(), ErrorCode::StreamIncomplete, "cut at {cut}");
            assert_eq!(
                error,
                Error::StreamIncomplete("it ended before metadata".into()),
                "cut at {cut}"
            );
            assert_eq!(partial.unwrap().generation_id, None, "cut at {cut}");
        }
    }
}

#[test]
fn every_documented_stop_reason_finishes_or_fails_with_its_code() {
    for (reason, finish) in [
        ("end_turn", Finish::Completed),
        ("tool_use", Finish::Completed),
        ("stop_sequence", Finish::Completed),
        ("max_tokens", Finish::OutputLimit),
    ] {
        let (reply, _) = decoded(&stopping_with(reason)[..]);
        let reply = reply.unwrap();
        assert_eq!(reply.finish, finish, "{reason}");
        assert_eq!(reply.text(), "hi", "{reason}");
    }
    for (reason, code) in [
        ("guardrail_intervened", ErrorCode::Refused),
        ("content_filtered", ErrorCode::Refused),
        ("model_context_window_exceeded", ErrorCode::ContextOverflow),
        ("malformed_model_output", ErrorCode::StreamIncomplete),
        ("malformed_tool_use", ErrorCode::StreamIncomplete),
        ("a_reason_aws_adds_later", ErrorCode::UnknownStopReason),
    ] {
        let (reply, _) = decoded(&stopping_with(reason)[..]);
        let (error, _) = reply.unwrap_err();
        assert_eq!(error.code(), code, "{reason}");
    }
}

#[test]
fn metadata_before_message_stop_fails_as_stream_incomplete() {
    let stream = [
        event(
            "contentBlockDelta",
            &json!({"contentBlockIndex": 0, "delta": {"text": "hi"}}),
        ),
        event("metadata", &json!({"usage": {"inputTokens": 3}})),
    ]
    .concat();
    let (reply, _) = decoded(&stream[..]);
    let (error, _) = reply.unwrap_err();
    assert_eq!(
        error,
        Error::StreamIncomplete("metadata arrived before messageStop".into())
    );
}

#[test]
fn an_exception_inside_the_stream_fails_with_the_code_its_name_decides() {
    // The five exceptions ConverseStream documents inside its stream.
    for (exception, code) in [
        ("internalServerException", ErrorCode::ProviderUnavailable),
        ("modelStreamErrorException", ErrorCode::ProviderUnavailable),
        (
            "serviceUnavailableException",
            ErrorCode::ProviderUnavailable,
        ),
        ("throttlingException", ErrorCode::RateLimited),
        ("validationException", ErrorCode::InvalidRequest),
    ] {
        let mut stream = PLAIN[..ends(PLAIN)[2]].to_vec();
        stream.extend(frame(
            &[
                (":exception-type", Value::Str(exception)),
                (":content-type", Value::Str("application/json")),
                (":message-type", Value::Str("exception")),
            ],
            br#"{"message":"Bedrock said no."}"#,
        ));
        let (reply, _) = decoded(&stream[..]);
        let (error, partial) = reply.unwrap_err();
        assert_eq!(error.code(), code, "{exception}");
        assert_eq!(
            error,
            Error::ReplyFailed {
                code: Some(exception.into()),
                message: "Bedrock said no.".into(),
            }
        );
        assert!(partial.is_some(), "{exception}");
    }
}

#[test]
fn an_event_that_is_not_json_fails_as_stream_incomplete() {
    let mut stream = PLAIN[..ends(PLAIN)[0]].to_vec();
    stream.extend(frame(
        &[
            (":event-type", Value::Str("contentBlockDelta")),
            (":message-type", Value::Str("event")),
        ],
        b"not json",
    ));
    let (reply, _) = decoded(&stream[..]);
    let (error, _) = reply.unwrap_err();
    assert_eq!(error.code(), ErrorCode::StreamIncomplete);
    assert!(
        error
            .to_string()
            .contains("a `contentBlockDelta` event is not JSON"),
        "{error}"
    );
}

#[test]
fn blocks_open_at_once_are_kept_apart_by_their_index() {
    let stream = [
        event("messageStart", &json!({"role": "assistant"})),
        event(
            "contentBlockStart",
            &json!({"contentBlockIndex": 1, "start": {"toolUse": {"toolUseId": "tooluse_a", "name": "read"}}}),
        ),
        event(
            "contentBlockStart",
            &json!({"contentBlockIndex": 2, "start": {"toolUse": {"toolUseId": "tooluse_b", "name": "grep"}}}),
        ),
        event(
            "contentBlockDelta",
            &json!({"contentBlockIndex": 1, "delta": {"toolUse": {"input": "{\"path\":\"a.md\"}"}}}),
        ),
        event(
            "contentBlockDelta",
            &json!({"contentBlockIndex": 2, "delta": {"toolUse": {"input": "{\"pattern\":\"x\"}"}}}),
        ),
        event("contentBlockStop", &json!({"contentBlockIndex": 1})),
        event("contentBlockStop", &json!({"contentBlockIndex": 2})),
        event("messageStop", &json!({"stopReason": "tool_use"})),
        event("metadata", &json!({"usage": {}})),
    ]
    .concat();
    let (reply, deltas) = decoded(&stream[..]);
    let reply = reply.unwrap();
    let calls: Vec<(&str, serde_json::Value)> = reply
        .actions
        .iter()
        .map(|action| match action {
            ReplyAction::ToolCall(call) => (call.name.as_str(), call.arguments.clone()),
            ReplyAction::Text(_)
            | ReplyAction::Reasoning(_)
            | ReplyAction::Hosted(_)
            | ReplyAction::HostedResult { .. } => panic!("{action:?}"),
        })
        .collect();
    assert_eq!(
        calls,
        [
            ("read", json!({"path": "a.md"})),
            ("grep", json!({"pattern": "x"}))
        ]
    );
    let positions: Vec<u32> = deltas
        .iter()
        .map(|delta| match delta {
            Delta::ToolCallArguments(d) => d.index,
            Delta::Text(_) | Delta::Reasoning(_) => panic!("{delta:?}"),
        })
        .collect();
    assert_eq!(positions, [0, 1]);
}

#[test]
fn a_tool_calls_input_in_several_deltas_joins_in_order_under_its_position() {
    let stream = [
        event("messageStart", &json!({"role": "assistant"})),
        event(
            "contentBlockStart",
            &json!({"contentBlockIndex": 1, "start": {"toolUse": {"toolUseId": "tooluse_a", "name": "read"}}}),
        ),
        event(
            "contentBlockDelta",
            &json!({"contentBlockIndex": 1, "delta": {"toolUse": {"input": "{\"path\":"}}}),
        ),
        event(
            "contentBlockDelta",
            &json!({"contentBlockIndex": 1, "delta": {"toolUse": {"input": "\"a.md\"}"}}}),
        ),
        event("contentBlockStop", &json!({"contentBlockIndex": 1})),
        event("messageStop", &json!({"stopReason": "tool_use"})),
        event("metadata", &json!({"usage": {}})),
    ]
    .concat();
    let (reply, deltas) = decoded(&stream[..]);
    let reply = reply.unwrap();
    let [ReplyAction::ToolCall(call)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(call.name, "read");
    assert_eq!(call.arguments, json!({"path": "a.md"}));
    let pieces: Vec<(u32, String)> = deltas
        .iter()
        .map(|delta| match delta {
            Delta::ToolCallArguments(d) => (d.index, d.text.clone()),
            Delta::Text(_) | Delta::Reasoning(_) => panic!("{delta:?}"),
        })
        .collect();
    assert_eq!(
        pieces,
        [(0, "{\"path\":".to_owned()), (0, "\"a.md\"}".to_owned())]
    );
}
