//! `bedrock-converse` through the provider crate's public API
//! (`docs/model-routing.md`, "Bedrock Converse wire facts"): the probe's
//! recorded event streams served by the fake provider server, the requests a
//! Converse model sends, and the failures Bedrock reports in its headers and
//! inside the stream (`research/bedrock-converse-probe`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

#[path = "support/harness.rs"]
mod harness;

use contract::events::ToolCallRequested;
use contract::provider::{
    CallError, Delta, Finish, ImageRef, Input, ModelRequest, PdfRef, Provider, Reply, ReplyAction,
    ToolDefinition,
};
use contract::{ActionId, ErrorCode, GenerationId, ProviderCallId, ThinkingLevel};
use fakes::{ProviderServer, Response, fingerprint};
use provider::bedrock_converse::Converse;
use provider::{Compat, Endpoint, ReasoningField};
use serde_json::{Map, Value, json};

use harness::{failed, run, sent_body};

const MODEL: &str = "openai.gpt-oss-20b-1:0";

const PLAIN: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/plain.eventstream");
const TOOL: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/tool-ministral.eventstream");
const SIGNED: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/signature.eventstream");
const REDACTED: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/redacted-first.eventstream");
const REASONING: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/reasoning-oss-low.eventstream");
const CACHE_WRITE: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/cache-write.eventstream");
const CACHE_READ: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/cache-read.eventstream");
const CUT_OFF: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/tool-oss.eventstream");

fn endpoint(server: &ProviderServer) -> Endpoint {
    Endpoint {
        model: MODEL.into(),
        base_url: server.url(),
        max_output_tokens: Some(128_000),
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

/// A synthetic request id: a fixed fake UUID, never one AWS issued.
const REQUEST_ID: &str = "11111111-2222-4333-8444-555555555555";

/// As [`stream`], naming the request in `x-amzn-RequestId` as AWS does.
fn stream_named(body: &[u8], request_id: &str) -> Response {
    let mut response = stream(body);
    response
        .headers
        .push(("x-amzn-RequestId".to_owned(), request_id.to_owned()));
    response
}

/// The reply `body` decodes into, with its deltas, through one call.
fn served(body: &[u8]) -> (Result<Reply, CallError>, Vec<Delta>) {
    let server = ProviderServer::start([stream(body)]).unwrap();
    run(Converse::new(endpoint(&server)).call(&harness::request()))
}

/// The body `request` sends from `endpoint`, as the server received it.
fn body_of(endpoint: impl Fn(&ProviderServer) -> Endpoint, request: &ModelRequest) -> Value {
    let server = ProviderServer::start([stream(PLAIN)]).unwrap();
    run(Converse::new(endpoint(&server)).call(request))
        .0
        .unwrap();
    sent_body(&server, 0)
}

/// A recording's decoded events, from the probe's record beside it.
fn recorded_events(record: &str) -> Vec<Value> {
    let record: Value = serde_json::from_str(record).unwrap();
    record["events"].as_array().unwrap().clone()
}

fn png(path: &str) -> ImageRef {
    ImageRef {
        path: path.into(),
        mime_type: "image/png".into(),
        width: 2,
        height: 1,
    }
}

fn tool(name: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        deferred: false,
        hosted: None,
    }
}

fn call(action: &str, provider_id: Option<&str>, name: &str, arguments: Value) -> Input {
    Input::ToolCall {
        action_id: ActionId(action.into()),
        call: ToolCallRequested {
            name: name.into(),
            arguments,
            provider_id: provider_id.map(|id| ProviderCallId(id.into())),
            repair: None,
            ran_by: None,
            provider_item: None,
        },
        model: format!("bedrock/{MODEL}"),
    }
}

fn result(action: &str, text: &str, is_error: bool, images: Vec<ImageRef>) -> Input {
    Input::ToolResult {
        action_id: ActionId(action.into()),
        text: text.into(),
        is_error,
        images,
        pdfs: Vec::new(),
    }
}

fn user(text: &str) -> Input {
    Input::User {
        text: text.into(),
        images: Vec::new(),
    }
}

#[test]
fn the_recorded_plain_stream_decodes_into_its_text_and_usage_under_no_generation() {
    let (reply, deltas) = served(PLAIN);
    let reply = reply.unwrap();
    assert_eq!(reply.finish, Finish::Completed);
    let [ReplyAction::Text(text)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(text.text, "Hello! How\u{2019}s it goin\u{2019} now?");
    assert_eq!(text.provider_item, None);
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
    assert_eq!(reply.generation_id, None);
    assert_eq!((reply.tokens.input, reply.tokens.output), (15, 11));
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
        ("Paris", "tooluse_tHrm6EVp9eFVhl3bFbWxUS"),
        ("Rome", "tooluse_GyH1vuzV6C6ytAEuTAfj84"),
    ]) {
        assert_eq!(call.name, "get_weather");
        assert_eq!(call.arguments, json!({"city": city}));
        assert_eq!(call.provider_id, Some(ProviderCallId(id.into())));
    }
    let indices: Vec<u32> = deltas
        .iter()
        .map(|delta| {
            let Delta::ToolCallArguments(d) = delta else {
                panic!("{delta:?}");
            };
            assert_eq!(d.name.as_deref(), Some("get_weather"));
            d.index
        })
        .collect();
    assert_eq!(indices, [0, 1]);
    assert_eq!((reply.tokens.input, reply.tokens.output), (96, 23));
}

