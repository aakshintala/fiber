//! `fiber login` and `fiber logout` (`docs/invocation.md`, "Fiber itself";
//! `docs/configuration.md`, "Secrets"; `docs/model-routing.md`, "Logging
//! in"): a provider's key is stored in `credentials/<stored>/<label>`, where
//! `<stored>` is the credential the provider reads and `<label>` is the
//! `--as` label (`default` without one), and deleted again. A secret an
//! installed extension declares is stored in `credentials/<name>`. No key or
//! secret reaches stdout, stderr, `Debug` or a log: it is a [`Secret`] from
//! the moment it is read.

use std::io::{self, BufRead, IsTerminal, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::thread::{self, JoinHandle};

use config::{
    Config, ConfigError, CredentialFile, CredentialSource, ProviderData, Secret, Sources,
    credential_labels, delete_credential, delete_credential_held, read_credential, read_secret,
    set_global_if_unset, store_credential, store_secret,
};
use contract::ErrorCode;
use contract::shapes::Failure;
use doors::failure;
use extensions::Providers;
use rustix::termios::{self, LocalModes, OptionalActions, Termios};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::{Handle, Signals};

use crate::{LOGOUT_SHAPE, fail, project_of};

/// The label a login stores under when `--as` is absent and the login
/// revealed no email.
const DEFAULT_LABEL: &str = "default";

/// What `fiber logout` deletes (`docs/invocation.md`, "Commands and flags").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoutTarget<'a> {
    /// No flag: the provider's one stored label.
    Only,
    /// `--as <label>`: that label.
    Label(&'a str),
    /// `--all`: every stored label.
    All,
}

/// The label a login stores under: `--as`, else the email the login
/// revealed, else `default` (`docs/model-routing.md`, "Logging in").
fn chosen_label<'a>(as_label: Option<&'a str>, email: Option<&'a str>) -> &'a str {
    as_label.or(email).unwrap_or(DEFAULT_LABEL)
}

/// Reads the key. The seam is the terminal: a terminal turns echo off, a
/// pipe reads a line as it is.
pub(crate) trait KeyReader {
    /// Writes `prompt` to `err` (nothing when it is empty), then reads one
    /// line from `input`, trimmed, wrapped in a [`Secret`] at once. A
    /// terminal reader turns echo off before the prompt is written, so a
    /// paste sent as soon as the prompt shows is never echoed.
    fn read_key(
        &mut self,
        prompt: &str,
        input: &mut dyn BufRead,
        err: &mut dyn Write,
    ) -> io::Result<Secret>;
}

/// Reads a key from a pipe or a file.
pub(crate) struct Plain;

impl KeyReader for Plain {
    fn read_key(
        &mut self,
        prompt: &str,
        input: &mut dyn BufRead,
        err: &mut dyn Write,
    ) -> io::Result<Secret> {
        write_prompt(prompt, err)?;
        read_line(input)
    }
}

/// Reads a key from a terminal with echo off. The terminal is restored on
/// every way out, a signal included.
pub(crate) struct NoEcho;

impl KeyReader for NoEcho {
    fn read_key(
        &mut self,
        prompt: &str,
        input: &mut dyn BufRead,
        err: &mut dyn Write,
    ) -> io::Result<Secret> {
        let guard = EchoOff::new()?;
        let key = write_prompt(prompt, err).and_then(|()| read_line(input));
        drop(guard);
        // The newline the person typed was not echoed.
        writeln!(err)?;
        key
    }
}

fn write_prompt(prompt: &str, err: &mut dyn Write) -> io::Result<()> {
    if prompt.is_empty() {
        return Ok(());
    }
    err.write_all(prompt.as_bytes())?;
    err.flush()
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

/// The secrets installed extensions declare, less any name that is also a
/// provider: that name always logs in to the provider.
fn declared(providers: &Providers) -> impl Iterator<Item = &str> {
    providers
        .secrets()
        .filter(|name| providers.get(name).is_none())
}

/// The message for a name that is neither a provider nor a declared
/// secret, listing both, with no CLI hint: `login` adds it.
fn unknown_message(providers: &Providers, name: &str) -> String {
    let names: Vec<&str> = providers.names().collect();
    let secrets: Vec<&str> = declared(providers).collect();
    let names = if names.is_empty() {
        "no provider is installed".to_owned()
    } else {
        format!("the installed providers are {}", names.join(", "))
    };
    let secrets = if secrets.is_empty() {
        "no installed extension declares a secret".to_owned()
    } else {
        format!("the declared secrets are {}", secrets.join(", "))
    };
    format!(
        "`{name}` is neither an installed provider nor a declared secret; {names}, and {secrets}."
    )
}

/// The provider or declared secret a person picks from the menu, by number
/// or by name. Providers are numbered first, then secrets.
fn choose(io: &mut LoginIo<'_>) -> Result<String, Failure> {
    if !io.terminal {
        return Err(usage(
            "`fiber login` takes a provider or a secret's name when there is no terminal to ask on.",
        ));
    }
    let all = targets(io.providers);
    let providers: Vec<&str> = all
        .iter()
        .filter_map(|target| match target {
            LoginName::Provider(name) => Some(name.as_str()),
            LoginName::Secret(_) => None,
        })
        .collect();
    let secrets: Vec<&str> = all
        .iter()
        .filter_map(|target| match target {
            LoginName::Secret(name) => Some(name.as_str()),
            LoginName::Provider(_) => None,
        })
        .collect();
    if providers.is_empty() && secrets.is_empty() {
        return Err(usage(
            "No provider is installed, and no installed extension declares a secret.",
        ));
    }
    let mut menu = String::new();
    if !providers.is_empty() {
        menu.push_str("Providers:\n");
    }
    for (index, name) in providers.iter().enumerate() {
        menu.push_str(&format!("  {}) {name}\n", index + 1));
    }
    if !secrets.is_empty() {
        menu.push_str("Secrets:\n");
    }
    for (index, name) in secrets.iter().enumerate() {
        menu.push_str(&format!("  {}) {name}\n", index + providers.len() + 1));
    }
    menu.push_str("Provider or secret, by number or name: ");
    io.err
        .write_all(menu.as_bytes())
        .and_then(|()| io.err.flush())
        .map_err(terminal_failure)?;
    let mut answer = String::new();
    io.stdin.read_line(&mut answer).map_err(terminal_failure)?;
    let answer = answer.trim();
    if answer.is_empty() {
        return Err(usage("Nothing was chosen."));
    }
    let picked = match answer.parse::<usize>() {
        Ok(number) => number.checked_sub(1).and_then(|index| all.get(index)),
        Err(_) => all.iter().find(|target| target.name() == answer),
    };
    picked.map(|target| target.name().to_owned()).ok_or_else(|| {
        usage(format!(
            "`{answer}` is neither a listed number nor an installed provider or declared secret."
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

/// A `fiber login` menu row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginName {
    /// An installed provider, by name.
    Provider(String),
    /// A secret an installed extension declares, by name.
    Secret(String),
}

impl LoginName {
    /// The provider or secret's name.
    fn name(&self) -> &str {
        match self {
            Self::Provider(name) | Self::Secret(name) => name,
        }
    }
}

/// What a login stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginStored {
    /// The file under Fiber home, such as `credentials/acme/default`.
    pub path: String,
    /// Whether it replaced a stored secret.
    pub replaced: bool,
}

/// Why a store failed: a usage refusal, with no CLI hint, or a failure.
enum StoreFailure {
    /// A usage refusal, with no CLI hint.
    Refused(String),
    /// A failure to report as is.
    Failed(Failure),
}

impl From<Failure> for StoreFailure {
    fn from(failure: Failure) -> Self {
        Self::Failed(failure)
    }
}

/// The installed providers by name, then the secrets installed extensions
/// declare: the one place the menu's order lives. A name that is both is
/// the provider's.
fn targets(providers: &Providers) -> Vec<LoginName> {
    providers
        .names()
        .map(|name| LoginName::Provider(name.to_owned()))
        .chain(declared(providers).map(|name| LoginName::Secret(name.to_owned())))
        .collect()
}

/// The installed providers in `home`.
fn providers_in(home: &Path) -> Result<Providers, Failure> {
    // debt: notices from loading are dropped, as `parts_with` drops them;
    // surfaced when #382 lands.
    let (providers, _notices) =
        Providers::load(home).map_err(|e| failure(e.code(), e.to_string()))?;
    Ok(providers)
}

/// Stores `key` for provider or secret `name`, under `label` for a
/// provider: the steps `login` runs after a name is chosen, in their order.
/// The key comes from `read`: a refused login reads no key.
fn store(
    home: &Path,
    providers: &Providers,
    name: &str,
    label: Option<&str>,
    read: impl FnOnce(&str) -> Result<Secret, Failure>,
) -> Result<LoginStored, StoreFailure> {
    if let Some(data) = providers.get(name) {
        let stored = stored_name(data);
        // A key login reveals no email; the OAuth login of #311 passes its own.
        let label = chosen_label(label, None);
        let file = CredentialFile::new(home, stored, label).map_err(config_failure)?;
        // Held until the login ends, so two logins never both pass the check.
        let Some(lock) = file.try_lock().map_err(config_failure)? else {
            return Err(StoreFailure::Failed(failure(
                ErrorCode::IoFailed,
                format!("another login for {name} is running"),
            )));
        };
        if read_credential(home, stored, label)
            .map_err(config_failure)?
            .is_some()
        {
            return Err(StoreFailure::Refused(format!(
                "credentials/{stored}/{label} is already stored; log in under another label with --as <label>, or run `fiber logout {name} --as {label}` first."
            )));
        }
        let key = read(&format!("Key for {name}: "))?;
        if key.expose().is_empty() {
            return Err(StoreFailure::Refused(
                "No key was given; nothing was stored.".to_owned(),
            ));
        }
        store_credential(home, stored, label, &key).map_err(config_failure)?;
        let first = set_global_if_unset(home, &credential_key(name), label.into());
        if let Err(e) = first {
            // A retry must start clean: no key stored without its label.
            delete_credential_held(home, stored, label, &lock).unwrap_or(false);
            return Err(config_failure(e).into());
        }
        Ok(LoginStored {
            path: format!("credentials/{stored}/{label}"),
            replaced: false,
        })
    } else if providers.secrets().any(|secret| secret == name) {
        if label.is_some() {
            return Err(StoreFailure::Refused(format!(
                "--as applies only to a provider, and `{name}` is a declared secret."
            )));
        }
        let replaced = read_secret(home, name).map_err(config_failure)?.is_some();
        let value = read(&format!("Value for {name}: "))?;
        if value.expose().is_empty() {
            return Err(StoreFailure::Refused(
                "No value was given; nothing was stored.".to_owned(),
            ));
        }
        store_secret(home, name, &value).map_err(config_failure)?;
        Ok(LoginStored {
            path: format!("credentials/{name}"),
            replaced,
        })
    } else {
        Err(StoreFailure::Refused(unknown_message(providers, name)))
    }
}

/// The installed providers by name, then the declared secrets.
pub fn login_targets(home: &Path) -> Result<Vec<LoginName>, Failure> {
    Ok(targets(&providers_in(home)?))
}

/// Stores `key` for `name` as `fiber login <name> [--as <label>]` would,
/// the key already read. A refusal carries no CLI hint.
pub fn login_store(
    home: &Path,
    name: &str,
    label: Option<&str>,
    key: Secret,
) -> Result<LoginStored, Failure> {
    let providers = providers_in(home)?;
    store(home, &providers, name, label, |_| Ok(key)).map_err(|error| match error {
        StoreFailure::Refused(message) => failure(ErrorCode::Usage, message),
        StoreFailure::Failed(failure) => failure,
    })
}

/// Logs in to the provider `name`, or stores the secret `name` an installed
/// extension declares. A provider's key goes in
/// `credentials/<stored>/<label>`, and the label is named in
/// `providers."<name>".credential` when that is unset
/// (`docs/model-routing.md`, "Logging in"). A name that is both is the
/// provider.
pub(crate) fn login(
    name: Option<&str>,
    label: Option<&str>,
    io: &mut LoginIo<'_>,
) -> Result<(), Failure> {
    let name = match name {
        Some(name) => name.to_owned(),
        None => choose(io)?,
    };
    let home = io.home;
    let providers = io.providers;
    let terminal = io.terminal;
    let stored = store(home, providers, &name, label, |prompt| {
        let prompt = if terminal { prompt } else { "" };
        io.keys
            .read_key(prompt, io.stdin, io.err)
            .map_err(terminal_failure)
    });
    match stored {
        Ok(stored) => {
            let done = if stored.replaced {
                "replaced"
            } else {
                "stored"
            };
            writeln!(io.err, "fiber: {done} {}", stored.path).map_err(terminal_failure)
        }
        Err(StoreFailure::Refused(message)) => Err(usage(message)),
        Err(StoreFailure::Failed(failure)) => Err(failure),
    }
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
    // `serde_json::Map` iterates in key order (no `preserve_order`), which is
    // label order; `a_configured_source_comes_before_the_providers_own_in_label_order` pins it.
    let from_config = configured.and_then(|labels| {
        labels
            .values()
            .find_map(|v| serde_json::from_value::<CredentialSource>(v.clone()).ok())
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

/// Deletes a provider's stored key, one label or every label. A key that
/// comes from an environment variable, a file or a command is named, never
/// removed.
pub(crate) fn logout(
    provider: Option<&str>,
    target: LogoutTarget<'_>,
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
    if labels.is_empty() {
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
    let doomed: Vec<&str> = match target {
        LogoutTarget::All => labels.iter().map(String::as_str).collect(),
        LogoutTarget::Label(label) if labels.iter().any(|l| l == label) => vec![label],
        LogoutTarget::Label(label) => {
            return Err(failure(
                ErrorCode::CredentialMissing,
                format!(
                    "no stored credential {label} for {name}; the stored labels are {}",
                    labels.join(", ")
                ),
            ));
        }
        LogoutTarget::Only => match labels.as_slice() {
            [one] => vec![one.as_str()],
            _ => {
                return Err(usage(format!(
                    "`{name}` has several stored credentials: {}; name one with --as <label>, or use --all.",
                    labels.join(", ")
                )));
            }
        },
    };
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
    for label in doomed {
        if !delete_credential(home, stored, label).map_err(config_failure)? {
            return Err(failure(
                ErrorCode::CredentialMissing,
                format!("no stored credential {label} for {name}"),
            ));
        }
        writeln!(err, "fiber: removed credentials/{stored}/{label}{also}")
            .map_err(terminal_failure)?;
    }
    Ok(())
}

/// Prints a failure the way every command does, and gives its exit code.
fn finish(result: Result<(), Failure>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

fn home_and_providers() -> Result<(std::path::PathBuf, Providers), Failure> {
    let home = config::fiber_home_from_env().map_err(config_failure)?;
    Ok((home.clone(), providers_in(&home)?))
}

/// `fiber login [<name>] [--as <label>]`.
pub fn run_login(name: Option<&str>, label: Option<&str>) -> i32 {
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
            name,
            label,
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

/// `fiber logout <provider> [--as <label> | --all]`.
pub fn run_logout(provider: Option<&str>, target: LogoutTarget<'_>) -> i32 {
    if provider.is_none() {
        return finish(Err(failure(ErrorCode::Usage, LOGOUT_SHAPE)));
    }
    let ran = home_and_providers().and_then(|(home, providers)| {
        let workspace = std::env::current_dir()
            .map_err(|e| failure(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
        let (_, project) = project_of(&home, &workspace)?;
        let config = Config::load(Sources {
            home: home.clone(),
            workspace,
            project,
            overrides: Vec::new(),
        })
        .map_err(config_failure)?;
        logout(
            provider,
            target,
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
