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

use super::Rewind;
use super::view::{EnterAction, Vis};
use crate::app::{App, Effect, Waiting, history_args, session_line};
use crate::keys::Key;
use crate::link::history_answer;
use crate::swapped::Spot;
use contract::commands::Command;
use contract::commands::SubscribeLevel;

/// A `rewind` request waiting for its answer. The outbox `RewindSend`
/// tag is the sole record of its command id.
#[derive(Debug)]
struct SendPending;

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
pub(super) struct Send {
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
    fn can_send(&self) -> bool {
        self.send.is_none() && self.send_error.is_none()
    }

    /// Whether a `rewind` waits for its answer.
    fn is_sending(&self) -> bool {
        self.send.is_some()
    }

    /// Whether the `summary_failed` choices draw.
    pub(super) fn choices_shown(&self) -> bool {
        self.summary_failed
    }

    /// Whether the summarise checkbox is on.
    pub(super) fn summarise(&self) -> bool {
        self.summarise
    }

    /// Flips the summarise checkbox.
    pub(super) fn toggle_summarise(&mut self) {
        self.summarise = !self.summarise;
    }

    /// Sets the summarise checkbox.
    fn set_summarise(&mut self, summarise: bool) {
        self.summarise = summarise;
    }

    /// Whether a `rewind` is on the wire. The outbox `RewindSend` tag
    /// is the sole record of its command id.
    pub(super) fn matches(&self) -> bool {
        self.send.is_some()
    }
    // debt: matches() duplicates is_sending(); PR #4204's merge frees the shrink to is_sending().

    /// Notes the `rewind` request on the wire.
    pub(super) fn begin(&mut self) {
        self.send = Some(SendPending);
    }

    /// Settles the sent `rewind` as refused with the start's message.
    pub(super) fn refuse(&mut self, message: String, summary_failed: bool) {
        self.send = None;
        self.send_error = Some(message);
        self.summary_failed = summary_failed;
    }

    /// Clears the standing refusal with its choices, if any.
    pub(super) fn clear_refusal(&mut self) {
        self.send_error = None;
        self.summary_failed = false;
    }

    /// How the Rewind row draws for a view holding turns or not.
    pub(super) fn action(&self, has_turns: bool) -> Action {
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
        // A reopen orphans the old view's tags: forget them before the
        // new view registers its own, so a late answer for the old view
        // folds nowhere and leaves the new wait alone.
        self.outbox.forget(&Waiting::RewindHistory);
        self.outbox.forget(&Waiting::RewindSend);
        self.rewind = Rewind::open(session.clone());
        let id = support::mint("c_");
        let line = session_line(
            &contract::CommandId(id.clone()),
            &session,
            Command::History(history_args(&session, &session, None, None)),
        );
        self.outbox.wait(&id, Waiting::RewindHistory);
        self.rewind.fold.begin_wait(self.motion().now_instant());
        Effect::Send(vec![line])
    }

    /// Folds a `command_accepted` into the open rewind view when it
    /// answers the pending `history`: a late answer, or another view's,
    /// names no waiting tag and folds nowhere. Answers the next page
    /// while the chain runs on, so one request stays on the wire.
    pub(crate) fn rewind_answered(
        &mut self,
        envelope: &Envelope,
        accepted: &CommandAccepted,
    ) -> Vec<String> {
        if self.outbox.waiting(&accepted.command_id.0) != Some(Waiting::RewindHistory) {
            return Vec::new();
        }
        if !self.rewind.fold.take_wait() {
            return Vec::new();
        }
        self.outbox.settled(&accepted.command_id.0);
        match history_answer(envelope) {
            Err(message) => self.rewind.fold.fail_walk(message),
            Ok(older) => match self.rewind.append_page(older) {
                Err(()) => {}
                Ok((at, rows)) => self.rewind_list_to(at, rows),
            },
        }
        if self.rewind.fold.chain_done() {
            return Vec::new();
        }
        let Some(args) = self.rewind.fold.next_args() else {
            return Vec::new();
        };
        let Some(session) = self.session().cloned() else {
            return Vec::new();
        };
        let id = support::mint("c_");
        let line = session_line(
            &contract::CommandId(id.clone()),
            &session,
            Command::History(args),
        );
        self.outbox.wait(&id, Waiting::RewindHistory);
        self.rewind.fold.begin_wait(self.motion().now_instant());
        vec![line]
    }

