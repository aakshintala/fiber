//! The steering queue one session's threads share
//! (`docs/architecture.md`, "One inbox"): raw steering entries keyed by
//! steer command id in arrival order, behind one lock. The
//! connection-reading thread admits a `steer` into it at once and answers
//! `command_accepted` before the loop ever sees the delivery; the loop
//! thread reconciles each delivery against it at the drain, running the
//! unchanged `before_message` hook pass there. Every section that mutates
//! the queue snapshots it and emits `steering_queue` before releasing the
//! lock, so the last event in the log matches the final queue in every
//! interleaving (`docs/events.md`, `steering_queue`).
//!
//! Lock order is queue, then log, never the reverse: the emitter append
//! runs inside the queue-lock section and takes the log lock leaf-last,
//! and durable writes happen on the loop thread with the queue lock
//! released. No hook ever runs while the queue lock is held: the drain
//! reads what it needs, releases the lock, runs the hook chain, then
//! re-acquires the lock and applies each outcome only if its entry is
//! still present.

use std::collections::{HashSet, VecDeque};
use std::sync::{Mutex, MutexGuard};

use crate::CommandId;
use crate::emit::Emit;
use crate::events::{Event, QueuedMessage, SteeringQueue};
use crate::hook::{Context, StateLine};
use crate::shapes::{ContentPart, Origin, Sender};

/// What the connection-reading thread shares with the loop thread: every
/// steering message admitted but not yet applied or removed, oldest first.
/// `Sync` through the one [`Mutex`]; every method holds it across mutate,
/// snapshot and the emitter append.
pub struct Queue {
    inner: Mutex<Inner>,
    /// A one-shot pause before the real acquisition, armed by one test:
    /// `None` unless the race test holds it, so the shipped path sends
    /// and waits on nothing extra.
    #[cfg(any(test, debug_assertions))]
    entry_gate: Mutex<Option<EntryGate>>,
}

struct Inner {
    entries: VecDeque<Entry>,
    /// Every id the reader answered at once, plus every id the loop took
    /// through its legacy path: the drain's record of which deliveries
    /// were already answered. A reader drop keeps its id here, so the
    /// loop's follow-up delivery is a no-op; every loop-side removal
    /// forgets its id, so a later drop is stale.
    admitted: HashSet<CommandId>,
}

/// One queued steering message: raw client bytes from the reader, plus
/// loop-owned hooked fields written and read only on the loop thread.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The `steer` command that sent it; a `steer_drop` names it.
    pub id: CommandId,
    /// The message as sent, or as a hook replaced it.
    pub content: Vec<ContentPart>,
    /// Where it came from.
    pub sender: Sender,
    /// The hook pass's verdict: `None` until the drain runs `admitted`
    /// over it, `Some` once hooks passed (or the loop admitted it).
    pub staged: Option<Staged>,
}

/// What the loop's hook pass left on an entry: the `before_message`
/// chain's state, replacement owners and contexts, in run order.
#[derive(Debug, Clone, Default)]
pub struct Staged {
    /// The state the hooks took, in run order.
    pub state: Vec<StateLine>,
    /// The extensions whose hook returned content, each once; `Some`
    /// exactly when the entry's content is a hook's.
    pub changed_by: Option<Vec<String>>,
    /// The contexts the hooks added, in run order.
    pub context: Vec<Context>,
}

/// A push for an id the queue already admitted: the caller answers
/// `duplicate_command` for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Duplicate;

/// The one-shot pause the race test arms on the queue's acquisition path:
/// the holder fires `entered` and blocks on `release` before attempting
/// the real lock. The test's release sender drops when the test ends,
/// which releases a still-blocked holder instead of hanging it.
#[cfg(any(test, debug_assertions))]
pub struct EntryGate {
    /// Fired once the holder reaches the gate.
    pub entered: std::sync::mpsc::Sender<()>,
    /// The holder blocks here until the test releases it.
    pub release: std::sync::mpsc::Receiver<()>,
}

