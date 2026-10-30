//! The `fiber` binary, and the composition root (`docs/architecture.md`):
//! parses argv, builds the session's parts once from configuration, the
//! installed providers and the credential, and hands them to a door
//! (`docs/invocation.md`). It holds no feature logic.

#[cfg(all(feature = "alloc-stats", feature = "dhat-heap"))]
compile_error!(
    "fiber: the alloc-stats and dhat-heap features each set the global allocator; enable only one."
);

#[cfg(feature = "alloc-stats")]
mod alloc_stats;
mod builtin;
mod case;
mod cli;
mod clock;
mod completion;
mod configure;
mod continue_session;
mod delegates;
mod handoff;
mod hub_command;
mod late_emit;
mod launch;
mod mcp_servers;
mod model_host;
mod model_list;
mod open;
mod parts;
mod placeholders;
mod prompt_files;
mod read_signals;
mod reap;
mod reload;
mod resume;
mod rewind;
mod session_command;
mod session_extensions;
mod settings;
mod shutdown;
mod startup_log;
mod switch;
mod theme_setting;
mod tui_extensions;

#[cfg(test)]
#[path = "command_reference_tests.rs"]
mod command_reference_tests;
#[cfg(test)]
#[path = "exit_state_tests.rs"]
mod exit_state_tests;
#[cfg(test)]
#[path = "live_tests.rs"]
mod live_tests;
#[cfg(test)]
#[path = "lua_warm_tests.rs"]
mod lua_warm_tests;
#[cfg(test)]
mod model_host_tests;
#[cfg(test)]
#[path = "read_signals_entry_tests.rs"]
mod read_signals_entry_tests;
#[cfg(test)]
#[path = "reviewer_tests.rs"]
mod reviewer_tests;
#[cfg(test)]
#[path = "scripted_session_tests.rs"]
mod scripted_session_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
#[path = "timing_tests.rs"]
mod timing_tests;

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
use contract::diag::DebugLog as _;
use contract::inbox::Delivery;
use contract::shapes::Failure;
use contract::{ErrorCode, SessionId};
use doors::{Session, failure};
use log::Log;
use r#loop::Loop;
#[cfg(test)]
use parts::parts_in;
use parts::{Parts, choose_reviewer, parts_with_session, per_run, reviewer_model};
use routing::{Here, connect, connect_observed};
use routing::{credential, lua_providers, scripted};

/// The `alloc-stats` build counts every allocation through [`System`]:
/// this static is the only global allocator, and it feeds the calling
/// thread's `log` counters.
#[cfg(feature = "alloc-stats")]
#[global_allocator]
static ALLOC: alloc_stats::Counting = alloc_stats::Counting;

/// The `dhat-heap` build profiles the heap through `dhat`: this static is
/// the only global allocator, and the `run` guard below writes the
/// profile. Never in a shipped build (`docs/dependencies.md`).
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() -> ExitCode {
    ExitCode::from(u8::try_from(run()).unwrap_or(1))
}

