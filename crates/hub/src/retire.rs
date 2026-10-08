//! A retiring relay's queue: commands read before an answer that
//! re-routes keep their order across the re-route
//! (`docs/invocation.md`, "What the hub speaks"). A relay is retiring once
//! it has seen the exited window (its first re-route) or once a write to
//! it failed. A command routed to a retiring relay whose thread is alive is
//! not written; it is appended to that relay's queue as unsent. The relay's
//! thread passes its leading unsent commands on, one at a time and in
//! order: after a `closing` answer it re-routed, after it has forwarded an
//! acknowledgement to the client, and at its end. The relay leaves the map
//! only once its queue is empty, checked and removed under one lock hold
//! that every enqueue also takes, so no command is lost between the two.
//!
//! A relay whose thread is gone recovers instead of queueing: nobody will
//! pass the queue on, so its unsent commands are routed first, in order,
//! then the caller's own command, each with the caller's bound, so a
//! recovered command never queues behind an older relay's command.
//!
//! Acknowledgements reach the client in the order the connection read the
//! commands across a re-route. Each new command is enqueued on its
//! session's acknowledgement queue in read order, except one the session
//! answers when it ends, such as `shell`, which is acknowledged then. A
//! relay that re-routes records what it handed over; while any of it is
//! still unanswered, that retiring relay holds back its own later
//! acknowledgements until the replacement has answered it, so the
//! replacement never answers a command before the acknowledgement that
//! came ahead of it is written to the client. A live relay never waits,
//! and neither does a refusal: re-routes already run in read order, and a
//! later command answering what an earlier one waits for (its `reply` to
//! an ask, say) must never wait for that earlier acknowledgement. Queued
//! commands that never reached a session are not recorded, so passing
//! them on after a forward cannot deadlock; a dropped command leaves both
//! queues without an acknowledgement, so nothing waits for it. Every wait
//! holds no relay or client lock, and only a retiring relay ever waits,
//! for answers only a live relay gives, so the order cannot deadlock and
//! one thread per relay stays.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use contract::{CommandId, ErrorCode, SessionId};
use serde_json::{Map, Value};

use crate::connection::{Hub, lock};
use crate::relay::{Kept, Relay, Relays, acknowledgement, relay_slot};

/// Why a relay no longer takes writes: it saw the exited window, or a
/// write to it failed.
pub(crate) enum Retire {
    /// Its first re-route: the session is exiting.
    Exited,
    /// A write to it failed: the session is gone.
    Dead,
}

/// One connection's commands per session in the order it read them, except
/// commands the session answers when they end, plus what each retiring
/// relay handed over: a retiring relay holds back its own later
/// acknowledgements until what it re-routed is answered, so the client
/// reads them in command order across both relays.
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

