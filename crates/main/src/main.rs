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
mod handoff;
mod late_emit;
mod mcp_servers;
mod prompt_files;
mod resume;
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
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use config::{Config, Sources};
use connect::connect;
use contract::inbox::Delivery;
use contract::provider::Provider;
use contract::shapes::Failure;
use contract::{ErrorCode, SessionId};
use doors::{Session, failure};
use extensions::{Origin, Provenance, Providers, Request};
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
    /// The installed extensions, started, and their hooks.
    extensions: Arc<extensions::SessionExtensions>,
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
            print_help(command.as_deref())
        }
        // The terminal door needs a tty and the hub; neither is built.
        cli::Invocation::Run(None) => {
            eprintln!(
                "fiber: The terminal door is not built; run `fiber ask \"<prompt>\"`. Run `fiber --help` for usage."
            );
            2
        }
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
        cli::Invocation::Run(Some(cli::Commands::Sessions(cmd))) => match cmd {
            cli::SessionsCommands::Export { id, path } => ::cli::export(&id, path.as_deref()),
        },
        cli::Invocation::Run(Some(cli::Commands::Models(args))) => {
            ::cli::models(args.search.as_deref(), args.json)
        }
        cli::Invocation::Run(Some(cli::Commands::Extension(cmd))) => extension(cmd, clock.as_ref()),
        cli::Invocation::Run(Some(cli::Commands::Approve(args))) => ::cli::approve(args.yes),
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

fn extension(cmd: cli::ExtensionCommands, clock: &dyn contract::clock::Clock) -> i32 {
    match cmd {
        cli::ExtensionCommands::Install { name_or_path } => {
            let request = if extensions::is_path(&name_or_path) {
                Request::Path(PathBuf::from(name_or_path))
            } else {
                Request::Install(name_or_path)
            };
            install(request, clock)
        }
        cli::ExtensionCommands::Update { name: Some(name) } => {
            install(Request::Update(name), clock)
        }
        cli::ExtensionCommands::Update { name: None } => update_all(clock),
        cli::ExtensionCommands::Remove { name } => remove(&name, clock),
        cli::ExtensionCommands::List => list(clock),
    }
}

/// `fiber extension update` with no name: each extension a person asked for.
fn update_all(clock: &dyn contract::clock::Clock) -> i32 {
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(e) => return fail(failed(e.code(), e)),
    };
    let installed = match extensions::list(&home, clock) {
        Ok(list) => list,
        Err(e) => return fail(failed(e.code(), e)),
    };
    for ext in installed.iter().filter(|i| i.requested) {
        let code = install(Request::Update(ext.name.clone()), clock);
        if code != 0 {
            return code;
        }
    }
    0
}

/// `fiber extension install <name or path>` and `fiber extension update <name>`: fetches and
/// checks the extension and its dependencies, shows what they register and
/// asks when stdin is a terminal (`docs/extensions.md`, "Installing"), and
/// prints each name installed.
fn install(request: Request, clock: &dyn contract::clock::Clock) -> i32 {
    match install_request(request, clock) {
        Ok(Some(names)) => {
            for name in names {
                eprintln!("fiber: installed {name}");
            }
            0
        }
        Ok(None) => {
            eprintln!("fiber: nothing was installed.");
            1
        }
        Err(e) => {
            eprintln!("fiber: {}", e.message);
            doors::exit_code(&e)
        }
    }
}

