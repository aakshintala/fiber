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
        assert_eq!(said.status.unwrap(), 401, "{}", protocol.name);
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

use std::sync::Arc;

use contract::signing::{SignRequest, Signer};
use serde_json::json;

fn endpoint_key(provider: &str, server: &ProviderServer, key: &str) -> Endpoint {
    Endpoint {
        provider: provider.into(),
        model: "m".into(),
        base_url: format!("{}/v1", server.url()),
        key: Some(contract::Secret::new(key.to_owned())),
        direct: true,
        ..Endpoint::default()
    }
}

/// Signs with an `authorization` scheme and a second header.
struct TwoHeaders;

impl Signer for TwoHeaders {
    fn sign(&self, _: &SignRequest<'_>) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Ok(vec![
            ("authorization".to_owned(), "Bearer tok-1".to_owned()),
            ("x-sig".to_owned(), "s-1".to_owned()),
        ])
    }
}

/// A 200 stream's error event echoing the key, and the redacted message it
/// must log, per protocol.
fn stream_error(name: &str) -> (Vec<u8>, &'static str) {
    match name {
        "anthropic" => (
            format!(
                "data: {}\n\n",
                json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded: sk-secret"}})
            )
            .into_bytes(),
            "Overloaded: [redacted]",
        ),
        "opencode" => (
            format!(
                "event: error\ndata: {}\n\n",
                json!({"type": "error", "code": "server_error", "message": "boom sk-secret"})
            )
            .into_bytes(),
            "boom [redacted]",
        ),
        "openrouter" => (
            format!("data: {}\n\n", json!({"error": {"message": "bad sk-secret"}}))
                .into_bytes(),
            "bad [redacted]",
        ),
        "gemini" => (
            format!(
                "data: {}\r\n\r\n",
                json!({"error": {"code": 503, "message": "Overloaded sk-secret", "status": "UNAVAILABLE"}})
            )
            .into_bytes(),
            "Overloaded [redacted]",
        ),
        _ => unreachable!("unknown protocol {name}"),
    }
}

#[test]
fn a_non_json_body_echoing_the_key_is_stored_redacted() {
    for protocol in protocols() {
        let server = ProviderServer::start([Response::status(400, "bad key sk-secret")]).unwrap();
        let endpoint = endpoint(protocol.name, &server);
        let (failure, _) = failed((protocol.call)(&endpoint));
        let said = failure.provider.as_ref().unwrap();
        assert_eq!(said.status.unwrap(), 400, "{}", protocol.name);
        assert_eq!(
            said.message.as_str(),
            "bad key [redacted]",
            "{}",
            protocol.name
        );
    }
}

#[test]
fn a_body_echoing_the_wrapped_key_redacts_the_key_only() {
    for protocol in protocols() {
        let server = ProviderServer::start([Response::status(
            401,
            r#"{"error":{"message":"Bearer sk-secret"}}"#,
        )])
        .unwrap();
        let endpoint = endpoint(protocol.name, &server);
        let (failure, _) = failed((protocol.call)(&endpoint));
        assert_eq!(
            failure.provider.as_ref().unwrap().message.as_str(),
            "Bearer [redacted]",
            "{}",
            protocol.name
        );
    }
}

#[test]
fn a_body_echoing_signed_header_values_is_stored_redacted() {
    for protocol in protocols() {
        let server = ProviderServer::start([Response::status(
            401,
            r#"{"error":{"message":"Bearer tok-1 then tok-1 then s-1"}}"#,
        )])
        .unwrap();
        let endpoint = Endpoint {
            provider: protocol.name.into(),
            model: "m".into(),
            base_url: format!("{}/v1", server.url()),
            signer: Some(Arc::new(TwoHeaders) as Arc<dyn Signer>),
            direct: true,
            ..Endpoint::default()
        };
        let (failure, _) = failed((protocol.call)(&endpoint));
        assert_eq!(
            failure.provider.as_ref().unwrap().message.as_str(),
            "[redacted] then [redacted] then [redacted]",
            "{}",
            protocol.name
        );
    }
}

