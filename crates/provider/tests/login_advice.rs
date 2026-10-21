//! Fiber's sentences about a provider's credential advise `fiber login` only
//! where it can fix the credential (`docs/errors.md`, "The shape"): the
//! endpoint's `login_advice` decides, for a failed `credential()` and for an
//! HTTP 401 or 403 alike, and the provider's own words stay in
//! `provider.message`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code, helpers included"
)]

#[path = "support/harness.rs"]
mod harness;

use std::sync::Arc;

use contract::ErrorCode;
use contract::signing::{SignRequest, Signer};
use fakes::{ProviderServer, Response};
use provider::openai_responses::Responses;
use provider::{Endpoint, LoginAdvice};

use harness::{failed, request};

/// Fails every request with its own signing error.
struct Failing(contract::signing::Error);

impl Signer for Failing {
    fn sign(&self, _: &SignRequest<'_>) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Err(self.0.clone())
    }
}

/// What fails a case: the signer, before any request is sent, or the
/// server's answer.
enum Cause {
    Sign(contract::signing::Error),
    Answer(Response),
}

/// The extension's own first line for a failed `credential()`.
const REMEDY: &str = "acme credential: set ACME_TOKEN or name a profile";

/// The provider's own words in a 401 or 403 body.
const REFUSED: &str = "The security token included in the request is invalid.";

fn credential(code: ErrorCode) -> Cause {
    Cause::Sign(contract::signing::Error::Credential {
        code,
        message: REMEDY.into(),
    })
}

fn answer(status: u16) -> Cause {
    Cause::Answer(Response::status(
        status,
        format!(r#"{{"message":"{REFUSED}"}}"#),
    ))
}

#[test]
fn a_credential_sentence_advises_fiber_login_only_where_it_can_fix_the_credential() {
    use LoginAdvice::{Advised, Omitted};
    let cases: Vec<(&str, LoginAdvice, Cause, ErrorCode, &str, &str)> = vec![
        (
            "failed credential, advised",
            Advised,
            credential(ErrorCode::CredentialFailed),
            ErrorCode::CredentialFailed,
            "acme's credential() failed. Run `fiber login acme`.",
            REMEDY,
        ),
        (
            "failed credential, omitted",
            Omitted,
            credential(ErrorCode::CredentialFailed),
            ErrorCode::CredentialFailed,
            "acme's credential() failed.",
            REMEDY,
        ),
        (
            "rejected refresh, advised",
            Advised,
            credential(ErrorCode::AuthenticationFailed),
            ErrorCode::AuthenticationFailed,
            "acme's credential() failed: the token endpoint rejected the refresh. Run `fiber \
             login acme`.",
            REMEDY,
        ),
        (
            "rejected refresh, omitted",
            Omitted,
            credential(ErrorCode::AuthenticationFailed),
            ErrorCode::AuthenticationFailed,
            "acme's credential() failed: the token endpoint rejected the refresh.",
            REMEDY,
        ),
        (
            "unattended login, omitted",
            Omitted,
            Cause::Sign(contract::signing::Error::Unattended {
                message: REMEDY.into(),
            }),
            ErrorCode::AuthenticationFailed,
            "acme's credential() failed: logging in needs a person, and nobody is attached.",
            REMEDY,
        ),
        (
            "login timed out, omitted",
            Omitted,
            Cause::Sign(contract::signing::Error::LoginTimedOut {
                message: REMEDY.into(),
            }),
            ErrorCode::AuthenticationFailed,
            "acme's credential() failed: the login did not finish in time.",
            REMEDY,
        ),
        (
            "401, advised",
            Advised,
            answer(401),
            ErrorCode::AuthenticationFailed,
            "acme rejected the credential (HTTP 401). Check the key it is configured with, or \
             log in again with `fiber login acme`.",
            REFUSED,
        ),
        (
            "401, omitted",
            Omitted,
            answer(401),
            ErrorCode::AuthenticationFailed,
            "acme rejected the credential (HTTP 401).",
            REFUSED,
        ),
        (
            "AWS 403, omitted",
            Omitted,
            Cause::Answer(
                Response::status(403, format!(r#"{{"Message":"{REFUSED}"}}"#)).header(
                    "x-amzn-ErrorType",
                    "UnrecognizedClientException:http://internal.amazon.com/coral/com.amazon.coral.service/",
                ),
            ),
            ErrorCode::AuthenticationFailed,
            "acme rejected the credential (HTTP 403).",
            REFUSED,
        ),
    ];
    for (name, login_advice, cause, code, message, said) in cases {
        let (server, signer) = match cause {
            Cause::Sign(error) => (
                ProviderServer::start([Response::status(500, "{}")]).unwrap(),
                Some(Arc::new(Failing(error)) as Arc<dyn Signer>),
            ),
            Cause::Answer(response) => (ProviderServer::start([response]).unwrap(), None),
        };
        let endpoint = Endpoint {
            signer,
            login_advice,
            ..harness::endpoint("acme", &server)
        };
        let (failure, _) = failed(Box::new(Responses::new(endpoint).request(&request())));
        assert_eq!(failure.code, code, "{name}");
        assert_eq!(failure.message.as_str(), message, "{name}");
        let provider = failure.provider.unwrap();
        assert!(provider.message.contains(said), "{name}: {provider:?}");
    }
}

#[test]
fn an_endpoint_advises_fiber_login_unless_told_otherwise() {
    // A key provider's endpoint is the default: `fiber login` stores the key
    // it reads.
    assert_eq!(Endpoint::default().login_advice, LoginAdvice::Advised);
}