/// Installs what `request` needs once approved; `None` when the person
/// declined.
fn install_request(
    request: Request,
    clock: &dyn contract::clock::Clock,
) -> Result<Option<Vec<String>>, Failure> {
    let home = config::fiber_home_from_env().map_err(|e| failed(e.code(), e))?;
    let plan = extensions::plan(
        &home,
        &request,
        env!("CARGO_PKG_VERSION"),
        &Origin::github(),
        clock,
    )
    .map_err(|e| failed(e.code(), e))?;
    let summaries: Vec<doors::InstallSummary> = plan
        .items()
        .map(|item| doors::InstallSummary {
            name: item.name.clone(),
            source: item.source(),
            version: item.version.clone(),
            changes: item.changes.clone(),
            providers: item
                .providers
                .iter()
                .map(|p| {
                    let mut urls: Vec<String> =
                        p.models.iter().map(|m| m.base_url.clone()).collect();
                    urls.sort();
                    urls.dedup();
                    (p.name.clone(), urls)
                })
                .collect(),
            process: item.manifest.process.as_ref().map(|p| {
                std::iter::once(p.program.as_str())
                    .chain(p.args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
            install_step: item.manifest.install.as_ref().map(|step| step.join(" ")),
            carries: item.carries(),
            staged: item.staged().to_path_buf(),
        })
        .collect();
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    if !doors::install_approved(&summaries, terminal, &mut stdin.lock(), &mut io::stderr())? {
        return Ok(None);
    }
    plan.commit().map(Some).map_err(|e| failed(e.code(), e))
}

/// `fiber extension remove <name>`: removes an extension, the dependencies nothing
/// else uses, and their data and settings, asking first in a terminal.
fn remove(typed: &str, clock: &dyn contract::clock::Clock) -> i32 {
    let removed = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let removal =
                extensions::removal(&home, typed, clock).map_err(|e| failed(e.code(), e))?;
            let stdin = io::stdin();
            let terminal = stdin.is_terminal();
            let approved = doors::remove_approved(
                &removal.names,
                &removal.data,
                terminal,
                &mut stdin.lock(),
                &mut io::stderr(),
            )?;
            if !approved {
                return Ok(None);
            }
            let names = removal.names.clone();
            removal.commit().map_err(|e| failed(e.code(), e))?;
            Ok(Some(names))
        });
    match removed {
        Ok(Some(names)) => {
            for name in names {
                eprintln!("fiber: removed {name}");
            }
            0
        }
        Ok(None) => {
            eprintln!("fiber: nothing was removed.");
            1
        }
        Err(e) => fail(e),
    }
}

/// `fiber extension list`: one line per installed extension: name, version and commit.
fn list(clock: &dyn contract::clock::Clock) -> i32 {
    let listed = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| extensions::list(&home, clock).map_err(|e| failed(e.code(), e)));
    match listed {
        Ok(installed) => {
            let mut out = io::stdout().lock();
            for i in installed {
                let commit = match &i.provenance {
                    Provenance::Git { commit } => commit.as_str(),
                    Provenance::Path(_) => "local",
                };
                // A closed stdout leaves nobody to tell.
                writeln!(out, "{} {} {commit}", i.name, i.version).unwrap_or(());
            }
            0
        }
        Err(e) => fail(e),
    }
}

