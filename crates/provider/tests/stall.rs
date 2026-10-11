//! A stalled model call through the provider crate's public API: a server
//! that never answers, and a stream that stops after one event, both end
//! the call as `connection_failed` under a short socket limit.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code, helpers included"
)]

#[path = "support/harness.rs"]
mod harness;

use std::time::Duration;

use contract::ErrorCode;
use fakes::{ProviderServer, Response};
use provider::Endpoint;
use provider::anthropic_messages::Messages;

use harness::{failed, request};

/// The socket limits a stalled call runs under: one second per address to
/// connect, 200 ms without a byte.
fn stall_limits() -> net::Limits {
    net::Limits::new(Duration::from_secs(1), Duration::from_millis(200)).unwrap()
}

fn endpoint(server: &ProviderServer) -> Endpoint {
    Endpoint {
        limits: stall_limits(),
        ..harness::endpoint("acme", server)
    }
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
