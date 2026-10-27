//! `/delete` for the session on screen (`docs/tui.md`, "Slash
//! commands"): the attached session's dry run, the immutable question
//! it opens, and the confirmed stop/delete correlation.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use super::super::super::reconnect::UNANSWERED;
use super::super::super::{App, Effect, Phase, mint};
use super::super::{refusal, refusal_code};
use super::Prompt;
use crate::home::title;
use contract::SessionId;

/// `/delete`'s feature-owned lifecycle under home's questions: one
/// command at a time, each phase exclusive. The candidate set is
/// immutable once the hub answers the discovery dry run.
#[derive(Debug, Default)]
pub(super) struct DeleteCommand {
    generation: u64,
    state: State,
}

/// The command's mutually exclusive phases: discovering the set,
/// asking with it, stopping what it holds, then deleting exactly it.
#[derive(Debug, Default)]
enum State {
    /// No command running.
    #[default]
    Idle,
    /// The cascading dry run is out; its answer opens the question.
    Discovering {
        command: String,
        target: SessionId,
        name: String,
    },
    /// The hub named the confirmed set; Enter stops the held ones.
    Asked {
        target: SessionId,
        sessions: Vec<SessionId>,
        held: Vec<SessionId>,
        fresh_exited: BTreeSet<SessionId>,
        question: String,
    },
    /// Stops are out for every still-live held id; each `session_left`
    /// with `how: exited` retires one. The delete goes out once none
    /// wait.
    Stopping {
        target: SessionId,
        sessions: Vec<SessionId>,
        waiting: BTreeSet<SessionId>,
        closes: BTreeMap<String, SessionId>,
    },
    /// Every held id stopped; the exact-set delete waits for its answer.
    Deleting {
        target: SessionId,
        sessions: Vec<SessionId>,
    },
}

impl App {
    /// `/delete` for the session on screen: sends the cascading dry run
    /// for the attached session and asks once its answer names the
    /// confirmed set. With no session on screen, the shared notice and a
    /// cleared draft; with the link down, nothing goes out and the draft
    /// stays.
    pub(in crate::app) fn delete_on_screen(&mut self) -> Effect {
        let Some((session, _)) = self.command_session() else {
            return Effect::None;
        };
        let Some(home) = self.home.as_mut() else {
            return Effect::None;
        };
        let name = home
            .sessions
            .row(&session)
            .map(title)
            .unwrap_or_else(|| session.0.clone());
        let command = mint();
        let line = json!({
            "id": command,
            "command": "delete",
            "args": {"session": session.0, "cascade": true, "dry_run": true},
        })
        .to_string();
        let generation = home.questions.delete_command.generation.wrapping_add(1);
        home.questions.delete_command = DeleteCommand {
            generation,
            state: State::Discovering {
                command: command.clone(),
                target: session,
                name,
            },
        };
        self.draft.clear();
        Effect::Send(vec![line])
    }

    /// Enter in the delete question: the command's confirmed stop/delete
    /// sequence when the question is `/delete`'s, else the existing
    /// delete.
    pub(super) fn delete_question_enter(&mut self) -> Effect {
        if self.command_owns_prompt() {
            return self.delete_command_confirm();
        }
        self.send_delete()
    }

    /// Whether the open prompt is `/delete`'s question: Esc and the ✕
    /// cancel it, Enter confirms its stops.
    pub(in crate::app) fn command_owns_prompt(&self) -> bool {
        let Some(home) = self.home.as_ref() else {
            return false;
        };
        match &home.questions.delete_command.state {
            State::Asked { target, .. } => {
                matches!(&home.questions.prompt, Some(Prompt::Delete { id, .. }) if id == target)
            }
            State::Discovering { .. }
            | State::Stopping { .. }
            | State::Deleting { .. }
            | State::Idle => false,
        }
    }

    /// Cancels the command's question: a discovery with no question yet
    /// or an open one. Stops already sent keep running; the delete never
    /// goes out after a cancel.
    pub(super) fn delete_command_cancel(&mut self) {
        if let Some(home) = self.home.as_mut()
            && matches!(
                home.questions.delete_command.state,
                State::Discovering { .. } | State::Asked { .. }
            )
        {
            home.questions.delete_command.state = State::Idle;
        }
    }