#[allow(
    clippy::print_stderr,
    reason = "prints the usage sentence (docs/code-quality.md, \"Lints\")"
)]
fn run() -> i32 {
    // A `dhat-heap` build profiles the whole run: the guard lives until
    // `run` returns, so the profile covers every dispatch arm and the
    // `dhat-heap.json` lands in the working directory at orderly exit.
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::builder().build();
    let fiber = std::env::current_exe().map_err(|error| format!("the running binary: {error}"));
    // Help and version print before anything reads the home, configuration,
    // credentials, or stdin.
    let clock: Arc<dyn contract::clock::Clock> = Arc::new(clock::System);
    // First: every process writes one crash report and aborts on a panic
    // (`docs/code-quality.md`, "What a panic leaves").
    log::crash::install(
        Box::new(|| {
            config::fiber_home(std::env::var_os("FIBER_HOME"), std::env::var_os("HOME"))
                .map_err(|e| e.to_string())
        }),
        tui::restore,
        Arc::clone(&clock),
    );
    match cli::parse() {
        cli::Invocation::Print(error) => {
            // A closed stdout leaves nobody to tell, as `fiber extension list` does.
            error.print().unwrap_or(());
            0
        }
        cli::Invocation::Run(Some(cli::Commands::Version), _) => {
            write!(io::stdout().lock(), "{}", cli::version_line()).unwrap_or(());
            0
        }
        cli::Invocation::Run(Some(cli::Commands::Completion { shell }), _) => {
            completion::print(shell)
        }
        cli::Invocation::Run(Some(cli::Commands::Help { command }), _) => {
            print_help(command.as_slice())
        }
        // `fiber` with no arguments opens the terminal: this tty as a
        // client of one hub, its own or a listed one.
        cli::Invocation::Run(None, top) => open::home(fiber, top.hub, top.model, top.overrides),
        cli::Invocation::Run(
            Some(cli::Commands::Resume {
                id,
                credential,
                hub,
            }),
            _,
        ) => open::resume(id, credential, hub, fiber),
        cli::Invocation::Run(Some(cli::Commands::Continue { hub }), _) => {
            open::continue_latest(fiber, hub)
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
        cli::Invocation::Run(Some(cli::Commands::Ask(args)), _) => {
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
        cli::Invocation::Run(Some(cli::Commands::ExtensionCase { case }), _) => {
            case::run::extension_case(case, clock, fiber)
        }
        cli::Invocation::Run(Some(cli::Commands::Session(args)), _) => {
            session_command::run(args, clock, fiber)
        }
        cli::Invocation::Run(Some(cli::Commands::Hub(command)), _) => {
            hub_command::run(command, fiber, clock.as_ref())
        }
        cli::Invocation::Run(Some(cli::Commands::Sessions(args)), _) => match args.command {
            None => sessions_list(
                args.all,
                args.json,
                args.hub.as_deref(),
                clock.as_ref(),
                fiber,
            ),
            Some(cli::SessionsCommands::Delete {
                hub,
                cascade,
                yes,
                id,
            }) => sessions_delete(&id, cascade, yes, hub.as_deref(), clock.as_ref(), fiber),
            Some(cli::SessionsCommands::Export { id, path }) => ::cli::export(&id, path.as_deref()),
            Some(cli::SessionsCommands::Search { all, json, text }) => {
                ::cli::sessions_search(&text, all, json)
            }
            Some(cli::SessionsCommands::Prune {
                hub,
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
                hub.as_deref(),
                clock.as_ref(),
                fiber,
            ),
        },
        cli::Invocation::Run(Some(cli::Commands::Models(args)), _) => {
            models_entry(args, clock, fiber, read_signals::CancelOnSignal::new)
        }
        cli::Invocation::Run(Some(cli::Commands::RefreshModelLists { providers }), _) => {
            refresh_entry(&providers, clock, read_signals::CancelOnSignal::new)
        }
        cli::Invocation::Run(
            Some(cli::Commands::ReleaseInstall {
                version,
                test_base_url,
            }),
            _,
        ) => ::cli::release_install(
            &version,
            test_base_url.base_url.as_deref(),
            env!("CARGO_PKG_VERSION"),
            option_env!("FIBER_COMMIT"),
            clock.as_ref(),
        ),
        cli::Invocation::Run(Some(cli::Commands::Extension(cmd)), _) => match cmd {
            cli::ExtensionCommands::Install {
                name_or_path,
                project,
            } => ::cli::extension_install(
                &name_or_path,
                project,
                env!("CARGO_PKG_VERSION"),
                clock.as_ref(),
            ),
            cli::ExtensionCommands::Update { name } => {
                ::cli::extension_update(name.as_deref(), env!("CARGO_PKG_VERSION"), clock.as_ref())
            }
            cli::ExtensionCommands::Remove { name } => {
                ::cli::extension_remove(&name, clock.as_ref())
            }
            cli::ExtensionCommands::Reinstall { name } => {
                ::cli::extension_reinstall(&name, env!("CARGO_PKG_VERSION"), clock.as_ref())
            }
            cli::ExtensionCommands::List => ::cli::extension_list(clock.as_ref()),
            cli::ExtensionCommands::Test { path } => {
                ::cli::extension_test(path.as_deref(), fiber, Arc::clone(&clock))
            }
        },
        cli::Invocation::Run(Some(cli::Commands::Mcp(command)), _) => {
            mcp_command(command, Arc::clone(&clock))
        }
        cli::Invocation::Run(Some(cli::Commands::Approve(args)), _) => {
            ::cli::approve(args.yes, env!("CARGO_PKG_VERSION"), Arc::clone(&clock))
        }
        cli::Invocation::Run(Some(cli::Commands::Import { source }), _) => {
            let fiber_home = match config::fiber_home_from_env() {
                Ok(home) => home,
                Err(error) => return fail(failure(error.code(), error.to_string())),
            };
            let check = crate::prompt_files::personal_source_check(
                fiber_home,
                crate::prompt_files::agents_home(std::env::var_os("HOME")),
                Arc::clone(&clock),
            );
            ::cli::import(source.into(), check.as_ref(), clock.as_ref())
        }
        cli::Invocation::Run(Some(cli::Commands::Config(cmd)), _) => match cmd {
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
                ::cli::config_set(layer, &key, &value, clock.as_ref())
            }
        },
        cli::Invocation::Run(Some(cli::Commands::Login(args)), _) => ::cli::run_login(
            args.name.as_deref(),
            args.label.as_deref(),
            args.device,
            Arc::clone(&clock),
        ),
        cli::Invocation::Run(Some(cli::Commands::Logout(args)), _) => {
            let target = match (args.label.as_deref(), args.all) {
                (_, true) => ::cli::LogoutTarget::All,
                (Some(label), false) => ::cli::LogoutTarget::Label(label),
                (None, false) => ::cli::LogoutTarget::Only,
            };
            ::cli::run_logout(args.provider.as_deref(), target)
        }
        cli::Invocation::Run(Some(cli::Commands::Setup(args)), _) => {
            setup_entry(args, clock, read_signals::CancelOnSignal::new)
        }
        cli::Invocation::Run(Some(cli::Commands::Doctor), _) => {
            ::cli::doctor(&cli::bare_version(), clock.as_ref(), &McpClient)
        }
        cli::Invocation::Run(Some(cli::Commands::Stop { hub }), _) => {
            let (home, target) = match hub_target(hub.as_deref()) {
                Ok(target) => target,
                Err(error) => return fail(error),
            };
            if target.is_listed() {
                let mut connect = || connect_hub(&home, &target, &fiber, clock.as_ref());
                ::cli::stop_remote(&mut connect, clock.as_ref())
            } else {
                let mut start = || start_hub(fiber.clone());
                ::cli::stop_local(&home, &mut start, clock.as_ref())
            }
        }
        cli::Invocation::Run(
            Some(cli::Commands::Update {
                channel,
                test_base_url,
            }),
            _,
        ) => {
            let flag = channel.map(|flag| match flag {
                cli::ChannelArg::Stable => ::cli::Channel::Stable,
                cli::ChannelArg::Nightly => ::cli::Channel::Nightly,
            });
            let channel = match ::cli::resolve_channel(
                flag,
                std::env::var("FIBER_CHANNEL").ok().as_deref(),
            ) {
                Ok(channel) => channel,
                Err(error) => {
                    eprintln!("fiber: {error}");
                    return 2;
                }
            };
            let home = match config::fiber_home_from_env() {
                Ok(home) => home,
                Err(e) => return fail(failed(e.code(), e)),
            };
            let stdin = io::stdin();
            let stderr = io::stderr();
            // A test seam, not a user control: the binary update tests
            // point both roots at their fixture server through
            // `--base-url`. Production runs leave it absent.
            let mut bases = ::cli::ReleaseBases::production();
            if let Some(base) = test_base_url.base_url {
                bases.web = base.clone();
                bases.api = base;
            }
            ::cli::update(
                &home,
                env!("CARGO_PKG_VERSION"),
                env!("FIBER_TARGET"),
                channel,
                &bases,
                clock.as_ref(),
                ::cli::Io {
                    terminal: stdin.is_terminal(),
                    input: &mut stdin.lock(),
                    err: &mut stderr.lock(),
                },
                &::extensions::Origin::github(),
            )
        }
        cli::Invocation::Run(Some(cli::Commands::Feedback(args)), _) => {
            match cli::ask_parts(&args.prompt) {
                Ok((prompt, dash)) => feedback(prompt, dash, args.session.map(SessionId), clock),
                Err(_) => {
                    eprintln!("fiber: {}", cli::FEEDBACK_SHAPE);
                    2
                }
            }
        }
        // The hidden search subcommands hold no feature logic: they only
        // call into `tools` (`docs/architecture.md`, "The call rules").
        cli::Invocation::Run(Some(cli::Commands::Grep { args }), _) => tools::grep_main(args),
        cli::Invocation::Run(Some(cli::Commands::Find { args }), _) => tools::find_main(args),
        // The image child holds no feature logic either: `picture` is the
        // only crate that links image code.
        cli::Invocation::Run(Some(cli::Commands::Image { args }), _) => picture::main(args),
    }
}

#[allow(
    clippy::print_stderr,
    reason = "prints the usage sentence (docs/code-quality.md, \"Lints\")"
)]
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

