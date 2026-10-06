//! A provider plugged straight into the provider seam (`contract::provider`),
//! with no protocol and no socket: each call streams a scripted reply and
//! records the request it was given. The loop's tests and its `turn` jig run
//! against it (`docs/testing.md`, "Levels", "Across crates").

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use contract::events::TextCompleted;
use contract::provider::{
    CallError, Delta, Finish, ModelCall, ModelRequest, Provider, Reply, ReplyAction,
};
use contract::shapes::{Failure, Tokens};
use contract::{ErrorCode, GenerationId};

/// One scripted model call: the fragments it streams, then how it ends.
#[derive(Debug, Clone, PartialEq)]
pub struct Scripted {
    /// Streamed in order before the call ends.
    pub deltas: Vec<Delta>,
    /// The reply, or the failure.
    pub end: Result<Reply, CallError>,
}

impl Scripted {
    /// A reply of `text` alone, streamed in two fragments.
    pub fn text(text: &str) -> Self {
        let middle = text.char_indices().nth(text.chars().count() / 2);
        let (head, tail) = text.split_at(middle.map_or(text.len(), |(i, _)| i));
        Self {
            deltas: [head, tail]
                .into_iter()
                .filter(|part| !part.is_empty())
                .map(|part| Delta::Text(contract::events::TextDelta { text: part.into() }))
                .collect(),
            end: Ok(reply(text)),
        }
    }

    /// A call that fails with `failure`.
    pub fn failed(failure: Failure) -> Self {
        Self {
            deltas: Vec::new(),
            end: Err(CallError::Failed {
                failure,
                should_retry: None,
            }),
        }
    }
}

/// A completed reply of one text part, or none when `text` is `""`,
/// generation `gen_1`, and 10 input and 3 output tokens.
pub fn reply(text: &str) -> Reply {
    Reply {
        actions: if text.is_empty() {
            Vec::new()
        } else {
            vec![ReplyAction::Text(TextCompleted {
                text: text.into(),
                provider_item: None,
            })]
        },
        finish: Finish::Completed,
        generation_id: GenerationId("gen_1".into()),
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: Default::default(),
            output: 3,
        },
        web_searches: None,
        cost: None,
    }
}

/// Answers each call with the next scripted one, in order. A call past the
/// end of the script fails with code `script_exhausted`.
#[derive(Debug, Default)]
pub struct ScriptedProvider {
    script: Mutex<VecDeque<Scripted>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedProvider {
    /// A provider that answers with `script`, one entry per call.
    pub fn new(script: impl IntoIterator<Item = Scripted>) -> Self {
        Self {
            script: Mutex::new(script.into_iter().collect()),
            requests: Mutex::default(),
        }
    }

    /// Every request it was given, in order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Provider for ScriptedProvider {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());
        let next = self
            .script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or_else(|| {
                Scripted::failed(Failure {
                    code: ErrorCode::Other("script_exhausted".into()),
                    message: "The scripted provider has no reply left.".into(),
                    retry_after: None,
                    provider: None,
                })
            });
        Box::new(Call {
            scripted: Mutex::new(Some(next)),
            cancelled: AtomicBool::new(false),
        })
    }
}

/// One scripted call. It runs once; cancelling it before `run` makes `run`
/// return at once.
struct Call {
    scripted: Mutex<Option<Scripted>>,
    cancelled: AtomicBool,
}

impl ModelCall for Call {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        let scripted = self
            .scripted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(scripted) = scripted else {
            return Err(CallError::Cancelled);
        };
        for delta in scripted.deltas {
            if self.cancelled.load(Ordering::SeqCst) {
                return Err(CallError::Cancelled);
            }
            sink(delta);
        }
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(CallError::Cancelled);
        }
        scripted.end
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "scripted_provider_tests.rs"]
mod tests;