#[test]
fn the_recorded_signed_reasoning_is_kept_in_the_form_converse_takes_back() {
    let (reply, deltas) = served(SIGNED);
    let reply = reply.unwrap();
    let signature: String = recorded_events(include_str!(
        "../../../research/bedrock-converse-probe/raw/signature.json"
    ))
    .iter()
    .filter_map(|event| {
        event
            .pointer("/payload/delta/reasoningContent/signature")?
            .as_str()
    })
    .collect();
    assert!(!signature.is_empty());
    let [ReplyAction::Reasoning(reasoning), ReplyAction::Text(text)] = reply.actions.as_slice()
    else {
        panic!("{:?}", reply.actions);
    };
    assert!(reasoning.text.ends_with("= 391"), "{}", reasoning.text);
    assert_eq!(
        reasoning.provider_item,
        Some(json!({"reasoningContent": {"reasoningText": {
            "text": reasoning.text, "signature": signature,
        }}}))
    );
    assert_eq!(text.text, "391");
    let streamed: String = deltas
        .iter()
        .filter_map(|delta| match delta {
            Delta::Reasoning(d) => Some(d.text.as_str()),
            Delta::Text(_) | Delta::ToolCallArguments(_) => None,
        })
        .collect();
    assert_eq!(streamed, reasoning.text);
}

