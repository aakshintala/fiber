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
  fiber [--model <model>] [-c <key>=<value>]...                                                                   Open the terminal
  ask [--model <model>] [-c <key>=<value>]... [--resume <id> [--credential <label>]] [--worktree] [<prompt>] [-]  Run one session of one turn; its events go to stdout
  resume [<id>] [--credential <label>]                                                                            Open a session in the terminal, or home at the session list
  continue                                                                                                        Open the most recent session in this project
  sessions [--all] [--json]                                                                                       List sessions: id, state, name, what it waits on, spend
  sessions search [--all] [--json] <text>                                                                         Search the logs of past and running sessions for the text
  sessions delete [--cascade] [--yes] <id>                                                                        Delete a session, and with --cascade the sessions that continue it
  sessions export <id> [<path>]                                                                                   Write the session's log and its artifacts to <path>
  sessions prune [--older-than <duration>] [--cascade] [--force] [--dry-run] [--yes]                              Delete old sessions, worktrees and diagnostic logs
  models [<search>] [--json]                                                                                      List the models the installed providers serve

Fiber itself:
  setup [--provider <name>]... [--model <model>] [--thinking <level>] [--import <claude-code | codex>]... [--completions <shell> | --no-completions] [--hub | --no-hub]  Ask the questions a new install needs, applying each answer
  update                                    Update the Fiber binary and every installed extension together; `upgrade` runs the same command, with `--channel stable|nightly`
  approve [--yes]                           Show what this repository ships and approve it
  login [<name>] [--as <label>] [--device]   Store a provider's key or an extension's secret
  logout <provider> [--as <label> | --all]  Delete a provider's stored key
  doctor                                    Says whether a session can start, and how to fix it when it cannot
  stop [--hub <name>]                       Shut down every running session, then the hub
  feedback [<prompt>] [-] [--session <id>]  Write a feedback report and print the issue link.
  completion <shell>                        Print a completion script for bash, zsh or fish
  help [<command>]                          Print this menu, or a command's help
  version                                   Print the version

Extensions:
  extension install [--project] <name or path>  Install an extension and its dependencies
  extension update [<name>]         Update one extension, or every installed extension, to its newest tag
  extension remove <name>           Remove an extension, the dependencies nothing else uses, and their data
  extension reinstall <name>        Remove an extension and install it again from its recorded source and commit
  extension list                    List installed extensions: name, version and commit
  extension test [<path>]           Run an extension's test cases against the scripted provider

MCP servers:
  mcp add [--project | --repo] <name> <url>                                                      Declare a remote server
  mcp add [--project | --repo] <name> [-e KEY=value]... [--secret KEY]... -- <command> [args]...  Declare a stdio server
  mcp remove [--project | --repo] <name>                                                         Remove a server's declaration
  mcp list                                                                                       List declared servers, their layers and approvals
  mcp login <server>                                                                             Log in to a server that needs OAuth
  mcp logout <server>                                                                            Delete a server's stored token

Configuration:
  config get <key>                              Print the effective value and the layer it came from
  config set [--project | --repo] <key> <value>  Write one key in one layer's file
  import <claude-code | codex>                   Copy another harness's configuration into Fiber home, once

The hub:
  hub add [--default] <name> <address>  Add a hub to this client's list
  hub remove <name>                    Remove a hub from this client's list
  hub install [--port <port>]          Register the hub as a login service
  hub uninstall                        Remove the hub's login service; running sessions carry on
  hub status [--json]                  Print the hub's state: running, version, port, clients, devices, installed
  hub refresh                          Rebuild the hub's environment for the sessions it starts from now on
  hub pair <device> [--address <url>]  Print a pairing code for a device of that name
  hub token list                       List paired devices: name, when paired, last connection
  hub token revoke <device>            Revoke a device's token and close its live connections

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

pub(crate) const FEEDBACK_SHAPE: &str = "`fiber feedback` takes one description, then an optional `-`; quote the description. Run `fiber --help` for usage.";

