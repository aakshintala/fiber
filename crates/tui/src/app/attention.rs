//! The attention state on the app: what the hub's `attention` lines queued
//! (`docs/tui.md`, "Getting the person's attention").

use serde_json::{Map, Value};

use contract::SessionId;

use super::App;
use crate::Attention;
use crate::home::State as HomeState;

/// What the app holds for the hub's `attention` lines.
#[derive(Debug, Default)]
pub(super) struct State {
    /// Whether the terminal supports OSC 9, decided once at start.
    osc9: bool,
    /// The bytes the lines queued, written to the tty after the frame.
    out: Vec<u8>,
    /// The latest `attention` line, for the window title.
    latest: Option<Latest>,
}

/// The latest `attention` line, as the window title shows it (`docs/tui.md`,
/// "Getting the person's attention").
#[derive(Debug)]
enum Latest {
    /// A session waiting on the person, and whether its feed row has been
    /// observed waiting: the hub writes `attention` lines on a different
    /// path from the feed, so the line can arrive before the row, or while
    /// the row still shows an earlier state.
    Waiting { session: SessionId, seen: bool },
    /// A session whose turn finished.
    Finished,
}

impl App {
    /// Records whether the terminal supports OSC 9, decided once at start
    /// from the environment (`docs/tui.md`, "Getting the person's
    /// attention").
    pub(crate) fn set_osc9(&mut self, osc9: bool) {
        self.attention.osc9 = osc9;
    }

    /// Folds one `attention` line's payload: its bytes queue for the tty
    /// after the frame, and it becomes the latest for the window title
    /// (`docs/tui.md`, "Getting the person's attention"). A line that
    /// does not parse queues nothing and changes no title.
    pub(super) fn attention_line(&mut self, payload: &Map<String, Value>) {
        if let Some(line) = crate::attention::parse(payload) {
            let settings = self.attention_settings();
            let bytes = crate::attention::bytes(&line, settings, self.attention.osc9);
            self.attention.out.extend_from_slice(&bytes);
            self.attention.latest = Some(match line.reason {
                crate::attention::Reason::Waiting { .. } => {
                    let seen = self
                        .home
                        .as_ref()
                        .and_then(|home| home.sessions.row(&line.session))
                        .is_some_and(|row| row.left.is_none() && row.state == HomeState::Waiting);
                    Latest::Waiting {
                        session: line.session,
                        seen,
                    }
                }
                crate::attention::Reason::Finished => Latest::Finished,
            });
        }
    }

    /// Takes the attention bytes queued since the last frame.
    pub(crate) fn take_alerts(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.attention.out)
    }

    /// The attention settings from home's launch description, or the
    /// defaults without home state (`docs/tui.md`, "Getting the person's
    /// attention").
    fn attention_settings(&self) -> Attention {
        self.home
            .as_ref()
            .map(|home| home.launch.attention)
            .unwrap_or_default()
    }

    /// The window title for the latest `attention` line (`docs/tui.md`,
    /// "Getting the person's attention"): `None` with the title off or
    /// with no attention, else worked out from the current feed row every
    /// time it is drawn.
    pub(crate) fn attention_title(&self) -> Option<String> {
        if !self.attention_settings().title {
            return None;
        }
        match &self.attention.latest {
            None => None,
            Some(Latest::Waiting { session, seen }) => self.waiting_title(session, *seen),
            Some(Latest::Finished) => Some("✓ fiber · finished".to_owned()),
        }
    }

    /// Folds the feed rows into the latest waiting `attention` line
    /// (`docs/tui.md`, "Getting the person's attention"): the first row
    /// observed waiting for the session marks it seen, and once seen, a row
    /// that is gone, left, or no longer waiting clears the title, so it
    /// never returns when the id waits again without a new line. A session
    /// with no row yet, or a row not yet waiting, still awaits its status. Runs after every hub line, which is where
    /// every row change folds in.
    pub(super) fn reconcile_attention(&mut self) {
        crate::work::add(|work| work.reconcile_attention += 1);
        let session = match &self.attention.latest {
            Some(Latest::Waiting { session, .. }) => session.clone(),
            Some(Latest::Finished) | None => return,
        };
        // Whether a row is listed, and whether it still waits: copied out
        // so the row's borrow ends before the title clears below.
        let (present, waiting) = match self
            .home
            .as_ref()
            .and_then(|home| home.sessions.row(&session))
        {
            None => (false, false),
            Some(row) => (true, row.left.is_none() && row.state == HomeState::Waiting),
        };
        if !present {
            let seen = matches!(
                &self.attention.latest,
                Some(Latest::Waiting { seen: true, .. })
            );
            if seen {
                self.attention.latest = None;
            }
            return;
        }
        if waiting {
            if let Some(Latest::Waiting { seen, .. }) = self.attention.latest.as_mut() {
                *seen = true;
            }
        } else if matches!(
            &self.attention.latest,
            Some(Latest::Waiting { seen: true, .. })
        ) {
            self.attention.latest = None;
        }
    }

    /// The window title while `session` waits on the person: `! fiber ·
    /// <kind>` while its feed row still waits, else `None` for the normal
    /// title. A session with no row yet still waits; one whose row was
    /// already seen stays cleared, and only a new `attention` line brings
    /// the title back.
    fn waiting_title(&self, session: &SessionId, seen: bool) -> Option<String> {
        let kind = match self
            .home
            .as_ref()
            .and_then(|home| home.sessions.row(session))
        {
            None if seen => return None,
            None => "waiting",
            Some(row) if row.left.is_none() && row.state == HomeState::Waiting => {
                waiting_kind(row.waiting.as_deref())
            }
            Some(_) => return None,
        };
        Some(format!("! fiber · {kind}"))
    }

    /// The person's stroke or left press saw the attention: it ends a
    /// finished title, never a waiting one (`docs/tui.md`, "Getting the
    /// person's attention").
    pub(crate) fn attention_seen(&mut self) {
        if matches!(self.attention.latest, Some(Latest::Finished)) {
            self.attention.latest = None;
        }
    }
}

/// The waiting kind a feed row's `waiting` text names (`docs/tui.md`,
/// "Getting the person's attention"): the text reads `"<kind>:
/// <summary>"`, where `approval` and `question` name themselves and
/// anything else reads as waiting.
fn waiting_kind(waiting: Option<&str>) -> &'static str {
    match waiting.and_then(|waiting| waiting.split(':').next()) {
        Some("approval") => "approval",
        Some("question") => "question",
        _ => "waiting",
    }
}

#[cfg(test)]
#[path = "attention_tests.rs"]
mod tests;
