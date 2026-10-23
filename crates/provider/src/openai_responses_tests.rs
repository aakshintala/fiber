#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::collections::BTreeMap;

use contract::events::CacheLifetime;
use contract::provider::{Delta, ModelRequest, Reply, ReplyAction};
use contract::shapes::Tokens;
use serde_json::{Value, json};

use super::{Responses, decode, decode_tracked};
use crate::Endpoint;

fn request() -> ModelRequest {
    ModelRequest {
        system_prompt: String::new(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::OneHour,
        cache_key: "s_root".into(),
        conversation: (Vec::new()).into(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        session_dir: std::path::PathBuf::new(),
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

#[test]
fn the_cache_key_goes_in_the_declared_header_and_nowhere_else_without_one() {
    let plain = Responses::new(Endpoint::default()).request(&request());
    assert_eq!(header(&plain.headers, "x-opencode-session"), None);

    let keyed = Responses::new(Endpoint::default())
        .cache_key_header("x-opencode-session")
        .request(&request());
    assert_eq!(header(&keyed.headers, "x-opencode-session"), Some("s_root"));
    assert_eq!(
        header(&keyed.headers, "user-agent"),
        Some(concat!("fiber/", env!("CARGO_PKG_VERSION")))
    );
}

fn sent_limit(extra: Option<u64>, requested: Option<u64>) -> Option<u64> {
    let mut endpoint = Endpoint::default();
    if let Some(limit) = extra {
        endpoint
            .extra_body
            .insert("max_output_tokens".into(), limit.into());
    }
    let mut asked = request();
    asked.max_output_tokens = requested;
    let call = Responses::new(endpoint).request(&asked);
    let body: serde_json::Value = serde_json::from_slice(&call.body).unwrap();
    body["max_output_tokens"].as_u64()
}

#[test]
fn an_extra_body_output_limit_and_the_requests_keep_the_smaller() {
    assert_eq!(sent_limit(Some(100), Some(4096)), Some(100));
    assert_eq!(sent_limit(Some(5000), Some(300)), Some(300));
    assert_eq!(sent_limit(Some(100), None), Some(100));
    assert_eq!(sent_limit(None, Some(300)), Some(300));
    assert_eq!(sent_limit(None, None), None);
    assert_eq!(sent_limit(Some(4), Some(4096)), Some(16));
}

/// A usage block holding `input` tokens with `write` written and `cached`
/// read: `write` `None` leaves the field out, as a reply without one.
fn usage(input: u64, write: Option<u64>, cached: u64, output: u64) -> Value {
    let mut details = serde_json::Map::new();
    if let Some(write) = write {
        details.insert("cache_write_tokens".into(), json!(write));
    }
    details.insert("cached_tokens".into(), json!(cached));
    json!({"input_tokens": input,
        "input_tokens_details": Value::Object(details),
        "output_tokens": output})
}

/// The tokens a stream ending in a completed reply with `usage` decodes
/// to: the public decode drops the request's lifetime, so a write lands
/// under five minutes.
fn decoded(usage: &Value) -> Tokens {
    let body = fakes::wire::stream_with_usage(&[], usage.clone()).body;
    let mut deltas = Vec::new();
    decode(body.as_slice(), &mut |delta| deltas.push(delta))
        .unwrap()
        .tokens
}

#[test]
fn a_warming_block_counts_its_write_under_five_minutes() {
    assert_eq!(
        decoded(&usage(2347, Some(2344), 0, 28)),
        Tokens {
            input: 3,
            cache_read: 0,
            cache_write: BTreeMap::from([("5m".to_owned(), 2344)]),
            output: 28,
        }
    );
}

#[test]
fn a_single_written_token_lands_in_the_map() {
    assert_eq!(
        decoded(&usage(10, Some(1), 4, 3)),
        Tokens {
            input: 5,
            cache_read: 4,
            cache_write: BTreeMap::from([("5m".to_owned(), 1)]),
            output: 3,
        }
    );
}

#[test]
fn a_cached_block_reads_without_a_write() {
    assert_eq!(
        decoded(&usage(2347, Some(0), 2344, 28)),
        Tokens {
            input: 3,
            cache_read: 2344,
            cache_write: BTreeMap::new(),
            output: 28,
        }
    );
}

#[test]
fn a_missing_write_counts_its_input_in_full() {
    assert_eq!(
        decoded(&usage(52, None, 0, 21)),
        Tokens {
            input: 52,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 21,
        }
    );
    let written_as_text = json!({"input_tokens": 52, "input_tokens_details":
            {"cache_write_tokens": "2344", "cached_tokens": 0},
            "output_tokens": 21});
    assert_eq!(decoded(&written_as_text).cache_write, BTreeMap::new());
}

#[test]
fn a_completed_call_counts_its_write_under_five_minutes_for_an_hour_request() {
    let body = fakes::wire::stream_with_usage(&[], usage(2347, Some(2344), 0, 28)).body;
    let mut deltas = Vec::new();
    let reply = decode_tracked(body.as_slice(), &mut |delta| deltas.push(delta)).unwrap();
    assert_eq!(
        reply.tokens,
        Tokens {
            input: 3,
            cache_read: 0,
            cache_write: BTreeMap::from([("5m".to_owned(), 2344)]),
            output: 28,
        }
    );
}

#[test]
fn a_failed_call_counts_its_write_under_five_minutes() {
    let usage = usage(2347, Some(2344), 0, 28);
    let event = json!({"type": "response.failed", "response": {"id": "resp_1",
        "status": "failed",
        "error": {"code": "server_error", "message": "boom"}, "usage": usage}});
    let bytes = format!(
        "event: {}\ndata: {event}\n\n",
        event["type"].as_str().unwrap()
    );
    let mut deltas = Vec::new();
    let (_, partial) =
        decode_tracked(bytes.as_bytes(), &mut |delta| deltas.push(delta)).unwrap_err();
    assert_eq!(
        partial.unwrap().tokens,
        Tokens {
            input: 3,
            cache_read: 0,
            cache_write: BTreeMap::from([("5m".to_owned(), 2344)]),
            output: 28,
        }
    );
}

#[test]
fn a_short_input_saturates_instead_of_wrapping() {
    assert_eq!(decoded(&usage(5, Some(3), 4, 1)).input, 0);
    assert_eq!(decoded(&usage(10, Some(6), 4, 1)).input, 0);
}

/// The reply of a stream of `events` ending in a completed response, and the
/// deltas it passed on.
fn replied(events: &[Value]) -> (Reply, Vec<Delta>) {
    let mut events = events.to_vec();
    events.push(json!({"type": "response.completed", "response": {
        "id": "resp_1", "status": "completed", "usage": {"input_tokens": 1, "output_tokens": 1}
    }}));
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    let mut deltas = Vec::new();
    let reply = decode(body.as_bytes(), &mut |delta| deltas.push(delta)).unwrap();
    (reply, deltas)
}

fn search_item(extra: Value) -> Value {
    let mut item = json!({
        "type": "tool_search_call", "id": "ts_1", "call_id": "call_1",
        "execution": "client", "status": "completed",
        "arguments": {"query": "echo"}
    });
    item.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    item
}

fn done(item: Value) -> Value {
    json!({"type": "response.output_item.done", "item": item})
}

/// The one tool call a reply holds.
fn only_call(reply: &Reply) -> &contract::events::ToolCallRequested {
    let [ReplyAction::ToolCall(call)] = reply.actions.as_slice() else {
        panic!("{:?}", reply.actions);
    };
    call
}

#[test]
fn a_tool_search_call_that_arrives_only_when_done_is_a_tool_call_named_tool_search() {
    let (reply, deltas) = replied(&[done(search_item(json!({})))]);
    let call = only_call(&reply);
    assert_eq!(call.name, "tool_search");
    assert_eq!(call.arguments, json!({"query": "echo"}));
    assert_eq!(call.provider_id.as_ref().unwrap().0, "call_1");
    assert!(call.repair.is_none() && call.ran_by.is_none() && call.provider_item.is_none());
    assert!(deltas.is_empty());
}

#[test]
fn an_added_tool_search_call_is_numbered_so_a_later_calls_deltas_count_it() {
    let (reply, deltas) = replied(&[
        json!({"type": "response.output_item.added", "item": search_item(json!({}))}),
        json!({"type": "response.output_item.added", "item": {
            "type": "function_call", "id": "fc_1", "call_id": "call_2", "name": "echo", "arguments": ""
        }}),
        json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "{}"}),
        done(search_item(json!({}))),
    ]);
    let call = only_call(&reply);
    assert_eq!(call.name, "tool_search");
    assert_eq!(call.arguments, json!({"query": "echo"}));
    let [Delta::ToolCallArguments(delta)] = deltas.as_slice() else {
        panic!("{deltas:?}");
    };
    assert_eq!((delta.index, delta.name.as_deref()), (1, Some("echo")));
}

#[test]
fn a_tool_search_calls_string_arguments_read_as_a_function_calls_do() {
    for (arguments, expected) in [
        (json!("{\"query\":\"echo\"}"), json!({"query": "echo"})),
        (json!(""), json!({})),
        (json!("not json"), json!("not json")),
        (json!("[1]"), json!("[1]")),
        (Value::Null, json!({})),
    ] {
        let (reply, _) = replied(&[done(search_item(json!({"arguments": arguments})))]);
        assert_eq!(only_call(&reply).arguments, expected, "{arguments}");
    }
    let mut missing = search_item(json!({}));
    missing.as_object_mut().unwrap().remove("arguments");
    let (reply, _) = replied(&[done(missing)]);
    assert_eq!(only_call(&reply).arguments, json!({}));
}

#[test]
fn a_tool_search_call_without_a_call_id_has_no_provider_id() {
    let mut item = search_item(json!({}));
    item.as_object_mut().unwrap().remove("call_id");
    let (reply, _) = replied(&[done(item)]);
    assert!(only_call(&reply).provider_id.is_none());
}
