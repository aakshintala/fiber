//! A connection's relays and its kept subscriptions (see the parent
//! module for what a subscription is kept for): the state held under the
//! connection's relays lock.

use std::collections::HashMap;
use std::net::Shutdown;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

use super::{Relay, line_bytes, relay_slot, write_all};
use crate::connection::lock;
use crate::first::First;
use crate::retire::{AckOrder, Retire};

/// A connection's relays, the last epoch minted on it, and the last
/// `subscribe` each session accepted from it. Epochs are never reused for the
/// connection's lifetime, so a stale relay thread never drops the entry of
/// a reconnect to the same session.
#[derive(Default)]
pub(crate) struct Relays {
    pub(crate) entries: Vec<Relay>,
    pub(crate) minted: u64,
    /// One gate per session this connection opened: an opener holds its
    /// session's gate from its check until its attach publishes, so two
    /// openers never publish two relays for one session (see #1462).
    pub(crate) gates: HashMap<String, Arc<Mutex<()>>>,
    /// Tests only: pauses after a transfer write, before its result returns.
    #[cfg(test)]
    pub(crate) after_transfer_write: Option<Box<dyn FnOnce() + Send>>,
    /// Tests only: a one-shot pause in `route` between keeping a command
    /// as sent and writing it, with the relays lock held.
    #[cfg(test)]
    pub(crate) before_command_write: Option<Box<dyn FnOnce() + Send>>,
    /// Tests only: a one-shot signal an opener calls after cloning its
    /// session's gate and dropping the relays lock, immediately before
    /// blocking on the gate.
    #[cfg(test)]
    pub(crate) at_gate: Option<Box<dyn FnOnce() + Send>>,
    /// Tests only: a one-shot pause in `closed::on_end` after the decision
    /// and before the drop, taken under the relays lock and called with no
    /// lock held.
    #[cfg(test)]
    pub(crate) at_close: Option<Box<dyn FnOnce() + Send>>,
    /// Per session, the last `subscribe` it accepted, without its
    /// `session_id`: what a reconnect sends again.
    pub(crate) subscribed: Vec<(String, Map<String, Value>)>,
    /// One connection's commands per session in read order, except
    /// commands the session answers when they end: acknowledgements wait
    /// on it so the client reads them in command order across a re-route.
    pub(crate) order: Arc<AckOrder>,
    /// Per session this connection started with `content`, the first
    /// prompt that waits for its `full` subscription (`crate::first`).
    pub(crate) awaiting: Vec<(String, Arc<First>)>,
    /// What the rejoin sweep needs: each session's opening marks and marks.
    pub(crate) rejoin: crate::rejoin::Rejoin,
}

impl Relays {
    /// The next relay epoch: one past every epoch minted on this connection.
    pub(crate) fn mint(&mut self) -> u64 {
        self.minted += 1;
        self.minted
    }

    /// `session`'s open gate, inserting it when absent: the opener clones
    /// this Arc under the relays lock, drops that lock, then holds the
    /// gate until its attach publishes.
    pub(crate) fn gate(&mut self, session: &str) -> Arc<Mutex<()>> {
        self.gates.get(session).cloned().unwrap_or_else(|| {
            let gate = Arc::new(Mutex::new(()));
            self.gates.insert(session.to_owned(), Arc::clone(&gate));
            gate
        })
    }

    /// Drops a finished relay thread's entry: its own session and epoch
    /// only, so a stale thread never drops a reconnect's entry.
    pub(crate) fn finish(&mut self, session: &str, epoch: u64) {
        if let Some(at) = relay_slot(&self.entries, session, epoch) {
            self.entries.remove(at);
        }
    }

