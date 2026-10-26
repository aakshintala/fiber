//! Request correlation for one process extension (`docs/extensions.md`,
//! "Process extensions").
//!
//! Every callback Fiber asks the child to run rides one request: a unique
//! id naming the pending callback, the callback's context, and the signal
//! cancelled when the request ends. Stream items — hooks, watcher
//! deliveries and commands — form one ordered queue: each starts only
//! after the reply to the last one. Tool calls run as they arrive, so
//! several can be outstanding and a long tool never holds up the stream.
//!
//! A reply, a deadline and a cancel race at one retirement seam: the first
//! settles the request, and every later one reads stale. Retiring the
//! generation cancels everything pending and bumps the generation, so no
//! id resolves across generations and a late reply or call changes
//! nothing. No registry, state or emitter lock is held over pipe I/O or
//! callback waits; the table is a leaf lock.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use contract::RequestId;
use contract::clock::Clock;
use contract::tool::Cancel;

use super::super::host::admission::CallbackContext;

/// What one pending request asks the child to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum RequestKind {
    /// One stream item: ordered behind the stream gate.
    Stream,
    /// One tool call: concurrent with everything.
    Tool,
    /// A nested function call, such as a refresh function or a computed
    /// effect: concurrent with everything.
    Function,
}

/// How one request ended.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum Outcome {
    /// The child's answer, as the Lua function would return it.
    Answered(Answer),
    /// The callback's deadline passed first: a timed-out hook fails with
    /// its `on_failure`, and the late reply is dropped.
    TimedOut,
    /// Cancelled first: the callback ended, was cancelled, or its
    /// generation retired.
    Cancelled,
}

/// The child's answer to one request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum Answer {
    /// What the Lua function returns; any JSON value, null included.
    Value(serde_json::Value),
    /// The failure, failing the call as a Lua error does.
    Failure {
        /// The failure's code.
        code: contract::ErrorCode,
        /// Fiber's own sentence.
        message: String,
    },
}

/// What settling a request reports: whether this settler won.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum Settled {
    /// This settler completed the request; the outcome is delivered.
    Completed,
    /// The request already ended or retired: a late reply or call reads
    /// `stale_request` and changes nothing.
    Stale,
}

/// One pending request: its kind, its callback context, its cancel signal
/// and its waiter.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
struct Pending {
    kind: RequestKind,
    context: CallbackContext,
    cancel: support::cancel::Token,
    done: bool,
    wait: Arc<WaitCell>,
    /// Wakes the pending request holds: dropping the caller-side watch
    /// must not drop the wake the subscription names.
    watchers: Vec<Arc<Fire>>,
}

/// What one dispatched request waits on: its outcome, once settled, and
/// the condvar its settler wakes. The waiter parks on the injected clock
/// instead of polling the process clock.
pub(crate) struct WaitCell {
    pub(crate) mutex: Mutex<Option<Outcome>>,
    changed: Condvar,
}

impl WaitCell {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            mutex: Mutex::new(None),
            changed: Condvar::new(),
        })
    }
}

/// Fires one request's cancel when its dispatching caller cancels: the
/// dispatching callback ended, so its request ends with it.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
struct Fire {
    table: std::sync::Weak<Table>,
    id: RequestId,
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
impl support::clock::Wake for Fire {
    fn wake(&self) {
        if let Some(table) = self.table.upgrade() {
            table.cancel(&self.id);
        }
    }
}

/// What minting one request hands back.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) struct Minted {
    /// The request's id, unique within the generation.
    pub id: RequestId,
    /// Fired when the request ends for any reason but an answer.
    pub cancel: support::cancel::Token,
    /// What the dispatch waits on.
    pub wait: Arc<WaitCell>,
}

/// One generation of pending requests, with the stream gate.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
struct Inner {
    /// The current generation: minted ids resolve only here.
    generation: u64,
    /// Every pending request by id.
    pending: BTreeMap<RequestId, Pending>,
    /// The stream item waiting for its reply, when one is outstanding.
    stream_held: Option<RequestId>,
}

