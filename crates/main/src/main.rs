//! The `fiber` binary, and the composition root (`docs/architecture.md`):
//! parses argv, builds the session's parts once from configuration, the
//! installed providers and the credential, and hands them to a door
//! (`docs/invocation.md`). It holds no feature logic.

#![allow(
    clippy::print_stderr,
    reason = "main prints the usage sentence (docs/code-quality.md, \"Lints\")"
)]

mod builtin;
mod cli;
mod clock;
mod connect;
mod cost;
mod crash;
mod credential;
mod extension_command;
mod handoff;
mod hub_command;
mod late_emit;
mod lua_providers;
mod mcp_servers;
mod prompt_files;
mod resume;
mod session_command;
mod session_extensions;
mod settings;
mod shutdown;

#[cfg(test)]
#[path = "live_tests.rs"]
mod live_tests;
#[cfg(test)]
#[path = "reviewer_tests.rs"]
mod reviewer_tests;

use std::fmt::Display;
use std::io::{self, IsTerminal, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use config::{Config, Layer, Sources};
use connect::connect;
use contract::inbox::Delivery;
use contract::provider::Provider;
use contract::shapes::Failure;
use contract::{ErrorCode, SessionId};
use doors::{Session, failure};
use extensions::Providers;
use log::Log;
use r#loop::{Loop, Model};

/// What a session is built from, read before it exists.
struct Parts {
    home: PathBuf,
    sessions: PathBuf,
    workspace: PathBuf,
    project: config::ProjectKey,
    provider: Arc<dyn Provider>,
    model: Model,
    prompt: r#loop::PromptInputs,
    /// Who judges step 7's calls; `Err` leaves every reviewed call to a
    /// person (`docs/permissions.md`, "How it runs").
    reviewer: Result<r#loop::Reviewer, Failure>,
    /// When a reviewer block hands the call to a person.
    limits: r#loop::BlockLimits,
    /// `budget.usd`, or none when the key is absent or not a number.
    budget: Option<f64>,
    /// How a failed model call is retried (`docs/model-routing.md`, "When
    /// a model call fails").
    retry: r#loop::Retry,
    /// `handoff.*` for the session's model.
    handoff: r#loop::HandoffSettings,
    /// How long an idle session waits before it exits.
    idle: Option<Duration>,
    /// How many cache lifetimes an idle session keeps its cache warm;
    /// `None` never warms.
    warm: Option<u32>,
    /// The installed extensions, started, and their hooks.
    extensions: Arc<extensions::SessionExtensions>,
    /// The session's per-path lock, shared by the file tools and `host.fs`.
    locks: Arc<tools::PathLocks>,
    mcp: mcp_servers::Specs,
    /// The session model's hosted search type, such as `web_search_20250305`.
    web_search: Option<String>,
}

fn main() -> ExitCode {
    ExitCode::from(u8::try_from(run()).unwrap_or(1))
}

fn run() -> i32 {
    // Help and version print before anything reads the home, configuration,
    // credentials, or stdin.
    let clock: Arc<dyn contract::clock::Clock> = Arc::new(clock::System);
    // First: every process writes one crash report and aborts on a panic
    // (`docs/code-quality.md`, "What a panic leaves").
    crash::install(
        std::env::var_os("FIBER_HOME"),
        std::env::var_os("HOME"),
        Arc::clone(&clock),
    );
    match cli::parse() {
        cli::Invocation::Print(error) => {
            // A closed stdout leaves nobody to tell, as `fiber extension list` does.
            error.print().unwrap_or(());
            0
        }
        cli::Invocation::Run(Some(cli::Commands::Version)) => {
            write!(io::stdout().lock(), "{}", cli::version_line()).unwrap_or(());
            0
        }
        cli::Invocation::Run(Some(cli::Commands::Help { command })) => {
            print_help(command.as_slice())
        }
        // `fiber` with no arguments opens the terminal: this tty as a
        // client of the hub, starting one when none runs.
        cli::Invocation::Run(None) => terminal(),
        cli::Invocation::Usage {
            ask: true,
            sentence,
        } => ask_failed(usage(sentence)),
        cli::Invocation::Usage {
            ask: false,
            sentence,
        } => {
            eprintln!("fiber: {sentence}");
            2
        }
        cli::Invocation::Run(Some(cli::Commands::Ask(args))) => {
            match cli::ask_parts(&args.prompt) {
                Ok((prompt, dash)) => ask(args.model, args.resume, prompt, dash, clock),
                Err(sentence) => ask_failed(usage(sentence)),
            }
        }
        cli::Invocation::Run(Some(cli::Commands::Session(args))) => {
            session_command::run(args, clock)
        }
        cli::Invocation::Run(Some(cli::Commands::Hub(command))) => hub_command::run(command),
        cli::Invocation::Run(Some(cli::Commands::Sessions(cmd))) => match cmd {
            cli::SessionsCommands::Delete { cascade, yes, id } => {
                sessions_delete(&id, cascade, yes, clock.as_ref())
            }
            cli::SessionsCommands::Export { id, path } => ::cli::export(&id, path.as_deref()),
        },
        cli::Invocation::Run(Some(cli::Commands::Models(args))) => ::cli::models(
            args.search.as_deref(),
            args.json,
            clock,
            Arc::new(tools::PathLocks::new()),
        ),
        cli::Invocation::Run(Some(cli::Commands::RefreshModelLists { providers })) => {
            ::cli::refresh_model_lists(&providers, clock, Arc::new(tools::PathLocks::new()));
            0
        }
        cli::Invocation::Run(Some(cli::Commands::Extension(cmd))) => {
            extension_command::extension(cmd, clock.as_ref())
        }
        cli::Invocation::Run(Some(cli::Commands::Approve(args))) => ::cli::approve(args.yes),
        cli::Invocation::Run(Some(cli::Commands::Config(cmd))) => match cmd {
            cli::ConfigCommands::Get { key } => ::cli::config_get(&key),
            cli::ConfigCommands::Set {
                project,
                repo,
                key,
                value,
            } => {
                let layer = if repo {
                    Layer::Repository
                } else if project {
                    Layer::Project
                } else {
                    Layer::Global
                };
                ::cli::config_set(layer, &key, &value)
            }
        },
        cli::Invocation::Run(Some(cli::Commands::Login(args))) => {
            ::cli::run_login(args.provider.as_deref(), args.label.as_deref())
        }
        cli::Invocation::Run(Some(cli::Commands::Logout(args))) => {
            let target = match (args.label.as_deref(), args.all) {
                (_, true) => ::cli::LogoutTarget::All,
                (Some(label), false) => ::cli::LogoutTarget::Label(label),
                (None, false) => ::cli::LogoutTarget::Only,
            };
            ::cli::run_logout(args.provider.as_deref(), target)
        }
        // The hidden search subcommands hold no feature logic: they only
        // call into `tools` (`docs/architecture.md`, "The call rules").
        cli::Invocation::Run(Some(cli::Commands::Grep { args })) => tools::grep_main(args),
        cli::Invocation::Run(Some(cli::Commands::Find { args })) => tools::find_main(args),
        // The image child holds no feature logic either: `picture` is the
        // only crate that links image code.
        cli::Invocation::Run(Some(cli::Commands::Image { args })) => picture::main(args),
    }
}

fn print_help(words: &[String]) -> i32 {
    match cli::render_help(words) {
        Ok(text) => {
            // A closed stdout leaves nobody to tell, as `fiber extension list` does.
            let mut out = io::stdout().lock();
            write!(out, "{text}").unwrap_or(());
            0
        }
        Err(sentence) => {
            eprintln!("fiber: {sentence}");
            2
        }
    }
}

fn fail(e: Failure) -> i32 {
    eprintln!("fiber: {}", e.message);
    doors::exit_code(&e)
}

/// `fiber ask`: one session, one turn, its events on stdout.
fn ask(
    model: Option<String>,
    resume: Option<String>,
    arg: Option<String>,
    dash: bool,
    clock: Arc<dyn contract::clock::Clock>,
) -> i32 {
    // First: a signal while the prompt is read exits at once
    // (`docs/invocation.md`, "Shutdown").
    let signals = match doors::Signals::install(Arc::clone(&clock)) {
        Ok(signals) => signals,
        Err(e) => return ask_failed(failed(ErrorCode::IoFailed, format!("signals: {e}"))),
    };
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let prompt = match doors::prompt(arg, dash, &mut stdin.lock(), terminal) {
        Ok(prompt) => prompt,
        Err(e) => return ask_failed(e),
    };
    match resume {
        Some(selector) => resume::ask_resume(selector, model, prompt, clock, &signals),
        None => ask_new(model, prompt, clock, &signals),
    }
}

/// `fiber ask` on a new session.
fn ask_new(
    model: Option<String>,
    prompt: String,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &doors::Signals,
) -> i32 {
    session_command::new_session(
        SessionId(doors::mint("s_")),
        model,
        Some(prompt),
        true,
        clock,
        signals,
    )
}

/// What an `ask` loop judges with: the workspace the session keeps, the
/// credentials beside it, and the standing rules.
fn ask_permissions(
    home: &Path,
    project: &config::ProjectKey,
    workspace: String,
    clock: &Arc<dyn contract::clock::Clock>,
) -> r#loop::Permissions {
    r#loop::Permissions {
        workspace,
        credentials: home.join("credentials"),
        rules: Arc::new(config::RulesFiles::new(
            home.to_path_buf(),
            project.clone(),
            Arc::clone(clock),
        )),
    }
}