/// Reports a credential no returned header carries: `sign()` replaced the
/// `authorization` header carrying it.
struct HiddenCredential;

impl Signer for HiddenCredential {
    fn sign(&self, _: &SignRequest<'_>) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Ok(vec![("x-sig".to_owned(), "s-1".to_owned())])
    }

    fn credentials(&self) -> Vec<contract::Secret> {
        vec![contract::Secret::new("hidden-tok".to_owned())]
    }
}

#[test]
fn a_body_echoing_a_replaced_credential_is_stored_redacted() {
    let server = ProviderServer::start([Response::status(
        401,
        r#"{"error":{"message":"bad hidden-tok"}}"#,
    )])
    .unwrap();
    let endpoint = Endpoint {
        provider: "openrouter".into(),
        model: "m".into(),
        base_url: format!("{}/v1", server.url()),
        signer: Some(Arc::new(HiddenCredential) as Arc<dyn Signer>),
        direct: true,
        ..Endpoint::default()
    };
    let (failure, _) = failed((protocols()[2].call)(&endpoint));
    assert_eq!(
        failure.provider.as_ref().unwrap().message.as_str(),
        "bad [redacted]"
    );
}

#[test]
fn an_error_inside_a_200_stream_is_stored_redacted() {
    for protocol in protocols() {
        let (body, expected) = stream_error(protocol.name);
        let server = ProviderServer::start([Response::stream(body)]).unwrap();
        let endpoint = endpoint(protocol.name, &server);
        let (failure, _) = failed((protocol.call)(&endpoint));
        let said = failure.provider.as_ref().unwrap();
        assert_eq!(said.status.unwrap(), 200, "{}", protocol.name);
        assert_eq!(said.message.as_str(), expected, "{}", protocol.name);
    }
}

#[test]
fn declared_and_fiber_built_header_values_stay() {
    for protocol in protocols() {
        let server = ProviderServer::start([Response::status(
            400,
            r#"{"error":{"message":"fiber sent application/json"}}"#,
        )])
        .unwrap();
        let mut endpoint = endpoint(protocol.name, &server);
        endpoint.headers = vec![("originator".to_owned(), "fiber".to_owned())];
        let (failure, _) = failed((protocol.call)(&endpoint));
        assert_eq!(
            failure.provider.as_ref().unwrap().message.as_str(),
            "fiber sent application/json",
            "{}",
            protocol.name
        );
    }
}

#[test]
fn a_json_escaped_key_is_decoded_then_redacted() {
    for protocol in protocols() {
        let server = ProviderServer::start([Response::status(
            400,
            r#"{"error":{"message":"bad sk-a\/b"}}"#,
        )])
        .unwrap();
        let endpoint = endpoint_key(protocol.name, &server, "sk-a/b");
        let (failure, _) = failed((protocol.call)(&endpoint));
        assert_eq!(
            failure.provider.as_ref().unwrap().message.as_str(),
            "bad [redacted]",
            "{}",
            protocol.name
        );
    }
}

#[test]
fn gemini_classification_reads_the_original_body() {
    let gemini = || {
        protocols()
            .into_iter()
            .find(|p| p.name == "gemini")
            .unwrap()
    };
    let protocol = gemini();
    let server = ProviderServer::start([Response::status(
        400,
        r#"{"error":{"message":"API key not valid: API_KEY_INVALID","details":[{"reason":"API_KEY_INVALID"}]}}"#,
    )])
    .unwrap();
    let endpoint = endpoint_key(protocol.name, &server, "API_KEY_INVALID");
    let (failure, _) = failed((protocol.call)(&endpoint));
    assert_eq!(failure.code, ErrorCode::AuthenticationFailed);
    assert_eq!(
        failure.provider.as_ref().unwrap().message.as_str(),
        "API key not valid: [redacted]"
    );

    let server = ProviderServer::start([Response::status(
        429,
        r#"{"error":{"message":"slow down 37s","details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"37s"}]}}"#,
    )])
    .unwrap();
    let endpoint = endpoint_key(protocol.name, &server, "37s");
    let (failure, _) = failed((protocol.call)(&endpoint));
    assert_eq!(failure.retry_after, Some(37.0));
    assert_eq!(
        failure.provider.as_ref().unwrap().message.as_str(),
        "slow down [redacted]"
    );
}

