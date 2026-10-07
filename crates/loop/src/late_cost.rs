//! A cost that settles late (`docs/model-routing.md`, "Cost"): for a call
//! recorded without the vendor's own figure, one `cost()` lookup 30 seconds
//! later on the session's clock. A worker thread waits and calls; only the
//! loop thread writes what it settles (`docs/architecture.md`,
//! "Streaming").

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::events::UsageRecorded;
use contract::provider::CostLookup;
use contract::{ActionId, GenerationId, TurnId};

use crate::progress::SharedWake;

/// How long after a call's first record its lookup runs.
pub(crate) const LOOKUP_AFTER: Duration = Duration::from_secs(30);

/// A second record whose cost the lookup returned, with the turn and action
/// the first record's envelope carried.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Settled {
    /// The first record with only its `cost` replaced.
    pub(crate) record: UsageRecorded,
    /// The first record's turn.
    pub(crate) turn: Option<TurnId>,
    /// The first record's action.
    pub(crate) action: Option<ActionId>,
}

/// One lookup waiting for its due instant.
struct Pending {
    lookup: Arc<dyn CostLookup>,
    settled: Settled,
}

/// What the worker and the loop thread share, under one lock.
#[derive(Default)]
struct State {
    /// Lookups by due instant, ties in scheduling order.
    pending: BTreeMap<(Instant, u64), Pending>,
    /// Settled records, in settle order, for the loop thread to write.
    settled: Vec<Settled>,
    /// Set once: no lookup is dispatched and no cost is pushed after it.
    stopped: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Arc<SharedWake>,
    /// Holds the worker between taking a lookup and calling it.
    #[cfg(test)]
    pause: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// The session's late lookups: at most one per `generation_id`, never
/// retried, run in due order one at a time on one worker thread started on
/// the first lookup.
#[derive(Default)]
pub(crate) struct LateCost {
    shared: Option<Arc<Shared>>,
    scheduled: BTreeSet<GenerationId>,
    order: u64,
    #[cfg(test)]
    pause: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl LateCost {
    /// Queues `lookup` for `first`'s generation, due [`LOOKUP_AFTER`] from
    /// `clock`'s now, and returns at once. A generation already scheduled
    /// this session is ignored. Err when the worker thread could not start;
    /// the lookup is then dropped, and the next schedule tries again.
    pub(crate) fn schedule(
        &mut self,
        lookup: Arc<dyn CostLookup>,
        first: UsageRecorded,
        turn: Option<TurnId>,
        action: Option<ActionId>,
        clock: &Arc<dyn Clock>,
    ) -> Result<(), std::io::Error> {
        if !self.scheduled.insert(first.generation_id.clone()) {
            return Ok(());
        }
        let due = clock.now() + LOOKUP_AFTER;
        self.order += 1;
        let pending = Pending {
            lookup,
            settled: Settled {
                record: first,
                turn,
                action,
            },
        };
        let key = (due, self.order);
        if let Some(shared) = &self.shared {
            lock(&shared.state).pending.insert(key, pending);
            shared.wake.wake();
            return Ok(());
        }
        // A new worker finds its first lookup already queued, so it parks
        // once, at that lookup's due.
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                pending: BTreeMap::from([(key, pending)]),
                ..State::default()
            }),
            wake: Arc::default(),
            #[cfg(test)]
            pause: self.pause.clone(),
        });
        let worker = Arc::clone(&shared);
        let clock = Arc::clone(clock);
        std::thread::Builder::new()
            .name("late-cost".into())
            .spawn(move || work(&worker, clock.as_ref()))?;
        self.shared = Some(shared);
        Ok(())
    }

    /// The records settled since the last take, in settle order.
    pub(crate) fn take_settled(&mut self) -> Vec<Settled> {
        match &self.shared {
            Some(shared) => std::mem::take(&mut lock(&shared.state).settled),
            None => Vec::new(),
        }
    }

    /// Stops the worker: no lookup is dispatched after this returns, and a
    /// lookup already running has its result dropped. What already settled
    /// stays for [`LateCost::take_settled`]. The worker is never joined, so
    /// a session's end never waits on a lookup.
    pub(crate) fn stop(&mut self) {
        if let Some(shared) = &self.shared {
            lock(&shared.state).stopped = true;
            shared.wake.wake();
        }
    }
}

impl Drop for LateCost {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What the worker does next.
enum Next {
    /// Calls this lookup, taken from the queue.
    Run(Box<Pending>),
    /// Parks until this instant, or until woken when `None`.
    Park(Option<Instant>),
    /// Ends the thread.
    Stop,
}

/// Under the lock: taking a due lookup off the queue is its dispatch.
fn next(state: &mut State, now: Instant) -> Next {
    if state.stopped {
        return Next::Stop;
    }
    let Some(entry) = state.pending.first_entry() else {
        return Next::Park(None);
    };
    let due = entry.key().0;
    if now >= due {
        Next::Run(Box::new(entry.remove()))
    } else {
        Next::Park(Some(due))
    }
}

/// The worker: waits on the session's clock, calls each lookup as it comes
/// due with the lock released, and pushes any returned cost unless stopped.
fn work(shared: &Shared, clock: &dyn Clock) {
    let wake: Arc<dyn Wake> = shared.wake.clone();
    clock.subscribe(Arc::downgrade(&wake));
    loop {
        let next = next(&mut lock(&shared.state), clock.now());
        match next {
            Next::Stop => return,
            Next::Park(until) => shared.wake.park(clock, until),
            Next::Run(pending) => {
                #[cfg(test)]
                if let Some(pause) = &shared.pause {
                    pause();
                }
                let Pending {
                    lookup,
                    mut settled,
                } = *pending;
                let cost = lookup.cost(&settled.record.generation_id);
                let mut state = lock(&shared.state);
                if state.stopped {
                    return;
                }
                if let Some(cost) = cost {
                    settled.record.cost = Some(cost);
                    state.settled.push(settled);
                }
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic aborts the process (`docs/code-quality.md`, "Panics"), so no
    // holder can leave the lock poisoned.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "late_cost_tests.rs"]
mod tests;