/// Finishes a loop, however it started: the budget, whether a person may
/// answer an approval, and the reviewer every session gets, so a change to
/// the chain is made once. `answerable` is false only for one-turn
/// `fiber ask`, new or resumed; the session command serves clients that may
/// answer (`docs/permissions.md`, "Headless").
#[allow(
    clippy::too_many_arguments,
    reason = "one hand-off of the loop's end: budget, answerability, reviewer, retry and cancel"
)]
fn finish(
    looped: Result<Loop, r#loop::Error>,
    budget: Option<f64>,
    idle: Option<Duration>,
    warm: Option<u32>,
    answerable: bool,
    reviewer: Result<r#loop::Reviewer, Failure>,
    limits: r#loop::BlockLimits,
    retry: r#loop::Retry,
    cancel: Arc<r#loop::TurnCancel>,
) -> Result<(), Failure> {
    looped
        .map(|looped| {
            looped
                .budget(budget)
                .idle_exit(idle)
                // Only one-turn `fiber ask` is not answerable, and it exits
                // when its run ends: it never warms.
                .warm(warm.filter(|_| answerable))
                .answerable(answerable)
                .reviewer(reviewer, limits)
                .retry(retry)
                .cancelled_by(cancel)
        })
        .and_then(Loop::run)
        .map_err(|e| failed(e.code(), e))
}

