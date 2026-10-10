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
    /// out is 1 and every ticket is used once.
    pub(crate) fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Starts a browser login: its ticket, the provider's name and the seam.
pub(crate) struct LoginStart {
    /// The waiting view's ticket: only its events land.
    pub(crate) ticket: LoginTicket,
    /// The provider to log in to.
    pub(crate) name: String,
    /// The seam the login runs through.
    pub(crate) seam: Arc<dyn Configure>,
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
/// out of the waiting mode stores nothing further.
pub(crate) struct LoginWorker {
    ticket: LoginTicket,
    login: Arc<dyn BrowserLogin>,
}

impl std::fmt::Debug for LoginWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LoginWorker({})", self.ticket.0)
    }
}

impl LoginWorker {
    /// The worker over `login`, waiting on `ticket`.
    pub(crate) fn new(ticket: LoginTicket, login: Arc<dyn BrowserLogin>) -> Self {
        Self { ticket, login }
    }

    /// The waiting ticket.
    pub(crate) fn ticket(&self) -> LoginTicket {
        self.ticket
    }
}

impl Drop for LoginWorker {
    fn drop(&mut self) {
        self.login.cancel();
    }
}

/// Starts `start`'s login: `browser_login` returns at once and does no I/O,
/// so it runs here, and `run` blocks on a `tui-login` thread whose end is
/// posted back. A spawn failure is dropped: the view stays waiting and Esc
/// still cancels.
pub(crate) fn start(start: LoginStart, clock: Arc<dyn Clock>, out: Sender<Input>) -> LoginWorker {
    let LoginStart { ticket, name, seam } = start;
    let shown = Arc::new(PostingShow {
        ticket,
        out: out.clone(),
    });
    let login = seam.browser_login(&name, shown, clock);
    let worker = LoginWorker::new(ticket, Arc::clone(&login));
    drop(crate::sources::builder("tui-login").spawn(move || {
        let result = login.run();
        drop(out.send(Input::Login {
            ticket,
            step: LoginStep::Done(result),
        }));
    }));
    worker
}

#[cfg(test)]
#[path = "login_worker_tests.rs"]
mod tests;