/// Fails `sign()` echoing the credential only `credentials()` reports.
struct EchoCredential;

impl Signer for EchoCredential {
    fn sign(&self, _: &SignRequest<'_>) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Err(contract::signing::Error::Failed("bad hidden-tok".into()))
    }

    fn credentials(&self) -> Vec<contract::Secret> {
        vec![contract::Secret::new("hidden-tok".to_owned())]
    }
}

#[test]
fn a_sign_error_echoing_the_credential_is_stored_redacted() {
    let server = ProviderServer::start([Response::status(500, "{}")]).unwrap();
    let endpoint = Endpoint {
        provider: "openrouter".into(),
        model: "m".into(),
        base_url: format!("{}/v1", server.url()),
        signer: Some(Arc::new(EchoCredential) as Arc<dyn Signer>),
        direct: true,
        ..Endpoint::default()
    };
    let (failure, _) = failed((protocols()[2].call)(&endpoint));
    assert_eq!(failure.code, ErrorCode::CredentialFailed);
    assert_eq!(failure.message.as_str(), "openrouter's sign() failed.");
    let said = failure.provider.as_ref().unwrap();
    assert_eq!(said.status, None);
    assert_eq!(said.message.as_str(), "bad [redacted]");
}

/// Fails `credential()` echoing the endpoint's key.
struct EchoKey;

impl Signer for EchoKey {
    fn sign(&self, _: &SignRequest<'_>) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Err(contract::signing::Error::Credential {
            code: ErrorCode::CredentialFailed,
            message: "bad sk-key".into(),
        })
    }
}

#[test]
fn a_credential_error_echoing_the_key_is_stored_redacted() {
    let server = ProviderServer::start([Response::status(500, "{}")]).unwrap();
    let mut endpoint = endpoint_key("openrouter", &server, "sk-key");
    endpoint.signer = Some(Arc::new(EchoKey) as Arc<dyn Signer>);
    let (failure, _) = failed((protocols()[2].call)(&endpoint));
    assert_eq!(failure.code, ErrorCode::CredentialFailed);
    let said = failure.provider.as_ref().unwrap();
    assert_eq!(said.status, None);
    assert_eq!(said.message.as_str(), "bad [redacted]");
}

/// Returns an unusable header name echoing the credential only
/// `credentials()` reports.
struct BadName;

impl Signer for BadName {
    fn sign(&self, _: &SignRequest<'_>) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Ok(vec![("hidden-tok\n".to_owned(), "x".to_owned())])
    }

    fn credentials(&self) -> Vec<contract::Secret> {
        vec![contract::Secret::new("hidden-tok".to_owned())]
    }
}

#[test]
fn an_unusable_header_name_holding_the_credential_is_redacted() {
    let server = ProviderServer::start([Response::status(500, "{}")]).unwrap();
    let endpoint = Endpoint {
        provider: "openrouter".into(),
        model: "m".into(),
        base_url: format!("{}/v1", server.url()),
        signer: Some(Arc::new(BadName) as Arc<dyn Signer>),
        direct: true,
        ..Endpoint::default()
    };
    let (failure, _) = failed((protocols()[2].call)(&endpoint));
    assert_eq!(failure.code, ErrorCode::CredentialFailed);
    assert!(
        !failure.message.contains("hidden-tok"),
        "{}",
        failure.message
    );
    assert!(
        failure.message.contains("[redacted]"),
        "{}",
        failure.message
    );
    assert_eq!(failure.provider, None);
}
