//! What a session is built from, read before it exists
//! (`docs/architecture.md`): the parts a door runs, composed once from
//! configuration, the installed providers and the credential.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::connect::{Here, connect};
use config::{Config, Sources};
use contract::provider::Provider;
use contract::shapes::Failure;
use contract::{ErrorCode, ThinkingLevel};
use doors::failure;
use extensions::Providers;
use r#loop::Model;

use crate::{
    cost, failed, handoff, lua_providers, mcp_servers, prompt_files, scripted, settings, switch,
};

/// What a session is built from, read before it exists.
pub(crate) struct Parts {
    pub(crate) home: PathBuf,
    pub(crate) sessions: PathBuf,
    pub(crate) workspace: PathBuf,
    pub(crate) project: config::ProjectKey,
    pub(crate) provider: Arc<dyn Provider>,
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
    /// Every configured `file` credential source, relative paths joined
    /// with the workspace the reader reads them from
    /// (`docs/permissions.md`, "Credentials").
    pub(crate) credential_files: Vec<PathBuf>,
    /// The installed extensions, started, and their hooks.
    pub(crate) extensions: Arc<extensions::SessionExtensions>,
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
/// `usage_recorded`, and `recorded_credential` its credential label:
/// `--credential` on a resume, else the recorded one; each beats configuration
/// (`docs/model-routing.md`, "Choosing the model"). A log with no
/// `usage_recorded` uses `--model`, then the configured default. A recorded
/// model or label that no longer resolves fails before any line is written.
pub(crate) fn parts_with<'a>(
    overrides: Vec<String>,
    recorded: Option<&str>,
    recorded_credential: impl Into<crate::credential::Labels<'a>>,
    recorded_thinking: Option<ThinkingLevel>,
    clock: Arc<dyn contract::clock::Clock>,
    host: Option<Arc<extensions::HostScript>>,
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
    )
}

/// As [`parts_with`], for the Fiber home `home` and the workspace
/// `workspace` rather than the environment's. `agents_home` is the skills
/// directory the prompt reads; the test passes `None` so the host's skills
/// cannot overflow the fixture model's context.
#[allow(
    clippy::too_many_arguments,
    reason = "one composition of the session's parts: homes, model choice, clock and runner host"
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
) -> Result<Parts, Failure> {
    let (sessions, project) = ::cli::project_of(&home, &workspace)?;
    let config = Config::load(Sources {
        home: home.clone(),
        workspace: workspace.clone(),
        project: project.clone(),
        overrides: overrides.clone(),
    })
    .map_err(|e| failed(e.code(), e))?;
    // Configuration notices first, then loading, discovery and
    // placeholders, then the thinking notice below: each load's notices
    // are emitted once, after `fiber_started` with the MCP notices.
    let mut startup_notices: Vec<contract::events::Notice> = config.notices().to_vec();
    let budget = config
        .get("budget.usd", None)
        .and_then(|(value, _)| value.as_f64());
    let (mut providers, notices) = Providers::load(&home).map_err(|e| failed(e.code(), e))?;
    startup_notices.extend(notices);
    let locks = Arc::new(tools::PathLocks::new());
    let session_locks: Arc<dyn contract::files::PathLock> = locks.clone();
    let mut extensions = extensions::SessionExtensions::load(
        &home,
        &config,
        Arc::clone(&clock),
        session_locks,
        host,
    );
    let (naming, notices) = lua_providers::add_lua(&extensions, &mut providers, &config)?;
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
    // `recorded` first, then `--model` and configuration's `model`
    // (`docs/model-routing.md`, "Choosing the model").
    let model = providers
        .choose(recorded, &config)
        .map_err(|e| failed(e.code(), e))?;
    let context_window = settings::context_window(model.model, &model.reference())?;
    let label = labels.label(&config, model.provider)?;
    let lua = providers.lua(&model.provider.name);
    let lua_providers::Access { key, signer, .. } = scripted::access(model.provider, || {
        lua_providers::session_credential(lua, model.provider, &label, || {
            crate::credential::session_credential(&config, model.provider, Some(&label))
                .map(|(_, key)| key)
        })
        .map(|read| lua_providers::Access::new(lua, read))
    })?;
    let session_credential = (key.clone(), signer.clone());
    let here = Here {
        workspace: workspace.clone(),
        clock: Arc::clone(&clock),
    };
    let provider = connect(model, key, signer, lua, &here)?;
    // The credentials read at startup, seeding `Switching`'s keys: the
    // session's entry first, so a reviewer on its provider reuses them.
    let mut credentials: switch::Credentials = BTreeMap::new();
    credentials.insert(
        model.provider.name.clone(),
        (label.clone(), session_credential),
    );
    let reviewer = {
        let mut lookup =
            |provider: &config::ProviderData| -> Result<lua_providers::Access, Failure> {
                scripted::access(provider, || {
                    let lua = providers.lua(&provider.name);
                    if let Some((_, read)) = credentials.get(&provider.name) {
                        return Ok(lua_providers::Access::new(lua, read.clone()));
                    }
                    let label = config.credentials().credential_label(provider);
                    let read = lua_providers::session_credential(lua, provider, &label, || {
                        crate::credential::session_credential(&config, provider, None)
                            .map(|(_, key)| key)
                    })?;
                    credentials.insert(provider.name.clone(), (label, read.clone()));
                    Ok(lua_providers::Access::new(lua, read))
                })
            };
        choose_reviewer(&providers, &config, &model, &here, &mut lookup)
    };
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
    let limits = settings::block_limits(&config);
    let reviewer_notes = config.reviewer_context();
    let retry = settings::retry_policy(&config);
    let handoff = handoff::handoff_settings(&config, &model.reference());
    let idle = settings::idle_exit(&config);
    let warm = scripted::warm(model.model, settings::warm(&config));
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
    );
    // The extensions loaded above, started before the model was chosen:
    // choosing a Lua provider's model waits on them.
    // The session log's path is set by the caller, which mints the session
    // directory after this returns.
    let mut prompt = r#loop::PromptInputs::new(
        home.clone(),
        std::env::var("SHELL").unwrap_or_else(|_| "unknown".into()),
        String::new(),
        clock,
        context_window,
    );
    prompt.system = prompt_files::system(&home, &project);
    prompt.append = prompt_files::append(&home, &project);
    prompt.agents_home = agents_home;
    prompt.addendum = providers.addendum(&model).map(str::to_owned);
    prompt.extensions = extensions.prompts();
    prompt.extension_dirs = extensions.dirs();
    prompt.extension_sections = extensions.sections(&project);
    prompt.skills_disabled = config.union_list("skills.disabled");
    prompt.skills_disabled_now = Some(settings::skills_disabled_reader(
        home.clone(),
        workspace.clone(),
        project.clone(),
        overrides,
    ));
    prompt.credential = crate::scripted::credential(model.provider, label);
    prompt.cache_lifetime = settings::cache_lifetime(&config, &model.reference());
    prompt.thinking = thinking;
    // The startup notices are written with the MCP notices, after
    // `fiber_started`.
    let mut mcp = mcp_servers::specs(&config);
    mcp.notices.splice(0..0, startup_notices);
    Ok(Parts {
        sessions,
        home,
        workspace,
        project,
        provider,
        prompt,
        model: Model {
            reference: model.reference(),
            cost: model.model.cost.clone().map(cost::declared),
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
        credential_files,
        locks,
        extensions,
        mcp,
        switching,
        // The startup model's `:level` suffix, if any: a resumed session
        // passes none with its recorded model, so the loop's seeded choice
        // stands.
        switchable: r#loop::Switchable {
            chosen: model.thinking,
        },
        resolve: crate::delegates::resolver(&providers, &config),
        web_search: model.model.web_search.clone(),
    })
}

