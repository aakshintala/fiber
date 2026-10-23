//! The commands waiting for their answer: the link they go out on, the
//! drafts they carry, the lines held until the hub connects, and the
//! written lines a reconnect sends again (`docs/tui.md`, "A dropped
//! connection"; `docs/invocation.md`, "The command line").
//!
//! The outbox owns only this correlation state: applying a failure stays
//! with `App`, which owns the queue, the offer, the phase and the draft.

use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};

use contract::SessionId;
use serde_json::{Value, json};

use crate::import_report::ImportSource;
use crate::input::Draft;

/// What the terminal is attached to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Phase {
    /// No session yet.
    Starting,
    /// `start` sent, or held until the hub connects; waiting for its answer.
    Pending {
        /// The `start` command's id.
        command_id: String,
    },
    /// Attached to a session.
    Attached {
        /// The session.
        session: SessionId,
        /// Whether a turn is running.
        busy: bool,
    },
}

/// Which command the terminal sent and waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Start,
    Prompt,
    Steer,
    Cancel,
    Reply,
    SteerDrop,
    Shell,
    /// A built-in command such as `handoff`, `reload` or `close`.
    Command,
    /// An `import` for a named source or both; its answer's report is
    /// titled from it.
    Import(ImportSource),
}

/// The hub connection, as the terminal sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Link {
    /// Not connected yet: an Enter is held until `hub_hello`.
    #[default]
    Waiting,
    /// The hub spoke a `hub_hello` this terminal reads.
    Up,
    /// The hub could not be reached or hung up; the terminal retries with
    /// backoff (`docs/tui.md`, "A dropped connection").
    Down,
    /// The hub runs a schema this terminal cannot read: never retried.
    Refused,
}

/// One session command written with no answer yet.
#[derive(Debug, Clone)]
pub(crate) struct Kept {
    /// The command's id.
    pub(crate) id: String,
    /// The session it names.
    pub(crate) session: SessionId,
    /// The line as written, sent again unchanged.
    pub(crate) line: String,
}

/// The link, the commands with no answer yet, the lines held until the
/// hub connects, and the written lines a reconnect sends again.
/// `pending` holds every command made through the outbox with no answer
/// yet; `held` holds the made-but-unwritten subset; `kept` holds
/// written-but-unanswered session commands except `subscribe` and
/// `commands`, which a reconnect sends afresh. Kept ids are always
/// pending keys: `start` carries no `session_id`, so it is never kept.
#[derive(Debug, Default)]
pub(crate) struct Outbox {
    /// The hub connection, as the terminal sees it.
    pub(crate) link: Link,
    /// Commands waiting for their answer: kind and the draft they carried.
    pub(crate) pending: HashMap<String, (Kind, Draft)>,
    /// Lines held until the hub connects: the `start` of an early Enter.
    pub(crate) held: Vec<String>,
    /// Session commands written and not yet answered, in the order they
    /// went out; no id appears twice.
    pub(crate) kept: Vec<Kept>,
}

impl Outbox {
    /// Whether the hub spoke a `hub_hello` this terminal reads, and the
    /// connection has not ended since.
    pub(crate) fn is_up(&self) -> bool {
        self.link == Link::Up
    }

    /// Records a made command.
    pub(crate) fn insert(&mut self, id: String, kind: Kind, draft: Draft) {
        self.pending.insert(id, (kind, draft));
    }

    /// Records a made command, sending its line when the link is up and
    /// holding it until `hub_hello` otherwise.
    pub(crate) fn send(
        &mut self,
        id: String,
        kind: Kind,
        draft: Draft,
        line: String,
    ) -> Option<String> {
        self.pending.insert(id, (kind, draft));
        if self.is_up() {
            Some(line)
        } else {
            // An Enter before the hub connects is held, not lost: `start`
            // goes out once the hub speaks `hub_hello`.
            self.held.push(line);
            None
        }
    }

    /// Pops a pending command, returning the kind and draft applying its
    /// failure needs, without reaching the queue, the offer, the phase
    /// or the draft.
    pub(crate) fn fail(&mut self, id: &str) -> Option<(Kind, Draft)> {
        self.pending.remove(id)
    }

    /// A command's answer: its pending entry closes, and its line is no
    /// longer kept for resending.
    pub(crate) fn answered(&mut self, id: &str) -> Option<(Kind, Draft)> {
        self.kept.retain(|kept| kept.id != id);
        self.pending.remove(id)
    }

    /// The kept commands for a reconnect, in the order they went out:
    /// each command's session with its line as written, so the caller
    /// interleaves the subscribes they need.
    pub(crate) fn reconnect_lines(&self) -> Vec<(SessionId, String)> {
        self.kept
            .iter()
            .map(|kept| (kept.session.clone(), kept.line.clone()))
            .collect()
    }

    /// A line was written to the hub: a session command other than
    /// `subscribe` and `commands`, which a reconnect sends afresh, is kept
    /// until its answer arrives.
    pub(crate) fn wrote(&mut self, line: &str) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let (Some(id), Some(session)) = (
            value.get("id").and_then(Value::as_str),
            value.get("session_id").and_then(Value::as_str),
        ) else {
            return;
        };
        if matches!(
            value.get("command").and_then(Value::as_str),
            Some("subscribe" | "commands")
        ) || self.kept.iter().any(|kept| kept.id == id)
        {
            return;
        }
        self.kept.push(Kept {
            id: id.to_owned(),
            session: SessionId(session.to_owned()),
            line: line.to_owned(),
        });
    }

    /// Whether `id` is a written command kept for resending: a failed
    /// resend leaves its pending entry alone, so the next connection sends
    /// it again (`docs/invocation.md`, "The command line").
    pub(crate) fn is_kept(&self, id: &str) -> bool {
        self.kept.iter().any(|kept| kept.id == id)
    }
}

/// A new command id from random bytes, as `doors::mint` makes one: `c_`
/// and 16 hex digits. `tui` keeps its own copy because it may not depend on
/// `doors`. `RandomState` seeds its keys from the operating system's
/// randomness.
pub(crate) fn mint() -> String {
    format!("c_{:016x}", RandomState::new().hash_one(()))
}

/// A command for a session: `session_id` beside `id`, `command` and `args`.
pub(crate) fn session_command(
    id: &str,
    command: &str,
    session: &SessionId,
    args: Option<Value>,
) -> Value {
    let mut line = json!({"id": id, "command": command, "session_id": session.0});
    if let (Some(args), Some(object)) = (args, line.as_object_mut()) {
        object.insert("args".to_owned(), args);
    }
    line
}

/// The `history` args for `segment`'s window, read under the serving
/// session `own`: `session_id` names the segment only for an ancestor
/// range, absent meaning the serving session itself; `from_seq` and
/// `to_seq` bound the window, each absent meaning its open end.
pub(crate) fn history_args(
    own: &SessionId,
    segment: &SessionId,
    from: Option<u64>,
    to: Option<u64>,
) -> Value {
    let mut args = serde_json::Map::new();
    if segment != own {
        args.insert("session_id".to_owned(), Value::String(segment.0.clone()));
    }
    if let Some(from) = from {
        args.insert("from_seq".to_owned(), Value::Number(from.into()));
    }
    if let Some(to) = to {
        args.insert("to_seq".to_owned(), Value::Number(to.into()));
    }
    Value::Object(args)
}

#[cfg(test)]
#[path = "outbox_tests.rs"]
mod tests;
