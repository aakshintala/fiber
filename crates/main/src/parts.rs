//! What a session is built from, read before it exists
//! (`docs/architecture.md`): the parts a door runs, composed once from
//! configuration, the installed providers and the credential.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use config::{Config, Sources};
use contract::diag::DebugLog as _;
use contract::diag::{Observe, Op};
use contract::provider::Provider;
use contract::shapes::Failure;
use contract::{ErrorCode, ThinkingLevel};
use doors::failure;
use extensions::Providers;
use r#loop::Model;
use routing::{Here, connect_observed};

use crate::{
    failed, handoff, lua_providers, mcp_servers, placeholders, prompt_files, scripted,
    session_extensions, settings, switch,
};

/// What a session is built from, read before it exists.
pub(crate) struct Parts {
    pub(crate) sources: Sources,
    pub(crate) home: PathBuf,
    pub(crate) sessions: PathBuf,
    pub(crate) workspace: PathBuf,
    pub(crate) project: config::ProjectKey,
    pub(crate) provider: Arc<dyn Provider>,
    /// Builds the session's observed model calls; `None` calls the
    /// provider directly.
    pub(crate) observe: Option<Observe>,
    /// Builds the reviewer's observed calls; `None` calls the reviewer's
    /// provider directly.
    pub(crate) reviewer_observe: Option<Observe>,
    /// The session's diagnostic writer, at the configured level.
    pub(crate) diag: Arc<log::diag::Diag>,
    pub(crate) model: Model,
    pub(crate) prompt: r#loop::PromptInputs,
    /// Who judges step 7's calls; `Err` leaves every reviewed call to a
    /// person (`docs/permissions.md`, "How it runs").
    pub(crate) reviewer: Result<r#loop::Reviewer, Failure>,
    /// When a reviewer block hands the call to a person.
    pub(crate) limits: r#loop::BlockLimits,
    /// The person's `reviewer.context` notes, rendered once from the
    /// session's configuration (`docs/permissions.md`, "What the person
    /// tells it").
    pub(crate) reviewer_notes: String,
    /// `budget.usd`, or none when the key is absent or not a number.
    pub(crate) budget: Option<f64>,
    /// How a failed model call is retried (`docs/model-routing.md`, "When
    /// a model call fails").
    pub(crate) retry: r#loop::Retry,
    /// `handoff.*` for the session's model.
    pub(crate) handoff: r#loop::HandoffSettings,
    /// How long an idle session waits before it exits.
    pub(crate) idle: Option<Duration>,
    /// How many cache lifetimes an idle session keeps its cache warm;
    /// `None` never warms.
    pub(crate) warm: Option<u32>,
    /// Each configured `tools."<name>".max_result_bytes`, by the tool's
    /// registered name (`docs/tools.md`, "Bounded results").
    pub(crate) caps: r#loop::ResultCaps,
    /// The person's `shell.read_only` entries, read from the session's
    /// configuration and shared with the model shell (`docs/tools.md`,
    /// "Other command-line tools").
    pub(crate) shell_read_only: crate::settings::ShellReadOnly,
    /// The person's OS sandbox settings, read from the session's
    /// configuration and shared with the model shell (`docs/sandbox.md`).
    pub(crate) sandbox: crate::settings::SandboxHandle,
    /// The session's protected paths, shared with the file tools and the
    /// model shell: confined Linux calls take their guard from it, and a
    /// stale baseline recovered at open reported in the startup notices.
    pub(crate) protection: Arc<tools::ProtectedPaths>,
    /// Every configured `file` credential source, relative paths joined
    /// with the workspace the reader reads them from
    /// (`docs/permissions.md`, "Credentials").
    pub(crate) credential_files: Vec<PathBuf>,
    /// The installed extensions, started, and their hooks.
    pub(crate) extensions: Arc<extensions::SessionExtensions>,
    /// Where the session's repository extensions are read, resolved to
    /// the workspace whose declarations count.
    pub(crate) repository: session_extensions::RepositoryScope,
    /// The session's per-path lock, shared by the file tools and `host.fs`.
    pub(crate) locks: Arc<tools::PathLocks>,
    pub(crate) mcp: mcp_servers::Specs,
    /// MCP starts while the session extensions load; joined before tools register.
    pub(crate) pending_mcp: Option<mcp_servers::PendingMcp>,
    /// The second-model preparation, for `Loop::switcher`.
    pub(crate) switching: switch::Switching,
    /// How a `fiber:` reference resolves for this session's delegates.
    pub(crate) resolve: jobs::Resolve,
    /// The session's own thinking choice at start.
    pub(crate) switchable: r#loop::Switchable,
    /// The session model's hosted search type, such as `web_search_20250305`.
    pub(crate) web_search: Option<String>,
}