/// Runs one turn of the session `session` writes to `log`, once its log,
/// model and prompt are known, shared by new and resumed sessions: sends
/// the first prompt, closes when `one_turn`, and writes `fiber_exited` for
/// what ran. One cancel signal, `cancel`, serves the door and the loop, so
/// a `cancel` ends the turn the prompt starts; a shutdown's code on it is
/// the exit code.
fn run_turn(
    session: &Session,
    log: &Arc<Log>,
    dir: &Path,
    prompt: Option<String>,
    one_turn: bool,
    cancel: Arc<r#loop::TurnCancel>,
    run: impl FnOnce(Receiver<Delivery>, Arc<r#loop::TurnCancel>) -> Result<(), Failure>,
) -> i32 {
    let door = Arc::clone(&cancel);
    let turn = Arc::clone(&cancel);
    let ran = match prompt {
        // `fiber ask` runs one turn: the prompt, then `close`. It always
        // supplies a prompt; without one it would wait for a client.
        Some(prompt) if one_turn => session.ask(prompt, Arc::new(move || door.cancel()), |inbox| {
            run(inbox, turn)
        }),
        // The session command queues its prompt when one was supplied and
        // serves clients until idle exit or `close`.
        prompt => session.serve(prompt, Arc::new(move || door.cancel()), |inbox| {
            run(inbox, turn)
        }),
    };
    // `fiber_exited` is the last line: nothing on the door side follows it.
    session.quiesce();
    // A `fiber_exited` that cannot be written leaves a log that reads as a
    // process that died, which it then is.
    match r#loop::fiber_exited(log, dir, ran, one_turn, cancel.shutdown_code()) {
        Ok(exited) => {
            // `fiber ask` names its failure on stderr, the sentence
            // `fiber_exited` just carried (`docs/errors.md`, "What a
            // caller gets"). The session command's clients read
            // `fiber_exited` itself, so it stays silent.
            if one_turn && let Some(error) = &exited.error {
                writeln!(io::stderr(), "fiber: {}", error.message).unwrap_or(());
            }
            exited.code
        }
        Err(_) => 1,
    }
}