/// What `fiber` was asked to do, or the parse error.
#[allow(
    clippy::large_enum_variant,
    reason = "one invocation is parsed and matched once per process; boxing would allocate for no path that copies it"
)]
#[derive(Debug)]
pub(crate) enum Invocation {
    /// Help or version, printed with clap's printer.
    Print(clap::Error),
    /// A command, or no arguments. The top-level flags are bare
    /// `fiber`'s per-run layer; with any subcommand they are a usage
    /// error, through clap's `args_conflicts_with_subcommands`, so a
    /// command never carries them.
    Run(Option<Commands>, TopLevel),
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

/// Bare `fiber`'s `--model`, `--hub` and `-c`: the terminal's per-run layer,
/// as `-c` reads it (`docs/configuration.md`, "Layers").
#[derive(Debug, Default)]
pub(crate) struct TopLevel {
    /// `--model <model>`, as `-c model=<model>` reads it.
    pub(crate) model: Option<String>,
    /// `--hub <name>`: the listed hub the terminal opens on.
    pub(crate) hub: Option<String>,
    /// Each `-c <key>=<value>`, in order.
    pub(crate) overrides: Vec<String>,
}

#[derive(Parser)]
#[command(
    name = "fiber",
    about = "Fiber, a coding agent.",
    disable_version_flag = true,
    disable_help_subcommand = true,
    args_conflicts_with_subcommands = true
)]
struct Cli {
    /// The model for this terminal run, as a person types it: bare
    /// `fiber` only, carried on each `start` in `overrides`
    /// (`docs/invocation.md`, "Sessions").
    #[arg(long, value_name = "model")]
    model: Option<String>,

    /// Use a hub from this client's list: bare `fiber` only, like
    /// `--model`.
    #[arg(long, value_name = "name")]
    hub: Option<String>,

    /// One configuration key for this terminal run, `<key>=<value>`:
    /// bare `fiber` only, carried on each `start` in `overrides`. May
    /// repeat; a later one wins, and every one wins over `--model`
    /// (`docs/configuration.md`, "Layers").
    #[arg(short = 'c', value_name = "key>=<value", value_parser = parse_override)]
    overrides: Vec<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

/// The release channel `fiber update` installs from. The names are
/// lowercase and matched exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ChannelArg {
    // The variants carry no doc comments: clap would show them as each
    // value's help, and `update -h` and `--help` would differ.
    Stable,
    Nightly,
}

/// The hidden `--base-url` test seam `update` and `release-install`
/// share: both release roots point at it, so the binary tests serve one
/// fixture server as both. Hidden and free to change.
#[derive(Debug, clap::Args)]
pub(crate) struct TestBaseUrl {
    /// Where releases are published, for tests.
    #[arg(long = "base-url", value_name = "url", hide = true)]
    pub(crate) base_url: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Commands {
    /// Update the Fiber binary and every installed extension together;
    /// `upgrade` runs the same command
    #[command(visible_alias = "upgrade")]
    Update {
        /// The release channel: `stable` or `nightly`. Absent reads
        /// `FIBER_CHANNEL`; an empty or unset variable means stable.
        #[arg(long, value_name = "channel")]
        channel: Option<ChannelArg>,
        #[command(flatten)]
        test_base_url: TestBaseUrl,
    },
    /// Run one session of one turn; its events go to stdout
    Ask(AskArgs),
    /// Open a session in the terminal, or home at the session list
    Resume {
        /// The session: a full id or a unique prefix of one. With none,
        /// home opens at the session list.
        #[arg(value_name = "id", value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: Option<String>,
        /// The credential label to switch the session to
        /// (`docs/model-routing.md`, "Which credential a session uses").
        #[arg(long, value_name = "label", requires = "id")]
        credential: Option<String>,
        /// Use a hub from this client's list.
        #[arg(long, value_name = "name")]
        hub: Option<String>,
    },
    /// Open the most recent session in this project
    Continue,
    /// List sessions, or delete, export or prune them
    Sessions(SessionsArgs),
    /// List the models the installed providers serve
    Models(ModelsArgs),
    /// Manage extensions
    #[command(subcommand, arg_required_else_help = false)]
    Extension(ExtensionCommands),
    /// Manage MCP servers
    #[command(subcommand, arg_required_else_help = false)]
    Mcp(McpCommands),
    /// Show what this repository ships and approve it
    Approve(ApproveArgs),
    /// Print a configuration value, or write one
    #[command(subcommand, arg_required_else_help = false)]
    Config(ConfigCommands),
    /// Copy another harness's configuration into Fiber home, once
    Import {
        /// The harness to import from: `claude-code` or `codex`.
        #[arg(value_name = "source")]
        source: ImportSource,
    },
    /// Store a provider's key or an extension's secret
    Login(LoginArgs),
    /// Ask the questions a new install needs, applying each answer
    Setup(SetupArgs),
    /// Delete a provider's stored key
    Logout(LogoutArgs),
    /// Say whether a session can start, and how to fix it when it cannot
    Doctor,
    /// Shut down every running session, then the hub
    Stop {
        /// Use a hub from this client's list.
        #[arg(long, value_name = "name")]
        hub: Option<String>,
    },
    /// Write a feedback report and print the issue link
    Feedback(FeedbackArgs),
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
        #[command(flatten)]
        test_base_url: TestBaseUrl,
    },
    /// Manage this client's hub list, the hub's service and paired devices
    #[command(subcommand, arg_required_else_help = false)]
    Hub(HubCommands),
}

