//! A stalled model call through the provider crate's public API: a server
//! that never answers, and a stream that stops after one event, both end
//! the call as `connection_failed` under a short socket limit.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code, helpers included"
)]

use std::time::Duration;

use contract::ErrorCode;
use contract::events::CacheLifetime;
use contract::provider::{CallError, Delta, Input, ModelCall, ModelRequest};
use fakes::{ProviderServer, Response};
use provider::Endpoint;
use provider::anthropic_messages::Messages;

/// How long a test waits for one stalled call to return: far above the
/// 200 ms idle limit, under nextest's 120 s kill.
const DEADLINE: Duration = Duration::from_secs(10);

/// The socket limits a stalled call runs under: one second per address to
/// connect, 200 ms without a byte.
fn stall_limits() -> net::Limits {
    net::Limits::new(Duration::from_secs(1), Duration::from_millis(200)).unwrap()
}

fn request() -> ModelRequest {
    ModelRequest {
        system_prompt: "You are terse.".into(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::OneHour,
        cache_key: "session_1".into(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        conversation: vec![Input::User {
            text: "hi".into(),
            images: Vec::new(),
        }],
        session_dir: std::path::PathBuf::new(),
    }
}

fn endpoint(server: &ProviderServer) -> Endpoint {
    Endpoint {
        provider: "acme".into(),
        model: "m".into(),
        base_url: format!("{}/v1", server.url()),
        key: Some(contract::Secret::new("sk-secret".into())),
        direct: true,
        limits: stall_limits(),
        ..Endpoint::default()
    }
}

/// Runs `call` on its own thread under [`DEADLINE`] and returns its
/// failure: without the limits a stalled call outlives the wait, and the
/// test fails naming the call.
fn failed(call: Box<dyn ModelCall>) -> (contract::shapes::Failure, Option<bool>) {
    fakes::within("the stalled call to fail", DEADLINE, move || {
        let mut deltas = Vec::new();
        let reply = call.run(&mut |d: Delta| deltas.push(d));
        let Err(CallError::Failed {
            failure,
            should_retry,
            ..
        }) = reply
        else {
            panic!("expected a failure");
        };
        (failure, should_retry)
    })
}

#[test]
fn a_server_that_never_answers_fails_the_call_as_connection_failed() {
    let server = ProviderServer::start([Response::status(200, "{}")]).unwrap();
    server.hold();
    let endpoint = endpoint(&server);
    let call = Box::new(Messages::new(endpoint).request(&request()));
    let (failure, should_retry) = failed(call);
    assert_eq!(failure.code, ErrorCode::ConnectionFailed);
    assert_eq!(should_retry, None);
}

#[test]
fn a_stream_that_stops_after_one_event_fails_the_call_and_closes_the_socket() {
    let server = ProviderServer::start([Response::stall(
        200,
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
        100_000,
    )
    .header("content-type", "text/event-stream")])
    .unwrap();
    let endpoint = endpoint(&server);
    let call = Box::new(Messages::new(endpoint).request(&request()));
    let (failure, should_retry) = failed(call);
    assert_eq!(failure.code, ErrorCode::ConnectionFailed);
    assert_eq!(should_retry, None);
    assert!(
        server.await_closed(1, fakes::MUST_SUCCEED_WITHIN),
        "the server saw the socket close"
    );
}
