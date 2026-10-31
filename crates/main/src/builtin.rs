//! The built-in tools one session registers (`docs/architecture.md`, "Tool
//! seam"). Registration order is name order, so the tool definitions in a
//! request stay byte-identical across sessions with the same inputs
//! (`docs/prompt-cache.md`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::diag::Timing;
use contract::events::{ToolInfo, ToolSource, ToolState};
use contract::search::SearchBackend;
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

/// `ask_user`, `delegate_message`, `delegate_models`, `delegate_spawn`, `edit`, `handoff`, `name_session`, `read`, `session_list`, `session_message`, `session_search`, `shell`,
/// `skill`, `web_fetch`, `write` and `jobs`, each registered by `builtin`, and
/// `web_search` as `web_search` chose it: the hosted search of the session's
/// model when it has one, else Fiber's own over the installed backend, else
/// nothing (`docs/tools.md`, "Hosted by the provider" and "Fiber's own, over
/// a backend"). `session_search` searches the logs under `home`, Fiber
/// home, and counts a session as this project's when `worktree::project` gives
/// its workspace the same identity as `workspace`, as resume looks a
/// session up. `read`, `write` and `edit`
/// share one session's file state, which a handoff forgets, take `locks`,
/// the session's per-path lock, which an extension's `host.fs` shares, and
/// `read` runs the image child (`fiber image`) into `artifacts`, the session's
/// `artifacts/` directory, where `web_fetch` saves the PDFs and images it
/// downloads; the model's `shell` moves commands into `jobs`, which the
/// `jobs` tool lists, waits on and stops. The driver shell runs in the
/// foreground only. `fiber` is the executable recorded at process startup.
#[cfg(test)]
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
    web_search: Option<(Arc<dyn Tool>, ToolInfo)>,
    delegates: &crate::delegates::Delegates,
    skills: Arc<dyn contract::skills::Skills>,
) -> Result<SessionTools, Failure> {
    // The unit tests' entry: no image-child timing and no sandbox.
    // Production forwards its timing and sandbox through `builtin_timed`.
    let sandbox = std::sync::Arc::new(std::sync::Mutex::new(tools::SandboxSettings {
        enabled: false,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: home.to_path_buf(),
    }));
    let images = session_images(fiber.clone(), artifacts, None);
    builtin_timed(
        fiber,
        home,
        workspace,
        artifacts,
        clock,
        jobs,
        locks,
        images,
        web_search,
        delegates,
        skills,
        None,
        Default::default(),
        sandbox,
        None,
    )
}

/// The shared image child for MCP servers and the session's image tool.
/// The caller builds it before extensions load so it can start with MCP.
pub(crate) fn session_images(
    fiber: PathBuf,
    artifacts: &Path,
    timing: Option<Timing>,
) -> Arc<dyn contract::images::Images> {
    let mut images = tools::ImageChild::new(fiber, artifacts.to_path_buf());
    if let Some(timing) = timing {
        images = images.with_timing(timing);
    }
    Arc::new(images)
}