    /// The command's delete frame over the attached session: the stored
    /// question and its scroll. Nothing on home, where home draws it, or
    /// while no command question shows.
    pub(crate) fn delete_command_frame(&self) -> Option<(String, usize)> {
        if self.on_home() {
            return None;
        }
        let Phase::Attached { session, .. } = &self.phase else {
            return None;
        };
        let home = self.home.as_ref()?;
        match &home.questions.delete_command.state {
            State::Asked {
                target, question, ..
            } if session == target => match &home.questions.prompt {
                Some(Prompt::Delete { scroll, .. }) => Some((question.clone(), *scroll)),
                _ => None,
            },
            State::Idle
            | State::Discovering { .. }
            | State::Asked { .. }
            | State::Stopping { .. }
            | State::Deleting { .. } => None,
        }
    }

    /// Folds the command's hub answers: the discovery dry run, then each
    /// confirmed stop. `None` when `id` is no command line.
    pub(in crate::app) fn delete_command_answer(
        &mut self,
        id: &str,
        accepted: bool,
        payload: &Map<String, Value>,
    ) -> Option<Vec<String>> {
        if self.delete_command_close(id, accepted, payload) {
            return Some(Vec::new());
        }
        let (target, name) = self.delete_discovery_parts(id)?;
        if !accepted {
            self.delete_command_finish(false);
            self.notices.push(&refusal_code(payload), refusal(payload));
            return Some(Vec::new());
        }
        let Some(result) = payload.get("result") else {
            self.delete_command_malformed(&target, "named no result");
            return Some(Vec::new());
        };
        let (sessions, held) = match validated_sets(result, &target) {
            Ok(sets) => sets,
            Err(what) => {
                self.delete_command_malformed(&target, &what);
                return Some(Vec::new());
            }
        };
        // The answer opens the question only while its session is still
        // the attached one and no other question took the foot.
        let attached = self.session() == Some(&target);
        let free = self
            .home
            .as_ref()
            .is_some_and(|home| home.questions.prompt.is_none());
        if !attached || !free {
            self.delete_command_finish(false);
            return Some(Vec::new());
        }
        let question = command_question(&name, &target, &sessions);
        if let Some(home) = self.home.as_mut() {
            home.questions.prompt = Some(Prompt::Delete {
                id: target.clone(),
                expect: Some(sessions.clone()),
                scroll: 0,
            });
            home.questions.delete_command.state = State::Asked {
                target,
                sessions,
                held,
                fresh_exited: BTreeSet::new(),
                question,
            };
        }
        Some(Vec::new())
    }

    /// The discovery's target and captured name, while its answer is
    /// still the one the command waits on.
    fn delete_discovery_parts(&self, id: &str) -> Option<(SessionId, String)> {
        let home = self.home.as_ref()?;
        match &home.questions.delete_command.state {
            State::Discovering {
                command,
                target,
                name,
            } if command == id => Some((target.clone(), name.clone())),
            State::Idle
            | State::Discovering { .. }
            | State::Asked { .. }
            | State::Stopping { .. }
            | State::Deleting { .. } => None,
        }
    }

    /// Refuses an undecodable discovery answer: a notice naming what was
    /// wrong, no question, and nothing sent.
    fn delete_command_malformed(&mut self, target: &SessionId, what: &str) {
        self.delete_command_finish(false);
        self.notices.push(
            "io_failed",
            format!(
                "The hub's /delete answer for {} {what}. Nothing was deleted.",
                target.0
            ),
        );
    }

    /// Folds a confirmed stop's answer: an acceptance still waits on the
    /// feed's `session_left`; a refusal stops nothing further and names
    /// the session still running. True when `id` closed for the command.
    fn delete_command_close(
        &mut self,
        id: &str,
        accepted: bool,
        payload: &Map<String, Value>,
    ) -> bool {
        let session =
            self.home
                .as_mut()
                .and_then(|home| match &mut home.questions.delete_command.state {
                    State::Stopping { closes, .. } => closes.remove(id),
                    State::Idle
                    | State::Discovering { .. }
                    | State::Asked { .. }
                    | State::Deleting { .. } => None,
                });
        let Some(session) = session else {
            return false;
        };
        if accepted {
            return true;
        }
        let display = self.delete_display(&session);
        let message = format!(
            "Stop refused for {display}: {}. Nothing was deleted.",
            refusal(payload)
        );
        self.delete_command_finish(false);
        self.notices.push(&refusal_code(payload), message);
        true
    }

