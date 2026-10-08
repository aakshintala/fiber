//! Wiring a Fiber delegate into a session (`docs/delegates.md`, "Choosing a
//! model" and "Lifetime"): resolving its `fiber:` reference through the
//! installed providers, building the child session's command, and watching
//! the child's socket as an ordinary client. `main` builds one [`Delegates`]
//! per session and hands it to the tool seam, which registers
//! `delegate_spawn` from it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use contract::SessionId;
use contract::clock::Clock;

/// How large a delegate's `events.jsonl` may grow before the runner stops
/// it as `failed` `output_cap`: the 5 GB every job's output file gets
/// (`docs/tools.md`, "Background jobs").
pub(crate) const OUTPUT_CAP: u64 = 5_000_000_000;

/// Everything one session needs to offer `delegate_spawn`: who the child
/// belongs to and where it logs, and the three closures the runner drives.
/// Built per session, because the parent session id differs for a new and
/// a resumed session.
pub(crate) struct Delegates {
    /// The parent session: the child's `--parent`.
    parent: SessionId,
    /// The parent's workspace, which the delegate shares.
    workspace: PathBuf,
    /// Where delegates log: each child's `events.jsonl` lives under
    /// `<sessions>/<session id>`.
    sessions: PathBuf,
    /// The session's jobs.
    jobs: Arc<jobs::Registry>,
    /// The session clock.
    clock: Arc<dyn Clock>,
    /// Resolves a `fiber:` reference, or lists the valid ones.
    resolve: jobs::Resolve,
    /// Builds the child's command.
    launch: jobs::Launch,
    /// Watches the child's socket.
    watch: jobs::Watch,
}

impl Delegates {
    /// Wires one session's delegates: `fiber` is the running binary the
    /// child re-executes, and `home` is Fiber home, where the child's
    /// socket lives.
    #[allow(
        clippy::too_many_arguments,
        reason = "one session's delegate wiring: its ids, paths, jobs, clock and resolver"
    )]
    pub(crate) fn new(
        fiber: PathBuf,
        home: PathBuf,
        parent: SessionId,
        workspace: PathBuf,
        sessions: PathBuf,
        jobs: Arc<jobs::Registry>,
        clock: Arc<dyn Clock>,
        resolve: jobs::Resolve,
    ) -> Self {
        let launch = launcher(&fiber);
        let watch = watcher(&home);
        Self {
            parent,
            workspace,
            sessions,
            jobs,
            clock,
            resolve,
            launch,
            watch,
        }
    }

    /// The `delegate_spawn` tool this session declares.
    pub(crate) fn tool(&self) -> jobs::DelegateSpawn {
        jobs::DelegateSpawn {
            registry: Arc::clone(&self.jobs),
            parent: self.parent.clone(),
            workspace: self.workspace.clone(),
            sessions: self.sessions.clone(),
            clock: Arc::clone(&self.clock),
            // A stop waits for the delegate's own shutdown before SIGKILL,
            // as the parent's shutdown bound does (`docs/invocation.md`,
            // "Shutdown").
            bound: doors::SHUTDOWN_BOUND,
            cap: OUTPUT_CAP,
            resolve: Arc::clone(&self.resolve),
            launch: Arc::clone(&self.launch),
            watch: Arc::clone(&self.watch),
        }
    }
}

/// Resolves a delegate's model through the installed providers: only full
/// `fiber:<provider>/<model>[:<level>]` references are accepted, and an
/// explicit level must be one the model takes, checked with the same rule
/// `--model` uses (`docs/delegates.md`, "Choosing a model"). Anything else
/// fails with the valid references, `fiber:<provider>/<model>` one per line.
pub(crate) fn resolver(
    providers: &extensions::Providers,
    config: &config::Config,
) -> jobs::Resolve {
    let providers = providers.clone();
    let config = config.clone();
    Arc::new(move |reference| resolve(&providers, &config, reference))
}

/// The valid references: every installed model as `fiber:<provider>/<model>`.
fn valid(providers: &extensions::Providers) -> Vec<String> {
    let mut references = Vec::new();
    for name in providers.names() {
        if let Some(provider) = providers.get(name) {
            for model in &provider.models {
                references.push(format!("fiber:{name}/{}", model.id));
            }
        }
    }
    references
}

fn resolve(
    providers: &extensions::Providers,
    config: &config::Config,
    reference: &str,
) -> Result<String, Vec<String>> {
    // Roles and other harnesses are later tickets: without the prefix the
    // reference is refused even when the plain lookup would take it, and a
    // bare id without a provider is refused too.
    let Some(rest) = reference.strip_prefix("fiber:") else {
        return Err(valid(providers));
    };
    if !rest.contains('/') {
        return Err(valid(providers));
    }
    let resolved = providers.resolve(rest).map_err(|_| valid(providers))?;
    if let Some(level) = resolved.thinking {
        let reference = resolved.reference();
        let mut notices = Vec::new();
        if let Err(failure) = crate::settings::thinking(
            Some(level),
            None,
            config,
            resolved.model,
            &reference,
            &mut notices,
        ) {
            return Err(vec![failure.message]);
        }
        return Ok(format!("{reference}:{level}"));
    }
    Ok(resolved.reference())
}

/// Builds the child's command from its launch: the running binary
/// re-executed as a one-turn child session of its parent
/// (`docs/delegates.md`, "Lifetime"). The runner sets the stdio, the
/// process group and the lifeline around it.
pub(crate) fn launcher(fiber: &Path) -> jobs::Launch {
    let fiber = fiber.to_path_buf();
    Arc::new(move |launched: &jobs::Launched| command(&fiber, launched))
}

fn command(fiber: &Path, launched: &jobs::Launched) -> Command {
    let mut command = Command::new(fiber);
    command.args([
        "session",
        "--id",
        &launched.session_id.0,
        "--workspace",
        &launched.workspace.to_string_lossy(),
        "--model",
        &launched.model,
        "--prompt",
        &launched.prompt,
        "--parent",
        &launched.parent.0,
        "--delegate-id",
        &launched.job_id.0,
    ]);
    command
}

/// Watches the child's socket as an ordinary client, folding its log from
/// the subscribe: the jobs-local outcome over `doors::watch`
/// (`docs/delegates.md`, "Streams").
pub(crate) fn watcher(home: &Path) -> jobs::Watch {
    let home = home.to_path_buf();
    Arc::new(move |id, on_line| {
        doors::watch(&home, id, on_line).map(|watched| match watched {
            doors::Watched::Exited => jobs::Watched::Exited,
            doors::Watched::Closed { .. } => jobs::Watched::Closed,
        })
    })
}

#[cfg(test)]
#[path = "delegates_tests.rs"]
mod tests;