    /// Shuts down every relay stream: each session sees this client leave.
    pub(crate) fn close_all(&mut self) {
        self.rejoin.close();
        for entry in self.entries.drain(..) {
            match entry.writer.shutdown(Shutdown::Both) {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Replaces `session`'s kept subscription with `line` when it is an
    /// accepted `subscribe` from the relay still in the map: a stale
    /// relay thread's buffered acknowledgement never overwrites the
    /// replacement's level. A relay retiring as `Exited` keeps nothing:
    /// its entry is already gone by the time it reads an acceptance, so
    /// the acknowledgement is still forwarded but the level is not kept.
    /// Adds it when none is kept. Anything else
    /// changes nothing. When the level is `full`, removes and returns the
    /// first prompt waiting for it, for the caller to release once the
    /// relays lock is dropped.
    pub(super) fn accepted(
        &mut self,
        session: &str,
        epoch: u64,
        line: Map<String, Value>,
    ) -> Option<Arc<First>> {
        let at = relay_slot(&self.entries, session, epoch)?;
        if self.entries.get(at).is_some_and(|entry| {
            entry
                .retiring
                .as_ref()
                .is_some_and(|retiring| matches!(retiring, Retire::Exited))
        }) {
            return None;
        }
        if line.get("command").and_then(Value::as_str) != Some("subscribe") {
            return None;
        }
        let full = line
            .get("args")
            .and_then(|args| args.get("level"))
            .and_then(Value::as_str)
            == Some("full");
        keep_subscription(self, session, line);
        if !full {
            return None;
        }
        let at = self
            .awaiting
            .iter()
            .position(|(waiting, _)| waiting == session)?;
        Some(self.awaiting.remove(at).1)
    }

    /// Transfers the kept level `line` onto `session`'s live relay:
    /// sends it under a hub-minted id the relay thread drops, and keeps it
    /// for the session. A relay already holding the level is left alone.
    /// The id registers before the write: the running relay answers even
    /// an instant reply after the registration, never before it, so its
    /// acknowledgement never leaks to the client. With only retiring
    /// relays the level is kept and nothing is opened: the next open
    /// replays it, and a client command queues on the retiring relay
    /// meanwhile. True when nothing more
    /// is needed: the level is kept, and either no relay exists or the
    /// existing one carries it. False when no relay exists and the level
    /// is only kept: the caller connects one.
    pub(crate) fn transfer(&mut self, session: &str, line: &Map<String, Value>) -> bool {
        if self.subscription(session).is_some() {
            return true;
        }
        keep_subscription(self, session, line.clone());
        let at = self
            .entries
            .iter()
            .position(|entry| entry.session == session && entry.retiring.is_none());
        let Some(at) = at else {
            // No live relay: with only retiring relays the next open
            // replays the kept level, so nothing is opened here. With no
            // relay at all the caller connects one.
            return self.entries.iter().any(|entry| entry.session == session);
        };
        let mut line = line.clone();
        let minted = crate::start::mint("c_");
        line.insert("id".to_owned(), Value::String(minted.clone()));
        let Some(bytes) = line_bytes(&line) else {
            return true;
        };
        let Some(entry) = self.entries.get_mut(at) else {
            return true;
        };
        lock(&entry.replayed).push(minted.clone());
        if write_all(&entry.writer, &bytes).is_err() {
            lock(&entry.replayed).retain(|muted| *muted != minted);
            // The relay died with its socket: it keeps no more writes,
            // but its thread still reads every buffered answer. The
            // thread handle stays in the entry.
            entry.retiring = Some(Retire::Dead);
            match entry.writer.shutdown(Shutdown::Write) {
                Ok(()) | Err(_) => {}
            }
            return true;
        }
        #[cfg(test)]
        if let Some(after_write) = self.after_transfer_write.take() {
            after_write();
        }
        true
    }

    /// `session`'s kept subscription, if any.
    pub(crate) fn subscription(&self, session: &str) -> Option<Map<String, Value>> {
        self.subscribed
            .iter()
            .find(|(kept, _)| kept == session)
            .map(|(_, line)| line.clone())
    }
}

/// Keeps `line` as `session`'s subscription, replacing the kept one.
pub(super) fn keep_subscription(relays: &mut Relays, session: &str, line: Map<String, Value>) {
    if let Some((_, kept)) = relays
        .subscribed
        .iter_mut()
        .find(|(kept, _)| kept == session)
    {
        *kept = line;
    } else {
        relays.subscribed.push((session.to_owned(), line));
    }
}