/// Stops the servers, then fails before any session line.
fn stop_and_fail(servers: mcp_servers::SessionServers, e: Failure) -> i32 {
    servers.servers.stop();
    ask_failed(e)
}

/// Fiber home, configuration, the chosen model, its credential and its
/// provider: everything a failure of which leaves no session. `model` is
/// `--model`. `recorded` is the resumed session's model, from the log's last
/// `usage_recorded`, and `recorded_credential` its credential label, from
/// the last `preamble_built`; each beats configuration
/// (`docs/model-routing.md`, "Choosing the model"). A log with no
/// `usage_recorded` uses `--model`, then the configured default. A recorded
/// model or label that no longer resolves fails before any line is written.
fn parts_with(
    model: Option<String>,
    recorded: Option<&str>,
    recorded_credential: Option<&str>,
    clock: Arc<dyn contract::clock::Clock>,
) -> Result<Parts, Failure> {
    let home = config::fiber_home_from_env().map_err(|e| failed(e.code(), e))?;
    let workspace = std::env::current_dir()
        .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
    let (sessions, project) = ::cli::project_of(&home, &workspace)?;
    let config = Config::load(Sources {
        home: home.clone(),
        workspace: workspace.clone(),
        project: project.clone(),
        overrides: model.map(|m| format!("model={m}")).into_iter().collect(),
    })
    .map_err(|e| failed(e.code(), e))?;
    let budget = config
        .get("budget.usd", None)
        .and_then(|(value, _)| value.as_f64());
    // debt: weakens docs/configuration.md, "When Fiber reads configuration",
    // and docs/extensions.md, "The extension API version"; fixed by #382.
    // Notices from configuration and loading are dropped.
    let (mut providers, _notices) = Providers::load(&home).map_err(|e| failed(e.code(), e))?;
    let locks = Arc::new(tools::PathLocks::new());
    let session_locks: Arc<dyn contract::files::PathLock> = locks.clone();
    let mut extensions =
        extensions::SessionExtensions::load(&home, &config, Arc::clone(&clock), session_locks);
    lua_providers::add_lua(&extensions, &mut providers, &config);
    // `recorded` first, then `--model` and configuration's `model`
    // (`docs/model-routing.md`, "Choosing the model").
    let model = providers
        .choose(recorded, &config)
        .map_err(|e| failed(e.code(), e))?;
    let label =
        recorded_credential.map_or_else(|| config.credential_label(model.provider), str::to_owned);
    let (key, signer) = lua_providers::session_credential(&providers, model.provider, || {
        crate::credential::session_credential(&config, model.provider, recorded_credential)
            .map(|(_, key)| key)
    })?;
    let session_credential = (key.clone(), signer.clone());
    let provider = connect(model, key, signer)?;
    let reviewer = choose_reviewer(&providers, &config, &model, &session_credential);
    // Every refreshed provider the session does not use is unloaded once
    // its list is written: only the session's and the reviewer's stay
    // loaded (`docs/model-routing.md`, "Model discovery"). `providers` is
    // a local, so dropping it unloads the rest; the session's and the
    // reviewer's signers hold their own Arcs.
    {
        let mut keep = vec![model.provider.name.as_str()];
        if let Some(name) = reviewer
            .as_ref()
            .ok()
            .and_then(|judge| judge.model.reference.split_once('/').map(|(name, _)| name))
        {
            keep.push(name);
        }
        extensions.retain_lua_providers(&keep);
    }
    let limits = settings::block_limits(&config);
    let retry = settings::retry_policy(&config);
    let handoff = handoff::handoff_settings(&config, &model.reference());
    let idle = settings::idle_exit(&config);
    // A Lua provider builds its own request body, so Fiber cannot show that
    // capping the output changes nothing else in it: it never warms
    // (`docs/prompt-cache.md`, "Warming while idle").
    let warm = settings::warm(&config).filter(|_| providers.lua(&model.provider.name).is_none());
    let thinking = settings::thinking(
        model.thinking,
        None,
        &config,
        model.model,
        &model.reference(),
    )
    .map_err(|e| failed(e.code, e.message))?;
    // The extensions loaded above, started before the model was chosen:
    // choosing a Lua provider's model waits on them.
    // The session log's path is set by the caller, which mints the session
    // directory after this returns.
    let mut prompt = r#loop::PromptInputs::new(
        home.clone(),
        std::env::var("SHELL").unwrap_or_else(|_| "unknown".into()),
        String::new(),
        clock,
    );
    prompt.system = prompt_files::system(&home, &project);
    prompt.append = prompt_files::append(&home, &project);
    prompt.context_window = model.model.context_window;
    prompt.agents_home = prompt_files::agents_home(std::env::var_os("HOME"));
    prompt.addendum = providers.addendum(&model).map(str::to_owned);
    prompt.extensions = extensions.prompts();
    prompt.extension_dirs = extensions.dirs();
    prompt.extension_sections = extensions.sections(&project);
    prompt.skills_disabled = config.union_list("skills.disabled");
    prompt.credential = Some(label);
    prompt.cache_lifetime = settings::cache_lifetime(&config, &model.reference());
    prompt.thinking = thinking;
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
        budget,
        retry,
        handoff,
        idle,
        warm,
        locks,
        extensions: Arc::new(extensions),
        mcp: mcp_servers::specs(&config),
        web_search: model.model.web_search.clone(),
    })
}

