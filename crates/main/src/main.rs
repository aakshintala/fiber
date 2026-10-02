//! The `fiber` binary, and the composition root (`docs/architecture.md`):
//! parses argv, builds the session's parts once from configuration, the
//! installed providers and the credential, and hands them to a door
//! (`docs/invocation.md`). It holds no feature logic.

#![allow(
    clippy::print_stderr,
    reason = "main prints the usage sentence (docs/code-quality.md, \"Lints\")"
)]

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
use extensions::{Origin, Providers, Request};
use log::Log;
use r#loop::Loop;
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
    provider: Arc<dyn Provider>,
    model: String,
}

fn main() -> ExitCode {
    let code = run();
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn run() -> i32 {
    let args: Vec<String> = match std::env::args_os()
        .skip(1)
        .map(|a| a.into_string())
        .collect()
    {
        Ok(args) => args,
        Err(_) => return ask_failed(usage("An argument is not UTF-8 text.")),
    };
    match args.split_first() {
        Some((door, rest)) if door == "ask" => ask(rest),
        Some((command, rest)) if command == "install" || command == "update" => {
            install(command, rest)
        }
        Some((command, rest)) if command == "remove" => remove(rest),
        Some((command, rest)) if command == "list" => list(rest),
        // The terminal door needs a tty and the hub; neither is built, so
        // every other invocation is called wrongly.
        Some(_) | None => {
            let message = "Usage: fiber ask [--model <model>] \"<prompt>\", fiber ask < <file>, fiber install <name or path>, fiber update <name>, fiber remove <name>, or fiber list.";
            eprintln!("fiber: {message}");
            2
        }
    }
}

/// `fiber install <name or path>` and `fiber update <name>`: fetches and
/// checks the extension and its dependencies, shows what they register and
/// asks when stdin is a terminal (`docs/extensions.md`, "Installing"), and
/// prints each name installed.
fn install(command: &str, args: &[String]) -> i32 {
    let installed = match (command, args) {
        ("install", [typed]) if extensions::is_path(typed) => {
            install_request(Request::Path(PathBuf::from(typed)))
        }
        ("install", [typed]) => install_request(Request::Install(typed.clone())),
        ("update", [typed]) => install_request(Request::Update(typed.clone())),
        ("install", _) => Err(usage("Usage: fiber install <name or path>.")),
        _ => Err(usage("Usage: fiber update <name>.")),
    };
    match installed {
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
fn install_request(request: Request) -> Result<Option<Vec<String>>, Failure> {
    let home = config::fiber_home_from_env().map_err(|e| failed(e.code(), e))?;
    let plan = extensions::plan(
        &home,
        &request,
        env!("CARGO_PKG_VERSION"),
        &Origin::github(),
    )
    .map_err(|e| failed(e.code(), e))?;
    let summaries: Vec<doors::InstallSummary> = plan
        .items()
        .map(|item| doors::InstallSummary {
            name: item.name.clone(),
            source: item.source.clone(),
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
        })
        .collect();
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    if !doors::install_approved(&summaries, terminal, &mut stdin.lock(), &mut io::stderr())? {
        return Ok(None);
    }
    plan.commit().map(Some).map_err(|e| failed(e.code(), e))
}

/// `fiber remove <name>`: removes an extension and the dependencies nothing
/// else uses.
fn remove(args: &[String]) -> i32 {
    let [typed] = args else {
        return fail(usage("Usage: fiber remove <name>."));
    };
    let removed = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| extensions::uninstall(&home, typed).map_err(|e| failed(e.code(), e)));
    match removed {
        Ok(names) => {
            for name in names {
                eprintln!("fiber: removed {name}");
            }
            0
        }
        Err(e) => fail(e),
    }
}

/// `fiber list`: one line per installed extension: name, version and commit.
fn list(args: &[String]) -> i32 {
    if !args.is_empty() {
        return fail(usage("Usage: fiber list."));
    }
    let listed = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| extensions::list(&home).map_err(|e| failed(e.code(), e)));
    match listed {
        Ok(installed) => {
            let mut out = io::stdout().lock();
            for i in installed {
                let commit = i.commit.as_deref().unwrap_or("local");
                // A closed stdout leaves nobody to tell.
                writeln!(out, "{} {} {commit}", i.name, i.version).unwrap_or(());
            }
            0
        }
        Err(e) => fail(e),
    }
}

fn fail(e: Failure) -> i32 {
    eprintln!("fiber: {}", e.message);
    doors::exit_code(&e)
}

/// `fiber ask`: one session, one turn, its events on stdout.
fn ask(args: &[String]) -> i32 {
    let (model, args) = match args {
        [flag, model, rest @ ..] if flag == "--model" => (Some(model.clone()), rest),
        [flag] if flag == "--model" => {
            return ask_failed(usage("`--model` takes a model, such as `provider/model`."));
        }
        _ => (None, args),
    };
    let arg = match args {
        [] => None,
        [flag, ..] if flag.starts_with('-') => {
            return ask_failed(usage(format!("`fiber ask` takes no flag `{flag}`.")));
        }
        [prompt] => Some(prompt.clone()),
        [_, _, ..] => {
            return ask_failed(usage(
                "`fiber ask` takes one prompt; quote it, or put it on stdin.",
            ));
        }
    };
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let prompt = match doors::prompt(arg, &mut stdin.lock(), terminal) {
        Ok(prompt) => prompt,
        Err(e) => return ask_failed(e),
    };
    let parts = match parts(model) {
        Ok(parts) => parts,
        Err(e) => return ask_failed(e),
    };
    let id = SessionId(doors::mint("s_"));
    let dir = parts.sessions.join(&id.0);
    let log = match Log::create(&parts.sessions, id) {
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
            // ponytail: an empty system prompt until the system prompt is
            // built (`docs/system-prompt.md`).
            String::new(),
            inbox,
            parts.workspace.to_string_lossy().into_owned(),
        )
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
    let config = Config::load(Sources {
        home: home.clone(),
        workspace: workspace.clone(),
        project: config::ProjectKey::new(key).map_err(|e| failed(e.code(), e))?,
        overrides: model.map(|m| format!("model={m}")).into_iter().collect(),
    })
    .map_err(|e| failed(e.code(), e))?;
    // ponytail: notices from configuration and loading are dropped until the
    // session writes them (`docs/events.md`, `notice`).
    let (providers, _notices) = Providers::load(&home).map_err(|e| failed(e.code(), e))?;
    let model = providers
        .choose(None, &config)
        .map_err(|e| failed(e.code(), e))?;
    let key = config
        .credential(model.provider)
        .map_err(|e| failed(e.code(), e))?;
    let endpoint = Endpoint {
        provider: model.provider.name.clone(),
        model: model.model.id.clone(),
        base_url: model.model.base_url.clone(),
        key: Some(key.expose().to_owned()),
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
    let provider: Arc<dyn Provider> = match model.model.protocol {
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
            // ponytail: docs/errors.md has no code for a protocol this Fiber
            // does not speak yet; `extension_missing` stands in.
            return Err(failure(
                ErrorCode::ExtensionMissing,
                format!(
                    "The model `{}` speaks a protocol this Fiber does not speak yet.",
                    model.reference()
                ),
            ));
        }
    };
    Ok(Parts {
        sessions,
        home,
        workspace,
        provider,
        model: model.reference(),
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
