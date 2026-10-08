//! The `fiber` command line (`docs/invocation.md`, "Commands and flags"):
//! the menu, the version, and the one-sentence form of a parse error.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::LazyLock;

use clap::error::{ContextKind, ContextValue};
use clap::{ArgAction, CommandFactory, FromArgMatches, Parser, Subcommand};

/// The hand-grouped menu (`docs/invocation.md`, "Commands and flags").
/// Clap appends the trailing newline when it prints.
const MENU: &str = r#"Fiber, a coding agent.

Usage: fiber <command> [arguments]

Sessions:
  ask [--model <model>] [--resume <id>] [--worktree] [<prompt>] [-]  Run one session of one turn; its events go to stdout
  sessions [--all] [--json]                                          List sessions: id, state, name, what it waits on, spend
  sessions delete [--cascade] [--yes] <id>                           Delete a session, and with --cascade the sessions that continue it
  sessions export <id> [<path>]                                      Write the session's log and its artifacts to <path>
  sessions prune [--older-than <duration>] [--dry-run]               Delete old sessions, worktrees and diagnostic logs
  models [<search>] [--json]                                         List the models the installed providers serve

Fiber itself:
  approve [--yes]                           Show what this repository ships and approve it
  login [<name>] [--as <label>]             Store a provider's key or an extension's secret
  logout <provider> [--as <label> | --all]  Delete a provider's stored key
  completion <shell>                        Print a completion script for bash, zsh or fish
  help [<command>]                          Print this menu, or a command's help
  version                                   Print the version

Extensions:
  extension install <name or path>  Install an extension and its dependencies
  extension update [<name>]         Update one extension, or every installed extension, to its newest tag
  extension remove <name>           Remove an extension, the dependencies nothing else uses, and their data
  extension list                    List installed extensions: name, version and commit
  extension test [<path>]           Run an extension's test cases against the scripted provider

Configuration:
  config get <key>                              Print the effective value and the layer it came from
  config set [--project | --repo] <key> <value>  Write one key in one layer's file

The hub:
  hub install [--port <port>]  Register the hub as a login service
  hub uninstall                Remove the hub's login service; running sessions carry on
  hub status [--json]          Print the hub's state: running, version, port, clients, devices, installed

Flags:
  -h, --help     Print this menu
  -v, --version  Print the version

Examples:
  fiber ask "review the diff on this branch"
  fiber ask < brief.md
  git diff | fiber ask "review this diff" -
  fiber extension install openrouter
  fiber login openrouter
  fiber help ask"#;

const HELP_SUFFIX: &str = " Run `fiber --help` for usage.";

const ASK_SHAPE: &str = "`fiber ask` takes one prompt, then an optional `-`; quote the prompt. Run `fiber --help` for usage.";

/// What `fiber` was asked to do, or the parse error.
#[derive(Debug)]
pub(crate) enum Invocation {
    /// Help or version, printed with clap's printer.
    Print(clap::Error),
    /// A command, or no arguments.
    Run(Option<Commands>),
    /// A usage error. `ask` is whether the invocation runs a session:
    /// `ask`, or the internal `session` command. Both fail before any
    /// session through the same pre-session exit.
    Usage {
        /// Whether the invocation's first argument is `ask`.
        ask: bool,
        /// The one sentence, without the `fiber: ` prefix.
        sentence: String,
    },
}

