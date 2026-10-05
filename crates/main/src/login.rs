//! `fiber login` and `fiber logout` (`docs/invocation.md`, "Fiber itself";
//! `docs/configuration.md`, "Secrets"; `docs/model-routing.md`, "Logging
//! in"): a provider's key is stored in `credentials/<stored>/default`, where
//! `<stored>` is the credential the provider reads, and deleted again. No
//! key reaches stdout, stderr, `Debug` or a log: it is a [`Secret`] from the
//! moment it is read.

use std::io::{self, BufRead, IsTerminal, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::thread::{self, JoinHandle};

use config::{
    Config, ConfigError, CredentialFile, CredentialSource, ProviderData, Secret, Sources,
    credential_labels, delete_credential, read_credential, set_global_if_unset, store_credential,
};
use contract::ErrorCode;
use contract::shapes::Failure;
use doors::failure;
use extensions::Providers;
use rustix::termios::{self, LocalModes, OptionalActions, Termios};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::{Handle, Signals};

use crate::cli::{LOGOUT_SHAPE, LoginArgs, LogoutArgs};

/// The label `fiber login` stores a key under until labels arrive.
const LABEL: &str = "default";

/// Reads the key. The seam is the terminal: a terminal turns echo off, a
/// pipe reads a line as it is.
pub(crate) trait KeyReader {
    /// One line from `input`, trimmed, wrapped in a [`Secret`] at once.
    fn read_key(&mut self, input: &mut dyn BufRead, err: &mut dyn Write) -> io::Result<Secret>;
}

/// Reads a key from a pipe or a file.
pub(crate) struct Plain;

impl KeyReader for Plain {
    fn read_key(&mut self, input: &mut dyn BufRead, _err: &mut dyn Write) -> io::Result<Secret> {
        read_line(input)
    }
}

/// Reads a key from a terminal with echo off. The terminal is restored on
/// every way out, a signal included.
pub(crate) struct NoEcho;

impl KeyReader for NoEcho {
    fn read_key(&mut self, input: &mut dyn BufRead, err: &mut dyn Write) -> io::Result<Secret> {
        let guard = EchoOff::new()?;
        let key = read_line(input);
        drop(guard);
        // The newline the person typed was not echoed.
        writeln!(err)?;
        key
    }
}

fn read_line(input: &mut dyn BufRead) -> io::Result<Secret> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(Secret::new(line.trim().to_owned()))
}

/// Standard input with echo off until it drops. While it lives, SIGINT,
/// SIGTERM and SIGHUP restore the terminal first and then end the process as
/// they would have: the default handler runs on a thread, so nothing in a
/// signal handler allocates or locks.
struct EchoOff {
    fd: OwnedFd,
    saved: Termios,
    signals: Handle,
    watcher: Option<JoinHandle<()>>,
}

impl EchoOff {
    fn new() -> io::Result<Self> {
        let fd = io::stdin().as_fd().try_clone_to_owned()?;
        let saved = termios::tcgetattr(&fd)?;
        let mut quiet = saved.clone();
        quiet.local_modes.remove(LocalModes::ECHO);
        let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
        let handle = signals.handle();
        let watched = fd.try_clone()?;
        let restore = saved.clone();
        let watcher = thread::spawn(move || {
            if let Some(signal) = signals.forever().next() {
                termios::tcsetattr(&watched, OptionalActions::Now, &restore).unwrap_or(());
                signal_hook::low_level::emulate_default_handler(signal).unwrap_or(());
            }
        });
        let off = Self {
            fd,
            saved,
            signals: handle,
            watcher: Some(watcher),
        };
        // A failure here drops `off`, which stops the watcher.
        termios::tcsetattr(&off.fd, OptionalActions::Drain, &quiet)?;
        Ok(off)
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        termios::tcsetattr(&self.fd, OptionalActions::Drain, &self.saved).unwrap_or(());
        self.signals.close();
        if let Some(watcher) = self.watcher.take() {
            watcher.join().unwrap_or(());
        }
    }
}

/// What `login` reads and writes.
pub(crate) struct LoginIo<'a> {
    /// Fiber home.
    pub(crate) home: &'a Path,
    /// The installed providers.
    pub(crate) providers: &'a Providers,
    /// Whether a person is there to ask: stdin and stderr are terminals.
    pub(crate) terminal: bool,
    /// Standard input: the provider menu's answer, and the key unless
    /// `keys` says otherwise.
    pub(crate) stdin: &'a mut dyn BufRead,
    /// Standard error: the menu, the prompt and the one line of the result.
    pub(crate) err: &'a mut dyn Write,
    /// Reads the key.
    pub(crate) keys: &'a mut dyn KeyReader,
}

fn usage(message: impl Into<String>) -> Failure {
    failure(
        ErrorCode::Usage,
        format!("{} Run `fiber --help` for usage.", message.into()),
    )
}