/// Who judges step 7's calls: `reviewer.model` when set, else the session
/// model's provider's reviewer model, else no reviewer at all. A failure to
/// resolve the model or to read its credential is not a startup error: the
/// loop gets it, and every reviewed call escalates it
/// (`docs/permissions.md`, "How it runs"). Fiber never reviews with the
/// session's own model. A reviewer on the session's own provider reuses
/// the session's key and signer, so a `command` credential runs once per
/// process; a reviewer elsewhere reads its own configured label.
fn choose_reviewer(
    providers: &Providers,
    config: &Config,
    session: &extensions::Model<'_>,
    session_credential: &lua_providers::KeyAndSigner,
) -> Result<r#loop::Reviewer, Failure> {
    let configured = config
        .get("reviewer.model", None)
        .and_then(|(value, _)| value.as_str().map(str::to_owned));
    let typed = match configured {
        Some(typed) => typed,
        None => match &session.provider.reviewer_model {
            Some(id) => format!("{}/{}", session.provider.name, id),
            None => {
                return Err(failure(ErrorCode::NoModel, r#loop::NO_MODEL_MESSAGE));
            }
        },
    };
    let model = providers.resolve(&typed).map_err(|e| failed(e.code(), e))?;
    // Another provider's reviewer goes through the same path, so a Lua
    // reviewer model works: the token when it registered `credential`,
    // else the key.
    let (key, signer) = if model.provider.name == session.provider.name {
        session_credential.clone()
    } else {
        lua_providers::session_credential(providers, model.provider, || {
            credential::session_credential(config, model.provider, None).map(|(_, key)| key)
        })?
    };
    // The token is read once, so a failing `credential()` fails here:
    // not a startup error, the loop gets it and every reviewed call
    // escalates it (`docs/permissions.md`, "How it runs").
    let provider = connect(model, key, signer)?;
    Ok(r#loop::Reviewer {
        provider,
        model: Model {
            reference: model.reference(),
            cost: model.model.cost.clone().map(cost::declared),
            subscription: model.model.subscription,
        },
        cache_lifetime: settings::cache_lifetime(config, &model.reference()),
    })
}

