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

use crate::Error;
use crate::runtime::FunctionRef;
use serde_json::Value;

use super::{Shared, envelope_line, hang_guard, lifecycle, transport};

use super::super::host::admission::CallbackContext;

/// What one pending request asks the child to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
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
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
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
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
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
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) enum Settled {
    /// This settler completed the request; the outcome is delivered.
    Completed,
    /// The request already ended or retired: a late reply or call reads
    /// `stale_request` and changes nothing.
    Stale,
}

/// One pending request: its kind, its callback context, its cancel signal
/// and its waiter.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
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
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
struct Fire {
    table: std::sync::Weak<Table>,
    id: RequestId,
}

#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
impl support::clock::Wake for Fire {
    fn wake(&self) {
        if let Some(table) = self.table.upgrade() {
            table.cancel(&self.id);
        }
    }
}

/// What minting one request hands back.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) struct Minted {
    /// The request's id, unique within the generation.
    pub id: RequestId,
    /// Fired when the request ends for any reason but an answer.
    pub cancel: support::cancel::Token,
    /// What the dispatch waits on.
    pub wait: Arc<WaitCell>,
}

/// One generation of pending requests, with the stream gate.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
struct Inner {
    /// The current generation: minted ids resolve only here.
    generation: u64,
    /// The next request sequence, never reset across retirement.
    next_request_id: u128,
    /// Every pending request by id.
    pending: BTreeMap<RequestId, Pending>,
    /// The stream item waiting for its reply, when one is outstanding.
    stream_held: Option<RequestId>,
}

/// The request table: one generation of pending requests and the stream
/// gate. The lock is a leaf: no callback runs under it.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) struct Table {
    inner: Mutex<Inner>,
    stream_gate: Condvar,
    #[cfg(test)]
    fixed_request_nonce: Mutex<Option<String>>,
    #[cfg(test)]
    next_stream_wait: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
