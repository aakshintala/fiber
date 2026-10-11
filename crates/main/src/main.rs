//! The `fiber` binary, and the composition root (`docs/architecture.md`):
//! parses argv, builds the session's parts once from configuration, the
//! installed providers and the credential, and hands them to a door
//! (`docs/invocation.md`). It holds no feature logic.

#![allow(
    clippy::print_stderr,
    reason = "main prints the usage sentence (docs/code-quality.md, \"Lints\")"
)]

mod builtin;
mod case;
mod cli;
mod clock;
mod completion;
mod configure;
mod connect;
mod cost;
mod crash;
mod credential;
mod delegates;
mod handoff;
mod hub_command;
mod late_emit;
mod launch;
mod lua_providers;
mod mcp_servers;
mod model_list;
mod open;
mod parts;
mod prompt_files;
mod resume;
mod rewind;
mod scripted;
mod session_command;
mod session_extensions;
mod settings;
mod shutdown;
mod switch;
mod theme_setting;

#[cfg(test)]
#[path = "live_tests.rs"]
mod live_tests;
#[cfg(test)]
#[path = "lua_warm_tests.rs"]
mod lua_warm_tests;
#[cfg(test)]
#[path = "reviewer_tests.rs"]
mod reviewer_tests;
#[cfg(test)]
mod test_support;

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
use connect::{Here, connect};
use contract::inbox::Delivery;
use contract::shapes::Failure;
use contract::{ErrorCode, SessionId};
use doors::{Session, failure};
use log::Log;
use r#loop::Loop;
#[cfg(test)]
use parts::parts_in;
use parts::{Parts, choose_reviewer, parts_with, per_run, reviewer_reference};

fn main() -> ExitCode {
    ExitCode::from(u8::try_from(run()).unwrap_or(1))
}

fn run() -> i32 {
    let fiber = std::env::current_exe().map_err(|error| format!("the running binary: {error}"));
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
        cli::Invocation::Run(Some(cli::Commands::Completion { shell })) => completion::print(shell),
        cli::Invocation::Run(Some(cli::Commands::Help { command })) => {
            print_help(command.as_slice())
        }
        // `fiber` with no arguments opens the terminal: this tty as a
        // client of the hub, starting one when none runs.
        cli::Invocation::Run(None) => terminal(fiber, tui::OpenAt::Home),
        cli::Invocation::Run(Some(cli::Commands::Resume { id })) => open::resume(id, fiber),
        cli::Invocation::Run(Some(cli::Commands::Continue)) => open::continue_latest(fiber),
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
                Ok((prompt, dash)) => ask(
                    per_run(args.model, args.overrides),
                    args.resume
                        .map(|id| resume::Resuming::new(id, args.credential)),
                    args.worktree,
                    prompt,
                    dash,
                    clock,
                    fiber,
                ),
                Err(sentence) => ask_failed(usage(sentence)),
            }
        }
        cli::Invocation::Run(Some(cli::Commands::ExtensionCase { case })) => {
            case::run::extension_case(case, clock, fiber)
        }
        cli::Invocation::Run(Some(cli::Commands::Session(args))) => {
            session_command::run(args, clock, fiber)
        }
        cli::Invocation::Run(Some(cli::Commands::Hub(command))) => hub_command::run(command, fiber),
        cli::Invocation::Run(Some(cli::Commands::Sessions(args))) => match args.command {
            None => sessions_list(args.all, args.json, clock.as_ref(), fiber),
            Some(cli::SessionsCommands::Delete { cascade, yes, id }) => {
                sessions_delete(&id, cascade, yes, clock.as_ref(), fiber)
            }
            Some(cli::SessionsCommands::Export { id, path }) => ::cli::export(&id, path.as_deref()),
            Some(cli::SessionsCommands::Search { all, json, text }) => {
                ::cli::sessions_search(&text, all, json)
            }
            Some(cli::SessionsCommands::Prune {
                older_than,
                cascade,
                dry_run,
                yes,
                force,
            }) => sessions_prune(
                older_than,
                cascade,
                dry_run,
                yes,
                force,
                clock.as_ref(),
                fiber,
            ),
        },
        cli::Invocation::Run(Some(cli::Commands::Models(args))) => ::cli::models(
            args.search.as_deref(),
            args.json,
            clock,
            Arc::new(tools::PathLocks::new()),
            fiber,
        ),
        cli::Invocation::Run(Some(cli::Commands::RefreshModelLists { providers })) => {
            ::cli::refresh_model_lists(&providers, clock, Arc::new(tools::PathLocks::new()));
            0
        }
        cli::Invocation::Run(Some(cli::Commands::ReleaseInstall { version, base_url })) => {
            ::cli::release_install(
                &version,
                base_url.as_deref(),
                env!("CARGO_PKG_VERSION"),
                option_env!("FIBER_COMMIT"),
                clock.as_ref(),
            )
        }
        cli::Invocation::Run(Some(cli::Commands::Extension(cmd))) => match cmd {
            cli::ExtensionCommands::Install { name_or_path } => {
                ::cli::extension_install(&name_or_path, env!("CARGO_PKG_VERSION"), clock.as_ref())
            }
            cli::ExtensionCommands::Update { name } => {
                ::cli::extension_update(name.as_deref(), env!("CARGO_PKG_VERSION"), clock.as_ref())
            }
            cli::ExtensionCommands::Remove { name } => {
                ::cli::extension_remove(&name, clock.as_ref())
            }
            cli::ExtensionCommands::List => ::cli::extension_list(clock.as_ref()),
            cli::ExtensionCommands::Test { path } => ::cli::extension_test(path.as_deref(), fiber),
        },
        cli::Invocation::Run(Some(cli::Commands::Approve(args))) => {
            ::cli::approve(args.yes, Arc::clone(&clock))
        }
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
        cli::Invocation::Run(Some(cli::Commands::Login(args))) => ::cli::run_login(
            args.name.as_deref(),
            args.label.as_deref(),
            args.device,
            Arc::clone(&clock),
        ),
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
    overrides: Vec<String>,
    resume: Option<resume::Resuming>,
    worktree: bool,
    arg: Option<String>,
    dash: bool,
    clock: Arc<dyn contract::clock::Clock>,
    fiber: Result<PathBuf, String>,
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
        Some(resuming) => resume::ask_resume(resuming, overrides, prompt, clock, &signals, fiber),
        None => ask_new(overrides, prompt, worktree, clock, &signals, fiber),
    }
}

