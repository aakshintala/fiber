//! Why a model call failed, and the code each case reports
//! (`docs/errors.md`, "A failed model call").

use contract::ErrorCode;
use contract::shapes::{Failure, ProviderFailure};
use serde_json::Value;

use crate::google_generative_ai::str_at;
use crate::redact::Secrets;

/// A failed model call. Each message is a phrase that follows the
/// provider's name, as [`Error::failure`] writes it.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Error {
    /// The connection could not be made, or dropped.
    #[error("could not be reached: {0}.")]
    Connection(String),
    /// The provider answered with a status other than 2xx.
    #[error("answered HTTP {status}.")]
    Status {
        /// The HTTP status.
        status: u16,
        /// The response body, as text.
        body: String,
        /// The seconds a `retry-after` header asked Fiber to wait, or the
        /// usage limit's reset measured from the reply's `Date` header.
        retry_after: Option<f64>,
        /// The `x-should-retry` header, when the response carried one.
        should_retry: Option<bool>,
        /// The host and path the request went to, from [`target`].
        url: String,
    },
    /// The stream ended before its terminal event, or carried something
    /// Fiber could not read.
    #[error("sent a reply stream that ended early: {0}.")]
    StreamIncomplete(String),
    /// The stream ended with a failure the provider reported in it, such as
    /// `response.failed`.
    #[error("failed the reply.")]
    ReplyFailed {
        /// The provider's own code for the failure, when it sent one.
        code: Option<String>,
        /// The provider's own message.
        message: String,
    },
    /// A stop reason the protocol does not map.
    #[error("ended the reply with a stop reason Fiber does not know: `{0}`.")]
    UnknownStopReason(String),
    /// The model ran out of context window: Anthropic's
    /// `model_context_window_exceeded` stop reason.
    #[error("ran out of the model's context window: {0}.")]
    ContextOverflow(String),
    /// The provider declined to answer on policy grounds.
    #[error("declined to answer: {0}.")]
    Refused(String),
    /// The provider reported a quota, billing or subscription limit, as an
    /// HTTP status or inside the reply stream (`docs/errors.md`,
    /// "Recognising a quota or billing error").
    #[error("reported a quota or billing limit in the reply stream.")]
    QuotaExceeded(String),
    /// The request could not be signed, so it was never sent.
    #[error("could not be signed: {0}")]
    Sign(contract::signing::Error),
}

