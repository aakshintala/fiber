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

/// Best-effort one-shot writer for a command holding no loaded
/// configuration: loads it from `home` and `workspace`, returning `None`
/// when the load fails or diagnostics run below debug. The writer stamps
/// its lines on the process clock: the operation only borrows its clock,
/// never shares it. Building it never fails the command.
pub(crate) fn cli_writer(
    home: &std::path::Path,
    workspace: &std::path::Path,
) -> Option<Arc<dyn DebugLog>> {
    let (_, project) = crate::project_of(home, workspace).ok()?;
    let config = config::Config::load(config::Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project,
        overrides: Vec::new(),
    })
    .ok()?;
    // The process clock stamps the lines; diagnostics stay best-effort,
    // so a missing writer only silences them.
    let clock: Arc<dyn Clock> = Arc::new(support::clock::System);
    debug_writer_for(home, &config, &clock)
}

#[cfg(test)]
#[path = "debug_writer_tests.rs"]
mod tests;
