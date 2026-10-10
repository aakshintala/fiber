//! The first prompt of a `start` with `content` (`docs/invocation.md`,
//! "What the hub speaks"): the hub answers `start` at once, then sends the
//! prompt on its own `summary` connection when the requesting connection's
//! `full` subscription is accepted, when that connection closes, or
//! [`FIRST_PROMPT_WAIT`] after the answer, whichever comes first.
//!
//! Lock order: the tick lock, then `First::deadline`. Nothing holds the
//! deadline guard while it takes the tick lock, and nothing holds the tick
//! lock while it takes the relays lock.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};

use crate::connection::{Hub, lock};
use crate::relay::Relays;
use crate::start::{self, Held};
use crate::tick::{Tick, Wait};

/// How long a first prompt waits for the requesting connection's `full`
/// subscription, from the `start` answer (`docs/invocation.md`, `start`).
pub(crate) const FIRST_PROMPT_WAIT: Duration = Duration::from_secs(1);

/// Tests only: a one-shot pause.
#[cfg(test)]
pub(crate) type Hook = Mutex<Option<Box<dyn FnOnce() + Send>>>;

/// When a first prompt may go: released once by an accepted `full`
/// subscribe on the requesting connection or by its close, or due at the
/// deadline armed after the `start` answer.
pub(crate) struct First {
    released: AtomicBool,
    /// `None` until armed: an unarmed wait has no deadline to expire.
    deadline: Mutex<Option<Instant>>,
    tick: Arc<Tick>,
    /// Tests only: runs in the waiter's first check, under the tick lock,
    /// before the deadline is read.
    #[cfg(test)]
    pub(crate) before_check: Hook,
    /// Tests only: runs in `arm` after the deadline guard is dropped,
    /// before the tick is woken.
    #[cfg(test)]
    pub(crate) armed: Hook,
}

impl First {
    pub(crate) fn new(tick: Arc<Tick>) -> Arc<Self> {
        Arc::new(Self {
            released: AtomicBool::new(false),
            deadline: Mutex::new(None),
            tick,
            #[cfg(test)]
            before_check: Mutex::new(None),
            #[cfg(test)]
            armed: Mutex::new(None),
        })
    }

    /// Lets the prompt go now: stores the flag, then wakes the tick.
    pub(crate) fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.tick.wake();
    }

    /// Sets the deadline, the first call only, then wakes the tick with
    /// the deadline guard already dropped.
    pub(crate) fn arm(&self, deadline: Instant) {
        {
            let mut armed = lock(&self.deadline);
            if armed.is_none() {
                *armed = Some(deadline);
            }
        }
        #[cfg(test)]
        if let Some(hook) = lock(&self.armed).take() {
            hook();
        }
        self.tick.wake();
    }

    /// Returns once released, or once `clock` reads the armed deadline.
    fn wait(&self, clock: &dyn Clock) {
        self.tick.wait_for(clock, &mut |now| {
            #[cfg(test)]
            if let Some(hook) = lock(&self.before_check).take() {
                hook();
            }
            if self.released.load(Ordering::SeqCst) {
                return Wait::Done;
            }
            match *lock(&self.deadline) {
                None => Wait::Until(None),
                Some(deadline) if now >= deadline => Wait::Done,
                Some(deadline) => Wait::Until(Some(deadline)),
            }
        });
    }
}

/// Removes `first`'s entry from `relays.awaiting`: the one removal for a
/// first prompt's awaiting entry, shared by the prompt thread and the
/// spawn-failure path so the predicate exists once.
pub(crate) fn forget(relays: &Mutex<Relays>, first: &Arc<First>) {
    lock(relays)
        .awaiting
        .retain(|(_, entry)| !Arc::ptr_eq(entry, first));
}

/// Starts `hub-first-prompt`: it waits for `first`, removes `first`'s
/// entry from `relays.awaiting` if a release has not already, then sends
/// `held`'s prompt once. A rejection is already logged by
/// [`start::prompt`], and the thread holds no client writer, so nothing
/// reaches a client. Err when the thread cannot start.
pub(crate) fn later(
    hub: &Arc<Hub>,
    relays: &Arc<Mutex<Relays>>,
    held: Held,
    first: Arc<First>,
) -> io::Result<()> {
    let hub = Arc::clone(hub);
    let relays = Arc::clone(relays);
    thread::Builder::new()
        .name("hub-first-prompt".to_owned())
        .spawn(move || {
            first.wait(hub.clock.as_ref());
            forget(&relays, &first);
            match start::prompt(held, &hub) {
                Ok(()) | Err(_) => {}
            }
        })
        .map(drop)
}

#[cfg(test)]
#[path = "first_tests.rs"]
mod tests;