impl Error {
    /// The stable code a consumer switches on.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Connection(_) => ErrorCode::ConnectionFailed,
            Self::Status {
                status,
                body,
                retry_after,
                ..
            } => status_code(*status, *retry_after, body),
            Self::StreamIncomplete(_) => ErrorCode::StreamIncomplete,
            Self::ReplyFailed { code, message } => reply_failed_code(code.as_deref(), message),
            Self::UnknownStopReason(_) => ErrorCode::UnknownStopReason,
            Self::QuotaExceeded(_) => ErrorCode::QuotaExceeded,
            Self::ContextOverflow(_) => ErrorCode::ContextOverflow,
            Self::Refused(_) => ErrorCode::Refused,
            // A credential failure carries its own code: a failed `sign()`
            // is `credential_failed`, a failed refresh keeps its own.
            Self::Sign(contract::signing::Error::Credential { code, .. }) => code.clone(),
            Self::Sign(contract::signing::Error::Unattended { .. }) => {
                ErrorCode::AuthenticationFailed
            }
            Self::Sign(_) => ErrorCode::CredentialFailed,
        }
    }

    /// The `x-should-retry` header of the response that failed, when it
    /// carried one.
    pub fn should_retry(&self) -> Option<bool> {
        match self {
            Self::Status { should_retry, .. } => *should_retry,
            // A failed signature is not retried.
            Self::Sign(_) => Some(false),
            Self::Connection(_)
            | Self::StreamIncomplete(_)
            | Self::ReplyFailed { .. }
            | Self::UnknownStopReason(_)
            | Self::ContextOverflow(_)
            | Self::QuotaExceeded(_)
            | Self::Refused(_) => None,
        }
    }

    /// The failure as a failed model call records it, naming `provider`.
    /// The stored provider message holds no secret value: every value in
    /// `secrets` is replaced with `[redacted]`; the code and retry advice
    /// read the original body unchanged.
    pub fn failure(&self, provider: &str, secrets: &Secrets) -> Failure {
        let code = self.code();
        let (retry_after_ms, said) = match self {
            Self::Status {
                status,
                body,
                retry_after,
                ..
            } => (
                (*retry_after).and_then(wait_ms),
                Some((Some(*status), secrets.redact(&body_message(body)))),
            ),
            Self::ReplyFailed { message, .. } => (None, Some((Some(200), secrets.redact(message)))),
            Self::QuotaExceeded(message) => (None, Some((Some(200), secrets.redact(message)))),
            Self::Sign(
                contract::signing::Error::Failed(text)
                | contract::signing::Error::Credential { message: text, .. }
                | contract::signing::Error::Unattended { message: text },
            ) => (None, Some((None, secrets.redact(text)))),
            Self::Connection(_)
            | Self::StreamIncomplete(_)
            | Self::UnknownStopReason(_)
            | Self::ContextOverflow(_)
            | Self::Refused(_)
            | Self::Sign(_) => (None, None),
        };
        let message = match (self, &code) {
            (Self::Sign(sign), _) => sign_sentence(provider, sign, secrets),
            // A usage-limit reply names its reset time; any other quota or
            // billing shape keeps the provider's wording.
            (Self::Status { body, .. }, ErrorCode::QuotaExceeded) if usage_limit_body(body) => {
                match resets_at_of(body) {
                    Some(resets) => format!(
                        "{provider} reached its usage limit; it resets at {}.",
                        reset_time(resets)
                    ),
                    None => format!("{provider} reached its usage limit."),
                }
            }
            (Self::Status { status, .. }, ErrorCode::AuthenticationFailed) => format!(
                "{provider} rejected the credential (HTTP {status}). Check the key it is \
                 configured with, or log in again with `fiber login {provider}`."
            ),
            // A 404 that does not name the model is often a wrong base URL.
            (
                Self::Status {
                    status: 404, url, ..
                },
                ErrorCode::InvalidRequest,
            ) => {
                format!("{provider} answered HTTP 404 for {url}. Check the base URL.")
            }
            _ => format!("{provider} {self}"),
        };
        Failure {
            code,
            message,
            retry_after_ms,
            provider: said.map(|(status, message)| ProviderFailure {
                name: provider.to_owned(),
                status,
                message,
            }),
        }
    }
}

/// The asked wait in milliseconds, rounded up and saturating at
/// `u64::MAX`; absent when `seconds` is not finite or is negative.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "seconds is finite and non-negative, clamped to u64::MAX milliseconds"
)]
fn wait_ms(seconds: f64) -> Option<u64> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some((seconds * 1000.0).ceil().clamp(0.0, u64::MAX as f64) as u64)
}

/// Fiber's own sentence for a signing failure (`docs/errors.md`, "The
/// shape"): it names the provider and the function, never the signer's
/// text, which stays in `provider.message`, redacted.
fn sign_sentence(provider: &str, error: &contract::signing::Error, secrets: &Secrets) -> String {
    use contract::signing::Error as Sign;
    match error {
        Sign::Failed(_) => format!("{provider}'s sign() failed."),
        Sign::Unattended { .. } => format!(
            "{provider}'s credential() failed: logging in needs a person, and nobody is attached. \
             Run `fiber login {provider}`."
        ),
        Sign::Credential { code, .. } if *code == ErrorCode::AuthenticationFailed => format!(
            "{provider}'s credential() failed: the token endpoint rejected the refresh. Run \
             `fiber login {provider}`."
        ),
        Sign::Credential { code, .. } if *code == ErrorCode::ConnectionFailed => {
            format!("{provider}'s credential() failed: the token endpoint could not be reached.")
        }
        Sign::Credential { .. } => {
            format!("{provider}'s credential() failed. Run `fiber login {provider}`.")
        }
        // Today's wording, redacted: the interpolated header name can carry
        // a known secret.
        Sign::NotHeaders(_) => secrets.redact(&format!("{provider} could not be signed: {error}")),
    }
}

