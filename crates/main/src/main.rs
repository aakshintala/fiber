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

#[cfg(test)]
#[path = "live_tests.rs"]
mod live_tests;

use std::fmt::Display;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use config::{Config, Protocol, Sources};
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
    /// Who judges step 7's calls; `Err` leaves every reviewed call to a
    /// person (`docs/permissions.md`, "How it runs").
    reviewer: Result<r#loop::Reviewer, Failure>,
    /// When a reviewer block hands the call to a person.
    limits: r#loop::BlockLimits,
    /// `budget.usd`, or none when the key is absent or not a number.
    budget: Option<f64>,
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
                Ok((prompt, dash)) => ask(args.model, prompt, dash, clock),
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
    let parts = match parts(model) {
        Ok(parts) => parts,
        Err(e) => return ask_failed(e),
    };
    let id = SessionId(doors::mint("s_"));
    let dir = parts.sessions.join(&id.0);
    let rules = config::RulesFiles::new(parts.home.clone(), parts.project.clone(), clock.clone());
    let log = match Log::create(&parts.sessions, id, clock) {
        Ok(log) => Arc::new(log),
        Err(e) => return ask_failed(failed(e.code(), e)),
    };
    let session = match Session::open(&parts.home, &dir, log.watch(), Box::new(io::stdout())) {
        Ok(session) => session,
        Err(e) => return ask_failed(e),
    };
    if let Err(e) = r#loop::fiber_started(&log, env!("CARGO_PKG_VERSION")) {
        session.close(log);
        return ask_failed(failed(e.code(), e));
    }
    let ran = session.ask(prompt, |inbox| {
        Loop::start(
            Arc::clone(&log),
            parts.provider,
            parts.model,
            // debt: weakens docs/system-prompt.md, "Two parts"; fixed by
            // #304. The system prompt is empty.
            String::new(),
            inbox,
            Vec::new(),
            r#loop::Permissions {
                workspace: parts.workspace.to_string_lossy().into_owned(),
                credentials: parts.home.join("credentials"),
                rules: Arc::new(rules),
            },
        )
        .map(|looped| {
            looped
                .budget(parts.budget)
                .answerable(false)
                .reviewer(parts.reviewer, parts.limits)
        })
        .and_then(Loop::run)
        .map_err(|e| failed(e.code(), e))
    });
    // A `fiber_exited` that cannot be written leaves a log that reads as a
    // process that died, which it then is.
    let code = r#loop::fiber_exited(&log, &dir, ran).unwrap_or(1);
    session.close(log);
    code
}

/// Fiber home, configuration, the chosen model, its credential and its
/// provider: everything a failure of which leaves no session. `model` is
/// `--model`, which sets configuration's `model` for this run.
fn parts(model: Option<String>) -> Result<Parts, Failure> {
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
    let model = providers
        .choose(None, &config)
        .map_err(|e| failed(e.code(), e))?;
    let key = config
        .credential(model.provider)
        .map_err(|e| failed(e.code(), e))?;
    let provider = connect(model, key.expose().to_owned())?;
    let reviewer = choose_reviewer(&providers, &config, &model);
    let limits = block_limits(&config);
    Ok(Parts {
        sessions,
        home,
        workspace,
        project,
        provider,
        model: Model {
            reference: model.reference(),
            cost: model.model.cost.clone().map(declared_cost),
            subscription: model.model.subscription,
        },
        reviewer,
        limits,
        budget,
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
                return Err(failure(
                    ErrorCode::NoModel,
                    "No reviewer model is set, so every reviewed call goes to a person. \
                     Set reviewer.model.",
                ));
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
