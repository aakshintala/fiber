//! The rewind view's send state (`docs/tui.md`, "Rewind"): the `rewind`
//! request on the wire, its refusal, and the summary checkbox.
//!
//! Owns its state; the parent coordinates it through the methods below.
//! The `App` seam sending the `rewind` and handling its keys and clicks
//! reaches the fold, the cursor, and the send state only through the
//! parent's coordinators.

use std::time::Instant;

use contract::commands::RewindArgs;
use contract::events::{CommandAccepted, CommandRejected, CommandResult};
use contract::{CommandId, Envelope, ErrorCode, Seq};
use serde_json::Value;

use super::Rewind;
use super::view::{EnterAction, Vis};
use crate::app::{App, Effect, history_args, mint, session_command};
use crate::home::Level;
use crate::keys::Key;
use crate::link::history_answer;
use crate::swapped::Spot;

/// A `rewind` request waiting for its answer.
#[derive(Debug)]
struct SendPending {
    /// The command id the answer names.
    id: String,
}

/// How the Rewind row draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// The last refusal's text stands in the button's place.
    Refusal(String),
    /// A `rewind` waits for its answer.
    Sending,
    /// No turns to rewind from.
    Disabled,
    /// Ready to send.
    Ready,
}

/// The `rewind` send state: the request on the wire, its refusal, and
/// the summary checkbox.
#[derive(Debug, Default)]
pub(crate) struct Send {
    /// The `rewind` request on the wire, if any.
    send: Option<SendPending>,
    /// Why the last `rewind` failed, standing in place of the button.
    send_error: Option<String>,
    /// The standing error is a `summary_failed`: its three choices draw
    /// under the Rewind row.
    summary_failed: bool,
    /// Whether the rewind summarises the path after the point.
    summarise: bool,
}

impl Send {
    /// Whether a `rewind` may go out: none on the wire and no refusal
    /// standing.
    pub(crate) fn can_send(&self) -> bool {
        self.send.is_none() && self.send_error.is_none()
    }

    /// Whether a `rewind` waits for its answer.
    pub(crate) fn is_sending(&self) -> bool {
        self.send.is_some()
    }

    /// Whether the `summary_failed` choices draw.
    pub(crate) fn choices_shown(&self) -> bool {
        self.summary_failed
    }

    /// Whether the summarise checkbox is on.
    pub(crate) fn summarise(&self) -> bool {
        self.summarise
    }

    /// Flips the summarise checkbox.
    pub(crate) fn toggle_summarise(&mut self) {
        self.summarise = !self.summarise;
    }

    /// Sets the summarise checkbox, returning what it was.
    pub(crate) fn swap_summarise(&mut self, summarise: bool) -> bool {
        std::mem::replace(&mut self.summarise, summarise)
    }

    /// Whether `id` is the `rewind` on the wire.
    pub(crate) fn matches(&self, id: &str) -> bool {
        self.send.as_ref().is_some_and(|send| send.id == id)
    }

    /// Notes the `rewind` request on the wire.
    pub(crate) fn begin(&mut self, id: String) {
        self.send = Some(SendPending { id });
    }

    /// Settles the sent `rewind` as refused with the start's message.
    pub(crate) fn refuse(&mut self, message: String, summary_failed: bool) {
        self.send = None;
        self.send_error = Some(message);
        self.summary_failed = summary_failed;
    }

    /// Clears the standing refusal with its choices, if any.
    pub(crate) fn clear_refusal(&mut self) {
        self.send_error = None;
        self.summary_failed = false;
    }

    /// How the Rewind row draws for a view holding turns or not.
    pub(crate) fn action(&self, has_turns: bool) -> Action {
        if let Some(error) = &self.send_error {
            Action::Refusal(error.clone())
        } else if self.send.is_some() {
            Action::Sending
        } else if !has_turns {
            Action::Disabled
        } else {
            Action::Ready
        }
    }
}

impl App {
    /// Opens the rewind view on the attached session and sends its
    /// first `history`: the walk pages older segments as their answers
    /// land, at most one request on the wire. The view opens before
    /// this runs.
    pub(in crate::app) fn open_rewind(&mut self) -> Effect {
        let Some(session) = self.session().cloned() else {
            return Effect::None;
        };
        self.rewind = Rewind::open(session.clone());
        let id = mint();
        let line = session_command(
            &id,
            "history",
            &session,
            Some(history_args(&session, &session, None, None)),
        );
        self.rewind
            .begin_wait(id.clone(), self.motion().now_instant());
        Effect::Send(vec![line.to_string()])
    }