#[test]
fn unsigned_and_redacted_reasoning_are_kept_as_they_came() {
    let (reply, _) = served(REASONING);
    let reply = reply.unwrap();
    let [ReplyAction::Reasoning(reasoning), ReplyAction::Text(text)] = reply.actions.as_slice()
    else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(reasoning.text, "Just compute 17*23 = 391.");
    assert_eq!(
        reasoning.provider_item,
        Some(json!({"reasoningContent": {"reasoningText": {"text": "Just compute 17*23 = 391."}}}))
    );
    assert_eq!(text.text, "391");

    let (reply, deltas) = served(REDACTED);
    let reply = reply.unwrap();
    let redacted: String = recorded_events(include_str!(
        "../../../research/bedrock-converse-probe/raw/redacted-first.json"
    ))
    .iter()
    .filter_map(|event| {
        event
            .pointer("/payload/delta/reasoningContent/redactedContent")?
            .as_str()
    })
    .collect();
    let [ReplyAction::Reasoning(reasoning), ReplyAction::Text(_)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    assert_eq!(reasoning.text, "");
    assert_eq!(
        reasoning.provider_item,
        Some(json!({"reasoningContent": {"redactedContent": redacted}}))
    );
    // Encrypted reasoning has no words to stream.
    assert!(
        !deltas
            .iter()
            .any(|delta| matches!(delta, Delta::Reasoning(_)))
    );
}

#[test]
fn recorded_cache_writes_and_reads_report_input_without_them() {
    let (reply, _) = served(CACHE_WRITE);
    let tokens = reply.unwrap().tokens;
    assert_eq!((tokens.input, tokens.cache_read, tokens.output), (4, 0, 3));
    assert_eq!(
        tokens.cache_write.into_iter().collect::<Vec<_>>(),
        [("5m".to_owned(), 2121)]
    );
    let (reply, _) = served(CACHE_READ);
    let tokens = reply.unwrap().tokens;
    assert_eq!(
        (tokens.input, tokens.cache_read, tokens.output),
        (4, 2121, 3)
    );
    assert!(tokens.cache_write.is_empty());
}

#[test]
fn a_recorded_reply_cut_off_at_max_tokens_finishes_at_the_output_limit() {
    let (reply, _) = served(CUT_OFF);
    let reply = reply.unwrap();
    assert_eq!(reply.finish, Finish::OutputLimit);
    assert!(matches!(
        reply.actions.as_slice(),
        [ReplyAction::Reasoning(_)]
    ));
    assert_eq!(reply.tokens.output, 512);
}

#[test]
fn a_request_goes_to_the_converse_path_in_bedrocks_shape() {
    let server = ProviderServer::start([stream(PLAIN)]).unwrap();
    let request = ModelRequest {
        tools: vec![tool("zeta", ""), tool("alpha", "Reads a file.")],
        ..harness::request()
    };
    run(Converse::new(endpoint(&server)).call(&request))
        .0
        .unwrap();
    let sent = &server.requests()[0];
    assert_eq!(sent.method, "POST");
    // The id's `:` is percent-encoded, as AWS's SDKs send it.
    assert_eq!(sent.path, "/model/openai.gpt-oss-20b-1%3A0/converse-stream");
    assert_eq!(
        sent.header("accept"),
        Some("application/vnd.amazon.eventstream")
    );
    assert_eq!(sent.header("content-type"), Some("application/json"));
    // A Bedrock API key is a bearer token.
    assert_eq!(
        sent.header("authorization"),
        Some(fingerprint("Bearer sk-secret").as_str())
    );
    let schema = json!({"type": "object", "properties": {"path": {"type": "string"}}});
    // Tools in name order, no `strict`, no `toolChoice` for `auto`, an
    // empty description left out, and no cache marker without the flag.
    assert_eq!(
        sent_body(&server, 0),
        json!({
            "system": [{"text": "You are terse."}],
            "messages": [{"role": "user", "content": [{"text": "hi"}]}],
            "toolConfig": {"tools": [
                {"toolSpec": {"name": "alpha", "description": "Reads a file.",
                    "inputSchema": {"json": schema}}},
                {"toolSpec": {"name": "zeta", "inputSchema": {"json": schema}}},
            ]},
            "inferenceConfig": {"maxTokens": 128_000},
        })
    );
    // What the request sends is the tools Fiber builds.
    assert_eq!(
        Converse::new(endpoint(&server)).wire_tools(&request.tools),
        sent_body(&server, 0)["toolConfig"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool.as_object().unwrap().clone())
            .collect::<Vec<Map<String, Value>>>()
    );
}

#[test]
fn a_conversation_renders_as_converse_blocks_with_images_beside_their_results() {
    let image = json!({"image": {"format": "png", "source": {"bytes": "YWJjZA=="}}});
    let reasoning =
        json!({"reasoningContent": {"reasoningText": {"text": "think", "signature": "sig"}}});
    let request = ModelRequest {
        conversation: (vec![
            Input::User {
                text: "look".into(),
                images: vec![png("i.png")],
            },
            Input::Reasoning {
                model: format!("bedrock/{MODEL}"),
                text: "think".into(),
                provider_item: Some(reasoning.clone()),
            },
            // Another model's reasoning is left out; its words are kept.
            Input::Reasoning {
                model: "other/m".into(),
                text: "theirs".into(),
                provider_item: Some(json!({"opaque": true})),
            },
            Input::Assistant {
                model: "other/m".into(),
                text: "Reading.".into(),
                provider_item: Some(json!({"opaque": true})),
            },
            call(
                "a_1",
                Some("tooluse_Ab12Cd34Ef"),
                "read",
                json!({"path": "a.md"}),
            ),
            call("a_2", None, "shell", json!({"cmd": "ls"})),
            result("a_1", "contents", false, Vec::new()),
            result("a_2", "boom", true, vec![png("i.png")]),
            user("go on"),
        ])
        .into(),
        media: harness::media(&[("i.png", b"abcd")]),
        ..harness::request()
    };
    let body = body_of(endpoint, &request);
    assert_eq!(
        body["messages"],
        json!([
            {"role": "user", "content": [{"text": "look"}, image]},
            {"role": "assistant", "content": [
                reasoning,
                {"text": "Reading."},
                {"toolUse": {"toolUseId": "tooluse_Ab12Cd34Ef", "name": "read", "input": {"path": "a.md"}}},
                {"toolUse": {"toolUseId": "a_2", "name": "shell", "input": {"cmd": "ls"}}},
            ]},
            {"role": "user", "content": [
                {"toolResult": {"toolUseId": "tooluse_Ab12Cd34Ef", "content": [{"text": "contents"}]}},
                {"toolResult": {"toolUseId": "a_2", "content": [{"text": "boom"}], "status": "error"}},
                image,
                {"text": "go on"},
            ]},
        ])
    );
}

#[test]
fn an_id_whose_last_nine_are_not_letters_and_digits_is_replayed_as_its_action_id() {
    // Mistral reads a `toolUseId`'s last nine characters and refuses any but
    // letters and digits; another provider's id can end otherwise.
    let request = ModelRequest {
        conversation: (vec![
            user("go"),
            call(
                "a_00000000000000a1",
                Some("tooluse_AbCdEfGhIjKlMn"),
                "read",
                json!({}),
            ),
            call(
                "a_00000000000000a2",
                Some("functions.read:0"),
                "read",
                json!({}),
            ),
            call("a_00000000000000a3", Some("call12345"), "read", json!({})),
            result("a_00000000000000a1", "one", false, Vec::new()),
            result("a_00000000000000a2", "two", false, Vec::new()),
            result("a_00000000000000a3", "three", false, Vec::new()),
        ])
        .into(),
        ..harness::request()
    };
    let body = body_of(endpoint, &request);
    let ids = |role: usize, key: &str| -> Vec<Value> {
        body["messages"][role]["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| block[key]["toolUseId"].clone())
            .collect()
    };
    let expected = json!(["tooluse_AbCdEfGhIjKlMn", "a_00000000000000a2", "call12345"]);
    assert_eq!(json!(ids(1, "toolUse")), expected);
    assert_eq!(json!(ids(2, "toolResult")), expected);
}

#[test]
fn a_pdfs_rendered_pages_ride_beside_its_result_and_a_text_only_model_gets_lines() {
    let request = ModelRequest {
        conversation: (vec![
            call(
                "a_1",
                Some("tooluse_Ab12Cd34Ef"),
                "read",
                json!({"path": "p.pdf"}),
            ),
            Input::ToolResult {
                action_id: ActionId("a_1".into()),
                text: "PDF: 1 page.".into(),
                is_error: false,
                images: Vec::new(),
                pdfs: vec![PdfRef {
                    path: "p.pdf".into(),
                    page_count: 1,
                    pages: Some(vec![png("p-1.png")]),
                }],
            },
        ])
        .into(),
        media: harness::media(&[("p.pdf", b"%PDF"), ("p-1.png", b"abcd")]),
        ..harness::request()
    };
    let body = body_of(endpoint, &request);
    assert_eq!(
        body["messages"][1]["content"],
        json!([
            {"toolResult": {"toolUseId": "tooluse_Ab12Cd34Ef", "content": [{"text": "PDF: 1 page."}]}},
            {"image": {"format": "png", "source": {"bytes": "YWJjZA=="}}},
        ])
    );
    let text_only = |server: &ProviderServer| Endpoint {
        text_only: true,
        ..endpoint(server)
    };
    let body = body_of(text_only, &request);
    assert_eq!(
        body["messages"][1]["content"],
        json!([{"toolResult": {"toolUseId": "tooluse_Ab12Cd34Ef", "content": [{"text":
            "PDF: 1 page.\n[PDF p.pdf left out: this model does not take images.]"}]}}])
    );
}

#[test]
fn cache_points_follow_text_where_anthropics_markers_go_on_a_model_that_declares_them() {
    let cached = |server: &ProviderServer| Endpoint {
        compat: Compat {
            cache_point: true,
            ..Compat::default()
        },
        ..endpoint(server)
    };
    let point = json!({"cachePoint": {"type": "default"}});
    let tool_use =
        json!({"toolUse": {"toolUseId": "tooluse_Ab12Cd34Ef", "name": "read", "input": {}}});
    let tool_result =
        json!({"toolResult": {"toolUseId": "tooluse_Ab12Cd34Ef", "content": [{"text": "x"}]}});
    let mut request = ModelRequest {
        conversation: (vec![
            user("first"),
            Input::Assistant {
                model: format!("bedrock/{MODEL}"),
                text: "ok".into(),
                provider_item: None,
            },
            call("a_1", Some("tooluse_Ab12Cd34Ef"), "read", json!({})),
            result("a_1", "x", false, Vec::new()),
        ])
        .into(),
        previous_end: Some(2),
        ..harness::request()
    };
    // The previous request ended on the reply's text: marked. The new end
    // is a tool result, which takes no marker.
    let body = body_of(cached, &request);
    assert_eq!(body["system"], json!([{"text": "You are terse."}, point]));
    assert_eq!(
        body["messages"],
        json!([
            {"role": "user", "content": [{"text": "first"}]},
            {"role": "assistant", "content": [{"text": "ok"}, point, tool_use]},
            {"role": "user", "content": [tool_result]},
        ])
    );
    // A text after the result takes the new end's marker.
    let mut conversation = request.conversation.to_vec();
    conversation.push(user("next"));
    request.conversation = conversation.into();
    let body = body_of(cached, &request);
    assert_eq!(
        body["messages"][2]["content"],
        json!([tool_result, {"text": "next"}, point])
    );
    // Without the flag, no marker anywhere.
    let body = body_of(endpoint, &request);
    assert!(!body.to_string().contains("cachePoint"), "{body}");
}

#[test]
fn a_thinking_level_rides_in_the_field_the_models_data_names() {
    let with = |field: Option<ReasoningField>| {
        move |server: &ProviderServer| Endpoint {
            compat: Compat {
                reasoning_field: field,
                ..Compat::default()
            },
            ..endpoint(server)
        }
    };
    let at = |level: Option<ThinkingLevel>| ModelRequest {
        thinking: level,
        ..harness::request()
    };
    let effort = with(Some(ReasoningField::ReasoningEffort));
    let config = with(Some(ReasoningField::ReasoningConfig));
    for level in [ThinkingLevel::Low, ThinkingLevel::High] {
        let body = body_of(effort, &at(Some(level)));
        assert_eq!(
            body["additionalModelRequestFields"],
            json!({"reasoning_effort": level.as_str()})
        );
        assert_eq!(body["inferenceConfig"], json!({"maxTokens": 128_000}));
    }
    let body = body_of(config, &at(Some(ThinkingLevel::Medium)));
    assert_eq!(
        body["additionalModelRequestFields"],
        json!({"reasoningConfig": {"type": "enabled", "maxReasoningEffort": "medium"}})
    );
    assert_eq!(body["inferenceConfig"], json!({"maxTokens": 128_000}));
    // Nova 2 Lite refuses `maxTokens` beside `high`.
    let body = body_of(config, &at(Some(ThinkingLevel::High)));
    assert_eq!(
        body["additionalModelRequestFields"],
        json!({"reasoningConfig": {"type": "enabled", "maxReasoningEffort": "high"}})
    );
    assert_eq!(body.get("inferenceConfig"), None, "{body}");
    // `off`, no level, or no field: nothing is sent for thinking.
    for (endpoint, level) in [
        (
            &effort as &dyn Fn(&ProviderServer) -> Endpoint,
            Some(ThinkingLevel::Off),
        ),
        (&config, None),
        (&with(None), Some(ThinkingLevel::High)),
    ] {
        let body = body_of(endpoint, &at(level));
        assert_eq!(body.get("additionalModelRequestFields"), None, "{body}");
        assert_eq!(body["inferenceConfig"], json!({"maxTokens": 128_000}));
    }
}

#[test]
fn extra_body_is_laid_over_the_inference_and_model_fields_key_by_key() {
    let mut extra = Map::new();
    extra.insert("additionalModelRequestFields".into(), json!({"top_k": 5}));
    extra.insert(
        "inferenceConfig".into(),
        json!({"maxTokens": 200_000, "temperature": 0.2}),
    );
    extra.insert("performanceConfig".into(), json!({"latency": "optimized"}));
    let extra_endpoint = |field: ReasoningField| {
        let extra = extra.clone();
        move |server: &ProviderServer| Endpoint {
            compat: Compat {
                reasoning_field: Some(field),
                ..Compat::default()
            },
            extra_body: extra.clone(),
            ..endpoint(server)
        }
    };
    let at = |level: ThinkingLevel| ModelRequest {
        thinking: Some(level),
        ..harness::request()
    };
    let body = body_of(
        extra_endpoint(ReasoningField::ReasoningEffort),
        &at(ThinkingLevel::Low),
    );
    assert_eq!(
        body["additionalModelRequestFields"],
        json!({"reasoning_effort": "low", "top_k": 5})
    );
    // The model's own output limit still caps the data's.
    assert_eq!(
        body["inferenceConfig"],
        json!({"maxTokens": 128_000, "temperature": 0.2})
    );
    assert_eq!(body["performanceConfig"], json!({"latency": "optimized"}));
    let body = body_of(
        extra_endpoint(ReasoningField::ReasoningConfig),
        &at(ThinkingLevel::High),
    );
    assert_eq!(body["inferenceConfig"], json!({"temperature": 0.2}));
}

#[test]
fn an_extra_body_object_for_any_other_field_replaces_fiber_s() {
    let mut extra = Map::new();
    extra.insert("toolConfig".into(), json!({"toolChoice": {"any": {}}}));
    let request = ModelRequest {
        tools: vec![tool("read", "reads a file")],
        ..harness::request()
    };
    let body = body_of(
        |server| Endpoint {
            extra_body: extra.clone(),
            ..endpoint(server)
        },
        &request,
    );
    assert_eq!(body["toolConfig"], json!({"toolChoice": {"any": {}}}));
}

#[test]
fn an_assistant_part_with_no_words_sends_no_text_block() {
    let request = ModelRequest {
        conversation: (vec![
            user("look"),
            Input::Assistant {
                model: format!("bedrock/{MODEL}"),
                text: String::new(),
                provider_item: None,
            },
            call(
                "a_1",
                Some("tooluse_Ab12Cd34Ef"),
                "read",
                json!({"path": "a.md"}),
            ),
            result("a_1", "contents", false, Vec::new()),
        ])
        .into(),
        ..harness::request()
    };
    let body = body_of(endpoint, &request);
    assert_eq!(
        body["messages"][1],
        json!({"role": "assistant", "content": [
            {"toolUse": {"toolUseId": "tooluse_Ab12Cd34Ef", "name": "read", "input": {"path": "a.md"}}},
        ]})
    );
}

#[test]
fn only_a_request_that_must_leave_out_max_tokens_declines_to_warm() {
    let server = ProviderServer::start(Vec::<Response>::new()).unwrap();
    let converse = |field| {
        Converse::new(Endpoint {
            compat: Compat {
                reasoning_field: field,
                ..Compat::default()
            },
            ..endpoint(&server)
        })
    };
    let at = |level| ModelRequest {
        thinking: level,
        ..harness::request()
    };
    assert!(!converse(Some(ReasoningField::ReasoningConfig)).warms(&at(Some(ThinkingLevel::High))));
    for (field, level) in [
        (
            Some(ReasoningField::ReasoningConfig),
            Some(ThinkingLevel::Medium),
        ),
        (
            Some(ReasoningField::ReasoningEffort),
            Some(ThinkingLevel::High),
        ),
        (None, Some(ThinkingLevel::High)),
        (Some(ReasoningField::ReasoningConfig), None),
    ] {
        assert!(converse(field).warms(&at(level)), "{field:?} {level:?}");
    }
}

#[test]
fn a_tool_choice_other_than_auto_or_none_is_sent_and_no_tools_send_no_tool_config() {
    let with_choice = |choice: &str| ModelRequest {
        tools: vec![tool("read", "Reads.")],
        tool_choice: choice.into(),
        ..harness::request()
    };
    for (choice, sent) in [
        ("any", Some(json!({"any": {}}))),
        ("read", Some(json!({"tool": {"name": "read"}}))),
        ("auto", None),
        ("none", None),
    ] {
        let body = body_of(endpoint, &with_choice(choice));
        assert_eq!(
            body["toolConfig"].get("toolChoice"),
            sent.as_ref(),
            "{choice}"
        );
    }
    let body = body_of(endpoint, &harness::request());
    assert_eq!(body.get("toolConfig"), None, "{body}");
    // An empty system prompt sends no `system`.
    let body = body_of(
        endpoint,
        &ModelRequest {
            system_prompt: String::new(),
            ..harness::request()
        },
    );
    assert_eq!(body.get("system"), None, "{body}");
}

#[test]
fn a_rewound_sessions_first_request_sends_its_parents_tools_verbatim() {
    let sent_tools = vec![
        json!({"toolSpec": {"name": "old", "inputSchema": {"json": {"type": "object"}}}})
            .as_object()
            .unwrap()
            .clone(),
    ];
    let request = ModelRequest {
        tools: vec![tool("new", "New.")],
        sent_tools: Some(sent_tools.clone()),
        ..harness::request()
    };
    let body = body_of(endpoint, &request);
    assert_eq!(
        body["toolConfig"]["tools"],
        Value::Array(sent_tools.into_iter().map(Value::Object).collect())
    );
}

#[test]
fn an_exception_inside_the_stream_fails_the_call_with_the_providers_message() {
    // The plain recording's first three messages, then a throttling
    // exception as ConverseStream documents it.
    let mut at = 0;
    for _ in 0..3 {
        at += usize::try_from(u32::from_be_bytes(PLAIN[at..at + 4].try_into().unwrap())).unwrap();
    }
    let mut body = PLAIN[..at].to_vec();
    body.extend(harness_frame(
        &[
            (":exception-type", "throttlingException"),
            (":content-type", "application/json"),
            (":message-type", "exception"),
        ],
        br#"{"message":"Too many requests."}"#,
    ));
    let server = ProviderServer::start([stream(&body)]).unwrap();
    let (failure, should_retry) =
        failed(Converse::new(endpoint(&server)).call(&harness::request()));
    assert_eq!(failure.code, ErrorCode::RateLimited);
    assert_eq!(should_retry, None);
    let said = failure.provider.unwrap();
    assert_eq!(said.status, Some(200));
    assert_eq!(said.message, "Too many requests.");
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
fn harness_frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
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
            include_str!("../../../research/bedrock-converse-probe/raw/bad-model.json"),
            ErrorCode::ModelNotFound,
            "The provided model identifier is invalid.",
        ),
        (
            include_str!("../../../research/bedrock-converse-probe/raw/validation.json"),
            ErrorCode::InvalidRequest,
            "1 validation error detected: Value '0' at 'inferenceConfig.maxTokens' failed to satisfy constraint: Member must have value greater than or equal to 1",
        ),
        (
            include_str!(
                "../../../research/bedrock-converse-probe/raw/classify/global.zai.glm-5.3.json"
            ),
            ErrorCode::AuthenticationFailed,
            "zai.glm-5.3 is not available for this account. You can explore other available models on Amazon Bedrock. For additional access options, contact AWS Sales at https://aws.amazon.com/contact-us/sales-support/",
        ),
    ] {
        let server = ProviderServer::start([answered(record)]).unwrap();
        let (failure, _) = failed(Converse::new(endpoint(&server)).call(&harness::request()));
        assert_eq!(failure.code, code, "{said}");
        assert_eq!(failure.provider.unwrap().message, said);
    }
}

