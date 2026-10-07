//! The missing wire statuses (`docs/errors.md`, "A failed model call"): what
//! each protocol reports for 408, 409, 500 and 503, a refused connection,
//! and the `retry-after` and `x-should-retry` each carries. The statuses the
//! protocol test files already cover are not repeated here.

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
use provider::google_generative_ai::Gemini;
use provider::openai_completions::Completions;
use provider::openai_responses::Responses;

const DEADLINE: Duration = Duration::from_secs(10);

fn request() -> ModelRequest {
    ModelRequest {
        system_prompt: "You are terse.".into(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::OneHour,
        cache_key: "session_1".into(),
        previous_end: None,
        max_output_tokens: None,
        conversation: vec![Input::User {
            text: "hi".into(),
            images: Vec::new(),
        }],
        session_dir: std::path::PathBuf::new(),
    }
}

fn endpoint(provider: &str, server: &ProviderServer) -> Endpoint {
    Endpoint {
        provider: provider.into(),
        model: "m".into(),
        base_url: format!("{}/v1", server.url()),
        key: Some(contract::Secret::new("sk-secret".into())),
        direct: true,
        ..Endpoint::default()
    }
}

/// Every call in order under one [`DEADLINE`], each of which must fail:
/// one deadline however many calls the protocol needs.
fn failures(calls: Vec<Box<dyn ModelCall>>) -> Vec<(contract::shapes::Failure, Option<bool>)> {
    fakes::within("the calls to fail", DEADLINE, move || {
        calls
            .into_iter()
            .map(|call| {
                let mut deltas = Vec::new();
                let reply = call.run(&mut |d: Delta| deltas.push(d));
                let Err(CallError::Failed {
                    failure,
                    should_retry,
                }) = reply
                else {
                    panic!("expected a failure");
                };
                (failure, should_retry)
            })
            .collect()
    })
}

/// A port nothing listens on: connecting is refused.
fn closed_port() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}/v1")
}

fn refused_endpoint(provider: &str) -> Endpoint {
    Endpoint {
        provider: provider.into(),
        model: "m".into(),
        base_url: closed_port(),
        key: Some(contract::Secret::new("sk-secret".into())),
        direct: true,
        ..Endpoint::default()
    }
}

/// One protocol's calls: against the fake server, and against a closed port.
struct Protocol {
    name: &'static str,
    call: fn(&Endpoint) -> Box<dyn ModelCall>,
}

fn protocols() -> [Protocol; 4] {
    fn messages(endpoint: &Endpoint) -> Box<dyn ModelCall> {
        Box::new(Messages::new(endpoint.clone()).request(&request()))
    }
    fn responses(endpoint: &Endpoint) -> Box<dyn ModelCall> {
        Box::new(Responses::new(endpoint.clone()).request(&request()))
    }
    fn completions(endpoint: &Endpoint) -> Box<dyn ModelCall> {
        Box::new(Completions::new(endpoint.clone()).request(&request()))
    }
    fn gemini(endpoint: &Endpoint) -> Box<dyn ModelCall> {
        Box::new(Gemini::new(endpoint.clone()).request(&request()))
    }
    [
        Protocol {
            name: "anthropic",
            call: messages,
        },
        Protocol {
            name: "opencode",
            call: responses,
        },
        Protocol {
            name: "openrouter",
            call: completions,
        },
        Protocol {
            name: "gemini",
            call: gemini,
        },
    ]
}

#[test]
fn timeout_conflict_and_server_errors_are_provider_unavailable() {
    for protocol in protocols() {
        let server = ProviderServer::start(
            [408, 409, 500, 503].map(|status| Response::status(status, "{}")),
        )
        .unwrap();
        let endpoint = endpoint(protocol.name, &server);
        let calls: Vec<Box<dyn ModelCall>> = [408, 409, 500, 503]
            .iter()
            .map(|_| (protocol.call)(&endpoint))
            .collect();
        let got = failures(calls);
        for (index, status) in [408, 409, 500, 503].into_iter().enumerate() {
            let (failure, should_retry) = &got[index];
            assert_eq!(failure.code, ErrorCode::ProviderUnavailable, "{status}");
            assert_eq!(should_retry, &None, "{status}");
            assert_eq!(failure.retry_after_ms, None, "{status}");
            assert_eq!(
                failure.provider.as_ref().unwrap().status.unwrap(),
                status,
                "{status}"
            );
        }
    }
}

#[test]
fn a_503_carries_retry_after_and_a_500_carries_x_should_retry() {
    for protocol in protocols() {
        let server = ProviderServer::start([
            Response::status(503, "{}").header("retry-after", "7"),
            Response::status(500, "{}").header("x-should-retry", "true"),
        ])
        .unwrap();
        let endpoint = endpoint(protocol.name, &server);
        let calls: Vec<Box<dyn ModelCall>> =
            [0, 1].iter().map(|_| (protocol.call)(&endpoint)).collect();
        let got = failures(calls);
        let (failure, should_retry) = &got[0];
        assert_eq!(failure.code, ErrorCode::ProviderUnavailable);
        assert_eq!(should_retry, &None);
        assert_eq!(failure.retry_after_ms, Some(7000));
        let (failure, should_retry) = &got[1];
        assert_eq!(failure.code, ErrorCode::ProviderUnavailable);
        assert_eq!(should_retry, &Some(true));
        assert_eq!(failure.retry_after_ms, None);
    }
}

#[test]
fn a_429_with_retry_after_2_records_retry_after_ms_2000() {
    for protocol in protocols() {
        let server =
            ProviderServer::start([Response::status(429, "{}").header("retry-after", "2")])
                .unwrap();
        let endpoint = endpoint(protocol.name, &server);
        let got = failures(vec![(protocol.call)(&endpoint)]);
        let (failure, _) = &got[0];
        assert_eq!(failure.code, ErrorCode::RateLimited, "{}", protocol.name);
        let value = serde_json::to_value(failure).unwrap();
        assert_eq!(
            value.get("retry_after_ms"),
            Some(&serde_json::json!(2000)),
            "{}",
            protocol.name
        );
        assert!(value.get("retry_after").is_none(), "{}", protocol.name);
    }
}

#[test]
fn a_refused_connection_is_connection_failed() {
    for protocol in protocols() {
        let endpoint = refused_endpoint(protocol.name);
        let got = failures(vec![(protocol.call)(&endpoint)]);
        let (failure, should_retry) = &got[0];
        assert_eq!(failure.code, ErrorCode::ConnectionFailed);
        assert_eq!(should_retry, &None);
    }
}
