//! The code a quota, billing, rate-limit or unknown-model reply maps to:
//! the documented quota shapes (`research/provider-errors/quota.md`, one
//! fixture each in `research/provider-errors/documented/`) and the recorded
//! replies (`research/retry-signals`, `research/provider-errors`).

use std::path::Path;

use contract::ErrorCode;
use contract::events::CacheLifetime;
use serde_json::Value;

use crate::Error;
use crate::redact::Secrets;

/// The recorded reply: its status, its body, and the `Retry-After` header
/// when it parses as seconds.
fn recorded(path: &str) -> Error {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../research")
        .join(path);
    let value: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    Error::Status {
        status: u16::try_from(value["status"].as_u64().unwrap()).unwrap(),
        body: value["body"].as_str().unwrap().to_owned(),
        retry_after: value
            .pointer("/headers/Retry-After")
            .and_then(Value::as_str)
            .and_then(|wait| wait.parse::<f64>().ok()),
        should_retry: None,
        url: crate::error::target(value["url"].as_str().unwrap_or_default()),
    }
}

/// The body a `documented/` status fixture holds.
fn fixture_body(path: &str) -> String {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../research")
        .join(path);
    let value: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    value["body"].as_str().unwrap().to_owned()
}

/// The stream bytes a `documented/` stream fixture holds in `raw_sse`.
fn stream_bytes(case: &str) -> Vec<u8> {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../research/provider-errors/documented")
        .join(case);
    let value: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    value["raw_sse"].as_str().unwrap().as_bytes().to_owned()
}

fn status(status: u16, body: &str, retry_after: Option<f64>) -> ErrorCode {
    Error::Status {
        status,
        body: body.to_owned(),
        retry_after,
        should_retry: None,
        url: String::new(),
    }
    .code()
}

fn anthropic_stream(case: &str) -> Error {
    crate::anthropic_messages_decode::decode(&stream_bytes(case)[..], &mut |_| {}).unwrap_err()
}

fn completions_stream(case: &str) -> Error {
    crate::openai_completions::decode(
        &stream_bytes(case)[..],
        &CacheLifetime::FiveMinutes,
        &mut |_| {},
    )
    .unwrap_err()
}

fn gemini_stream(case: &str) -> Error {
    crate::google_generative_ai_decode::decode(&stream_bytes(case)[..], &mut |_| {}).unwrap_err()
}

#[test]
fn a_recorded_unknown_model_is_model_not_found() {
    for path in [
        "retry-signals/raw/muse.unknown-model.json",
        "retry-signals/raw/openai.unknown-model.json",
        "retry-signals/raw/anthropic.unknown-model.json",
        "retry-signals/raw/gemini.unknown-model.json",
        "retry-signals/raw/openrouter.unknown-model.json",
        "provider-errors/raw/or-completions-haiku.unknown-model.json",
        "provider-errors/raw/muse-completions.unknown-model.json",
        "provider-errors/raw/muse-responses.unknown-model.json",
        "provider-errors/raw/muse-messages.unknown-model.json",
        "provider-errors/raw/anthropic-messages.unknown-model.json",
    ] {
        assert_eq!(recorded(path).code(), ErrorCode::ModelNotFound, "{path}");
    }
}

#[test]
fn a_recorded_wrong_path_is_invalid_request() {
    for path in [
        "provider-errors/raw/anthropic-messages.wrong-path.json",
        "provider-errors/raw/openai-completions.wrong-path.json",
        "provider-errors/raw/or-completions.wrong-path.json",
        "retry-signals/raw/anthropic.wrong-path.json",
        "retry-signals/raw/muse.wrong-path.json",
    ] {
        assert_eq!(recorded(path).code(), ErrorCode::InvalidRequest, "{path}");
    }
}

#[test]
fn a_wrong_path_failure_names_the_host_and_path_not_the_query() {
    let message = recorded("provider-errors/raw/openai-completions.wrong-path.json")
        .failure("openai", &Secrets::default())
        .message;
    assert_eq!(
        message,
        "openai answered HTTP 404 for api.openai.com/v1/chat/completionz. Check the base URL."
    );
    let wrong_key = Error::Status {
        status: 404,
        body: String::new(),
        retry_after: None,
        should_retry: None,
        url: crate::error::target("https://user:pw@host.test:8080/v1beta/m:gen?key=SECRET#frag"),
    };
    let message = wrong_key.failure("p", &Secrets::default()).message;
    assert_eq!(
        message,
        "p answered HTTP 404 for host.test:8080/v1beta/m:gen. Check the base URL."
    );
    assert_eq!(crate::error::target("host.test"), "host.test");
    assert_eq!(crate::error::target("http://host.test?q=1"), "host.test");
}

#[test]
fn a_404_that_names_the_model_keeps_the_provider_wording() {
    let message = recorded("provider-errors/raw/muse-messages.unknown-model.json")
        .failure("muse", &Secrets::default())
        .message;
    assert_eq!(message, "muse answered HTTP 404.");
    let other = Error::Status {
        status: 400,
        body: String::new(),
        retry_after: None,
        should_retry: None,
        url: "h/p".to_owned(),
    };
    assert_eq!(
        other.failure("p", &Secrets::default()).message,
        "p answered HTTP 400."
    );
}

