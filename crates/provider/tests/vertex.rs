//! Google Vertex through the provider crate's public API
//! (`docs/model-routing.md`, "Google Vertex wire facts"): Claude on
//! `anthropic-messages` with `compat.vertex_raw_predict`, Gemini on
//! `google-generative-ai` at a Vertex base URL, each signed with a bearer
//! token as the `vertex` package's `credential()` gives one, and the probe's
//! recorded streams and error bodies (`research/vertex-probe`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

#[path = "support/harness.rs"]
mod harness;

use std::sync::Arc;

use contract::provider::{Delta, Finish, Provider, Reply, ReplyAction};
use contract::shapes::Tokens;
use contract::signing::{SignRequest, Signer};
use contract::{ErrorCode, ProviderCallId};
use fakes::{ProviderServer, Response, fingerprint};
use provider::anthropic_messages::Messages;
use provider::google_generative_ai::{Gemini, decode};
use provider::{Compat, Endpoint};
use serde_json::Value;

use harness::{anthropic_completed, failed, request, run, sent_body};

/// The path every Vertex request starts with, at the `global` location.
const PROJECT_PATH: &str = "/v1/projects/example-project/locations/global/publishers";

/// Adds the bearer token, as the `vertex` package's `credential()` has
/// Fiber do for every request (`docs/model-routing.md`, "Keys, tokens and
/// OAuth").
struct Bearer;

impl Signer for Bearer {
    fn sign(&self, _: &SignRequest<'_>) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Ok(vec![(
            "authorization".to_owned(),
            "Bearer vertex-test-token".to_owned(),
        )])
    }
}

/// Every stream the probe recorded whole, compiled in (`docs/ci.md`,
/// "Selection"): all but `gemini-search-cut.sse`, which Vertex ended early.
const STREAMS: &[(&str, &[u8])] = &[
    (
        "gemini-imgparts-call.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-imgparts-call.sse"),
    ),
    (
        "gemini-imgparts.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-imgparts.sse"),
    ),
    (
        "gemini-models-gemini-3-flash-preview.sse",
        include_bytes!(
            "../../../research/vertex-probe/raw/gemini-models-gemini-3-flash-preview.sse"
        ),
    ),
    (
        "gemini-models-gemini-3.1-flash-lite.sse",
        include_bytes!(
            "../../../research/vertex-probe/raw/gemini-models-gemini-3.1-flash-lite.sse"
        ),
    ),
    (
        "gemini-models-gemini-3.1-pro-preview-customtools.sse",
        include_bytes!(
            "../../../research/vertex-probe/raw/gemini-models-gemini-3.1-pro-preview-customtools.sse"
        ),
    ),
    (
        "gemini-models-gemini-3.1-pro-preview.sse",
        include_bytes!(
            "../../../research/vertex-probe/raw/gemini-models-gemini-3.1-pro-preview.sse"
        ),
    ),
    (
        "gemini-models-gemini-3.5-flash-lite.sse",
        include_bytes!(
            "../../../research/vertex-probe/raw/gemini-models-gemini-3.5-flash-lite.sse"
        ),
    ),
    (
        "gemini-models-gemini-3.5-flash.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-models-gemini-3.5-flash.sse"),
    ),
    (
        "gemini-models-gemini-3.7-flash.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-models-gemini-3.7-flash.sse"),
    ),
    (
        "gemini-models-gemini-3.8-flash.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-models-gemini-3.8-flash.sse"),
    ),
    (
        "gemini-models-gemini-4-argon.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-models-gemini-4-argon.sse"),
    ),
    (
        "gemini-replay-id-long65.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-replay-id-long65.sse"),
    ),
    (
        "gemini-replay-id-mismatch.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-replay-id-mismatch.sse"),
    ),
    (
        "gemini-replay-id-odd.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-replay-id-odd.sse"),
    ),
    (
        "gemini-replay-signed.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-replay-signed.sse"),
    ),
    (
        "gemini-search-no-flag.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-search-no-flag.sse"),
    ),
    (
        "gemini-search.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-search.sse"),
    ),
    (
        "gemini-sysrole-model.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-sysrole-model.sse"),
    ),
    (
        "gemini-sysrole-none.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-sysrole-none.sse"),
    ),
    (
        "gemini-sysrole-user.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-sysrole-user.sse"),
    ),
    (
        "gemini-thinking-replay.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-thinking-replay.sse"),
    ),
    (
        "gemini-thinking.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-thinking.sse"),
    ),
    (
        "gemini-tool.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini-tool.sse"),
    ),
    (
        "gemini2-image-user-part.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini2-image-user-part.sse"),
    ),
    (
        "gemini2-imgparts-call.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini2-imgparts-call.sse"),
    ),
    (
        "gemini2-imgparts.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini2-imgparts.sse"),
    ),
    (
        "gemini2-regional.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini2-regional.sse"),
    ),
    (
        "gemini2-replay-id.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini2-replay-id.sse"),
    ),
    (
        "gemini2-tool.sse",
        include_bytes!("../../../research/vertex-probe/raw/gemini2-tool.sse"),
    ),
];

