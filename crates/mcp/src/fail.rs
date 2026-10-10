//! One `Failure` and the `Output` that carries it.

use contract::ErrorCode;
use contract::shapes::Failure;
use contract::tool::Output;

/// One failure with no retry and no provider.
pub(crate) fn failure(code: ErrorCode, message: String) -> Failure {
    Failure {
        code,
        message,
        retry_after_ms: None,
        provider: None,
    }
}

/// One failed output with no retry and no provider.
pub(crate) fn failed(code: ErrorCode, message: String) -> Output {
    Output {
        error: Some(failure(code, message)),
        ..Output::default()
    }
}