/// The per-run layer: `--model <m>` reads as a `-c model=<m>` given before
/// every `-c`, so an explicit `-c model=` wins, and among `-c` flags the
/// later one wins.
pub(crate) fn per_run(model: Option<String>, overrides: Vec<String>) -> Vec<String> {
    model
        .map(|model| format!("model={model}"))
        .into_iter()
        .chain(overrides)
        .collect()
}

/// As [`parts_in`], for the Fiber home and workspace the environment
/// names: a live session composes process extensions under its launch
/// identity, starting pending MCP before extension/provider waits.
#[allow(
    clippy::too_many_arguments,
    reason = "one entry composition: the recorded model, credential, thinking, clock, host, reads and diagnostic process"
)]
pub(crate) fn parts_with_session<'a>(
    overrides: Vec<String>,
    recorded: Option<&str>,
    recorded_credential: impl Into<crate::credential::Labels<'a>>,
    recorded_thinking: Option<ThinkingLevel>,
    clock: Arc<dyn contract::clock::Clock>,
    host: Option<Arc<extensions::HostScript>>,
    reads: Arc<switch::Reads>,
    process: log::diag::Process,
    seed: extensions::StateSeed,
    repository: session_extensions::RepositoryScope,
    launch: SessionLaunch,
    fiber: PathBuf,
) -> Result<Parts, Failure> {
    let home = config::fiber_home_from_env().map_err(|e| failed(e.code(), e))?;
    let workspace = std::env::current_dir()
        .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
    parts_inner(
        home,
        workspace,
        overrides,
        recorded,
        recorded_credential.into(),
        recorded_thinking,
        clock,
        host,
        prompt_files::agents_home(std::env::var_os("HOME")),
        reads,
        process,
        seed,
        repository,
        Some(launch),
        Some(fiber),
    )
}

#[allow(
    dead_code,
    clippy::too_many_arguments,
    reason = "the model/inspection/test entry keeps its established composition inputs"
)]
pub(crate) fn parts_in(
    home: PathBuf,
    workspace: PathBuf,
    overrides: Vec<String>,
    recorded: Option<&str>,
    labels: crate::credential::Labels<'_>,
    recorded_thinking: Option<ThinkingLevel>,
    clock: Arc<dyn contract::clock::Clock>,
    host: Option<Arc<extensions::HostScript>>,
    agents_home: Option<PathBuf>,
    reads: Arc<switch::Reads>,
    process: log::diag::Process,
    seed: extensions::StateSeed,
    repository: session_extensions::RepositoryScope,
) -> Result<Parts, Failure> {
    parts_inner(
        home,
        workspace,
        overrides,
        recorded,
        labels,
        recorded_thinking,
        clock,
        host,
        agents_home,
        reads,
        process,
        seed,
        repository,
        None,
        None,
    )
}

/// What a live session composes with beyond [`parts_in`]: the session's
/// id, why it started and the log cursor captured before its processes
/// started. Process activation waits and admits under this identity; a
/// required miss fails the composition before any line is written.
pub(crate) struct SessionLaunch {
    /// The session's id, as `extension_hello` carries it.
    pub id: contract::SessionId,
    /// Why the session started, resent after a restart.
    pub reason: contract::events::ExtensionReason,
    /// The log cursor captured before the first process started.
    pub event_cursor: u64,
}

