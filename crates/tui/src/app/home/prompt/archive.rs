//! `/archive` for the session on screen (`docs/tui.md`, "Slash
//! commands"): the attached session's dry run, then its cascade's
//! stops and the one archival. It asks nothing at any phase: a stop
//! the hub refuses archives nothing.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use super::super::super::{App, Effect, Waiting};
use super::{refusal, refusal_code};
use crate::home::title;
use contract::ErrorCode;
use contract::SessionId;

/// `/archive`'s feature-owned lifecycle under home's questions: one
/// command at a time, each phase exclusive. The stops retire on the
/// feed's `session_left`; the archive goes out once every held stop
/// retired. The command's hub asks live only as outbox tags carrying
/// `generation`: an answer folds when its tag's generation is this one
/// and the state is the tag's phase.
#[derive(Debug, Default)]
pub(super) struct ArchiveCommand {
    generation: u64,
    state: State,
}

/// The command's mutually exclusive phases: discovering the set,
/// stopping what it holds, then archiving exactly the target.
#[derive(Debug, Default)]
enum State {
    /// No command running.
    #[default]
    Idle,
    /// The cascading dry run is out; its answer stops the held set.
    Discovering { target: SessionId, name: String },
    /// Stops are out for each feed-backed held row that was live;
    /// `session_left` with `how: exited` or `crashed` retires one. The
    /// one archive goes out once none wait.
    Stopping {
        target: SessionId,
        name: String,
        waiting: Vec<SessionId>,
    },
    /// The one archive waits for its answer; it names only the target.
    Archiving { target: SessionId, name: String },
}

impl App {
    /// `/archive` for the session on screen: sends the cascading dry
    /// run for the attached session and stops the held cascade when
    /// its answer names it, then archives that one session. With no
    /// session on screen, the shared notice and a cleared draft; with
    /// the link down, nothing goes out and the draft stays.
    pub(in crate::app) fn archive_on_screen(&mut self) -> Effect {
        // Offline invocation is a no-op, even when a lost sequence still
        // awaits reconnect settlement. Never replace that sequence.
        if !self.outbox.is_up() {
            return Effect::None;
        }
        if self.archive_command_active() {
            self.notices.push(
                ErrorCode::StaleRequest,
                "`/archive` is already in progress.".to_owned(),
            );
            return Effect::None;
        }
        let Some((session, _)) = self.command_session() else {
            return Effect::None;
        };
        let name = self
            .home
            .sessions
            .row(&session)
            .map(title)
            .or_else(|| {
                self.name()
                    .filter(|name| !name.is_empty())
                    .map(crate::text::controls_to_space)
            })
            .unwrap_or_else(|| session.0.clone());
        let home = &mut self.home;
        let command = support::mint("c_");
        let line = json!({
            "id": command,
            "command": "delete",
            "args": {"session": session.0, "cascade": true, "dry_run": true},
        })
        .to_string();
        let generation = home.questions.archive_command.generation.wrapping_add(1);
        home.questions.archive_command = ArchiveCommand {
            generation,
            state: State::Discovering {
                target: session,
                name,
            },
        };
        self.outbox
            .wait(&command, Waiting::ArchiveDiscovery(generation));
        self.draft.clear();
        Effect::Send(vec![line])
    }

    /// Whether any phase of the command owns the stop/archive sequence.
    fn archive_command_active(&self) -> bool {
        !matches!(self.home.questions.archive_command.state, State::Idle)
    }

    /// Folds the command's hub answers: the discovery dry run, then
    /// each confirmed stop and the one archive, by the tag `id`
    /// carries. `None` when `id` is no line of the running command: an
    /// earlier command's tag, or one for a phase the command is not in.
    pub(in crate::app) fn archive_command_answer(
        &mut self,
        id: &str,
        accepted: bool,
        payload: &Map<String, Value>,
    ) -> Option<Vec<String>> {
        let tag = self.outbox.waiting(id)?;
        if let Waiting::ArchiveFinal(generation) = tag {
            if !self.archive_archiving_command(generation) {
                return None;
            }
            self.take_ask(id);
            return self.archive_command_archive_answer(accepted, payload);
        }
        if let Waiting::ArchiveStop {
            generation,
            session,
        } = &tag
        {
            // A command's stops register no ask, only their tag.
            return self
                .archive_command_close(*generation, session, id, accepted, payload)
                .then(Vec::new);
        }
        let Waiting::ArchiveDiscovery(generation) = tag else {
            return None;
        };
        let (target, name) = self.archive_discovery_parts(generation)?;
        self.outbox.settled(id);
        if !accepted {
            self.archive_command_finish(false);
            self.notices.push(
                refusal_code(payload),
                format!("Archive failed: {}", refusal(payload)),
            );
            return Some(Vec::new());
        }
        let Some(result) = payload.get("result") else {
            self.archive_command_malformed_preview();
            return Some(Vec::new());
        };
        let Some((_, held)) = validated_sets(result, &target) else {
            self.archive_command_malformed_preview();
            return Some(Vec::new());
        };
        // No question opens: stop the held cascade at once through the
        // existing close path, then archive the target alone.
        let feed_held: Vec<SessionId> = held
            .iter()
            .filter(|id| {
                self.home
                    .sessions
                    .feed_row(id)
                    .is_some_and(|row| row.left.is_none())
            })
            .cloned()
            .collect();
        let waiting = feed_held.clone();
        if feed_held.is_empty() {
            self.go_home();
            return Some(self.archive_command_send(target, name));
        }
        let (lines, pairs) = self.close_lines(&feed_held);
        for (session, close) in &pairs {
            self.home
                .questions
                .closing
                .push((session.clone(), close.clone()));
            self.outbox.wait(
                close,
                Waiting::ArchiveStop {
                    generation: self.home.questions.archive_command.generation,
                    session: session.clone(),
                },
            );
        }
        self.home.questions.archive_command.state = State::Stopping {
            target,
            name,
            waiting,
        };
        self.go_home();
        Some(lines)
    }