#[test]
fn the_responses_request_id_names_the_generation_and_a_missing_or_empty_one_leaves_it_unnamed() {
    for (response, named) in [
        (
            stream_named(PLAIN, REQUEST_ID),
            Some(GenerationId(REQUEST_ID.into())),
        ),
        (stream_named(PLAIN, ""), None),
        (stream(PLAIN), None),
    ] {
        let server = ProviderServer::start([response]).unwrap();
        let (reply, _) = run(Converse::new(endpoint(&server)).call(&harness::request()));
        let reply = reply.unwrap();
        assert_eq!(reply.generation_id, named);
        assert_eq!(reply.text(), "Hello! How\u{2019}s it goin\u{2019} now?");
    }
}

#[test]
fn a_stream_that_fails_after_the_head_still_names_its_generation() {
    // The recording without its last message, `metadata`.
    let mut at = 0;
    let mut ends = Vec::new();
    while at < PLAIN.len() {
        at += usize::try_from(u32::from_be_bytes(PLAIN[at..at + 4].try_into().unwrap())).unwrap();
        ends.push(at);
    }
    let cut = &PLAIN[..ends[ends.len() - 2]];
    let server = ProviderServer::start([stream_named(cut, REQUEST_ID)]).unwrap();
    let (reply, _) = run(Converse::new(endpoint(&server)).call(&harness::request()));
    let Err(CallError::Failed { failure, usage, .. }) = reply else {
        panic!("expected a failure: {reply:?}");
    };
    assert_eq!(failure.code, ErrorCode::StreamIncomplete);
    assert_eq!(usage.generation_id, Some(GenerationId(REQUEST_ID.into())));
}

