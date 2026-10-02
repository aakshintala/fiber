//! Why a model call failed, and the code each case reports
//! (`docs/errors.md`, "A failed model call").

use contract::ErrorCode;
use contract::shapes::{Failure, ProviderFailure};
use serde_json::Value;

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
        /// The seconds a `retry-after` header asked Fiber to wait.
        retry_after: Option<f64>,
        /// The `x-should-retry` header, when the response carried one.
        should_retry: Option<bool>,
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
    /// The request could not be signed, so it was never sent.
    #[error("could not be signed: {0}")]
    Sign(contract::signing::Error),
}

impl Error {
    /// The stable code a consumer switches on.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Connection(_) => ErrorCode::ConnectionFailed,
            Self::Status { status, body, .. } => status_code(*status, body),
            Self::StreamIncomplete(_) => ErrorCode::StreamIncomplete,
            Self::ReplyFailed { code, message } => reply_failed_code(code.as_deref(), message),
            Self::UnknownStopReason(_) => ErrorCode::UnknownStopReason,
            Self::ContextOverflow(_) => ErrorCode::ContextOverflow,
            Self::Refused(_) => ErrorCode::Refused,
            // ponytail: #322 has not named the code a failed `sign()` reports,
            // so this is `connection_failed` until it does. It is not retried.
            Self::Sign(_) => ErrorCode::ConnectionFailed,
        }
    }

    /// The `x-should-retry` header of the response that failed, when it
    /// carried one.
    pub fn should_retry(&self) -> Option<bool> {
        match self {
            Self::Status { should_retry, .. } => *should_retry,
            // A failed signature is not a transport failure, so the retry
            // policy's default for `connection_failed` does not apply.
            Self::Sign(_) => Some(false),
            Self::Connection(_)
            | Self::StreamIncomplete(_)
            | Self::ReplyFailed { .. }
            | Self::UnknownStopReason(_)
            | Self::ContextOverflow(_)
            | Self::Refused(_) => None,
        }
    }

    /// The failure as a failed model call records it, naming `provider`.
    pub fn failure(&self, provider: &str) -> Failure {
        let code = self.code();
        let (retry_after, said) = match self {
            Self::Status {
                status,
                body,
                retry_after,
                ..
            } => (*retry_after, Some((*status, body_message(body)))),
            Self::ReplyFailed { message, .. } => (None, Some((200, message.clone()))),
            Self::Connection(_)
            | Self::StreamIncomplete(_)
            | Self::UnknownStopReason(_)
            | Self::ContextOverflow(_)
            | Self::Refused(_)
            | Self::Sign(_) => (None, None),
        };
        let message =
            if let (Self::Status { status, .. }, ErrorCode::AuthenticationFailed) = (self, &code) {
                format!(
                    "{provider} rejected the credential (HTTP {status}). Check the key it is \
                 configured with, or log in again with `fiber login {provider}`."
                )
            } else {
                format!("{provider} {self}")
            };
        Failure {
            code,
            message,
            retry_after,
            provider: said.map(|(status, message)| ProviderFailure {
                name: provider.to_owned(),
                status,
                message,
            }),
        }
    }
}

/// The code for an HTTP status, reading the body where the status alone
/// cannot classify.
fn status_code(status: u16, body: &str) -> ErrorCode {
    match status {
        401 => ErrorCode::AuthenticationFailed,
        429 => ErrorCode::RateLimited,
        408 | 409 | 500..=599 => ErrorCode::ProviderUnavailable,
        _ if overflow(body_code(body).as_deref(), &body_message(body)) => {
            ErrorCode::ContextOverflow
        }
        _ => ErrorCode::InvalidRequest,
    }
}

/// The code for a failure inside a 200 stream: from the provider's own code,
/// or `stream_incomplete` when none matches (`docs/model-routing.md`, "When
/// a model call fails").
fn reply_failed_code(code: Option<&str>, message: &str) -> ErrorCode {
    match code {
        Some("rate_limit_exceeded" | "rate_limit_error") => ErrorCode::RateLimited,
        Some("server_error" | "overloaded_error" | "api_error") => ErrorCode::ProviderUnavailable,
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