    /// A `command_rejected` for a pending request settles it with the
    /// rejection's text: the walk ends for `history`, the button shows
    /// it for `rewind`, with its three choices for a `summary_failed`.
    /// Any other id reaches no rewind state.
    pub(in crate::app) fn rewind_refused(&mut self, rejected: &CommandRejected) {
        let Some(id) = rejected.command_id.as_ref() else {
            return;
        };
        let waiting = self.outbox.waiting(&id.0);
        if waiting == Some(Waiting::RewindHistory) && self.rewind.fold.take_wait() {
            self.outbox.settled(&id.0);
            self.rewind.fold.fail_walk(rejected.message.clone());
        } else if waiting == Some(Waiting::RewindSend) && self.rewind.send.matches() {
            self.outbox.settled(&id.0);
            self.rewind.send.refuse(
                rejected.message.clone(),
                rejected.code == ErrorCode::SummaryFailed,
            );
        }
    }

    /// A hub-relayed `command_rejected` for the waiting `rewind` or
    /// `/fork` settles it with the start's code: the terminal never
    /// moves. A `/fork` settles as a notice, even when another view is
    /// open. True when the wait owns the refusal.
    pub(in crate::app) fn rewind_hub_refused(
        &mut self,
        id: &str,
        code: ErrorCode,
        message: &str,
        waiting: &Waiting,
    ) -> bool {
        // The hub carries the code as text; an unknown one still
        // settles, read as itself.
        match waiting {
            Waiting::Fork => {
                self.settle_fork_refusal(id, &code, message);
                true
            }
            Waiting::RewindSend => {
                if !self.rewind.send.matches() {
                    return false;
                }
                self.rewind_refused(&CommandRejected {
                    command_id: Some(CommandId(id.to_owned())),
                    code,
                    message: message.to_owned(),
                });
                true
            }
            Waiting::Find
            | Waiting::Commands
            | Waiting::Model
            | Waiting::Reply
            | Waiting::OfferFile
            | Waiting::Steer
            | Waiting::Shell => false,
            Waiting::Dismiss(_)
            | Waiting::RewindHistory
            | Waiting::ChangeWorkspace(_)
            | Waiting::ClearStart
            | Waiting::ClearClose
            | Waiting::History
            | Waiting::ImageFetch { .. }
            | Waiting::ImageDisplay { .. }
            | Waiting::Sessions
            | Waiting::Abandoned
            | Waiting::Directories(..)
            | Waiting::Subscribe { .. }
            | Waiting::Stop
            | Waiting::Delete
            | Waiting::OfferImport
            | Waiting::DeleteDiscovery(..)
            | Waiting::DeleteStop { .. }
            | Waiting::DeleteFinal(..)
            | Waiting::ArchiveDiscovery(..)
            | Waiting::ArchiveStop { .. }
            | Waiting::ArchiveFinal(..)
            | Waiting::Feed
            | Waiting::Recent(..)
            | Waiting::Close => false,
            // The session views and panels own no rewind refusal.
            Waiting::Figures
            | Waiting::Diff
            | Waiting::Hooks
            | Waiting::Branch
            | Waiting::Quota
            | Waiting::ArtifactPage
            | Waiting::ConfigAsk
            | Waiting::Credentials
            | Waiting::CredentialSwitch
            | Waiting::JobStop
            | Waiting::JobInput => false,
        }
    }

