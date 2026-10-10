//! Running one provider's `credential()` login for `fiber login`
//! (`docs/model-routing.md`, "Logging in"): the browser or device-code flow
//! of a provider whose data marks it as a browser login, stored under the
//! chosen label's lock after the already-stored check. A login never holds
//! the label's lock while Lua runs: the flow runs first, and the store
//! follows under the lock.

use std::path::Path;
use std::sync::{Arc, Mutex};

use config::{CredentialFile, delete_credential_held, read_credential, set_global_if_unset};
use contract::clock::Clock;
use contract::shapes::Failure;
use doors::failure;
use extensions::{Browser, LoginMethod, Providers, SystemBrowser, login_provider};

/// Why a cancelled login fails: it stored nothing.
fn cancelled() -> Failure {
    failure(
        contract::ErrorCode::AuthenticationFailed,
        "the login was cancelled; nothing was stored.",
    )
}

/// Serializes a browser login's cancel against its credential commit: the
/// cancel either precedes the whole store step, which then never runs, or
/// follows it, leaving the store step's own outcome standing as in
/// `fiber login` (`docs/model-routing.md`, "Logging in"). One mutex over
/// the flag and the stop, which never runs while the mutex is held.
#[derive(Default)]
pub struct LoginCancel {
    state: Mutex<CancelState>,
}

#[derive(Default)]
struct CancelState {
    cancelled: bool,
    stop: Option<Box<dyn FnOnce() + Send>>,
}

impl LoginCancel {
    /// Cancels the login: a store under way finishes first, and the
    /// registered stop runs once, outside the mutex. Idempotent.
    pub fn cancel(&self) {
        let stop = match self.state.lock() {
            Ok(mut state) => {
                state.cancelled = true;
                state.stop.take()
            }
            Err(_) => None,
        };
        if let Some(stop) = stop {
            stop();
        }
    }

    /// Registers the stop for the running flow: after a cancel it runs at
    /// once, outside the mutex, and reports cancelled.
    fn started(&self, stop: Box<dyn FnOnce() + Send>) -> Result<(), Failure> {
        let run_now = match self.state.lock() {
            Ok(mut state) => {
                if state.cancelled {
                    Some(stop)
                } else {
                    state.stop = Some(stop);
                    None
                }
            }
            Err(_) => Some(stop),
        };
        if let Some(stop) = run_now {
            stop();
            return Err(cancelled());
        }
        Ok(())
    }

    /// Runs the store step under the mutex, refusing without running it
    /// when cancelled: a cancel during the commit waits for it.
    fn commit<T>(&self, store: impl FnOnce() -> Result<T, Failure>) -> Result<T, Failure> {
        let guard = match self.state.lock() {
            Ok(guard) => guard,
            Err(_) => return Err(cancelled()),
        };
        if guard.cancelled {
            return Err(cancelled());
        }
        let result = store();
        drop(guard);
        result
    }
}

use super::{LoginIo, chosen_label, config_failure, credential_key, installed, stored_name, usage};

/// The system browser as `fiber login` runs it: a person is attached,
/// whatever stdin is, since a person ran the command
/// (`docs/model-routing.md`, "Logging in").
pub(crate) struct Attended(Arc<dyn Browser>);

impl Attended {
    /// The system browser with a person attached.
    pub(crate) fn attached() -> Arc<dyn Browser> {
        Arc::new(Self(Arc::new(SystemBrowser::default())))
    }
}

impl Browser for Attended {
    fn open(&self, url: &str) {
        self.0.open(url);
    }

    fn show(&self, url: &str, code: &str) {
        self.0.show(url, code);
    }

    fn attended(&self) -> bool {
        true
    }
}

/// The label the login stores under, or why it stores nothing: a label the
/// flow already stored is refused, naming `--as`
/// (`docs/model-routing.md`, "Logging in").
fn already_stored(name: &str, stored: &str, label: &str) -> String {
    format!(
        "credentials/{stored}/{label} is already stored; log in under another label with --as <label>, or run `fiber logout {name} --as {label}` first."
    )
}

/// Runs the provider `name`'s `credential()` login and stores what it
/// returned under the `--as` label, or the login's email, or `default`.
/// With `--as` an already-stored label is refused before the flow opens
/// anything; without one the check follows the flow under the label's lock.
/// A failed global write deletes the stored file under the held lock, so a
/// retry starts clean. Returns the stored path, such as
/// `credentials/codex/alice@example.com`.
/// The browser login's arguments are one per input the flow reads, so the
/// count stays: a cancel handle joins the home, providers, name, label,
/// method, browser and clock it already took.
#[allow(clippy::too_many_arguments, reason = "one per input the flow reads")]
pub fn browser_login(
    home: &Path,
    providers: &Providers,
    name: &str,
    label: Option<&str>,
    method: LoginMethod,
    browser: Arc<dyn Browser>,
    clock: Arc<dyn Clock>,
    cancel: &LoginCancel,
) -> Result<String, Failure> {
    let data = installed(providers, name)?;
    let stored = stored_name(data);
    if let Some(label) = label
        && read_credential(home, stored, label)
            .map_err(config_failure)?
            .is_some()
    {
        return Err(usage(already_stored(name, stored, label)));
    }
    let provider = login_provider(home, providers, name, browser, clock)
        .map_err(|e| failure(e.code(), e.to_string()))?;
    let stop = {
        let provider = std::sync::Arc::clone(&provider);
        Box::new(move || provider.stop()) as Box<dyn FnOnce() + Send>
    };
    cancel.started(stop)?;
    let logged = provider
        .login(stored, label, method)
        .map_err(|e| failure(e.code(), e.to_string()))?;
    cancel.commit(|| {
        let label = chosen_label(label, logged.email.as_deref());
        let file = CredentialFile::new(home, stored, label).map_err(|_| {
            usage(format!(
                "`{label}` is not a label `fiber login` can store; pass one with `--as <label>`."
            ))
        })?;
        // Held until the store ends, so two logins never both pass the check.
        let Some(lock) = file.try_lock().map_err(config_failure)? else {
            return Err(failure(
                contract::ErrorCode::IoFailed,
                format!("another login for {name} is running"),
            ));
        };
        if read_credential(home, stored, label)
            .map_err(config_failure)?
            .is_some()
        {
            return Err(usage(already_stored(name, stored, label)));
        }
        lock.write(logged.stored.as_value())
            .map_err(config_failure)?;
        if let Err(e) = set_global_if_unset(home, &credential_key(name), label.into()) {
            // A retry must start clean: no file stored without its label.
            delete_credential_held(home, stored, label, &lock).unwrap_or(false);
            return Err(config_failure(e));
        }
        Ok(format!("credentials/{stored}/{label}"))
    })
}

/// Runs the browser login `name` under `label` for `io`, which carries
/// the device flag and the process clock, and prints the one line of the
/// result.
pub(crate) fn login_with(
    io: &mut LoginIo<'_>,
    name: &str,
    label: Option<&str>,
) -> Result<(), Failure> {
    let method = if io.device {
        LoginMethod::Device
    } else {
        LoginMethod::Browser
    };
    let path = browser_login(
        io.home,
        io.providers,
        name,
        label,
        method,
        Attended::attached(),
        Arc::clone(&io.clock),
        &LoginCancel::default(),
    )?;
    std::io::Write::write_fmt(io.err, format_args!("fiber: stored {path}\n"))
        .map_err(super::terminal_failure)
}

#[cfg(test)]
#[path = "browser_tests.rs"]
mod tests;