impl Table {
    /// An empty table at its first generation.
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                generation: 0,
                next_request_id: 0,
                pending: BTreeMap::new(),
                stream_held: None,
            }),
            stream_gate: Condvar::new(),
            #[cfg(test)]
            fixed_request_nonce: Mutex::new(None),
            #[cfg(test)]
            next_stream_wait: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn use_fixed_request_nonce(&self, nonce: &str) {
        *support::lock(&self.fixed_request_nonce) = Some(nonce.to_owned());
    }

    #[cfg(test)]
    pub(crate) fn observe_next_stream_wait(&self) -> std::sync::mpsc::Receiver<()> {
        let (sender, receiver) = std::sync::mpsc::channel();
        *support::lock(&self.next_stream_wait) = Some(sender);
        receiver
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
        let sequence = inner.next_request_id;
        inner.next_request_id = sequence.wrapping_add(1);
        #[cfg(test)]
        let nonce = support::lock(&self.fixed_request_nonce)
            .clone()
            .unwrap_or_else(|| support::mint("r_"));
        #[cfg(not(test))]
        let nonce = support::mint("r_");
        let id = RequestId(format!("{nonce}_{sequence:032x}"));
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

    /// Waits until no request is still live, or the caller cancels: the
    /// final stream request's answer ends the shutdown's wait, and
    /// retirement settles the rest. Every settle, cancel, timeout and
    /// retire notifies the gate, so the wait rechecks instead of hanging;
    /// the poll slice bounds a missed notify. True when the table idles,
    /// false on cancel.
    pub(crate) fn wait_idle(&self, clock: &dyn Clock, cancel: &dyn Cancel) -> bool {
        let mut inner = support::lock(&self.inner);
        while inner.pending.values().any(|pending| !pending.done) {
            if cancel.is_cancelled() {
                return false;
            }
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
        true
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
            #[cfg(test)]
            if let Some(waiting) = support::lock(&self.next_stream_wait).take() {
                let _sent = waiting.send(());
            }
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

/// Sends one request and waits for its answer: the stream gate first
/// for stream items, then the line, then the reply, the deadline, the
/// cancel or the hang guard, whichever is first. The request settles
/// exactly once on every path, releasing the stream gate. A free function
/// over the shared owner, so installed handlers own it beside the
/// dispatches.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) fn run_request(
    shared: &Shared,
    dispatch: Dispatch,
    caller: &dyn contract::tool::Cancel,
) -> Result<Option<Value>, Error> {
    // The callback's clock starts when Fiber asks, so time spent waiting
    // behind earlier stream items counts against it.
    let clock = shared.clock.as_ref();
    let deadline = clock.now().checked_add(dispatch.timeout);
    // A dead child restarts under the dispatch before anything mints: the
    // one restart runs on the next caller, the others await it, and a
    // gone or finishing owner stops. The restart waits under this same
    // deadline, so the callback never outlives its own timeout.
    if let Err((code, _message)) = lifecycle::ensure_running(shared, deadline, caller) {
        // A missed restart deadline fails as the callback's own timeout;
        // cancellation and a gone owner stop it, as they do mid-request.
        if code == contract::ErrorCode::Timeout {
            return Err(Error::Timeout {
                extension: shared.owner.clone(),
                callback: dispatch.key,
                timeout_ms: dispatch.timeout.as_millis().try_into().unwrap_or(u64::MAX),
            });
        }
        return Err(Error::Stopped {
            extension: shared.owner.clone(),
        });
    }
    // No dispatcher behind the package seam runs a provider's
    // `credential()`: dispatches carry no credential scope, and a
    // `host.oauth.refresh` under them is refused. The session sets the
    // scope when it dispatches provider functions.
    let minted = shared.table.mint(
        dispatch.kind,
        &shared.owner,
        dispatch.ran_by,
        deadline,
        dispatch.state_call,
        None,
    );
    let id = minted.id;
    if dispatch.kind == RequestKind::Stream {
        let generation = shared.table.generation();
        if !shared.table.acquire_stream(&id, generation) {
            shared.table.cancel(&id);
            return Err(Error::Stopped {
                extension: shared.owner.clone(),
            });
        }
    }
    shared.table.watch_caller(caller, &id);
    let payload = serde_json::to_value(&contract::events::ExtensionRequest {
        request_id: id.clone(),
        registration: dispatch.registration,
        key: dispatch.key.clone(),
        args: dispatch.args,
    })
    .map_err(|error| Error::BadReturn {
        extension: shared.owner.clone(),
        callback: dispatch.key.clone(),
        why: format!("the request never serialized: {error}"),
    })?;
    let line = envelope_line(
        shared,
        "extension_request",
        payload.as_object().cloned().unwrap_or_default(),
    );
    match support::lock(&shared.transport).send(line) {
        Ok(()) => {}
        Err(transport::SendError::TooLarge) => {
            shared.table.cancel(&id);
            return Err(Error::BadReturn {
                extension: shared.owner.clone(),
                callback: dispatch.key.clone(),
                why: "the request does not fit the 64 MiB wire cap".to_owned(),
            });
        }
        Err(transport::SendError::Gone) => {
            shared.table.cancel(&id);
            return Err(Error::Stopped {
                extension: shared.owner.clone(),
            });
        }
    }
    let hang = hang_guard(clock, deadline);
    let outcome = match Table::wait_for(&minted.wait, deadline, &minted.cancel, clock, hang) {
        Some(Outcome::Answered(answer)) => Outcome::Answered(answer),
        Some(Outcome::TimedOut) => shared.table.end_race(&id, &minted.wait, Outcome::TimedOut),
        Some(Outcome::Cancelled) | None => {
            shared.table.end_race(&id, &minted.wait, Outcome::Cancelled)
        }
    };
    match outcome {
        Outcome::Answered(Answer::Value(value)) => Ok(Some(value)),
        Outcome::Answered(Answer::Failure { code, message }) => Err(Error::Lua {
            extension: shared.owner.clone(),
            message: format!(
                "`{key}` failed with {code:?}: {message}",
                key = dispatch.key
            ),
        }),
        Outcome::TimedOut => Err(Error::Timeout {
            extension: shared.owner.clone(),
            callback: dispatch.key,
            timeout_ms: dispatch.timeout.as_millis().try_into().unwrap_or(u64::MAX),
        }),
        Outcome::Cancelled => Err(Error::Stopped {
            extension: shared.owner.clone(),
        }),
    }
}

/// What one dispatch sends: the request's kind, whom it answers, what it
/// runs as, its arguments, its timeout and its hook transaction. One
/// value carries the dispatch so the send path stays narrow.
#[derive(Debug, Clone)]
pub(crate) struct Dispatch {
    /// Ordered behind the stream gate, or concurrent with everything.
    pub kind: RequestKind,
    /// The registering call: the request answers under its generation.
    pub registration: contract::CommandId,
    /// The function key within the registration: `run`, `effects` or a
    /// numbered function.
    pub key: String,
    /// What the callback runs as, naming its attribution.
    pub ran_by: String,
    /// The arguments the Lua function receives, as a JSON array.
    pub args: Vec<Value>,
    /// How long the callback may run before it fails.
    pub timeout: Duration,
    /// The hook state transaction the call's writes join, when the
    /// request is a hook delivery.
    pub state_call: Option<u64>,
}

impl Dispatch {
    /// One ordered stream item: a hook, a watcher delivery or a command.
    #[allow(
        dead_code,
        reason = "the admission test seam; the session dispatches through its installed runtime handlers"
    )]
    pub(crate) fn stream(
        func: &FunctionRef,
        ran_by: String,
        args: Value,
        timeout: Duration,
        state_call: Option<u64>,
    ) -> Self {
        Self {
            kind: RequestKind::Stream,
            registration: func.command.clone(),
            key: func.key.clone(),
            ran_by,
            args: wire_args(args),
            timeout,
            state_call,
        }
    }

    /// One tool call: concurrent with everything.
    #[allow(
        dead_code,
        reason = "the admission test seam; the session dispatches through its installed runtime handlers"
    )]
    pub(crate) fn tool(func: &FunctionRef, ran_by: String, args: Value, timeout: Duration) -> Self {
        Self {
            kind: RequestKind::Tool,
            registration: func.command.clone(),
            key: func.key.clone(),
            ran_by,
            args: wire_args(args),
            timeout,
            state_call: None,
        }
    }
}

/// The arguments the Lua function receives, as a JSON array: a function
/// taking one value receives it alone, and an array value spreads, so a
/// multi-argument function receives each element.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) fn wire_args(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        other @ (Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Object(_)) => vec![other],
    }
}