/// One session's built-in tools with an optional image-child timing: the
/// same registration as [`builtin`], timing each entered image or PDF
/// count/cut call as `picture.run` when `timing` is present. `images` is
/// the shared child already used by MCP startup.
#[allow(
    clippy::too_many_arguments,
    reason = "one session's built-ins need Fiber home and the recorded executable alongside the session's own paths"
)]
pub(crate) fn builtin_timed(
    fiber: PathBuf,
    home: &Path,
    workspace: &Path,
    artifacts: &Path,
    clock: &Arc<dyn Clock>,
    jobs: &Arc<jobs::Registry>,
    locks: &Arc<tools::PathLocks>,
    images: Arc<dyn contract::images::Images>,
    web_search: Option<(Arc<dyn Tool>, ToolInfo)>,
    delegates: &crate::delegates::Delegates,
    skills: Arc<dyn contract::skills::Skills>,
    timing: Option<Timing>,
    shell_read_only: crate::settings::ShellReadOnly,
    sandbox: crate::settings::SandboxHandle,
    protection: Option<Arc<tools::ProtectedPaths>>,
) -> Result<SessionTools, Failure> {
    let mut files = tools::Files::with_locks(workspace.to_path_buf(), Arc::clone(locks));
    // A successful write or edit advances the baseline its path landed in,
    // so the call's end restores to the approved content.
    if let Some(protection) = &protection {
        files = files.with_protection(Arc::clone(protection));
    }
    // The scan's clone comes before `timing` moves; the shell's clone is
    // taken alongside, for the same reason.
    let scan_timing = timing.clone();
    let shell_timing = timing.clone();
    if let Some(timing) = timing {
        files = files.with_timing(timing);
    }
    let files = Arc::new(files.with_images(fiber.clone(), artifacts.to_path_buf()));
    let moves: Arc<dyn contract::jobs::Jobs> = jobs.clone();
    // Only `fiber ask` reaches here, new or resumed: a non-interactive run.
    // The model's shell and the driver shell spill their output past 1 MiB
    // into the session's `artifacts/`; the driver has no hook. The model's
    // shell classifies from the person's `shell.read_only` entries beside
    // the built-in list; the driver runs the person's own command and is
    // not classified for review, so it stays unlisted.
    let mut shell = tools::Shell::new(workspace.to_path_buf(), Arc::clone(clock))
        .with_search(fiber.clone())
        .with_jobs(moves)
        .with_artifacts(artifacts.to_path_buf())
        .with_read_only(shell_read_only)
        .with_sandbox(sandbox)
        .non_interactive();
    // Confined Linux spawns take their end-of-call guard from the session's
    // shared handle; the driver shell below never runs confined, so it
    // takes none.
    if let Some(protection) = &protection {
        shell = shell.with_protection(Arc::clone(protection));
    }
    // The model's shell moves commands into jobs, so it times a
    // background start as `jobs.start`. The driver shell never moves,
    // so it stays untimed.
    if let Some(timing) = shell_timing {
        shell = shell.with_timing(timing);
    }
    let driver = Arc::new(
        tools::Shell::new(workspace.to_path_buf(), Arc::clone(clock))
            .with_search(fiber)
            .with_artifacts(artifacts.to_path_buf()),
    );
    // One socket client for both session tools: each call connects afresh.
    let sessions: Arc<dyn contract::sessions::Sessions> =
        Arc::new(doors::SessionSockets::new(home, Arc::clone(clock)));
    let mut built = vec![
        registered(tools::AskUser)?,
        registered(delegates.message())?,
        registered(delegates.models())?,
        registered(delegates.tool())?,
        registered(delegates.fork())?,
        registered(files.edit())?,
        registered(tools::Handoff)?,
        registered(tools::NameSession)?,
        registered(files.read())?,
        registered(tools::SessionList::new(
            Arc::clone(&sessions),
            delegates.session().clone(),
        ))?,
        registered(tools::SessionMessage::new(
            sessions,
            delegates.session().clone(),
            Arc::new({
                let home = home.to_path_buf();
                let own = delegates.session().clone();
                move || log::session_parent(&home, &own)
            }),
        ))?,
        registered(tools::SessionSearch::new(Arc::new(match scan_timing {
            Some(scan_timing) => {
                log::SessionScan::new(home, workspace, Arc::new(worktree::project))
                    .with_timing(scan_timing)
            }
            None => log::SessionScan::new(home, workspace, Arc::new(worktree::project)),
        })))?,
        registered(shell)?,
        registered(tools::Skill::new(skills))?,
        registered(tools::WebFetch::new(
            artifacts.to_path_buf(),
            Arc::clone(clock),
        ))?,
        registered(files.write())?,
        registered(jobs::JobsTool::new(Arc::clone(jobs)))?,
    ];
    // What `web_search` chose for the session's preamble: the hosted
    // search, Fiber's own over the backend, or nothing. It is fixed when
    // the session's preamble is built (`docs/tools.md`, "Hosted by the
    // provider").
    if let Some(given) = web_search {
        built.push(given);
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

/// What `web_search` chose for the session: the declared tool and what
/// the `tools` command answers for it, if any.
type Chosen = Option<(Arc<dyn Tool>, ToolInfo)>;

/// The `web_search` the session declares: the model's hosted search when
/// it has one, else Fiber's own tool over the chosen backend, else nothing
/// (`docs/tools.md`, "web_search": the model sees one tool, whichever way
/// it runs). At most one `web_search` is registered at any time.
pub(crate) fn web_search(
    kind: Option<&str>,
    backend: Option<&Arc<dyn SearchBackend>>,
) -> Result<Chosen, Failure> {
    if let Some(kind) = kind {
        return hosted(kind).map(Some);
    }
    let Some(backend) = backend else {
        return Ok(None);
    };
    registered(tools::BackendSearch::new(Arc::clone(backend))).map(Some)
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

/// Builds the session's `tool_search` from the deferred definitions as
/// sent and their sources (`docs/tools.md`, "Tool search"): the search
/// tool by role, holding no session state.
pub(crate) fn tool_search_factory() -> r#loop::ToolSearchFactory {
    Arc::new(
        |catalogue: &[contract::provider::ToolDefinition], sources: &[String]| {
            Arc::new(tools::ToolSearch::new(catalogue.to_vec(), sources.to_vec())) as Arc<dyn Tool>
        },
    )
}

/// Publishes the loop's `/tools` rows through the session's declarer and
/// state setter (`docs/tools.md`, "Seeing the tools"): `declare` reaches
/// the declarer only for the search tool the loop adds and removes, so
/// every other tool keeps the source its own registration gave it; `state`
/// reaches the setter for every name, correcting each row at each build.
#[allow(
    clippy::type_complexity,
    reason = "the setter type is doors' closure type, named at neither end"
)]
pub(crate) fn tool_rows(
    declare: doors::Declare,
    state: Arc<dyn Fn(&str, ToolState) + Send + Sync>,
) -> r#loop::ToolRows {
    r#loop::ToolRows {
        declare: Arc::new(move |name: &str, info: Option<ToolInfo>| {
            if name == contract::provider::TOOL_SEARCH {
                declare(name, info);
            }
        }),
        state,
    }
}

#[cfg(test)]
#[path = "builtin_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tool_budget_tests.rs"]
mod tool_budget_tests;

#[cfg(test)]
#[path = "timed_tests.rs"]
mod timed_tests;