/// The subcommands that host a session in their process, by CLI name: the
/// internal session command, `fiber ask`, and the case runner's child
/// (`docs/permissions.md`, "Sessions on the sockets"). The list lives
/// beside the enum so a new in-process host is added in the same place.
#[cfg(test)]
impl Commands {
    pub(crate) fn session_host_commands() -> &'static [&'static str] {
        &["session", "ask", "extension-case"]
    }
}

#[derive(Debug, Subcommand)]
#[command(disable_help_subcommand = true)]
pub(crate) enum HubCommands {
    /// Add a hub to this client's list
    Add {
        /// Use this hub when no --hub is given.
        #[arg(long)]
        default: bool,
        /// The hub's name in this client's list.
        #[arg(value_name = "name")]
        name: String,
        /// A ws://, wss:// or unix: address.
        #[arg(value_name = "address")]
        address: String,
    },
    /// Remove a hub from this client's list
    Remove {
        /// The hub's name in this client's list.
        #[arg(value_name = "name")]
        name: String,
    },
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
    /// Rebuild the hub's environment for the sessions it starts from now on
    Refresh {
        /// Use a hub from this client's list.
        #[arg(long, value_name = "name")]
        hub: Option<String>,
    },
    /// Print a pairing code for a device of that name
    Pair {
        /// Use a hub from this client's list.
        #[arg(long, value_name = "name")]
        hub: Option<String>,
        /// The device to pair.
        #[arg(value_name = "device")]
        device: String,
        /// The websocket address carried in the QR code.
        #[arg(long, value_name = "url", value_parser = ::cli::Address::parse)]
        address: Option<::cli::Address>,
    },
    /// Manage paired devices
    #[command(subcommand)]
    Token(TokenCommands),
    /// The internal hub command: hidden and free to change.
    #[command(hide = true)]
    Serve {
        /// Run as the login service: wait for the `run/` lock and never
        /// exit for being idle.
        #[arg(long, hide = true)]
        installed: bool,
    },
}

#[derive(Debug, Subcommand)]
#[command(disable_help_subcommand = true, arg_required_else_help = false)]
pub(crate) enum TokenCommands {
    /// List paired devices: name, when paired, last connection
    List {
        /// Use a hub from this client's list.
        #[arg(long, value_name = "name")]
        hub: Option<String>,
    },
    /// Revoke a device's token and close its live connections
    Revoke {
        /// Use a hub from this client's list.
        #[arg(long, value_name = "name")]
        hub: Option<String>,
        /// The device to revoke.
        #[arg(value_name = "device")]
        device: String,
    },
}

/// `fiber sessions`: the list, or one of its subcommands. A flag with a
/// subcommand is a usage error.
#[derive(Debug, clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct SessionsArgs {
    /// Use a hub from this client's list.
    #[arg(long, value_name = "name")]
    pub(crate) hub: Option<String>,
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
    /// Search the logs of past and running sessions for the text
    Search {
        /// Search every project, not only this repository's.
        #[arg(long)]
        all: bool,
        /// Print each hit as one JSON object per line.
        #[arg(long)]
        json: bool,
        /// The text to find, as a literal.
        #[arg(value_name = "text", value_parser = clap::builder::NonEmptyStringValueParser::new())]
        text: String,
    },
}

