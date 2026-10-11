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

#[path = "support/harness.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use contract::ErrorCode;
use contract::signing::{SignRequest, Signer};
use fakes::{ProviderServer, Response};
use provider::Endpoint;
use provider::openai_responses::Responses;

use harness::{failed, protocols, request, run};

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

fn endpoint(provider: &str, server: &ProviderServer, signer: Arc<dyn Signer>) -> Endpoint {
    Endpoint {
        signer: Some(signer),
        ..harness::endpoint(provider, server)
    }
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
        let (_reply, _deltas) = run((protocol.call)(&endpoint, &request()));
        let (_reply, _deltas) = run((protocol.call)(&endpoint, &request()));
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
        let (failure, should_retry) = failed((protocol.call)(&endpoint, &request()));
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

/// Fails every request with its own signing error.
struct Failing(contract::signing::Error);

impl Signer for Failing {
    fn sign(&self, _: &SignRequest<'_>) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Err(self.0.clone())
    }
}

#[test]
fn a_failed_credential_or_sign_keeps_its_text_apart_from_fiber_s_sentence() {
    use contract::signing::Error as SignError;
    let cases: &[(&str, SignError, ErrorCode, &str, Option<&str>)] = &[
        (
            "failed sign",
            SignError::Failed("init.lua:3: boom".into()),
            ErrorCode::CredentialFailed,
            "acme's sign() failed.",
            Some("init.lua:3: boom"),
        ),
        (
            "failed credential",
            SignError::Credential {
                code: ErrorCode::CredentialFailed,
                message: "init.lua:3: boom".into(),
            },
            ErrorCode::CredentialFailed,
            "acme's credential() failed. Run `fiber login acme`.",
            Some("init.lua:3: boom"),
        ),
        (
            "quota credential",
            SignError::Credential {
                code: ErrorCode::QuotaExceeded,
                message: "init.lua:3: boom".into(),
            },
            ErrorCode::QuotaExceeded,
            "acme's credential() failed. Run `fiber login acme`.",
            Some("init.lua:3: boom"),
        ),
        (
            "rejected refresh",
            SignError::Credential {
                code: ErrorCode::AuthenticationFailed,
                message: "init.lua:3: boom".into(),
            },
            ErrorCode::AuthenticationFailed,
            "acme's credential() failed: the token endpoint rejected the refresh. Run `fiber login \
             acme`.",
            Some("init.lua:3: boom"),
        ),
        (
            "unattended login",
            SignError::Unattended {
                message: "init.lua:3: boom".into(),
            },
            ErrorCode::AuthenticationFailed,
            "acme's credential() failed: logging in needs a person, and nobody is attached. Run \
             `fiber login acme`.",
            Some("init.lua:3: boom"),
        ),
        (
            "unreachable refresh",
            SignError::Credential {
                code: ErrorCode::ConnectionFailed,
                message: "init.lua:3: boom".into(),
            },
            ErrorCode::ConnectionFailed,
            "acme's credential() failed: the token endpoint could not be reached.",
            Some("init.lua:3: boom"),
        ),
        (
            "unusable headers",
            SignError::NotHeaders("a header name that is not valid HTTP: \"x\"".into()),
            ErrorCode::CredentialFailed,
            "acme could not be signed: `sign()` returned a header name that is not valid HTTP: \"x\"",
            None,
        ),
    ];
    for (name, error, code, message, provider_message) in cases {
        let server = ProviderServer::start([Response::status(500, "{}")]).unwrap();
        let endpoint = endpoint("acme", &server, Arc::new(Failing(error.clone())));
        let (failure, should_retry) =
            failed(Box::new(Responses::new(endpoint).request(&request())));
        assert_eq!(failure.code, *code, "{name}");
        assert_eq!(failure.message.as_str(), *message, "{name}");
        assert_eq!(should_retry, Some(false), "{name}");
        match (provider_message, &failure.provider) {
            (None, None) => {}
            (Some(expected), Some(said)) => {
                assert_eq!(said.name.as_str(), "acme", "{name}");
                assert_eq!(said.status, None, "{name}");
                assert_eq!(said.message.as_str(), *expected, "{name}");
            }
            (expected, found) => panic!("{name}: expected provider {expected:?}, found {found:?}"),
        }
        assert!(
            server.requests().is_empty(),
            "{name}: an unsigned request is never sent"
        );
    }
}

/// Adds the account-id header a codex `credential()` returns.
struct AccountId;

impl Signer for AccountId {
    fn sign(&self, _: &SignRequest<'_>) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Ok(vec![(
            "chatgpt-account-id".to_owned(),
            "acct_secret".to_owned(),
        )])
    }
}

#[test]
fn a_credential_header_value_is_redacted_in_a_failure() {
    let server = ProviderServer::start([Response::status(500, "for acct_secret")]).unwrap();
    let endpoint = endpoint("codex", &server, Arc::new(AccountId));
    let (failure, _) = failed(Box::new(Responses::new(endpoint).request(&request())));
    assert_eq!(failure.code, ErrorCode::ProviderUnavailable);
    let said = failure.provider.unwrap();
    assert_eq!(said.status, Some(500));
    assert_eq!(said.message.as_str(), "for [redacted]");
}

/// Reports a credential no returned header carries, in the error it raises.
struct Leaking {
    credential: contract::Secret,
}

impl Signer for Leaking {
    fn sign(&self, _: &SignRequest<'_>) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Err(contract::signing::Error::Credential {
            code: ErrorCode::CredentialFailed,
            message: "saw acct_secret".into(),
        })
    }

    fn credentials(&self) -> Vec<contract::Secret> {
        vec![self.credential.clone()]
    }
}

#[test]
fn a_sign_error_echoing_a_listed_credential_is_redacted() {
    let server = ProviderServer::start([Response::status(500, "{}")]).unwrap();
    let endpoint = endpoint(
        "codex",
        &server,
        Arc::new(Leaking {
            credential: contract::Secret::new("acct_secret".into()),
        }),
    );
    let (failure, should_retry) = failed(Box::new(Responses::new(endpoint).request(&request())));
    assert_eq!(failure.code, ErrorCode::CredentialFailed);
    assert_eq!(should_retry, Some(false));
    let said = failure.provider.unwrap();
    assert_eq!(said.status, None);
    assert_eq!(said.message.as_str(), "saw [redacted]");
    assert!(
        server.requests().is_empty(),
        "an unsigned request is never sent"
    );
}