/// The reviewer model's reference: `reviewer.model` when set, else the
/// session model's provider's reviewer model. A missing reviewer is not a
/// startup error: the loop gets it, and every reviewed call escalates it
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
    match &session.provider.reviewer_model {
        Some(id) => Ok(format!("{}/{}", session.provider.name, id)),
        None => Err(failure(ErrorCode::NoModel, r#loop::NO_MODEL_MESSAGE)),
    }
}

/// Who judges step 7's calls: `reviewer.model` when set, else the session
/// model's provider's reviewer model, else no reviewer at all. A failure to
/// resolve the model or to read its credential is not a startup error: the
/// loop gets it, and every reviewed call escalates it
/// (`docs/permissions.md`, "How it runs"). Fiber never reviews with the
/// session's own model. `credential` reads the startup map first, then the
/// provider's configured label, so a reviewer on the session's own provider
/// reuses the session's key and signer, and a `command` credential runs once
/// per process; a switch passes the access its read returned
/// (`docs/model-routing.md`, "Keys, tokens and OAuth").
pub(crate) fn choose_reviewer(
    providers: &Providers,
    config: &Config,
    session: &extensions::Model<'_>,
    here: &Here,
    credential: &mut dyn FnMut(&config::ProviderData) -> Result<lua_providers::Access, Failure>,
) -> Result<r#loop::Reviewer, Failure> {
    let typed = reviewer_reference(config, session)?;
    let model = providers.resolve(&typed).map_err(|e| failed(e.code(), e))?;
    let context_window = settings::context_window(model.model, &model.reference())?;
    // Another provider's reviewer goes through the same path, so a Lua
    // reviewer model works: the token when it registered `credential`,
    // else the key.
    let access = credential(model.provider)?;
    // The token is read once, so a failing `credential()` fails here:
    // not a startup error, the loop gets it and every reviewed call
    // escalates it (`docs/permissions.md`, "How it runs").
    let provider = connect(model, access.key, access.signer, access.lua.as_ref(), here)?;
    Ok(r#loop::Reviewer {
        provider,
        model: Model {
            reference: model.reference(),
            cost: model.model.cost.clone().map(cost::declared),
            subscription: model.model.subscription,
        },
        cache_lifetime: settings::cache_lifetime(config, &model.reference()),
        context_window,
        thinking_levels: model.model.thinking_levels.clone(),
    })
}