/// `fiber` with no arguments: the terminal on this tty, a client of the hub
/// (`docs/invocation.md`, "Two doors"). Without a tty it is a usage error
/// naming `fiber ask`. The hub it starts listens on its local socket only
/// and is never waited on.
fn terminal() -> i32 {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!(
            "fiber: The terminal needs a tty; run `fiber ask \"<prompt>\"`. Run `fiber --help` for usage."
        );
        return 2;
    }
    let clock: Arc<dyn contract::clock::Clock> = Arc::new(clock::System);
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(e) => return fail(failed(e.code(), e)),
    };
    let workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(e) => {
            return fail(failed(
                ErrorCode::IoFailed,
                format!("the current directory: {e}"),
            ));
        }
    };
    // `tui.hover`, defaulting to on (`docs/configuration.md`, "Keys").
    let hover = match ::cli::project_of(&home, &workspace).and_then(|(_, project)| {
        Config::load(Sources {
            home: home.clone(),
            workspace: workspace.clone(),
            project,
            overrides: Vec::new(),
        })
        .map_err(|e| failed(e.code(), e))
    }) {
        Ok(config) => config
            .get("tui.hover", None)
            .and_then(|(value, _)| value.as_bool())
            .unwrap_or(true),
        Err(e) => return fail(e),
    };
    let tty = match io::stdin().as_fd().try_clone_to_owned() {
        Ok(tty) => std::fs::File::from(tty),
        Err(e) => return fail(failed(ErrorCode::IoFailed, format!("the terminal: {e}"))),
    };
    let hub_clock = Arc::clone(&clock);
    let connect: tui::Connect =
        Box::new(move || doors::hub::connect(&home, &mut start_hub, hub_clock.as_ref()));
    let project = log::project_key(&doors::project(&workspace));
    tui::run(
        tty,
        workspace,
        project,
        connect,
        Box::new(crash::attach),
        clock,
        hover,
    )
}

/// Starts `fiber hub serve` detached, as every client of the hub does when
/// none is running (`docs/invocation.md`, "The hub").
fn start_hub() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .arg("hub")
        .arg("serve")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}

/// `fiber sessions delete`: the hub it reaches is started when none runs.
fn sessions_delete(id: &str, cascade: bool, yes: bool, clock: &dyn contract::clock::Clock) -> i32 {
    let mut connect = || {
        let home = config::fiber_home_from_env().map_err(io::Error::other)?;
        doors::hub::connect(&home, &mut start_hub, clock)
    };
    ::cli::delete(id, cascade, yes, &mut connect)
}

fn usage(message: impl Into<String>) -> Failure {
    failure(ErrorCode::Usage, message)
}

fn failed(code: ErrorCode, e: impl Display) -> Failure {
    failure(code, e.to_string())
}

fn ask_failed(e: Failure) -> i32 {
    doors::exit_before_session(e, &mut io::stdout(), &mut io::stderr())
}
