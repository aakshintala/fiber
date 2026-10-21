//! Running one provider's `credential()` login for `fiber login`
//! (`docs/model-routing.md`, "Logging in"): the browser or device-code flow
//! of a provider whose data marks it as a browser login, stored under the
//! chosen label's lock after the already-stored check. A login never holds
//! the label's lock while Lua runs: the flow runs first, and the store
//! follows under the lock.

use std::path::Path;
use std::sync::{Arc, Mutex};

use config::read_credential;
use contract::clock::Clock;
use extensions::{Browser, LoginMethod, Providers, SystemBrowser, login_provider};

use crate::CliError;

/// Why a cancelled login fails: it stored nothing.
fn cancelled() -> CliError {
    CliError::LoginCancelled
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
    fn started(&self, stop: Box<dyn FnOnce() + Send>) -> Result<(), CliError> {
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
    fn commit<T>(&self, store: impl FnOnce() -> Result<T, CliError>) -> Result<T, CliError> {
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

use super::{LoginIo, already_stored, chosen_label, installed, store_locked, stored_name};

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
) -> Result<String, CliError> {
    let data = installed(providers, name)?;
    let stored = stored_name(data);
    if let Some(label) = label
        && read_credential(home, stored, label)?.is_some()
    {
        return Err(CliError::StoreRefused {
            refusal: already_stored(name, stored, label),
            hint: true,
        });
    }
    let provider = login_provider(home, providers, name, browser, clock)?;
    let stop = {
        let provider = std::sync::Arc::clone(&provider);
        Box::new(move || provider.stop()) as Box<dyn FnOnce() + Send>
    };
    cancel.started(stop)?;
    let logged = provider.login(stored, label, method)?;
    cancel.commit(|| {
        let label = chosen_label(label, logged.email.as_deref());
        store_locked(home, data, label, |lock| {
            lock.write(logged.stored.as_value())?;
            Ok(())
        })
        // Only the build error maps: every later lock, read, write and
        // global error passes through as store_locked built it.
        .map_err(|error| {
            if matches!(error, CliError::CredentialFile(_)) {
                CliError::LabelNotStorable {
                    label: label.to_owned(),
                }
            } else if let CliError::StoreRefused { refusal, .. } = error {
                CliError::StoreRefused {
                    refusal,
                    hint: true,
                }
            } else {
                error
            }
        })
    })
}

/// Runs the browser login `name` under `label` for `io`, which carries
/// the device flag and the process clock, and prints the one line of the
/// result.
pub(crate) fn login_with(
    io: &mut LoginIo<'_>,
    name: &str,
    label: Option<&str>,
) -> Result<(), CliError> {
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
    // The same terminal-only question a key login asks: a pipe stores
    // the credential, prints one remedy line per presence gap, and asks
    // nothing. Cancellation arms before the stored-line readiness output,
    // as in key login.
    if io.terminal {
        super::host::hosts(io, name)?;
        super::post_store_reviewer(io, name, &format!("fiber: stored {path}"))?;
    } else {
        super::host::hosts(io, name)?;
        std::io::Write::write_fmt(io.err, format_args!("fiber: stored {path}\n"))
            .map_err(CliError::Terminal)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "browser_tests.rs"]
mod tests;
