//! Shared argument parsing and result builders for the built-in tools
//! (`docs/tools.md`, "Before a call runs").

use std::path::Path;

use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure};
use contract::tool::{Effects, Output};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::{ArgumentsError, FileError, ToolError};

/// Parses the call's arguments against the tool's schema as a named case.
/// A malformed call never runs: the loop rejects it before `effects`, so
/// only a direct caller sees this error.
pub(crate) fn parsed<'a, T: Deserialize<'a>>(
    arguments: &'a Map<String, Value>,
) -> Result<T, ArgumentsError> {
    T::deserialize(arguments).map_err(|source| ArgumentsError::DoNotFit { source })
}

/// Parses the call's arguments against the tool's schema. A malformed call
/// never runs: the loop rejects it before `effects`, so only a direct
/// caller sees this message.
pub(crate) fn arguments<'a, T: Deserialize<'a>>(
    arguments: &'a Map<String, Value>,
) -> Result<T, String> {
    parsed(arguments).map_err(|error| error.to_string())
}

/// The call's output for a named failure: the case's sentence with a trailing
/// newline, and its code. The one place a `Output` failure is built.
pub(crate) fn errored(error: impl Into<ToolError>) -> Output {
    let error = error.into();
    let message = error.to_string();
    Output {
        content: vec![ContentPart::Text {
            text: format!("{message}\n"),
        }],
        error: Some(Failure::from(&error)),
        ..Output::default()
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
            confined: false,
        },
        subject,
        prefix,
        always_reviewed: false,
        hosts: None,
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
pub(crate) fn recheck(
    raw: &str,
    path: &Path,
    held: Option<&Path>,
    judged: Option<&Path>,
    act: Act,
) -> Result<(), FileError> {
    let changed =
        held.is_some_and(|held| held != path) || judged.is_some_and(|judged| judged != path);
    if !changed {
        return Ok(());
    }
    Err(match act {
        Act::Read => FileError::ChangedBeforeRead {
            raw: raw.to_owned(),
        },
        Act::Write => FileError::ChangedBeforeWrite {
            raw: raw.to_owned(),
        },
    })
}

#[cfg(test)]
#[path = "tool_util_tests.rs"]
mod tests;