#[derive(Debug, Subcommand)]
#[command(disable_help_subcommand = true)]
pub(crate) enum ExtensionCommands {
    /// Install an extension and its dependencies
    Install {
        /// Enable the extension only for the current project.
        #[arg(long)]
        project: bool,
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
    /// Remove an extension and install it again from its recorded source and commit
    Reinstall {
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
pub(crate) enum McpCommands {
    /// Declare a remote or stdio server
    Add(McpAddArgs),
    /// Remove a server's declaration
    Remove(McpRemoveArgs),
    /// List declared servers, their layers and approvals
    List,
    /// Log in to a server that needs OAuth
    Login {
        /// The server's name.
        #[arg(value_name = "server", value_parser = clap::builder::NonEmptyStringValueParser::new())]
        server: String,
    },
    /// Delete a server's stored token
    Logout {
        /// The server's name.
        #[arg(value_name = "server", value_parser = clap::builder::NonEmptyStringValueParser::new())]
        server: String,
    },
}

/// `fiber mcp add`: one transport, and the layer to write. Its `Debug`
/// prints names and the layer only, never values: the url prints
/// redacted (never its query or userinfo), `-e` prints its keys, and the
/// command prints redacted (`docs/code-quality.md`, "Errors").
#[derive(clap::Args)]
pub(crate) struct McpAddArgs {
    /// Write the per-project file in Fiber home.
    #[arg(long, conflicts_with = "repo")]
    pub(crate) project: bool,
    /// Write the repository's `.fiber/config.json`.
    #[arg(long, conflicts_with = "project")]
    pub(crate) repo: bool,
    /// The server's name.
    #[arg(value_name = "name", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub(crate) name: String,
    /// The server's URL: a remote server. With none, `-- <command>` declares
    /// a stdio server.
    #[arg(value_name = "url")]
    pub(crate) url: Option<String>,
    /// A clear variable, `KEY=value`; only with `-- <command>`. May repeat.
    #[arg(short = 'e', value_name = "KEY=value")]
    pub(crate) env: Vec<String>,
    /// A secret variable's name, stored beside the configuration; only with
    /// `-- <command>`. May repeat.
    #[arg(long = "secret", value_name = "KEY")]
    pub(crate) secret: Vec<String>,
    /// The program and its arguments: a stdio server.
    #[arg(last = true, value_name = "command")]
    pub(crate) command: Vec<String>,
}

impl std::fmt::Debug for McpAddArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let layer = if self.repo {
            "repository"
        } else if self.project {
            "project"
        } else {
            "global"
        };
        let env: Vec<&str> = self
            .env
            .iter()
            .map(|raw| raw.split('=').next().unwrap_or(raw))
            .collect();
        let command: Vec<&str> = self.command.iter().map(|_| "redacted").collect();
        f.debug_struct("McpAddArgs")
            .field("layer", &layer)
            .field("name", &self.name)
            .field("url", &self.url.as_ref().map(|_| "redacted"))
            .field("env", &env)
            .field("secret", &self.secret)
            .field("command", &command)
            .finish()
    }
}

/// `fiber mcp remove`: the declaration and the layer to remove it from.
#[derive(Debug, clap::Args)]
pub(crate) struct McpRemoveArgs {
    /// Remove from the per-project file in Fiber home.
    #[arg(long, conflicts_with = "repo")]
    pub(crate) project: bool,
    /// Remove from the repository's `.fiber/config.json`.
    #[arg(long, conflicts_with = "project")]
    pub(crate) repo: bool,
    /// The server's name.
    #[arg(value_name = "name", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub(crate) name: String,
}

/// The harness `fiber import` copies configuration from. The names are
/// lowercase and matched exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ImportSource {
    #[value(name = "claude-code")]
    ClaudeCode,
    Codex,
}

