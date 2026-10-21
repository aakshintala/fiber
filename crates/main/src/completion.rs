//! `fiber completion <shell>` (`docs/invocation.md`, "Commands and flags"): a
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

impl Shell {
    /// The parser's spelling of the shell, the one name every call site uses.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        }
    }
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
        Shell::Fish => format!("{text}{}complete -c fiber -f\n", fish_deep_flags(&cmd)),
    }
}

// clap_complete's fish generator stops at two command levels. Its level-two
// rules still match deeper commands, so their flags must not be repeated.
fn fish_deep_flags(cmd: &Command) -> String {
    fn quoted(text: &str) -> String {
        format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
    }
    fn walk(
        cmd: &Command,
        path: &[&str],
        inherited: &std::collections::BTreeSet<String>,
        out: &mut String,
    ) {
        let mut offered = inherited.clone();
        if path.len() >= 2 {
            for arg in cmd.get_arguments() {
                let mut flags = Vec::new();
                for short in arg.get_short_and_visible_aliases().into_iter().flatten() {
                    flags.push(format!("-s {short}"));
                }
                for long in arg.get_long_and_visible_aliases().into_iter().flatten() {
                    flags.push(format!("-l {long}"));
                }
                let new: Vec<_> = flags
                    .into_iter()
                    .filter(|flag| offered.insert(flag.clone()))
                    .collect();
                if path.len() >= 3 && !new.is_empty() {
                    let mut conditions = Vec::new();
                    for (index, part) in path.iter().enumerate() {
                        let helper = if index == 0 {
                            "__fish_fiber_using_subcommand"
                        } else {
                            "__fish_seen_subcommand_from"
                        };
                        conditions.push(format!("{helper} {}", quoted(part)));
                    }
                    out.push_str(&format!(
                        "complete -c fiber -n \"{}\" {}",
                        conditions.join("; and "),
                        new.join(" ")
                    ));
                    if arg.get_action().takes_values() {
                        out.push_str(" -r -f");
                    }
                    out.push('\n');
                }
            }
        }
        for sub in cmd.get_subcommands() {
            for name in sub.get_name_and_visible_aliases() {
                let mut path = path.to_vec();
                path.push(name);
                walk(sub, &path, &offered, out);
            }
        }
    }
    let mut out = String::new();
    walk(cmd, &[], &std::collections::BTreeSet::new(), &mut out);
    out
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
