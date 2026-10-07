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

fn secrets(values: &[&str]) -> provider::redact::Secrets {
    let mut secrets = provider::redact::Secrets::default();
    for value in values {
        secrets.add(contract::Secret::new((*value).to_owned()));
    }
    secrets
}

#[test]
fn secrets_redact_exact_output() {
    let cases: &[(&[&str], &str, &str)] = &[
        (&["sk-1"], "nothing here", "nothing here"),
        (
            &["sk-1"],
            "sk-1 and sk-1 again",
            "[redacted] and [redacted] again",
        ),
        (&[""], "abc", "abc"),
        (&["sk-abc", "sk-abcdef"], "sk-abcdef", "[redacted]"),
        (&["sk-abcdef", "sk-abc"], "sk-abcdef", "[redacted]"),
        (&["red", "sk-1"], "sk-1", "[redacted]"),
        (&["sk-1"], "é sk-1 ü", "é [redacted] ü"),
    ];
    for (values, text, expected) in cases {
        assert_eq!(
            secrets(values).redact(text),
            *expected,
            "values {values:?} in {text:?}"
        );
    }
}

#[test]
fn add_header_splits_authorization() {
    let mut signed = provider::redact::Secrets::default();
    signed.add_header("authorization", "Bearer tok");
    assert_eq!(signed.redact("Bearer tok"), "[redacted]");
    assert_eq!(signed.redact("tok"), "[redacted]");

    let mut mixed = provider::redact::Secrets::default();
    mixed.add_header("Authorization", "Bearer tok");
    assert_eq!(mixed.redact("Bearer tok"), "[redacted]");
    assert_eq!(mixed.redact("tok"), "[redacted]");

    let mut other = provider::redact::Secrets::default();
    other.add_header("x-sig", "a b");
    assert_eq!(other.redact("a b"), "[redacted]");
    assert_eq!(other.redact("b"), "b");

    let mut bare = provider::redact::Secrets::default();
    bare.add_header("authorization", "Bearer ");
    assert_eq!(bare.redact("Bearer "), "[redacted]");
    assert_eq!(bare.redact("xyz"), "xyz");

    let mut nospace = provider::redact::Secrets::default();
    nospace.add_header("authorization", "tok");
    assert_eq!(nospace.redact("tok"), "[redacted]");
}

#[test]
fn secrets_debug_shows_no_added_value() {
    let mut secrets = provider::redact::Secrets::default();
    secrets.add(contract::Secret::new("sk-q9".to_owned()));
    secrets.add_header("authorization", "Bearer tok-q9");
    let shown = format!("{secrets:?}");
    assert!(!shown.contains("sk-q9"), "{shown:?}");
    assert!(!shown.contains("tok-q9"), "{shown:?}");
}