    /// Folds a `command_accepted` into the open rewind view when it
    /// answers the pending `history`: a late answer, or another view's,
    /// matches no pending id and folds nowhere. Answers the next page
    /// while the chain runs on, so one request stays on the wire.
    pub(crate) fn rewind_answered(
        &mut self,
        envelope: &Envelope,
        accepted: &CommandAccepted,
    ) -> Vec<String> {
        if !self.rewind.take_answer_slot(&accepted.command_id.0) {
            return Vec::new();
        }
        match history_answer(envelope) {
            Err(message) => self.rewind.fail_walk(message),
            Ok(older) => match self.rewind.append_page(older) {
                Err(()) => {}
                Ok((at, rows)) => self.rewind_list_to(at, rows),
            },
        }
        if self.rewind.chain_done() {
            return Vec::new();
        }
        let Some(args) = self.rewind.next_args() else {
            return Vec::new();
        };
        let Some(session) = self.session().cloned() else {
            return Vec::new();
        };
        let id = mint();
        let line = session_command(&id, "history", &session, Some(args));
        self.rewind
            .begin_wait(id.clone(), self.motion().now_instant());
        vec![line.to_string()]
    }

    /// A `command_rejected` for a pending request settles it with the
    /// rejection's text: the walk ends for `history`, the button shows
    /// it for `rewind`, with its three choices for a `summary_failed`.
    /// Any other id reaches no rewind state.
    pub(in crate::app) fn rewind_refused(&mut self, rejected: &CommandRejected) {
        let Some(id) = rejected.command_id.as_ref() else {
            return;
        };
        if self.rewind.take_answer_slot(&id.0) {
            self.rewind.fail_walk(rejected.message.clone());
        } else if self.rewind.send_matches(&id.0) {
            self.rewind.refuse_send(
                rejected.message.clone(),
                rejected.code == ErrorCode::SummaryFailed,
            );
        }
    }

    /// A hub-relayed `command_rejected` for the sent `rewind` settles
    /// it with the start's code: the terminal never moves. True when
    /// the id was the send's; any other id reaches no rewind state.
    pub(in crate::app) fn rewind_hub_refused(
        &mut self,
        id: &str,
        code: &str,
        message: &str,
    ) -> bool {
        if !self.rewind.send_matches(id) {
            return false;
        }
        // The hub carries the code as text; an unknown one still
        // settles, read as itself.
        let code = serde_json::from_value(Value::String(code.to_owned()))
            .unwrap_or_else(|_| ErrorCode::Other(code.to_owned()));
        self.rewind_refused(&CommandRejected {
            command_id: Some(CommandId(id.to_owned())),
            code,
            message: message.to_owned(),
        });
        true
    }

    /// A `command_accepted` answering the sent `rewind` with its new
    /// session subscribes full to it, asks its commands, attaches, and
    /// closes the view. The reset attachment settles the send. Any
    /// other id, or another command's shape, falls through to the
    /// history path.
    pub(in crate::app) fn rewind_send_accepted(
        &mut self,
        accepted: &CommandAccepted,
    ) -> Option<Vec<String>> {
        if !self.rewind.confirm_send(&accepted.command_id.0) {
            return None;
        }
        let Some(CommandResult::Rewind { new_session_id }) = accepted.result.as_ref() else {
            return None;
        };
        let new = new_session_id.clone();
        // A new attachment asks the new session's commands, as a clear
        // `start` accept does for its session.
        let lines = vec![self.subscribe(&new, Level::Full), self.ask_commands(&new)];
        self.attach(new);
        self.close_session_view();
        Some(lines)
    }

    /// The connection ended: the pending `history` drops, and its late
    /// answer folds nowhere.
    pub(in crate::app) fn rewind_lost(&mut self) {
        self.rewind.abort_wait();
    }

    /// The stream reopened: a `history` wait ends at once, and its late
    /// answer folds nowhere.
    pub(in crate::app) fn rewind_reopened(&mut self) {
        self.rewind.abort_wait();
    }

    /// The pending `history`'s deadline: what the frame wakes for.
    pub(in crate::app) fn rewind_wake(&self) -> Option<Instant> {
        self.rewind.wait_deadline()
    }

    /// Whether the pending `history` wait passed its bound: at or past
    /// ten seconds on checked arithmetic, failing closed when the bound
    /// is unrepresentable.
    pub(crate) fn rewind_due(&mut self, now: Instant) -> bool {
        self.rewind.wait_due(now)
    }

    /// The view's rows and its height: what the cursor moves over.
    fn rewind_metrics(&self) -> Option<(usize, usize)> {
        let frame = self.session_view_screen(0)?;
        Some((
            frame.rows.len(),
            crate::swapped::rows_height(&frame, self.conversation_height()),
        ))
    }

    /// Every job that stops with the rewind: the running jobs beside
    /// the running delegates, a delegate listed like other jobs.
    pub(in crate::app) fn rewind_stopping(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .panel_state
            .running_jobs()
            .into_iter()
            .map(|(_, description)| description.to_owned())
            .collect();
        out.extend(
            self.panel_state
                .running_delegates()
                .into_iter()
                .map(|(_, description)| description.to_owned()),
        );
        out
    }