/// The code for an HTTP status, reading the body where the status alone
/// cannot classify. A ChatGPT/codex usage-limit body wins over everything,
/// on any status (`docs/model-routing.md`, "Protocols and providers");
/// authentication wins over every other shape; a documented quota or
/// billing shape wins over every other code (`docs/errors.md`,
/// "Recognising a quota or billing error").
fn status_code(status: u16, retry_after: Option<f64>, body: &str) -> ErrorCode {
    match status {
        _ if usage_limit_body(body) => ErrorCode::QuotaExceeded,
        401 => ErrorCode::AuthenticationFailed,
        // Gemini answers a bad key with 400 `API_KEY_INVALID`
        // (`research/google-generative-ai-probe`, `raw/auth-badheader.json`).
        400 if has_reason(body, "API_KEY_INVALID") => ErrorCode::AuthenticationFailed,
        // OpenRouter's in-flight budget 402 is a wait-and-retry case only
        // with a `Retry-After` in seconds (`research/provider-errors/quota.md`, N6).
        402 if in_flight_budget(body) && retry_after.is_some() => ErrorCode::RateLimited,
        402 => ErrorCode::QuotaExceeded,
        _ if quota_status(status, body) => ErrorCode::QuotaExceeded,
        429 => ErrorCode::RateLimited,
        408 | 409 | 500..=599 => ErrorCode::ProviderUnavailable,
        _ if unknown_model(body) => ErrorCode::ModelNotFound,
        _ if overflow(body_code(body).as_deref(), &body_message(body)) => {
            ErrorCode::ContextOverflow
        }
        _ => ErrorCode::InvalidRequest,
    }
}

/// Whether an error object is a ChatGPT/codex usage-limit shape: its
/// `error.code` or `error.type` is `usage_limit_reached` or
/// `usage_not_included`, whatever the HTTP status
/// (`docs/model-routing.md`, "Protocols and providers"). Only the codes
/// are matched.
pub(crate) fn usage_limit(error: &Value) -> bool {
    matches!(
        error.get("code").and_then(Value::as_str),
        Some("usage_limit_reached" | "usage_not_included")
    ) || matches!(
        error.get("type").and_then(Value::as_str),
        Some("usage_limit_reached" | "usage_not_included")
    )
}

/// Whether a status body carries the usage-limit shape.
fn usage_limit_body(body: &str) -> bool {
    let value: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => return false,
    };
    value.get("error").is_some_and(usage_limit)
}

/// A usage-limit body's integer `resets_at` (Unix seconds), when it has one.
fn resets_at_of(body: &str) -> Option<u64> {
    let value: Value = serde_json::from_str(body).ok()?;
    value.get("error")?.get("resets_at")?.as_u64()
}

/// `unix` seconds as `YYYY-MM-DD HH:MM UTC`, from the timestamp alone; no
/// clock is read (`docs/model-routing.md`, "Protocols and providers").
fn reset_time(unix: u64) -> String {
    let days = unix / 86_400;
    let rest = unix % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rest / 3_600,
        (rest % 3_600) / 60
    )
}

