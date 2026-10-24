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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use config::{
    Config, CredentialFile, CredentialLock, Login, ProviderData, Secret, Sources,
    credential_labels, delete_credential, delete_credential_held, read_credential, read_secret,
    set_global_if_unset, store_credential, store_secret,
};
use contract::clock::Clock;
use contract::events::Notice;
use extensions::{Providers, RepositorySet};
use rustix::termios::{self, LocalModes, OptionalActions, Termios};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::{Handle, Signals};

mod browser;
mod host;
mod reviewer;

use crate::{CliError, Refusal, finish, project_of};
use browser::login_with;
pub use browser::{LoginCancel, browser_login};
pub use host::save_host;
pub use reviewer::{ReviewerModels, reviewer_models, set_reviewer};

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

    /// Writes `prompt` to `err`, then reads one line from `input` taken
    /// exactly: only its final newline is removed, so a secret with
    /// leading or trailing spaces round-trips (`fiber mcp add --secret`).
    fn read_key_exact(
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

    fn read_key_exact(
        &mut self,
        prompt: &str,
        input: &mut dyn BufRead,
        err: &mut dyn Write,
    ) -> io::Result<Secret> {
        write_prompt(prompt, err)?;
        read_exact_line(input)
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

    fn read_key_exact(
        &mut self,
        prompt: &str,
        input: &mut dyn BufRead,
        err: &mut dyn Write,
    ) -> io::Result<Secret> {
        let guard = EchoOff::new()?;
        let key = write_prompt(prompt, err).and_then(|()| read_exact_line(input));
        drop(guard);
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

/// One line taken exactly: only its final newline goes, so a secret with
/// spaces round-trips. A final line with no newline is taken whole, and a
/// trailing `\r` without a `\n` stays: only `\n` or `\r\n` is removed.
pub(crate) fn read_exact_line(input: &mut dyn BufRead) -> io::Result<Secret> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    let line = match line.strip_suffix('\n') {
        Some(stripped) => stripped.strip_suffix('\r').unwrap_or(stripped),
        None => line.as_str(),
    };
    Ok(Secret::new(line.to_owned()))
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
    // Exempted from mutation testing: the drop restores terminal echo on
    // the real stdin file descriptor, which no test reaches without a real
    // TTY or a production hook.
    #[cfg_attr(false, mutants::skip)]
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
    /// Whether to log in with a device code instead of the browser, for a
    /// provider that logs in by browser.
    pub(crate) device: bool,
    /// The process clock, which a browser login's timeouts read.
    pub(crate) clock: Arc<dyn Clock>,
}

/// The credential directory a provider reads.
fn stored_name(provider: &ProviderData) -> &str {
    provider
        .credential_name
        .as_deref()
        .unwrap_or(&provider.name)
}

/// The provider named `name`, or a usage error naming the installed ones.
fn installed<'a>(providers: &'a Providers, name: &str) -> Result<&'a ProviderData, CliError> {
    providers.get(name).ok_or_else(|| {
        let installed: Vec<String> = providers.names().map(str::to_owned).collect();
        CliError::ProviderNotInstalled {
            name: name.to_owned(),
            installed,
        }
    })
}

/// The secrets installed extensions declare, less any name that is also a
/// provider: that name always logs in to the provider.
fn declared(providers: &Providers) -> impl Iterator<Item = &str> {
    providers
        .secrets()
        .filter(|name| providers.get(name).is_none())
}

/// The refusal for a name that is neither a provider nor a declared
/// secret, listing both, with no CLI hint: `login` adds it.
fn unknown_refusal(providers: &Providers, name: &str) -> Refusal {
    Refusal::UnknownName {
        name: name.to_owned(),
        providers: providers.names().map(str::to_owned).collect(),
        secrets: declared(providers).map(str::to_owned).collect(),
    }
}

