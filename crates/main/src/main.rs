//! The `fiber` binary, and the composition root (`docs/architecture.md`):
//! parses argv, builds the session's parts once from configuration, the
//! installed providers and the credential, and hands them to a door
//! (`docs/invocation.md`). It holds no feature logic.

#![allow(
    clippy::print_stderr,
    reason = "main prints the usage sentence (docs/code-quality.md, \"Lints\")"
)]

mod cli;
mod clock;
mod prompt_files;
mod resume;

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

use config::{Config, Protocol, Sources};
use contract::inbox::Delivery;
use contract::provider::Provider;
use contract::shapes::Failure;
use contract::{ErrorCode, SessionId};
use doors::{Session, failure};
use extensions::{Origin, Provenance, Providers, Request};
use log::Log;
use r#loop::{Loop, Model};
use provider::anthropic_messages::Messages;
use provider::google_generative_ai::Gemini;
use provider::openai_completions::Completions;
use provider::openai_responses::Responses;
use provider::{Compat, Endpoint};
use serde_json::Value;

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
}

fn main() -> ExitCode {
    let code = run();
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn run() -> i32 {
    // Help and version print before anything reads the home, configuration,
    // credentials, or stdin.
    let clock: Arc<dyn contract::clock::Clock> = Arc::new(clock::System);
    match cli::parse() {
        cli::Invocation::Print(error) => {
            // A closed stdout leaves nobody to tell, as `fiber extension list` does.
            error.print().unwrap_or(());
            0
        }
        cli::Invocation::Run(Some(cli::Commands::Version)) => {
            let mut out = io::stdout().lock();
            write!(out, "{}", cli::version_line()).unwrap_or(());
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
        cli::Invocation::Run(Some(cli::Commands::Extension(cmd))) => extension(cmd, clock.as_ref()),
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
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let prompt = match doors::prompt(arg, dash, &mut stdin.lock(), terminal) {
        Ok(prompt) => prompt,
        Err(e) => return ask_failed(e),
    };
    match resume {
        Some(selector) => resume::ask_resume(selector, model, prompt, clock),
        None => ask_new(model, prompt, clock),
    }
}

/// `fiber ask` on a new session.
fn ask_new(model: Option<String>, prompt: String, clock: Arc<dyn contract::clock::Clock>) -> i32 {
    let parts = match parts_with(model, None) {
        Ok(parts) => parts,
        Err(e) => return ask_failed(e),
    };
    let Parts {
        provider,
        model,
        prompt: prompt_inputs,
        reviewer,
        limits,
        budget,
        retry,
        home,
        project,
        workspace,
        sessions,
    } = parts;
    let id = SessionId(doors::mint("s_"));
    let dir = sessions.join(&id.0);
    let permissions = ask_permissions(
        &home,
        &project,
        workspace.to_string_lossy().into_owned(),
        &clock,
    );
    let log = match Log::create(&sessions, id, Arc::clone(&clock)) {
        Ok(log) => Arc::new(log),
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let session = match Session::open(
        &home,
        &dir,
        &log,
        Arc::clone(&clock),
        Vec::new(),
        Box::new(io::stdout()),
    ) {
        Ok(session) => session,
        Err(e) => return ask_failed(e),
    };
    let code = run_turn(&session, &log, &dir, prompt, |inbox, cancel| {
        finish(
            // `Loop::start` writes `session_started`, which `fiber_started`
            // follows (`docs/events.md`).
            Loop::start(
                Arc::clone(&log),
                provider,
                model,
                prompt_inputs,
                inbox,
                Vec::new(),
                permissions,
            )
            .and_then(|looped| {
                r#loop::fiber_started(&log, env!("CARGO_PKG_VERSION"), false)?;
                Ok(looped)
            }),
            budget,
            reviewer,
            limits,
            retry,
            cancel,
        )
    });
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
    reviewer: Result<r#loop::Reviewer, Failure>,
    limits: r#loop::BlockLimits,
    retry: r#loop::Retry,
    cancel: Arc<r#loop::TurnCancel>,
) -> Result<(), Failure> {
    looped
        .map(|looped| {
            looped
                .budget(budget)
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
/// signal serves the door and the loop, so a `cancel` ends the turn the
/// prompt starts.
fn run_turn(
    session: &Session,
    log: &Arc<Log>,
    dir: &Path,
    prompt: String,
    run: impl FnOnce(Receiver<Delivery>, Arc<r#loop::TurnCancel>) -> Result<(), Failure>,
) -> i32 {
    let cancel = Arc::new(r#loop::TurnCancel::default());
    let door = Arc::clone(&cancel);
    let ran = session.ask(prompt, Arc::new(move || door.cancel()), |inbox| {
        run(inbox, cancel)
    });
    // A `fiber_exited` that cannot be written leaves a log that reads as a
    // process that died, which it then is.
    r#loop::fiber_exited(log, dir, ran).unwrap_or(1)
}

/// Fiber home, configuration, the chosen model, its credential and its
/// provider: everything a failure of which leaves no session. `model` is
/// `--model`, which sets configuration's `model` for this run. `recorded` is
/// the resumed session's model: the model the
/// log's last `usage_recorded` names, beats `--model`
/// (`docs/model-routing.md`, "Choosing the model"). A log with no
/// `usage_recorded` uses `--model`, then the configured default as a new
/// session does. A recorded model that no longer resolves fails with the
/// resolver's own failure, before any session line is written.
fn parts_with(model: Option<String>, recorded: Option<&str>) -> Result<Parts, Failure> {
    let home = config::fiber_home_from_env().map_err(|e| failed(e.code(), e))?;
    let workspace = std::env::current_dir()
        .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
    let sessions = log::sessions_dir(&home, &doors::project(&workspace));
    // `projects/<key>/sessions`: the project's key names its parent.
    let key = sessions
        .parent()
        .and_then(Path::file_name)
        .map(|key| key.to_string_lossy().into_owned())
        .unwrap_or_default();
    let project = config::ProjectKey::new(key).map_err(|e| failed(e.code(), e))?;
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
    let key = config
        .credential(model.provider)
        .map_err(|e| failed(e.code(), e))?;
    let provider = connect(model, key.expose().to_owned())?;
    let reviewer = choose_reviewer(&providers, &config, &model);
    let limits = block_limits(&config);
    let retry = retry_policy(&config);
    // debt: extension prompt texts and the model's addendum arrive empty;
    // filled by #510.
    let prompt = r#loop::PromptInputs {
        system: prompt_files::system(&home, &project),
        append: prompt_files::append(&home, &project),
        addendum: None,
        extensions: Vec::new(),
        context_window: model.model.context_window,
    };
    Ok(Parts {
        sessions,
        home,
        workspace,
        project,
        provider,
        prompt,
        model: Model {
            reference: model.reference(),
            cost: model.model.cost.clone().map(declared_cost),
            subscription: model.model.subscription,
        },
        reviewer,
        limits,
        budget,
        retry,
    })
}

/// The provider a model reaches: the endpoint and protocol construction the
/// session's model and the reviewer's share. A reviewer failure never falls
/// back to the session's model (`docs/permissions.md`, "How it runs").
fn connect(model: extensions::Model<'_>, key: String) -> Result<Arc<dyn Provider>, Failure> {
    let endpoint = Endpoint {
        provider: model.provider.name.clone(),
        model: model.model.id.clone(),
        base_url: model.model.base_url.clone(),
        key: Some(key),
        headers: model
            .provider
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        compat: Compat::from_data(&model.model.compat),
        max_output_tokens: model.model.max_output_tokens,
        extra_body: model.model.extra_body.clone(),
        direct: false,
    };
    Ok(match model.model.protocol {
        Protocol::OpenaiResponses => {
            let responses = Responses::new(endpoint);
            Arc::new(
                match model
                    .model
                    .compat
                    .get("cache_key_header")
                    .and_then(Value::as_str)
                {
                    Some(name) => responses.cache_key_header(name),
                    None => responses,
                },
            )
        }
        Protocol::OpenaiCompletions => Arc::new(Completions::new(endpoint)),
        Protocol::AnthropicMessages => Arc::new(Messages::new(endpoint)),
        Protocol::GoogleGenerativeAi => {
            let gemini = Gemini::new(endpoint);
            Arc::new(
                match model
                    .model
                    .compat
                    .get("cache_key_header")
                    .and_then(Value::as_str)
                {
                    Some(name) => gemini.cache_key_header(name),
                    None => gemini,
                },
            )
        }
        Protocol::BedrockConverse => {
            return Err(failure(
                ErrorCode::ProtocolUnsupported,
                format!(
                    "The model `{}` speaks a protocol this Fiber does not speak yet.",
                    model.reference()
                ),
            ));
        }
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
    let key = config
        .credential(model.provider)
        .map_err(|e| failed(e.code(), e))?;
    let provider = connect(model, key.expose().to_owned())?;
    Ok(r#loop::Reviewer {
        provider,
        model: Model {
            reference: model.reference(),
            cost: model.model.cost.clone().map(declared_cost),
            subscription: model.model.subscription,
        },
    })
}

/// When a reviewer block hands the call to a person, from configuration
/// with the documented defaults (`docs/configuration.md`).
fn block_limits(config: &Config) -> r#loop::BlockLimits {
    let limit = |key: &str, default: u64| {
        config
            .get(key, None)
            .and_then(|(value, _)| value.as_u64())
            .unwrap_or(default)
    };
    r#loop::BlockLimits {
        consecutive: limit("reviewer.block_limits.consecutive", 3),
        session: limit("reviewer.block_limits.session", 20),
    }
}

/// How a failed model call is retried, from configuration with the
/// documented defaults (`docs/configuration.md`). `attempts` is clamped
/// to `u32`, so a huge configured count never overflows the loop.
fn retry_policy(config: &Config) -> r#loop::Retry {
    let count = |key: &str, default: u64| {
        config
            .get(key, None)
            .and_then(|(value, _)| value.as_u64())
            .unwrap_or(default)
    };
    r#loop::Retry {
        attempts: u32::try_from(count("retry.attempts", 3)).unwrap_or(u32::MAX),
        initial: Duration::from_millis(count("retry.initial_delay_ms", 2000)),
        max: Duration::from_millis(count("retry.max_delay_ms", 60000)),
    }
}

/// Field-by-field copy of a model's declared prices. `loop` cannot depend on
/// `config`, so the prices it prices with live in `contract`.
fn declared_cost(cost: config::Cost) -> contract::provider::Cost {
    contract::provider::Cost {
        input: cost.input,
        output: cost.output,
        cache_read: cost.cache_read,
        cache_write: cost.cache_write,
        tiers: cost
            .tiers
            .into_iter()
            .map(|tier| contract::provider::Tier {
                input_tokens_above: tier.input_tokens_above,
                input: tier.input,
                output: tier.output,
                cache_read: tier.cache_read,
                cache_write: tier.cache_write,
            })
            .collect(),
    }
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

#[cfg(test)]
mod retry_policy_tests {
    //! `retry.attempts` from configuration reaches the loop's retry policy.

    use super::retry_policy;

    fn config(overrides: Vec<String>) -> config::Config {
        let root = fakes::TempDir::new("fiber-retry-policy");
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let project = config::ProjectKey::new("test").unwrap();
        let config = config::Config::load(config::Sources {
            home,
            workspace,
            project,
            overrides,
        })
        .unwrap();
        // `root` is dropped here; the configuration was already read.
        config
    }

    #[test]
    fn retry_policy_clamps_huge_attempts() {
        let retry = retry_policy(&config(vec!["retry.attempts=18446744073709551615".into()]));
        assert_eq!(retry.attempts, u32::MAX);
    }
}
