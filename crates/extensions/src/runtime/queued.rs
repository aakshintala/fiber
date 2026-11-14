//! Queued process commands and their command-scoped inner-call runner.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};

use contract::CommandId;
use contract::clock::Wake;
use contract::events::ToolCallCompleted;
use contract::inbox::{Delivery, InnerCallRequest};
use contract::tool::{Cancel, InnerCalls};
use serde_json::Value;
use support::lock;

use crate::Error;
use crate::lua::CredentialFor;
use crate::lua::command_calls::{Ended, Reply, cancelled};

use super::RuntimeInner;
use super::refs::ProcessInvoke;

/// One held command call (`super`): the run waits for its release, on either backend.
#[derive(Clone)]
pub(crate) enum CommandWait {
    /// A Lua command held in its VM's queue.
    Lua(Arc<crate::lua::Queued>),
    /// A process command held for its release.
    Process(Arc<ProcessQueued>),
}

impl CommandWait {
    /// Releases the call, so the run may start.
    pub(crate) fn release(&self) {
        match self {
            Self::Lua(queued) => queued.release(),
            Self::Process(queued) => queued.release(),
        }
    }

    /// Waits until the run ends, and returns its answer.
    pub(crate) fn wait(&self) -> Result<Value, Error> {
        match self {
            Self::Lua(queued) => queued.wait(),
            Self::Process(queued) => queued.wait(),
        }
    }
}

/// A process command's one-shot invocation, with its answer published after
/// the callback worker has returned.
pub(crate) struct ProcessQueued {
    state: Arc<Mutex<ProcessQueuedState>>,
    done: Arc<Condvar>,
}

struct ProcessQueuedState {
    owner: String,
    invoke: Option<ProcessInvoke>,
    arg: Value,
    scope: Option<CredentialFor>,
    runner: Arc<ProcessCommandCalls>,
    answer: Option<Result<Value, Error>>,
    in_flight: bool,
}

impl ProcessQueued {
    /// A held process command with the driver's invocation as its anchor.
    pub(super) fn new(
        owner: String,
        command: CommandId,
        invoke: ProcessInvoke,
        arg: Value,
        scope: Option<CredentialFor>,
        inbox: Option<mpsc::Sender<Delivery>>,
        active: RuntimeInner,
    ) -> Self {
        let ended = Arc::new(Ended::default());
        let runner = Arc::new(ProcessCommandCalls {
            owner: owner.clone(),
            command,
            inbox,
            ended,
            active,
        });
        Self {
            state: Arc::new(Mutex::new(ProcessQueuedState {
                owner,
                invoke: Some(invoke),
                arg,
                scope,
                runner,
                answer: None,
                in_flight: false,
            })),
            done: Arc::new(Condvar::new()),
        }
    }

    /// Claims and starts the callback on a worker, leaving the driver's
    /// client-connection reader available for replies.
    pub(crate) fn release(&self) {
        let (invoke, arg, scope, runner, state, done) = {
            let mut state = lock(&self.state);
            if state.answer.is_some() || state.in_flight {
                return;
            }
            let Some(invoke) = state.invoke.take() else {
                return;
            };
            state.in_flight = true;
            (
                invoke,
                state.arg.clone(),
                state.scope.clone(),
                Arc::clone(&state.runner),
                Arc::clone(&self.state),
                Arc::clone(&self.done),
            )
        };
        let worker_runner = Arc::clone(&runner);
        let spawned = std::thread::Builder::new()
            .name("process command".to_owned())
            .spawn(move || {
                let inner_calls: Arc<dyn InnerCalls> = worker_runner.clone();
                let result = invoke(arg, scope, worker_runner.ended.as_ref(), Some(inner_calls))
                    .map(|returned| returned.unwrap_or(Value::Null));
                worker_runner.ended.end();
                let mut state = lock(&state);
                state.answer = Some(result);
                state.in_flight = false;
                done.notify_all();
            });
        if let Err(error) = spawned {
            runner.ended.end();
            let mut state = lock(&self.state);
            state.answer = Some(Err(Error::Lua {
                extension: runner.owner.clone(),
                message: format!("cannot start process command worker: {error}"),
            }));
            state.in_flight = false;
            self.done.notify_all();
        }
    }