#[derive(Parser)]
#[command(
    name = "fiber",
    about = "Fiber, a coding agent.",
    disable_version_flag = true,
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Commands {
    /// Run one session of one turn; its events go to stdout
    Ask(AskArgs),
    /// List sessions, or delete, export or prune them
    Sessions(SessionsArgs),
    /// List the models the installed providers serve
    Models(ModelsArgs),
    /// Manage extensions
    #[command(subcommand, arg_required_else_help = false)]
    Extension(ExtensionCommands),
    /// Show what this repository ships and approve it
    Approve(ApproveArgs),
    /// Print a configuration value, or write one
    #[command(subcommand, arg_required_else_help = false)]
    Config(ConfigCommands),
    /// Store a provider's key or an extension's secret
    Login(LoginArgs),
    /// Delete a provider's stored key
    Logout(LogoutArgs),
    /// Print a completion script for bash, zsh or fish
    Completion {
        /// The shell to complete in.
        #[arg(value_name = "shell")]
        shell: crate::completion::Shell,
    },
    /// Print the version
    Version,
    /// Print this menu, or a command's help
    Help {
        /// The command to describe, a word per level: `extension install`.
        /// Absent prints the menu.
        #[arg(value_name = "command")]
        command: Vec<String>,
    },
    /// The search behind the shell's `grep`: hidden and free to change.
    #[command(hide = true, disable_help_flag = true)]
    Grep {
        /// Everything after `grep`, passed through untouched.
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "args"
        )]
        args: Vec<OsString>,
    },
    /// The search behind the shell's `find`: hidden and free to change.
    #[command(hide = true, disable_help_flag = true)]
    Find {
        /// Everything after `find`, passed through untouched.
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "args"
        )]
        args: Vec<OsString>,
    },
    /// The image child behind `read`: hidden and free to change.
    #[command(hide = true, disable_help_flag = true)]
    Image {
        /// Everything after `image`, passed through untouched.
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "args"
        )]
        args: Vec<OsString>,
    },
    /// The case runner's child command: hidden and free to change.
    #[command(hide = true)]
    ExtensionCase {
        /// The case file the parent asked this child to run.
        #[arg(value_name = "case")]
        case: PathBuf,
    },
    /// The internal session command: hidden and free to change.
    #[command(hide = true)]
    Session(SessionArgs),
    /// The detached model-list refresh `fiber models` spawns: hidden and
    /// free to change.
    #[command(hide = true, disable_help_flag = true)]
    RefreshModelLists {
        /// The providers to refresh, by name.
        #[arg(value_name = "provider")]
        providers: Vec<String>,
    },
    /// The release install step `install.sh` runs: hidden and free to
    /// change.
    #[command(hide = true)]
    ReleaseInstall {
        /// The release's version, which must be this binary's own.
        #[arg(value_name = "version")]
        version: String,
        /// Where releases are published, for tests.
        #[arg(long = "base-url", value_name = "url")]
        base_url: Option<String>,
    },
    /// Manage the hub's login service, or print the hub's state
    #[command(subcommand, arg_required_else_help = false)]
    Hub(HubCommands),
}

#[derive(Debug, Subcommand)]
#[command(disable_help_subcommand = true)]
pub(crate) enum HubCommands {
    /// Register the hub as a login service
    Install {
        /// Also listen on this port of `127.0.0.1`, where every connection
        /// presents a device token. Without it the hub listens on its local
        /// socket only.
        #[arg(long, value_name = "port", value_parser = clap::value_parser!(u16).range(1..))]
        port: Option<u16>,
    },
    /// Remove the hub's login service; running sessions carry on
    Uninstall,
    /// Print the hub's state: running, version, port, clients, devices, installed
    Status {
        /// Print one JSON object.
        #[arg(long)]
        json: bool,
    },
    /// The internal hub command: hidden and free to change.
    #[command(hide = true)]
    Serve {
        /// Run as the login service: wait for the `run/` lock and never
        /// exit for being idle.
        #[arg(long, hide = true)]
        installed: bool,
    },
}

/// `fiber sessions`: the list, or one of its subcommands. A flag with a
/// subcommand is a usage error.
#[derive(Debug, clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct SessionsArgs {
    #[command(subcommand)]
    pub(crate) command: Option<SessionsCommands>,
    /// List every project, not only this repository's.
    #[arg(long)]
    pub(crate) all: bool,
    /// Print each session as one JSON object per line.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Subcommand)]
