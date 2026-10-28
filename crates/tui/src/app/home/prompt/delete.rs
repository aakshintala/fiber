//! `/delete` for the session on screen (`docs/tui.md`, "Slash
//! commands"): the attached session's dry run, the immutable question
//! it opens, and the confirmed stop/delete correlation.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

mod settlement;
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
        name: String,
        question: String,
    },
    /// Stops are out for each feed-backed held row that was live at
    /// confirmation; `session_left` with `how: exited` or `crashed`
    /// retires one. The delete goes out once none wait.
    Stopping {
        target: SessionId,
        sessions: Vec<SessionId>,
        name: String,
        waiting: Vec<SessionId>,
        closes: BTreeMap<String, SessionId>,
    },
    /// Every held id stopped; the exact-set delete waits for its answer.
    Deleting {
        command: String,
        target: SessionId,
        sessions: Vec<SessionId>,
        name: String,
    },
}

/// The asked command's confirmed sets while its question is open.
struct AskedSets {
    target: SessionId,
    sessions: Vec<SessionId>,
    held: Vec<SessionId>,
    name: String,
}

impl App {
    /// `/delete` for the session on screen: sends the cascading dry run
    /// for the attached session and asks once its answer names the
    /// confirmed set. With no session on screen, the shared notice and a
    /// cleared draft; with the link down, nothing goes out and the draft
    /// stays.
    pub(in crate::app) fn delete_on_screen(&mut self) -> Effect {
        // Offline invocation is a no-op, even when a lost sequence still
        // awaits reconnect settlement. Never replace that sequence.
        if !self.outbox.is_up() {
            return Effect::None;
        }
        if self.delete_command_active() {
            self.notices.push(
                "stale_request",
                "`/delete` is already in progress.".to_owned(),
            );
            return Effect::None;
        }
        let Some((session, _)) = self.command_session() else {
            return Effect::None;
        };
        let name = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.row(&session))
            .map(title)
            .or_else(|| {
                self.name()
                    .filter(|name| !name.is_empty())
                    .map(crate::text::controls_to_space)
            })
            .unwrap_or_else(|| session.0.clone());
        let Some(home) = self.home.as_mut() else {
            return Effect::None;
        };
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

