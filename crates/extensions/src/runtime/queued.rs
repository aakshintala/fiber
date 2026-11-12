//! Queued process commands and their command-scoped inner-call runner.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};

use contract::CommandId;
use contract::clock::Wake;
use contract::events::{CallStatus, ToolCallCompleted};
use contract::inbox::{Delivery, InnerCallRequest};
use contract::tool::{Cancel, InnerCalls};
use serde_json::Value;
use support::lock;

use crate::Error;
use crate::lua::CredentialFor;
use crate::lua::command_calls::Ended;

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

type ActiveCommands = Arc<Mutex<BTreeMap<(String, CommandId), usize>>>;

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
    pub(crate) fn new(
        owner: String,
        command: CommandId,
        invoke: ProcessInvoke,
        arg: Value,
        scope: Option<CredentialFor>,
        inbox: Option<mpsc::Sender<Delivery>>,
        active: ActiveCommands,
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
    active: ActiveCommands,
}

struct ActiveInnerCall {
    entered: AtomicBool,
    active: ActiveCommands,
    key: (String, CommandId),
}

impl ActiveInnerCall {
    fn enter(&self) {
        if self.entered.swap(true, Ordering::AcqRel) {
            return;
        }
        *lock(&self.active).entry(self.key.clone()).or_default() += 1;
    }
}

impl Drop for ActiveInnerCall {
    fn drop(&mut self) {
        if !self.entered.load(Ordering::Acquire) {
            return;
        }
        let mut active = lock(&self.active);
        if let Some(count) = active.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                active.remove(&self.key);
            }
        }
    }
}

struct InnerAnswer {
    reply: Arc<Reply>,
    answered: bool,
}

impl InnerAnswer {
    fn resolve(&mut self, completed: Box<ToolCallCompleted>) {
        if !self.answered {
            self.answered = true;
            self.reply.resolve(completed);
        }
    }
}

impl Drop for InnerAnswer {
    fn drop(&mut self) {
        if !self.answered {
            self.reply.resolve(cancelled());
        }
    }
}

struct Reply {
    state: Mutex<Option<Box<ToolCallCompleted>>>,
    changed: Condvar,
    ended: Arc<Ended>,
}

impl Reply {
    fn resolve(&self, result: Box<ToolCallCompleted>) {
        let mut state = lock(&self.state);
        if state.is_none() {
            *state = Some(result);
            self.changed.notify_all();
        }
    }

    fn wait(&self) -> Box<ToolCallCompleted> {
        let mut state = lock(&self.state);
        while state.is_none() {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state.take().expect("reply is present")
    }
}

impl Wake for Reply {
    fn wake(&self) {
        if self.ended.is_cancelled() {
            self.resolve(cancelled());
        }
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
        let reply = Arc::new(Reply {
            state: Mutex::new(None),
            changed: Condvar::new(),
            ended: Arc::clone(&self.ended),
        });
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
        });
        let taken_active = Arc::clone(&active);
        let answered_active = Arc::clone(&active);
        let answer = InnerAnswer {
            reply: Arc::clone(&reply),
            answered: false,
        };
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
                let mut answer = answer;
                answer.resolve(completed);
            }),
        });
        if inbox.send(request).is_err() {
            return Ok(cancelled());
        }
        Ok(reply.wait())
    }
}

fn cancelled() -> Box<ToolCallCompleted> {
    Box::new(ToolCallCompleted {
        status: CallStatus::Cancelled,
        reason: None,
        error: None,
        process: None,
        content: Vec::new(),
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
        hosts_surfaced: Vec::new(),
        loaded: None,
        protected_paths: None,
    })
}
