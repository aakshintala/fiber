//! A 401 body echoing the key is logged with the key replaced
//! (`docs/errors.md`, "The shape").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code, helpers included"
)]

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::events::CacheLifetime;
use contract::provider::{CallError, Delta, Input, ModelCall, ModelRequest, Reply};
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
        conversation: vec![Input::User { text: "hi".into() }],
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

/// Runs `call` on its own thread, so a call that never returns fails the
/// test at the deadline instead of hanging it.
#[allow(
    clippy::result_large_err,
    reason = "the error is the model call's, returned unchanged"
)]
fn run(call: Box<dyn ModelCall>) -> Result<Reply, CallError> {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut deltas = Vec::new();
        let reply = call.run(&mut |d: Delta| deltas.push(d));
        done.send(reply).unwrap();
    });
    finished
        .recv_timeout(DEADLINE)
        .expect("waited for the call to return")
}

fn failed(call: Box<dyn ModelCall>) -> (contract::shapes::Failure, Option<bool>) {
    let Err(CallError::Failed {
        failure,
        should_retry,
    }) = run(call)
    else {
        panic!("expected a failure");
    };
    (failure, should_retry)
}

/// One protocol's calls: against the fake server.
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
fn a_401_that_echoes_the_key_stores_it_redacted() {
    for protocol in protocols() {
        let server = ProviderServer::start([Response::status(
            401,
            r#"{"error":{"message":"Incorrect API key provided: sk-secret"}}"#,
        )])
        .unwrap();
        let endpoint = endpoint(protocol.name, &server);
        let (failure, _) = failed((protocol.call)(&endpoint));
        assert_eq!(
            failure.code,
            ErrorCode::AuthenticationFailed,
            "{}",
            protocol.name
        );
        let said = failure.provider.as_ref().unwrap();
        assert_eq!(said.status, 401, "{}", protocol.name);
        assert_eq!(
            said.message.as_str(),
            "Incorrect API key provided: [redacted]",
            "{}",
            protocol.name
        );
    }
}