    /// Folds the feed's `session_left` for the confirmed stops: only
    /// `how: exited` retires a waiting session. Once none wait, the one
    /// existing delete goes out with the confirmed set. Never a second
    /// delete.
    pub(in crate::app) fn delete_command_session_left(
        &mut self,
        id: &SessionId,
        exited: bool,
    ) -> Vec<String> {
        if !exited {
            return Vec::new();
        }
        let complete = match self.home.as_mut() {
            Some(home) => match &mut home.questions.delete_command.state {
                State::Stopping { waiting, .. } => {
                    waiting.remove(id);
                    if waiting.is_empty() {
                        // The borrow ends here; the send below reborrows.
                        true
                    } else {
                        // A stopped session needs no resume line.
                        home.questions.closing.retain(|(session, _)| session != id);
                        false
                    }
                }
                State::Idle
                | State::Discovering { .. }
                | State::Asked { .. }
                | State::Deleting { .. } => false,
            },
            None => false,
        };
        if !complete {
            return Vec::new();
        }
        let (target, sessions) = match self.delete_stopping_sets() {
            Some(sets) => sets,
            None => return Vec::new(),
        };
        self.delete_command_send(target, sessions)
    }

    /// The stopping command's confirmed sets, while every held id has
    /// stopped.
    fn delete_stopping_sets(&self) -> Option<(SessionId, Vec<SessionId>)> {
        let home = self.home.as_ref()?;
        match &home.questions.delete_command.state {
            State::Stopping {
                target, sessions, ..
            } => Some((target.clone(), sessions.clone())),
            State::Idle
            | State::Discovering { .. }
            | State::Asked { .. }
            | State::Deleting { .. } => None,
        }
    }

    /// Enter in the command's question: stops every still-live held id
    /// through the existing close path, then deletes the confirmed set
    /// once each one reports its exit. With the link down nothing goes
    /// out and the question stays.
    pub(super) fn delete_command_confirm(&mut self) -> Effect {
        if !self.command_owns_prompt() {
            return Effect::None;
        }
        if !self.outbox.is_up() {
            return Effect::None;
        }
        let (target, sessions, held, fresh_exited) = match self.delete_asked_sets() {
            Some(sets) => sets,
            None => return Effect::None,
        };
        // A marked exit is trusted only while the feed row is still left:
        // a later `session_status` resumes the row, so the session must
        // be closed again rather than assumed stopped.
        let live: Vec<SessionId> = held
            .iter()
            .filter(|id| !self.delete_exit_is_current(id, &fresh_exited))
            .cloned()
            .collect();
        let waiting: BTreeSet<SessionId> = live
            .iter()
            .filter(|id| self.delete_feed_represented(id))
            .cloned()
            .collect();
        // The answered question closes; the continuation owns the wait.
        if let Some(home) = self.home.as_mut() {
            home.questions.prompt = None;
        }
        if live.is_empty() {
            let lines = self.delete_command_send(target, sessions);
            return match lines.is_empty() {
                true => Effect::None,
                false => Effect::Send(lines),
            };
        }
        let (mut lines, pairs) = self.close_lines(&live);
        if waiting.is_empty() {
            // The closes still cover sessions absent from the feed, but
            // there is no feed event to await for them.
            lines.extend(self.delete_command_send(target, sessions));
            return Effect::Send(lines);
        }
        if let Some(home) = self.home.as_mut() {
            for (session, close) in &pairs {
                home.questions
                    .closing
                    .push((session.clone(), close.clone()));
            }
            home.questions.delete_command.state = State::Stopping {
                target,
                sessions,
                waiting,
                closes: pairs
                    .into_iter()
                    .map(|(session, close)| (close, session))
                    .collect(),
            };
        }
        Effect::Send(lines)
    }

