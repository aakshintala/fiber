//! `fiber completion <shell>` (`docs/invocation.md`, "Fiber itself"): a
//! completion script generated from the parser's own definitions. It
//! completes the visible commands and flags, and no values.

use std::ffi::OsString;
use std::io::{self, Write};
use std::sync::LazyLock;

use clap::{Command, ValueHint};

/// The shells `fiber completion` writes a script for. The names are
/// lowercase and matched exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Shell {
    // The variants carry no doc comments: clap would show them as each
    // value's help, and `completion -h` and `--help` would differ.
    Bash,
    Zsh,
    Fish,
}

/// The parser, built once so the copy's names borrow `'static` text.
static PARSER: LazyLock<Command> = LazyLock::new(crate::cli::command);

/// The whole script for one shell. It reads no file, environment variable
/// or configuration, so the same shell always gives the same bytes.
pub(crate) fn script(shell: Shell) -> String {
    let mut cmd = visible(&PARSER);
    let mut out = Vec::new();
    let generator = match shell {
        Shell::Bash => clap_complete::Shell::Bash,
        Shell::Zsh => clap_complete::Shell::Zsh,
        Shell::Fish => clap_complete::Shell::Fish,
    };
    clap_complete::generate(generator, &mut cmd, "fiber", &mut out);
    let text = String::from_utf8_lossy(&out);
    match shell {
        // bash falls back to file names when the function offers nothing.
        Shell::Bash => text.replace(" -o bashdefault -o default", ""),
        Shell::Zsh => text.into_owned(),
        // fish offers file names wherever no other rule matches.
        Shell::Fish => format!("{text}complete -c fiber -f\n"),
    }
}

/// Writes the script to stdout in one write and exits 0. A closed stdout
/// leaves nobody to tell, as `fiber version` does.
pub(crate) fn print(shell: Shell) -> i32 {
    write!(io::stdout().lock(), "{}", script(shell)).unwrap_or(());
    0
}

/// The grammar a generator reads, without what a person never types:
/// hidden subcommands and hidden arguments are dropped at every level.
/// A value-taking argument offers no values, not even file names.
fn visible(cmd: &'static Command) -> Command {
    let mut copy = Command::new(cmd.get_name())
        .disable_help_subcommand(true)
        .disable_help_flag(cmd.is_disable_help_flag_set())
        .disable_version_flag(cmd.is_disable_version_flag_set())
        .visible_aliases(cmd.get_visible_aliases());
    if let Some(about) = cmd.get_about() {
        copy = copy.about(about.clone());
    }
    if let Some(about) = cmd.get_long_about() {
        copy = copy.long_about(about.clone());
    }
    // A version action needs a version, or clap's checks fail.
    if let Some(version) = cmd.get_version() {
        copy = copy.version(version);
    }
    if let Some(version) = cmd.get_long_version() {
        copy = copy.long_version(version);
    }
    let args = cmd
        .get_arguments()
        .filter(|arg| !arg.is_hide_set())
        .map(|arg| {
            if arg.get_action().takes_values() {
                arg.clone()
                    .value_hint(ValueHint::Other)
                    .value_parser(clap::value_parser!(OsString))
            } else {
                arg.clone()
            }
        });
    copy.args(args).subcommands(
        cmd.get_subcommands()
            .filter(|sub| !sub.is_hide_set())
            .map(visible),
    )
}

#[cfg(test)]
#[path = "completion_tests.rs"]
mod tests;