/// `contract::McpRemote` over `mcp`: the seam `cli`'s MCP commands speak
/// through (`docs/architecture.md`, "The call rules").
struct McpClient;

impl contract::McpRemote for McpClient {
    fn has_userinfo(&self, url: &str) -> bool {
        mcp::url_has_userinfo(url)
    }

    fn token_key(&self, url: &str) -> Result<String, contract::shapes::Failure> {
        mcp::token_key(url).map_err(|e| failure(e.code(), e.to_string()))
    }

    fn login(
        &self,
        files: &dyn contract::mcp_tokens::TokenFiles,
        url: &str,
        present: &dyn Fn(&str),
        clock: &dyn contract::clock::Clock,
    ) -> Result<(), contract::shapes::Failure> {
        // The hub keeps its `None`: the login writes no `http_request` line.
        mcp::login_observed(files, url, present, clock, None)
            .map_err(|e| failure(e.code(), e.to_string()))
    }
}

/// The layer `--project` and `--repo` name, as `config set` reads them:
/// the repository's file, the per-project file, or the global file.
fn mcp_layer(project: bool, repo: bool) -> Layer {
    if repo {
        Layer::Repository
    } else if project {
        Layer::Project
    } else {
        Layer::Global
    }
}

/// `fiber mcp add|remove|list`: the parser shapes argv and `cli` checks
/// every value. A shape the parser cannot say — both a url and a command,
/// neither, `-e` or `--secret` with a url — is a usage error here.
#[allow(
    clippy::print_stderr,
    reason = "prints the usage sentence (docs/code-quality.md, \"Lints\")"
)]
fn mcp_command(command: cli::McpCommands, clock: Arc<dyn contract::clock::Clock>) -> i32 {
    match command {
        cli::McpCommands::Add(args) => match mcp_declaration(&args) {
            Ok(declaration) => ::cli::mcp_add(
                mcp_layer(args.project, args.repo),
                &args.name,
                declaration,
                &McpClient,
                clock,
            ),
            Err(sentence) => {
                eprintln!("fiber: {sentence}");
                2
            }
        },
        cli::McpCommands::Remove(args) => {
            ::cli::mcp_remove(mcp_layer(args.project, args.repo), &args.name)
        }
        cli::McpCommands::List => ::cli::mcp_list(clock),
        cli::McpCommands::Login { server } => ::cli::mcp_login(&server, &McpClient, clock),
        cli::McpCommands::Logout { server } => ::cli::mcp_logout(&server, &McpClient),
    }
}

/// The declaration `fiber mcp add`'s arguments shape: exactly one
/// transport, and no `-e` or `--secret` with a url. Each `-e` splits on
/// its first `=`; `cli` checks every name and value.
fn mcp_declaration(args: &cli::McpAddArgs) -> Result<::cli::McpDeclaration, String> {
    const HELP: &str = "Run `fiber --help` for usage.";
    let mut env = Vec::new();
    for raw in &args.env {
        let Some((key, value)) = raw.split_once('=') else {
            return Err(format!("`{raw}` is not `KEY=value`. {HELP}"));
        };
        env.push((key.to_owned(), value.to_owned()));
    }
    match (&args.url, args.command.as_slice()) {
        (Some(url), []) => {
            if !args.env.is_empty() || !args.secret.is_empty() {
                return Err(format!(
                    "`-e` and `--secret` apply only to `-- <command>`, not to a url. {HELP}"
                ));
            }
            Ok(::cli::McpDeclaration::Remote { url: url.clone() })
        }
        (None, [program, rest @ ..]) => Ok(::cli::McpDeclaration::Stdio {
            command: program.clone(),
            args: rest.to_vec(),
            env,
            secrets: args.secret.clone(),
        }),
        (Some(_), _) => Err(format!(
            "`fiber mcp add` takes either a url or `-- <command>`, not both. {HELP}"
        )),
        (None, []) => Err(format!(
            "`fiber mcp add` takes a url or `-- <command>`. {HELP}"
        )),
    }
}