/// The failures these tests serve, as the probe saved them, compiled in.
const RECORDS: &[(&str, &str)] = &[
    (
        "claude-bad-location.json",
        include_str!("../../../research/vertex-probe/raw/claude-bad-location.json"),
    ),
    (
        "claude-bad-token.json",
        include_str!("../../../research/vertex-probe/raw/claude-bad-token.json"),
    ),
    (
        "claude-data-sharing.json",
        include_str!("../../../research/vertex-probe/raw/claude-data-sharing.json"),
    ),
    (
        "claude-unknown-model.json",
        include_str!("../../../research/vertex-probe/raw/claude-unknown-model.json"),
    ),
    (
        "gemini-429.json",
        include_str!("../../../research/vertex-probe/raw/gemini-429.json"),
    ),
    (
        "gemini-api-key-header.json",
        include_str!("../../../research/vertex-probe/raw/gemini-api-key-header.json"),
    ),
    (
        "gemini-bad-request.json",
        include_str!("../../../research/vertex-probe/raw/gemini-bad-request.json"),
    ),
    (
        "gemini-expired-token.json",
        include_str!("../../../research/vertex-probe/raw/gemini-expired-token.json"),
    ),
    (
        "gemini-unknown-model.json",
        include_str!("../../../research/vertex-probe/raw/gemini-unknown-model.json"),
    ),
];

/// One recorded stream's bytes.
fn stream(name: &str) -> &'static [u8] {
    STREAMS
        .iter()
        .find(|(file, _)| *file == name)
        .map(|(_, bytes)| *bytes)
        .unwrap()
}

/// One recorded exchange's saved record.
fn record(name: &str) -> Value {
    let (_, text) = RECORDS.iter().find(|(file, _)| *file == name).unwrap();
    serde_json::from_str(text).unwrap()
}

/// A recorded failure's status and body, as the server sent it.
fn recorded_failure(name: &str) -> Response {
    let saved = record(name);
    let status = u16::try_from(saved["status"].as_u64().unwrap()).unwrap();
    let mut response = Response::status(status, saved["body"].to_string());
    response.headers = vec![(
        "content-type".to_owned(),
        saved["headers"]["content-type"]
            .as_str()
            .unwrap()
            .to_owned(),
    )];
    response
}

/// Claude Haiku 4.5 on Vertex, with no key: the signer authenticates.
fn claude(server: &ProviderServer) -> Endpoint {
    Endpoint {
        provider: "vertex".into(),
        model: "claude-haiku-4-5@20251001".into(),
        base_url: format!("{}{PROJECT_PATH}/anthropic", server.url()),
        key: None,
        signer: Some(Arc::new(Bearer)),
        compat: Compat {
            vertex_raw_predict: true,
            thinking_budget: true,
            ..Compat::default()
        },
        ..harness::endpoint("vertex", server)
    }
}

/// Gemini 3.1 Flash-Lite on Vertex, with no key: the signer authenticates.
fn gemini(server: &ProviderServer) -> Endpoint {
    Endpoint {
        provider: "vertex".into(),
        model: "gemini-3.1-flash-lite".into(),
        base_url: format!("{}{PROJECT_PATH}/google", server.url()),
        key: None,
        signer: Some(Arc::new(Bearer)),
        ..harness::endpoint("vertex", server)
    }
}