    /// The discovery's target and captured name, while its answer is
    /// still the one the command waits on: the running command's, in
    /// the discovery phase.
    fn archive_discovery_parts(&self, generation: u64) -> Option<(SessionId, String)> {
        let home = &self.home;
        if home.questions.archive_command.generation != generation {
            return None;
        }
        match &home.questions.archive_command.state {
            State::Discovering { target, name } => Some((target.clone(), name.clone())),
            State::Idle | State::Stopping { .. } | State::Archiving { .. } => None,
        }
    }

    /// Refuses an invalid discovery preview without exposing its data.
    fn archive_command_malformed_preview(&mut self) {
        self.archive_command_finish(false);
        self.notices.push(
            ErrorCode::Malformed,
            "Not archived: the hub's preview was invalid. Nothing was archived.".to_owned(),
        );
    }

    /// Folds a confirmed stop's answer: an acceptance still waits on
    /// the feed's `session_left`; a refusal stops nothing further and
    /// archives nothing. True when the stop `id` of `session` belongs
    /// to the running command, which is stopping.
    fn archive_command_close(
        &mut self,
        generation: u64,
        session: &SessionId,
        id: &str,
        accepted: bool,
        payload: &Map<String, Value>,
    ) -> bool {
        let stopping = self.home.questions.archive_command.generation == generation
            && matches!(
                self.home.questions.archive_command.state,
                State::Stopping { .. }
            );
        if !stopping {
            return false;
        }
        self.outbox.settled(id);
        if accepted {
            return true;
        }
        let display = self.archive_display(session);
        let message = format!(
            "Stop refused for {display}: {}. Nothing was archived.",
            refusal(payload)
        );
        self.archive_command_finish(false);
        self.notices.push(refusal_code(payload), message);
        true
    }

    /// Folds the feed's `session_left` for confirmed stops: both exited
    /// and crashed retire a waiting session. Once none wait, the one
    /// archive goes out for the target alone. Never a second archive.
    pub(in crate::app) fn archive_command_session_left(&mut self, id: &SessionId) -> Vec<String> {
        let home = &mut self.home;
        let complete = match &mut home.questions.archive_command.state {
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
            State::Idle | State::Discovering { .. } | State::Archiving { .. } => false,
        };
        if !complete {
            return Vec::new();
        }
        let (target, name) = match self.archive_stopping_target() {
            Some(target) => target,
            None => return Vec::new(),
        };
        self.archive_command_send(target, name)
    }

    /// The stopping command's target, while every held id has stopped.
    fn archive_stopping_target(&self) -> Option<(SessionId, String)> {
        let home = &self.home;
        match &home.questions.archive_command.state {
            State::Stopping { target, name, .. } => Some((target.clone(), name.clone())),
            State::Idle | State::Discovering { .. } | State::Archiving { .. } => None,
        }
    }

    /// Whether the running command is `generation`'s and is archiving:
    /// the phase that owns the one archive's answer.
    fn archive_archiving_command(&self, generation: u64) -> bool {
        self.home.questions.archive_command.generation == generation
            && matches!(
                self.home.questions.archive_command.state,
                State::Archiving { .. }
            )
    }

    /// Sends the one archive through the row archive path, on the
    /// current connection: exactly the target session, never the held
    /// cascade. A line that cannot go out ends in the archive
    /// uncertainty instead.
    fn archive_command_send(&mut self, target: SessionId, name: String) -> Vec<String> {
        if !self.outbox.is_up() {
            self.archive_command_archive_unknown();
            return Vec::new();
        }
        let (command, line) = super::archive_line(&target);
        self.home
            .questions
            .asks
            .insert(command.clone(), super::Ask::Archive(target.clone()));
        self.outbox.wait(
            &command,
            Waiting::ArchiveFinal(self.home.questions.archive_command.generation),
        );
        self.home.questions.archive_command.state = State::Archiving { target, name };
        vec![line]
    }