#[allow(
    clippy::print_stderr,
    reason = "prints the usage sentence (docs/code-quality.md, \"Lints\")"
)]
fn fail(e: Failure) -> i32 {
    eprintln!("fiber: {}", e.message);
    doors::exit_code(&e)
}

/// The signal registration an entry point runs: its [`read_signals::Register`].
type Register = read_signals::Register;

/// `fiber models`: lists the installed models, killing what its credential
/// commands left on a normal return and on a foreground signal. The guard
/// is built before the runner, so a refused registration returns before
/// any credential command starts.
fn models_entry(
    args: cli::ModelsArgs,
    clock: Arc<dyn contract::clock::Clock>,
    fiber: Result<PathBuf, String>,
    register: Register,
) -> i32 {
    let reads = Arc::new(crate::switch::Reads::default());
    let guards = match read_signals::guarded(&reads, &clock, &read_signals::FOREGROUND, register) {
        Ok(guards) => guards,
        Err(error) => return fail(error),
    };
    // Bound in watcher-then-cancel order, so the watcher drops first: a
    // taken signal's exit lands before the plain cancel runs.
    let (_cancel, _watcher) = guards;
    let runner = |command: &mut std::process::Command, timeout: std::time::Duration| {
        reads.credential(command, timeout, &*clock)
    };
    ::cli::models(
        args.search.as_deref(),
        args.json,
        Arc::clone(&clock),
        Arc::new(tools::PathLocks::new()),
        fiber,
        &runner,
    )
}

/// `fiber setup`: answers each install question with its command, killing
/// what its credential commands left on a normal return and on a
/// foreground signal. The guard is built before the runner, so a refused
/// registration returns before any credential command starts.
fn setup_entry(
    args: cli::SetupArgs,
    clock: Arc<dyn contract::clock::Clock>,
    register: Register,
) -> i32 {
    let reads = Arc::new(crate::switch::Reads::default());
    let guards = match read_signals::guarded(&reads, &clock, &read_signals::FOREGROUND, register) {
        Ok(guards) => guards,
        Err(error) => return fail(error),
    };
    // Bound in watcher-then-cancel order, so the watcher drops first: a
    // taken signal's exit lands before the plain cancel runs.
    let (_cancel, _watcher) = guards;
    let runner = |command: &mut std::process::Command, timeout: std::time::Duration| {
        reads.credential(command, timeout, &*clock)
    };
    // Clap's `conflicts_with` refuses `--completions` with
    // `--no-completions` and `--hub` with `--no-hub` before this runs,
    // which `setup_flags_conflict_in_pairs` asserts, so both set never
    // reaches the `else if`.
    let completions = if args.no_completions {
        ::cli::Completions::No
    } else if let Some(shell) = args.completions {
        ::cli::Completions::Yes(shell.name().to_owned())
    } else {
        ::cli::Completions::Unset
    };
    let hub = if args.no_hub {
        ::cli::Hub::No
    } else if args.hub {
        ::cli::Hub::Yes
    } else {
        ::cli::Hub::Unset
    };
    let flags = ::cli::SetupFlags {
        provider: args.provider,
        model: args.model,
        thinking: args.thinking,
        import: args
            .import
            .into_iter()
            .map(config::ImportSource::from)
            .collect(),
        completions,
        hub,
    };
    let fiber_home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(error) => return fail(failure(error.code(), error.to_string())),
    };
    let check = crate::prompt_files::personal_source_check(
        fiber_home,
        crate::prompt_files::agents_home(std::env::var_os("HOME")),
        Arc::clone(&clock),
    );
    ::cli::setup(
        &flags,
        check.as_ref(),
        Arc::clone(&clock),
        Arc::new(tools::PathLocks::new()),
        &runner,
    )
}

/// `fiber refresh-model-lists`: refreshes the named providers' cached
/// lists, killing what its credential commands left on a normal return
/// and on a foreground signal. Always exits 0, as the background refresh
/// does. The guard is built before the runner, so a refused registration
/// returns before any credential command starts.
fn refresh_entry(
    providers: &[String],
    clock: Arc<dyn contract::clock::Clock>,
    register: Register,
) -> i32 {
    let reads = Arc::new(crate::switch::Reads::default());
    let guards = match read_signals::guarded(&reads, &clock, &read_signals::FOREGROUND, register) {
        Ok(guards) => guards,
        Err(error) => return fail(error),
    };
    // Bound in watcher-then-cancel order, so the watcher drops first.
    let (_cancel, _watcher) = guards;
    let run = |command: &mut std::process::Command, timeout: std::time::Duration| {
        reads.credential(command, timeout, &*clock)
    };
    ::cli::refresh_model_lists(
        providers,
        Arc::clone(&clock),
        Arc::new(tools::PathLocks::new()),
        &run,
    );
    0
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
    let signals = match crate::shutdown::install(Arc::clone(&clock)) {
        Ok(signals) => signals,
        Err(e) => return ask_failed(failed(ErrorCode::IoFailed, format!("signals: {e}"))),
    };
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let prompt = match doors::prompt(arg, dash, &mut stdin.lock(), terminal) {
        Ok(prompt) => prompt,
        Err(e) => return ask_failed(Failure::from(e)),
    };
    match resume {
        Some(resuming) => resume::ask_resume(resuming, overrides, prompt, clock, &signals, fiber),
        None => ask_new(overrides, prompt, worktree, clock, &signals, fiber),
    }
}

