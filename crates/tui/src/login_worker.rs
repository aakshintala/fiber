//! A `/login` browser login's worker (`docs/tui.md`, "Logging in"): the
//! loop starts it after the input that asked, and its progress and its end
//! arrive as `Input::Login` on the existing channel. The worker thread
//! never touches the app; the loop thread never calls `run`.

use std::sync::Arc;
use std::sync::mpsc::Sender;

use contract::clock::Clock;

use crate::Input;
use crate::configure::{BrowserLogin, Configure, LoginShow};

/// A browser login's ticket: only its events land. A newtype so it
/// cannot be confused with an image ticket
/// (`docs/code-quality.md`, "Types").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) struct LoginTicket(pub(crate) u64);

impl LoginTicket {
    /// The next ticket: the counter starts at none, so the first handed
    /// out is 1. Tickets are distinct until the counter saturates at
    /// `u64::MAX`, which is then handed out again for every later start:
    /// a late result on `MAX` can land on a later login waiting on `MAX`,
    /// because admission checks only the ticket and the stage, never the
    /// provider or a generation. Below saturation every start advances.
    pub(crate) fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// What a login start asks the worker to run: a browser login, or only
/// the reviewer read after a key was stored. The reviewer job owns no
/// credential store and no cancel handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginJob {
    /// The provider's `credential()` login, then its reviewer read.
    Browser,
    /// Only the reviewer read for an already stored credential.
    Reviewer,
}

/// Starts a browser login: its ticket, the provider's name and the seam.
pub(crate) struct LoginStart {
    /// The waiting view's ticket: only its events land.
    pub(crate) ticket: LoginTicket,
    /// The provider to log in to.
    pub(crate) name: String,
    /// The seam the login runs through.
    pub(crate) seam: Arc<dyn Configure>,
    /// Whether the worker runs the login, or only the reviewer read.
    pub(crate) job: LoginJob,
}

/// A browser login's progress or its end.
#[derive(Debug)]
pub(crate) enum LoginStep {
    /// The login opened `url`: the loop opens it and the view shows it.
    Open(String),
    /// The login shows the `code` to enter at `url`; opens nothing.
    Code {
        /// The URL to enter the code at.
        url: String,
        /// The device code to enter.
        code: String,
    },
    /// The login ended: what `fiber login` stored.
    Done(Result<crate::configure::Stored, crate::configure::ConfigureError>),
    /// The gaps read ended: the provider's presence-gap placeholders
    /// after a successful `Done`, each with its declared environment
    /// variable. Posted only when `Done` succeeded.
    Host(Result<Vec<crate::configure::HostGap>, crate::configure::ConfigureError>),
    /// The reviewer read ended: the provider's models with the saved
    /// pick preselected, or why they could not be read. Posted after a
    /// successful `Done`, or alone for a reviewer-only job.
    Reviewer(Result<crate::configure::ReviewerModels, crate::configure::ConfigureError>),
}

/// Shows the login's URLs by posting them to the loop: `open` shows `url`
/// and asks the terminal to open it, `show` shows the code and opens
/// nothing.
struct PostingShow {
    ticket: LoginTicket,
    out: Sender<Input>,
}

impl LoginShow for PostingShow {
    fn open(&self, url: &str) {
        drop(self.out.send(Input::Login {
            ticket: self.ticket,
            step: LoginStep::Open(url.to_owned()),
        }));
    }

    fn show(&self, url: &str, code: &str) {
        drop(self.out.send(Input::Login {
            ticket: self.ticket,
            step: LoginStep::Code {
                url: url.to_owned(),
                code: code.to_owned(),
            },
        }));
    }
}

/// A running browser login: dropping it cancels the login, so every way
/// out of the waiting mode stores nothing further. A reviewer-only read
/// holds no login: its bounded callback may finish after dismissal, but
/// its result is then ignored and the VM is released.
pub(crate) struct LoginWorker {
    ticket: LoginTicket,
    login: Option<Arc<dyn BrowserLogin>>,
}

