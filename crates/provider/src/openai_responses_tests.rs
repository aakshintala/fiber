#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::collections::BTreeMap;

use contract::events::CacheLifetime;
use contract::provider::ModelRequest;
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
        conversation: Vec::new(),
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
fn a_failed_call_counts_its_write_under_the_request_lifetime() {
    let usage = usage(2347, Some(2344), 0, 28);
    let event = json!({"type": "response.failed", "response": {"id": "resp_1",
        "status": "failed",
        "error": {"code": "server_error", "message": "boom"}, "usage": usage}});
    let bytes = format!(
        "event: {}\ndata: {event}\n\n",
        event["type"].as_str().unwrap()
    );
    let mut deltas = Vec::new();
    let (_, partial) = decode_tracked(bytes.as_bytes(), &CacheLifetime::OneHour, &mut |delta| {
        deltas.push(delta)
    })
    .unwrap_err();
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