/// The `doctor` output both feedback doors capture: the production
/// report through the same `run` seam `doctor` prints, mapped into the
/// report writer's error.
fn doctor_report(
    home: &Path,
    workspace: &Path,
    version: &str,
    clock: &dyn contract::clock::Clock,
) -> Result<String, ::cli::FeedbackError> {
    ::cli::capture(home, workspace, version, clock, &McpClient)
        .map_err(|error| ::cli::FeedbackError::Doctor(error.to_string()))
}

/// `fiber feedback`: resolves the description like `fiber ask` resolves
/// its prompt and the session like `fiber continue` does, then runs the
/// shared report writer; the path and the issue link print on stdout.
fn feedback(
    arg: Option<String>,
    dash: bool,
    session: Option<SessionId>,
    clock: Arc<dyn contract::clock::Clock>,
) -> i32 {
    let stdin = io::stdin();
    let description = match doors::prompt(arg, dash, &mut stdin.lock(), stdin.is_terminal()) {
        Ok(description) => description,
        // Stdin shapes that yield no description carry `ask`'s no-prompt
        // sentence; the report writer's empty-description sentence names
        // the command the person ran.
        Err(error) if error.code() == ErrorCode::Usage => {
            return fail(::cli::FeedbackError::EmptyDescription.into());
        }
        Err(error) => return fail(Failure::from(error)),
    };
    let home = match config::fiber_home_path_from_env() {
        Ok(home) => home,
        Err(error) => return fail(failure(error.code(), error.to_string())),
    };
    let workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(source) => {
            return fail(failed(
                ErrorCode::IoFailed,
                format!("the current directory: {source}"),
            ));
        }
    };
    let session = match ::cli::select_session(&home, &workspace, session) {
        Ok(session) => session,
        Err(error) => return fail(error),
    };
    let version = cli::bare_version();
    let context = ::cli::ReportContext {
        version: &version,
        cwd: &workspace,
        clock: clock.as_ref(),
        doctor: &|| doctor_report(&home, &workspace, &version, clock.as_ref()),
        diagnostics: &|id| ::cli::diagnostics(&home, clock.as_ref(), id),
    };
    match ::cli::write(&description, session.as_ref(), &context) {
        Ok(done) => {
            // A closed stdout leaves nobody to tell, as `fiber --version` does.
            let mut out = io::stdout().lock();
            writeln!(out, "{}", done.path.display()).unwrap_or(());
            writeln!(out, "{}", done.link).unwrap_or(());
            0
        }
        Err(error) => fail(error.into()),
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
        None,
        clock,
        signals,
        fiber,
        None,
        None,
        false,
        Vec::new(),
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
/// the chain is made once. The effective answerability is `answerable`
/// without `supervised`: false for one-turn `fiber ask` and for supervised
/// sessions, new or resumed; the session command serves clients that may
/// answer (`docs/permissions.md`, "Headless", "Supervised sessions").
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
    supervised: bool,
    reviewer: Result<r#loop::Reviewer, Failure>,
    limits: r#loop::BlockLimits,
    retry: r#loop::Retry,
    cancel: Arc<r#loop::TurnCancel>,
    slot: &mut Option<r#loop::Summary>,
) -> Result<(), Failure> {
    let answerable = crate::session_command::effective_answerable(answerable, supervised);
    looped
        .map(|looped| {
            looped
                .budget(budget)
                .idle_exit(idle)
                // Only one-turn `fiber ask` is not answerable, and it exits
                // when its run ends: it never warms. A supervised session
                // never warms either: nothing may answer it.
                .warm(warm.filter(|_| answerable))
                .answerable(answerable)
                .reviewer(reviewer, limits)
                .retry(retry)
                .cancelled_by(cancel)
        })
        .and_then(|looped| {
            let ran = looped.run();
            // A built loop always leaves its summary behind, for
            // `fiber_exited` below; a loop that never built leaves the
            // slot as it was found.
            *slot = Some(ran.summary);
            ran.result
        })
        .map_err(|e| failed(e.code(), e))
}

/// Closes state before the final drain, without extending a rewound or
/// moved log: the transition line already closed that process boundary.
fn write_state_at_exit(
    log: &Log,
    dir: &Path,
    extensions: &extensions::SessionExtensions,
) -> Result<(), r#loop::Error> {
    use contract::hook::Hooks;
    extensions.close_state();
    let lines = extensions.take_state_lines();
    if log::last_line(dir).is_some_and(|line| line.kind == "rewound" || line.kind == "moved") {
        return Ok(());
    }
    for event in lines {
        log.append(&event, None, None)?;
    }
    Ok(())
}

/// The process exit after the session-end drain: the shutdown code only
/// when the drain was stopped and a shutdown is recorded (Ruling 19,
/// T4(d)), else the `fiber_exited` code. A stop with no shutdown recorded
/// (an abandonment, an idle hub) keeps `exited`; a shutdown recorded with
/// no stop keeps `exited` (the #2980 case).
fn drain_exit_code(exited: i32, stopped: bool, code: Option<i32>) -> i32 {
    match (stopped, code) {
        (true, Some(code)) => code,
        _ => exited,
    }
}

