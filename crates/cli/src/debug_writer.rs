//! The one-shot command writer (`docs/state.md`, "Diagnostic logs"): a
//! `Process::Cli` writer when the loaded configuration runs diagnostics
//! at debug, else `None`. The writer names `logs/cli-<pid>.log` for the
//! running process and carries no session id; building it never fails
//! the command — diagnostics stay best-effort.

use std::sync::Arc;

use contract::clock::Clock;
use contract::diag::DebugLog;
use log::diag::{Diag, Level, Process};

/// The writer for a one-shot CLI command under `config`: `Some` when
/// diagnostics run at debug, `None` at info. The caller passes its
/// already-loaded configuration, so no second load is needed.
pub(crate) fn debug_writer_for(
    home: &std::path::Path,
    config: &config::Config,
    clock: &Arc<dyn Clock>,
) -> Option<Arc<dyn DebugLog>> {
    config::diagnostics_debug(config).then(|| {
        Arc::new(Diag::new(
            home,
            Process::Cli,
            Level::Debug,
            Arc::clone(clock),
        )) as Arc<dyn DebugLog>
    })
}

#[cfg(test)]
#[path = "debug_writer_tests.rs"]
mod tests;
