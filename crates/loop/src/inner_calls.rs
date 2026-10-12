//! Inner calls run inside their anchor on the loop's step machinery.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};

use contract::ActionId;
use contract::clock::Wake;
use contract::events::{CallStatus, ToolCallCompleted};
use contract::tool::{Cancel, InnerCalls};
use serde_json::Value;

use crate::TurnCancel;
use crate::progress::{SharedWake, Stream};

pub(crate) const MAX_INNER_DEPTH: u8 = 8;

/// One response shared by the requester, the loop and the closing slot.
#[derive(Default)]
struct Answer {
    completed: Mutex<Option<Box<ToolCallCompleted>>>,
    changed: Condvar,
}

impl Answer {
    fn resolve(&self, completed: Box<ToolCallCompleted>) {
        let mut answer = lock(&self.completed);
        answer.get_or_insert(completed);
        self.changed.notify_all();
    }

    fn wait(&self) -> Box<ToolCallCompleted> {
        let mut answer = lock(&self.completed);
        loop {
            if let Some(completed) = answer.take() {
                return completed;
            }
            answer = self
                .changed
                .wait(answer)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// A request not yet taken by the loop thread.
pub(crate) struct Raised {
    pub(crate) extension: String,
    pub(crate) name: String,
    pub(crate) arguments: Value,
    answer: Arc<Answer>,
}

impl Raised {
    pub(crate) fn answer(self, completed: Box<ToolCallCompleted>) {
        self.answer.resolve(completed);
    }
}

#[derive(Default)]
struct Requests {
    raised: Vec<Raised>,
    waiting: BTreeMap<usize, Arc<Answer>>,
    closed: bool,
}

/// The runner for a stream's inner calls. Raising and closing share one lock.
pub(crate) struct InnerSlot {
    pub(crate) action: ActionId,
    tool: String,
    pub(crate) depth: u8,
    wake: Arc<SharedWake>,
    requests: Mutex<Requests>,
}

impl InnerSlot {
    pub(crate) fn new(wake: Arc<SharedWake>, action: ActionId, tool: String, depth: u8) -> Self {
        Self {
            action,
            tool,
            depth,
            wake,
            requests: Mutex::default(),
        }
    }

    pub(crate) fn take_raised(&self) -> Vec<Raised> {
        std::mem::take(&mut lock(&self.requests).raised)
    }

    pub(crate) fn close(&self) {
        let mut requests = lock(&self.requests);
        requests.closed = true;
        requests.raised.clear();
        for answer in std::mem::take(&mut requests.waiting).into_values() {
            answer.resolve(cancelled());
        }
    }
}

impl InnerCalls for InnerSlot {
    fn run(
        &self,
        extension: &str,
        name: &str,
        arguments: Value,
    ) -> Result<Box<ToolCallCompleted>, String> {
        if name == self.tool {
            return Err(format!("host.tool: the tool `{name}` cannot run itself"));
        }
        if self.depth >= MAX_INNER_DEPTH {
            return Err(format!(
                "host.tool: inner-call depth exceeds {MAX_INNER_DEPTH}"
            ));
        }
        let answer = Arc::new(Answer::default());
        {
            let mut requests = lock(&self.requests);
            if requests.closed {
                return Ok(cancelled());
            }
            // An answer's address is unique while this map holds its Arc.
            requests
                .waiting
                .insert(Arc::as_ptr(&answer).addr(), Arc::clone(&answer));
            requests.raised.push(Raised {
                extension: extension.to_owned(),
                name: name.to_owned(),
                arguments,
                answer: Arc::clone(&answer),
            });
        }
        self.wake.wake();
        let completed = answer.wait();
        lock(&self.requests)
            .waiting
            .remove(&Arc::as_ptr(&answer).addr());
        Ok(completed)
    }
}

fn cancelled() -> Box<ToolCallCompleted> {
    Box::new(ToolCallCompleted {
        status: CallStatus::Cancelled,
        ..crate::completion::completed(String::new(), None)
    })
}

#[derive(Default)]
struct Signal {
    inner: Mutex<(bool, Vec<Weak<dyn Wake>>)>,
}

impl Signal {
    fn cancel(&self) {
        let wakers = {
            let mut inner = lock(&self.inner);
            inner.0 = true;
            std::mem::take(&mut inner.1)
        };
        for waker in wakers.into_iter().filter_map(|waker| waker.upgrade()) {
            waker.wake();
        }
    }
}

impl Wake for Signal {
    fn wake(&self) {
        self.cancel();
    }
}

/// Cancelled by the turn or by its anchor ending, without cancelling the anchor.
pub(crate) struct InnerCancel {
    turn: Arc<TurnCancel>,
    signal: Arc<Signal>,
}

impl InnerCancel {
    pub(crate) fn new(turn: Arc<TurnCancel>) -> Self {
        let signal = Arc::new(Signal::default());
        let relay: Arc<dyn Wake> = signal.clone();
        turn.subscribe(Arc::downgrade(&relay));
        if turn.is_cancelled() {
            signal.cancel();
        }
        Self { turn, signal }
    }

    pub(crate) fn cancel(&self) {
        self.signal.cancel();
    }
}

impl Cancel for InnerCancel {
    fn is_cancelled(&self) -> bool {
        lock(&self.signal.inner).0 || self.turn.is_cancelled()
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        let mut inner = lock(&self.signal.inner);
        // The turn can be cancelled before its relay reaches this signal.
        // A subscription after that cancel must not replay it.
        if !inner.0 && !self.turn.is_cancelled() {
            inner.1.push(waker);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A started inner call, held until its own inner calls have completed.
pub(crate) struct InnerRunning {
    pub(crate) id: ActionId,
    anchor: ActionId,
    request: Raised,
    pub(crate) stream: Arc<Stream>,
    cancel: Arc<InnerCancel>,
    bound: contract::tool::Bound,
    declared: contract::shapes::DeclaredEffects,
    arguments: Arc<serde_json::Map<String, Value>>,
    rewritten: Option<Vec<String>>,
}

impl crate::Loop {
    /// Takes requests on the loop thread, using preparation and only the deny steps.
    pub(crate) fn take_inner<'scope, 'env>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'env>,
        streams: &[(&ActionId, &Stream)],
        inner: &mut Vec<InnerRunning>,
        release: &mut crate::asking::Release,
        wake: &Arc<SharedWake>,
        turn: &contract::TurnId,
    ) -> Result<(), crate::Error> {
        use contract::events::{
            Anchor, Decision, Event, RanBy, ToolCallRequested, ToolCallStarted,
        };
        for (_, stream) in streams {
            let Some(slot) = stream.inner_slot() else {
                continue;
            };
            for request in slot.take_raised() {
                let id = ActionId(crate::mint("a_"));
                let call = self.repaired(ToolCallRequested {
                    name: request.name.clone(),
                    arguments: request.arguments.clone(),
                    provider_id: None,
                    repair: None,
                    provider_item: None,
                    ran_by: Some(RanBy {
                        extension: request.extension.clone(),
                        anchor: Anchor::OuterCall(slot.action.clone()),
                    }),
                });
                self.append(&Event::ToolCallRequested(call.clone()), turn, Some(&id))?;
                let decision = if self.turn_cancelled() || stream.returned() {
                    Err(self.cancelled_before_ran())
                } else {
                    self.prepared(&call, turn)?
                };
                let prepared = match decision {
                    Err(mut done) => {
                        self.after_completed(None, &mut done, &id, turn)?;
                        request.answer(done);
                        continue;
                    }
                    Ok(prepared) => prepared,
                };
                if let crate::permission::Verdict::Deny { by, reason, why } =
                    crate::permission::inner_verdict(
                        &call.name,
                        &prepared.effects,
                        &self.rules.read(),
                        &self.workspace,
                        &self.credentials,
                        &self.credential_files,
                    )
                {
                    let message = format!("{why} It did not run.");
                    self.decided(
                        &id,
                        turn,
                        crate::completion::resolved(None, Decision::Deny, by, Some(why), None),
                    )?;
                    let mut done = crate::completion::denied(reason, message);
                    self.after_completed(None, &mut done, &id, turn)?;
                    request.answer(done);
                    continue;
                }
                if self.turn_cancelled() || stream.returned() {
                    let mut done = self.cancelled_before_ran();
                    self.after_completed(None, &mut done, &id, turn)?;
                    request.answer(done);
                    continue;
                }
                let (arguments, changed_by) = match &prepared.rewritten {
                    Some(names) => (Some(prepared.arguments.clone()), Some(names.clone())),
                    None => (None, None),
                };
                self.append(
                    &Event::ToolCallStarted(ToolCallStarted {
                        declared: prepared.effects.declared.clone(),
                        arguments,
                        changed_by,
                    }),
                    turn,
                    Some(&id),
                )?;
                let cancel = Arc::new(InnerCancel::new(Arc::clone(&self.cancel)));
                let stream = Arc::new(Stream::new(Arc::clone(wake), id.clone(), false).with_inner(
                    Arc::new(InnerSlot::new(
                        Arc::clone(wake),
                        id.clone(),
                        call.name.clone(),
                        slot.depth + 1,
                    )),
                ));
                release.add_cancel(Arc::clone(&cancel));
                release.add(Arc::clone(&stream));
                let arguments = Arc::new(prepared.arguments);
                let worker_stream = Arc::clone(&stream);
                let worker_cancel = Arc::clone(&cancel);
                let worker_arguments = Arc::clone(&arguments);
                let tool = prepared.tool;
                let bound = tool.bound();
                scope.spawn(move || {
                    let output = tool.run_asking(
                        &worker_arguments,
                        worker_cancel.as_ref(),
                        worker_stream.as_ref(),
                        worker_stream.as_ref(),
                    );
                    worker_stream.finish(output);
                });
                inner.push(InnerRunning {
                    id,
                    anchor: slot.action.clone(),
                    request,
                    stream,
                    cancel,
                    bound,
                    declared: prepared.effects.declared,
                    arguments,
                    rewritten: prepared.rewritten,
                });
            }
        }
        Ok(())
    }

    /// Serves all inner calls in completion order, holding each anchor until its
    /// inner calls end. An anchor returning cancels its still-running calls.
    pub(crate) fn serve_inner(
        &mut self,
        inner: &mut Vec<InnerRunning>,
        finished: &dyn Fn(&ActionId) -> bool,
        turn: &contract::TurnId,
    ) -> Result<bool, crate::Error> {
        let now = self.log.clock().now();
        let mut wrote = false;
        for call in inner.iter() {
            let anchor_stopped = match inner.iter().find(|anchor| anchor.id == call.anchor) {
                Some(anchor) => anchor.stream.returned() || anchor.cancel.is_cancelled(),
                None => finished(&call.anchor),
            };
            if anchor_stopped {
                call.cancel.cancel();
            }
            if let Some(delta) = call.stream.take_due(now) {
                let bytes = self.write_delta(&delta, turn, &call.id);
                call.stream.wrote(bytes, self.log.clock().now());
                wrote = true;
            }
            if let Some(delta) = call.stream.take_flush() {
                self.write_delta(&delta, turn, &call.id);
                wrote = true;
            }
        }
        // Remove only leaves: their parent remains until its completion is written.
        while let Some((index, (output, flushed))) =
            inner.iter().enumerate().find_map(|(index, call)| {
                if Self::inner_anchored(inner, &call.id) {
                    return None;
                }
                call.stream.take_finished().map(|output| (index, output))
            })
        {
            let call = inner.remove(index);
            if let Some(slot) = call.stream.inner_slot() {
                slot.close();
            }
            if let Some(delta) = flushed {
                self.write_delta(&delta, turn, &call.id);
            }
            let mut done = self.finish(
                output,
                (&call.request.name, &call.arguments),
                call.rewritten.as_deref(),
                call.bound,
                call.cancel.is_cancelled(),
                &call.id,
                turn,
            )?;
            self.after_completed(Some(&call.declared), &mut done, &call.id, turn)?;
            call.request.answer(Box::new(done));
            wrote = true;
        }
        Ok(wrote)
    }

    pub(crate) fn inner_anchored(inner: &[InnerRunning], action: &ActionId) -> bool {
        inner.iter().any(|call| &call.anchor == action)
    }
}

#[cfg(test)]
#[path = "inner_calls_tests.rs"]
mod tests;