    /// The asked command's confirmed sets, while its question is open.
    fn delete_asked_sets(
        &self,
    ) -> Option<(
        SessionId,
        Vec<SessionId>,
        Vec<SessionId>,
        BTreeSet<SessionId>,
    )> {
        let home = self.home.as_ref()?;
        match &home.questions.delete_command.state {
            State::Asked {
                target,
                sessions,
                held,
                fresh_exited,
                ..
            } => Some((
                target.clone(),
                sessions.clone(),
                held.clone(),
                fresh_exited.clone(),
            )),
            State::Idle
            | State::Discovering { .. }
            | State::Stopping { .. }
            | State::Deleting { .. } => None,
        }
    }

    /// Records only exits observed after the dry-run answer opened the
    /// confirmation question. A row's old `left` value is not stop
    /// evidence: the session may have resumed since that feed event.
    pub(in crate::app) fn delete_command_note_session_left(
        &mut self,
        id: &SessionId,
        exited: bool,
    ) {
        if !exited {
            return;
        }
        if let Some(home) = self.home.as_mut()
            && let State::Asked {
                held, fresh_exited, ..
            } = &mut home.questions.delete_command.state
            && held.contains(id)
            && home.sessions.row(id).is_some()
        {
            fresh_exited.insert(id.clone());
        }
    }

    /// Whether the feed can provide fresh exit evidence for `id`. A
    /// missing row is not evidence of exit: delegates are not represented
    /// in the feed, and their parent shutdown covers them.
    fn delete_feed_represented(&self, id: &SessionId) -> bool {
        self.home
            .as_ref()
            .is_some_and(|home| home.sessions.row(id).is_some())
    }

    /// Whether `id` both exited after the discovery answer and has not
    /// resumed since: the feed row still exists and is still left. A
    /// `session_status` clears the row's `left`, resuming the session.
    fn delete_exit_is_current(&self, id: &SessionId, fresh_exited: &BTreeSet<SessionId>) -> bool {
        fresh_exited.contains(id)
            && self
                .home
                .as_ref()
                .and_then(|home| home.sessions.row(id))
                .is_some_and(|row| row.left.is_some())
    }

    /// Sends the confirmed delete through the existing delete path with
    /// the immutable set, on the current connection: `expect` carries the
    /// exact sessions the question named, even for one session. A line
    /// that cannot go out ends in the delete uncertainty instead.
    fn delete_command_send(&mut self, target: SessionId, sessions: Vec<SessionId>) -> Vec<String> {
        if !self.outbox.is_up() {
            self.delete_command_delete_unknown(&sessions);
            return Vec::new();
        }
        let old_prompt = if let Some(home) = self.home.as_mut() {
            home.questions.delete_command.state = State::Deleting {
                target: target.clone(),
                sessions: sessions.clone(),
            };
            let old = home.questions.prompt.take();
            home.questions.prompt = Some(Prompt::Delete {
                id: target,
                expect: Some(sessions.clone()),
                scroll: 0,
            });
            old
        } else {
            None
        };
        // Reuse the ordinary delete path so it registers Ask::Delete and
        // retains exact-set answer correlation. Restore another question
        // before the event loop can draw or fold an answer.
        let lines = match self.send_delete() {
            Effect::Send(lines) => lines,
            Effect::None
            | Effect::Quit
            | Effect::ListFiles
            | Effect::ReadImage(_)
            | Effect::Feedback { .. }
            | Effect::FindPause { .. }
            | Effect::Search { .. }
            | Effect::Editor { .. }
            | Effect::Exit(_)
            | Effect::Copy(_)
            | Effect::OpenLink(_)
            | Effect::OpenFile(_) => Vec::new(),
        };
        if let Some(home) = self.home.as_mut() {
            home.questions.prompt = old_prompt;
        }
        if lines.is_empty() {
            self.delete_command_delete_unknown(&sessions);
            return Vec::new();
        }
        // Stopped sessions need no resume line.
        if let Some(home) = self.home.as_mut()
            && let State::Deleting { sessions, .. } = &home.questions.delete_command.state
        {
            let gone = sessions.clone();
            home.questions
                .closing
                .retain(|(session, _)| !gone.contains(session));
        }
        lines
    }

