//! A provider plugged straight into the provider seam (`contract::provider`),
//! with no protocol and no socket: each call streams a scripted reply and
//! records the request it was given. The loop's tests and its `turn` jig run
//! against it (`docs/testing.md`, "Levels", "Across crates").

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use contract::events::TextCompleted;
use contract::provider::{
    CallError, CallUsage, Delta, Finish, InputSize, ModelCall, ModelRequest, Provider, Reply,
    ReplyAction,
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

    /// A call that fails with `failure` before any generation.
    pub fn failed(failure: Failure) -> Self {
        Self {
            deltas: Vec::new(),
            end: Err(CallError::Failed {
                failure,
                should_retry: None,
                usage: None,
            }),
        }
    }

    /// A call that fails with `failure` after reporting `usage`.
    pub fn failed_after(failure: Failure, usage: CallUsage) -> Self {
        Self {
            deltas: Vec::new(),
            end: Err(CallError::Failed {
                failure,
                should_retry: None,
                usage: Some(Box::new(usage)),
            }),
        }
    }

    /// A call cancelled after reporting `usage`.
    pub fn cancelled_after(usage: CallUsage) -> Self {
        Self {
            deltas: Vec::new(),
            end: Err(CallError::Cancelled {
                usage: Some(Box::new(usage)),
            }),
        }
    }
}

/// A completed reply of one text part, or none when `text` is `""`,
/// generation `gen_1`, 10 input and 3 output tokens, and 1000 input bytes.
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
        input_size: InputSize {
            bytes: 1000,
            media: false,
        },
    }
}

/// A call's generation and what it reported: 10 input and 3 output tokens
/// with 1000 input bytes, as `reply` uses.
pub fn call_usage(generation: &str) -> CallUsage {
    CallUsage {
        generation_id: GenerationId(generation.into()),
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: Default::default(),
            output: 3,
        },
        web_searches: None,
        input_size: InputSize {
            bytes: 1000,
            media: false,
        },
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
                    retry_after_ms: None,
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
            return Err(CallError::Cancelled { usage: None });
        };
        for delta in scripted.deltas {
            if self.cancelled.load(Ordering::SeqCst) {
                return cancelled_end(scripted.end);
            }
            sink(delta);
        }
        if self.cancelled.load(Ordering::SeqCst) {
            return cancelled_end(scripted.end);
        }
        scripted.end
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

/// What a cancelled scripted call returns: its scripted end when that end
/// is itself `Cancelled` (what the call saw when the cancel landed), and
/// `Cancelled` with no usage otherwise.
#[allow(
    clippy::result_large_err,
    reason = "the seam returns the call's error by value; the test fake mirrors it"
)]
fn cancelled_end(end: Result<Reply, CallError>) -> Result<Reply, CallError> {
    match end {
        Err(CallError::Cancelled { .. }) => end,
        _ => Err(CallError::Cancelled { usage: None }),
    }
}

#[cfg(test)]
#[path = "scripted_provider_tests.rs"]
mod tests;
