//! What a session is built from, read before it exists
//! (`docs/architecture.md`): the parts a door runs, composed once from
//! configuration, the installed providers and the credential.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::connect::{Here, connect, connect_observed};
use config::{Config, Sources};
use contract::diag::DebugLog as _;
use contract::diag::{Observe, Op};
use contract::provider::Provider;
use contract::shapes::Failure;
use contract::{ErrorCode, ThinkingLevel};
use doors::failure;
use extensions::Providers;
use r#loop::Model;

use crate::{
    failed, handoff, lua_providers, mcp_servers, prompt_files, scripted, session_extensions,
    settings, switch,
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

/// Fiber home, configuration, the chosen model, its credential and its
/// provider: everything a failure of which leaves no session. `overrides`
/// is the per-run layer: `model=<m>` from `--model` first, then each `-c`,
/// so a later entry wins. `recorded` is the resumed session's model, from the log's last
/// `preamble_built` or `model_changed`: the fallback when the per-run layer
/// names no model, before configuration's default. `recorded_credential`
/// is its credential label: `--credential` on a resume, else the recorded
/// one; each beats configuration (`docs/model-routing.md`, "Choosing the
/// model"). A log with no recorded model uses the per-run layer, then the
/// configured default. A model or label that no longer resolves fails before
/// any line is written.
#[allow(
    clippy::too_many_arguments,
    reason = "one entry composition: the recorded model, credential, thinking, clock, host, reads and diagnostic process"
)]
pub(crate) fn parts_with<'a>(
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
) -> Result<Parts, Failure> {
    let home = config::fiber_home_from_env().map_err(|e| failed(e.code(), e))?;
    let workspace = std::env::current_dir()
        .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
    parts_in(
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
    )
}