    /// Settles the command's exact-set delete: accepted removes the row
    /// and goes home with a fresh page; `session_held` never deletes or
    /// retries and keeps the attached screen with the display-name
    /// notice; a stale or dependent set explains itself on home without
    /// re-questioning; anything else keeps the ordinary note and notice.
    /// `None` when the answer is no command delete.
    pub(super) fn delete_command_delete_answer(
        &mut self,
        id: &SessionId,
        accepted: bool,
        payload: &Map<String, Value>,
    ) -> Option<Vec<String>> {
        let (target, sessions) = self.delete_deleting_sets(id)?;
        if accepted {
            let project = self
                .home
                .as_ref()
                .and_then(|home| home.sessions.row(&target))
                .map(|row| row.project.clone());
            if let Some(home) = self.home.as_mut() {
                home.sessions.remove(&target);
            }
            let lines = self.ask_recent_first(project.as_deref());
            self.delete_command_finish(false);
            self.go_home();
            return Some(lines);
        }
        let code = refusal_code(payload);
        let message = refusal(payload);
        if code == "session_held" {
            let (first, more) = held_refusal_parts(payload, &target, &sessions);
            let display = self.delete_display(&first);
            let notice = if more == 0 {
                format!("Not deleted: {display} is running again. Nothing was deleted.")
            } else {
                format!(
                    "Not deleted: {display} is running again and {more} more. Nothing was deleted."
                )
            };
            self.delete_command_finish(false);
            self.notices.push("session_held", notice);
            return Some(Vec::new());
        }
        if code == "session_has_dependents" || code == "stale_request" {
            let project = self
                .home
                .as_ref()
                .and_then(|home| home.sessions.row(&target))
                .map(|row| row.project.clone());
            let lines = self.ask_recent_first(project.as_deref());
            self.delete_command_finish(false);
            self.notices.push(&code, message);
            self.go_home();
            return Some(lines);
        }
        if message == UNANSWERED {
            let (display, more) = self.delete_set_display(&sessions);
            let notice = if more == 0 {
                format!("Delete status unknown for {display}. Nothing was confirmed deleted.")
            } else {
                format!(
                    "Delete status unknown for {display} and {more} more. Nothing was confirmed deleted."
                )
            };
            self.delete_command_finish(true);
            self.notices.push("malformed", notice);
            return Some(Vec::new());
        }
        if let Some(home) = self.home.as_mut() {
            home.sessions.note(&target, message.clone());
        }
        let project = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.row(&target))
            .map(|row| row.project.clone());
        let lines = self.ask_recent_first(project.as_deref());
        self.delete_command_finish(true);
        self.notices.push(&code, message);
        self.go_home();
        Some(lines)
    }

    /// The deleting command's confirmed sets, while its delete waits and
    /// no home-row delete question is open.
    fn delete_deleting_sets(&self, id: &SessionId) -> Option<(SessionId, Vec<SessionId>)> {
        let home = self.home.as_ref()?;
        match &home.questions.delete_command.state {
            State::Deleting { target, sessions } if target == id => {
                if matches!(home.questions.prompt, Some(Prompt::Delete { .. })) {
                    return None;
                }
                Some((target.clone(), sessions.clone()))
            }
            State::Deleting { .. }
            | State::Idle
            | State::Discovering { .. }
            | State::Asked { .. }
            | State::Stopping { .. } => None,
        }
    }

    /// The first display name and how many more follow in `sessions`:
    /// each row's sanitized title, or its id without one.
    fn delete_set_display(&self, sessions: &[SessionId]) -> (String, usize) {
        let mut names = sessions.iter();
        let first = names
            .next()
            .map(|id| self.delete_display(id))
            .unwrap_or_default();
        (first, names.len())
    }

    /// Settles a dropped connection or a write failure for the command:
    /// discovery with no question yet drops silently; stops with no
    /// answer end in the stop uncertainty, keeping the attached screen;
    /// a deleting command ends as unknown and releases its state.
    pub(super) fn delete_command_disconnected(&mut self) {
        let deleting =
            self.home
                .as_ref()
                .and_then(|home| match &home.questions.delete_command.state {
                    State::Deleting { sessions, .. } => Some(sessions.clone()),
                    State::Idle
                    | State::Discovering { .. }
                    | State::Asked { .. }
                    | State::Stopping { .. } => None,
                });
        if let Some(sessions) = deleting {
            self.delete_command_delete_unknown(&sessions);
            return;
        }
        let waiting = match self.delete_waiting_sets() {
            Some(waiting) => waiting,
            None => {
                if let Some(home) = self.home.as_mut()
                    && matches!(
                        home.questions.delete_command.state,
                        State::Discovering { .. }
                    )
                {
                    home.questions.delete_command.state = State::Idle;
                }
                return;
            }
        };
        let (display, more) = self.delete_waiting_display(&waiting);
        let notice = if more == 0 {
            format!("Stop status unknown for {display}. Nothing was deleted.")
        } else {
            format!("Stop status unknown for {display} and {more} more. Nothing was deleted.")
        };
        self.delete_command_finish(true);
        self.notices.push("io_failed", notice);
    }

    /// The stopping command's waiting sessions, while stops are out.
    fn delete_waiting_sets(&self) -> Option<Vec<SessionId>> {
        let home = self.home.as_ref()?;
        match &home.questions.delete_command.state {
            State::Stopping { waiting, .. } => Some(waiting.iter().cloned().collect()),
            State::Idle
            | State::Discovering { .. }
            | State::Asked { .. }
            | State::Deleting { .. } => None,
        }
    }

    /// The first waiting display name and how many more follow.
    fn delete_waiting_display(&self, waiting: &[SessionId]) -> (String, usize) {
        self.delete_set_display(waiting)
    }

    /// Settles lines the hub never wrote for the command: a stop that
    /// never went out ends in the stop uncertainty, like an unanswered
    /// one. Other lines are not the command's.
    pub(super) fn delete_command_unsent(&mut self, unsent: &[String]) {
        let mine = match self.delete_waiting_close_ids() {
            Some(closes) => closes,
            None => return,
        };
        if !mine.iter().any(|close| unsent.contains(close)) {
            return;
        }
        self.delete_command_disconnected();
    }

    /// The stopping command's close command ids, while stops are out.
    fn delete_waiting_close_ids(&self) -> Option<Vec<String>> {
        let home = self.home.as_ref()?;
        match &home.questions.delete_command.state {
            State::Stopping { closes, .. } => Some(closes.keys().cloned().collect()),
            State::Idle
            | State::Discovering { .. }
            | State::Asked { .. }
            | State::Deleting { .. } => None,
        }
    }

    /// The delete's answer never arrived after its stops completed: the
    /// stop evidence stands but nothing confirms the removal.
    fn delete_command_delete_unknown(&mut self, sessions: &[SessionId]) {
        let (display, more) = self.delete_set_display(sessions);
        let notice = if more == 0 {
            format!("Delete status unknown for {display}. Nothing was confirmed deleted.")
        } else {
            format!(
                "Delete status unknown for {display} and {more} more. Nothing was confirmed deleted."
            )
        };
        self.delete_command_finish(false);
        self.notices.push("malformed", notice);
    }

    /// Drops the command's continuation: with `keep_closing` its sent
    /// stops still exclude their sessions from the exit lines, without
    /// it a later quit resumes the live ones.
    fn delete_command_finish(&mut self, keep_closing: bool) {
        let Some(home) = self.home.as_mut() else {
            return;
        };
        let taken = std::mem::take(&mut home.questions.delete_command);
        if keep_closing {
            home.questions.delete_command.generation = taken.generation;
            return;
        }
        let sessions = match taken.state {
            State::Stopping { sessions, .. } | State::Deleting { sessions, .. } => sessions,
            State::Asked {
                target, sessions, ..
            } => std::iter::once(target).chain(sessions).collect(),
            State::Idle | State::Discovering { .. } => Vec::new(),
        };
        home.questions.delete_command.generation = taken.generation;
        home.questions
            .closing
            .retain(|(session, _)| !sessions.contains(session));
    }

    /// The row's sanitized title, or its id without one, for notices.
    fn delete_display(&self, id: &SessionId) -> String {
        self.home
            .as_ref()
            .and_then(|home| home.sessions.row(id))
            .map(title)
            .unwrap_or_else(|| id.0.clone())
    }
}