impl Queue {
    /// An empty queue admitting nothing.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: VecDeque::new(),
                admitted: HashSet::new(),
            }),
            #[cfg(any(test, debug_assertions))]
            entry_gate: Mutex::new(None),
        }
    }

    /// Arms the one-shot acquisition pause the race test holds.
    #[cfg(any(test, debug_assertions))]
    pub fn hold_entry_for_test(&self, gate: EntryGate) {
        *lock(&self.entry_gate) = Some(gate);
    }

    /// The queue lock, pausing at the test's gate first when armed. The
    /// gate's own lock is released before blocking, so it takes no lock
    /// into the wait.
    fn acquire(&self) -> MutexGuard<'_, Inner> {
        #[cfg(any(test, debug_assertions))]
        {
            let gate = lock(&self.entry_gate).take();
            if let Some(gate) = gate {
                let _sent = gate.entered.send(());
                let _released = gate.release.recv();
            }
        }
        lock(&self.inner)
    }

    /// Admits one raw reader entry, unstaged: the drain still has to run
    /// the hook pass over it. One section: push, snapshot, emit.
    pub fn push(
        &self,
        id: CommandId,
        content: Vec<ContentPart>,
        sender: Sender,
        emit: &dyn Emit,
    ) -> Result<(), Duplicate> {
        let mut inner = self.acquire();
        if !inner.admitted.insert(id.clone()) {
            return Err(Duplicate);
        }
        inner.entries.push_back(Entry {
            id,
            content,
            sender,
            staged: None,
        });
        emit.emit(&queue_event(&inner.entries));
        Ok(())
    }

    /// Admits one loop-side entry whose hook pass already ran: tracked
    /// like a reader push, staged from the start.
    pub fn admit_hooked(
        &self,
        id: CommandId,
        content: Vec<ContentPart>,
        sender: Sender,
        staged: Staged,
        emit: &dyn Emit,
    ) -> Result<(), Duplicate> {
        let mut inner = self.acquire();
        if !inner.admitted.insert(id.clone()) {
            return Err(Duplicate);
        }
        inner.entries.push_back(Entry {
            id,
            content,
            sender,
            staged: Some(staged),
        });
        emit.emit(&queue_event(&inner.entries));
        Ok(())
    }

    /// Admits one session message whose hook pass already ran: staged
    /// from the start, but never tracked in `admitted`, so the id stays
    /// stale to `is_admitted` and drops. A queued entry with the same id
    /// from the same sender is a duplicate. One section: push, snapshot,
    /// emit.
    pub fn admit_session_hooked(
        &self,
        id: CommandId,
        content: Vec<ContentPart>,
        sender: Sender,
        staged: Staged,
        emit: &dyn Emit,
    ) -> Result<(), Duplicate> {
        let mut inner = self.acquire();
        if inner
            .entries
            .iter()
            .any(|entry| entry.id == id && entry.sender.origin == sender.origin)
        {
            return Err(Duplicate);
        }
        inner.entries.push_back(Entry {
            id,
            content,
            sender,
            staged: Some(staged),
        });
        emit.emit(&queue_event(&inner.entries));
        Ok(())
    }

    /// Takes the still-queued non-session entry `id` names off, if any.
    /// Whether one was still queued: the reader answers at once only
    /// then. A taken id stays admitted, so the loop's follow-up delivery
    /// is a no-op; session messages are never taken by id.
    pub fn remove(&self, id: &CommandId, emit: &dyn Emit) -> bool {
        let mut inner = self.acquire();
        let Some(at) = position(&inner.entries, id) else {
            return false;
        };
        inner.entries.remove(at);
        emit.emit(&queue_event(&inner.entries));
        true
    }

    /// Whether `id` names a staged entry from `origin`: hook-passed and
    /// still queued. One lock, no emit: the apply checks each holder
    /// against it, so a drop between the drain and the apply skips the
    /// holder. The origin joins the id because a session message's
    /// command id comes from another session and is not globally unique.
    pub fn is_staged(&self, id: &CommandId, origin: &Origin) -> bool {
        self.acquire().entries.iter().any(|entry| {
            entry.id == *id && entry.sender.origin == *origin && entry.staged.is_some()
        })
    }

    /// Whether the reader or the loop's legacy path already answered `id`.
    pub fn is_admitted(&self, id: &CommandId) -> bool {
        self.acquire().admitted.contains(id)
    }

    /// The still-queued, not-yet-hook-passed non-session entry `id`
    /// names, if any: what the drain runs the hook pass over. Session
    /// entries and already-staged ones are never hook-passed twice.
    pub fn unstaged(&self, id: &CommandId) -> Option<(Vec<ContentPart>, Sender)> {
        self.acquire()
            .entries
            .iter()
            .find(|entry| &entry.id == id && entry.staged.is_none() && !is_session(entry))
            .map(|entry| (entry.content.clone(), entry.sender.clone()))
    }

    /// Writes the hook pass's outcome into the still-queued entry `id`
    /// names. False when the entry is already gone: the outcome is
    /// discarded with no second emit (drop-wins). A replacement re-emits
    /// the queue; an unchanged staging does not.
    pub fn stage(
        &self,
        id: &CommandId,
        content: Vec<ContentPart>,
        staged: Staged,
        emit: &dyn Emit,
    ) -> bool {
        let mut inner = self.acquire();
        let Some(at) = position(&inner.entries, id) else {
            return false;
        };
        let Some(entry) = inner.entries.get_mut(at) else {
            return false;
        };
        let replaced = entry.content != content;
        entry.content = content;
        entry.staged = Some(staged);
        if replaced {
            emit.emit(&queue_event(&inner.entries));
        }
        true
    }

    /// Removes the queued `id` and reports it without forgetting it: an
    /// idle drain moves the entry into the turn's input, keeping its id so
    /// a drop arriving in the same wait still finds it. Emits the queue as
    /// it stands when an entry moved, under the same lock. The turn forgets
    /// the id once it takes the input, so a later drop is stale.
    pub fn move_out(&self, id: &CommandId, emit: &dyn Emit) -> Option<Entry> {
        let mut inner = self.acquire();
        let at = position(&inner.entries, id)?;
        let entry = inner.entries.remove(at)?;
        emit.emit(&queue_event(&inner.entries));
        Some(entry)
    }

    /// Forgets `id` without touching the queue: the entry already left, so
    /// no emit follows. The turn took its input, so a later drop is stale.
    pub fn forget(&self, id: &CommandId) {
        self.acquire().admitted.remove(id);
    }

    /// Removes the non-session entry `id` names and forgets its id,
    /// emitting the queue as the section left it when one was removed.
    /// The loop's hook-pass removals, shutdown and after-close go through
    /// here: a later drop for the id is stale.
    pub fn take(&self, id: &CommandId, emit: &dyn Emit) -> Option<Entry> {
        let mut inner = self.acquire();
        let at = position(&inner.entries, id)?;
        let entry = inner.entries.remove(at)?;
        inner.admitted.remove(id);
        emit.emit(&queue_event(&inner.entries));
        Some(entry)
    }

    /// Snapshots the staged entries, clears them and emits the queue as
    /// the section left it when one was cleared, all in one section, and
    /// forgets their ids. The caller logs what it returns with the lock
    /// released. Unstaged entries stay for their delivery's hook pass;
    /// the emit still lists them, so it never claims an empty queue that
    /// is not. Session entries were never admitted, so only non-session
    /// entries forget: a same-id session entry never clears a steer id
    /// another session reused.
    pub fn take_staged(&self, emit: &dyn Emit) -> Vec<Entry> {
        let mut inner = self.acquire();
        let drained = std::mem::take(&mut inner.entries);
        let mut taken = Vec::new();
        let mut kept = VecDeque::new();
        for entry in drained {
            if entry.staged.is_some() {
                if !is_session(&entry) {
                    inner.admitted.remove(&entry.id);
                }
                taken.push(entry);
            } else {
                kept.push_back(entry);
            }
        }
        inner.entries = kept;
        if !taken.is_empty() {
            emit.emit(&queue_event(&inner.entries));
        }
        taken
    }

    /// Whether a session message waits in the queue: the loop is not idle
    /// while one does (`docs/invocation.md`, "Lifecycle").
    pub fn has_session(&self) -> bool {
        self.acquire().entries.iter().any(is_session)
    }

    /// The queued entries oldest-first, without taking them.
    pub fn snapshot(&self) -> Vec<QueuedMessage> {
        queue_messages(&self.acquire().entries)
    }

    /// Moves every session-message entry out and drops every person
    /// steer still queued, forgetting the dropped ids: after `close` no
    /// person steer runs. Emits the queue, empty, when a message moved;
    /// a persons-only clear stays silent, as the running queue did.
    /// Returns the messages, oldest first.
    pub fn take_for_ending(&self, emit: &dyn Emit) -> Vec<Entry> {
        let mut inner = self.acquire();
        let mut messages = Vec::new();
        for entry in std::mem::take(&mut inner.entries) {
            if is_session(&entry) {
                messages.push(entry);
            } else {
                inner.admitted.remove(&entry.id);
            }
        }
        if !messages.is_empty() {
            emit.emit(&queue_event(&inner.entries));
        }
        messages
    }
}

/// The queue event for `entries`, oldest first.
fn queue_event(entries: &VecDeque<Entry>) -> Event {
    Event::SteeringQueue(SteeringQueue {
        messages: queue_messages(entries),
    })
}

/// The queued messages of `entries`, oldest first.
fn queue_messages(entries: &VecDeque<Entry>) -> Vec<QueuedMessage> {
    entries
        .iter()
        .map(|entry| QueuedMessage {
            content: entry.content.clone(),
            sender: entry.sender.clone(),
        })
        .collect()
}

/// The index of the still-queued non-session entry `id` names, if any.
fn position(entries: &VecDeque<Entry>, id: &CommandId) -> Option<usize> {
    entries
        .iter()
        .position(|entry| &entry.id == id && !is_session(entry))
}

/// Whether `entry` came from another session: never dropped by id.
fn is_session(entry: &Entry) -> bool {
    matches!(entry.sender.origin, Origin::Session { .. })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    support::lock(mutex)
}

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "steering_tests.rs"]
mod tests;