/// A debug writer that keeps nothing, so a call runs observed.
struct Debugging;

impl contract::diag::DebugLog for Debugging {
    fn debugging(&self) -> bool {
        true
    }

    fn http_request(&self, _: &contract::diag::HttpRequest) {}
}

#[test]
fn an_observed_call_names_its_generation_from_the_request_id_too() {
    let server = ProviderServer::start([stream_named(PLAIN, REQUEST_ID)]).unwrap();
    let observation = provider::RequestObservation::new(
        std::sync::Arc::new(Debugging),
        fakes::clock::FakeClock::new(),
        std::num::NonZeroU32::MIN,
    );
    let call = Converse::new(endpoint(&server)).call_observed(&harness::request(), observation);
    let (reply, _) = run(call);
    assert_eq!(
        reply.unwrap().generation_id,
        Some(GenerationId(REQUEST_ID.into()))
    );
}

/// A debug writer that keeps each line a call writes.
#[derive(Default)]
struct Kept(std::sync::Mutex<Vec<contract::diag::HttpRequest>>);

impl contract::diag::DebugLog for Kept {
    fn debugging(&self) -> bool {
        true
    }

    fn http_request(&self, line: &contract::diag::HttpRequest) {
        self.0.lock().unwrap().push(line.clone());
    }
}