fn print_help(name: Option<&str>) -> i32 {
    match cli::render_help(name) {
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
    let mut parts = match parts_with(model, None, None, Arc::clone(&clock)) {
        Ok(parts) => parts,
        Err(e) => return ask_failed(e),
    };
    let id = SessionId(doors::mint("s_"));
    crash::attach(&id);
    let dir = parts.sessions.join(&id.0);
    // The session directory's log: the opening message's environment
    // names it.
    parts.prompt.session_log = dir.join("events.jsonl").display().to_string();
    let Parts {
        provider,
        model,
        prompt: prompt_inputs,
        reviewer,
        limits,
        budget,
        retry,
        handoff,
        idle,
        home,
        project,
        workspace,
        sessions,
        extensions,
        mcp,
        web_search,
    } = parts;
    let (job_emit, jobs) = late_emit::registry(&dir, &clock);
    shutdown::arm(signals);
    // Before the log exists: a failure here, such as not finding the running
    // binary, leaves no session line; every server starts with the session too.
    let (tools, infos, driver, session_servers) = match mcp_servers::session_tools(
        &workspace,
        &dir.join("artifacts"),
        &clock,
        &jobs,
        mcp.specs,
        web_search.as_deref(),
    ) {
        Ok(built) => built,
        Err(e) => return ask_failed(e),
    };
    let forget = Arc::clone(&session_servers.forget);
    let permissions = ask_permissions(
        &home,
        &project,
        workspace.to_string_lossy().into_owned(),
        &clock,
    );
    let log = match Log::create(&sessions, id, Arc::clone(&clock)) {
        Ok(log) => Arc::new(log),
        Err(e) => return stop_and_fail(session_servers, failed(e.code(), e)),
    };
    job_emit.set(Arc::clone(&log) as _);
    let session = match Session::open(
        &home,
        &dir,
        &log,
        Arc::clone(&clock),
        infos,
        Box::new(io::stdout()),
    ) {
        Ok(session) => session,
        Err(e) => return stop_and_fail(session_servers, e),
    };
    session.shell(driver);
    session.jobs(jobs.clone());
    let cancel = Arc::new(r#loop::TurnCancel::default());
    // A signal while armed: nothing was written, so nothing more is.
    if let Some(code) = shutdown::start(signals, &cancel, &session, jobs.clone()) {
        session_servers.servers.stop();
        session.close(log);
        return code;
    }
    let code = run_turn(&session, &log, &dir, prompt, cancel, |inbox, cancel| {
        finish(
            // `Loop::start` writes `session_started`, which `fiber_started`
            // follows (`docs/events.md`).
            Loop::start(
                Arc::clone(&log),
                provider,
                model,
                prompt_inputs,
                inbox,
                tools,
                permissions,
            )
            .and_then(|looped| {
                r#loop::fiber_started(&log, env!("CARGO_PKG_VERSION"), false)?;
                session_extensions::written(&log, &extensions)?;
                r#loop::mcp_servers_started(&log, session_servers.failed, mcp.notices)?;
                let looped = session_extensions::hooked(looped.jobs(jobs), &extensions);
                Ok(looped.handoff(handoff).on_handoff(forget))
            }),
            budget,
            idle,
            reviewer,
            limits,
            retry,
            cancel,
        )
    });
    session_servers.servers.stop();
    session.close(log);
    code
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

/// Finishes an `ask` loop, however it started: the budget, the headless
/// answers and the reviewer every session gets, so a change to the chain is
/// made once.
fn finish(
    looped: Result<Loop, r#loop::Error>,
    budget: Option<f64>,
    idle: Option<Duration>,
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
                .answerable(false)
                .reviewer(reviewer, limits)
                .retry(retry)
                .cancelled_by(cancel)
        })
        .and_then(Loop::run)
        .map_err(|e| failed(e.code(), e))
}

/// Runs one turn of the session `session` writes to `log`, once its log,
/// model and prompt are known, shared by new and resumed sessions: sends
/// the prompt, closes, and writes `fiber_exited` for what ran. One cancel
/// signal, `cancel`, serves the door and the loop, so a `cancel` ends the
/// turn the prompt starts; a shutdown's code on it is the exit code.
fn run_turn(
    session: &Session,
    log: &Arc<Log>,
    dir: &Path,
    prompt: String,
    cancel: Arc<r#loop::TurnCancel>,
    run: impl FnOnce(Receiver<Delivery>, Arc<r#loop::TurnCancel>) -> Result<(), Failure>,
) -> i32 {
    let door = Arc::clone(&cancel);
    let turn = Arc::clone(&cancel);
    let ran = session.ask(prompt, Arc::new(move || door.cancel()), |inbox| {
        run(inbox, turn)
    });
    // `fiber_exited` is the last line: nothing on the door side follows it.
    session.quiesce();
    // A `fiber_exited` that cannot be written leaves a log that reads as a
    // process that died, which it then is.
    r#loop::fiber_exited(log, dir, ran, cancel.shutdown_code()).unwrap_or(1)
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
    let (providers, _notices) = Providers::load(&home).map_err(|e| failed(e.code(), e))?;
    // `recorded` first, then `--model` and configuration's `model`
    // (`docs/model-routing.md`, "Choosing the model").
    let model = providers
        .choose(recorded, &config)
        .map_err(|e| failed(e.code(), e))?;
    let (label, key) =
        credential::session_credential(&config, model.provider, recorded_credential)?;
    let provider = connect(model, key.expose().to_owned())?;
    let reviewer = choose_reviewer(&providers, &config, &model, &label);
    let limits = settings::block_limits(&config);
    let retry = settings::retry_policy(&config);
    let handoff = handoff::handoff_settings(&config, &model.reference());
    let idle = settings::idle_exit(&config);
    let extensions = extensions::SessionExtensions::load(&home, &config, Arc::clone(&clock));
    // debt: extension prompt texts and the model's addendum arrive empty;
    // filled by #510.
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
    prompt.extension_dirs = extensions.dirs();
    prompt.extension_sections = extensions.sections(&project);
    prompt.skills_disabled = config.union_list("skills.disabled");
    prompt.credential = Some(label);
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
/// session's own model.
fn choose_reviewer(
    providers: &Providers,
    config: &Config,
    session: &extensions::Model<'_>,
    label: &str,
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
    let key = credential::reviewer_credential(config, model.provider, session.provider, label)?;
    let provider = connect(model, key.expose().to_owned())?;
    Ok(r#loop::Reviewer {
        provider,
        model: Model {
            reference: model.reference(),
            cost: model.model.cost.clone().map(cost::declared),
            subscription: model.model.subscription,
        },
    })
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
