//! The `fiber` binary, and the composition root (`docs/architecture.md`):
//! parses argv, builds the session's parts once from configuration, the
//! installed providers and the credential, and hands them to a door
//! (`docs/invocation.md`). It holds no feature logic.

#![allow(
    clippy::print_stderr,
    reason = "main prints the usage sentence (docs/code-quality.md, \"Lints\")"
)]

use std::fmt::Display;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use config::{Config, Protocol, Sources};
use contract::provider::Provider;
use contract::shapes::Failure;
use contract::{ErrorCode, SessionId};
use doors::{Session, failure};
use extensions::Providers;
use log::Log;
use r#loop::Loop;
use provider::anthropic_messages::Messages;
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
        Some((command, rest)) if command == "install" => install(rest),
        // The terminal door needs a tty and the hub; neither is built, so
        // every other invocation is called wrongly.
        Some(_) | None => {
            let message = "Usage: fiber ask [--model <model>] \"<prompt>\", fiber ask < <file>, or fiber install <path>.";
            eprintln!("fiber: {message}");
            2
        }
    }
}

/// `fiber install <path>`: installs the extension at a local path into Fiber
/// home, after showing what it registers and asking when stdin is a
/// terminal (`docs/extensions.md`, "Installing"), and prints its name.
fn install(args: &[String]) -> i32 {
    let installed = match args {
        [path] => install_path(Path::new(path)),
        _ => Err(usage("Usage: fiber install <path>.")),
    };
    match installed {
        Ok(Some(name)) => {
            eprintln!("fiber: installed {name}");
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

/// Installs from `source` once approved; `None` when the person declined.
fn install_path(source: &Path) -> Result<Option<String>, Failure> {
    let home = config::fiber_home_from_env().map_err(|e| failed(e.code(), e))?;
    let manifest = config::read_manifest(source).map_err(|e| failed(e.code(), e))?;
    let providers = config::read_providers(source).map_err(|e| failed(e.code(), e))?;
    let summary = doors::InstallSummary {
        name: manifest.name,
        source: source.display().to_string(),
        providers: providers
            .into_iter()
            .map(|p| {
                let mut urls: Vec<String> = p.models.into_iter().map(|m| m.base_url).collect();
                urls.sort();
                urls.dedup();
                (p.name, urls)
            })
            .collect(),
    };
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    if !doors::install_approved(&summary, terminal, &mut stdin.lock(), &mut io::stderr())? {
        return Ok(None);
    }
    extensions::install(&home, source, env!("CARGO_PKG_VERSION"))
        .map(Some)
        .map_err(|e| failed(e.code(), e))
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
            Vec::new(),
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
        compat: Compat {
            store: model.model.compat.get("store").and_then(Value::as_bool),
        },
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
        Protocol::AnthropicMessages => Arc::new(Messages::new(endpoint)),
        Protocol::OpenaiCompletions | Protocol::GoogleGenerativeAi | Protocol::BedrockConverse => {
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
