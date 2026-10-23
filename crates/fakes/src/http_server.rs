//! One bounded HTTP/1.1 server core (`docs/testing.md`, "Fakes"): the
//! listener, request budgets and transports every HTTP fake builds on.
//! Each accept attempt runs inside one budget, made before the attempt:
//! accept, head and body charge the same budget, never restarted. The
//! budget runs on the injected clock, with a real hang guard besides it.
//! Expiry names the request; shutdown ends every wait stopped.
//!
//! The core is split at real ownership boundaries: [`server`] owns the
//! listener lifecycle, [`connection`] owns one connection's transport,
//! and [`request`] owns framing. This file keeps the bounds, budgets and
//! waits they share.

#[cfg(test)]
use std::sync::Condvar;
use std::sync::{Arc, mpsc};
#[cfg(test)]
use std::sync::{Mutex, PoisonError};
#[cfg(test)]
use std::thread::ThreadId;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
/// A lock that outlives a panicked holder: plain data, and a panicked
/// holder has already failed its test.
use support::lock;

use contract::clock::Clock;

use crate::clock::SystemClock;
use crate::deadline::Deadline;

pub(crate) mod connection;
pub(crate) mod request;
pub(crate) mod server;
pub(crate) mod tunnel;

pub(crate) use connection::{Connection, FillOutcome, ReplyEnd, StallEnd};
#[cfg(test)]
pub(crate) use connection::{Framing, Pause, PauseEnd, ReadErr, ReadOutcome, classify, pump};
#[cfg(test)]
pub(crate) use server::woken;
pub(crate) use server::{HttpServer, Shared};
pub(crate) use tunnel::TunnelEnd;
#[cfg(test)]
pub(crate) use tunnel::{tunnel_copy, tunnel_handshake};

const REQUEST_WITHIN: Duration = Duration::from_secs(30);
const HANG_GUARD: Duration = Duration::from_secs(45);
const POLL: Duration = Duration::from_millis(1);
/// How long shutdown waits for the serving threads to stop.
pub(crate) const SHUTDOWN_WITHIN: Duration = Duration::from_secs(2);
const READ_CHUNK: usize = 8192;

/// The process clock behind every real hang guard.
static PROCESS: SystemClock = SystemClock;

/// One request's bounds and caps: thirty injected-clock seconds behind a
/// forty-five real-second guard; 64 KiB of request line, 64 KiB of one
/// header or chunk line, 1 MiB over at most 4096 headers, 64 MiB of
/// decoded body, 1 MiB over at most 4096 trailers. Terminators count.
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) request: Duration,
    pub(crate) guard: Duration,
    pub(crate) poll: Duration,
    pub(crate) line: usize,
    pub(crate) frame: usize,
    pub(crate) headers: usize,
    pub(crate) header_count: usize,
    pub(crate) body: usize,
    pub(crate) trailers: usize,
    pub(crate) trailer_count: usize,
}

impl Limits {
    pub(crate) fn default() -> Self {
        Self {
            request: REQUEST_WITHIN,
            guard: HANG_GUARD,
            poll: POLL,
            line: 64 * 1024,
            frame: 64 * 1024,
            headers: 1024 * 1024,
            header_count: 4096,
            body: 64 * 1024 * 1024,
            trailers: 1024 * 1024,
            trailer_count: 4096,
        }
    }
}

/// Whether the listener waits for one named request or renews idle
/// windows: matched explicitly at both expiry edges.
pub(crate) enum AcceptMode {
    Persistent,
    Expected {
        /// The request the wait names on expiry.
        name: String,
    },
}

impl AcceptMode {
    /// The request every failure on this mode names.
    fn name(&self) -> &str {
        match self {
            AcceptMode::Persistent => "the persistent request",
            AcceptMode::Expected { name } => name,
        }
    }
}

/// One request's two clocks, both started before its accept attempt and
/// never restarted: the injected bound the test advances, and the
/// wall-clock guard ending a hang no advance rescues.
pub(crate) struct RequestBudget {
    pub(crate) clock: Arc<dyn Clock>,
    start: Instant,
    duration: Duration,
    guard_start: Instant,
    pub(crate) guard_for: Duration,
}

impl RequestBudget {
    fn start_on(clock: &Arc<dyn Clock>, duration: Duration, guard: Duration) -> Self {
        let cloned = Arc::clone(clock);
        let start = clock.now();
        let guard_start = PROCESS.now();
        Self {
            clock: cloned,
            start,
            duration,
            guard_start,
            guard_for: guard,
        }
    }

    /// Exact expiry counts; one tick before does not.
    fn expired(&self) -> bool {
        self.clock.now().saturating_duration_since(self.start) >= self.duration
    }

    /// The bound for clock parks: `None` only past instant overflow.
    fn until(&self) -> Option<Instant> {
        self.start.checked_add(self.duration)
    }

    fn guard_left(&self) -> Duration {
        self.guard_for
            .saturating_sub(PROCESS.now().saturating_duration_since(self.guard_start))
    }
}

