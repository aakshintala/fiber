//! One held command call (`super`): the run waits for its release, on
//! either backend. A Lua command holds its VM queue entry; a process
//! command holds its handler here. Release runs the handler on the
//! releasing thread, so the door protocol holds: admit, wait on one
//! thread, release on the other.
//!
//! The state machine owns its own lock and wakeup: release runs the run
//! once, and the wait takes the one answer. A second wait finds no answer
//! and fails stopped instead of hanging.

use std::sync::{Arc, Condvar, Mutex, PoisonError};

use serde_json::Value;
use support::lock;

use crate::Error;

use super::refs::ProcessInvoke;
use crate::lua::CredentialFor;

/// One queued command both backends admit: it never starts until its
/// release, and the wait sees the run's answer.
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

/// A held queued process command: the handler runs on release, and the
/// wait sees its answer. Release runs the handler on the releasing thread,
/// so the door protocol holds: admit, wait on one thread, release on the
/// other.
pub(crate) struct ProcessQueued {
    /// The run, guarded: release runs it once, and the wait sees the one
    /// answer.
    state: Mutex<ProcessQueuedState>,
    /// Wakes the wait when release answers.
    done: Condvar,
}

struct ProcessQueuedState {
    /// The owner that registered the command, for the stopped message.
    owner: String,
    /// The command's handler, taken by the release that runs it.
    invoke: Option<ProcessInvoke>,
    /// The command's text, as its handler receives it.
    arg: Value,
    /// The invocation's credential scope: commands run user code, never
    /// a provider's `credential()`, so a queued command carries none.
    scope: Option<CredentialFor>,
    /// The run's answer, once release ran it.
    answer: Option<Result<Value, Error>>,
}

impl ProcessQueued {
    /// A held call of `owner` running `invoke` on `arg` at release,
    /// carrying `scope`.
    pub(crate) fn new(
        owner: String,
        invoke: ProcessInvoke,
        arg: Value,
        scope: Option<CredentialFor>,
    ) -> Self {
        Self {
            state: Mutex::new(ProcessQueuedState {
                owner,
                invoke: Some(invoke),
                arg,
                scope,
                answer: None,
            }),
            done: Condvar::new(),
        }
    }

    /// Runs the handler, so the wait sees its answer. A second release
    /// changes nothing: the first answer stands.
    pub(crate) fn release(&self) {
        let mut state = lock(&self.state);
        if state.answer.is_some() {
            return;
        }
        let invoke = state.invoke.take();
        let arg = state.arg.clone();
        let scope = state.scope.clone();
        if let Some(invoke) = invoke {
            let cancel = support::cancel::Token::new();
            // The driver's return value is ignored (`docs/extensions.md`,
            // "Commands and screens"): answering nothing completes, and only
            // an error fails the wait.
            state.answer =
                Some(invoke(arg, scope, &cancel).map(|returned| returned.unwrap_or(Value::Null)));
        }
        self.done.notify_all();
    }

    /// Waits until a release answers the run, and takes the one answer: a
    /// second wait finds no answer and fails stopped instead of hanging.
    pub(crate) fn wait(&self) -> Result<Value, Error> {
        let mut state = lock(&self.state);
        while state.answer.is_none() && state.invoke.is_some() {
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
