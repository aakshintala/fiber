//! The `fiber` command line (`docs/invocation.md`, "Commands and flags"):
//! the menu, the version, and the one-sentence form of a parse error.

use std::ffi::OsString;
use std::sync::LazyLock;

use clap::error::{ContextKind, ContextValue};
use clap::{ArgAction, CommandFactory, FromArgMatches, Parser, Subcommand};

/// The hand-grouped menu (`docs/invocation.md`, "Commands and flags").
/// Clap appends the trailing newline when it prints.
const MENU: &str = r#"Fiber, a coding agent.

Usage: fiber <command> [arguments]

Sessions:
  ask [--model <model>] [--resume <id>] [<prompt>] [-]  Run one session of one turn; its events go to stdout

Fiber itself:
  login [<provider>]  Store a provider's key
  logout <provider>   Delete a provider's stored key
  help [<command>]    Print this menu, or a command's help
  version             Print the version

Extensions:
  extension install <name or path>  Install an extension and its dependencies
  extension update [<name>]         Update one extension, or every installed extension, to its newest tag
  extension remove <name>           Remove an extension, the dependencies nothing else uses, and their data
  extension list                    List installed extensions: name, version and commit
  approve [--yes]                   Show what this repository ships and approve it

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
    /// A usage error. `ask` is whether argv's first argument is `ask`.
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
    /// Manage extensions
    #[command(subcommand, arg_required_else_help = false)]
    Extension(ExtensionCommands),
    /// Show what this repository ships and approve it
    Approve(ApproveArgs),
    /// Store a provider's key
    Login(LoginArgs),
    /// Delete a provider's stored key
    Logout(LogoutArgs),
    /// Print the version
    Version,
    /// Print this menu, or a command's help
    Help {
        /// The command to describe. Absent prints the menu.
        #[arg(value_name = "command")]
        command: Option<String>,
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
}

#[derive(Debug, clap::Args)]
pub(crate) struct ApproveArgs {
    /// Approve without asking, for a script or a machine image.
    #[arg(long)]
    pub(crate) yes: bool,
}

#[derive(Debug, clap::Args)]
pub(crate) struct LoginArgs {
    /// The installed provider to store a key for. With none, a terminal
    /// offers the installed providers.
    #[arg(value_name = "provider")]
    pub(crate) provider: Option<String>,
}

#[derive(Debug, clap::Args)]
pub(crate) struct LogoutArgs {
    /// The provider whose stored key to delete. A missing provider is a
    /// usage error `fiber` words itself, so it matches the other sentences.
    #[arg(value_name = "provider")]
    pub(crate) provider: Option<String>,
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

    /// The prompt. A final `-` reads stdin.
    #[arg(value_name = "prompt", num_args = 0..)]
    pub(crate) prompt: Vec<String>,
}

/// Parses the process arguments.
pub(crate) fn parse() -> Invocation {
    parse_from(std::env::args_os())
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
    let ask = args.get(1).is_some_and(|arg| arg == "ask");
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

fn command() -> clap::Command {
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

/// The menu, or one command's help. An unknown name is the same sentence
/// as invoking that name directly.
pub(crate) fn render_help(name: Option<&str>) -> Result<String, String> {
    let mut cmd = command();
    let Some(name) = name else {
        return Ok(cmd.render_help().to_string());
    };
    // The usage line names the parent (`fiber ask`) only after the parent
    // builds bin names, which `fiber ask --help` does while parsing.
    cmd.build();
    let Some(sub) = cmd.find_subcommand_mut(name) else {
        let error = match command().try_get_matches_from(["fiber", name]) {
            Err(error) => error,
            Ok(_) => return Err(format!("Unrecognized subcommand '{name}'.{HELP_SUFFIX}")),
        };
        return Err(usage_sentence(&error));
    };
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