    /// Settles the command's one archive: accepted archives that
    /// session, refetches its project's totals and goes home; a
    /// refusal keeps rows with one notice and retries nothing. `None`
    /// when the answer is not this command's archive.
    fn archive_command_archive_answer(
        &mut self,
        accepted: bool,
        payload: &Map<String, Value>,
    ) -> Option<Vec<String>> {
        let (target, _name) = self.archive_archiving_target()?;
        if accepted {
            let archived = payload
                .get("result")
                .and_then(|result| result.get("archived"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if archived {
                self.clear_prompt(&target);
                let project = self.archive_target_project(&target);
                self.home.sessions.remove(&target);
                let lines = self.ask_recent_first(project.as_deref());
                self.archive_command_finish(false);
                self.go_home();
                return Some(lines);
            }
            self.archive_command_malformed_answer();
            return Some(Vec::new());
        }
        let code = refusal_code(payload);
        let message = refusal(payload);
        self.archive_command_finish(false);
        self.notices
            .push(code, format!("Archive failed: {message}"));
        self.go_home();
        Some(Vec::new())
    }

    /// Refuses an archive answer that confirms nothing without
    /// exposing its data.
    fn archive_command_malformed_answer(&mut self) {
        self.archive_command_finish(false);
        self.notices.push(
            ErrorCode::Malformed,
            "Not archived: the hub's archive answer was invalid. Nothing was archived.".to_owned(),
        );
    }

    /// The archiving command's target, while its archive waits.
    fn archive_archiving_target(&self) -> Option<(SessionId, String)> {
        let home = &self.home;
        match &home.questions.archive_command.state {
            State::Archiving { target, name } => Some((target.clone(), name.clone())),
            State::Idle | State::Discovering { .. } | State::Stopping { .. } => None,
        }
    }

    /// Drops the command's continuation: with `keep_closing` its sent
    /// stops still exclude their sessions from the exit lines, without
    /// it a later quit resumes the live ones.
    fn archive_command_finish(&mut self, keep_closing: bool) {
        let home = &mut self.home;
        let taken = std::mem::take(&mut home.questions.archive_command);
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
                State::Idle | State::Discovering { .. } | State::Archiving { .. } => {
                    BTreeSet::new()
                }
            };
            home.questions
                .closing
                .retain(|(session, _)| stopped.contains(session));
            home.questions.archive_command.generation = taken.generation;
            return;
        }
        if let State::Archiving { target, .. } = taken.state {
            home.questions
                .closing
                .retain(|(session, _)| session != &target);
        }
        home.questions.archive_command.generation = taken.generation;
    }

    /// The ids of the running command's tags of one kind, in send order:
    /// `generation_of` reads the generation off a tag of that kind, and
    /// only the running command's generation counts.
    fn archive_tag_ids(&self, generation_of: impl Fn(&Waiting) -> Option<u64>) -> Vec<String> {
        let running = self.home.questions.archive_command.generation;
        self.outbox
            .tags()
            .filter(|(_, tag)| generation_of(tag) == Some(running))
            .map(|(id, _)| id.to_owned())
            .collect()
    }

    /// The row's sanitized title, or its id without one, for notices.
    fn archive_display(&self, id: &SessionId) -> String {
        self.home
            .sessions
            .row(id)
            .map(title)
            .unwrap_or_else(|| id.0.clone())
    }

    /// Settles a sent slash archive on reconnect, or drops a lost
    /// discovery silently. The exact ask is removed before generic row
    /// archive settlement can see it.
    pub(in crate::app::home::prompt) fn archive_command_disconnected(&mut self) {
        let archiving = matches!(
            self.home.questions.archive_command.state,
            State::Archiving { .. }
        );
        if archiving {
            for command in self.archive_tag_ids(archive_final_generation) {
                self.take_ask(&command);
            }
            self.archive_command_archive_unknown();
            return;
        }
        let discovering = matches!(
            self.home.questions.archive_command.state,
            State::Discovering { .. }
        );
        if !discovering {
            return;
        }
        let commands = self.archive_tag_ids(archive_discovery_generation);
        self.home.questions.archive_command.state = State::Idle;
        for command in commands {
            self.outbox.settled(&command);
        }
    }