    /// Handles the open rewind view's keys: moving, opening turns,
    /// sending from a boundary or the Rewind row, toggling the summary
    /// checkbox, and Esc, which closes the view when no `rewind` is
    /// pending and is ignored while one is.
    pub(in crate::app) fn rewind_key(&mut self, key: &Key) -> Option<Effect> {
        if *key == Key::Esc {
            if self.rewind.is_sending() {
                return Some(Effect::None);
            }
            self.close_session_view();
            return Some(Effect::None);
        }
        if *key == Key::Enter {
            return Some(self.rewind_enter());
        }
        if *key == Key::Char(' ') {
            self.rewind.toggle_summarise();
            return Some(Effect::None);
        }
        let Some((rows, height)) = self.rewind_metrics() else {
            return Some(Effect::None);
        };
        let (before, after) = match self.rewind_list_mut() {
            Some(list) => {
                let before = list.selected();
                list.key(key, rows, height);
                (before, list.selected())
            }
            None => return Some(Effect::None),
        };
        if after != before {
            self.rewind.follow_selection(after);
        }
        Some(Effect::None)
    }

    /// Handles the open rewind view's clicks: closing, selecting,
    /// toggling the checkbox, and sending from the Rewind row.
    pub(in crate::app) fn rewind_click(&mut self, spot: Spot) -> Effect {
        match spot {
            Spot::Close => {
                if self.rewind.is_sending() {
                    return Effect::None;
                }
                self.close_session_view();
            }
            Spot::Row(at) => {
                self.rewind.select_at(at);
                let (at, rows) = self.rewind.reseat_at(at);
                self.rewind_list_to(at, rows);
            }
            Spot::Cell(at, _) => match self.rewind.visible_row(at) {
                Some(Vis::Check) => self.rewind.toggle_summarise(),
                Some(Vis::Action) => return self.send_rewind(),
                Some(Vis::Retry) => return self.send_rewind_as(true),
                Some(Vis::NoSummary) => return self.send_rewind_as(false),
                Some(Vis::Cancel) => self.rewind.clear_refusal(),
                _ => self.rewind.select_at(at),
            },
            Spot::Switch { .. } | Spot::Revoke(_) | Spot::Item(_) => {}
        }
        Effect::None
    }

    /// Enter on the cursor's row: opens or closes a turn, toggles the
    /// checkbox, or sends the rewind from the Rewind row.
    fn rewind_enter(&mut self) -> Effect {
        let at = self
            .rewind_list_mut()
            .map(|list| list.selected())
            .unwrap_or(0);
        match self.rewind.enter(at) {
            EnterAction::Reseat => {
                let (at, rows) = self.rewind.reseat();
                self.rewind_list_to(at, rows);
            }
            EnterAction::Send => return self.send_rewind(),
            EnterAction::SendAs(summarise) => return self.send_rewind_as(summarise),
            EnterAction::None => {}
        }
        Effect::None
    }

    /// Sends the rewind for the chosen point: `from_session_id` names
    /// the holding session only past the attached one, `summarise`
    /// rides the checkbox. One request at a time, and none with no
    /// point, no session, a wait on the wire, or a refusal standing.
    fn send_rewind(&mut self) -> Effect {
        // No point, no session, a wait on the wire, or a refusal
        // standing: with no turns there is no point either, so the
        // point check disables the empty view.
        if !self.rewind.can_send() {
            return Effect::None;
        }
        let Some((session, seq)) = self.rewind.point() else {
            return Effect::None;
        };
        let Some(serving) = self.session().cloned() else {
            return Effect::None;
        };
        let from = if session == serving {
            None
        } else {
            Some(session)
        };
        let args = serde_json::to_value(RewindArgs {
            from_session_id: from,
            seq: Some(Seq(seq)),
            summarise: self.rewind.summarise(),
        })
        .unwrap_or(Value::Null);
        let id = mint();
        let line = session_command(&id, "rewind", &serving, Some(args));
        self.rewind.begin_send(id.clone());
        Effect::Send(vec![line.to_string()])
    }

    /// Sends a fresh `rewind` for the chosen point with the summary
    /// flag the choice names, clearing the standing refusal first: a
    /// retry keeps the summary on, a rewind without summary turns it
    /// off, whatever the checkbox says.
    fn send_rewind_as(&mut self, summarise: bool) -> Effect {
        if self.rewind.is_sending() {
            return Effect::None;
        }
        self.rewind.clear_refusal();
        let saved = self.rewind.swap_summarise(summarise);
        let out = self.send_rewind();
        self.rewind.swap_summarise(saved);
        out
    }

    /// One turn's step boundaries, oldest first, as holding sessions
    /// and seqs. Tests only: production reads the fold through the
    /// frame.
    #[cfg(test)]
    pub(crate) fn rewind_turn_bounds(&self, at: usize) -> Vec<(String, u64)> {
        self.rewind.turn_bounds(at)
    }
}