/// Reads the one startup configuration and constructs its diagnostic writer.
fn load_configuration(
    home: &Path,
    workspace: &Path,
    project: &config::ProjectKey,
    overrides: &[String],
    clock: Arc<dyn contract::clock::Clock>,
    process: log::diag::Process,
) -> Result<(Config, Arc<log::diag::Diag>, Vec<contract::events::Notice>), Failure> {
    // The one monotonic read the `startup.config` timer starts from,
    // taken before configuration is read so the line covers the load
    // and writer construction.
    let started = clock.now();
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: project.clone(),
        overrides: overrides.to_vec(),
    })
    .map_err(|e| failed(e.code(), e))?;
    let diag = Arc::new(log::diag::Diag::new(
        home,
        process,
        if config::diagnostics_debug(&config) {
            log::diag::Level::Debug
        } else {
            log::diag::Level::Info
        },
        clock,
    ));
    drop(log::diag::timed(
        &diag,
        Op::StartupConfig,
        None,
        Some(started),
    ));
    let notices = config.notices().to_vec();
    Ok((config, diag, notices))
}

/// Settles one composition step for a live session: a failure after
/// process activation abandons what started, bounded, so no descendant
/// outlives the failed composition. Model, inspection and test paths
/// pass no launch and settle unchanged.
fn settle<T>(
    result: Result<T, Failure>,
    extensions: &extensions::SessionExtensions,
    launch: &Option<SessionLaunch>,
) -> Result<T, Failure> {
    result.inspect_err(|_| {
        if launch.is_some() {
            drop(extensions.finish_processes(&support::cancel::Token::new()));
        }
    })
}

/// Loads the session's extensions: without a launch, the registry-only
/// load model, inspection and test callers use; with one, the session
/// load that starts process extensions under the launch identity. A
/// required process miss abandons what started and fails the composition
/// before any session line is written, as a required MCP server does.
#[allow(
    clippy::too_many_arguments,
    reason = "one extension load: homes, model choice, clock, runner host and launch identity"
)]
fn load_session_extensions(
    home: &Path,
    workspace: &Path,
    project: &config::ProjectKey,
    config: &Config,
    clock: Arc<dyn contract::clock::Clock>,
    locks: Arc<dyn contract::files::PathLock>,
    host: Option<Arc<extensions::HostScript>>,
    seed: extensions::StateSeed,
    set: extensions::RepositorySet,
    launch: &Option<SessionLaunch>,
) -> Result<extensions::SessionExtensions, Failure> {
    let Some(launch) = launch else {
        return Ok(extensions::SessionExtensions::load_repository(
            home, config, clock, locks, host, seed, set,
        ));
    };
    let start = extensions::StartContext {
        session_id: launch.id.clone(),
        reason: launch.reason,
        workspace: workspace.to_path_buf(),
        data_dirs: extensions::DataDirBases {
            home: home.to_path_buf(),
            project: project.clone(),
        },
        drive: None,
        event_cursor: launch.event_cursor,
    };
    let set = extensions::SessionExtensions::load_session(
        home,
        config,
        clock,
        locks,
        host,
        seed,
        set,
        start,
        BTreeSet::new(),
    );
    if let Some(notice) = set
        .notices()
        .into_iter()
        .find(|notice| notice.code == contract::ErrorCode::ExtensionRequiredFailed)
    {
        drop(set.finish_processes(&support::cancel::Token::new()));
        return Err(Failure {
            code: contract::ErrorCode::ExtensionRequiredFailed,
            message: format!(
                "{}: {}",
                notice.extension.unwrap_or_default(),
                notice.message
            ),
            retry_after_ms: None,
            provider: None,
        });
    }
    Ok(set)
}