/// Whether `id` names a session the hub minted: `s_` plus 16 lowercase
/// hex digits. The discovery answer is trusted only when every id fits,
/// so a malformed answer never sends a stop or a real delete.
fn minted(id: &str) -> bool {
    let hex = id.strip_prefix("s_").unwrap_or("");
    hex.len() == 16
        && hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Checks the discovery answer before it opens a question: `sessions`
/// is a nonempty unique set containing the attached session, and `held`
/// is a unique subset of it. Anything else describes what was wrong.
fn validated_sets(
    result: &Value,
    target: &SessionId,
) -> Result<(Vec<SessionId>, Vec<SessionId>), String> {
    let sessions = result
        .get("sessions")
        .and_then(Value::as_array)
        .ok_or_else(|| "named no sessions".to_owned())?;
    if sessions.is_empty() {
        return Err("named no sessions".to_owned());
    }
    let mut seen = BTreeSet::new();
    let mut set = Vec::with_capacity(sessions.len());
    for id in sessions {
        let id = id.as_str().unwrap_or("");
        if !minted(id) {
            return Err(format!("named an id the hub never mints: `{id}`"));
        }
        if !seen.insert(id) {
            return Err(format!("repeated a session: `{id}`"));
        }
        set.push(SessionId(id.to_owned()));
    }
    if !seen.contains(target.0.as_str()) {
        return Err(format!("omits the session on screen: `{}`", target.0));
    }
    let held = result
        .get("held")
        .and_then(Value::as_array)
        .ok_or_else(|| "named no held sessions".to_owned())?;
    let mut held_seen = BTreeSet::new();
    let mut held_set = Vec::with_capacity(held.len());
    for id in held {
        let id = id.as_str().unwrap_or("");
        if !minted(id) {
            return Err(format!("held an id the hub never mints: `{id}`"));
        }
        if !held_seen.insert(id) {
            return Err(format!("repeated a held session: `{id}`"));
        }
        if !seen.contains(id) {
            return Err(format!("holds a session it would not remove: `{id}`"));
        }
        held_set.push(SessionId(id.to_owned()));
    }
    Ok((set, held_set))
}

/// The command's delete question: the target and every session the
/// cascade adds, stating they will be stopped and permanently deleted.
/// The legend keeps the home-row footer words, so the frame draws the
/// same footer for the same set shape.
fn command_question(name: &str, target: &SessionId, sessions: &[SessionId]) -> String {
    let others: Vec<&str> = sessions
        .iter()
        .filter(|id| *id != target)
        .map(|id| id.0.as_str())
        .collect();
    if others.is_empty() {
        return format!(
            "Delete {name} ({})? It will be stopped and permanently deleted. It cannot be undone · Enter delete · Esc keep",
            target.0
        );
    }
    let continued = if others.len() == 1 {
        format!("and 1 session that continues it: {}", others.join(", "))
    } else {
        format!(
            "and {} sessions that continue it: {}",
            others.len(),
            others.join(", ")
        )
    };
    format!(
        "Delete {name} ({}) {continued}? They will be stopped and permanently deleted. It cannot be undone · Enter delete all · Esc keep",
        target.0
    )
}

/// The refused delete's first busy id and how many more follow. The
/// structured `held` field is trusted only as a complete, valid unique
/// list; otherwise fall back to the target and single-session wording.
fn held_refusal_parts(
    payload: &Map<String, Value>,
    target: &SessionId,
    sessions: &[SessionId],
) -> (SessionId, usize) {
    let Some(held) = payload.get("held").and_then(Value::as_array) else {
        return (target.clone(), 0);
    };
    let allowed: BTreeSet<&str> = sessions.iter().map(|session| session.0.as_str()).collect();
    if held.is_empty() {
        return (target.clone(), 0);
    }
    let mut seen = BTreeSet::new();
    let mut ids = Vec::with_capacity(held.len());
    for value in held {
        let Some(id) = value.as_str().filter(|id| minted(id)) else {
            return (target.clone(), 0);
        };
        if !seen.insert(id) || !allowed.contains(id) {
            return (target.clone(), 0);
        }
        ids.push(id);
    }
    let Some(first) = ids.first() else {
        return (target.clone(), 0);
    };
    (SessionId((*first).to_owned()), ids.len() - 1)
}

#[cfg(test)]
#[path = "delete_tests.rs"]
mod tests;
