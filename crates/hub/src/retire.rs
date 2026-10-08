//! A retiring relay's queue and the acknowledgement order across a
//! re-route (`docs/invocation.md`, "What the hub speaks"): red-commit
//! seams for the tests pinning that order. The fix commit wires them up.
//!
//! One connection's commands live per session in the order it read them,
//! and each re-route is recorded; the fix commit's retiring relays wait on
//! that record so acknowledgements reach the client in command order.

//! Seams only: nothing here runs in production yet.
#![allow(dead_code, reason = "red-commit seams: the fix commit wires these up")]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use contract::CommandId;
use serde_json::{Map, Value};

use crate::connection::lock;

/// Why a relay no longer takes writes: it saw the exited window, or a
/// write to it failed.
pub(crate) enum Retire {
    /// Its first re-route: the session is exiting.
    Exited,
    /// A write to it failed: the session is gone.
    Dead,
}

#[derive(Default)]
pub(crate) struct AckOrder {
    inner: Mutex<Inner>,
    changed: Condvar,
}

/// What [`AckOrder`] tracks: the unanswered commands per session in read
/// order, and, per command a retiring relay re-routed, the session and
/// epoch that handed it over. A re-routed command stays recorded until
/// its answer is forwarded or refused, or it is dropped without one.
#[derive(Default)]
struct Inner {
    pending: HashMap<String, VecDeque<String>>,
    rerouted: HashMap<String, (String, u64)>,
}

impl AckOrder {
    /// Enqueues `id` on `session`'s queue in read order, once: a recovered
    /// command routed again is already queued, so it is left where it was
    /// read.
    pub(crate) fn enqueue(&self, session: &str, id: &str) {
        let mut inner = lock(&self.inner);
        if inner
            .pending
            .values()
            .any(|queued| queued.contains(&id.to_owned()))
        {
            return;
        }
        inner
            .pending
            .entry(session.to_owned())
            .or_default()
            .push_back(id.to_owned());
    }

    /// The session whose queue holds `id`, if any: a command the session
    /// answers when it ends is never queued, so it is forwarded at once.
    pub(crate) fn session_for(&self, id: &str) -> Option<String> {
        lock(&self.inner)
            .pending
            .iter()
            .find(|(_, queued)| queued.contains(&id.to_owned()))
            .map(|(session, _)| session.clone())
    }

    /// Records `id` as re-routed by `session`'s relay `epoch`: while it
    /// stays recorded, that retiring relay holds back its own later
    /// acknowledgements. Anything but a `shell` ending is recorded: a
    /// command the session answers when it ends never holds another back.
    pub(crate) fn mark_rerouted(&self, id: &str, session: &str, epoch: u64) {
        lock(&self.inner)
            .rerouted
            .insert(id.to_owned(), (session.to_owned(), epoch));
    }

    /// Whether `line` is a command the session answers when it ends, such
    /// as `shell`: never queued and never recorded when re-routed.
    pub(crate) fn is_shell_ended(line: &Map<String, Value>) -> bool {
        line.get("command").and_then(Value::as_str) == Some("shell")
    }

    /// Waits until nothing `session`'s relay `epoch` re-routed, except `id`
    /// itself, is still unanswered: every earlier handover has been
    /// forwarded or refused. Holds no relay or client lock while parked;
    /// only a retiring relay ever waits, for answers only a live relay
    /// gives, so the wait cannot deadlock. A queued command that never
    /// reached a session is not recorded, so passing it on after a
    /// forward cannot deadlock either.
    pub(crate) fn wait_rerouted(&self, session: &str, epoch: u64, id: &str) {
        let mut inner = lock(&self.inner);
        while inner
            .rerouted
            .iter()
            .any(|(rerouted, (at, from))| rerouted != id && at == session && *from == epoch)
        {
            inner = self
                .changed
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Drops `id` from its session's queue and unrecords it as re-routed,
    /// waking every waiter: the acknowledgement was forwarded or refused.
    pub(crate) fn done(&self, session: &str, id: &str) {
        self.drop_ids(&[(session.to_owned(), id.to_owned())]);
    }

    /// Drops every `(session, id)` without an acknowledgement, unrecording
    /// it as re-routed, and wakes every waiter: an unanswered command at
    /// a relay thread's end holds back nothing behind it.
    pub(crate) fn drop_ids(&self, ids: &[(String, String)]) {
        if ids.is_empty() {
            return;
        }
        let mut inner = lock(&self.inner);
        for (session, id) in ids {
            if let Some(queued) = inner.pending.get_mut(session) {
                queued.retain(|queued| *queued != *id);
            }
            inner.rerouted.remove(id);
        }
        inner.pending.retain(|_, queued| !queued.is_empty());
        self.changed.notify_all();
    }
}

/// Enqueues a new command on the session's acknowledgement queue, except
/// one the session answers when it ends, such as `shell`, which is
/// acknowledged then and holds back nothing behind it.
pub(crate) fn enqueue_new(
    order: &Arc<AckOrder>,
    session: &str,
    id: &CommandId,
    stripped: &Map<String, Value>,
) {
    if AckOrder::is_shell_ended(stripped) {
        return;
    }
    order.enqueue(session, &id.0);
}

/// What one handover moved, for tests.
#[cfg(test)]
pub(crate) enum PassOn {
    /// A queued command popped, about to be routed.
    Popped(String),
    /// Queued commands cleared after a disconnect, passed on nowhere.
    Dropped(usize),
}