    /// A `command_accepted` answering the waiting `rewind` or `/fork`
    /// with its new session subscribes full to it, asks its commands,
    /// attaches, and closes the view. The reset attachment settles the
    /// send. Another command's shape falls through to the history path.
    /// A `/fork` attaches the same way: its id lives beside the rewind
    /// view's state, so it still matches after opening another view
    /// reset that state.
    pub(in crate::app) fn rewind_send_accepted(
        &mut self,
        accepted: &CommandAccepted,
        waiting: &Waiting,
    ) -> Option<Vec<String>> {
        match waiting {
            Waiting::Fork => {}
            Waiting::Find
            | Waiting::Commands
            | Waiting::Model
            | Waiting::Reply
            | Waiting::OfferFile
            | Waiting::Steer
            | Waiting::Shell => return None,
            Waiting::RewindSend => {
                if !self.rewind.send.matches() {
                    return None;
                }
            }
            Waiting::Dismiss(_)
            | Waiting::RewindHistory
            | Waiting::ChangeWorkspace(_)
            | Waiting::ClearStart
            | Waiting::ClearClose
            | Waiting::History
            | Waiting::ImageFetch { .. }
            | Waiting::ImageDisplay { .. }
            | Waiting::Sessions
            | Waiting::Abandoned
            | Waiting::Directories(..)
            | Waiting::Subscribe { .. }
            | Waiting::Stop
            | Waiting::Delete
            | Waiting::OfferImport
            | Waiting::DeleteDiscovery(..)
            | Waiting::DeleteStop { .. }
            | Waiting::DeleteFinal(..)
            | Waiting::ArchiveDiscovery(..)
            | Waiting::ArchiveStop { .. }
            | Waiting::ArchiveFinal(..)
            | Waiting::Feed
            | Waiting::Recent(..)
            | Waiting::Close => return None,
            // The session views and panels own no rewind answer.
            Waiting::Figures
            | Waiting::Diff
            | Waiting::Hooks
            | Waiting::Branch
            | Waiting::Quota
            | Waiting::ArtifactPage
            | Waiting::ConfigAsk
            | Waiting::Credentials
            | Waiting::CredentialSwitch
            | Waiting::JobStop
            | Waiting::JobInput => return None,
        }
        let Some(CommandResult::Rewind { new_session_id }) = accepted.result.as_ref() else {
            return None;
        };
        let new = new_session_id.clone();
        // A new attachment asks the new session's commands, as a clear
        // `start` accept does for its session.
        let lines = vec![
            self.subscribe(&new, SubscribeLevel::Full),
            self.ask_commands(&new),
        ];
        self.attach(new);
        self.close_session_view();
        self.outbox.settled(&accepted.command_id.0);
        Some(lines)
    }

    /// Sends `/fork`'s `rewind` with `at_end`: no view opens and no
    /// cursor point is read. The pending id lives beside the rewind
    /// view's state, so opening another view resets that state without
    /// dropping the fork. One fork at a time: a second `/fork` while
    /// one waits sends nothing.
    pub(in crate::app) fn send_fork(&mut self) -> Effect {
        if self.outbox.waiting_any(&Waiting::Fork) {
            return Effect::None;
        }
        let Some(serving) = self.session().cloned() else {
            return Effect::None;
        };
        let id = support::mint("c_");
        let line = session_line(
            &contract::CommandId(id.clone()),
            &serving,
            Command::Rewind(RewindArgs::AtEnd),
        );
        self.outbox.wait(&id, Waiting::Fork);
        Effect::Send(vec![line])
    }

    /// The ruled `/fork` refusal for an end-of-log point that is not a
    /// step boundary: the session ends in a tool call with no recorded
    /// result.
    const FORK_NOT_STEP_BOUNDARY: &str = "The session ends in a tool call with no recorded result, so it cannot be forked. Rewind to an earlier point instead.";

    /// A `command_rejected` answering the waiting `/fork` shows its
    /// refusal as a notice and clears the fork, so retrying sends
    /// again: the ruled text for `not_step_boundary`, the rejection's
    /// own text for any other code. The router matches `Waiting::Fork`
    /// before calling.
    pub(in crate::app) fn rewind_fork_refused(&mut self, rejected: &CommandRejected) {
        let Some(id) = rejected.command_id.as_ref() else {
            return;
        };
        self.settle_fork_refusal(&id.0, &rejected.code, &rejected.message);
    }

    /// Clears a pending `/fork` and shows its refusal as a notice: the
    /// ruled text for `not_step_boundary`, the rejection's own text for
    /// any other code. Both the session and hub refusal paths settle
    /// through here, so retrying sends again either way.
    fn settle_fork_refusal(&mut self, id: &str, code: &ErrorCode, message: &str) {
        self.outbox.settled(id);
        if *code == ErrorCode::NotStepBoundary {
            self.push_notice(
                ErrorCode::NotStepBoundary,
                Self::FORK_NOT_STEP_BOUNDARY.to_owned(),
            );
        } else {
            self.push_notice(code.clone(), message.to_owned());
        }
    }