#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "one composition carries shared session inputs with its launch identity and recorded executable"
)]
fn parts_inner(
    home: PathBuf,
    workspace: PathBuf,
    overrides: Vec<String>,
    recorded: Option<&str>,
    labels: crate::credential::Labels<'_>,
    recorded_thinking: Option<ThinkingLevel>,
    clock: Arc<dyn contract::clock::Clock>,
    host: Option<Arc<extensions::HostScript>>,
    agents_home: Option<PathBuf>,
    reads: Arc<switch::Reads>,
    process: log::diag::Process,
    seed: extensions::StateSeed,
    repository: session_extensions::RepositoryScope,
    launch: Option<SessionLaunch>,
    fiber: Option<PathBuf>,
) -> Result<Parts, Failure> {
    let (sessions, project) = ::cli::project_of(&home, &workspace)?;
    let (config, diag, mut startup_notices) = load_configuration(
        &home,
        &workspace,
        &project,
        &overrides,
        Arc::clone(&clock),
        process,
    )?;
    // The protected paths' baseline: opening recovers a stale baseline when
    // no confined call holds it, and reports what it restored. A baseline
    // that cannot be recorded fails composition before any call runs.
    let (protection, startup) =
        tools::ProtectedPaths::open(&home, &workspace).map_err(|error| match error {
            tools::ProtectedPathsOpenError::Project(message) => {
                failed(ErrorCode::IoFailed, message)
            }
            tools::ProtectedPathsOpenError::HomeNotDirectory { path } => failed(
                ErrorCode::IoFailed,
                format!(
                    "{} is not a directory, so protected-path state cannot be recorded",
                    path.display()
                ),
            ),
            tools::ProtectedPathsOpenError::Recovery(source) => failed(
                ErrorCode::IoFailed,
                format!("protected-path recovery failed: {source}"),
            ),
        })?;
    if let Some(report) = startup {
        startup_notices.extend(report.warning_lines().into_iter().map(|message| {
            contract::events::Notice {
                code: contract::ErrorCode::ProtectedPathsRestored,
                message,
                extension: None,
            }
        }));
    }
    // A live session starts pending MCP before extension/provider waits,
    // so the two startups overlap; other callers compose with no pending
    // start, as before. Configuration refusals stay deferred until the
    // pending start is joined, after provider credential discovery keeps
    // its existing startup order. Dropping the pending owner on any early
    // return below joins its start, then stops its servers.
    let (preloaded_mcp, pending_mcp) = match (&launch, fiber) {
        (Some(launch), Some(fiber)) => {
            let (mcp_start, mcp) = match mcp_servers::specs(&config) {
                Ok(mut mcp) => {
                    let start = Ok((
                        std::mem::take(&mut mcp.specs),
                        std::mem::take(&mut mcp.repository),
                    ));
                    (start, mcp)
                }
                Err(failure) => (Err(failure), mcp_servers::Specs::default()),
            };
            let timing = diag.debugging().then(|| diag.timings());
            let artifacts = log::SessionDir::new(&sessions, &launch.id).artifacts();
            let images = crate::builtin::session_images(fiber, &artifacts, timing.clone());
            let pending = mcp_servers::start_session_tools(
                &home,
                &workspace,
                &artifacts,
                &clock,
                &project,
                mcp_start,
                repository.skipped.clone(),
                images,
                timing,
            )?;
            (Some(mcp), Some(pending))
        }
        _ => (None, None),
    };
    // Loading extensions runs under its own timer, from the installed
    // provider packages through the session extensions' load. The budget
    // read above stays outside, and the diagnostics below stay outside:
    // the guard ends before either.
    let extensions_timing = log::diag::timed(&diag, Op::StartupExtensions, None, None);
    // The repository set loads from the scope's workspace, or the launch
    // directory when it names none; skipped content stays out.
    let repository = session_extensions::RepositoryScope {
        workspace: Some(
            repository
                .workspace
                .clone()
                .unwrap_or_else(|| workspace.clone()),
        ),
        skipped: repository.skipped.clone(),
    };
    let set = session_extensions::repository_set(&home, &project, &workspace, &repository, &clock);
    let (mut providers, notices) =
        Providers::load_repository(&home, &config, &set).map_err(|e| failed(e.code(), e))?;
    startup_notices.extend(notices);
    let locks = Arc::new(tools::PathLocks::new());
    let session_locks: Arc<dyn contract::files::PathLock> = locks.clone();
    let mut extensions = load_session_extensions(
        &home,
        &workspace,
        &project,
        &config,
        Arc::clone(&clock),
        session_locks,
        host,
        seed,
        set,
        &launch,
    )?;
    drop(extensions_timing);
    // A required repository extension that did not load fails the start
    // before any session line is written.
    settle(
        session_extensions::required(&extensions),
        &extensions,
        &launch,
    )?;
    // Declared hooks with no extension to run them and any `blocking`
    // entry fail the start closed, so a guard never fails open. The hooks
    // gate reads the session's filtered repository contribution, so an
    // unapproved entry stays withheld.
    settle(
        session_extensions::missing_hooks(&config, &extensions),
        &extensions,
        &launch,
    )?;
    // The session's diagnostics, installed after load and before any
    // provider work: at debug the installed writer observes eligible
    // provider HTTP and times non-tool hook chains, and the retained sink
    // reaches provider VMs that start late. Any other level stays silent.
    if diag.debugging() {
        extensions = extensions.with_diagnostics(
            diag.timings(),
            Arc::clone(&diag) as Arc<dyn contract::diag::DebugLog>,
        );
    }
    // Routing runs under its own timer, from adding the Lua providers
    // through choosing the model, reading its credential, connecting
    // and choosing the reviewer. Unloading below stays outside: the
    // guard ends before it. A failure after entry still emits the line
    // on drop, with the unchanged error.
    let routing_timing = log::diag::timed(&diag, Op::StartupRouting, None, None);
    // One runner for every startup credential read: the session key, the
    // Lua discovery and refresh gate, and the reviewer lookup all run
    // through the session's reads, so one cancel reaches them all.
    let run = |command: &mut std::process::Command, timeout: std::time::Duration| {
        reads.credential(command, timeout, &*clock)
    };
    let lookup = placeholders::lookup(&home, &providers, clock.as_ref());
    let (naming, notices) = settle(
        lua_providers::add_lua(&extensions, &mut providers, &config, &run, &lookup),
        &extensions,
        &launch,
    )?;
    startup_notices.extend(notices);
    scripted::prepare(&mut providers, &config, recorded);
    let owners = (extensions.lua_providers().iter())
        .map(|(extension, lua)| (lua.name().to_owned(), extension.clone()))
        .collect();
    // Every `file` credential source configuration declares, and every path
    // a provider's data declares its `credential()` reads, after the Lua
    // providers are added so theirs are protected too: a relative path
    // joins the workspace the reader reads it from, an absolute one stands,
    // and each resolves as a call's path does (`docs/permissions.md`,
    // "Credentials").
    let credential_files: Vec<PathBuf> = config
        .credentials()
        .credential_files(
            providers.names().filter_map(|name| providers.get(name)),
            &|name| std::env::var(name).ok(),
        )
        .into_iter()
        .map(|file| config::resolve_declared(&workspace.join(file)))
        .collect();
    // The per-run layer first (`--model`, then `-c model=`), then the
    // recorded model, then configuration's `model`
    // (`docs/model-routing.md`, "Choosing the model").
    let model = settle(
        providers
            .choose(recorded, &config)
            .map_err(|e| failed(e.code(), e)),
        &extensions,
        &launch,
    )?;
    let context_window = settle(
        settings::context_window(model.model, &model.reference()),
        &extensions,
        &launch,
    )?;
    let label = settle(labels.label(&config, model.provider), &extensions, &launch)?;
    let lua = providers.lua(&model.provider.name);
    let lua_providers::Access { key, signer, .. } = settle(
        scripted::access(model.provider, || {
            lua_providers::session_credential(lua, model.provider, &label, || {
                if let Some(asked) = labels.asked {
                    let listed = config.credentials().labels(model.provider);
                    if !listed.iter().any(|label| label == asked) {
                        return Err(crate::credential::no_label(
                            &model.provider.name,
                            asked,
                            &listed,
                        ));
                    }
                }
                crate::credential::session_credential(&config, model.provider, Some(&label), &run)
                    .map(|(_, key)| key)
            })
            .map(|read| lua_providers::Access::new(lua, read))
        }),
        &extensions,
        &launch,
    )?;
    let session_credential = (key.clone(), signer.clone());
    let here = Here {
        workspace: workspace.clone(),
        clock: Arc::clone(&clock),
        cache_diagnostics: config.flag("cache.diagnostics", None).unwrap_or_default(),
    };
    let (provider, observe) = settle(
        connect_observed(
            model,
            key,
            signer,
            lua.map(|lua| (lua, label.as_str())),
            &here,
            &diagnostics(&diag),
        ),
        &extensions,
        &launch,
    )?;
    // The credentials read at startup, seeding `Switching`'s keys: the
    // session's entry first, so a reviewer on its provider reuses them.
    let mut credentials: switch::Credentials = BTreeMap::new();
    credentials.insert(
        model.provider.name.clone(),
        (label.clone(), session_credential),
    );
    let choose = {
        let mut lookup =
            |provider: &config::ProviderData| -> Result<(String, lua_providers::Access), Failure> {
                let access = scripted::access(provider, || {
                    let lua = providers.lua(&provider.name);
                    if let Some((_, read)) = credentials.get(&provider.name) {
                        return Ok(lua_providers::Access::new(lua, read.clone()));
                    }
                    let label = config.credentials().credential_label(provider);
                    let read = lua_providers::session_credential(lua, provider, &label, || {
                        crate::credential::session_credential(&config, provider, None, &run)
                            .map(|(_, key)| key)
                    })?;
                    credentials.insert(provider.name.clone(), (label, read.clone()));
                    Ok(lua_providers::Access::new(lua, read))
                })?;
                // The label the access was read for: the startup map's, else the
                // provider's configured one. A scripted reviewer reads no
                // credential, so its label is only the configured default.
                let label = credentials
                    .get(&provider.name)
                    .map(|(label, _)| label.clone())
                    .unwrap_or_else(|| config.credentials().credential_label(provider));
                Ok((label, access))
            };
        choose_reviewer(&providers, &config, &model, &here, &mut lookup, Some(&diag))
    };
    // A reviewer that failed to resolve or read is not a startup error:
    // the loop gets the failure and every reviewed call escalates it,
    // with no factory (`docs/permissions.md`, "How it runs").
    let (reviewer, reviewer_observe) = match choose {
        Ok((reviewer, observe)) => (Ok(reviewer), observe),
        Err(failure) => (Err(failure), None),
    };
    drop(routing_timing);
    // Every refreshed provider the session does not use is unloaded once
    // its list is written: only the session's and the reviewer's stay
    // loaded, held by `Switching` (`docs/model-routing.md`, "Model
    // discovery"). The switch registry is `providers` holding no Lua handle.
    let loaded: Vec<_> = {
        let mut keep = vec![model.provider.name.as_str()];
        if let Some(name) = reviewer
            .as_ref()
            .ok()
            .and_then(|judge| judge.model.reference.split_once('/').map(|(name, _)| name))
        {
            keep.push(name);
        }
        extensions.retain_lua_providers(&keep);
        (extensions.lua_providers().iter())
            .map(|(_, lua)| (lua.name().to_owned(), Arc::clone(lua)))
            .collect()
    };
    extensions.retain_lua_providers(&[]);
    let extensions = Arc::new(extensions);
    let (
        patch,
        r#loop::ReloadSettings {
            limits,
            reviewer_notes,
            retry,
            handoff,
            idle_exit: idle,
            warm_cap: warm,
            budget,
        },
    ) = reload_inputs(
        &config,
        &home,
        &project,
        &extensions,
        model.model,
        &model.reference(),
        &mut startup_notices,
    );
    let thinking = settle(
        settings::thinking(
            model.thinking,
            recorded_thinking,
            &config,
            model.model,
            &model.reference(),
            &mut startup_notices,
        )
        .map_err(|e| failed(e.code, e.message)),
        &extensions,
        &launch,
    )?;
    let loader = switch::Loader {
        extensions: Arc::clone(&extensions),
        clock: Arc::clone(&clock),
        locks: locks.clone(),
        owners,
        workspace: workspace.clone(),
    };
    let switching = switch::Switching::new(
        providers.clone(),
        naming,
        config.clone(),
        credentials,
        loaded,
        loader,
        reads,
    );
    // The model's provider file loads at its credential read; other
    // provider files wait for their own first use.
    // The session log's path is set by the caller, which mints the session
    // directory after this returns.
    let mut prompt = r#loop::PromptInputs::new(
        home.clone(),
        std::env::var("SHELL").unwrap_or_else(|_| "unknown".into()),
        String::new(),
        clock,
        context_window,
    );
    prompt.system = patch.system;
    prompt.append = patch.append;
    prompt.agents_home = agents_home;
    prompt.docs = Some(prompt_files::docs_place(&home, env!("CARGO_PKG_VERSION")));
    prompt.addendum = providers.addendum(&model).map(str::to_owned);
    prompt.extensions = patch.extensions;
    prompt.extension_dirs = patch.extension_dirs;
    prompt.extension_sections = patch.extension_sections;
    prompt.skills_disabled = patch.skills_disabled;
    prompt.skills_disabled_now = Some(settings::skills_disabled_reader(
        home.clone(),
        workspace.clone(),
        project.clone(),
        overrides.clone(),
    ));
    prompt.credential = crate::scripted::credential(model.provider, label);
    prompt.deferrals = patch.deferrals;
    prompt.cache_lifetime = patch.cache_lifetime;
    prompt.thinking = thinking;
    // The startup notices are written with the MCP notices, after
    // `fiber_started`.
    let sandbox_settings = settings::sandbox_settings(&config, &home, &credential_files);
    if sandbox_settings.enabled
        && let Err(unavailable) = tools::sandbox_check_available(&sandbox_settings, &workspace)
    {
        // One notice names the unavailable case; deny/write overlap is a
        // bad configuration, every other case a tool error
        // (`docs/errors.md`, "Sandbox").
        startup_notices.push(contract::events::Notice {
            code: unavailable.code(),
            message: format!("sandbox unavailable: {unavailable}"),
            extension: None,
        });
    }
    let sandbox = std::sync::Arc::new(std::sync::Mutex::new(sandbox_settings));
    let mut mcp = match preloaded_mcp {
        Some(mcp) => mcp,
        None => settle(mcp_servers::specs(&config), &extensions, &launch)?,
    };
    mcp.notices.splice(0..0, startup_notices);
    Ok(Parts {
        sources: Sources {
            home: home.clone(),
            workspace: workspace.clone(),
            project: project.clone(),
            overrides,
        },
        sessions,
        home,
        workspace,
        project,
        repository,
        provider,
        observe,
        reviewer_observe,
        diag,
        prompt,
        model: Model {
            reference: model.reference(),
            cost: model.model.cost.clone(),
            subscription: model.model.subscription,
        },
        reviewer,
        limits,
        reviewer_notes,
        budget,
        retry,
        handoff,
        idle,
        warm,
        caps: settings::result_caps(&config),
        shell_read_only: std::sync::Arc::new(std::sync::Mutex::new(settings::shell_read_only(
            &config,
        ))),
        sandbox,
        protection,
        credential_files,
        locks,
        extensions,
        mcp,
        pending_mcp,
        switching,
        // The startup model's `:level` suffix, if any: a resume with
        // `--model <m>:<level>` passes one with its per-run model, else the
        // loop keeps its seeded choice, the recorded one.
        switchable: r#loop::Switchable {
            chosen: model.thinking,
        },
        resolve: crate::delegates::resolver(&providers, &config),
        web_search: model.model.web_search.clone(),
    })
}