/// Runs one turn of the session `session` writes to `log`, once its log,
/// model and prompt are known, shared by new and resumed sessions: sends
/// the first prompt, closes when `one_turn`, and writes `fiber_exited` for
/// what ran. `run` writes the session's first lines through
/// [`Signals::commit`](doors::Signals::commit): `Ok(Some(code))` means
/// nothing was written to the session log for this process, and `run_turn` returns `code`
/// with no `quiesce`, no `fiber_exited` and no stderr line. One cancel
/// signal, `cancel`, serves the door and the loop, so
/// a `cancel` ends the turn the prompt starts; a shutdown's code on it is
/// the exit code.
#[allow(
    clippy::too_many_arguments,
    reason = "one hand-off of the turn's end: the session, log, prompt, mode, cancel, diagnostics and runner"
)]
fn run_turn(
    session: &Session,
    log: &Arc<Log>,
    dir: &Path,
    prompt: Option<String>,
    one_turn: bool,
    cancel: Arc<r#loop::TurnCancel>,
    diag: &Arc<log::diag::Diag>,
    current: &Arc<session_extensions::Current>,
    run: impl FnOnce(
        Receiver<Delivery>,
        Arc<r#loop::TurnCancel>,
        &mut Option<r#loop::Summary>,
    ) -> Result<Option<i32>, Failure>,
) -> i32 {
    // The session's watchers, subscribed for the whole turn: every appended
    // line fans out to them, until the shutdown drops the offer. Dropped
    // before `fiber_exited` below, so the session-end drain owns the exit
    // line.
    let _watchers = session_extensions::subscribe(log, current, &cancel);
    let door = Arc::clone(&cancel);
    let turn = Arc::clone(&cancel);
    let mut committed: Option<i32> = None;
    // The summary the run leaves behind, as `committed` is: `fiber_exited`
    // reads it, never the log.
    let mut summary: Option<r#loop::Summary> = None;
    let ran = match prompt {
        // `fiber ask` runs one turn: the prompt, then `close`. It always
        // supplies a prompt; without one it would wait for a client.
        Some(prompt) if one_turn => session.ask(prompt, Arc::new(move || door.cancel()), |inbox| {
            match run(inbox, turn, &mut summary) {
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
            match run(inbox, turn, &mut summary) {
                Ok(code) => {
                    committed = code;
                    Ok(())
                }
                Err(failure) => Err(failure),
            }
        }),
    };
    // Nothing was written to the session log for this process: the commit found a recorded
    // signal, so the log stays as it was and the code is the exit code.
    if let Some(code) = committed {
        return code;
    }
    // `fiber_exited` is the last line: nothing on the door side follows it.
    session.quiesce();
    let written = write_state_at_exit(log, dir, &current.get());
    let ran = ran.and(written.map_err(|error| failed(error.code(), error)));
    // The exit line belongs to the session-end drain, not the live fan-out:
    // dropping the subscription keeps `fiber_exited` unoffered, so the drain
    // replays it and collects its failures for stderr below.
    drop(_watchers);
    // A `fiber_exited` that cannot be written leaves a log that reads as a
    // process that died, which it then is.
    let summary = summary.unwrap_or_default();
    match r#loop::fiber_exited(log, &summary, ran, one_turn, cancel.shutdown_code()) {
        Ok(exited) => {
            // `fiber ask` names its failure on stderr, the sentence
            // `fiber_exited` just carried (`docs/errors.md`, "What a
            // caller gets"). The session command's clients read
            // `fiber_exited` itself, so it stays silent.
            if one_turn && let Some(error) = &exited.error {
                writeln!(io::stderr(), "fiber: {}", error.message).unwrap_or(());
            }
            // The session-end watcher drain: every watcher's `(cursor, end]`
            // range runs now, within its own timeout, and each failure is
            // printed under `fiber_exited` — never as a notice, since the
            // log is already closed to new lines (`docs/extensions.md`,
            // "When a session ends").
            let (failures, stopped) = current.get().drain_watchers(log, dir);
            for failure in failures {
                writeln!(
                    io::stderr(),
                    "fiber: {} {} failed at session end: {}",
                    failure.extension,
                    failure.callback,
                    failure.message
                )
                .unwrap_or(());
            }
            // Process extensions shut down after the watcher drain: their
            // pending watcher replies wait out each manifest's finish
            // allowance, and a failure prints under `fiber_exited` with
            // the watcher failures, never as a notice.
            for failure in current.get().finish_processes(&*cancel) {
                writeln!(
                    io::stderr(),
                    "fiber: {} {} failed at session end: {}",
                    failure.extension,
                    failure.callback,
                    failure.message
                )
                .unwrap_or(());
            }
            // The process's exit sample, after its successful exit line:
            // whatever the exit code, a failed read writes nothing.
            diag.resources();
            // A signal during the drain wins over the pre-signal line
            // (Ruling 19): `fiber_exited` already holds the pre-signal
            // code, but the process exits with the signal code.
            drain_exit_code(exited.code, stopped, cancel.shutdown_code())
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

/// The terminal's shared reads with its guards: a plain cancel for a
/// normal quit with a watcher that cancels and settles on SIGTERM and
/// SIGHUP. Built before the model-list reader, so a signal cancels a
/// picker read in flight. The watcher never exits: the TUI takes its
/// normal exit path and returns the signal's code. The guards drop after
/// `tui::run` returns: the watcher first, then the plain cancel.
fn terminal_guards(
    clock: &Arc<dyn contract::clock::Clock>,
    register: Register,
) -> Result<
    (
        Arc<crate::switch::Reads>,
        read_signals::CancelAndSettle,
        read_signals::CancelOnSignal,
    ),
    Failure,
> {
    let reads = Arc::new(crate::switch::Reads::default());
    read_signals::guarded(&reads, clock, &read_signals::TERMINAL, register)
        .map(|(cancel, watcher)| (reads, cancel, watcher))
}

/// `fiber` with no arguments: the terminal on this tty, a client of the
/// hub `target` names (`docs/invocation.md`, "Two doors"). Without a tty
/// it is a usage error naming `fiber ask`. The local hub starts when none
/// runs; a listed one never does. `model` and `overrides` are the run's
/// top-level flags, forwarded into the launch description; a resume or a
/// continue carries none.
#[allow(
    clippy::print_stderr,
    reason = "prints client configuration notices (docs/code-quality.md, \"Lints\")"
)]
fn terminal(
    fiber: Result<PathBuf, String>,
    target: doors::hub::Target,
    open_at: tui::OpenAt,
    model: Option<String>,
    overrides: Vec<String>,
) -> i32 {
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
    let project = match ::cli::project_of(&home, &workspace) {
        Ok((_, project)) => project,
        Err(error) => return fail(Failure::from(error)),
    };
    let sources = Sources {
        home: home.clone(),
        workspace: workspace.clone(),
        project,
        overrides: Vec::new(),
    };
    let config = match Config::load(sources.clone()) {
        Ok(config) => config,
        Err(error) => return fail(failed(error.code(), error)),
    };
    // A listed hub owns no startup notice: the client's own ignored keys
    // print once to stderr before the terminal takes the screen, while a
    // local launch carries them as startup notices instead.
    if target.is_listed() {
        for line in open::notice_lines(config.notices()) {
            eprintln!("{line}");
        }
    }
    let tty = match io::stdin().as_fd().try_clone_to_owned() {
        Ok(tty) => std::fs::File::from(tty),
        Err(e) => return fail(failed(ErrorCode::IoFailed, format!("the terminal: {e}"))),
    };
    let theme = theme_setting::setting(&home, &config, &|path| std::fs::read_to_string(path));
    let seam: Arc<dyn tui::Configure> = Arc::new(configure::Seam::new(home.clone()));
    let hub_clock = Arc::clone(&clock);
    // The entry point's guards before the picker reader: a refused
    // registration returns before any credential command starts, and a
    // normal quit cancels once `tui::run` returns.
    let (reads, _cancel, _watcher) =
        match terminal_guards(&clock, read_signals::CancelOnSignal::watch) {
            Ok(guards) => guards,
            Err(error) => return fail(error),
        };
    // The picker's model lists: the cached copy at once, refreshed in the
    // background (`docs/model-routing.md`, "Model discovery"). The lock
    // is the one `fiber models` uses: a refresh beside another process
    // refreshes once. The terminal's diagnostic writer installs first:
    // built here, before the reader, so an entry script's own `host.http`
    // still records; at info no writer passes.
    let diag_home = home.clone();
    // The terminal's diagnostic writer: one process, one file. Built once
    // here and shared with the terminal, so the error path below writes to
    // the same file a run would have (`docs/state.md`, "Diagnostic logs").
    let diag = Arc::new(log::diag::Diag::new(
        &diag_home,
        log::diag::Process::Tui,
        if config::diagnostics_debug(&config) {
            log::diag::Level::Debug
        } else {
            log::diag::Level::Info
        },
        Arc::clone(&clock),
    ));
    let models = model_list::reader(
        home.clone(),
        workspace.clone(),
        Arc::clone(&clock),
        Arc::new(tools::PathLocks::new()),
        Arc::clone(&reads),
        diag.debugging()
            .then(|| Arc::clone(&diag) as Arc<dyn contract::diag::DebugLog>),
    );
    // The viewer's copies live under Fiber home: cloned before the
    // connect closure moves it, as is the diagnostic writer's.
    let launch_home = home.clone();
    // The interactive door's report writer reads the same launch
    // directory the terminal opens in: cloned before `launch` takes it.
    let feedback_home = home.clone();
    let feedback_workspace = workspace.clone();
    let feedback_version = cli::bare_version();
    let feedback_clock = Arc::clone(&clock);
    // The `doctor` output both the feedback writer and the `/doctor`
    // view read: one closure over the launch directory, home, version
    // and clock, so the view shows the report the headless door would
    // print.
    let report_home = home.clone();
    let report_workspace = workspace.clone();
    let report_version = cli::bare_version();
    let report_clock = Arc::clone(&clock);
    let report = std::sync::Arc::new(move || {
        doctor_report(
            &report_home,
            &report_workspace,
            &report_version,
            report_clock.as_ref(),
        )
    });
    let identity = worktree::project(&workspace);
    let mut launch = launch::launch(
        workspace,
        &identity,
        &config,
        theme,
        &launch_home,
        model,
        overrides,
    );
    if let doors::hub::Target::Listed { name, .. } = &target {
        launch = launch::remote(launch, name);
    }
    // The converter runs this executable again: its path, when the run
    // knows one, rides the launch so JPEG and WebP convert before Kitty
    // transmission (`docs/tui.md`, "Images").
    launch.fiber = fiber.as_ref().ok().cloned();
    let connect: tui::Connect = Box::new(move || {
        connect_hub(&home, &target, &fiber, hub_clock.as_ref()).map_err(io::Error::other)
    });
    launch.models = Some(models);
    launch.configure = Some(seam);
    launch.extensions = Some(Arc::new(tui_extensions::Loader::new(
        sources,
        Arc::clone(&clock),
    )));
    // The interactive door's report writer: `cli::feedback::write`
    // behind the contract seam, beside the model-list reader. It captures
    // the launch directory, so a later `/feedback` reports the same
    // `doctor` output the headless door would.
    let feedback_report = std::sync::Arc::clone(&report);
    let feedback: tui::WriteFeedback =
        std::sync::Arc::new(move |description: &str, session: Option<&SessionId>| {
            let context = ::cli::ReportContext {
                version: &feedback_version,
                cwd: &feedback_workspace,
                clock: feedback_clock.as_ref(),
                doctor: &|| feedback_report(),
                diagnostics: &|id| ::cli::diagnostics(&feedback_home, feedback_clock.as_ref(), id),
            };
            ::cli::write(description, session, &context).map_err(|error| error.to_string())
        });
    launch.feedback = Some(feedback);
    // The `/doctor` view's report reader: `cli::doctor`'s capture
    // behind the contract seam, beside the report writer. It shares the
    // feedback writer's report closure, so the view shows the same
    // `doctor` output the headless door prints.
    let doctor: contract::feedback::ReadDoctor =
        std::sync::Arc::new(move || report().map_err(|error| error.to_string()));
    launch.doctor = Some(doctor);
    launch.open_at = open_at;
    match tui::run(
        tty,
        launch,
        connect,
        Box::new(log::crash::attach),
        Arc::clone(&clock),
        Arc::clone(&diag),
    ) {
        Ok(code) => code,
        Err(error) => {
            let failure = failed(error.code(), error.to_string());
            // The terminal never started: one `error` line in
            // `logs/tui-<pid>.log` keeps the failure diagnosable, as a
            // startup failure does (`docs/state.md`, "Diagnostic logs").
            let code = serde_json::to_value(&failure.code)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "other".to_owned());
            diag.line(log::diag::Severity::Error, None, &code, &failure.message);
            fail(failure)
        }
    }
}