    /// Settles a stopping sequence immediately when the live hub link
    /// drops. Waiting rows are not evidence of exit; the stop's outcome
    /// is unknown, so nothing is archived.
    pub(in crate::app) fn archive_command_link_lost(&mut self) {
        let stopping = matches!(
            self.home.questions.archive_command.state,
            State::Stopping { .. }
        );
        if !stopping {
            return;
        }
        self.outbox
            .settle_where(|waiting| matches!(waiting, crate::app::Waiting::ArchiveStop { .. }));
        self.archive_command_finish(true);
        self.notices
            .push(ErrorCode::Indeterminate, "Stop status unknown".to_owned());
    }

    /// Settles only this command's archive ask, or an unsent stop
    /// among `stops`, the command's stop ids read before the unsent
    /// closes' tags settled. An ambiguous archival write is never retried.
    pub(in crate::app::home::prompt) fn archive_command_unsent(
        &mut self,
        unsent: &[String],
        stops: &[String],
    ) {
        let archiving = matches!(
            self.home.questions.archive_command.state,
            State::Archiving { .. }
        );
        let archives = self.archive_tag_ids(archive_final_generation);
        if archiving && let Some(command) = archives.iter().find(|command| unsent.contains(command))
        {
            self.take_ask(command);
            self.archive_command_archive_unknown();
            return;
        }
        if stops.iter().any(|close| unsent.contains(close)) {
            self.archive_command_link_lost();
        }
    }

    /// The stopping command's close command ids, while stops are out.
    pub(in crate::app::home::prompt) fn archive_waiting_close_ids(&self) -> Option<Vec<String>> {
        let home = &self.home;
        match &home.questions.archive_command.state {
            State::Stopping { .. } => Some(self.archive_tag_ids(archive_stop_generation)),
            State::Idle | State::Discovering { .. } | State::Archiving { .. } => None,
        }
    }

    /// The target's project, for the `recent` refresh after it goes.
    fn archive_target_project(&self, target: &SessionId) -> Option<String> {
        self.home
            .sessions
            .row(target)
            .map(|row| row.project.clone())
    }

    /// The archive's answer never arrived after its stops completed: the
    /// stop evidence stands but nothing confirms the archival.
    fn archive_command_archive_unknown(&mut self) {
        self.archive_command_finish(false);
        self.notices.push(
            ErrorCode::Indeterminate,
            "Archive status unknown".to_owned(),
        );
    }
}

/// The generation an `ArchiveDiscovery` tag carries.
fn archive_discovery_generation(tag: &Waiting) -> Option<u64> {
    if let Waiting::ArchiveDiscovery(generation) = tag {
        Some(*generation)
    } else {
        None
    }
}

/// The generation an `ArchiveFinal` tag carries.
fn archive_final_generation(tag: &Waiting) -> Option<u64> {
    if let Waiting::ArchiveFinal(generation) = tag {
        Some(*generation)
    } else {
        None
    }
}

/// The generation an `ArchiveStop` tag carries.
fn archive_stop_generation(tag: &Waiting) -> Option<u64> {
    if let Waiting::ArchiveStop { generation, .. } = tag {
        Some(*generation)
    } else {
        None
    }
}

/// Checks the discovery answer before it stops the held set: `sessions`
/// is a nonempty unique set containing the attached session, and `held`
/// is a unique subset of it in the preview's relative order. Anything
/// else is malformed: the caller shows the invalid-preview notice and
/// the invalid detail never reaches a person.
fn validated_sets(result: &Value, target: &SessionId) -> Option<(Vec<SessionId>, Vec<SessionId>)> {
    let sessions = result.get("sessions").and_then(Value::as_array)?;
    if sessions.is_empty() {
        return None;
    }
    let mut seen = BTreeSet::new();
    let mut set = Vec::with_capacity(sessions.len());
    for id in sessions {
        let id = id.as_str().unwrap_or("");
        if !contract::is_minted_session_id(id) {
            return None;
        }
        if !seen.insert(id) {
            return None;
        }
        set.push(SessionId(id.to_owned()));
    }
    if !seen.contains(target.0.as_str()) {
        return None;
    }
    let held = result.get("held").and_then(Value::as_array)?;
    let mut held_seen = BTreeSet::new();
    let mut held_order = Vec::with_capacity(held.len());
    for id in held {
        let id = id.as_str().unwrap_or("");
        if !contract::is_minted_session_id(id) {
            return None;
        }
        if !held_seen.insert(id) {
            return None;
        }
        if !seen.contains(id) {
            return None;
        }
        held_order.push(id);
    }
    let held_set: Vec<SessionId> = set
        .iter()
        .filter(|id| held_seen.contains(id.0.as_str()))
        .cloned()
        .collect();
    // The hub names `held` in the preview's order; a `held` array that
    // disagrees with it relatively is not the preview the hub checked.
    if held_set
        .iter()
        .map(|id| id.0.as_str())
        .ne(held_order.iter().copied())
    {
        return None;
    }
    Some((set, held_set))
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