impl From<ImportSource> for config::ImportSource {
    fn from(source: ImportSource) -> Self {
        match source {
            ImportSource::ClaudeCode => Self::ClaudeCode,
            ImportSource::Codex => Self::Codex,
        }
    }
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
    /// Log in with a device code instead of the browser, for a provider
    /// that logs in by browser.
    #[arg(long)]
    pub(crate) device: bool,
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

/// `fiber setup`: one flag per question. A flag answers its question and
/// the rest are still asked; without a terminal nothing is asked and each
/// flag is applied as its command does it (`docs/invocation.md",
/// "Setting up").
#[derive(Debug, clap::Args)]
pub(crate) struct SetupArgs {
    /// Sign in to this provider, as `fiber login <name>` does. May repeat.
    #[arg(long = "provider", value_name = "name")]
    pub(crate) provider: Vec<String>,
    /// The default model, written to the global `config.json`.
    #[arg(long, value_name = "model")]
    pub(crate) model: Option<String>,
    /// The default thinking level for the model, written beside it.
    #[arg(long, value_name = "level")]
    pub(crate) thinking: Option<String>,
    /// Import this source's configuration, as `fiber import` does. May repeat.
    #[arg(long = "import", value_name = "claude-code | codex")]
    pub(crate) import: Vec<ImportSource>,
    /// Install completions for this shell in its startup file.
    #[arg(long, value_name = "shell", conflicts_with = "no_completions")]
    pub(crate) completions: Option<crate::completion::Shell>,
    /// Leave the shell's startup file alone.
    #[arg(long)]
    pub(crate) no_completions: bool,
    /// Register the hub as a login service, as `fiber hub install` does.
    #[arg(long, conflicts_with = "no_hub")]
    pub(crate) hub: bool,
    /// Leave the hub's login service alone.
    #[arg(long)]
    pub(crate) no_hub: bool,
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

    /// One configuration key for this run, `<key>=<value>`: the value is
    /// JSON, or a bare string when it does not parse as JSON. May repeat;
    /// a later one wins, and every one wins over `--model`.
    #[arg(short = 'c', value_name = "key>=<value", value_parser = parse_override)]
    pub(crate) overrides: Vec<String>,

    /// The first prompt. With none the session waits for a client.
    #[arg(long, value_name = "text")]
    pub(crate) prompt: Option<String>,

    /// Resume the session `--id` names instead of starting it; the
    /// workspace is the launch directory the hub selected from the
    /// session's log (`docs/invocation.md`, "The hub").
    #[arg(long)]
    pub(crate) resume: bool,

    /// The credential label the resumed session starts on: hidden and
    /// free to change, passed by the hub that resumes an exited session
    /// (`docs/model-routing.md`, "Which credential a session uses").
    /// Only a resume carries it; `parse_from` refuses it without
    /// `--resume`, since clap's `requires` does not cover every
    /// conflicting combination.
    #[arg(long, value_name = "label", hide = true)]
    pub(crate) credential: Option<String>,

    /// Run the session in a new worktree (`docs/invocation.md`,
    /// "Isolation").
    #[arg(long, conflicts_with = "resume")]
    pub(crate) worktree: bool,

