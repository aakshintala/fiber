//! A connection's relays and its kept subscriptions (see the parent
//! module for what a subscription is kept for): the state held under the
//! connection's relays lock.

use std::collections::HashMap;
use std::net::Shutdown;
use std::sync::{Arc, Mutex, Weak};

use contract::diag::Op;
use log::diag::{Timer, timed};
use serde_json::{Map, Value};

use super::{Relay, line_bytes, relay_slot, write_all};
use crate::connection::{Hub, lock};
use crate::first::First;
use crate::retire::{AckOrder, Retire};

/// One subscription replay's in-flight timer, by the session, the relay
/// epoch and the hub-minted command id that identify it.
pub(crate) type ReplayKey = (String, u64, String);

/// Tests only: the one-shot fault a test arms in `route` between keeping
/// a command as sent and writing it.
#[cfg(test)]
pub(crate) type BeforeCommandWrite = Box<dyn FnOnce(&mut Option<usize>, &[u8]) + Send>;

/// A connection's relays, the last epoch minted on it, and the last
/// `subscribe` each session accepted from it. Epochs are never reused for the
/// connection's lifetime, so a stale relay thread never drops the entry of
/// a reconnect to the same session.
#[derive(Default)]
pub(crate) struct Relays {
    pub(crate) entries: Vec<Relay>,
    /// The accepted client's attribution, reused when commands are rerouted.
    pub(crate) attribution: Option<crate::connection::Attribution>,
    pub(crate) minted: u64,
    /// One gate per session this connection opened: an opener holds its
    /// session's gate from its check until its attach publishes, so two
    /// openers never publish two relays for one session (see #1462).
    pub(crate) gates: HashMap<String, Arc<Mutex<()>>>,
    /// Tests only: pauses after a transfer write, before its result returns.
    /// It pauses the transfer with the relays lock held so the session can
    /// answer in that gap, proving the timer cannot complete before the
    /// transfer returns. The paused tests answer over the socket and
    /// channels, never needing the held lock; no outside seam pauses the
    /// transfer between the write and its result.
    #[cfg(test)]
    pub(crate) after_transfer_write: Option<Box<dyn FnOnce() + Send>>,
    /// Tests only: a one-shot fault in `route` between keeping a command
    /// as sent and writing it, with the relays lock held. The closure
    /// runs before the write and names how many bytes of the command
    /// the writer accepts: `None` writes all, as today. The
    /// stranded-command tests end the session in that gap, and the tripwire
    /// asserts the gap never opens on replay-only paths. The forced party
    /// ends the session over the socket, never needing the held lock; no
    /// outside seam pauses `route` between the keep and the write. The
    /// cut-write tests cap the write below the production byte counter,
    /// so the counter observes the partial write. Replay writes never
    /// consult this hook.
    #[cfg(test)]
    pub(crate) before_command_write: Option<BeforeCommandWrite>,
    /// Tests only: a one-shot pause in `route` after the live command
    /// write returns and before its outcome is published, with the relays
    /// lock held. It pauses the serving thread in that gap so the
    /// acknowledgement thread consumes the in-flight attempt there,
    /// proving the late outcome changes nothing. The acknowledgement runs
    /// over the socket and channels, never needing the held lock; no
    /// outside seam pauses `route` between the write and its outcome.
    #[cfg(test)]
    pub(crate) after_command_write: Option<Box<dyn FnOnce() + Send>>,
    /// Tests only: a one-shot signal an opener calls after cloning its
    /// session's gate and dropping the relays lock, immediately before
    /// blocking on the gate. It marks the opener about to block so the
    /// race test orders the two openers' holds; without the gate the second
    /// never signals contention. No outside seam stages two openers onto
    /// one gate on demand.
    #[cfg(test)]
    pub(crate) at_gate: Option<Box<dyn FnOnce() + Send>>,
    /// Tests only: a one-shot signal when `try_lock` sees another opener
    /// holding the gate, before this opener blocks on it. It signals the
    /// contention the race test requires before the second opener blocks.
    /// No outside seam forces two openers onto one gate on demand.
    #[cfg(test)]
    pub(crate) blocked_gate: Option<Box<dyn FnOnce() + Send>>,
    /// Tests only: a one-shot pause in `closed::on_end` after the decision
    /// and before the drop, taken under the relays lock and called with no
    /// lock held. It parks the closing relay between its loop's end and the
    /// relays lock for the teardown race. Exiting the session acts before
    /// the loop ends or after the lock is taken, never in the gap.
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
    /// The hub whose diagnostic writer times subscription replays, held
    /// weakly so connections never keep it alive: `Some` on debug
    /// production connections, `None` on info ones and on unit fixtures
    /// that bypass connection dispatch.
    pub(crate) timing_hub: Option<Weak<Hub>>,
    /// One in-flight replay timer per outstanding replay, by session,
    /// relay epoch and minted command id: a worker completes only its
    /// own session and epoch, so a stale acknowledgement, another epoch
    /// or a removed relay finishes nothing.
    pub(crate) pending_replay: HashMap<ReplayKey, Timer>,
    /// Replay timers stamped at a failed transfer, in failure order:
    /// the next post-follow drain emits them outside the relays lock.
    /// Each entry's duration is fixed at its failure, so which worker
    /// emits it changes no `ms`.
    pub(crate) completed_replay: Vec<Timer>,
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
        let epoch = self.entries.get(at).map(|entry| entry.epoch).unwrap_or(0);
        let mut line = line.clone();
        let minted = crate::start::mint("c_");
        line.insert("id".to_owned(), Value::String(minted.clone()));
        let Some(bytes) = line_bytes(&line) else {
            return true;
        };
        // The replay's timer registers before the write, beside the muted
        // id below: an instant reply completes it instead of leaking. The
        // key is built only with a timing context, so at info one `Option`
        // check finds no context and nothing is allocated or touched.
        let replay_key: Option<ReplayKey> = if let Some(weak) = self.timing_hub.as_ref() {
            let key = (session.to_owned(), epoch, minted.clone());
            if let Some(hub) = weak.upgrade()
                && let Some(timer) = timed(&hub.diag, Op::HubSubscribeReplay, None, None)
            {
                self.pending_replay.insert(key.clone(), timer);
                Some(key)
            } else {
                None
            }
        } else {
            None
        };
        let wrote = {
            let Some(entry) = self.entries.get_mut(at) else {
                if let Some(key) = replay_key.as_ref() {
                    self.pending_replay.remove(key);
                }
                return true;
            };
            lock(&entry.replayed).push(minted.clone());
            let outcome = write_all(&entry.writer, &bytes);
            let wrote = outcome.is_ok();
            if !wrote {
                // Bytes the failed replay accepted may still draw its
                // acknowledgement: the replay stays muted so it cannot
                // leak to the client. Only a write that accepted nothing
                // removes the muted id.
                if let Err(partial) = &outcome
                    && partial.written == 0
                {
                    lock(&entry.replayed).retain(|muted| *muted != minted);
                }
                // The relay died with its socket: it keeps no more writes,
                // but its thread still reads every buffered answer. The
                // thread handle stays in the entry.
                entry.retiring = Some(Retire::Dead);
                match entry.writer.shutdown(Shutdown::Write) {
                    Ok(()) | Err(_) => {}
                }
            }
            wrote
        };
        if !wrote {
            // Stamped at this failure for the next post-follow drain,
            // which emits it outside the relays lock; the duration never
            // covers the deferred cleanup.
            if let Some(key) = replay_key.as_ref()
                && let Some(mut timer) = self.pending_replay.remove(key)
            {
                timer.stop();
                self.completed_replay.push(timer);
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

    /// Takes the in-flight replay timer the acknowledgement `id` settles,
    /// on `session`'s `epoch` only: an unrelated acknowledgement, another
    /// epoch or a removed slot completes nothing. The caller drops what
    /// comes back after releasing the relays lock, so emission never
    /// happens under it.
    pub(crate) fn take_replay(&mut self, session: &str, epoch: u64, id: &str) -> Option<Timer> {
        self.pending_replay
            .remove(&(session.to_owned(), epoch, id.to_owned()))
    }

    /// Takes every in-flight replay timer of `session`'s `epoch`: what a
    /// terminating worker completes before any follow-on rewind work, so
    /// no guard waits for a healthy connection to close. The caller drops
    /// them after releasing the relays lock.
    pub(crate) fn take_session_replays(&mut self, session: &str, epoch: u64) -> Vec<Timer> {
        let owned: Vec<ReplayKey> = self
            .pending_replay
            .keys()
            .filter(|(kept, kept_epoch, _)| kept == session && *kept_epoch == epoch)
            .cloned()
            .collect();
        owned
            .into_iter()
            .filter_map(|key| self.pending_replay.remove(&key))
            .collect()
    }

    /// Takes every failure-stamped replay timer: what the calling worker
    /// emits after `rewind::follow` returns. Removal under the lock keeps
    /// emission exactly once across concurrent workers.
    pub(crate) fn take_completed_replays(&mut self) -> Vec<Timer> {
        std::mem::take(&mut self.completed_replay)
    }
}

/// Keeps `line` as `session`'s subscription, replacing the kept one. A
/// credential label applies to that subscribe only, so only
/// `args.credential` is stripped from the stored copy: a reconnect or a
/// later attach must not reapply an earlier asked label after another
/// client changed it. The command in flight keeps its args until
/// answered; an absent label is a no-op.
pub(super) fn keep_subscription(relays: &mut Relays, session: &str, line: Map<String, Value>) {
    let mut line = line;
    if let Some(args) = line.get_mut("args").and_then(Value::as_object_mut) {
        args.remove("credential");
    }
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