/// Starts `fiber hub serve` detached, as every client of the hub does when
/// none is running (`docs/invocation.md`, "The hub"). The child is handed
/// to its reaper thread at once: the hub keeps running while the thread
/// owns its wait. A reaper thread that never starts leaves the child
/// running unreaped, and its failure is recorded in the diagnostic log.
fn start_hub(exe: Result<PathBuf, String>) -> io::Result<()> {
    let exe = exe.map_err(io::Error::other)?;
    let child = std::process::Command::new(exe)
        .arg("hub")
        .arg("serve")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()?;
    if let Err(error) = reap::on_thread(child) {
        startup_log::record(
            &failed(ErrorCode::IoFailed, format!("hub reap: {error}")),
            Arc::new(clock::System),
        );
    }
    Ok(())
}

/// Connects to the hub under `home`, starting it with `fiber` when none runs.
fn connect_hub(
    home: &Path,
    target: &doors::hub::Target,
    fiber: &Result<PathBuf, String>,
    clock: &dyn contract::clock::Clock,
) -> Result<doors::hub::Hub, doors::Error> {
    let mut start = || start_hub(fiber.clone());
    doors::hub::connect_target(home, target, &mut start, clock)
}

fn hub_target(named: Option<&str>) -> Result<(PathBuf, doors::hub::Target), Failure> {
    let home = config::fiber_home_from_env().map_err(|e| failed(e.code(), e))?;
    let workspace = std::env::current_dir()
        .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
    let target =
        doors::hub::load_target(&home, &workspace, named).map_err(|e| failed(e.code(), e))?;
    Ok((home, target))
}