#[test]
fn claude_posts_to_stream_raw_predict_with_the_version_in_the_body_and_no_model() {
    let server = ProviderServer::start([anthropic_completed()]).unwrap();
    let (reply, _) = run(Messages::new(claude(&server)).call(&request()));
    assert_eq!(reply.unwrap().text(), "hi");

    let sent = &server.requests()[0];
    assert_eq!(
        sent.path,
        format!("{PROJECT_PATH}/anthropic/models/claude-haiku-4-5@20251001:streamRawPredict")
    );
    assert_eq!(
        sent.header("authorization"),
        Some(fingerprint("Bearer vertex-test-token").as_str())
    );
    assert_eq!(sent.header("x-api-key"), None);
    let body = sent_body(&server, 0);
    assert_eq!(body["anthropic_version"], "vertex-2023-10-16");
    assert!(body.get("model").is_none(), "{body}");
    assert_eq!(body["stream"], true);
    assert_eq!(body["messages"][0]["role"], "user");
}

#[test]
fn a_trailing_slash_on_the_base_url_is_dropped() {
    let server = ProviderServer::start([anthropic_completed()]).unwrap();
    let endpoint = claude(&server);
    let endpoint = Endpoint {
        base_url: format!("{}/", endpoint.base_url),
        ..endpoint
    };
    run(Messages::new(endpoint).call(&request())).0.unwrap();
    assert_eq!(
        server.requests()[0].path,
        format!("{PROJECT_PATH}/anthropic/models/claude-haiku-4-5@20251001:streamRawPredict")
    );
}

#[test]
fn a_listed_error_is_read_as_the_object_inside() {
    // The probe's project had no Claude model enabled, so these are the
    // failures it reached: an unknown model, and a token Google refused.
    let server = ProviderServer::start([
        recorded_failure("claude-unknown-model.json"),
        recorded_failure("claude-bad-token.json"),
        recorded_failure("claude-data-sharing.json"),
    ])
    .unwrap();
    let messages = Messages::new(claude(&server));

    let (unknown, retry) = failed(messages.call(&request()));
    assert_eq!(unknown.code, ErrorCode::ModelNotFound);
    assert_eq!(retry, None);
    let said = unknown.provider.unwrap();
    assert_eq!(said.status, Some(404));
    assert!(
        said.message
            .starts_with("Publisher model `projects/example-project/"),
        "{}",
        said.message
    );

    let (token, _) = failed(messages.call(&request()));
    assert_eq!(token.code, ErrorCode::AuthenticationFailed);
    assert!(
        token
            .provider
            .unwrap()
            .message
            .starts_with("Request had invalid authentication credentials."),
    );

    let (sharing, _) = failed(messages.call(&request()));
    assert_eq!(sharing.code, ErrorCode::InvalidRequest);
    assert!(
        sharing
            .provider
            .unwrap()
            .message
            .starts_with("Access to this model requires data sharing"),
    );
}

#[test]
fn without_the_flag_a_listed_error_keeps_its_body() {
    // Only Vertex sends the list, so only its flag reads inside it.
    let server = ProviderServer::start([recorded_failure("claude-unknown-model.json")]).unwrap();
    let endpoint = Endpoint {
        compat: Compat::default(),
        ..claude(&server)
    };
    let (failure, _) = failed(Messages::new(endpoint).call(&request()));
    assert_eq!(failure.code, ErrorCode::InvalidRequest);
    assert!(
        failure.provider.unwrap().message.starts_with("[{"),
        "the body is kept whole"
    );
}

#[test]
fn a_list_that_is_not_one_object_keeps_its_body() {
    let bodies = [
        r#"[]"#,
        r#"[{"error":{"message":"a"}},{"error":{"message":"b"}}]"#,
        r#"["Publisher model"]"#,
    ];
    let server = ProviderServer::start(bodies.map(|body| Response::status(400, body))).unwrap();
    let messages = Messages::new(claude(&server));
    for body in bodies {
        let (failure, _) = failed(messages.call(&request()));
        assert_eq!(failure.provider.unwrap().message, body);
    }
}