/// Refuses `id` for `session` at once, then leaves the queue: re-routes
/// already run in read order, and a later command answering what an
/// earlier one waits for must never wait for that earlier acknowledgement.
/// A command with no queue entry (one the session answers when it ends)
/// is refused the same way.
pub(crate) fn refuse(
    writer: &Arc<Mutex<UnixStream>>,
    hub: &Arc<Hub>,
    relays: &Arc<Mutex<Relays>>,
    session: &str,
    id: &CommandId,
    code: &ErrorCode,
    message: &str,
) {
    let order = lock(relays).order.clone();
    crate::connection::reject(writer, hub, Some(id), code, message);
    if let Some(queued) = order.session_for(&id.0) {
        order.done(&queued, &id.0);
    } else {
        let _ = session;
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

/// Answers a relayed command `session_not_found` in command order: the
/// open failed, so no relay thread will forward it.
pub(crate) fn refuse_not_found(
    writer: &Arc<Mutex<UnixStream>>,
    hub: &Arc<Hub>,
    relays: &Arc<Mutex<Relays>>,
    session: &str,
    id: &CommandId,
) {
    let refused = crate::resume::not_found(&SessionId(session.to_owned()));
    refuse(
        writer,
        hub,
        relays,
        session,
        id,
        &refused.code,
        &refused.message,
    );
}

/// The only writer of session lines to the client: every session line the
/// relay passes back goes through it. An acknowledgement from a retiring
/// relay waits until what that relay re-routed is answered, so the client
/// reads acknowledgements in command order across the re-route; a live
/// relay's acknowledgement, and anything else, is written at once. It runs
/// the one-shot `before_forward` test hook once any wait is over, with no
/// lock held, so the hook observes the acknowledgement after it is
/// recorded and in order.
pub(crate) fn forward(
    buf: &[u8],
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    hub: &Hub,
    order: &Arc<AckOrder>,
    epoch: u64,
) -> std::io::Result<()> {
    let ordered =
        acknowledgement(buf).and_then(|(id, _)| order.session_for(&id).map(|at| (at, id)));
    if let Some((at, id)) = &ordered {
        let retiring = lock(relays)
            .entries
            .iter()
            .any(|entry| entry.session == *at && entry.epoch == epoch && entry.retiring.is_some());
        if retiring {
            order.wait_rerouted(at, epoch, id);
        }
    }
    #[cfg(test)]
    {
        if let Some(before) = lock(&hub.before_forward).take() {
            before(buf, relays);
        }
    }
    #[cfg(not(test))]
    {
        let _ = (relays, hub);
    }
    let mut out = lock(writer);
    let written = out.write_all(buf).and_then(|()| out.flush());
    drop(out);
    if let Some((session, id)) = &ordered {
        order.done(session, id);
    }
    written
}

/// What one handover moved, for tests.
#[cfg(test)]
pub(crate) enum PassOn {
    /// A queued command popped, about to be routed.
    Popped(String),
    /// Queued commands cleared after a disconnect, passed on nowhere.
    Dropped(usize),
}

/// Passes a retiring relay's queued commands on, oldest first, each to the
/// oldest relay newer than it. Releases the relays lock before each route,
/// so commands read meanwhile queue behind. Never drops the entry: the
/// thread may still read answers, and the end hands over last.
pub(crate) fn pass_on(
    session: &str,
    epoch: u64,
    kept: &Kept,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
) {
    pump(session, epoch, kept, hub, writer, relays, false);
}

/// The thread's end: unanswered commands the session never answered are
/// dropped, as when a live relay's socket closes, and everything queued is
/// passed on. Drops the entry only in the hold that saw the queue empty.
/// A `closing` read after the client disconnected finds no entry: the queue
/// is cleared and nothing is passed on.
pub(crate) fn drain(
    session: &str,
    epoch: u64,
    kept: &Kept,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
) {
    let dropped: Vec<(String, String)> = lock(kept)
        .iter()
        .filter(|(_, _, sent)| *sent)
        .map(|(id, _, _)| (session.to_owned(), id.clone()))
        .collect();
    lock(kept).retain(|(_, _, sent)| !sent);
    if !dropped.is_empty() {
        let order = lock(relays).order.clone();
        order.drop_ids(&dropped);
    }
    pump(session, epoch, kept, hub, writer, relays, true);
}

/// Retires the relay for its first re-route, without dropping its entry:
/// commands read meanwhile queue on it in order. False when the entry is
/// gone: the client disconnected, and nothing more is done with the
/// command.
pub(crate) fn mark_retiring(relays: &Arc<Mutex<Relays>>, session: &str, epoch: u64) -> bool {
    let mut held = lock(relays);
    match relay_slot(&held.entries, session, epoch) {
        Some(at) => {
            if let Some(entry) = held.entries.get_mut(at) {
                entry.retiring = Some(Retire::Exited);
            }
            true
        }
        None => false,
    }
}

/// Retires the relay for its first re-route and records what it handed
/// over, unless the session answers it when it ends: while a handover
/// stays recorded, that retiring relay holds back its own later
/// acknowledgements. A refusal below unrecords it again through its
/// acknowledgement. False when the entry is gone: the client
/// disconnected, and nothing more is done with the command.
pub(crate) fn hand_over(
    order: &Arc<AckOrder>,
    relays: &Arc<Mutex<Relays>>,
    session: &str,
    epoch: u64,
    id: &str,
    line: &Map<String, Value>,
) -> bool {
    if !mark_retiring(relays, session, epoch) {
        return false;
    }
    if !AckOrder::is_shell_ended(line) {
        order.mark_rerouted(id, session, epoch);
    }
    true
}

/// Whether the relay still has its entry and is retiring: its thread
/// passes its queue on once the acknowledgement ahead of it is forwarded.
pub(crate) fn is_retiring(relays: &Arc<Mutex<Relays>>, session: &str, epoch: u64) -> bool {
    lock(relays)
        .entries
        .iter()
        .any(|entry| entry.session == session && entry.epoch == epoch && entry.retiring.is_some())
}

/// Routes a dead relay's unsent commands, in the order they were read,
/// then the caller's own command. Each keeps the caller's bound, never the
/// top, so a recovered command cannot queue behind an older command. A
/// recovered command opens by connect or resume, like a client command.
#[allow(
    clippy::too_many_arguments,
    reason = "the dead relay, the command, its session, the hub, the client and the relays are one hand-off"
)]
pub(crate) fn recover(
    entry: Relay,
    id: &CommandId,
    session: &str,
    stripped: Map<String, Value>,
    exited: bool,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    from: Option<u64>,
) {
    let queued: Vec<(String, Map<String, Value>)> = lock(&entry.kept)
        .iter()
        .filter(|(_, _, sent)| !sent)
        .map(|(queued, line, _)| (queued.clone(), line.clone()))
        .collect();
    drop(entry);
    for (queued, line) in queued {
        crate::relay::route(
            &CommandId(queued),
            session,
            line,
            hub,
            writer,
            relays,
            from,
            false,
        );
    }
    crate::relay::route(id, session, stripped, hub, writer, relays, from, exited);
}