/// `fiber sessions`: the hub it reaches is started when none runs.
fn sessions_list(
    all: bool,
    json: bool,
    named: Option<&str>,
    clock: &dyn contract::clock::Clock,
    fiber: Result<PathBuf, String>,
) -> i32 {
    let (home, target) = match hub_target(named) {
        Ok(target) => target,
        Err(error) => return fail(error),
    };
    let mut connect = || connect_hub(&home, &target, &fiber, clock);
    ::cli::sessions_list(all || target.is_listed(), json, &mut connect)
}

/// `fiber sessions delete`: the hub it reaches is started when none runs.
fn sessions_delete(
    id: &str,
    cascade: bool,
    yes: bool,
    named: Option<&str>,
    clock: &dyn contract::clock::Clock,
    fiber: Result<PathBuf, String>,
) -> i32 {
    let (home, target) = match hub_target(named) {
        Ok(target) => target,
        Err(error) => return fail(error),
    };
    let hub = match &target {
        doors::hub::Target::Local => None,
        doors::hub::Target::Listed { name, .. } => Some(name.as_str()),
    };
    let mut connect = || connect_hub(&home, &target, &fiber, clock);
    ::cli::delete(id, cascade, yes, hub, &mut connect)
}

/// `fiber sessions prune`: the hub it reaches is started when none runs.
#[allow(
    clippy::too_many_arguments,
    reason = "one hand-off of prune's flags: what to prune, the hub, the clock and the binary"
)]
fn sessions_prune(
    older_than: Option<String>,
    cascade: bool,
    dry_run: bool,
    yes: bool,
    force: bool,
    named: Option<&str>,
    clock: &dyn contract::clock::Clock,
    fiber: Result<PathBuf, String>,
) -> i32 {
    let (home, target) = match hub_target(named) {
        Ok(target) => target,
        Err(error) => return fail(error),
    };
    let hub = match &target {
        doors::hub::Target::Local => None,
        doors::hub::Target::Listed { name, .. } => Some(name.as_str()),
    };
    let mut connect = || connect_hub(&home, &target, &fiber, clock);
    ::cli::prune(
        &::cli::PruneArgs {
            older_than,
            cascade,
            dry_run,
            yes,
            force,
        },
        hub,
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
    startup_log::record(&e, Arc::new(clock::System));
    doors::exit_before_session(e, &mut io::stdout(), &mut io::stderr())
}