#[command(disable_help_subcommand = true)]
pub(crate) enum SessionsCommands {
    /// Delete a session, and with --cascade the sessions that continue it
    Delete {
        /// Also delete every session that continues it: its forks and
        /// rewinds, and theirs.
        #[arg(long)]
        cascade: bool,
        /// Delete without asking.
        #[arg(long)]
        yes: bool,
        /// The session: a full id or a unique prefix of one.
        #[arg(value_name = "id")]
        id: String,
    },
    /// Write the session's log and its artifacts to <path>
    Export {
        /// The session: a full id or a unique prefix of one.
        #[arg(value_name = "id")]
        id: String,
        /// The directory to write, from the current directory when
        /// relative; the session id when absent.
        #[arg(value_name = "path")]
        path: Option<PathBuf>,
    },
    /// Delete old sessions, worktrees and diagnostic logs
    Prune {
        /// Delete every exited session whose last line is older than
        /// this, such as `30d`: a whole number and `s`, `m`, `h` or `d`.
        /// Without it no session is deleted.
        #[arg(long, value_name = "duration")]
        older_than: Option<String>,
        /// Also delete every session that continues a deleted one.
        #[arg(long)]
        cascade: bool,
        /// Print what would be deleted and the space it would free,
        /// and delete nothing.
        #[arg(long)]
        dry_run: bool,
        /// Prune without asking.
        #[arg(long)]
        yes: bool,
        /// Remove a worktree even when removing it would lose something.
        /// A worktree a running session works in is never removed.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
#[command(disable_help_subcommand = true)]
pub(crate) enum ExtensionCommands {
    /// Install an extension and its dependencies
    Install {
        /// The extension's name, or a path to its package.
        #[arg(value_name = "name or path")]
        name_or_path: String,
    },
    /// Update one extension, or every installed extension, to its newest tag
    Update {
        /// The installed extension's name.
        #[arg(value_name = "name")]
        name: Option<String>,
    },
    /// Remove an extension, the dependencies nothing else uses, and their data
    Remove {
        /// The installed extension's name.
        #[arg(value_name = "name")]
        name: String,
    },
    /// List installed extensions: name, version and commit
    List,
    /// Run an extension's test cases against the scripted provider
    Test {
        /// The extension package; defaults to the current directory.
        #[arg(value_name = "path")]
        path: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
#[command(disable_help_subcommand = true)]
pub(crate) enum ConfigCommands {
    /// Print the effective value and the layer it came from
    Get {
        /// The dotted key, such as `model` or `handoff.tokens`.
        #[arg(value_name = "key")]
        key: String,
    },
    /// Write one key in one layer's file
    Set {
        /// Write the per-project file in Fiber home.
        #[arg(long, conflicts_with = "repo")]
        project: bool,
        /// Write the repository's `.fiber/config.json`.
        #[arg(long, conflicts_with = "project")]
        repo: bool,
        /// The dotted key, such as `model`.
        #[arg(value_name = "key")]
        key: String,
        /// The value as JSON, or a bare string when it does not parse as JSON.
        #[arg(value_name = "value")]
        value: String,
    },
}

#[derive(Debug, clap::Args)]
pub(crate) struct ApproveArgs {
    /// Approve without asking, for a script or a machine image.
    #[arg(long)]
    pub(crate) yes: bool,
}

#[derive(Debug, clap::Args)]
pub(crate) struct LoginArgs {
    /// The installed provider to store a key for, or the declared secret to
    /// store. With none, a terminal offers the installed providers and the
    /// declared secrets.
    #[arg(value_name = "name")]
    pub(crate) name: Option<String>,
    /// The credential label to store a provider's key under. Without it the
    /// label is the account's email when the login reveals one, otherwise
    /// `default`.
    #[arg(long = "as", value_name = "label")]
    pub(crate) label: Option<String>,
}

#[derive(Debug, clap::Args)]
pub(crate) struct LogoutArgs {
    /// The provider whose stored key to delete. A missing provider is a
    /// usage error `fiber` words itself, so it matches the other sentences.
    #[arg(value_name = "provider")]
    pub(crate) provider: Option<String>,
    /// Delete this credential label alone.
    #[arg(long = "as", value_name = "label", conflicts_with = "all")]
    pub(crate) label: Option<String>,
    /// Delete every stored label.
    #[arg(long)]
    pub(crate) all: bool,
}

#[derive(Debug, clap::Args)]
pub(crate) struct ModelsArgs {
    /// Only list models whose `provider/model` holds this text.
    #[arg(value_name = "search")]
    pub(crate) search: Option<String>,

    /// Print one JSON object per model.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, clap::Args)]
pub(crate) struct SessionArgs {
    /// The session's id, minted by whoever starts it.
    #[arg(long, value_name = "session_id", value_parser = parse_session_id)]
    pub(crate) id: String,

    /// The workspace the session runs in.
    #[arg(long, value_name = "path")]
    pub(crate) workspace: PathBuf,

    /// The model for this run, as a person types it.
    #[arg(long, value_name = "model")]
    pub(crate) model: Option<String>,

    /// The first prompt. With none the session waits for a client.
    #[arg(long, value_name = "text")]
    pub(crate) prompt: Option<String>,

    /// Resume the session `--id` names instead of starting it; the
    /// workspace is the one its log recorded (`docs/invocation.md`, "The
    /// hub").
    #[arg(long, conflicts_with = "prompt")]
    pub(crate) resume: bool,

    /// Run the session in a new worktree (`docs/invocation.md`,
    /// "Isolation").
    #[arg(long, conflicts_with = "resume")]
    pub(crate) worktree: bool,
}

#[derive(Debug, clap::Args)]
pub(crate) struct AskArgs {
    /// The model for this run, as a person types it.
    #[arg(long, value_name = "model")]
    pub(crate) model: Option<String>,

    /// Resume the session: sends the prompt to an existing session instead
    /// of starting a new one (`docs/invocation.md`, "Lifecycle").
    #[arg(long, value_name = "id")]
    pub(crate) resume: Option<String>,

    /// Run the session in a new worktree (`docs/invocation.md`,
    /// "Isolation").
    #[arg(long, conflicts_with = "resume")]
    pub(crate) worktree: bool,

    /// The prompt. A final `-` reads stdin.
    #[arg(value_name = "prompt", num_args = 0..)]
    pub(crate) prompt: Vec<String>,
}

/// Parses the process arguments.
pub(crate) fn parse() -> Invocation {
    parse_from(std::env::args_os())
}

/// A session id: `s_` followed by exactly 16 lowercase hex digits, the
/// shape `doors::mint("s_")` makes. Anything else cannot name a session
/// directory, so it is a usage error before anything is read or created.
fn parse_session_id(text: &str) -> Result<String, String> {
    let hex = text.strip_prefix("s_").unwrap_or("");
    let valid = hex.len() == 16
        && hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    if valid {
        Ok(text.to_owned())
    } else {
        Err("a session id is `s_` followed by 16 lowercase hex digits".to_owned())
    }
}

/// `fiber <version>` or `fiber <version> (<commit>)`, with the trailing
/// newline clap prints.
pub(crate) fn version_line() -> String {
    command().render_version()
}

/// The text after `fiber `: the package version, and the commit when one
/// was recorded. An empty commit is left out, so the line never contains
/// an empty `()`.
fn version_text(version: &str, commit: Option<&str>) -> String {
    match commit.filter(|commit| !commit.is_empty()) {
        Some(commit) => format!("{version} ({commit})"),
        None => version.to_owned(),
    }
}

/// Clap stores a `&'static str`. The commit is known only after the build
/// script runs, so the text is built once rather than written as a literal.
fn compiled_version() -> &'static str {
    static TEXT: LazyLock<String> =
        LazyLock::new(|| version_text(env!("CARGO_PKG_VERSION"), option_env!("FIBER_COMMIT")));
    TEXT.as_str()
}

fn parse_from(args: impl IntoIterator<Item = impl Into<OsString>>) -> Invocation {
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    // A usage error for `ask` or the internal session command fails
    // before any session, through the pre-session exit both share.
    let ask = args
        .get(1)
        .is_some_and(|arg| arg == "ask" || arg == "session");
    match command().try_get_matches_from(&args) {
        Ok(matches) => match Cli::from_arg_matches(&matches) {
            // Clap consumes the argv delimiter `--`, so the raw slice
            // after the subcommand restores it: `fiber grep -- -needle`
            // keeps `--` before the pattern, for the built-in and the
            // fallback alike.
            Ok(cli) if matches!(cli.command, Some(Commands::Grep { .. })) => {
                Invocation::Run(Some(Commands::Grep {
                    args: passthrough(&args, "grep"),
                }))
            }
            Ok(cli) if matches!(cli.command, Some(Commands::Find { .. })) => {
                Invocation::Run(Some(Commands::Find {
                    args: passthrough(&args, "find"),
                }))
            }
            Ok(cli) if matches!(cli.command, Some(Commands::Image { .. })) => {
                Invocation::Run(Some(Commands::Image {
                    args: passthrough(&args, "image"),
                }))
            }
            // `--resume` with an empty value names no session: a usage
            // error before `resolve` runs, which itself matches none.
            Ok(cli) => match cli.command {
                Some(Commands::Ask(args)) if args.resume.as_deref() == Some("") => {
                    Invocation::Usage {
                        ask,
                        sentence: "The argument '--resume <id>' requires a session id but \
                                 none was given. Run `fiber --help` for usage."
                            .to_owned(),
                    }
                }
                command => Invocation::Run(command),
            },
            Err(error) => usage(error, ask),
        },
        Err(error) => {
            let kind = error.kind();
            if kind == clap::error::ErrorKind::DisplayHelp
                || kind == clap::error::ErrorKind::DisplayVersion
            {
                Invocation::Print(error)
            } else {
                usage(error, ask)
            }
        }
    }
}

/// Everything after the hidden search subcommand's name, verbatim: the
/// subcommand is the first `name` past the binary, so a pattern of its
/// own name still reads as an operand.
fn passthrough(args: &[OsString], name: &str) -> Vec<OsString> {
    let delimiter = args
        .iter()
        .skip(1)
        .skip_while(|arg| arg.as_encoded_bytes() != name.as_bytes())
        .skip(1);
    delimiter.cloned().collect()
}

pub(crate) fn command() -> clap::Command {
    // The derive marks a `bool` required before `ArgAction::Version` is
    // applied, so the flag is added here rather than as a field.
    Cli::command()
        .version(compiled_version())
        .help_template(MENU)
        .arg(
            clap::Arg::new("version-flag")
                .short('v')
                .long("version")
                .action(ArgAction::Version)
                .help("Print the version"),
        )
}

/// The menu, or one command's help, found a word per level. An unknown
/// name is the same sentence as invoking those words directly.
pub(crate) fn render_help<S: AsRef<str>>(words: &[S]) -> Result<String, String> {
    let mut cmd = command();
    if words.is_empty() {
        return Ok(cmd.render_help().to_string());
    }
    // The usage line names the parent (`fiber ask`) only after the parent
    // builds bin names, which `fiber ask --help` does while parsing.
    cmd.build();
    let mut sub = &mut cmd;
    for (depth, word) in words.iter().enumerate() {
        let Some(next) = sub.find_subcommand_mut(word.as_ref()) else {
            let typed = words.iter().take(depth + 1).map(AsRef::as_ref);
            let error = match command().try_get_matches_from(["fiber"].into_iter().chain(typed)) {
                Err(error) => error,
                Ok(_) => {
                    let name = word.as_ref();
                    return Err(format!("Unrecognized subcommand '{name}'.{HELP_SUFFIX}"));
                }
            };
            return Err(usage_sentence(&error));
        };
        sub = next;
    }
    Ok(sub.render_help().to_string())
}

/// `-` is only the last positional. Anything else is one prompt too many.
pub(crate) fn ask_parts(positionals: &[String]) -> Result<(Option<String>, bool), String> {
    match positionals {
        [] => Ok((None, false)),
        [only] if only == "-" => Ok((None, true)),
        [prompt] => Ok((Some(prompt.clone()), false)),
        [prompt, dash] if dash == "-" && prompt != "-" => Ok((Some(prompt.clone()), true)),
        _ => Err(ASK_SHAPE.to_owned()),
    }
}

fn usage(error: clap::Error, ask: bool) -> Invocation {
    Invocation::Usage {
        ask,
        sentence: usage_sentence(&error),
    }
}

/// Clap's first paragraph, then its suggestion when it has one, then the
/// usage suffix. Tips such as "use '--'" are dropped.
fn usage_sentence(error: &clap::Error) -> String {
    let rendered = error.to_string();
    let paragraph = rendered
        .lines()
        .map(str::trim)
        .take_while(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let paragraph = paragraph.strip_prefix("error: ").unwrap_or(&paragraph);
    let mut sentence = capitalize(paragraph);
    if sentence.ends_with('.') {
        sentence.pop();
    }
    match suggestion(error) {
        Some(name) => sentence.push_str(&format!("; did you mean '{name}'?")),
        None => sentence.push('.'),
    }
    sentence.push_str(HELP_SUFFIX);
    sentence
}

fn suggestion(error: &clap::Error) -> Option<String> {
    if let Some(value) = error.get(ContextKind::SuggestedArg) {
        return one_suggestion(value);
    }
    if let Some(value) = error.get(ContextKind::SuggestedValue) {
        return one_suggestion(value);
    }
    // Clap stores subcommand candidates worst-first. The nearest is last.
    // A typed prefix of a candidate (`instal` → `install`) wins over that
    // order when several pass the cutoff (`list` does too).
    let names = error
        .get(ContextKind::SuggestedSubcommand)
        .map(all_suggestions)
        .filter(|names| !names.is_empty())?;
    let invalid = error
        .get(ContextKind::InvalidSubcommand)
        .and_then(one_suggestion)
        .unwrap_or_default();
    names
        .iter()
        .rev()
        .find(|name| name.starts_with(&invalid))
        .or_else(|| names.last())
        .cloned()
}

#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "clap::error::ContextValue is non_exhaustive"
)]
fn all_suggestions(value: &ContextValue) -> Vec<String> {
    let texts = match value {
        ContextValue::String(text) => vec![text.clone()],
        ContextValue::Strings(texts) => texts.clone(),
        _ => Vec::new(),
    };
    texts.into_iter().filter(|text| !text.is_empty()).collect()
}

fn one_suggestion(value: &ContextValue) -> Option<String> {
    all_suggestions(value).into_iter().next()
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut sentence = first.to_uppercase().collect::<String>();
    sentence.push_str(chars.as_str());
    sentence
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