impl std::fmt::Debug for LoginWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LoginWorker({})", self.ticket.0)
    }
}

impl LoginWorker {
    /// The worker over `login`, waiting on `ticket`.
    pub(crate) fn new(ticket: LoginTicket, login: Arc<dyn BrowserLogin>) -> Self {
        Self {
            ticket,
            login: Some(login),
        }
    }

    /// The worker over only a reviewer read, waiting on `ticket`: no
    /// credential store, no cancel handle.
    pub(crate) fn reviewer(ticket: LoginTicket) -> Self {
        Self {
            ticket,
            login: None,
        }
    }

    /// The waiting ticket.
    pub(crate) fn ticket(&self) -> LoginTicket {
        self.ticket
    }
}

impl Drop for LoginWorker {
    fn drop(&mut self) {
        if let Some(login) = &self.login {
            login.cancel();
        }
    }
}

/// Starts `start`'s job: a browser login's `browser_login` returns at
/// once and does no I/O, so it runs here, and `run` blocks on a
/// `tui-login` thread whose end is posted back. A successful browser
/// credential posts its `Done`, then its presence gaps, before the same
/// thread reads the reviewer choices and posts them; a failed one posts
/// only its `Done`. A failed gaps read posts its error between them: the
/// reviewer read still runs. A reviewer-only job reads the gaps, then the
/// choices, with no credential store. A spawn failure is dropped: the
/// view stays waiting and Esc still cancels.
pub(crate) fn start(start: LoginStart, clock: Arc<dyn Clock>, out: Sender<Input>) -> LoginWorker {
    let LoginStart {
        ticket,
        name,
        seam,
        job,
    } = start;
    match job {
        LoginJob::Browser => start_browser(ticket, name, seam, clock, out),
        LoginJob::Reviewer => start_reviewer(ticket, name, seam, clock, out),
    }
}

/// Starts the browser login for `ticket`.
fn start_browser(
    ticket: LoginTicket,
    name: String,
    seam: Arc<dyn Configure>,
    clock: Arc<dyn Clock>,
    out: Sender<Input>,
) -> LoginWorker {
    let shown = Arc::new(PostingShow {
        ticket,
        out: out.clone(),
    });
    let login = seam.browser_login(&name, shown, Arc::clone(&clock));
    let worker = LoginWorker::new(ticket, Arc::clone(&login));
    drop(crate::sources::builder("tui-login").spawn({
        let seam = Arc::clone(&seam);
        move || {
            let result = login.run();
            let stored = result.as_ref().is_ok();
            drop(out.send(Input::Login {
                ticket,
                step: LoginStep::Done(result),
            }));
            if stored {
                let gaps = seam.gaps(&name);
                drop(out.send(Input::Login {
                    ticket,
                    step: LoginStep::Host(gaps),
                }));
                let choices = seam.reviewer_models(&name, clock);
                drop(out.send(Input::Login {
                    ticket,
                    step: LoginStep::Reviewer(choices),
                }));
            }
        }
    }));
    worker
}

/// Starts only the gaps and reviewer reads for `ticket`: no credential
/// store, no cancel handle, so dismissal drops nothing.
fn start_reviewer(
    ticket: LoginTicket,
    name: String,
    seam: Arc<dyn Configure>,
    clock: Arc<dyn Clock>,
    out: Sender<Input>,
) -> LoginWorker {
    let worker = LoginWorker::reviewer(ticket);
    drop(crate::sources::builder("tui-login").spawn(move || {
        let gaps = seam.gaps(&name);
        drop(out.send(Input::Login {
            ticket,
            step: LoginStep::Host(gaps),
        }));
        let choices = seam.reviewer_models(&name, clock);
        drop(out.send(Input::Login {
            ticket,
            step: LoginStep::Reviewer(choices),
        }));
    }));
    worker
}

#[cfg(test)]
#[path = "login_worker_tests.rs"]
mod tests;