/// The civil date of `days` days after the epoch, as year, month and day
/// (Howard Hinnant's algorithm: shift to the civil era starting March).
/// The inputs fit `u64` many times over: the largest day count below
/// `u64::MAX` seconds still leaves every product far from overflowing.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let ordinal = shifted - era * 146_097;
    let year_of_era = (ordinal - ordinal / 1_460 + ordinal / 36_524 - ordinal / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = ordinal - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Whether a status body carries a documented quota or billing shape
/// (`research/provider-errors/quota.md`). Each clause matches its shape
/// exactly, including the documented status.
fn quota_status(status: u16, body: &str) -> bool {
    let value: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let error = value.pointer("/error").unwrap_or(&Value::Null);
    match status {
        429 => {
            matches!(
                error.get("code").and_then(Value::as_str),
                Some(
                    "credit_balance_exhausted"
                        | "organization_spend_limit_exceeded"
                        | "project_spend_limit_exceeded"
                        | "organization_usage_limit_exceeded"
                        | "insufficient_quota"
                )
            ) || error.get("type").and_then(Value::as_str) == Some("insufficient_quota")
                || anthropic_spend_cap(error)
                || google_quota(error)
        }
        400 => {
            (error.get("type").and_then(Value::as_str) == Some("invalid_request_error") && {
                let message = error.get("message").and_then(Value::as_str).unwrap_or("");
                message.starts_with("You have reached your specified API usage limits")
                    || message
                        .starts_with("You have reached your specified workspace API usage limits")
            }) || google_quota(error)
        }
        _ => google_quota(error),
    }
}

/// Whether an Anthropic error object is the spend-cap shape: a 429
/// `rate_limit_error` the `enforced_spend_limit_reached` detail tells apart
/// from a rate limit (`research/provider-errors/quota.md`, A2).
fn anthropic_spend_cap(error: &Value) -> bool {
    error.get("type").and_then(Value::as_str) == Some("rate_limit_error")
        && error.pointer("/details/error_code").and_then(Value::as_str)
            == Some("enforced_spend_limit_reached")
}

/// Whether an error object carries a Google quota `ErrorInfo`: a `details[]`
/// entry whose `@type` is `google.rpc.ErrorInfo` and whose reason is
/// `BILLING_DISABLED` or `RESOURCE_QUOTA_EXCEEDED`
/// (`research/provider-errors/quota.md`, G2 and G3).
fn google_quota(error: &Value) -> bool {
    error
        .get("details")
        .and_then(Value::as_array)
        .is_some_and(|details| {
            details.iter().any(|entry| {
                error_info_reason(entry, "BILLING_DISABLED")
                    || error_info_reason(entry, "RESOURCE_QUOTA_EXCEEDED")
            })
        })
}

/// Whether a 402 body is OpenRouter's in-flight budget case
/// (`research/provider-errors/quota.md`, N6).
fn in_flight_budget(body: &str) -> bool {
    let value: Option<Value> = serde_json::from_str(body).ok();
    value
        .as_ref()
        .and_then(|v| v.pointer("/error/metadata/limit_source"))
        .and_then(Value::as_str)
        == Some("openrouter_in_flight_budget")
}

/// Whether an in-stream error object is a documented quota or billing shape:
/// an Anthropic `billing_error` or spend-cap event, a numeric 402 code, or a
/// Google quota `ErrorInfo` (`research/provider-errors/quota.md`, S1-S4).
fn stream_quota(error: &Value) -> bool {
    error.get("type").and_then(Value::as_str) == Some("billing_error")
        || anthropic_spend_cap(error)
        || error.get("code").and_then(Value::as_u64) == Some(402)
        || google_quota(error)
}

/// The error for a failure inside a 200 stream: `QuotaExceeded` for a
/// documented quota or billing shape, else the provider's own `ReplyFailed`.
/// Each decoder passes the code it computes today, so `reply_failed_code`
/// is unchanged.
pub(crate) fn stream_failure(error: &Value, code: Option<String>) -> Error {
    let message = str_at(error, "message").to_owned();
    if stream_quota(error) {
        return Error::QuotaExceeded(message);
    }
    Error::ReplyFailed { code, message }
}

/// The host and path of `url`: no scheme, credentials, query or fragment, so
/// a failure can name where a request went without leaking a key in the
/// query string.
pub(crate) fn target(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (authority, path) = rest.find('/').map_or((rest, ""), |i| rest.split_at(i));
    let host = authority.rsplit('@').next().unwrap_or(authority);
    format!("{host}{path}")
}

/// Whether an error body says the provider does not know the model: the
/// code `model_not_found` (muse, OpenAI), OpenRouter's "is not a valid model
/// ID", a `not_found_error` whose message names the model (Anthropic, muse
/// on messages; the same type with "Not found" is a wrong path), or Gemini's
/// `NOT_FOUND` for a `models/` name (`research/provider-errors`, "Unknown
/// model" and "Wrong path").
fn unknown_model(body: &str) -> bool {
    let value: Option<Value> = serde_json::from_str(body).ok();
    let field = |pointer: &str| {
        value
            .as_ref()
            .and_then(|v| v.pointer(pointer)?.as_str())
            .unwrap_or("")
    };
    let message = body_message(body);
    body_code(body).as_deref() == Some("model_not_found")
        || message.contains("is not a valid model ID")
        || (field("/error/type") == "not_found_error"
            && message.to_ascii_lowercase().contains("model"))
        || (field("/error/status") == "NOT_FOUND" && message.starts_with("models/"))
}

/// The code for a failure inside a 200 stream: from the provider's own code,
/// or `stream_incomplete` when none matches (`docs/model-routing.md`, "When
/// a model call fails").
fn reply_failed_code(code: Option<&str>, message: &str) -> ErrorCode {
    match code {
        Some("rate_limit_exceeded" | "rate_limit_error") => ErrorCode::RateLimited,
        // Gemini's in-stream `error.status` is a `google.rpc.Code` name.
        Some("RESOURCE_EXHAUSTED") => ErrorCode::RateLimited,
        Some(
            "server_error" | "overloaded_error" | "api_error" | "INTERNAL" | "UNAVAILABLE"
            | "DEADLINE_EXCEEDED",
        ) => ErrorCode::ProviderUnavailable,
        _ if overflow(code, message) => ErrorCode::ContextOverflow,
        _ => ErrorCode::StreamIncomplete,
    }
}

/// The overflow shapes `docs/errors.md` lists ("Recognising a context
/// overflow"). The Anthropic phrase is unprobed: no probed request
/// overflowed the context window.
fn overflow(code: Option<&str>, message: &str) -> bool {
    (code == Some("invalid_prompt") && message.contains("exceeds the context window"))
        || code == Some("context_length_exceeded")
        || message.contains("prompt is too long")
        || openrouter_overflow(message)
}

/// OpenRouter's own size check: "This endpoint's maximum context length is
/// N tokens. However, you requested about T tokens (I of text input, O in
/// the output)." The same words reject an oversized `max_tokens`; it is an
/// overflow only when the input alone fills the context
/// (`research/provider-errors`, "Context overflow").
fn openrouter_overflow(message: &str) -> bool {
    let number = |after: &str| -> Option<u64> {
        let rest = message.split_once(after)?.1.trim_start();
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    let input = message
        .split_once(" of text input")
        .and_then(|(head, _)| head.rsplit(['(', ' ']).next())
        .and_then(|n| n.parse::<u64>().ok());
    match (number("maximum context length is"), input) {
        (Some(context), Some(input)) => input >= context,
        _ => false,
    }
}

/// The provider's own code in an error body: `error.code`, or the
/// upstream's `error.metadata.provider_code` that OpenRouter adds.
fn body_code(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    ["/error/metadata/provider_code", "/error/code"]
        .iter()
        .find_map(|p| value.pointer(p).and_then(Value::as_str))
        .map(str::to_owned)
}

/// Whether one `error.details[]` entry is a `google.rpc.ErrorInfo` naming
/// `reason`. Only the quota match requires the `@type`: Google's quota
/// shape always carries it, while a bare `API_KEY_INVALID` reason without
/// it still fails authentication.
fn error_info_reason(entry: &Value, reason: &str) -> bool {
    entry.get("@type").and_then(Value::as_str) == Some("type.googleapis.com/google.rpc.ErrorInfo")
        && entry.get("reason").and_then(Value::as_str) == Some(reason)
}

/// Whether a Google error body's `error.details` names `reason`
/// (`google.rpc.ErrorInfo`). A bare reason without the `@type` still
/// matches: only the quota match requires it.
fn has_reason(body: &str, reason: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.pointer("/error/details")?.as_array().cloned())
        .is_some_and(|details| details.iter().any(|d| d["reason"] == reason))
}

/// The provider's own message in an error body: `error.message`, or
/// `detail` as ChatGPT/codex sends it, or else the body itself.
fn body_message(body: &str) -> String {
    let value: Option<Value> = serde_json::from_str(body).ok();
    value
        .as_ref()
        .and_then(|v| v.pointer("/error/message").or_else(|| v.get("detail")))
        .and_then(Value::as_str)
        .unwrap_or(body)
        .to_owned()
}