/// As [`parts_with`], for the Fiber home `home` and the workspace
/// `workspace` rather than the environment's. `agents_home` is the skills
/// directory the prompt reads; the test passes `None` so the host's skills
/// cannot overflow the fixture model's context.
#[allow(
    clippy::too_many_arguments,
    reason = "one composition of the session's parts: homes, model choice, clock, runner host and diagnostic process"
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
    let (sessions, project) = ::cli::project_of(&home, &workspace)?;
    // The one monotonic read the `startup.config` timer starts from,
    // taken before the configuration is read so the line covers the
    // load and the writer's construction. At `info` the timer stays
    // shut, so this stamp is the only extra cost.
    let started = clock.now();
    let config = Config::load(Sources {
        home: home.clone(),
        workspace: workspace.clone(),
        project: project.clone(),
        overrides: overrides.clone(),
    })
    .map_err(|e| failed(e.code(), e))?;
    // One writer for the process, at the configured level: the session's
    // loop, its factories and its exit sample share it once the durable
    // log exists. A process reads the level when it starts, so a changed
    // value reaches new processes only.
    let diag = Arc::new(log::diag::Diag::new(
        &home,
        process,
        if config::diagnostics_debug(&config) {
            log::diag::Level::Debug
        } else {
            log::diag::Level::Info
        },
        Arc::clone(&clock),
    ));
    // The configuration timer ends here, dropped at once so it covers
    // the load and the writer above and ends before any extension work.
    // A failed load returns before any writer exists, so it writes
    // nothing and nothing re-reads the configuration.
    drop(log::diag::timed(
        &diag,
        Op::StartupConfig,
        None,
        Some(started),
    ));
    // Configuration notices first, then loading, discovery and
    // placeholders, then the thinking notice below: each load's notices
    // are emitted once, after `fiber_started` with the MCP notices.
    let mut startup_notices: Vec<contract::events::Notice> = config.notices().to_vec();
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
    let mut extensions = extensions::SessionExtensions::load_repository(
        &home,
        &config,
        Arc::clone(&clock),
        session_locks,
        host,
        seed,
        set,
    );
    drop(extensions_timing);
    // A required repository extension that did not load fails the start
    // before any session line is written.
    session_extensions::required(&extensions)?;
    // Declared hooks with no extension to run them and any `blocking`
    // entry fail the start closed, so a guard never fails open.
    session_extensions::missing_hooks(&config, &extensions)?;
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
    let (naming, notices) = lua_providers::add_lua(
        &extensions,
        &mut providers,
        &config,
        &run,
        &home,
        clock.as_ref(),
    )?;
    startup_notices.extend(notices);
    scripted::prepare(&mut providers, &config, recorded);
    let owners = (extensions.lua_providers().iter())
        .map(|(extension, lua)| (lua.name().to_owned(), extension.clone()))
        .collect();
    // Every `file` credential source configuration declares, after the Lua
    // providers are added so their sources are protected too: a relative
    // path joins the workspace the reader reads it from, an absolute one
    // stands (`docs/permissions.md`, "Credentials").
    let credential_files: Vec<PathBuf> = config
        .credentials()
        .credential_files(providers.names().filter_map(|name| providers.get(name)))
        .into_iter()
        .map(|file| workspace.join(file))
        .collect();
    // The per-run layer first (`--model`, then `-c model=`), then the
    // recorded model, then configuration's `model`
    // (`docs/model-routing.md`, "Choosing the model").
    let model = providers
        .choose(recorded, &config)
        .map_err(|e| failed(e.code(), e))?;
    let context_window = settings::context_window(model.model, &model.reference())?;
    let label = labels.label(&config, model.provider)?;
    let lua = providers.lua(&model.provider.name);
    let lua_providers::Access { key, signer, .. } = scripted::access(model.provider, || {
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
    })?;
    let session_credential = (key.clone(), signer.clone());
    let here = Here {
        workspace: workspace.clone(),
        clock: Arc::clone(&clock),
        cache_diagnostics: config.flag("cache.diagnostics", None).unwrap_or_default(),
    };
    let (provider, observe) = connect_observed(
        model,
        key,
        signer,
        lua.map(|lua| (lua, label.as_str())),
        &here,
        &diag,
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
    );
    let thinking = settings::thinking(
        model.thinking,
        recorded_thinking,
        &config,
        model.model,
        &model.reference(),
        &mut startup_notices,
    )
    .map_err(|e| failed(e.code, e.message))?;
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
    prompt.cache_lifetime = patch.cache_lifetime;
    prompt.thinking = thinking;
    // The startup notices are written with the MCP notices, after
    // `fiber_started`.
    let mut mcp = mcp_servers::specs(&config)?;
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
        credential_files,
        locks,
        extensions,
        mcp,
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
) -> (r#loop::PromptPatch, r#loop::ReloadSettings) {
    (
        r#loop::PromptPatch {
            system: config::system_prompt(home, project),
            append: config::append_prompt(home, project),
            extensions: extensions.prompts(),
            extension_dirs: extensions.dirs(),
            extension_sections: extensions.sections(project),
            skills_disabled: config.union_list("skills.disabled"),
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

/// The reviewer model's reference: `reviewer.model` when set, else the
/// session provider's own `providers."<name>".reviewer_model`, else the
/// package's `reviewer_model`, else no reviewer. The provider lookup reads
/// the effective layers, so a project value wins over the global one and
/// a repository value is ignored; the stored ID is local and gains the
/// session provider's prefix once. A missing reviewer is not a startup
/// error: the loop gets it, and every reviewed call escalates it
/// (`docs/permissions.md`, "How it runs").
pub(crate) fn reviewer_reference(
    config: &Config,
    session: &extensions::Model<'_>,
) -> Result<String, Failure> {
    if let Some(typed) = config
        .get("reviewer.model", None)
        .and_then(|(value, _)| value.as_str().map(str::to_owned))
    {
        return Ok(typed);
    }
    // Segments, never dotted text: a provider name holding a dot stays
    // one segment.
    let local = [
        "providers".to_owned(),
        session.provider.name.clone(),
        "reviewer_model".to_owned(),
    ];
    if let Some((value, _)) = config.get_segments(&local, None)
        && let Some(id) = value.as_str()
    {
        return Ok(format!("{}/{}", session.provider.name, id));
    }
    match &session.provider.reviewer_model {
        Some(id) => Ok(format!("{}/{}", session.provider.name, id)),
        None => Err(failure(ErrorCode::NoModel, r#loop::NO_MODEL_MESSAGE)),
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
    let typed = reviewer_reference(config, session)?;
    let model = providers.resolve(&typed).map_err(|e| failed(e.code(), e))?;
    let context_window = settings::context_window(model.model, &model.reference())?;
    // Another provider's reviewer goes through the same path, so a Lua
    // reviewer model works: the token when it registered `credential`,
    // else the key.
    let (label, access) = credential(model.provider)?;
    // The token is read once, so a failing `credential()` fails here:
    // not a startup error, the loop gets it and every reviewed call
    // escalates it (`docs/permissions.md`, "How it runs").
    let (provider, reviewer_observe) = match diag {
        Some(diag) => connect_observed(
            model,
            access.key,
            access.signer,
            access.lua.as_ref().map(|lua| (lua, label.as_str())),
            here,
            diag,
        )?,
        None => (
            connect(
                model,
                access.key,
                access.signer,
                access.lua.as_ref().map(|lua| (lua, label.as_str())),
                here,
            )?,
            None,
        ),
    };
    Ok((
        r#loop::Reviewer {
            provider,
            model: Model {
                reference: model.reference(),
                cost: model.model.cost.clone(),
                subscription: model.model.subscription,
            },
            cache_lifetime: settings::cache_lifetime(config, &model.reference()),
            context_window,
            thinking_levels: model.model.thinking_levels.clone(),
        },
        reviewer_observe,
    ))
}