#[test]
fn with_cache_diagnostics_the_line_carries_each_segments_hash_and_the_reported_read() {
    let server = ProviderServer::start([stream(CACHE_READ)]).unwrap();
    let kept = std::sync::Arc::new(Kept::default());
    let observation = provider::RequestObservation::new(
        kept.clone(),
        fakes::clock::FakeClock::new(),
        std::num::NonZeroU32::MIN,
    )
    .with_cache_diagnostics(true);
    let mut request = harness::request();
    request.tools = vec![ToolDefinition {
        name: "read".to_owned(),
        description: "reads a file".to_owned(),
        input_schema: json!({"type": "object"}),
        deferred: false,
        hosted: None,
    }];
    let (reply, _) = run(Converse::new(endpoint(&server)).call_observed(&request, observation));
    assert!(reply.is_ok(), "{reply:?}");
    let lines = kept.0.lock().unwrap();
    assert_eq!(lines.len(), 1, "one call writes one line");
    let hashes = lines[0].prefix_hashes.as_ref().expect("hashes are set");
    let hex64 =
        |digest: &String| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit());
    assert!(
        hashes.system.as_ref().is_some_and(hex64),
        "the system blocks hash"
    );
    assert!(hashes.tools.as_ref().is_some_and(hex64), "the tools hash");
    assert!(!hashes.messages.is_empty(), "the conversation hashes");
    assert!(
        hashes.messages.iter().all(hex64),
        "each message boundary hashes"
    );
    assert_eq!(lines[0].cache_read, Some(2121), "the recorded read");
    // The hashed text never lands in the line.
    let line = serde_json::to_string(&lines[0]).unwrap();
    for text in ["You are terse.", "reads a file"] {
        assert!(!line.contains(text), "no {text:?} in the line");
    }
}