/// The provider or declared secret a person picks from the menu, by number
/// or by name. Providers are numbered first, then secrets.
fn choose(io: &mut LoginIo<'_>) -> Result<String, CliError> {
    if !io.terminal {
        return Err(CliError::LoginNeedsName);
    }
    let all = targets(io.providers);
    let providers: Vec<&str> = all
        .iter()
        .filter_map(|target| match target {
            LoginName::Provider(name) | LoginName::Browser(name) => Some(name.as_str()),
            LoginName::Secret(_) => None,
        })
        .collect();
    let secrets: Vec<&str> = all
        .iter()
        .filter_map(|target| match target {
            LoginName::Secret(name) => Some(name.as_str()),
            LoginName::Provider(_) | LoginName::Browser(_) => None,
        })
        .collect();
    if providers.is_empty() && secrets.is_empty() {
        return Err(CliError::NothingToLogInTo);
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
        .map_err(CliError::Terminal)?;
    let mut answer = String::new();
    io.stdin
        .read_line(&mut answer)
        .map_err(CliError::Terminal)?;
    let answer = answer.trim();
    if answer.is_empty() {
        return Err(CliError::NothingChosen);
    }
    let picked = match answer.parse::<usize>() {
        Ok(number) => number.checked_sub(1).and_then(|index| all.get(index)),
        Err(_) => all.iter().find(|target| target.name() == answer),
    };
    picked
        .map(|target| target.name().to_owned())
        .ok_or_else(|| CliError::ChoiceUnknown {
            answer: answer.to_owned(),
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
    /// An installed provider whose data says `login: browser`, by name.
    Browser(String),
    /// A secret an installed extension declares, by name.
    Secret(String),
}

impl LoginName {
    /// The provider or secret's name.
    fn name(&self) -> &str {
        match self {
            Self::Provider(name) | Self::Browser(name) | Self::Secret(name) => name,
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

/// Stores one provider credential under the label's lock: build the file,
/// hold the label lock across the re-read, the caller's write, the global
/// label update and any rollback, then return the relative credential
/// path. The lock order stays credential then global. A failed global
/// update deletes only the just-written label under the held lock and
/// still returns the global error, even when the deletion fails.
fn store_locked(
    home: &Path,
    provider: &ProviderData,
    label: &str,
    write: impl FnOnce(&CredentialLock) -> Result<(), CliError>,
) -> Result<String, CliError> {
    let name = provider.name.as_str();
    let stored = stored_name(provider);
    let file = CredentialFile::new(home, stored, label).map_err(CliError::CredentialFile)?;
    // Held until the login ends, so two logins never both pass the check.
    let Some(lock) = file.try_lock()? else {
        return Err(CliError::LoginRunning {
            name: name.to_owned(),
        });
    };
    if read_credential(home, stored, label)?.is_some() {
        return Err(CliError::StoreRefused {
            refusal: already_stored(name, stored, label),
            hint: false,
        });
    }
    write(&lock)?;
    if let Err(e) = set_global_if_unset(home, &credential_key(name), label.into()) {
        // A retry must start clean: no key stored without its label.
        delete_credential_held(home, stored, label, &lock).unwrap_or(false);
        return Err(CliError::Config(e));
    }
    Ok(format!("credentials/{stored}/{label}"))
}

/// The installed providers by name, then the secrets installed extensions
/// declare: the one place the menu's order lives. A name that is both is
/// the provider's. A provider whose data says `login: browser` is a browser
/// row, in provider order; every other provider is a key row.
fn targets(providers: &Providers) -> Vec<LoginName> {
    providers
        .names()
        .map(|name| {
            if providers
                .get(name)
                .is_some_and(|data| data.credential_from.login() == Some(Login::Browser))
            {
                LoginName::Browser(name.to_owned())
            } else {
                LoginName::Provider(name.to_owned())
            }
        })
        .chain(declared(providers).map(|name| LoginName::Secret(name.to_owned())))
        .collect()
}

/// The installed providers in `home`.
pub fn providers_in(home: &Path) -> Result<Providers, CliError> {
    Ok(providers_with_notices(home)?.0)
}

/// The installed providers in `home`, and the notices loading them gave.
pub(crate) fn providers_with_notices(home: &Path) -> Result<(Providers, Vec<Notice>), CliError> {
    Ok(Providers::load(home)?)
}

/// Stores `key` for provider or secret `name`, under `label` for a
/// provider: the steps `login` runs after a name is chosen, in their order.
/// The key comes from `read`: a refused login reads no key.
fn store(
    home: &Path,
    providers: &Providers,
    name: &str,
    label: Option<&str>,
    read: impl FnOnce(&str) -> Result<Secret, CliError>,
) -> Result<LoginStored, CliError> {
    if let Some(data) = providers.get(name) {
        // A key login reveals no email; the browser login passes its own.
        let label = chosen_label(label, None);
        let path = store_locked(home, data, label, |_lock| {
            let key = read(&format!("Key for {name}: "))?;
            if key.expose().is_empty() {
                return Err(CliError::StoreRefused {
                    refusal: Refusal::NoKeyGiven,
                    hint: false,
                });
            }
            store_credential(home, stored_name(data), label, &key)?;
            Ok(())
        })?;
        Ok(LoginStored {
            path,
            replaced: false,
        })
    } else if providers.secrets().any(|secret| secret == name) {
        if label.is_some() {
            return Err(CliError::StoreRefused {
                refusal: Refusal::AsOnlyForProvider {
                    name: name.to_owned(),
                },
                hint: false,
            });
        }
        let replaced = read_secret(home, name)?.is_some();
        let value = read(&format!("Value for {name}: "))?;
        if value.expose().is_empty() {
            return Err(CliError::StoreRefused {
                refusal: Refusal::NoValueGiven,
                hint: false,
            });
        }
        store_secret(home, name, &value)?;
        Ok(LoginStored {
            path: format!("credentials/{name}"),
            replaced,
        })
    } else {
        Err(CliError::StoreRefused {
            refusal: unknown_refusal(providers, name),
            hint: false,
        })
    }
}

/// Why a store failed to start: the label is already stored, naming
/// `--as` (`docs/model-routing.md`, "Logging in").
fn already_stored(name: &str, stored: &str, label: &str) -> Refusal {
    Refusal::AlreadyStored {
        name: name.to_owned(),
        stored: stored.to_owned(),
        label: label.to_owned(),
    }
}

/// The installed providers by name, then the declared secrets.
pub fn login_targets(home: &Path) -> Result<Vec<LoginName>, CliError> {
    Ok(targets(&providers_in(home)?))
}

/// Whether any label is stored for the provider's stored credential
/// name: the live signed-in mark the setup question shows.
pub(crate) fn signed_in(home: &Path, data: &ProviderData) -> Result<bool, config::ConfigError> {
    Ok(!credential_labels(home, stored_name(data))?.is_empty())
}

/// Stores `key` for `name` as `fiber login <name> [--as <label>]` would,
/// the key already read. A refusal carries no CLI hint.
pub fn login_store(
    home: &Path,
    name: &str,
    label: Option<&str>,
    key: Secret,
) -> Result<LoginStored, CliError> {
    let providers = providers_in(home)?;
    store(home, &providers, name, label, |_| Ok(key))
}

/// Logs in as `fiber login` does: prints each notice as one line on
/// standard error first, then runs [`login`]. `loading` is the notices
/// the providers load gave; the one configuration this command loads
/// adds its own. A configuration that fails to load fails no login:
/// there is nothing to print.
pub(crate) fn login_with_notices(
    name: Option<&str>,
    label: Option<&str>,
    io: &mut LoginIo<'_>,
    loading: Vec<Notice>,
) -> Result<(), CliError> {
    let mut notices = Vec::new();
    if let Ok(workspace) = std::env::current_dir()
        && let Ok((_, project)) = crate::project_of(io.home, &workspace)
        && let Ok(config) = Config::load(Sources {
            home: io.home.to_path_buf(),
            workspace,
            project,
            overrides: Vec::new(),
        })
    {
        notices.extend(config.notices().iter().cloned());
    }
    notices.extend(loading);
    crate::config::print_notices(io.err, &notices);
    login(name, label, io)
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
) -> Result<(), CliError> {
    let name = match name {
        Some(name) => name.to_owned(),
        None => choose(io)?,
    };
    // A provider that logs in by browser runs its `credential()` login
    // instead of reading a key (`docs/model-routing.md`, "Logging in").
    if io
        .providers
        .get(&name)
        .is_some_and(|data| data.credential_from.login() == Some(Login::Browser))
    {
        return login_with(io, &name, label);
    }
    if io.device
        && (io.providers.get(&name).is_some()
            || io.providers.secrets().any(|secret| secret == name))
    {
        return Err(CliError::DeviceNeedsBrowserProvider);
    }
    let home = io.home;
    let providers = io.providers;
    let terminal = io.terminal;
    let stored = store(home, providers, &name, label, |prompt| {
        let prompt = if terminal { prompt } else { "" };
        io.keys
            .read_key(prompt, io.stdin, io.err)
            .map_err(CliError::Terminal)
    });
    match stored {
        Ok(stored) => {
            let done = if stored.replaced {
                "replaced"
            } else {
                "stored"
            };
            // The host questions follow a stored provider key, and the
            // reviewer only after no gaps: never a declared secret or a
            // failed store, and never a browser login (which asks through
            // its own post-store step). Cancellation arms before the
            // stored-line readiness output, so an interrupt between the
            // store and the prompt still ends the login with no reviewer
            // set instead of terminating it.
            if io.terminal
                && io
                    .providers
                    .get(&name)
                    .is_some_and(|data| data.credential_from.login() != Some(Login::Browser))
            {
                host::hosts(io, &name)?;
                post_store_reviewer(io, &name, &format!("fiber: {done} {}", stored.path))?;
            } else if !io.terminal && io.providers.get(&name).is_some() {
                host::hosts(io, &name)?;
                writeln!(io.err, "fiber: {done} {}", stored.path).map_err(CliError::Terminal)?;
            } else {
                writeln!(io.err, "fiber: {done} {}", stored.path).map_err(CliError::Terminal)?;
            }
            Ok(())
        }
        Err(CliError::StoreRefused { refusal, .. }) => Err(CliError::StoreRefused {
            refusal,
            hint: true,
        }),
        Err(error) => Err(error),
    }
}

/// The two pause points around the reviewer save's commit claim, both
/// owned by the test: the stage signals each entry and waits for its
/// release, so a test forces the before-claim and after-claim
/// interleavings on every run. The waits block: production never takes
/// them, and the test bounds the whole stage with its own deadline, so a
/// barrier the test never releases fails the test instead of hanging it.
#[derive(Debug)]
pub(crate) struct ClaimProbe {
    /// Entered after selection and the pre-claim checks, before the claim.
    pub before_entered: mpsc::Sender<()>,
    /// Releases the stage to attempt the claim.
    pub before_release: mpsc::Receiver<()>,
    /// Entered after the claim won, immediately before the write.
    pub after_entered: mpsc::Sender<()>,
    /// Releases the stage to run the write.
    pub after_release: mpsc::Receiver<()>,
}

/// The post-store SIGINT record: the interrupt is recorded directly on
/// the stage's latch, never default-terminating, and unregistered on
/// every return. Terminal modes need no restore: the reviewer choice is
/// no secret, so echo stays on through the whole stage. The login process
/// ends with the stage either way, so no disposition outlives it.
struct SigGuard {
    id: signal_hook::SigId,
}

impl Drop for SigGuard {
    fn drop(&mut self) {
        signal_hook::low_level::unregister(self.id);
    }
}

fn arm_sigint(stopped: Arc<AtomicBool>) -> Result<SigGuard, CliError> {
    signal_hook::flag::register(SIGINT, stopped)
        .map(|id| SigGuard { id })
        .map_err(CliError::InterruptHandler)
}

/// Asks for the reviewer's model after `name`'s credential was stored:
/// the provider's own list with the saved choice preselected, then the
/// one-key save. Cancellation is armed before `stored_line` is printed,
/// so an interrupt anywhere past the store takes the cancelled outcome:
/// EOF or cancellation keeps the credential and the previous reviewer
/// setting, prints that no reviewer was set and ends 0; a failed save
/// keeps the credential and names the key with a non-zero exit. Only the
/// prompt reads the terminal; discovery and the save never do.
fn post_store_reviewer(
    io: &mut LoginIo<'_>,
    name: &str,
    stored_line: &str,
) -> Result<(), CliError> {
    let stopped = Arc::new(AtomicBool::new(false));
    let _sig = arm_sigint(Arc::clone(&stopped))?;
    writeln!(io.err, "{stored_line}").map_err(CliError::Terminal)?;
    let home = io.home;
    let providers = io.providers;
    let terminal = io.terminal;
    let clock = Arc::clone(&io.clock);
    let discover = || reviewer_models(home, providers, name, Arc::clone(&clock));
    reviewer_stage_armed(
        home, name, io.stdin, io.err, &discover, &stopped, terminal, &clock, None,
    )
}

/// The post-store reviewer stage [`post_store_reviewer`] runs: armed
/// cancellation, discovery, the prompt and the claimed save. `discover`
/// is injected so a test holds it on a barrier; `stopped` is the stage's
/// own latch, armed once here and never reset. Cancellation recorded
/// before the save's commit claim prevents the write; once the claim
/// wins, even during the global-lock wait, the write's own outcome
/// stands and is never relabelled as cancelled.
#[cfg(test)]
#[allow(clippy::too_many_arguments, reason = "one per input the stage reads")]
pub(crate) fn reviewer_stage(
    home: &Path,
    name: &str,
    input: &mut dyn BufRead,
    err: &mut dyn Write,
    discover: &dyn Fn() -> Result<ReviewerModels, CliError>,
    stopped: &Arc<AtomicBool>,
    terminal: bool,
    clock: Arc<dyn Clock>,
    probe: Option<&ClaimProbe>,
) -> Result<(), CliError> {
    // Armed before discovery, never reset: a SIGINT during the bounded
    // callback is recorded, and after the callback the stage takes the
    // cancelled outcome instead of presenting or saving.
    let _sig = arm_sigint(Arc::clone(stopped))?;
    reviewer_stage_armed(
        home, name, input, err, discover, stopped, terminal, &clock, probe,
    )
}

/// The armed stage [`reviewer_stage`] and [`post_store_reviewer`] share:
/// discovery, the prompt and the claimed save under an already-armed
/// cancellation latch. `terminal` tells the prompt that `input` is the
/// process stdin, so it reads stoppably.
#[allow(clippy::too_many_arguments, reason = "one per input the stage reads")]
fn reviewer_stage_armed(
    home: &Path,
    name: &str,
    input: &mut dyn BufRead,
    err: &mut dyn Write,
    discover: &dyn Fn() -> Result<ReviewerModels, CliError>,
    stopped: &Arc<AtomicBool>,
    terminal: bool,
    clock: &Arc<dyn Clock>,
    probe: Option<&ClaimProbe>,
) -> Result<(), CliError> {
    let choices = discover()?;
    if stopped.load(Ordering::SeqCst) {
        return no_reviewer_set(err, name);
    }
    for notice in &choices.notices {
        writeln!(err, "{notice}").map_err(CliError::Terminal)?;
    }
    if choices.models.is_empty() {
        return Ok(());
    }
    let stdin = io::stdin();
    let id = match reviewer::prompt_choice(
        name,
        &choices,
        input,
        err,
        &|| stopped.load(Ordering::SeqCst),
        terminal,
        clock,
        stdin.as_fd(),
    )
    .map_err(CliError::Terminal)?
    {
        reviewer::Pick::Chosen(id) => id,
        reviewer::Pick::Cancelled => return no_reviewer_set(err, name),
    };
    if let Some(probe) = probe {
        probe.before_entered.send(()).unwrap_or(());
        probe.before_release.recv().unwrap_or(());
    }
    // The commit boundary, immediately before the write and its
    // global-lock wait: the atomic claim of the save on the same latch
    // cancellation records on, never a last-check-then-write sequence.
    if stopped.swap(true, Ordering::SeqCst) {
        return no_reviewer_set(err, name);
    }
    if let Some(probe) = probe {
        probe.after_entered.send(()).unwrap_or(());
        probe.after_release.recv().unwrap_or(());
    }
    reviewer::set_reviewer(home, name, &id)?;
    writeln!(err, "fiber: reviewer {name} {id}").map_err(CliError::Terminal)?;
    Ok(())
}

/// The cancelled-or-EOF outcome: the credential and any previous reviewer
/// setting stand, and the login still ends 0.
fn no_reviewer_set(err: &mut dyn Write, name: &str) -> Result<(), CliError> {
    writeln!(
        err,
        "fiber: no reviewer was set for {name}; the credential stays."
    )
    .map_err(CliError::Terminal)
}

/// Where a provider's key comes from when nothing is stored: the first
/// source configured for one of its labels, in label order, then the one its
/// own data declares, described without its secret.
fn declared_source(config: &Config, provider: &ProviderData) -> Option<String> {
    let source = config
        .credentials()
        .credential_sources(&provider.name)
        .into_iter()
        .next()
        .map(|(_, declared)| declared)
        .or_else(|| provider.credential_from.credential().cloned())?;
    Some(source.describe())
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
) -> Result<(), CliError> {
    let Some(name) = provider else {
        return Err(CliError::LogoutNeedsProvider);
    };
    let data = installed(providers, name)?;
    let stored = stored_name(data);
    let labels = credential_labels(home, stored)?;
    if labels.is_empty() {
        return Err(match declared_source(config, data) {
            Some(origin) => CliError::KeyFromElsewhere {
                name: name.to_owned(),
                origin,
            },
            None => CliError::NoStoredCredential {
                name: name.to_owned(),
            },
        });
    }
    let doomed: Vec<&str> = match target {
        LogoutTarget::All => labels.iter().map(String::as_str).collect(),
        LogoutTarget::Label(label) if labels.iter().any(|l| l == label) => vec![label],
        LogoutTarget::Label(label) => {
            return Err(CliError::LabelNotAmong {
                name: name.to_owned(),
                label: label.to_owned(),
                labels,
            });
        }
        LogoutTarget::Only => match labels.as_slice() {
            [one] => vec![one.as_str()],
            _ => {
                return Err(CliError::LogoutSeveral {
                    name: name.to_owned(),
                    labels,
                });
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
        if !delete_credential(home, stored, label)? {
            return Err(CliError::NoStoredLabel {
                name: name.to_owned(),
                label: label.to_owned(),
            });
        }
        writeln!(err, "fiber: removed credentials/{stored}/{label}{also}")
            .map_err(CliError::Terminal)?;
    }
    Ok(())
}

fn home_and_providers() -> Result<(std::path::PathBuf, Providers, Vec<Notice>), CliError> {
    let home = config::fiber_home_from_env()?;
    let (providers, notices) = providers_with_notices(&home)?;
    Ok((home, providers, notices))
}

/// The home, the installed providers merged with the current workspace's
/// approved repository set, the retained set, and the notices loading
/// them gave: what `fiber login` reads. Every personal install loads, as
/// [`home_and_providers`] reads, with approved repository entries merged
/// in. Outside a project the set is empty and the providers are the
/// installed ones.
fn home_providers_and_set(
    clock: Arc<dyn Clock>,
) -> Result<(std::path::PathBuf, Providers, RepositorySet, Vec<Notice>), CliError> {
    let home = config::fiber_home_from_env()?;
    let workspace = std::env::current_dir().map_err(CliError::CurrentDirectory)?;
    // A workspace with no project reads as home-only, as at Base: only a
    // resolved project holds a repository set.
    let set = project_of(&home, &workspace)
        .map(|(_, project)| {
            RepositorySet::load_pinned(&home, &project, &workspace, Arc::clone(&clock))
        })
        .unwrap_or_default();
    let (providers, notices) = Providers::load_all_repository(&home, &set)?;
    Ok((home, providers, set, notices))
}

/// `fiber login [<name>] [--as <label>] [--device]`.
pub fn run_login(
    name: Option<&str>,
    label: Option<&str>,
    device: bool,
    clock: Arc<dyn Clock>,
) -> i32 {
    run_login_with(name, label, device, clock, None)
}

/// [`run_login`] reading its answers from `stdin` when given, so a caller
/// already holding the standard input lock (`fiber setup` on a terminal)
/// does not lock it a second time; `None` locks standard input here.
// Exempted from mutation testing: telling `&&` from `||` here needs
// exactly one of the real stdin or stderr to be a terminal, which no test
// controls without a production hook.
#[cfg_attr(false, mutants::skip)]
pub(crate) fn run_login_with(
    name: Option<&str>,
    label: Option<&str>,
    device: bool,
    clock: Arc<dyn Clock>,
    stdin: Option<&mut dyn BufRead>,
) -> i32 {
    let ran =
        home_providers_and_set(Arc::clone(&clock)).and_then(|(home, providers, _set, loading)| {
            let on_terminal = io::stdin().is_terminal();
            let mut err = io::stderr();
            let terminal = on_terminal && err.is_terminal();
            let mut keys: Box<dyn KeyReader> = if on_terminal {
                Box::new(NoEcho)
            } else {
                Box::new(Plain)
            };
            let mut locked;
            let stdin: &mut dyn BufRead = match stdin {
                Some(reader) => reader,
                None => {
                    locked = io::stdin().lock();
                    &mut locked
                }
            };
            login_with_notices(
                name,
                label,
                &mut LoginIo {
                    home: &home,
                    providers: &providers,
                    terminal,
                    stdin,
                    err: &mut err,
                    keys: keys.as_mut(),
                    device,
                    clock,
                },
                loading,
            )
        });
    finish(ran)
}

/// `fiber logout <provider> [--as <label> | --all]`.
pub fn run_logout(provider: Option<&str>, target: LogoutTarget<'_>) -> i32 {
    if provider.is_none() {
        return finish(Err(CliError::LogoutNeedsProvider));
    }
    let ran = home_and_providers().and_then(|(home, providers, loading)| {
        let workspace = std::env::current_dir().map_err(CliError::CurrentDirectory)?;
        let (_, project) = project_of(&home, &workspace)?;
        let config = Config::load(Sources {
            home: home.clone(),
            workspace,
            project,
            overrides: Vec::new(),
        })?;
        let mut err = io::stderr();
        let notices: Vec<Notice> = config.notices().iter().cloned().chain(loading).collect();
        crate::config::print_notices(&mut err, &notices);
        logout(provider, target, &home, &providers, &config, &mut err)
    });
    finish(ran)
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
