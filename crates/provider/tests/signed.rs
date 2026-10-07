//! A signed request on every protocol (`docs/model-routing.md`, "Signing a
//! request"): a call built from an endpoint with a signer sends the signer's
//! headers on every send, a retry included, and a signer that fails reports
//! its own code without a retry.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code, helpers included"
)]

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::events::CacheLifetime;
use contract::provider::{CallError, Delta, Input, ModelCall, ModelRequest};
use contract::signing::{SignRequest, Signer};
use fakes::{ProviderServer, Response};
use provider::Endpoint;
use provider::anthropic_messages::Messages;
use provider::google_generative_ai::Gemini;
use provider::openai_completions::Completions;
use provider::openai_responses::Responses;

const DEADLINE: Duration = Duration::from_secs(10);

/// Counts the requests it signed and adds one header to each.
struct Counting(Mutex<usize>);

impl Signer for Counting {
    fn sign(&self, _: &SignRequest<'_>) -> Result<Vec<(String, String)>, contract::signing::Error> {
        *self.0.lock().unwrap() += 1;
        Ok(vec![("x-signature".to_owned(), "sig".to_owned())])
    }
}

/// Refuses with the signer's own code.
struct Refuses {
    code: ErrorCode,
}

impl Signer for Refuses {
    fn sign(&self, _: &SignRequest<'_>) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Err(contract::signing::Error::Credential {
            code: self.code.clone(),
            message: "the token refresh was rejected".into(),
        })
    }
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
        max_output_tokens: None,
        conversation: vec![Input::User { text: "hi".into() }],
        session_dir: std::path::PathBuf::new(),
    }
}

fn endpoint(provider: &str, server: &ProviderServer, signer: Arc<dyn Signer>) -> Endpoint {
    Endpoint {
        provider: provider.into(),
        model: "m".into(),
        base_url: format!("{}/v1", server.url()),
        key: Some(contract::Secret::new("sk-secret".into())),
        signer: Some(signer),
        direct: true,
        ..Endpoint::default()
    }
}

/// One protocol's calls from an endpoint.
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

/// Runs `call` on its own thread, so a call that never returns fails the
/// test at the deadline instead of hanging it.
fn run(call: Box<dyn ModelCall>) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut deltas = Vec::new();
        let _reply = call.run(&mut |d: Delta| deltas.push(d));
        done.send(()).unwrap();
    });
    finished
        .recv_timeout(DEADLINE)
        .expect("waited for the call to return");
}

fn failed(call: Box<dyn ModelCall>) -> (contract::shapes::Failure, Option<bool>) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut deltas = Vec::new();
        done.send(call.run(&mut |d: Delta| deltas.push(d))).unwrap();
    });
    let reply = finished
        .recv_timeout(DEADLINE)
        .expect("waited for the call to return");
    let Err(CallError::Failed {
        failure,
        should_retry,
    }) = reply
    else {
        panic!("expected a failure");
    };
    (failure, should_retry)
}

#[test]
fn a_signed_call_sends_the_signers_headers_on_every_send() {
    for protocol in protocols() {
        let server =
            ProviderServer::start([Response::status(500, "{}"), Response::status(500, "{}")])
                .unwrap();
        let signer: Arc<Counting> = Arc::new(Counting(Mutex::new(0)));
        let endpoint = endpoint(
            protocol.name,
            &server,
            Arc::clone(&signer) as Arc<dyn Signer>,
        );
        // Two builds, one send each: a retry signs again.
        run((protocol.call)(&endpoint));
        run((protocol.call)(&endpoint));
        assert_eq!(*signer.0.lock().unwrap(), 2, "{}", protocol.name);
        let requests = server.requests();
        assert_eq!(requests.len(), 2, "{}", protocol.name);
        for request in &requests {
            assert_eq!(
                request.header("x-signature"),
                Some("sig"),
                "{}",
                protocol.name
            );
        }
    }
}

#[test]
fn a_signer_with_its_own_code_fails_with_it_and_is_not_retried() {
    for protocol in protocols() {
        let server = ProviderServer::start([Response::status(500, "{}")]).unwrap();
        let endpoint = endpoint(
            protocol.name,
            &server,
            Arc::new(Refuses {
                code: ErrorCode::AuthenticationFailed,
            }),
        );
        let (failure, should_retry) = failed((protocol.call)(&endpoint));
        assert_eq!(
            failure.code,
            ErrorCode::AuthenticationFailed,
            "{}",
            protocol.name
        );
        assert_eq!(should_retry, Some(false), "{}", protocol.name);
        assert!(
            server.requests().is_empty(),
            "{}: an unsigned request is never sent",
            protocol.name
        );
    }
}

#[test]
fn a_failed_sign_is_credential_failed_and_not_retried() {
    struct Fails;
    impl Signer for Fails {
        fn sign(
            &self,
            _: &SignRequest<'_>,
        ) -> Result<Vec<(String, String)>, contract::signing::Error> {
            Err(contract::signing::Error::Failed("no key".into()))
        }
    }
    let server = ProviderServer::start([Response::status(500, "{}")]).unwrap();
    let endpoint = endpoint("opencode", &server, Arc::new(Fails));
    let (failure, should_retry) = failed(Box::new(Responses::new(endpoint).request(&request())));
    assert_eq!(failure.code, ErrorCode::CredentialFailed);
    assert_eq!(should_retry, Some(false));
    assert!(server.requests().is_empty());
}

#[test]
fn a_credential_error_displays_its_message_alone() {
    let error = contract::signing::Error::Credential {
        code: ErrorCode::AuthenticationFailed,
        message: "the token refresh was rejected".into(),
    };
    assert_eq!(error.to_string(), "the token refresh was rejected");
}

#[test]
fn endpoint_debug_names_the_signer_without_reaching_into_it() {
    let unsigned = Endpoint {
        provider: "p".into(),
        model: "m".into(),
        ..Endpoint::default()
    };
    let text = format!("{unsigned:?}");
    assert!(text.contains("signer: None"), "{text:?}");
    let signer: Arc<Counting> = Arc::new(Counting(Mutex::new(0)));
    let signed = Endpoint {
        provider: "p".into(),
        model: "m".into(),
        signer: Some(Arc::clone(&signer) as Arc<dyn Signer>),
        ..Endpoint::default()
    };
    let text = format!("{signed:?}");
    assert!(text.contains("signer: Some(\"Signer\")"), "{text:?}");
}
