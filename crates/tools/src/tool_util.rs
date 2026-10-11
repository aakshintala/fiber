//! Shared argument parsing and result builders for the built-in tools
//! (`docs/tools.md`, "Before a call runs").

use std::path::Path;

use contract::ErrorCode;
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure};
use contract::tool::{Effects, Output};
use serde::Deserialize;
use serde_json::{Map, Value};

/// Parses the call's arguments against the tool's schema. A malformed call
/// never runs: the loop rejects it before `effects`, so only a direct
/// caller sees this message.
pub(crate) fn arguments<'a, T: Deserialize<'a>>(
    arguments: &'a Map<String, Value>,
) -> Result<T, String> {
    T::deserialize(arguments)
        .map_err(|error| format!("The arguments do not fit the schema: {error}."))
}

/// The call's output for `code` and `message`.
pub(crate) fn failed(code: ErrorCode, message: String) -> Output {
    Output {
        content: vec![ContentPart::Text {
            text: format!("{message}\n"),
        }],
        error: Some(failure(code, message)),
        ..Output::default()
    }
}

/// The error a failed call carries.
pub(crate) fn failure(code: ErrorCode, message: String) -> Failure {
    Failure {
        code,
        message,
        retry_after_ms: None,
        provider: None,
    }
}

/// The call's output for plain text.
pub(crate) fn text_output(text: String) -> Output {
    Output {
        content: vec![ContentPart::Text { text }],
        ..Output::default()
    }
}

/// The output when the call was cancelled before it started.
pub(crate) fn cancelled_before() -> Output {
    text_output("Cancelled before it started.\n".to_owned())
}

/// A tool with no effects declares nothing, reversibly.
pub(crate) fn no_effects() -> Effects {
    effects(Vec::new(), true, None, Some(String::new()), None)
}

/// A tool's declared effects.
pub(crate) fn effects(
    effects: Vec<Effect>,
    reversible: bool,
    paths: Option<Vec<String>>,
    subject: Option<String>,
    prefix: Option<String>,
) -> Effects {
    Effects {
        declared: DeclaredEffects {
            effects,
            reversible,
            paths,
        },
        subject,
        prefix,
        always_reviewed: false,
    }
}

/// Whether the rechecked call reads or writes.
pub(crate) enum Act {
    Read,
    Write,
}

/// Rejects a call whose path changed between the permission check and the
/// run. `held` is the path locked at the start of the run; `judged` is the
/// path the permission check saw for `raw`.
#[allow(
    clippy::result_large_err,
    reason = "the error is the call's Output, returned unchanged"
)]
pub(crate) fn recheck(
    raw: &str,
    path: &Path,
    held: Option<&Path>,
    judged: Option<&Path>,
    act: Act,
) -> Result<(), Output> {
    let changed =
        held.is_some_and(|held| held != path) || judged.is_some_and(|judged| judged != path);
    if !changed {
        return Ok(());
    }
    let (verb, nothing) = match act {
        Act::Read => ("read", "Nothing was read."),
        Act::Write => ("write", "Nothing was written."),
    };
    Err(failed(
        ErrorCode::PathChanged,
        format!("`{raw}` changed between the permission check and the {verb}. {nothing}"),
    ))
}

#[cfg(test)]
#[path = "tool_util_tests.rs"]
mod tests;