/// `fiber ask` on a new session.
fn ask_new(
    overrides: Vec<String>,
    prompt: String,
    worktree: bool,
    clock: Arc<dyn contract::clock::Clock>,
    signals: &Arc<doors::Signals>,
    fiber: Result<PathBuf, String>,
) -> i32 {
    session_command::new_session(
        SessionId(doors::mint("s_")),
        overrides,
        Some(prompt),
        true,
        worktree,
        clock,
        signals,
        fiber,
        None,
        None,
    )
}

/// What an `ask` loop judges with: the workspace the session keeps, the
/// credentials beside it, and the standing rules.
fn ask_permissions(
    home: &Path,
    project: &config::ProjectKey,
    workspace: String,
    credential_files: Vec<PathBuf>,
    clock: &Arc<dyn contract::clock::Clock>,
) -> r#loop::Permissions {
    r#loop::Permissions {
        workspace,
        credentials: home.join("credentials"),
        credential_files,
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
/// what ran. `run` writes the session's first lines through
/// [`Signals::commit`](doors::Signals::commit): `Ok(Some(code))` means
/// nothing was written for this process, and `run_turn` returns `code`
/// with no `quiesce`, no `fiber_exited` and no stderr line. One cancel
/// signal, `cancel`, serves the door and the loop, so
/// a `cancel` ends the turn the prompt starts; a shutdown's code on it is
/// the exit code.
fn run_turn(
    session: &Session,
    log: &Arc<Log>,
    dir: &Path,
    prompt: Option<String>,
    one_turn: bool,
    cancel: Arc<r#loop::TurnCancel>,
    run: impl FnOnce(Receiver<Delivery>, Arc<r#loop::TurnCancel>) -> Result<Option<i32>, Failure>,
) -> i32 {
    let door = Arc::clone(&cancel);
    let turn = Arc::clone(&cancel);
    let mut committed: Option<i32> = None;
    let ran = match prompt {
        // `fiber ask` runs one turn: the prompt, then `close`. It always
        // supplies a prompt; without one it would wait for a client.
        Some(prompt) if one_turn => session.ask(prompt, Arc::new(move || door.cancel()), |inbox| {
            match run(inbox, turn) {
                Ok(code) => {
                    committed = code;
                    Ok(())
                }
                Err(failure) => Err(failure),
            }
        }),
        // The session command queues its prompt when one was supplied and
        // serves clients until idle exit or `close`.
        prompt => session.serve(prompt, Arc::new(move || door.cancel()), |inbox| {
            match run(inbox, turn) {
                Ok(code) => {
                    committed = code;
                    Ok(())
                }
                Err(failure) => Err(failure),
            }
        }),
    };
    // Nothing was written for this process: the commit found a recorded
    // signal, so the log stays as it was and the code is the exit code.
    if let Some(code) = committed {
        return code;
    }
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
/// Stops the servers and prints a startup failure. Only
/// `retry_policy_tests` still fails this way; new sessions report through
/// `session_command::report`, after asking about a recorded signal.
#[cfg(test)]
fn stop_and_fail(servers: mcp_servers::SessionServers, e: Failure) -> i32 {
    servers.servers.stop();
    ask_failed(e)
}

/// `fiber` with no arguments: the terminal on this tty, a client of the hub
/// (`docs/invocation.md`, "Two doors"). Without a tty it is a usage error
/// naming `fiber ask`. The hub it starts listens on its local socket only
/// and is never waited on.
fn terminal(fiber: Result<PathBuf, String>, open_at: tui::OpenAt) -> i32 {
    if let Some(code) = open::needs_tty() {
        return code;
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
    let config = match ::cli::project_of(&home, &workspace).and_then(|(_, project)| {
        Config::load(Sources {
            home: home.clone(),
            workspace: workspace.clone(),
            project,
            overrides: Vec::new(),
        })
        .map_err(|e| failed(e.code(), e))
    }) {
        Ok(config) => config,
        Err(e) => return fail(e),
    };
    let tty = match io::stdin().as_fd().try_clone_to_owned() {
        Ok(tty) => std::fs::File::from(tty),
        Err(e) => return fail(failed(ErrorCode::IoFailed, format!("the terminal: {e}"))),
    };
    let theme = theme_setting::setting(&home, &config, &|path| std::fs::read_to_string(path));
    let seam: Arc<dyn tui::Configure> = Arc::new(configure::Seam::new(home.clone()));
    let hub_clock = Arc::clone(&clock);
    // The picker's model lists: the cached copy at once, refreshed in the
    // background (`docs/model-routing.md`, "Model discovery"). The lock
    // is the one `fiber models` uses: a refresh beside another process
    // refreshes once.
    let models = model_list::reader(
        home.clone(),
        workspace.clone(),
        Arc::clone(&clock),
        Arc::new(tools::PathLocks::new()),
    );
    // The viewer's copies live under Fiber home: cloned before the
    // connect closure moves it.
    let launch_home = home.clone();
    let connect: tui::Connect = Box::new(move || {
        let mut start = || start_hub(fiber.clone());
        doors::hub::connect(&home, &mut start, hub_clock.as_ref())
    });
    let identity = doors::project(&workspace);
    let mut launch = launch::launch(workspace, &identity, &config, theme, &launch_home);
    launch.models = Some(models);
    launch.configure = Some(seam);
    launch.open_at = open_at;
    tui::run(tty, launch, connect, Box::new(crash::attach), clock)
}

/// Starts `fiber hub serve` detached, as every client of the hub does when
/// none is running (`docs/invocation.md`, "The hub").
fn start_hub(exe: Result<PathBuf, String>) -> io::Result<()> {
    let exe = exe.map_err(io::Error::other)?;
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

/// `fiber sessions`: the hub it reaches is started when none runs.
fn sessions_list(
    all: bool,
    json: bool,
    clock: &dyn contract::clock::Clock,
    fiber: Result<PathBuf, String>,
) -> i32 {
    let mut connect = || {
        let home = config::fiber_home_from_env().map_err(io::Error::other)?;
        let mut start = || start_hub(fiber.clone());
        doors::hub::connect(&home, &mut start, clock)
    };
    ::cli::sessions_list(all, json, &mut connect)
}

/// `fiber sessions delete`: the hub it reaches is started when none runs.
fn sessions_delete(
    id: &str,
    cascade: bool,
    yes: bool,
    clock: &dyn contract::clock::Clock,
    fiber: Result<PathBuf, String>,
) -> i32 {
    let mut connect = || {
        let home = config::fiber_home_from_env().map_err(io::Error::other)?;
        let mut start = || start_hub(fiber.clone());
        doors::hub::connect(&home, &mut start, clock)
    };
    ::cli::delete(id, cascade, yes, &mut connect)
}

/// `fiber sessions prune`: the hub it reaches is started when none runs.
fn sessions_prune(
    older_than: Option<String>,
    cascade: bool,
    dry_run: bool,
    yes: bool,
    force: bool,
    clock: &dyn contract::clock::Clock,
    fiber: Result<PathBuf, String>,
) -> i32 {
    let mut connect = || {
        let home = config::fiber_home_from_env().map_err(io::Error::other)?;
        let mut start = || start_hub(fiber.clone());
        doors::hub::connect(&home, &mut start, clock)
    };
    ::cli::prune(
        &::cli::PruneArgs {
            older_than,
            cascade,
            dry_run,
            yes,
            force,
        },
        clock,
        &mut connect,
    )
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
