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
use contract::events::VariablesSource;

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
    /// Resolves a `delegate_spawn` `model` argument, and lists the
    /// catalogue every refusal carries.
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
            resolve: self.resolve.clone(),
            launch: Arc::clone(&self.launch),
            watch: Arc::clone(&self.watch),
        }
    }

    /// The `delegate_models` tool this session declares: the catalogue
    /// built with the session's resolver.
    pub(crate) fn models(&self) -> jobs::DelegateModels {
        jobs::DelegateModels {
            catalogue: Arc::clone(&self.resolve.catalogue),
        }
    }
}

/// Resolves a delegate's model through the installed providers and the
/// configured roles: an argument that equals a role name is that role,
/// anything else is a full `fiber:<provider>/<model>[:<level>]` reference
/// (`docs/delegates.md`, "Choosing a model"). An explicit level must be
/// one the model takes, checked with the same rule `--model` uses. A
/// role's target is resolved as a reference only: roles never chain. Both
/// the lookup and the catalogue are built from one providers and
/// configuration snapshot, so they never disagree within a session.
pub(crate) fn resolver(
    providers: &extensions::Providers,
    config: &config::Config,
) -> jobs::Resolve {
    let providers = providers.clone();
    let config = config.clone();
    let listed = providers.clone();
    let read = config.clone();
    jobs::Resolve {
        lookup: Arc::new(move |argument| lookup(&providers, &config, argument)),
        catalogue: Arc::from(catalogue(&listed, &read)),
    }
}

/// Resolves one `delegate_spawn` `model` argument to the
/// `provider/model[:level]` the child is started with, or refuses it with
/// one reason sentence.
fn lookup(
    providers: &extensions::Providers,
    config: &config::Config,
    argument: &str,
) -> Result<String, String> {
    if let Some(target) = role_target(config, argument) {
        return resolve_reference(providers, config, &target)
            .map_err(|reason| format!("Role {argument:?} maps to {target:?}: {reason}"));
    }
    resolve_reference(providers, config, argument)
}

/// A role's target reference as configured now: a string value is the
/// target, and an object's string `model` is the target while its
/// `credential` is ignored. Any other shape was already rejected when
/// configuration loaded (`crates/config/src/keys.rs`, `Kind::Role`), so it
/// resolves as no role.
fn role_target(config: &config::Config, name: &str) -> Option<String> {
    let merged = config.merged(None);
    let roles = merged.get("roles")?;
    let value = roles.get(name)?;
    if let Some(target) = value.as_str() {
        return Some(target.to_owned());
    }
    value.get("model")?.as_str().map(str::to_owned)
}

/// Resolves a full `fiber:<provider>/<model>[:<level>]` reference to the
/// `provider/model[:level]` the child is started with, or refuses it with
/// one reason sentence: `Unknown delegate model "<arg>".` for anything
/// not naming an installed model, or the thinking rule's message for a
/// level the model does not take. Without the prefix the reference is
/// refused even when the plain lookup would take it, and a bare id
/// without a provider is refused too.
fn resolve_reference(
    providers: &extensions::Providers,
    config: &config::Config,
    reference: &str,
) -> Result<String, String> {
    let unknown = || format!("Unknown delegate model {reference:?}.");
    let Some(rest) = reference.strip_prefix("fiber:") else {
        return Err(unknown());
    };
    if !rest.contains('/') {
        return Err(unknown());
    }
    let resolved = providers.resolve(rest).map_err(|_| unknown())?;
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
            return Err(failure.message);
        }
        return Ok(format!("{reference}:{level}"));
    }
    Ok(resolved.reference())
}

/// The catalogue text `delegate_models` returns and every model refusal
/// lists: the configured roles, what each maps to now, and the full
/// references available. Roles and references are each sorted by name. A
/// role whose target is not a usable reference is marked ` (unavailable)`.
fn catalogue(providers: &extensions::Providers, config: &config::Config) -> String {
    let mut roles: Vec<(String, String)> = config
        .merged(None)
        .get("roles")
        .and_then(|roles| roles.as_object())
        .map(|roles| {
            roles
                .iter()
                .filter_map(|(name, value)| {
                    let target = if let Some(target) = value.as_str() {
                        target.to_owned()
                    } else {
                        value.get("model")?.as_str()?.to_owned()
                    };
                    Some((name.clone(), target))
                })
                .collect()
        })
        .unwrap_or_default();
    roles.sort();
    let mut references: Vec<String> = Vec::new();
    for name in providers.names() {
        if let Some(provider) = providers.get(name) {
            for model in &provider.models {
                let reference = format!("fiber:{name}/{}", model.id);
                if model.thinking_levels.is_empty() {
                    references.push(reference);
                } else {
                    let levels = model
                        .thinking_levels
                        .iter()
                        .map(|level| level.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    references.push(format!("{reference} (thinking: {levels})"));
                }
            }
        }
    }
    references.sort();
    let roles_text = if roles.is_empty() {
        String::from("Roles: none")
    } else {
        let mut text = String::from("Roles:");
        for (name, target) in &roles {
            let marker = if resolve_reference(providers, config, target).is_ok() {
                ""
            } else {
                " (unavailable)"
            };
            text.push_str(&format!("\n{name}: {target}{marker}"));
        }
        text
    };
    let references_text = if references.is_empty() {
        String::from("References: none")
    } else {
        let mut text = String::from("References:");
        for reference in &references {
            text.push_str(&format!("\n{reference}"));
        }
        text
    };
    format!("{roles_text}\n{references_text}")
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
    let workspace = launched.workspace.to_string_lossy();
    command.args([
        "session",
        "--id",
        &launched.session_id.0,
        "--workspace",
        &workspace,
    ]);
    if launched.resume {
        // A resume continues the delegate's own session with one
        // unattended turn: no `--model` (fixed when the delegate
        // started), and the message as one `--prompt=<text>` argument, so
        // a message starting with `-` reaches the child whole.
        command.args(["--resume", &format!("--prompt={}", launched.prompt)]);
    } else {
        command.args(["--model", &launched.model, "--prompt", &launched.prompt]);
    }
    command.args([
        "--parent",
        &launched.parent.0,
        "--delegate-id",
        &launched.job_id.0,
    ]);
    // A delegate carries its parent's environment, so it carries the
    // parent's source too (`docs/invocation.md`, "A session's
    // environment").
    if r#loop::variables_source() == VariablesSource::LoginShell {
        command.arg("--login-shell");
    }
    command
}

/// Watches the child's socket as an ordinary client, folding its log from
/// the subscribe past the run's boundary: the jobs-local outcome over
/// `doors::watch_after` (`docs/delegates.md`, "Streams").
pub(crate) fn watcher(home: &Path) -> jobs::Watch {
    let home = home.to_path_buf();
    Arc::new(move |id, after, on_line| {
        doors::watch_after(&home, id, after, on_line).map(|watched| match watched {
            doors::Watched::Exited => jobs::Watched::Exited,
            doors::Watched::Closed { .. } => jobs::Watched::Closed,
        })
    })
}

#[cfg(test)]
#[path = "delegates_tests.rs"]
mod tests;