fn config_failure(e: ConfigError) -> Failure {
    failure(e.code(), e.to_string())
}

fn terminal_failure(e: io::Error) -> Failure {
    failure(ErrorCode::IoFailed, format!("the terminal: {e}"))
}

/// The credential directory a provider reads.
fn stored_name(provider: &ProviderData) -> &str {
    provider
        .credential_name
        .as_deref()
        .unwrap_or(&provider.name)
}

/// The provider named `name`, or a usage error naming the installed ones.
fn installed<'a>(providers: &'a Providers, name: &str) -> Result<&'a ProviderData, Failure> {
    providers.get(name).ok_or_else(|| {
        let names: Vec<&str> = providers.names().collect();
        usage(if names.is_empty() {
            format!("`{name}` is not an installed provider, and none is installed.")
        } else {
            format!(
                "`{name}` is not an installed provider; the installed providers are {}.",
                names.join(", ")
            )
        })
    })
}

/// The provider a person picks from the menu, by number or by name.
fn choose(io: &mut LoginIo<'_>) -> Result<String, Failure> {
    if !io.terminal {
        return Err(usage(
            "`fiber login` takes a provider when there is no terminal to ask on.",
        ));
    }
    let names: Vec<&str> = io.providers.names().collect();
    if names.is_empty() {
        return Err(usage("No provider is installed."));
    }
    let mut menu = String::from("Providers:\n");
    for (index, name) in names.iter().enumerate() {
        menu.push_str(&format!("  {}) {name}\n", index + 1));
    }
    menu.push_str("Provider, by number or name: ");
    io.err
        .write_all(menu.as_bytes())
        .and_then(|()| io.err.flush())
        .map_err(terminal_failure)?;
    let mut answer = String::new();
    let read = io.stdin.read_line(&mut answer).map_err(terminal_failure)?;
    let answer = answer.trim();
    if read == 0 || answer.is_empty() {
        return Err(usage("No provider was chosen."));
    }
    let picked = match answer.parse::<usize>() {
        Ok(number) => number.checked_sub(1).and_then(|index| names.get(index)),
        Err(_) => names.iter().find(|name| **name == answer),
    };
    picked.map(|name| (*name).to_owned()).ok_or_else(|| {
        usage(format!(
            "`{answer}` is neither a listed number nor an installed provider."
        ))
    })
}

/// `providers."<name>".credential`, with a name that holds a dot quoted.
fn credential_key(name: &str) -> String {
    if name.contains('.') {
        format!("providers.\"{name}\".credential")
    } else {
        format!("providers.{name}.credential")
    }
}

/// Stores a provider's key as `credentials/<stored>/default`, and names the
/// label in `providers."<name>".credential` when that is unset
/// (`docs/model-routing.md`, "Logging in").
pub(crate) fn login(provider: Option<&str>, io: &mut LoginIo<'_>) -> Result<(), Failure> {
    let name = match provider {
        Some(name) => name.to_owned(),
        None => choose(io)?,
    };
    let data = installed(io.providers, &name)?;
    let stored = stored_name(data);
    let file = CredentialFile::new(io.home, stored, LABEL).map_err(config_failure)?;
    // Held until the login ends, so two logins never both pass the check.
    let Some(_lock) = file.try_lock().map_err(config_failure)? else {
        return Err(failure(
            ErrorCode::IoFailed,
            format!("another login for {name} is running"),
        ));
    };
    if read_credential(io.home, stored, LABEL)
        .map_err(config_failure)?
        .is_some()
    {
        return Err(usage(format!(
            "credentials/{stored}/{LABEL} is already stored; run `fiber logout {name}` first."
        )));
    }
    if io.terminal {
        write!(io.err, "Key for {name}: ")
            .and_then(|()| io.err.flush())
            .map_err(terminal_failure)?;
    }
    let key = io
        .keys
        .read_key(io.stdin, io.err)
        .map_err(terminal_failure)?;
    if key.expose().is_empty() {
        return Err(usage("No key was given; nothing was stored."));
    }
    store_credential(io.home, stored, LABEL, &key).map_err(config_failure)?;
    let first = set_global_if_unset(io.home, &credential_key(&name), LABEL.into());
    if let Err(e) = first {
        // A retry must start clean: no key stored without its label.
        delete_credential(io.home, stored, LABEL).unwrap_or(false);
        return Err(config_failure(e));
    }
    writeln!(io.err, "fiber: stored credentials/{stored}/{LABEL}").map_err(terminal_failure)
}