/// One clock-wait acknowledgment, announced after the waiter's clock
/// check: its phase, thread and own sequence. Each phase is announced once
/// per wake generation, so a phase change cannot hide behind an earlier
/// wait's acknowledgment. `until` is the parked bound
/// [`FakeClock::await_thread_parked`](crate::clock::FakeClock::await_thread_parked)
/// matches.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct WaitAck {
    pub(crate) phase: &'static str,
    pub(crate) thread: ThreadId,
    pub(crate) seq: u64,
    pub(crate) until: Option<Instant>,
}

/// One held waiter's announcement: its phase and its own sequence.
#[cfg(test)]
pub(crate) type HeldNote = (&'static str, u64);

/// The test-only check-to-park pause: while armed, a waiter holds after
/// its check, subscription and acknowledgment but before its park. The
/// test owns it like a release channel: no deadline of its own applies
/// while held. Each holder announces itself before it holds, and a pass
/// lets exactly one holder of its phase through while the gate stays
/// armed: a turnstile, so a waiter that sprints to its next hold is held
/// again instead of racing a re-arm.
#[cfg(test)]
pub(crate) struct CheckGate {
    armed: Mutex<bool>,
    changed: Condvar,
    /// Phases passed and not yet taken, one holder each.
    passes: Mutex<Vec<&'static str>>,
    held: mpsc::Sender<HeldNote>,
    notes: Mutex<Option<mpsc::Receiver<HeldNote>>>,
}

#[cfg(test)]
impl CheckGate {
    #[cfg(test)]
    fn new() -> Self {
        let (held, notes) = mpsc::channel();
        Self {
            armed: Mutex::new(false),
            changed: Condvar::new(),
            passes: Mutex::new(Vec::new()),
            held,
            notes: Mutex::new(Some(notes)),
        }
    }

    /// Announces the waiter, then holds it while armed and unpassed. The
    /// note goes out under the armed lock, so a pass or a release cannot
    /// slip between the note and the hold.
    fn hold(&self, phase: &'static str, seq: u64) {
        let mut armed = lock(&self.armed);
        match self.held.send((phase, seq)) {
            Ok(()) | Err(_) => {}
        }
        while *armed && !self.take_pass(phase) {
            armed = self
                .changed
                .wait(armed)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Takes one pass for `phase`: true when one was waiting.
    fn take_pass(&self, phase: &'static str) -> bool {
        let mut passes = lock(&self.passes);
        match passes.iter().position(|passed| *passed == phase) {
            Some(at) => {
                passes.remove(at);
                true
            }
            None => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn arm(&self) {
        *lock(&self.armed) = true;
    }

    #[cfg(test)]
    pub(crate) fn release(&self) {
        *lock(&self.armed) = false;
        self.changed.notify_all();
    }

    /// Lets one holder of `phase` through and stays armed.
    #[cfg(test)]
    pub(crate) fn pass(&self, phase: &'static str) {
        let _armed = lock(&self.armed);
        lock(&self.passes).push(phase);
        self.changed.notify_all();
    }

    /// Every held waiter's announcement from here on, once: `None` past
    /// the first call.
    #[cfg(test)]
    pub(crate) fn held_notes(&self) -> Option<mpsc::Receiver<HeldNote>> {
        lock(&self.notes).take()
    }

    /// Another sender into the held announcements: a test merges its own
    /// events into the same ordered stream.
    #[cfg(test)]
    pub(crate) fn held_sender(&self) -> mpsc::Sender<HeldNote> {
        self.held.clone()
    }
}

/// How one bounded wait ended. The verdict is authoritative: a check
/// before it is only a fast path, so the caller maps expiry itself.
pub(crate) enum WaitEnd {
    Waited,
    Expired,
    Stopped,
}

/// Fails naming the request whose bound ran out waiting for `phase`.
#[allow(
    clippy::panic,
    reason = "a fake's wait; the panic names the request whose bound ran out"
)]
fn expired(budget: &RequestBudget, name: &str, phase: &str) -> ! {
    panic!(
        "fakes: the HTTP core waited {:?} {phase} {name}",
        budget.duration
    );
}

/// The real hang guard ran out waiting for `phase`: fails naming it,
/// even under an injected clock whose budget never moves.
#[allow(
    clippy::panic,
    reason = "a fake's wait; the panic names the request whose real guard ran out"
)]
pub(crate) fn guard_expired(budget: &RequestBudget, name: &str, phase: &str) -> ! {
    panic!(
        "fakes: the HTTP core's real guard of {:?} ran out {phase} {name}",
        budget.guard_for
    );
}

/// Joins `accept` within `within`, leaving a stuck thread blocked rather
/// than hanging the drop. Builds abort on panic, so a join never carries
/// one.
pub(crate) fn join_within(accept: JoinHandle<()>, within: Duration) -> bool {
    let (done, joined) = mpsc::channel::<()>();
    thread::spawn(move || {
        match accept.join() {
            Ok(()) | Err(_) => {}
        }
        match done.send(()) {
            Ok(()) | Err(_) => {}
        }
    });
    Deadline::after(within).recv(&joined).is_ok()
}

#[cfg(test)]
#[path = "http_server_tests.rs"]
mod tests;
