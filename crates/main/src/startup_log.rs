//! The diagnostic line a `fiber ask` startup failure leaves in
//! `logs/ask-<pid>.log` (`docs/state.md`, "Diagnostic logs"). The line's
//! message is fixed text: the failure's own sentence can name a path or
//! echo a configuration value, and nothing in `logs/` holds either.

use std::sync::Arc;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::shapes::Failure;
use log::diag::{Diag, Level, Process, Severity};

/// Records `failure` as one `error` line in this process's `logs/` file.
/// A `usage` failure is the caller's typo, not a startup failure, and a
/// home that cannot be resolved has no `logs/`: both record nothing.
pub(crate) fn record(failure: &Failure, clock: Arc<dyn Clock>) {
    if failure.code == ErrorCode::Usage {
        return;
    }
    let Ok(home) = config::fiber_home_from_env() else {
        return;
    };
    let code = serde_json::to_value(&failure.code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "other".to_owned());
    Diag::new(&home, Process::Ask, Level::Info, clock).line(
        Severity::Error,
        None,
        &code,
        "fiber ask failed before its session started; the sentence went to stderr.",
    );
}