/// The request table: one generation of pending requests and the stream
/// gate. The lock is a leaf: no callback runs under it.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) struct Table {
    inner: Mutex<Inner>,
    stream_gate: Condvar,
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
impl Table {
    /// An empty table at its first generation.
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                generation: 0,
                pending: BTreeMap::new(),
                stream_held: None,
            }),
            stream_gate: Condvar::new(),
        }
    }

    /// The current generation.
    pub(crate) fn generation(&self) -> u64 {
        support::lock(&self.inner).generation
    }

    /// Mints one pending request of `kind`, run as `ran_by` for
    /// `extension` and ending at `deadline`, joining hook transaction
    /// `state_call` when the request is a hook delivery. `credential` is
    /// what a `host.oauth.refresh` under this request refreshes, set only
    /// by the dispatcher that runs a provider's `credential()`. The
    /// stored context is what calls made under this request run under,
    /// with the request's own id and cancel signal. The id is unique
    /// within the generation: minted ids never repeat while pending, and
    /// retirement clears the table, so no id resolves across generations.
    pub(crate) fn mint(
        &self,
        kind: RequestKind,
        extension: &str,
        ran_by: String,
        deadline: Option<Instant>,
        state_call: Option<u64>,
        credential: Option<crate::lua::CredentialFor>,
    ) -> Minted {
        let mut inner = support::lock(&self.inner);
        let id = loop {
            let id = RequestId(support::mint("r_"));
            if !inner.pending.contains_key(&id) {
                break id;
            }
        };
        let cancel = support::cancel::Token::new();
        let context = CallbackContext::request(
            extension,
            id.clone(),
            ran_by,
            deadline,
            cancel.clone(),
            state_call,
            credential,
        );
        let wait = WaitCell::new();
        inner.pending.insert(
            id.clone(),
            Pending {
                kind,
                context,
                cancel: cancel.clone(),
                done: false,
                wait: Arc::clone(&wait),
                watchers: Vec::new(),
            },
        );
        Minted { id, cancel, wait }
    }

    /// Whether `id` names a live request of this generation: answered,
    /// timed out, cancelled or retired ids read stale.
    pub(crate) fn is_live(&self, id: &RequestId) -> bool {
        let inner = support::lock(&self.inner);
        inner.pending.get(id).is_some_and(|pending| !pending.done)
    }

    /// Waits until `id` is no longer live: answered, timed out,
    /// cancelled, or retired with its generation. Every end settles
    /// under the gate's notify, so the wait always ends; a stale id
    /// returns at once. The pump's parked calls wait here, never on the
    /// pump thread.
    pub(crate) fn wait_end(&self, id: &RequestId, clock: &dyn Clock) {
        let mut inner = support::lock(&self.inner);
        while inner.pending.get(id).is_some_and(|pending| !pending.done) {
            let parked = support::clock::park(
                clock,
                None,
                Some(Self::WAIT_POLL),
                &self.stream_gate,
                inner,
                |_| false,
            );
            inner = match parked {
                Some(guard) => guard,
                None => support::lock(&self.inner),
            };
        }
    }

    /// The live request's callback context, for attributing a call made
    /// under it. A stale id names none.
    pub(crate) fn context(&self, id: &RequestId) -> Option<CallbackContext> {
        let inner = support::lock(&self.inner);
        let pending = inner.pending.get(id)?;
        (!pending.done).then(|| pending.context.clone())
    }

    /// The live request's kind. A stale id names none.
    pub(crate) fn kind(&self, id: &RequestId) -> Option<RequestKind> {
        let inner = support::lock(&self.inner);
        let pending = inner.pending.get(id)?;
        (!pending.done).then_some(pending.kind)
    }

    /// Completes `id` with `outcome`: the first settler wins and its
    /// outcome is stored for the waiter; every later settler reads stale.
    /// Completing a stream item releases the stream gate before waking
    /// its waiter: a waiter that answers from its own release cannot
    /// deadlock on the gate.
    pub(crate) fn settle(&self, id: &RequestId, outcome: Outcome) -> Settled {
        let wait = {
            let mut inner = support::lock(&self.inner);
            let wait = match inner.pending.get_mut(id) {
                Some(pending) if !pending.done => {
                    pending.done = true;
                    Arc::clone(&pending.wait)
                }
                _ => return Settled::Stale,
            };
            if inner.stream_held.as_ref() == Some(id) {
                inner.stream_held = None;
            }
            wait
        };
        self.stream_gate.notify_all();
        {
            let mut guard = support::lock(&wait.mutex);
            *guard = Some(outcome);
        }
        wait.changed.notify_all();
        Settled::Completed
    }

    /// Cancels `id`: fires its cancel signal and completes it cancelled.
    /// The first settler wins; a settled id reads stale.
    pub(crate) fn cancel(&self, id: &RequestId) -> Settled {
        let cancel = {
            let inner = support::lock(&self.inner);
            match inner.pending.get(id) {
                Some(pending) if !pending.done => pending.cancel.clone(),
                _ => return Settled::Stale,
            }
        };
        cancel.cancel();
        self.settle(id, Outcome::Cancelled)
    }

    /// Times `id` out: completes it without firing its cancel signal — a
    /// missed hook timeout stops nothing, the hook has failed and a late
    /// reply is dropped. The first settler wins.
    pub(crate) fn timeout(&self, id: &RequestId) -> Settled {
        self.settle(id, Outcome::TimedOut)
    }

    /// Ends the local wait's race for `id`: tries to settle `prefer`
    /// (the wait's own end, `TimedOut` or `Cancelled`), and the loser
    /// takes what the cell holds. A reply that arrived first still wins,
    /// even when the clock just passed the deadline.
    pub(crate) fn end_race(
        &self,
        id: &RequestId,
        wait: &Arc<WaitCell>,
        prefer: Outcome,
    ) -> Outcome {
        match self.settle(id, prefer.clone()) {
            Settled::Completed => prefer,
            Settled::Stale => support::lock(&wait.mutex).clone().unwrap_or(prefer),
        }
    }

    /// Fires `id`'s cancel when `caller` cancels: the dispatching callback
    /// ended, so its request ends with it. The wake lives in the pending
    /// entry, since the subscription holds only a weak reference. A
    /// caller already cancelled fires at once, since its wake never
    /// replays. A settled id links nothing.
    pub(crate) fn watch_caller(self: &Arc<Self>, caller: &dyn Cancel, id: &RequestId) {
        let fire = Arc::new(Fire {
            table: Arc::downgrade(self),
            id: id.clone(),
        });
        {
            let mut inner = support::lock(&self.inner);
            match inner.pending.get_mut(id) {
                Some(pending) if !pending.done => {
                    pending.watchers.push(Arc::clone(&fire));
                }
                _ => return,
            }
        }
        caller.subscribe(Arc::downgrade(&fire) as std::sync::Weak<dyn support::clock::Wake>);
        if caller.is_cancelled() {
            self.cancel(id);
        }
    }

    /// Retires the generation: every pending request completes cancelled
    /// with its cancel signal fired, the table clears, and the generation
    /// bumps. Late replies and calls name ids of a dead generation and
    /// read stale. Signals fire and cells fill outside the table lock, so
    /// a woken waiter never blocks on it.
    pub(crate) fn retire(&self) -> u64 {
        let retired: Vec<(support::cancel::Token, Arc<WaitCell>)> = {
            let mut inner = support::lock(&self.inner);
            inner.generation += 1;
            inner.stream_held = None;
            let mut retired = Vec::with_capacity(inner.pending.len());
            for pending in inner.pending.values_mut() {
                if !pending.done {
                    pending.done = true;
                    retired.push((pending.cancel.clone(), Arc::clone(&pending.wait)));
                }
            }
            inner.pending.clear();
            retired
        };
        self.stream_gate.notify_all();
        for (cancel, wait) in retired {
            cancel.cancel();
            {
                let mut guard = support::lock(&wait.mutex);
                *guard = Some(Outcome::Cancelled);
            }
            wait.changed.notify_all();
        }
        support::lock(&self.inner).generation
    }

    /// Acquires the stream gate for `id` in `generation`: blocks while an
    /// earlier stream item waits for its reply. Tools never take the gate,
    /// so a long tool never holds up the stream. Returns false when the
    /// generation retired underneath: the waiter ends stale instead of
    /// hanging.
    pub(crate) fn acquire_stream(&self, id: &RequestId, generation: u64) -> bool {
        let mut inner = support::lock(&self.inner);
        while inner.stream_held.is_some() && inner.generation == generation {
            inner = self
                .stream_gate
                .wait(inner)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        if inner.generation != generation {
            return false;
        }
        inner.stream_held = Some(id.clone());
        true
    }

    /// How long one park sleeps before rechecking the wait: the
    /// notification wakes it first, and the slice only bounds a missed
    /// one. The OAuth poll sleeps the same slice.
    pub(crate) const WAIT_POLL: Duration = Duration::from_millis(20);

    /// Waits for one request's outcome until its end: the answer, its
    /// deadline, its cancel signal, or the hang guard `hang`, whichever
    /// is first. The waiter parks on the injected clock, which the tests
    /// freeze or advance. A wait that outlives every bound reads `None`,
    /// and the test fails instead of hanging.
    pub(crate) fn wait_for(
        wait: &WaitCell,
        deadline: Option<Instant>,
        cancel: &support::cancel::Token,
        clock: &dyn Clock,
        hang: Duration,
    ) -> Option<Outcome> {
        let start = clock.now();
        loop {
            let guard = support::lock(&wait.mutex);
            if let Some(outcome) = guard.clone() {
                return Some(outcome);
            }
            if cancel.is_cancelled() {
                return Some(Outcome::Cancelled);
            }
            let now = clock.now();
            if deadline.is_some_and(|end| now >= end) {
                return Some(Outcome::TimedOut);
            }
            if now.duration_since(start) >= hang {
                return None;
            }
            let bound = start + hang;
            let until = Some(deadline.map_or(bound, |end| end.min(bound)));
            let parked = support::clock::park(
                clock,
                until,
                Some(Self::WAIT_POLL),
                &wait.changed,
                guard,
                |state| state.is_some(),
            );
            if parked.is_none() {
                continue;
            }
        }
    }
}