pub(crate) fn reload_inputs(
    config: &Config,
    home: &std::path::Path,
    project: &config::ProjectKey,
    extensions: &extensions::SessionExtensions,
    model: &config::ModelData,
    reference: &str,
    notices: &mut Vec<contract::events::Notice>,
) -> (r#loop::PromptPatch, r#loop::ReloadSettings) {
    (
        r#loop::PromptPatch {
            system: config::system_prompt(home, project),
            append: config::append_prompt(home, project),
            extensions: extensions.prompts(),
            extension_dirs: extensions.dirs(),
            extension_sections: extensions.sections(project),
            skills_disabled: config.union_list("skills.disabled"),
            deferrals: settings::deferrals(config, notices),
            cache_lifetime: settings::cache_lifetime(config, reference),
        },
        r#loop::ReloadSettings {
            limits: settings::block_limits(config),
            reviewer_notes: config.reviewer_context(),
            retry: settings::retry_policy(config),
            handoff: handoff::handoff_settings(config, reference),
            idle_exit: Some(settings::idle_exit(config)),
            warm_cap: scripted::warm(model, settings::warm(config)),
            budget: config
                .get("budget.usd", None)
                .and_then(|(value, _)| value.as_f64()),
        },
    )
}

/// The reviewer's model: `routing::reviewer`'s lookup, where no source is
/// `no_model` with the loop's message. A missing reviewer is not a startup
/// error: the loop gets it, and every reviewed call escalates it
/// (`docs/permissions.md`, "How it runs").
pub(crate) fn reviewer_model<'a>(
    providers: &'a Providers,
    config: &Config,
    session: &extensions::Model<'_>,
) -> Result<extensions::Model<'a>, Failure> {
    routing::reviewer::model(providers, config, session)?
        .ok_or_else(|| failure(ErrorCode::NoModel, r#loop::NO_MODEL_MESSAGE))
}

/// The session's `Diag` as routing's observed factories take it: the writer
/// and the connector timing, both from the one shared `Diag`.
pub(crate) fn diagnostics(diag: &Arc<log::diag::Diag>) -> routing::Diagnostics {
    routing::Diagnostics {
        log: Arc::clone(diag) as Arc<dyn contract::diag::DebugLog>,
        timings: diag.timings(),
    }
}

/// Who judges step 7's calls: `reviewer.model` when set, else the session
/// provider's own `providers."<name>".reviewer_model`, else the package's
/// `reviewer_model`, else no reviewer at all. A failure to
/// resolve the model or to read its credential is not a startup error: the
/// loop gets it, and every reviewed call escalates it
/// (`docs/permissions.md`, "How it runs"). There is no automatic fallback
/// to the session's own model: only an explicit pick may name it.
/// `credential` reads the startup map first, then the
/// provider's configured label, so a reviewer on the session's own provider
/// reuses the session's key and signer, and a `command` credential runs once
/// per process; a switch passes the access its read returned
/// (`docs/model-routing.md`, "Keys, tokens and OAuth"). The lookup returns
/// the label beside the access, so the reviewer's quota is fetched for the
/// account the lookup read (`docs/tools.md`, "Provider quota").
pub(crate) fn choose_reviewer(
    providers: &Providers,
    config: &Config,
    session: &extensions::Model<'_>,
    here: &Here,
    credential: &mut impl FnMut(
        &config::ProviderData,
    ) -> Result<(String, lua_providers::Access), Failure>,
    diag: Option<&Arc<log::diag::Diag>>,
) -> Result<(r#loop::Reviewer, Option<Observe>), Failure> {
    let diag = diag.map(diagnostics);
    let chosen =
        routing::reviewer::choose(providers, config, session, here, credential, diag.as_ref())?
            .ok_or_else(|| failure(ErrorCode::NoModel, r#loop::NO_MODEL_MESSAGE))?;
    let model = chosen.model;
    Ok((
        r#loop::Reviewer {
            provider: chosen.provider,
            model: Model {
                reference: model.reference(),
                cost: model.model.cost.clone(),
                subscription: model.model.subscription,
            },
            cache_lifetime: settings::cache_lifetime(config, &model.reference()),
            context_window: chosen.context_window,
            thinking_levels: model.model.thinking_levels.clone(),
        },
        chosen.observe,
    ))
}