/// Where a provider's key comes from when nothing is stored: the first
/// source configured for one of its labels, in label order, then the one its
/// own data declares. A command is named by its program alone: its arguments
/// may hold a key.
fn declared_source(config: &Config, provider: &ProviderData) -> Option<String> {
    let merged = config.merged(None);
    let configured = merged
        .get("providers")
        .and_then(|p| p.get(&provider.name))
        .and_then(|p| p.get("credentials"))
        .and_then(serde_json::Value::as_object);
    let mut labels: Vec<&String> = configured.into_iter().flat_map(|l| l.keys()).collect();
    labels.sort();
    let from_config = labels.into_iter().find_map(|label| {
        let value = configured?.get(label)?;
        serde_json::from_value::<CredentialSource>(value.clone()).ok()
    });
    let source = from_config.or_else(|| provider.credential.clone())?;
    Some(match source {
        CredentialSource::Env(var) => format!("the environment variable {var}"),
        CredentialSource::File(file) => format!("the file {}", file.display()),
        CredentialSource::Command(argv) => match argv.first() {
            Some(program) => format!("the command {program}"),
            None => "an empty command".to_owned(),
        },
    })
}

/// Deletes a provider's stored key. A key that comes from an environment
/// variable, a file or a command is named, never removed.
pub(crate) fn logout(
    provider: Option<&str>,
    home: &Path,
    providers: &Providers,
    config: &Config,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let Some(name) = provider else {
        return Err(failure(ErrorCode::Usage, LOGOUT_SHAPE));
    };
    let data = installed(providers, name)?;
    let stored = stored_name(data);
    let labels = credential_labels(home, stored).map_err(config_failure)?;
    let label = match labels.as_slice() {
        [one] => one,
        [] => {
            return Err(failure(
                ErrorCode::CredentialMissing,
                match declared_source(config, data) {
                    Some(source) => {
                        format!("{name}'s key comes from {source}; fiber logout cannot remove it")
                    }
                    None => format!("no stored credential for {name}"),
                },
            ));
        }
        [..] => {
            return Err(usage(format!(
                "`{name}` has several stored credentials: {}.",
                labels.join(", ")
            )));
        }
    };
    if !delete_credential(home, stored, label).map_err(config_failure)? {
        return Err(failure(
            ErrorCode::CredentialMissing,
            format!("no stored credential for {name}"),
        ));
    }
    let siblings: Vec<&str> = providers
        .names()
        .filter(|other| *other != name)
        .filter(|other| {
            providers
                .get(other)
                .is_some_and(|p| stored_name(p) == stored)
        })
        .collect();
    let also = if siblings.is_empty() {
        String::new()
    } else {
        format!(", which {} also reads", siblings.join(", "))
    };
    writeln!(err, "fiber: removed credentials/{stored}/{label}{also}").map_err(terminal_failure)
}

/// Prints a failure the way every command does, and gives its exit code.
fn finish(result: Result<(), Failure>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            // A closed stderr leaves nobody to tell.
            writeln!(io::stderr(), "fiber: {}", e.message).unwrap_or(());
            doors::exit_code(&e)
        }
    }
}

fn home_and_providers() -> Result<(std::path::PathBuf, Providers), Failure> {
    let home = config::fiber_home_from_env().map_err(config_failure)?;
    // debt: notices from loading are dropped, as `parts_with` drops them;
    // surfaced when #382 lands.
    let (providers, _notices) =
        Providers::load(&home).map_err(|e| failure(e.code(), e.to_string()))?;
    Ok((home, providers))
}

/// `fiber login [<provider>]`.
pub(crate) fn run_login(args: LoginArgs) -> i32 {
    let ran = home_and_providers().and_then(|(home, providers)| {
        let stdin = io::stdin();
        let on_terminal = stdin.is_terminal();
        let mut err = io::stderr();
        let terminal = on_terminal && err.is_terminal();
        let mut keys: Box<dyn KeyReader> = if on_terminal {
            Box::new(NoEcho)
        } else {
            Box::new(Plain)
        };
        login(
            args.provider.as_deref(),
            &mut LoginIo {
                home: &home,
                providers: &providers,
                terminal,
                stdin: &mut stdin.lock(),
                err: &mut err,
                keys: keys.as_mut(),
            },
        )
    });
    finish(ran)
}

/// `fiber logout <provider>`.
pub(crate) fn run_logout(args: LogoutArgs) -> i32 {
    if args.provider.is_none() {
        return finish(Err(failure(ErrorCode::Usage, LOGOUT_SHAPE)));
    }
    let ran = home_and_providers().and_then(|(home, providers)| {
        let workspace = std::env::current_dir()
            .map_err(|e| failure(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
        let (_, project) = crate::project_of(&home, &workspace)?;
        let config = Config::load(Sources {
            home: home.clone(),
            workspace,
            project,
            overrides: Vec::new(),
        })
        .map_err(config_failure)?;
        logout(
            args.provider.as_deref(),
            &home,
            &providers,
            &config,
            &mut io::stderr(),
        )
    });
    finish(ran)
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