#[test]
fn another_protocol_never_takes_the_request_id_as_its_generation() {
    // A Chat Completions stream that names no generation, behind a gateway
    // that adds `x-amzn-RequestId`.
    let body = harness::completions_sse(&[
        json!({"object": "chat.completion.chunk",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}]}),
        json!({"choices": [], "usage": {"prompt_tokens": 3, "completion_tokens": 1}}),
    ]);
    let mut response = Response::stream(body);
    response
        .headers
        .push(("x-amzn-RequestId".to_owned(), REQUEST_ID.to_owned()));
    let server = ProviderServer::start([response]).unwrap();
    let (reply, _) = run(Box::new(
        provider::openai_completions::Completions::new(harness::endpoint("gateway", &server))
            .request(&harness::request()),
    ));
    let reply = reply.unwrap();
    assert_eq!(reply.text(), "hi");
    assert_eq!(reply.generation_id, None);
}

/// The malformed text #3017 replays safely: it parses as neither an object
/// nor an array, so the decoder keeps the raw text for the loop to quote.
const MALFORMED_ARGUMENTS: &str = "{\"command\":\"ls\"]}";

/// The replayed `toolUse` block and its paired `toolResult` for one call
/// with `arguments` beside a completed result.
fn replayed_tool_use(arguments: Value, provider_id: Option<&str>) -> (Value, Value) {
    let request = ModelRequest {
        conversation: vec![
            call("a_1", provider_id, "read", arguments),
            result("a_1", "contents", false, Vec::new()),
        ]
        .into(),
        ..harness::request()
    };
    let body = body_of(endpoint, &request);
    let mut blocks = Vec::new();
    for message in body["messages"].as_array().unwrap() {
        blocks.extend(message["content"].as_array().unwrap().clone());
    }
    let tool_use = blocks
        .iter()
        .find(|block| block.get("toolUse").is_some())
        .unwrap_or_else(|| panic!("no toolUse in {body}"))
        .clone();
    let tool_result = blocks
        .iter()
        .find(|block| block.get("toolResult").is_some())
        .unwrap_or_else(|| panic!("no toolResult in {body}"))
        .clone();
    (tool_use, tool_result)
}

