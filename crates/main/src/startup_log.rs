//! The diagnostic line a `fiber ask` or internal `fiber session`
//! startup failure leaves in `logs/<kind>-<pid>.log` (`docs/state.md`, "Diagnostic logs"). The line's
//! message is fixed text: the failure's own sentence can name a path or
//! echo a configuration value, and nothing in `logs/` holds either.

use std::sync::{Arc, OnceLock};

use contract::ErrorCode;
use contract::clock::Clock;
use contract::shapes::Failure;
use log::diag::{Diag, Level, Process, Severity};

/// The process kind that records, `ask` until [`as_session`].
static PROCESS: OnceLock<Process> = OnceLock::new();

/// Marks this process as an internal `fiber session`, so its startup
/// failures go to `logs/session-<pid>.log` (`docs/state.md`, "Diagnostic
/// logs"). Called once, before anything can fail.
pub(crate) fn as_session() {
    PROCESS.set(Process::Session).unwrap_or(());
}

/// A writer for this process's pid-named diagnostic file, or `None`
/// when the home cannot be resolved: a missing home has no `logs/`, so
/// the caller records nothing.
pub(crate) fn diag(clock: Arc<dyn Clock>) -> Option<Diag> {
    let home = config::fiber_home_from_env().ok()?;
    Some(Diag::new(
        &home,
        PROCESS.get().copied().unwrap_or(Process::Ask),
        Level::Info,
        clock,
    ))
}

/// Records `failure` as one `error` line in this process's `logs/` file.
/// A `usage` failure is the caller's typo, not a startup failure, and a
/// home that cannot be resolved has no `logs/`: both record nothing.
pub(crate) fn record(failure: &Failure, clock: Arc<dyn Clock>) {
    if failure.code == ErrorCode::Usage {
        return;
    }
    let Some(diag) = self::diag(clock) else {
        return;
    };
    let code = serde_json::to_value(&failure.code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "other".to_owned());
    diag.line(
        Severity::Error,
        None,
        &code,
        "The process failed before its session started; the sentence went to stderr.",
    );
}
