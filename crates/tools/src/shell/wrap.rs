//! The shell wrappers (`docs/tools.md`, "Shell", "Effects"): a wrapper is
//! classified by the command it runs. `timeout <duration> <command>` and
//! `env <assignments> <command>` are read-only when the inner command is,
//! and so is `NAME=value <command>`, but only for names on the fixed list
//! below. `xargs` is read-only only into six commands with their own flags.

use super::read_only::{COMMANDS, Command, XARGS_PRINTF};
use super::syntax::Word;

/// The names an assignment may carry without making the call `executes`:
/// the fixed list `docs/tools.md` writes. Any other name, such as `PATH`,
/// `LD_PRELOAD`, `PAGER` or a `GIT_CONFIG_*` variable, is refused.
pub(super) const ASSIGNABLE: [&str; 7] = [
    "RUST_BACKTRACE",
    "LANG",
    "LC_ALL",
    "TZ",
    "TERM",
    "CI",
    "GIT_TERMINAL_PROMPT",
];

/// The most nested wrappers one stage may hold; deeper nesting is
/// `executes`, so a long alias chain can never loop the classifier.
pub(super) const MAX_WRAPPER_DEPTH: usize = 4;

/// The commands `xargs` may run: `echo`, `printf`, `wc`, `grep`, `head`
/// and `tail` (`docs/tools.md`, "Shell", "Effects"). Any other target is
/// `executes`, and so is a target with a flag its own entry refuses.
const XARGS_COMMANDS: [&str; 6] = ["echo", "printf", "wc", "grep", "head", "tail"];

/// What one unwrap step finds in a stage's words.
pub(super) enum Unwrapped<'a> {
    /// No wrapper leads the words: classify them as written.
    Plain,
    /// The words after `env` assignments or a `timeout` duration: classify
    /// them by the same lookup.
    Inner(&'a [Word]),
    /// The words after `xargs`: the first must name one of the six
    /// read-only commands above.
    Xargs(&'a [Word]),
    /// A wrapper the allow-list refuses: the call is `executes`.
    Refused,
}

/// Whether every leading `NAME=value` assignment names an allowed variable.
pub(super) fn assignments_allowed(assignments: &[(String, String)]) -> bool {
    assignments
        .iter()
        .all(|(name, _)| ASSIGNABLE.contains(&name.as_str()))
}

/// The entry classifying an `xargs` target: the built-in row for five of
/// the six commands, and the flagless `printf` row beside them (a direct
/// `printf` stays `executes`, so the row never reaches the built-in list).
/// A path-qualified target matches nothing: it is `executes`.
pub(super) fn xargs_entry(name: &str) -> Option<&'static Command> {
    if !XARGS_COMMANDS.contains(&name) {
        return None;
    }
    if name == "printf" {
        return Some(&XARGS_PRINTF);
    }
    COMMANDS.iter().find(|entry| entry.name == name)
}

/// Peel one wrapper off a stage's words: `env`, `timeout` or `xargs`, or
/// `Plain` when no wrapper leads. Only the bare names unwrap: a
/// path-qualified `/usr/bin/env` classifies as written and executes.
pub(super) fn unwrap(words: &[Word]) -> Unwrapped<'_> {
    let Some((first, rest)) = words.split_first() else {
        return Unwrapped::Plain;
    };
    match first.cooked.as_str() {
        "env" => unwrap_env(rest),
        "timeout" => unwrap_timeout(rest),
        "xargs" => unwrap_xargs(rest),
        _ => Unwrapped::Plain,
    }
}

/// The words after `env`: leading `NAME=value` assignments with allowed
/// names, then the inner command. A flag of `env`'s own, a refused name,
/// and a bare `env` with no command left are all `executes`.
fn unwrap_env(rest: &[Word]) -> Unwrapped<'_> {
    let mut rest = rest;
    loop {
        let Some((word, tail)) = rest.split_first() else {
            return Unwrapped::Refused;
        };
        if word.cooked.starts_with('-') {
            return Unwrapped::Refused;
        }
        let Some((name, _)) = word.cooked.split_once('=') else {
            return Unwrapped::Inner(rest);
        };
        if !ASSIGNABLE.contains(&name) {
            return Unwrapped::Refused;
        }
        rest = tail;
    }
}

/// The words after `timeout`: one duration, then the inner command. A flag,
/// a refused duration, and a duration with no command left are `executes`.
fn unwrap_timeout(rest: &[Word]) -> Unwrapped<'_> {
    let Some((duration, rest)) = rest.split_first() else {
        return Unwrapped::Refused;
    };
    if duration.cooked.starts_with('-') || !is_duration(&duration.cooked) {
        return Unwrapped::Refused;
    }
    if rest.is_empty() {
        return Unwrapped::Refused;
    }
    Unwrapped::Inner(rest)
}

/// The words after `xargs`: its own flags are unlisted, so a leading `-`
/// word executes, as does a bare `xargs` with no command left.
fn unwrap_xargs(rest: &[Word]) -> Unwrapped<'_> {
    let Some((first, _)) = rest.split_first() else {
        return Unwrapped::Refused;
    };
    if first.cooked.starts_with('-') {
        return Unwrapped::Refused;
    }
    Unwrapped::Xargs(rest)
}

/// Whether `cooked` is a `timeout` duration: digits with an optional
/// fraction and one optional `s`, `m`, `h` or `d` suffix.
fn is_duration(cooked: &str) -> bool {
    let digits = cooked
        .strip_suffix(|suffix| matches!(suffix, 's' | 'm' | 'h' | 'd'))
        .unwrap_or(cooked);
    let mut parts = digits.split('.');
    let int = parts.next();
    let int_ok =
        int.is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    match parts.next() {
        None => int_ok,
        Some(frac) => {
            int_ok
                && !frac.is_empty()
                && frac.bytes().all(|byte| byte.is_ascii_digit())
                && parts.next().is_none()
        }
    }
}

#[cfg(test)]
#[path = "wrap_tests.rs"]
mod tests;
