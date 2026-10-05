//! The built-in tools one session registers (`docs/architecture.md`, "Tool
//! seam"). Registration order is name order, so the tool definitions in a
//! request stay byte-identical across sessions with the same inputs
//! (`docs/prompt-cache.md`).

use std::path::Path;
use std::sync::Arc;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{ToolInfo, ToolSource, ToolState};
use contract::shapes::Failure;
use contract::tool::Tool;

use crate::failed;

/// The loop's `(who, tool)` pairs, the infos the `tools` command answers
/// with, the driver shell [`doors::Session::shell`] runs, and what a handoff
/// runs to clear what the file tools have seen.
type SessionTools = (
    Vec<(String, Arc<dyn Tool>)>,
    Vec<ToolInfo>,
    Arc<dyn Tool>,
    Arc<dyn Fn() + Send + Sync>,
);

/// `edit`, `handoff`, `read`, `shell`, `write` and `jobs`, each registered
/// by `builtin`. `read`, `write` and `edit` share one session's file state,
/// which a handoff forgets; the model's `shell` moves commands into `jobs`,
/// which the `jobs` tool lists, waits on and stops. The driver shell runs in
/// the foreground only. A failure to find the running binary is
/// `io_failed`, before any session line.
pub(crate) fn builtin(
    workspace: &Path,
    clock: &Arc<dyn Clock>,
    jobs: &Arc<jobs::Registry>,
) -> Result<SessionTools, Failure> {
    let files = Arc::new(tools::Files::new(workspace.to_path_buf()));
    let fiber = std::env::current_exe()
        .map_err(|error| failed(ErrorCode::IoFailed, format!("the running binary: {error}")))?;
    let moves: Arc<dyn contract::jobs::Jobs> = jobs.clone();
    let shell = tools::Shell::new(workspace.to_path_buf(), Arc::clone(clock))
        .with_search(fiber.clone())
        .with_jobs(moves);
    let driver =
        Arc::new(tools::Shell::new(workspace.to_path_buf(), Arc::clone(clock)).with_search(fiber));
    let built = [
        registered(files.edit())?,
        registered(tools::Handoff)?,
        registered(files.read())?,
        registered(shell)?,
        registered(files.write())?,
        registered(jobs::JobsTool::new(Arc::clone(jobs)))?,
    ];
    let mut pairs = Vec::new();
    let mut infos = Vec::new();
    for (tool, info) in built {
        pairs.push((String::from("builtin"), tool));
        infos.push(info);
    }
    let forget: Arc<dyn Fn() + Send + Sync> = Arc::new(move || files.forget());
    Ok((pairs, infos, driver, forget))
}

/// One tool and what the `tools` command answers for it.
fn registered(tool: impl Tool + 'static) -> Result<(Arc<dyn Tool>, ToolInfo), Failure> {
    let definition = tool.definition();
    let encoded = serde_json::to_vec(&definition)
        .map_err(|error| failed(ErrorCode::IoFailed, format!("a tool definition: {error}")))?;
    let info = ToolInfo {
        name: definition.name,
        source: ToolSource::Builtin,
        state: ToolState::Full,
        bytes: u64::try_from(encoded.len()).unwrap_or(u64::MAX),
        tokens: None,
    };
    Ok((Arc::new(tool), info))
}

#[cfg(test)]
#[path = "builtin_tests.rs"]
mod tests;
