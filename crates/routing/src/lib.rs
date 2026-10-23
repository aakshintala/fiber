//! Model routing: picks the model for a session or a switch, looks up its
//! credential, starts its Lua provider, builds its `Provider`, and finds
//! the reviewer (`docs/architecture.md`, module table, `routing` row).
//! The one module that uses `config`, `extensions` and `provider`
//! together. `main` calls it at startup and when a session switches models.

pub mod credential;
pub mod lua_providers;
pub mod scripted;

use contract::shapes::Failure;

#[cfg(test)]
mod test_support;

/// A failure with `code` and `message`: the `Failure` struct built the way
/// `main`'s `settings::context_window` builds its own, since routing
/// builds `contract`'s shape itself instead of depending on `doors`.
pub(crate) fn failure(code: contract::ErrorCode, message: impl std::fmt::Display) -> Failure {
    Failure {
        code,
        message: message.to_string(),
        retry_after_ms: None,
        provider: None,
    }
}

/// A failure with `code` for the error `display` names.
pub(crate) fn failed(code: contract::ErrorCode, display: impl std::fmt::Display) -> Failure {
    failure(code, display.to_string())
}