fn pump(
    session: &str,
    epoch: u64,
    kept: &Kept,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    finish: bool,
) {
    loop {
        let next = {
            let mut held = lock(relays);
            if relay_slot(&held.entries, session, epoch).is_none() {
                drop(held);
                clear(session, kept, hub, relays);
                return;
            }
            let mut kept = lock(kept);
            // Only the leading unsent commands pass on: a sent command
            // ahead of them still waits for its acknowledgement, which is
            // forwarded first, so the replacement never answers a command
            // before the acknowledgement that came ahead of it.
            let leading = kept.first().is_some_and(|(_, _, sent)| !sent);
            if !leading {
                if finish && kept.is_empty() {
                    held.finish(session, epoch);
                }
                None
            } else {
                let (id, line, _) = kept.remove(0);
                Some((id, line))
            }
        };
        let Some((id, line)) = next else {
            return;
        };
        #[cfg(test)]
        if let Some(passed) = lock(&hub.on_pass_on).take() {
            passed(PassOn::Popped(id.clone()));
        }
        crate::relay::route(
            &CommandId(id),
            session,
            line,
            hub,
            writer,
            relays,
            Some(epoch),
            false,
        );
    }
}

/// Clears the queue whose entry is gone: the client disconnected, so
/// nothing is passed on. Its commands leave the acknowledgement queue
/// without answers, so nothing waits for them.
fn clear(session: &str, kept: &Kept, hub: &Arc<Hub>, relays: &Arc<Mutex<Relays>>) {
    let dropped_ids: Vec<(String, String)> = lock(kept)
        .iter()
        .map(|(id, _, _)| (session.to_owned(), id.clone()))
        .collect();
    let order = lock(relays).order.clone();
    #[cfg(test)]
    let dropped = {
        let mut kept = lock(kept);
        let dropped = kept.iter().filter(|(_, _, sent)| !sent).count();
        kept.clear();
        dropped
    };
    #[cfg(not(test))]
    lock(kept).clear();
    if !dropped_ids.is_empty() {
        order.drop_ids(&dropped_ids);
    }
    #[cfg(test)]
    if let Some(passed) = lock(&hub.on_pass_on).take() {
        passed(PassOn::Dropped(dropped));
    }
    #[cfg(not(test))]
    let _ = hub;
}