    /// Continue the session this rewind names as the session its `rewound`
    /// line points at: hidden and free to change, passed by the hub that
    /// starts a rewind's new session (`docs/invocation.md`, "`rewind`
    /// starts a new session process").
    #[arg(
        long = "rewound-from",
        value_name = "session_id",
        value_parser = parse_session_id,
        hide = true,
        conflicts_with_all = ["resume", "prompt", "model", "overrides", "worktree", "parent", "delegate_id", "forked_from", "forked_seq", "continues"]
    )]
    pub(crate) rewound_from: Option<String>,

    /// Continue in the worktree of the session `start` names: hidden
    /// and free to change, passed by the hub that starts a session with
    /// `continues` (`docs/invocation.md`, "Isolation").
    #[arg(
        long = "continues",
        value_name = "session_id",
        value_parser = parse_session_id,
        hide = true,
        conflicts_with_all = ["resume", "rewound_from", "parent", "forked_from", "forked_seq"]
    )]
    pub(crate) continues: Option<String>,
    /// Continue the session this fork names from its point: hidden and
    /// free to change, passed by the parent that starts a fork's new
    /// session. Requires its sequence, the parent, the delegate job and
    /// the prompt, and conflicts with every other way to start.
    #[arg(
        long = "forked-from",
        value_name = "session_id",
        value_parser = parse_session_id,
        hide = true,
        requires = "forked_seq",
        requires = "parent",
        requires = "delegate_id",
        requires = "prompt",
        conflicts_with_all = ["resume", "model", "overrides", "worktree", "rewound_from", "continues"]
    )]
    pub(crate) forked_from: Option<String>,

    /// The point in the forked session, as a `seq`: hidden and free to
    /// change, alongside `--forked-from`.
    #[arg(
        long = "forked-seq",
        value_name = "seq",
        hide = true,
        requires = "forked_from",
        requires = "parent",
        requires = "delegate_id",
        requires = "prompt",
        conflicts_with_all = ["resume", "model", "overrides", "worktree", "rewound_from", "continues"]
    )]
    pub(crate) forked_seq: Option<u64>,

    /// The parent session: its id. A Fiber delegate records it and its job
    /// id on `session_started` (`docs/delegates.md`, "Events").
    #[arg(
        long,
        value_name = "session_id",
        value_parser = parse_session_id,
        requires = "delegate_id",
        requires = "prompt",
        conflicts_with = "worktree"
    )]
    pub(crate) parent: Option<String>,

    /// The delegate's job id in its parent session.
    #[arg(
        long = "delegate-id",
        value_name = "job_id",
        value_parser = parse_job_id,
        requires = "parent"
    )]
    pub(crate) delegate_id: Option<String>,

    /// This session's depth in the delegate tree, passed by its parent.
    #[arg(long, value_name = "depth", requires = "parent", hide = true)]
    pub(crate) depth: Option<u32>,

    /// Passed by the hub when the session's environment came from the
    /// login-shell capture (`docs/invocation.md`, "A session's
    /// environment").
    #[arg(long = "login-shell", hide = true)]
    pub(crate) login_shell: bool,

    /// The session is driven by a program, never by a person: its
    /// escalations stay blocks (`docs/permissions.md`, "Supervised
    /// sessions"). Hidden and free to change, passed by the hub that
    /// starts a supervised session; fixed at `start`, so never with a
    /// resume or a delegate's parent.
    #[arg(
        long,
        hide = true,
        conflicts_with_all = ["resume", "rewound_from", "parent", "forked_from", "forked_seq"]
    )]
    pub(crate) supervised: bool,
}

impl From<&SessionArgs> for contract::session_argv::SessionArgv {
    fn from(args: &SessionArgs) -> Self {
        Self {
            id: contract::SessionId(args.id.clone()),
            workspace: args.workspace.clone(),
            model: args.model.clone(),
            overrides: args.overrides.clone(),
            prompt: args.prompt.clone(),
            resume: args.resume,
            credential: args.credential.clone(),
            worktree: args.worktree,
            rewound_from: args.rewound_from.clone().map(contract::SessionId),
            continues: args.continues.clone().map(contract::SessionId),
            forked_from: args.forked_from.clone().map(contract::SessionId),
            forked_seq: args.forked_seq,
            parent: args.parent.clone().map(contract::SessionId),
            delegate_id: args.delegate_id.clone().map(contract::JobId),
            depth: args.depth,
            login_shell: args.login_shell,
            supervised: args.supervised,
        }
    }
}

#[derive(Debug, clap::Args)]
pub(crate) struct AskArgs {
    /// The model for this run, as a person types it.
    #[arg(long, value_name = "model")]
    pub(crate) model: Option<String>,

    /// One configuration key for this run, `<key>=<value>`: the value is
    /// JSON, or a bare string when it does not parse as JSON. May repeat;
    /// a later one wins, and every one wins over `--model`.
    #[arg(short = 'c', value_name = "key>=<value", value_parser = parse_override)]
    pub(crate) overrides: Vec<String>,

    /// Resume the session: sends the prompt to an existing session instead
    /// of starting a new one (`docs/invocation.md`, "Lifecycle").
    #[arg(long, value_name = "id")]
    pub(crate) resume: Option<String>,

    /// On a resume, the credential label to switch the session to.
    #[arg(long, value_name = "label", requires = "resume")]
    pub(crate) credential: Option<String>,

    /// Run the session in a new worktree (`docs/invocation.md`,
    /// "Isolation").
    #[arg(long, conflicts_with = "resume")]
    pub(crate) worktree: bool,