#[test]
fn non_object_tool_arguments_replay_as_an_empty_object() {
    // Every non-object class replays as exactly the empty object, while
    // objects keep their members.
    for (label, arguments) in [
        (
            "malformed string",
            Value::String(MALFORMED_ARGUMENTS.into()),
        ),
        ("empty array", json!([])),
        ("nonempty array", json!([1, 2])),
        ("null", Value::Null),
        ("boolean", json!(true)),
        ("number", json!(3)),
    ] {
        let (tool_use, tool_result) = replayed_tool_use(arguments, None);
        assert_eq!(tool_use["toolUse"]["input"], json!({}), "{label}");
        assert_eq!(
            tool_use["toolUse"]["toolUseId"], tool_result["toolResult"]["toolUseId"],
            "{label}"
        );
    }
    for (label, arguments) in [
        ("empty object", json!({})),
        ("nonempty object", json!({"path": "a.md"})),
    ] {
        let (tool_use, _) = replayed_tool_use(arguments.clone(), None);
        assert_eq!(tool_use["toolUse"]["input"], arguments, "{label}");
    }
}

#[test]
fn a_non_object_call_replays_under_its_provider_id_with_its_paired_result() {
    let (tool_use, tool_result) = replayed_tool_use(
        Value::String(MALFORMED_ARGUMENTS.into()),
        Some("tooluse_Ab12Cd34Ef"),
    );
    assert_eq!(tool_use["toolUse"]["toolUseId"], "tooluse_Ab12Cd34Ef");
    assert_eq!(tool_use["toolUse"]["input"], json!({}));
    assert_eq!(tool_result["toolResult"]["toolUseId"], "tooluse_Ab12Cd34Ef");
}
