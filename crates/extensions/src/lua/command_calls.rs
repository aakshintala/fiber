//! A command's turnless `host.tool` runner and its live serving window
//! (`docs/extensions.md`, "Running a tool").

use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};

use contract::clock::Wake;
use contract::events::{CallStatus, ToolCallCompleted};
use contract::inbox::{Delivery, InnerCallRequest};
use contract::tool::{Cancel, InnerCalls};
use serde_json::Value;

use super::Hub;

/// The end of one command. Subscribers wake outside the signal's lock.
#[derive(Default)]
pub(super) struct Ended {
    state: Mutex<(bool, Vec<Weak<dyn Wake>>)>,
}

impl Ended {
    pub(super) fn end(&self) {
        let subscribers = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.0 {
                return;
            }
            state.0 = true;
            std::mem::take(&mut state.1)
        };
        for subscriber in subscribers.into_iter().filter_map(|weak| weak.upgrade()) {
            subscriber.wake();
        }
    }
}

impl Cancel for Ended {
    fn is_cancelled(&self) -> bool {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).0
    }

    fn subscribe(&self, wake: Weak<dyn Wake>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.0 {
            state.1.push(wake);
        }
    }
}

/// A runner kept by the hub only while its command is admitted.
pub(super) struct CommandCalls {
    pub(super) hub: Weak<Hub>,
    pub(super) command: contract::CommandId,
    pub(super) call: u64,
    pub(super) ended: Arc<Ended>,
    #[cfg(test)]
    pub(super) pause: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}

enum State {
    Waiting,
    Answered(Box<ToolCallCompleted>),
    Returned,
}

struct Reply {
    state: Mutex<State>,
    changed: Condvar,
    ended: Arc<Ended>,
}

impl Reply {
    fn pending(&self) -> bool {
        matches!(
            *self.state.lock().unwrap_or_else(PoisonError::into_inner),
            State::Waiting
        )
    }

    fn resolve(&self, completed: Box<ToolCallCompleted>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(*state, State::Waiting) {
            *state = State::Answered(completed);
            self.changed.notify_all();
        }
    }

    fn wait(&self) -> Box<ToolCallCompleted> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if matches!(*state, State::Answered(_))
                && let State::Answered(completed) = std::mem::replace(&mut *state, State::Returned)
            {
                return completed;
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl Wake for Reply {
    fn wake(&self) {
        if self.ended.is_cancelled() {
            self.resolve(cancelled());
        }
    }
}

/// Owns the unanswered request. Its drop closes the window before waking
/// the runner, including when a sealed router drops the delivery at once.
struct Answer {
    hub: Weak<Hub>,
    call: u64,
    reply: Arc<Reply>,
}

impl Answer {
    fn resolve(&self, completed: Box<ToolCallCompleted>) {
        if let Some(hub) = self.hub.upgrade() {
            let mut shared = hub.lock();
            if shared.serving == Some(self.call) {
                shared.serving = None;
            }
            self.reply.resolve(completed);
        } else {
            self.reply.resolve(completed);
        }
    }
}

impl Drop for Answer {
    fn drop(&mut self) {
        self.resolve(cancelled());
    }
}

impl InnerCalls for CommandCalls {
    fn run(
        &self,
        extension: &str,
        name: &str,
        arguments: Value,
    ) -> Result<Box<ToolCallCompleted>, String> {
        if self.ended.is_cancelled() {
            return Ok(cancelled());
        }
        let Some(hub) = self.hub.upgrade() else {
            return Ok(cancelled());
        };
        let reply = Arc::new(Reply {
            state: Mutex::new(State::Waiting),
            changed: Condvar::new(),
            ended: self.ended.clone(),
        });
        let wake: Arc<dyn Wake> = reply.clone();
        self.ended.subscribe(Arc::downgrade(&wake));
        reply.wake();
        let taken_hub = self.hub.clone();
        let taken_reply = reply.clone();
        let ended = self.ended.clone();
        let call = self.call;
        let answer = Answer {
            hub: self.hub.clone(),
            call,
            reply: reply.clone(),
        };
        hub.send(Delivery::InnerCall(InnerCallRequest {
            extension: extension.to_owned(),
            command: self.command.clone(),
            name: name.to_owned(),
            arguments,
            cancel: self.ended.clone(),
            taken: Box::new(move || {
                if let Some(hub) = taken_hub.upgrade() {
                    let mut shared = hub.lock();
                    if shared.runners.contains_key(&call)
                        && taken_reply.pending()
                        && !ended.is_cancelled()
                    {
                        shared.serving = Some(call);
                    }
                }
            }),
            answer: Box::new(move |completed| answer.resolve(completed)),
        }));
        let completed = reply.wait();
        #[cfg(test)]
        if let Some((entered, held)) = self
            .pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _sent = entered.send(());
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                .recv_or_fail(&held, "runner backstop release");
        }
        let mut shared = hub.lock();
        if shared.serving == Some(call) {
            shared.serving = None;
        }
        Ok(completed)
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
    })
}

#[cfg(test)]
#[path = "command_calls_tests.rs"]
mod tests;