    /// The prompt. A final `-` reads stdin.
    #[arg(value_name = "prompt", num_args = 0..)]
    pub(crate) prompt: Vec<String>,
}

/// The `feedback` command: its description, resolved like `ask`'s prompt.
#[derive(Debug, clap::Args)]
pub(crate) struct FeedbackArgs {
    /// The session whose diagnostics the report carries: any session in
    /// Fiber home. Without it, the newest session in this project.
    #[arg(long, value_name = "session_id", value_parser = parse_session_id)]
    pub(crate) session: Option<String>,

    /// The description. A final `-` reads stdin.
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

/// One `-c` override: a dotted key and a value, as in
/// `-c handoff.tokens=200000`. The parser checks only for the `=`; the
/// configuration load checks the key path and the value's type.
fn parse_override(text: &str) -> Result<String, String> {
    if text.contains('=') {
        Ok(text.to_owned())
    } else {
        Err("expected a dotted key and a value, as in `-c handoff.tokens=200000`".to_owned())
    }
}

/// A delegate id: `j_` followed by exactly 16 lowercase hex digits, the
/// shape a job id has. Anything else cannot name a job, so it is a usage
/// error before anything is read or created.
fn parse_job_id(text: &str) -> Result<String, String> {
    let hex = text.strip_prefix("j_").unwrap_or("");
    let valid = hex.len() == 16
        && hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    if valid {
        Ok(text.to_owned())
    } else {
        Err("a job id is `j_` followed by 16 lowercase hex digits".to_owned())
    }
}

/// `fiber <version>` or `fiber <version> (<commit>)`, with the trailing
/// newline clap prints.
pub(crate) fn version_line() -> String {
    command().render_version()
}

/// The version text without the `fiber ` prefix or trailing newline, for
/// reports that print the command name themselves.
pub(crate) fn bare_version() -> String {
    version_text(env!("CARGO_PKG_VERSION"), option_env!("FIBER_COMMIT"))
}

/// The text after `fiber `: the package version, and the first 7
/// characters of the commit when one was recorded. An empty commit is
/// left out, so the line never contains an empty `()`.
fn version_text(version: &str, commit: Option<&str>) -> String {
    match commit.filter(|commit| !commit.is_empty()) {
        Some(commit) => format!("{version} ({})", commit.get(..7).unwrap_or(commit)),
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
                // No top-level flag reaches a subcommand: clap's
                // `args_conflicts_with_subcommands` refused them, so
                // the layer is empty here and below.
                Invocation::Run(
                    Some(Commands::Grep {
                        args: passthrough(&args, "grep"),
                    }),
                    TopLevel::default(),
                )
            }
            Ok(cli) if matches!(cli.command, Some(Commands::Find { .. })) => Invocation::Run(
                Some(Commands::Find {
                    args: passthrough(&args, "find"),
                }),
                TopLevel::default(),
            ),
            Ok(cli) if matches!(cli.command, Some(Commands::Image { .. })) => Invocation::Run(
                Some(Commands::Image {
                    args: passthrough(&args, "image"),
                }),
                TopLevel::default(),
            ),
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
                // `--credential` without `--resume` names no startup
                // label: the hub passes it only when it resumes an
                // exited session, never for a fresh session or a rewind.
                Some(Commands::Session(args)) if args.credential.is_some() && !args.resume => {
                    Invocation::Usage {
                        ask,
                        sentence: "The argument '--credential <label>' requires '--resume': \
                                 only a resumed session starts on a label. \
                                 Run `fiber --help` for usage."
                            .to_owned(),
                    }
                }
                // `--prompt` with `--resume` is a delegate's resume
                // only: the hub's resume never carries a prompt.
                Some(Commands::Session(args))
                    if args.resume && args.prompt.is_some() && args.parent.is_none() =>
                {
                    Invocation::Usage {
                        ask,
                        sentence: "The arguments '--resume' and '--prompt' require '--parent': \
                                 only a delegate resumes with a prompt. \
                                 Run `fiber --help` for usage."
                            .to_owned(),
                    }
                }
                command => Invocation::Run(
                    command,
                    TopLevel {
                        model: cli.model,
                        hub: cli.hub,
                        overrides: cli.overrides,
                    },
                ),
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