    /// The connection ended: the pending `history` drops, and its late
    /// answer folds nowhere.
    pub(in crate::app) fn rewind_lost(&mut self) {
        self.outbox.forget(&Waiting::RewindHistory);
        self.rewind.fold.abort_wait();
    }

    /// The stream reopened: a `history` wait ends at once, and its late
    /// answer folds nowhere.
    pub(in crate::app) fn rewind_reopened(&mut self) {
        self.outbox.forget(&Waiting::RewindHistory);
        self.rewind.fold.abort_wait();
    }

    /// The pending `history`'s deadline: what the frame wakes for.
    pub(in crate::app) fn rewind_wake(&self) -> Option<Instant> {
        self.rewind.fold.wait_deadline()
    }

    /// Whether the pending `history` wait passed its bound: at or past
    /// ten seconds on checked arithmetic, failing closed when the bound
    /// is unrepresentable.
    pub(crate) fn rewind_due(&mut self, now: Instant) -> bool {
        self.rewind.fold.wait_due(now)
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
            if self.rewind.send.is_sending() {
                return Some(Effect::None);
            }
            self.close_session_view();
            return Some(Effect::None);
        }
        if *key == Key::Enter {
            return Some(self.rewind_enter());
        }
        if *key == Key::Char(' ') {
            self.rewind.send.toggle_summarise();
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

    /// Shifts the open rewind view's list one wheel step: three rows,
    /// clamped at either end, keeping the cursor while it shows. A
    /// moved cursor follows the content rows as the keys do. Never an
    /// Up/Down step: the keys keep their own path.
    pub(in crate::app) fn rewind_wheel(&mut self, up: bool) -> Effect {
        let Some((rows, height)) = self.rewind_metrics() else {
            return Effect::None;
        };
        let (before, after) = match self.rewind_list_mut() {
            Some(list) => {
                let before = list.selected();
                list.wheel(up, rows, height, 3);
                (before, list.selected())
            }
            None => return Effect::None,
        };
        if after != before {
            self.rewind.follow_selection(after);
        }
        Effect::None
    }

    /// Handles the open rewind view's clicks: closing, selecting,
    /// toggling the checkbox, and sending from the Rewind row.
    pub(in crate::app) fn rewind_click(&mut self, spot: Spot) -> Effect {
        match spot {
            Spot::Close => {
                if self.rewind.send.is_sending() {
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
                Some(Vis::Check) => self.rewind.send.toggle_summarise(),
                Some(Vis::Action) => return self.send_rewind(),
                Some(Vis::Retry) => return self.send_rewind_as(true),
                Some(Vis::NoSummary) => return self.send_rewind_as(false),
                Some(Vis::Cancel) => self.rewind.send.clear_refusal(),
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
        if !self.rewind.send.can_send() {
            return Effect::None;
        }
        let Some((session, seq)) = self.rewind.cursor.point() else {
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
        let args = RewindArgs::Rewind {
            from_session_id: from,
            seq: Some(Seq(seq)),
            summarise: self.rewind.send.summarise(),
        };
        let id = support::mint("c_");
        let line = session_line(
            &contract::CommandId(id.clone()),
            &serving,
            Command::Rewind(args),
        );
        self.outbox.wait(&id, Waiting::RewindSend);
        self.rewind.send.begin();
        Effect::Send(vec![line])
    }

    /// Sends a fresh `rewind` for the chosen point with the summary
    /// flag the choice names, clearing the standing refusal first: a
    /// retry keeps the summary on, a rewind without summary turns it
    /// off, whatever the checkbox says.
    fn send_rewind_as(&mut self, summarise: bool) -> Effect {
        if self.rewind.send.is_sending() {
            return Effect::None;
        }
        self.rewind.send.clear_refusal();
        let saved = self.rewind.send.summarise();
        self.rewind.send.set_summarise(summarise);
        let out = self.send_rewind();
        self.rewind.send.set_summarise(saved);
        out
    }
}