#[test]
fn gemini_posts_to_the_vertex_path_with_the_bearer_token_and_no_api_key() {
    let server = ProviderServer::start([Response::stream(stream("gemini-tool.sse"))]).unwrap();
    let (reply, _) = run(Gemini::new(gemini(&server)).call(&request()));
    let reply = reply.unwrap();
    // The call's signature rides ahead of it, on a reasoning carrier.
    let [ReplyAction::Reasoning(carrier), ReplyAction::ToolCall(call)] = reply.actions.as_slice()
    else {
        panic!("{:?}", reply.actions);
    };
    assert!(carrier.provider_item.as_ref().unwrap()["thoughtSignature"].is_string());
    assert_eq!(call.name, "get_weather");
    assert_eq!(call.provider_id, Some(ProviderCallId("call_275871".into())));

    let sent = &server.requests()[0];
    assert_eq!(
        sent.path,
        format!("{PROJECT_PATH}/google/models/gemini-3.1-flash-lite:streamGenerateContent?alt=sse")
    );
    assert_eq!(
        sent.header("authorization"),
        Some(fingerprint("Bearer vertex-test-token").as_str())
    );
    assert_eq!(sent.header("x-goog-api-key"), None);
}

#[test]
fn geminis_vertex_failures_take_their_codes() {
    let server = ProviderServer::start([
        recorded_failure("gemini-unknown-model.json"),
        recorded_failure("gemini-429.json"),
        recorded_failure("gemini-bad-request.json"),
        recorded_failure("gemini-api-key-header.json"),
        recorded_failure("gemini-expired-token.json"),
    ])
    .unwrap();
    let gemini = Gemini::new(gemini(&server));
    let codes: Vec<ErrorCode> = (0..5)
        .map(|_| failed(gemini.call(&request())).0.code)
        .collect();
    assert_eq!(
        codes,
        [
            ErrorCode::ModelNotFound,
            ErrorCode::RateLimited,
            ErrorCode::InvalidRequest,
            ErrorCode::AuthenticationFailed,
            ErrorCode::AuthenticationFailed,
        ]
    );
}

#[test]
fn a_404_that_names_no_publisher_model_is_still_a_wrong_path() {
    // Vertex answers an unknown location's host with an HTML 404.
    let server = ProviderServer::start([recorded_failure("claude-bad-location.json")]).unwrap();
    let (failure, _) = failed(Messages::new(claude(&server)).call(&request()));
    assert_eq!(failure.code, ErrorCode::InvalidRequest);
    assert!(
        failure.message.contains("Check the base URL"),
        "{}",
        failure.message
    );
}

fn decoded(bytes: &[u8]) -> (Result<Reply, provider::Error>, Vec<Delta>) {
    let mut deltas = Vec::new();
    let reply = decode(bytes, &mut |d| deltas.push(d));
    (reply, deltas)
}

/// What a recorded stream holds, read from its chunks independently of the
/// decoder: the reply text, the thought text, the calls with their ids,
/// the signed parts and the last usage.
fn holds(bytes: &[u8]) -> (String, String, Vec<Value>, usize, Value) {
    let (mut text, mut thought, mut calls, mut signed, mut usage) =
        (String::new(), String::new(), Vec::new(), 0, Value::Null);
    for line in String::from_utf8_lossy(bytes).lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let chunk: Value = serde_json::from_str(data).unwrap();
        for part in chunk["candidates"][0]["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if part.get("thoughtSignature").is_some() {
                signed += 1;
            }
            if let Some(call) = part.get("functionCall") {
                calls.push(call.clone());
            } else if part["thought"] == true {
                thought.push_str(part["text"].as_str().unwrap_or(""));
            } else if let Some(fragment) = part["text"].as_str() {
                text.push_str(fragment);
            }
        }
        if let Some(meta) = chunk.get("usageMetadata") {
            usage = meta.clone();
        }
    }
    (text, thought, calls, signed, usage)
}