    /// Whether any phase of the command owns the destructive sequence.
    fn delete_command_active(&self) -> bool {
        self.home
            .as_ref()
            .is_some_and(|home| !matches!(home.questions.delete_command.state, State::Idle))
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

    /// The immutable command question shown on Home while it remains open.
    pub(crate) fn delete_command_home_question(&self) -> Option<(String, usize)> {
        if !self.on_home() || !self.command_owns_prompt() {
            return None;
        }
        let home = self.home.as_ref()?;
        let State::Asked { question, .. } = &home.questions.delete_command.state else {
            return None;
        };
        let scroll = match &home.questions.prompt {
            Some(Prompt::Delete { scroll, .. }) => *scroll,
            _ => return None,
        };
        Some((question.clone(), scroll))
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
        if self.delete_deleting_command(id) {
            self.take_ask(id);
            return self.delete_command_delete_answer(id, accepted, payload);
        }
        if self.delete_command_close(id, accepted, payload) {
            self.take_ask(id);
            return Some(Vec::new());
        }
        let (target, name) = self.delete_discovery_parts(id)?;
        if !accepted {
            self.delete_command_finish(false);
            self.notices.push(
                &refusal_code(payload),
                format!("Delete failed: {}", refusal(payload)),
            );
            return Some(Vec::new());
        }
        let Some(result) = payload.get("result") else {
            self.delete_command_malformed();
            return Some(Vec::new());
        };
        let (sessions, held) = match validated_sets(result, &target) {
            Ok(sets) => sets,
            Err(_what) => {
                self.delete_command_malformed();
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
                name,
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

    /// Refuses an invalid discovery preview without exposing its data.
    fn delete_command_malformed(&mut self) {
        self.delete_command_finish(false);
        self.notices.push(
            "malformed",
            "Not deleted: the hub's preview was invalid. Nothing was deleted.".to_owned(),
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

    /// Folds the feed's `session_left` for confirmed stops: both exited
    /// and crashed retire a waiting session. Once none wait, the one
    /// existing delete goes out with the confirmed set. Never a second
    /// delete.
    pub(in crate::app) fn delete_command_session_left(&mut self, id: &SessionId) -> Vec<String> {
        let complete = match self.home.as_mut() {
            Some(home) => match &mut home.questions.delete_command.state {
                State::Stopping { waiting, .. } => {
                    waiting.retain(|waiting_id| waiting_id != id);
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
        let (target, sessions, name) = match self.delete_stopping_sets() {
            Some(sets) => sets,
            None => return Vec::new(),
        };
        self.delete_command_send(target, sessions, name)
    }

    /// The stopping command's confirmed sets, while every held id has
    /// stopped.
    fn delete_stopping_sets(&self) -> Option<(SessionId, Vec<SessionId>, String)> {
        let home = self.home.as_ref()?;
        match &home.questions.delete_command.state {
            State::Stopping {
                target,
                sessions,
                name,
                ..
            } => Some((target.clone(), sessions.clone(), name.clone())),
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
        let AskedSets {
            target,
            sessions,
            held,
            name,
        } = match self.delete_asked_sets() {
            Some(sets) => sets,
            None => return Effect::None,
        };
        let feed_held: Vec<SessionId> = held
            .iter()
            .filter(|id| {
                self.home
                    .as_ref()
                    .and_then(|home| home.sessions.feed_row(id))
                    .is_some_and(|row| row.left.is_none())
            })
            .cloned()
            .collect();
        let waiting = feed_held.clone();
        // The answered question closes; the continuation owns the wait.
        if let Some(home) = self.home.as_mut() {
            home.questions.prompt = None;
        }
        if feed_held.is_empty() {
            self.go_home();
            let lines = self.delete_command_send(target, sessions, name);
            return match lines.is_empty() {
                true => Effect::None,
                false => Effect::Send(lines),
            };
        }
        let (lines, pairs) = self.close_lines(&feed_held);
        if let Some(home) = self.home.as_mut() {
            for (session, close) in &pairs {
                home.questions
                    .closing
                    .push((session.clone(), close.clone()));
            }
            home.questions.delete_command.state = State::Stopping {
                target,
                sessions,
                name,
                waiting,
                closes: pairs
                    .into_iter()
                    .map(|(session, close)| (close, session))
                    .collect(),
            };
        }
        self.go_home();
        Effect::Send(lines)
    }

    /// Whether `id` is the exact command answer owned by the deleting sequence.
    fn delete_deleting_command(&self, id: &str) -> bool {
        self.home.as_ref().is_some_and(|home| {
            matches!(
                &home.questions.delete_command.state,
                State::Deleting { command, .. } if command == id
            )
        })
    }

    /// The asked command's confirmed sets, while its question is open.
    fn delete_asked_sets(&self) -> Option<AskedSets> {
        let home = self.home.as_ref()?;
        match &home.questions.delete_command.state {
            State::Asked {
                target,
                sessions,
                held,
                name,
                ..
            } => Some(AskedSets {
                target: target.clone(),
                sessions: sessions.clone(),
                held: held.clone(),
                name: name.clone(),
            }),
            State::Idle
            | State::Discovering { .. }
            | State::Stopping { .. }
            | State::Deleting { .. } => None,
        }
    }

    /// Sends the confirmed delete through the existing delete path with
    /// the immutable set, on the current connection: `expect` carries the
    /// exact sessions the question named, even for one session. A line
    /// that cannot go out ends in the delete uncertainty instead.
    fn delete_command_send(
        &mut self,
        target: SessionId,
        sessions: Vec<SessionId>,
        name: String,
    ) -> Vec<String> {
        if !self.outbox.is_up() {
            self.delete_command_delete_unknown();
            return Vec::new();
        }
        let (command, line) = super::delete_line(&target, Some(&sessions));
        if let Some(home) = self.home.as_mut() {
            home.questions
                .asks
                .insert(command.clone(), super::Ask::Delete(target.clone()));
            home.questions.delete_command.state = State::Deleting {
                command,
                target,
                sessions: sessions.clone(),
                name,
            };
            // Stopped sessions need no resume line; unrelated close markers
            // remain available to the ordinary quit path.
            home.questions
                .closing
                .retain(|(session, _)| !sessions.contains(session));
        }
        vec![line]
    }

    /// The deleting command's confirmed sets, while its delete waits and
    /// no home-row delete question is open.
    fn delete_deleting_sets(&self, id: &str) -> Option<(SessionId, Vec<SessionId>, String)> {
        let home = self.home.as_ref()?;
        match &home.questions.delete_command.state {
            State::Deleting {
                command,
                target,
                sessions,
                name,
            } if command == id => Some((target.clone(), sessions.clone(), name.clone())),
            State::Deleting { .. }
            | State::Idle
            | State::Discovering { .. }
            | State::Asked { .. }
            | State::Stopping { .. } => None,
        }
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
            let stopped: BTreeSet<SessionId> = match taken.state {
                State::Stopping { waiting, .. } => {
                    let outstanding: BTreeSet<SessionId> = waiting.into_iter().collect();
                    home.questions
                        .closing
                        .iter()
                        .filter_map(|(session, _)| {
                            (!outstanding.contains(session)
                                && home
                                    .sessions
                                    .feed_row(session)
                                    .is_some_and(|row| row.left.is_some()))
                            .then_some(session.clone())
                        })
                        .collect()
                }
                State::Deleting { sessions, .. } => sessions.into_iter().collect(),
                State::Idle | State::Discovering { .. } | State::Asked { .. } => BTreeSet::new(),
            };
            home.questions
                .closing
                .retain(|(session, _)| stopped.contains(session));
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
    contract::is_minted_session_id(id)
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
    }
    let held_set = set
        .iter()
        .filter(|id| held_seen.contains(id.0.as_str()))
        .cloned()
        .collect();
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

#[cfg(test)]
#[path = "delete_tests.rs"]
mod tests;