#[test]
fn only_the_unknown_model_shapes_are_model_not_found() {
    assert_eq!(status(404, "{}", None), ErrorCode::InvalidRequest);
    assert_eq!(status(404, "", None), ErrorCode::InvalidRequest);
    assert_eq!(
        status(
            404,
            r#"{"error":{"type":"not_found_error","message":"Not found"}}"#,
            None
        ),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(
            404,
            r#"{"error":{"type":"not_found_error","message":"model: x"}}"#,
            None
        ),
        ErrorCode::ModelNotFound,
    );
    assert_eq!(status(400, "{}", None), ErrorCode::InvalidRequest);
    assert_eq!(
        status(400, r#"{"error":{"code":"model_not_found"}}"#, None),
        ErrorCode::ModelNotFound,
    );
    assert_eq!(
        status(
            400,
            r#"{"error":{"type":"not_found_error","message":"Model not found"}}"#,
            None
        ),
        ErrorCode::ModelNotFound,
    );
    for body in [
        r#"{"error":{"status":"NOT_FOUND","message":"models/x is not found"}}"#,
        r#"{"error":{"status":"NOT_FOUND","message":"the page is not found"}}"#,
        r#"{"error":{"status":"INVALID_ARGUMENT","message":"models/x is not found"}}"#,
    ] {
        let expected = if body.contains("NOT_FOUND\",\"message\":\"models/") {
            ErrorCode::ModelNotFound
        } else {
            ErrorCode::InvalidRequest
        };
        assert_eq!(status(404, body, None), expected, "{body}");
    }
    assert_eq!(
        status(400, r#"{"error":{"type":"not_found_error"}}"#, None),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(
            400,
            r#"{"error":{"message":"x is not a valid model ID"}}"#,
            None
        ),
        ErrorCode::ModelNotFound,
    );
    assert_eq!(
        status(
            400,
            r#"{"error":{"message":"x is a valid model ID"}}"#,
            None
        ),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(400, r#"{"error":{"type":"invalid_request_error"}}"#, None),
        ErrorCode::InvalidRequest,
    );
}

#[test]
fn a_documented_quota_or_billing_reply_is_quota_exceeded() {
    for case in [
        "provider-errors/documented/anthropic.billing-402.json",
        "provider-errors/documented/anthropic.spend-cap-429.json",
        "provider-errors/documented/anthropic.spend-limit-400.json",
        "provider-errors/documented/anthropic.workspace-spend-limit-400.json",
        "provider-errors/documented/openai.credit-balance-exhausted-429.json",
        "provider-errors/documented/openai.organization-spend-limit-429.json",
        "provider-errors/documented/openai.project-spend-limit-429.json",
        "provider-errors/documented/openai.organization-usage-limit-429.json",
        "provider-errors/documented/openai.insufficient-quota-429.json",
        "provider-errors/documented/openai.insufficient-quota-code-429.json",
        "provider-errors/documented/gemini.prepay-depleted-402.json",
        "provider-errors/documented/gemini.billing-disabled-403.json",
        "provider-errors/documented/gemini.resource-quota-exceeded-429.json",
        "provider-errors/documented/openrouter.insufficient-credits-402.json",
    ] {
        assert_eq!(recorded(case).code(), ErrorCode::QuotaExceeded, "{case}");
    }
}

#[test]
fn a_documented_rate_limit_stays_rate_limited() {
    for case in [
        "provider-errors/documented/openai.slow-down-429.json",
        "provider-errors/documented/gemini.quota-failure-429.json",
        "provider-errors/documented/gemini.rate-limit-exceeded-429.json",
        "provider-errors/documented/openrouter.in-flight-budget-402.json",
    ] {
        assert_eq!(recorded(case).code(), ErrorCode::RateLimited, "{case}");
    }
}

#[test]
fn an_in_flight_budget_402_without_retry_after_is_quota_exceeded() {
    let body = fixture_body("provider-errors/documented/openrouter.in-flight-budget-402.json");
    assert_eq!(status(402, &body, None), ErrorCode::QuotaExceeded);
    // A1 names no `limit_source`, so even a header leaves it a billing error.
    let billing = fixture_body("provider-errors/documented/anthropic.billing-402.json");
    assert_eq!(status(402, &billing, Some(5.0)), ErrorCode::QuotaExceeded);
}

#[test]
fn a_documented_stream_quota_failure_is_quota_exceeded() {
    let mut failures = Vec::new();
    failures.push(anthropic_stream("anthropic.billing-error-event.json"));
    failures.push(anthropic_stream("anthropic.spend-cap-event.json"));
    failures.push(completions_stream("openrouter.payment-required-chunk.json"));
    for case in [
        "gemini.payment-required-chunk.json",
        "gemini.billing-disabled-chunk.json",
        "gemini.resource-quota-exceeded-chunk.json",
    ] {
        failures.push(gemini_stream(case));
    }
    for err in &failures {
        assert_eq!(err.code(), ErrorCode::QuotaExceeded);
        let failure = err.failure("p", &Secrets::default());
        assert_eq!(failure.code, ErrorCode::QuotaExceeded);
        let provider = failure.provider.unwrap();
        assert_eq!(provider.status, Some(200));
    }
}

#[test]
fn a_bare_stream_resource_exhausted_stays_rate_limited() {
    let err = gemini_stream("gemini.resource-exhausted-chunk.json");
    assert_eq!(err.code(), ErrorCode::RateLimited);
}

#[test]
fn only_the_quota_shapes_are_quota_exceeded() {
    // One clause is enough: the code alone, the type alone, A2's shape
    // alone, and a quota `ErrorInfo` with no other clause.
    assert_eq!(
        status(
            429,
            r#"{"error":{"code":"credit_balance_exhausted"}}"#,
            None
        ),
        ErrorCode::QuotaExceeded,
    );
    assert_eq!(
        status(429, r#"{"error":{"type":"insufficient_quota"}}"#, None),
        ErrorCode::QuotaExceeded,
    );
    assert_eq!(
        status(429, r#"{"error":{"code":"insufficient_quota"}}"#, None),
        ErrorCode::QuotaExceeded,
    );
    assert_eq!(
        status(
            429,
            r#"{"error":{"type":"rate_limit_error","details":{"error_code":"enforced_spend_limit_reached"}}}"#,
            None
        ),
        ErrorCode::QuotaExceeded,
    );
    // Near misses keep their old codes.
    assert_eq!(
        status(
            400,
            r#"{"error":{"type":"invalid_request_error","message":"bad field"}}"#,
            None
        ),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(
            400,
            r#"{"error":{"type":"other","message":"You have reached your specified API usage limits"}}"#,
            None
        ),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(429, r#"{"error":{"type":"rate_limit_error"}}"#, None),
        ErrorCode::RateLimited,
    );
    assert_eq!(
        status(
            429,
            r#"{"error":{"type":"rate_limit_error","details":{"error_code":"other"}}}"#,
            None
        ),
        ErrorCode::RateLimited,
    );
}

#[test]
fn quota_wins_over_every_code_but_authentication() {
    let billing_disabled = r#"{"error":{"code":403,"message":"billing is disabled","status":"PERMISSION_DENIED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"BILLING_DISABLED"}]}}"#;
    assert_eq!(
        status(404, billing_disabled, None),
        ErrorCode::QuotaExceeded
    );
    assert_eq!(
        status(500, billing_disabled, None),
        ErrorCode::QuotaExceeded
    );
    assert_eq!(
        status(
            400,
            r#"{"error":{"type":"invalid_request_error","message":"You have reached your specified API usage limits for this model but the prompt is too long"}}"#,
            None
        ),
        ErrorCode::QuotaExceeded,
    );
    // Authentication still wins.
    assert_eq!(
        status(401, billing_disabled, None),
        ErrorCode::AuthenticationFailed
    );
    assert_eq!(
        status(
            400,
            r#"{"error":{"details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"API_KEY_INVALID"},{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"BILLING_DISABLED"}]}}"#,
            None
        ),
        ErrorCode::AuthenticationFailed,
    );
    // `insufficient_quota` is exact to 429: a 404 with it names no model.
    assert_eq!(
        status(404, r#"{"error":{"type":"insufficient_quota"}}"#, None),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(404, r#"{"error":{"code":"insufficient_quota"}}"#, None),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(
            400,
            r#"{"error":{"type":"insufficient_quota","message":"the prompt is too long"}}"#,
            None
        ),
        ErrorCode::ContextOverflow,
    );
}

#[test]
fn no_recorded_reply_is_quota_exceeded() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../research");
    let mut checked = 0;
    for project in std::fs::read_dir(&root).unwrap() {
        let project = project.unwrap();
        let raw = project.path().join("raw");
        let Ok(files) = std::fs::read_dir(&raw) else {
            continue;
        };
        for file in files {
            let file = file.unwrap();
            if file.path().extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let value: Value =
                serde_json::from_str(&std::fs::read_to_string(file.path()).unwrap()).unwrap();
            let Some(status) = value.get("status").and_then(Value::as_u64) else {
                continue;
            };
            if (200..300).contains(&status) || value.get("body").is_none_or(|b| !b.is_string()) {
                continue;
            }
            let body = value["body"].as_str().unwrap();
            let code = Error::Status {
                status: u16::try_from(status).unwrap(),
                body: body.to_owned(),
                retry_after: None,
                should_retry: None,
                url: String::new(),
            }
            .code();
            assert_ne!(code, ErrorCode::QuotaExceeded, "{}", file.path().display());
            checked += 1;
        }
    }
    assert!(checked >= 150, "only {checked} recorded replies checked");
}

#[test]
fn an_error_info_reason_needs_its_type() {
    // A `BILLING_DISABLED` reason under another `@type` is not a quota shape.
    assert_eq!(
        status(
            403,
            r#"{"error":{"details":[{"@type":"type.googleapis.com/google.rpc.QuotaFailure","reason":"BILLING_DISABLED"}]}}"#,
            None
        ),
        ErrorCode::InvalidRequest,
    );
}
