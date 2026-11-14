//! Dynamic shell completion, isolated from the unstable engine API
//! (`docs/invocation.md`, "Commands and flags").

use std::collections::btree_map::Entry;
use std::ffi::OsStr;
use std::path::Path;

use clap::{Arg, ArgAction};
use clap_complete::env::{Bash, CompleteEnv, EnvCompleter, Fish, Zsh};
use clap_complete::{ArgValueCompleter, CompletionCandidate};

use crate::completion::{PARSER, Shell, visible_inner};

/// Handles an environment-activated completion request before normal parsing.
pub(crate) fn complete() {
    let current_dir = std::env::current_dir().ok();
    let result = CompleteEnv::with_factory(command)
        .var("FIBER_COMPLETE")
        .try_complete(std::env::args_os(), current_dir.as_deref());
    if let Ok(true) = result {
        std::process::exit(0);
    }
}

/// The visible command grammar that dynamic and static completion share.
pub(crate) fn command() -> clap::Command {
    let mut command = visible_inner(&PARSER, true);
    // The engine otherwise treats the hidden `session` word as a root
    // positional and can offer the root `--model` completer after it.
    if let Some(session) = PARSER.find_subcommand("session") {
        command = command.subcommand(visible_inner(session, true).hide(true));
    }
    let workspace = std::env::current_dir().ok();
    attach_completers(command, &[], workspace.as_deref())
}

// The engine de-duplicates short and long spellings by argument id. Keep
// help and version spellings separate in this completion-only command.
fn separate_help_and_version_flags(command: clap::Command) -> clap::Command {
    let has_help = !command.is_disable_help_flag_set();
    let has_version_short = command
        .get_arguments()
        .any(|arg| arg.get_id() == "version-flag" && arg.get_short() == Some('v'));
    let command = command.mut_args(|arg| {
        if arg.get_id() == "version-flag" {
            arg.short(None)
        } else {
            arg
        }
    });
    let command = if has_help {
        command
            .disable_help_flag(true)
            .arg(
                Arg::new("fiber-completion-help-long")
                    .long("help")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("fiber-completion-help-short")
                    .short('h')
                    .action(ArgAction::SetTrue),
            )
    } else {
        command
    };
    if has_version_short {
        command.arg(
            Arg::new("fiber-completion-version-short")
                .short('v')
                .action(ArgAction::SetTrue),
        )
    } else {
        command
    }
}

fn attach_completers(
    command: clap::Command,
    parent: &[String],
    workspace: Option<&Path>,
) -> clap::Command {
    let mut path = parent.to_vec();
    path.push(command.get_name().to_owned());
    let extension_remove = path
        .iter()
        .map(String::as_str)
        .eq(["fiber", "extension", "remove"]);
    let hidden_session = path.iter().map(String::as_str).eq(["fiber", "session"]);
    let workspace = workspace.map(Path::to_path_buf);
    let command = separate_help_and_version_flags(command);
    let command = command.mut_args(|arg| {
        if extension_remove && arg.get_id() == "name" {
            arg.add(ArgValueCompleter::new(extension_candidates))
        } else if !hidden_session && arg.get_long() == Some("model") {
            if let Some(workspace) = workspace.clone() {
                arg.add(ArgValueCompleter::new(move |current: &OsStr| {
                    model_candidates(current, &workspace)
                }))
            } else {
                arg
            }
        } else {
            arg
        }
    });
    command.mut_subcommands(|subcommand| attach_completers(subcommand, &path, workspace.as_deref()))
}

/// Registration script for one supported shell.
pub(crate) fn registration(shell: Shell) -> Result<Vec<u8>, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    registration_for_exe(shell, executable.as_os_str())
}

/// Registration script using a supplied executable path.
pub(crate) fn registration_for_exe(shell: Shell, exe: &OsStr) -> Result<Vec<u8>, String> {
    let executable = exe
        .to_str()
        .ok_or_else(|| "the executable path is not UTF-8".to_owned())?;
    let mut script = Vec::new();
    let write = match shell {
        Shell::Bash => {
            Bash.write_registration("FIBER_COMPLETE", "fiber", "fiber", executable, &mut script)
        }
        Shell::Zsh => {
            Zsh.write_registration("FIBER_COMPLETE", "fiber", "fiber", executable, &mut script)
        }
        Shell::Fish => {
            Fish.write_registration("FIBER_COMPLETE", "fiber", "fiber", executable, &mut script)
        }
    };
    write.map_err(|error| error.to_string())?;
    if shell == Shell::Bash {
        let script = String::from_utf8(script).map_err(|error| error.to_string())?;
        Ok(script.replace(" -o bashdefault", "").into_bytes())
    } else {
        Ok(script)
    }
}

fn extension_candidates(current: &OsStr) -> Vec<CompletionCandidate> {
    let Ok(home) = config::fiber_home_path_from_env() else {
        return Vec::new();
    };
    extension_candidates_in(current, &home)
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

fn extension_candidates_in(current: &OsStr, home: &Path) -> Vec<String> {
    let prefix = current.to_string_lossy();
    extensions::package_names(home)
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with(prefix.as_ref()))
        .collect()
}

fn model_candidates(current: &OsStr, workspace: &Path) -> Vec<CompletionCandidate> {
    let Ok(home) = config::fiber_home_path_from_env() else {
        return Vec::new();
    };
    model_candidates_in(current, &home, workspace)
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

fn model_candidates_in(current: &OsStr, home: &Path, workspace: &Path) -> Vec<String> {
    let Ok((mut providers, config, _)) = cli::providers_and_config(home, workspace) else {
        return Vec::new();
    };
    providers.retain_credentialed(&config);

    let mut ids = std::collections::BTreeMap::<String, (String, bool)>::new();
    for provider in providers.names() {
        for model in providers.data(provider).models {
            match ids.entry(model.id) {
                Entry::Vacant(entry) => {
                    entry.insert((provider.to_owned(), false));
                }
                Entry::Occupied(mut entry) if entry.get().0 != provider => {
                    entry.get_mut().1 = true;
                }
                Entry::Occupied(_) => {}
            }
        }
    }
    let prefix = current.to_string_lossy();
    ids.into_iter()
        .filter(|(id, (_, ambiguous))| !ambiguous && id.starts_with(prefix.as_ref()))
        .map(|(id, _)| id)
        .collect()
}

#[cfg(test)]
#[path = "dynamic_complete_tests.rs"]
mod tests;