#[test]
fn every_recorded_vertex_stream_decodes_into_what_it_holds() {
    let (mut calls_seen, mut with_ids, mut thoughts) = (0, 0, 0);
    for (label, bytes) in STREAMS {
        let (reply, _) = decoded(bytes);
        let reply = reply.unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(reply.finish, Finish::Completed, "{label}");
        let (text, thought, calls, signed, usage) = holds(bytes);
        assert_eq!(reply.text(), text, "{label}");

        let got: Vec<_> = reply
            .actions
            .iter()
            .filter_map(|action| match action {
                ReplyAction::ToolCall(call) => Some(call),
                ReplyAction::Reasoning(_)
                | ReplyAction::Text(_)
                | ReplyAction::Hosted(_)
                | ReplyAction::HostedResult { .. } => None,
            })
            .collect();
        assert_eq!(got.len(), calls.len(), "{label}");
        for (call, want) in got.iter().zip(&calls) {
            assert_eq!(call.name, want["name"].as_str().unwrap(), "{label}");
            assert_eq!(call.arguments, want["args"], "{label}");
            assert_eq!(
                call.provider_id,
                want["id"].as_str().map(|id| ProviderCallId(id.into())),
                "{label}"
            );
            calls_seen += 1;
            with_ids += usize::from(call.provider_id.is_some());
        }

        let reasoning: String = reply
            .actions
            .iter()
            .filter_map(|action| match action {
                ReplyAction::Reasoning(reasoning) => Some(reasoning.text.as_str()),
                ReplyAction::ToolCall(_)
                | ReplyAction::Text(_)
                | ReplyAction::Hosted(_)
                | ReplyAction::HostedResult { .. } => None,
            })
            .collect();
        assert_eq!(reasoning, thought, "{label}");
        thoughts += usize::from(!thought.is_empty());

        // Every signature the stream carried rides on an item, so each
        // goes back with its part.
        let items = reply
            .actions
            .iter()
            .filter(|action| {
                let item = match action {
                    ReplyAction::Text(part) => part.provider_item.as_ref(),
                    ReplyAction::Reasoning(reasoning) => reasoning.provider_item.as_ref(),
                    ReplyAction::ToolCall(_)
                    | ReplyAction::Hosted(_)
                    | ReplyAction::HostedResult { .. } => None,
                };
                item.is_some_and(|item| item.get("thoughtSignature").is_some())
            })
            .count();
        assert_eq!(items, signed, "{label}");

        let count = |key: &str| usage[key].as_u64().unwrap_or(0);
        assert_eq!(
            reply.tokens,
            Tokens {
                input: count("promptTokenCount") - count("cachedContentTokenCount"),
                cache_read: count("cachedContentTokenCount"),
                cache_write: std::collections::BTreeMap::new(),
                output: count("candidatesTokenCount") + count("thoughtsTokenCount"),
            },
            "{label}"
        );
        assert!(reply.generation_id.is_some(), "{label}");
    }
    // 29 streams; 13 calls, of which the 2.5 model's two and the one from
    // `gemini-3.1-pro-preview-customtools` carry no id; four streams
    // carried thought text.
    assert_eq!(
        (STREAMS.len(), calls_seen, with_ids, thoughts),
        (29, 13, 10, 4)
    );
}

#[test]
fn a_thinking_level_streams_a_thought_then_the_text_and_its_signature() {
    let (reply, deltas) = decoded(stream("gemini-thinking.sse"));
    let reply = reply.unwrap();
    assert_eq!(reply.text(), "No. 391 = 17 × 23.");
    assert!(
        deltas
            .iter()
            .any(|delta| matches!(delta, Delta::Reasoning(_))),
        "{deltas:?}"
    );
    let Some(ReplyAction::Text(last)) = reply.actions.last() else {
        panic!("{:?}", reply.actions);
    };
    assert!(
        last.provider_item.as_ref().unwrap()["thoughtSignature"].is_string(),
        "the empty signed part's signature rides on the text before it"
    );
}

#[test]
fn a_stream_vertex_ended_before_its_finish_reason_is_incomplete() {
    // One of two requests with the hosted search ended after its first
    // chunk, with no finish reason and no usage.
    let cut = include_bytes!("../../../research/vertex-probe/raw/gemini-search-cut.sse");
    let (reply, _) = decoded(cut);
    assert_eq!(reply.unwrap_err().code(), ErrorCode::StreamIncomplete);
}

#[test]
fn a_hosted_search_on_vertex_streams_no_tool_call_parts() {
    // With `includeServerSideToolInvocations` Vertex still sends the search
    // only as the last chunk's `groundingMetadata`, which Fiber places on a
    // hosted call; with none to place it on, the reply is its text alone.
    let (reply, _) = decoded(stream("gemini-search.sse"));
    let reply = reply.unwrap();
    assert!(
        reply.actions.iter().all(|action| !matches!(
            action,
            ReplyAction::Hosted(_) | ReplyAction::HostedResult { .. }
        )),
        "{:?}",
        reply.actions
    );
    assert!(reply.text().contains("Rust"), "{}", reply.text());
}
