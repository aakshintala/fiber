//! The terminal's launch open (`docs/invocation.md`, "Commands and flags"):
//! the session `fiber resume <id>` and `fiber continue` name, or the
//! session list `fiber resume` with no id names. Both run through the
//! same open path as a row, so a refusal shows on home the same way.

use contract::SessionId;

use super::super::App;
use super::{Opening, subscribe_line};
use crate::OpenAt;
use crate::home::{Level, opening};

impl App {
    /// Opens `session` the way a row opens (`docs/tui.md`, "Home"):
    /// through the hub, so an exited session is resumed by it and a
    /// refusal shows on home. With no level expected, one `full`
    /// subscribe goes out; at `full` a `summary` first, since the hub
    /// resumes an exited session at the level last held and a lone
    /// `full` would be refused as already held. The last subscribe's
    /// acknowledgement ends the gate.
    pub(super) fn open_session(
        &mut self,
        session: SessionId,
        expected: Option<Level>,
    ) -> Vec<String> {
        self.go_home();
        self.attach(session.clone());
        let levels = opening(expected);
        let mut ack = String::new();
        let mut lines = Vec::new();
        for level in levels {
            let (id, line) = subscribe_line(&session, *level);
            if let Some(home) = self.home.as_mut() {
                home.subs.sent(id.clone(), session.clone(), *level);
            }
            ack = id;
            lines.push(line);
        }
        lines.push(self.ask_commands(&session));
        if let Some(home) = self.home.as_mut() {
            home.opening = Some(Opening { session, ack });
        }
        lines
    }

    /// The launch's session open, going out with home's first `feed` and
    /// `recent` once the link is up: one `full` subscribe for the
    /// session and its commands, through the same path as a row.
    /// Anything else, and a second call, sends nothing: the launch opens
    /// at most once. A list launch stays armed until the first `recent`
    /// answer spends it.
    pub(super) fn launch_lines(&mut self) -> Vec<String> {
        let session = match self.home.as_mut().map(|home| &mut home.launch.open_at) {
            Some(open_at @ OpenAt::Session(_)) => {
                let OpenAt::Session(session) = std::mem::replace(open_at, OpenAt::Home) else {
                    return Vec::new();
                };
                session
            }
            Some(OpenAt::Home | OpenAt::List) | None => return Vec::new(),
        };
        self.open_session(session, None)
    }

    /// Folds the launch's list request into the first `recent` answer:
    /// with rows listed, the next frame focuses the list as `/resume`
    /// does, and with none the focus stays in the box. A rejection
    /// leaves the focus in the box too. Either way the request is spent.
    pub(super) fn list_answered(&mut self, first: bool, accepted: bool, listed: bool) {
        let Some(home) = self.home.as_mut() else {
            return;
        };
        if home.launch.open_at != OpenAt::List || !first {
            return;
        }
        home.launch.open_at = OpenAt::Home;
        if accepted && listed {
            home.focus_list = true;
        }
    }
}

#[cfg(test)]
#[path = "app_open_at_tests.rs"]
mod tests;