    /// Waits through both queued and in-flight states, then takes the result.
    pub(crate) fn wait(&self) -> Result<Value, Error> {
        let mut state = lock(&self.state);
        while state.answer.is_none() && (state.invoke.is_some() || state.in_flight) {
            state = self
                .done
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state.answer.take().unwrap_or_else(|| {
            Err(Error::Stopped {
                extension: state.owner.clone(),
            })
        })
    }
}

struct ProcessCommandCalls {
    owner: String,
    command: CommandId,
    inbox: Option<mpsc::Sender<Delivery>>,
    ended: Arc<Ended>,
    active: RuntimeInner,
}

struct ActiveInnerCall {
    entered: AtomicBool,
    active: RuntimeInner,
    key: (String, CommandId),
    thread: Mutex<Option<std::thread::ThreadId>>,
}

impl ActiveInnerCall {
    fn enter(&self) {
        if self.entered.swap(true, Ordering::AcqRel) {
            return;
        }
        let thread = std::thread::current().id();
        *lock(&self.thread) = Some(thread);
        *lock(&self.active)
            .command_inner
            .entry((self.key.0.clone(), self.key.1.clone(), thread))
            .or_default() += 1;
    }
}

impl Drop for ActiveInnerCall {
    fn drop(&mut self) {
        if !self.entered.swap(false, Ordering::AcqRel) {
            return;
        }
        let Some(thread) = lock(&self.thread).take() else {
            return;
        };
        let key = (self.key.0.clone(), self.key.1.clone(), thread);
        let mut inner = lock(&self.active);
        if let Some(count) = inner.command_inner.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                inner.command_inner.remove(&key);
            }
        }
    }
}

struct InnerAnswer(Arc<Reply>);

impl InnerAnswer {
    fn resolve(self, completed: Box<ToolCallCompleted>) {
        self.0.resolve(completed);
    }
}

impl Drop for InnerAnswer {
    fn drop(&mut self) {
        self.0.resolve(cancelled());
    }
}

impl InnerCalls for ProcessCommandCalls {
    fn run(
        &self,
        extension: &str,
        name: &str,
        arguments: Value,
    ) -> Result<Box<ToolCallCompleted>, String> {
        if self.ended.is_cancelled() {
            return Ok(cancelled());
        }
        let Some(inbox) = self.inbox.as_ref() else {
            return Ok(cancelled());
        };
        let reply = Arc::new(Reply::new(Arc::clone(&self.ended)));
        let wake: Arc<dyn Wake> = reply.clone();
        self.ended.subscribe(Arc::downgrade(&wake));
        reply.wake();
        if self.ended.is_cancelled() {
            return Ok(cancelled());
        }
        let active = Arc::new(ActiveInnerCall {
            entered: AtomicBool::new(false),
            active: Arc::clone(&self.active),
            key: (self.owner.clone(), self.command.clone()),
            thread: Mutex::new(None),
        });
        let taken_active = Arc::clone(&active);
        let answered_active = Arc::clone(&active);
        let answer = InnerAnswer(Arc::clone(&reply));
        let taken_reply = Arc::clone(&reply);
        let ended = Arc::clone(&self.ended);
        let request = Delivery::InnerCall(InnerCallRequest {
            extension: extension.to_owned(),
            command: self.command.clone(),
            name: name.to_owned(),
            arguments,
            cancel: self.ended.clone(),
            taken: Box::new(move || {
                if !ended.is_cancelled() {
                    taken_active.enter();
                }
                taken_reply.wake();
            }),
            answer: Box::new(move |completed| {
                answered_active.enter();
                answer.resolve(completed);
            }),
        });
        if inbox.send(request).is_err() {
            return Ok(cancelled());
        }
        Ok(reply.wait())
    }
}
