//! The built-in tools one session registers (`docs/architecture.md`, "Tool
//! seam"). Registration order is name order, so the tool definitions in a
//! request stay byte-identical across sessions with the same inputs
//! (`docs/prompt-cache.md`).

use std::path::{Path, PathBuf};
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
    Arc<dyn contract::images::Images>,
);

/// `ask_user`, `delegate_spawn`, `edit`, `handoff`, `read`, `session_search`, `shell`,
/// `skill`, `web_fetch`, `write` and `jobs`, each registered by `builtin`, and
/// `web_search` when `web_search` names the hosted search type of the
/// session's model. `session_search` searches the logs under `home`, Fiber
/// home, and counts a session as this project's when `doors::project` gives
/// its workspace the same identity as `workspace`, as resume looks a
/// session up. `read`, `write` and `edit`
/// share one session's file state, which a handoff forgets, take `locks`,
/// the session's per-path lock, which an extension's `host.fs` shares, and
/// `read` runs the image child (`fiber image`) into `artifacts`, the session's
/// `artifacts/` directory, where `web_fetch` saves the PDFs and images it
/// downloads; the model's `shell` moves commands into `jobs`, which the
/// `jobs` tool lists, waits on and stops. The driver shell runs in the
/// foreground only. `fiber` is the executable recorded at process startup.
#[allow(
    clippy::too_many_arguments,
    reason = "one session's built-ins need Fiber home and the recorded executable alongside the session's own paths"
)]
pub(crate) fn builtin(
    fiber: PathBuf,
    home: &Path,
    workspace: &Path,
    artifacts: &Path,
    clock: &Arc<dyn Clock>,
    jobs: &Arc<jobs::Registry>,
    locks: &Arc<tools::PathLocks>,
    web_search: Option<&str>,
    delegates: &crate::delegates::Delegates,
    skills: Arc<dyn contract::skills::Skills>,
) -> Result<SessionTools, Failure> {
    let files = Arc::new(
        tools::Files::with_locks(workspace.to_path_buf(), Arc::clone(locks))
            .with_images(fiber.clone(), artifacts.to_path_buf()),
    );
    let images: Arc<dyn contract::images::Images> = Arc::new(tools::ImageChild::new(
        fiber.clone(),
        artifacts.to_path_buf(),
    ));
    let moves: Arc<dyn contract::jobs::Jobs> = jobs.clone();
    // Only `fiber ask` reaches here, new or resumed: a non-interactive run.
    let shell = tools::Shell::new(workspace.to_path_buf(), Arc::clone(clock))
        .with_search(fiber.clone())
        .with_jobs(moves)
        .non_interactive();
    let driver =
        Arc::new(tools::Shell::new(workspace.to_path_buf(), Arc::clone(clock)).with_search(fiber));
    let mut built = vec![
        registered(tools::AskUser)?,
        registered(delegates.tool())?,
        registered(files.edit())?,
        registered(tools::Handoff)?,
        registered(files.read())?,
        registered(tools::SessionSearch::new(Arc::new(log::SessionScan::new(
            home,
            workspace,
            Arc::new(doors::project),
        ))))?,
        registered(shell)?,
        registered(tools::Skill::new(skills))?,
        registered(tools::WebFetch::new(
            artifacts.to_path_buf(),
            Arc::clone(clock),
        ))?,
        registered(files.write())?,
        registered(jobs::JobsTool::new(Arc::clone(jobs)))?,
    ];
    // Declared only for a model whose provider hosts a search; it is fixed
    // when the session's preamble is built (`docs/tools.md`, "Hosted by the
    // provider").
    if let Some(kind) = web_search {
        built.push(hosted(kind)?);
    }
    let mut pairs = Vec::new();
    let mut infos = Vec::new();
    for (tool, info) in built {
        pairs.push((String::from("builtin"), tool));
        infos.push(info);
    }
    let forget: Arc<dyn Fn() + Send + Sync> = Arc::new(move || files.forget());
    Ok((pairs, infos, driver, forget, images))
}

/// The provider's hosted search of type `kind`, and what the `tools`
/// command answers for it (`docs/tools.md`, "Hosted by the provider").
pub(crate) fn hosted(kind: &str) -> Result<(Arc<dyn Tool>, ToolInfo), Failure> {
    registered(tools::HostedSearch::new(kind.to_owned()))
}

/// One tool and what the `tools` command answers for it.
fn registered(tool: impl Tool + 'static) -> Result<(Arc<dyn Tool>, ToolInfo), Failure> {
    let info = info(&tool, ToolSource::Builtin)?;
    Ok((Arc::new(tool), info))
}

/// What the `tools` command answers for `tool`, declared in full, which
/// `source` registered.
pub(crate) fn info(tool: &dyn Tool, source: ToolSource) -> Result<ToolInfo, Failure> {
    let definition = tool.definition();
    let encoded = serde_json::to_vec(&definition)
        .map_err(|error| failed(ErrorCode::IoFailed, format!("a tool definition: {error}")))?;
    Ok(ToolInfo {
        name: definition.name,
        source,
        state: ToolState::Full,
        bytes: u64::try_from(encoded.len()).unwrap_or(u64::MAX),
        tokens: None,
    })
}

#[cfg(test)]
#[path = "builtin_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tool_budget_tests.rs"]
mod tool_budget_tests;
