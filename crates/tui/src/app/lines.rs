//! What the hub's lines do to the app: the link coming up or going down, the
//! answers to its commands, and the attached session's stream folded into
//! the conversation (`docs/tui.md`, "Turns", "Steering", "Notices").

use contract::events::{
    CommandAccepted, CommandRejected, Notice, SessionNamed, ShellCommand, SteeringQueue,
    TurnStarted,
};
use contract::{Envelope, HubLine, SessionId};
use serde_json::{Map, Value};

use super::{App, Kind, Link, Phase, read};
use crate::approvals;
use crate::home::Level;
use crate::link::Line;
use crate::shell;

impl App {
    /// Folds one line from the hub, returning command lines to send.
    pub(crate) fn on_line(&mut self, line: Line) -> Vec<String> {
        let mut lines = match self.home_line(&line) {
            Some(consumed) => consumed,
            None => match line {
                Line::Hub(hub) => self.on_hub(&hub),
                Line::Session(envelope) => self.on_session(&envelope),
            },
        };
        lines.extend(self.home_outgoing());
        self.settle();
        lines
    }

    /// The hub could not be reached, or runs a schema this terminal cannot
    /// read: the notice, and a held `start` fails as if rejected.
    pub(crate) fn connect_failed(&mut self, notice: String) {
        self.link = Link::Down;
        self.notices.push(notice);
        self.held.clear();
        if let Phase::Pending { command_id } = &self.phase {
            let id = command_id.clone();
            self.fail(&id);
        }
        self.settle();
    }

    /// The hub connection ended. Reconnecting is a later ticket. A
    /// connection never connected, refused for its schema version, keeps
    /// the notice that says why.
    pub(crate) fn disconnected(&mut self) {
        if self.link == Link::Up {
            self.link = Link::Down;
            self.notices.push("Connection lost.".to_owned());
        }
        self.settle();
    }

    /// Writing `unsent`, command lines this app made, to the hub failed:
    /// the connection is lost, and their commands fail as if rejected, so
    /// a draft they carried returns to an empty draft. A close from the
    /// quit question that was never written keeps its resume line.
    pub(crate) fn write_failed(&mut self, unsent: &[String]) {
        self.home_unsent(unsent);
        self.disconnected();
        for line in unsent {
            if let Some(id) = serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
            {
                self.fail(&id);
            }
        }
        self.settle();
    }

    fn on_hub(&mut self, hub: &HubLine) -> Vec<String> {
        let command_id = hub_string(&hub.payload, "command_id");
        match hub.kind.as_str() {
            "hub_hello" if hub.schema_version == contract::SCHEMA_VERSION => {
                self.link = Link::Up;
                std::mem::take(&mut self.held)
            }
            "hub_hello" => {
                self.connect_failed(format!(
                    "The hub runs schema version {}; this terminal reads {}.",
                    hub.schema_version,
                    contract::SCHEMA_VERSION
                ));
                Vec::new()
            }
            "command_accepted" => {
                let Some(id) = command_id else {
                    return Vec::new();
                };
                if let Some(lines) = self.history_answered(&id, hub.payload.get("result")) {
                    return lines;
                }
                let session = hub
                    .payload
                    .get("result")
                    .and_then(|result| result.get("session_id"))
                    .and_then(Value::as_str)
                    .map(|id| SessionId(id.to_owned()));
                match (self.pending.remove(&id), session) {
                    (Some((Kind::Start, _)), Some(session)) => self.started(session),
                    _ => Vec::new(),
                }
            }
            "command_rejected" => {
                if let Some(id) = command_id {
                    let message = hub_string(&hub.payload, "message").unwrap_or_default();
                    if !self.history_rejected(&id, &message) {
                        self.rejected(&id, message);
                    }
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// `start` was accepted: attach with a `full` connection, and ask for
    /// the session's `/` commands. A started session is the attached one.
    fn started(&mut self, session: SessionId) -> Vec<String> {
        self.attach(session.clone());
        vec![
            self.subscribe(&session, Level::Full),
            self.ask_commands(&session),
        ]
    }

    /// A command the terminal sent was rejected: a notice, and its
    /// text back in the draft when the draft is empty. A rejected `cancel`
    /// shows nothing; after a rejected `start` the next Enter tries again.
    fn rejected(&mut self, id: &str, message: String) {
        match self.pending.get(id) {
            None => {}
            // Another client may have dropped or amended the row first.
            Some((Kind::Cancel | Kind::SteerDrop, _)) => {
                self.pending.remove(id);
            }
            Some((
                Kind::Start
                | Kind::Prompt
                | Kind::Steer
                | Kind::Reply
                | Kind::Shell
                | Kind::Command,
                _,
            )) => {
                self.notices.push(message);
                self.fail(id);
            }
        }
    }

    /// Drops a pending command, returning its text to an empty draft.
    fn fail(&mut self, id: &str) {
        let Some((kind, text)) = self.pending.remove(id) else {
            return;
        };
        if self.draft.is_empty() {
            self.draft.paste(&text);
        }
        if kind == Kind::Start {
            self.phase = Phase::Starting;
        }
        if kind == Kind::Reply {
            self.queue.restore(id);
        }
    }

    /// Folds one session line, returning command lines to send.
    fn on_session(&mut self, envelope: &Envelope) -> Vec<String> {
        // The queue takes every session's requests; everything else is the
        // attached session's alone.
        if approvals::KINDS.contains(&envelope.kind.as_str()) {
            self.queue.fold(envelope);
        }
        if self.session() != Some(&envelope.session_id) {
            return Vec::new();
        }
        let mut send = Vec::new();
        if envelope.kind == "turn_started"
            && let Some(started) = read!(envelope, TurnStarted)
        {
            self.history.saw(&envelope.session_id, &started.input);
        }
        let applied = self.pages.apply(envelope);
        let mut changed = applied.changed;
        match envelope.kind.as_str() {
            "command_accepted" => {
                if let Some(accepted) = read!(envelope, CommandAccepted) {
                    let sent = self.pending.remove(&accepted.command_id.0);
                    self.commands_answered(&accepted);
                    let shell = sent.filter(|(kind, _)| *kind == Kind::Shell);
                    let item = shell.and_then(|(_, text)| shell::answered(&text, accepted.result));
                    changed |= self.pages.add_shell(item);
                }
            }
            "shell_command" => {
                if let Some(ran) = read!(envelope, ShellCommand) {
                    changed |= self.pages.add_shell(Some(shell::ran(&ran)));
                }
            }
            "command_rejected" => {
                if let Some(rejected) = read!(envelope, CommandRejected)
                    && let Some(id) = rejected.command_id
                {
                    self.rejected(&id.0, rejected.message);
                }
            }
            "reloaded" => {
                if self.link == Link::Up {
                    send.push(self.ask_commands(&envelope.session_id));
                }
            }
            "session_named" => {
                if let Some(named) = read!(envelope, SessionNamed) {
                    self.name = named.name;
                }
            }
            "notice" => {
                if let Some(notice) = read!(envelope, Notice) {
                    self.notices.push(notice.message);
                }
            }
            "steering_queue" => {
                if let Some(queue) = read!(envelope, SteeringQueue) {
                    self.steering.fold(&queue, &mut self.draft);
                }
            }
            _ => {}
        }
        if let Some(busy) = applied.busy {
            self.set_busy(busy);
        }
        if changed {
            self.scroll.changed();
        }
        send
    }

    fn set_busy(&mut self, busy: bool) {
        if let Phase::Attached { busy: flag, .. } = &mut self.phase {
            *flag = busy;
        }
    }
}

fn hub_string(payload: &Map<String, Value>, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_owned)
}
