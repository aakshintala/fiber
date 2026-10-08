//! The `scripted` protocol (`docs/model-routing.md`, "The scripted
//! provider"): it reads a script file instead of a socket. Request `n`,
//! counted per [`Scripted`] value from 1, takes step `n`; no request content
//! is read, so a prompt that changes does not change the reply. A request
//! after the last step fails `invalid_request`.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::events::{ReasoningCompleted, TextCompleted, TextDelta, ToolCallRequested};
use contract::provider::{
    CallError, CallUsage, Delta, Finish, InputSize, ModelCall, ModelRequest, Provider, Reply,
    ReplyAction,
};
use contract::shapes::{Failure, Tokens};
use contract::{ErrorCode, ProviderCallId};
use serde_json::{Map, Value};

#[cfg(test)]
#[path = "scripted_tests.rs"]
mod tests;

/// The codes a scripted error step may fail with: those of a failed model
/// call (`docs/errors.md`, "A failed model call").
const CALL_CODES: [ErrorCode; 11] = [
    ErrorCode::RateLimited,
    ErrorCode::ProviderUnavailable,
    ErrorCode::ConnectionFailed,
    ErrorCode::StreamIncomplete,
    ErrorCode::QuotaExceeded,
    ErrorCode::AuthenticationFailed,
    ErrorCode::ContextOverflow,
    ErrorCode::Refused,
    ErrorCode::ModelNotFound,
    ErrorCode::InvalidRequest,
    ErrorCode::UnknownStopReason,
];

/// A script that cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    /// The file could not be read.
    #[error("The script `{}` could not be read: {source}.", path.display())]
    Unreadable {
        /// The script's path.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// The file is not a script.
    #[error("The script `{}` is malformed{}: {reason}.", path.display(), at(*step))]
    Malformed {
        /// The script's path.
        path: PathBuf,
        /// The malformed step, from 1; `None` for the file as a whole.
        step: Option<usize>,
        /// What is wrong.
        reason: String,
    },
}

/// ` at step <n>`, or nothing for the file as a whole.
fn at(step: Option<usize>) -> String {
    step.map(|n| format!(" at step {n}")).unwrap_or_default()
}

/// A script: its steps, one model reply each, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    steps: Vec<Step>,
}

/// One step: a reply, or the failure the request ends with.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    Reply(ReplyStep),
    Error(Failure),
}

/// A reply: its reasoning, its text fragments, its tool calls, the tokens it
/// reports and the pause before every text fragment after the first.
#[derive(Debug, Clone, PartialEq)]
struct ReplyStep {
    reasoning: Option<String>,
    text: Vec<String>,
    tool_calls: Vec<(String, Value)>,
    tokens: Tokens,
    every: Duration,
}

impl Script {
    /// Reads and parses the script at `path`, whole.
    pub fn read(path: &Path) -> Result<Self, ScriptError> {
        let bytes = std::fs::read(path).map_err(|source| ScriptError::Unreadable {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(path, &bytes)
    }

    /// Parses a script file's bytes: `{"steps": [...]}`, each step an
    /// object (`docs/model-routing.md`, "The scripted provider").
    pub fn parse(path: &Path, bytes: &[u8]) -> Result<Self, ScriptError> {
        let malformed = |step: Option<usize>, reason: String| ScriptError::Malformed {
            path: path.to_owned(),
            step,
            reason,
        };
        let top: Value = serde_json::from_slice(bytes)
            .map_err(|e| malformed(None, format!("it is not JSON ({e})")))?;
        let steps = match top.as_object() {
            Some(object) if object.len() == 1 => object.get("steps").and_then(Value::as_array),
            Some(_) | None => None,
        }
        .ok_or_else(|| {
            malformed(
                None,
                "it must be an object with one key, `steps`, a list".into(),
            )
        })?;
        let steps = steps
            .iter()
            .enumerate()
            .map(|(index, step)| {
                parse_step(step).map_err(|reason| malformed(Some(index + 1), reason))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { steps })
    }
}

fn parse_step(step: &Value) -> Result<Step, String> {
    let object = step.as_object().ok_or("the step is not an object")?;
    if let Some(error) = object.get("error") {
        if let Some(other) = object.keys().find(|key| *key != "error") {
            return Err(format!(
                "an `error` step takes no other key, such as `{other}`"
            ));
        }
        return parse_error(error).map(Step::Error);
    }
    only(
        object,
        &["text", "tool_calls", "reasoning", "usage", "every_ms"],
    )?;
    let text = match object.get("text") {
        None => Vec::new(),
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(fragments)) if !fragments.is_empty() => fragments
            .iter()
            .map(|fragment| fragment.as_str().map(str::to_owned))
            .collect::<Option<_>>()
            .ok_or("`text` must be a string or a list of strings")?,
        Some(_) => return Err("`text` must be a string or a non-empty list of strings".into()),
    };
    let tool_calls = match object.get("tool_calls") {
        None => Vec::new(),
        Some(Value::Array(calls)) => calls
            .iter()
            .map(parse_tool_call)
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("`tool_calls` must be a list".into()),
    };
    if text.is_empty() && tool_calls.is_empty() {
        return Err("a reply needs `text` or `tool_calls`".into());
    }
    let reasoning = match object.get("reasoning") {
        None => None,
        Some(Value::String(reasoning)) => Some(reasoning.clone()),
        Some(_) => return Err("`reasoning` must be a string".into()),
    };
    let tokens = match object.get("usage") {
        None => Tokens {
            input: 0,
            cache_read: 0,
            cache_write: Default::default(),
            output: 0,
        },
        Some(Value::Object(usage)) => {
            only(usage, &["input", "output", "cache_read"])?;
            Tokens {
                input: count(usage, "input")?,
                cache_read: count(usage, "cache_read")?,
                cache_write: Default::default(),
                output: count(usage, "output")?,
            }
        }
        Some(_) => return Err("`usage` must be an object".into()),
    };
    let every = Duration::from_millis(count(object, "every_ms")?);
    Ok(Step::Reply(ReplyStep {
        reasoning,
        text,
        tool_calls,
        tokens,
        every,
    }))
}

fn parse_tool_call(call: &Value) -> Result<(String, Value), String> {
    let object = call
        .as_object()
        .ok_or("each of `tool_calls` must be an object")?;
    only(object, &["name", "arguments"])?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or("a tool call needs `name`, a string")?;
    let arguments = object
        .get("arguments")
        .filter(|arguments| arguments.is_object())
        .ok_or("a tool call needs `arguments`, an object")?;
    Ok((name.to_owned(), arguments.clone()))
}

fn parse_error(error: &Value) -> Result<Failure, String> {
    let object = error.as_object().ok_or("`error` must be an object")?;
    only(object, &["code", "message", "retry_after_ms"])?;
    let name = object
        .get("code")
        .and_then(Value::as_str)
        .ok_or("an error needs `code`, a string")?;
    let code = serde_json::from_value::<ErrorCode>(Value::String(name.to_owned()))
        .ok()
        .filter(|code| CALL_CODES.contains(code))
        .ok_or_else(|| format!("`{name}` is not a code a model call fails with"))?;
    let message = object
        .get("message")
        .and_then(Value::as_str)
        .ok_or("an error needs `message`, a string")?;
    let retry_after_ms = match object.get("retry_after_ms") {
        None => None,
        Some(_) => Some(count(object, "retry_after_ms")?),
    };
    Ok(Failure {
        code,
        message: message.to_owned(),
        retry_after_ms,
        provider: None,
    })
}

/// Refuses the first key of `object` not in `allowed`, naming it.
fn only(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match object.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(format!("`{key}` is not a key it takes")),
        None => Ok(()),
    }
}

/// The whole number under `key`; 0 when it is absent.
fn count(object: &Map<String, Value>, key: &str) -> Result<u64, String> {
    match object.get(key) {
        None => Ok(0),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| format!("`{key}` must be a whole number at or above 0")),
    }
}

/// The `scripted` protocol for one script file: each [`Provider::call`]
/// takes the next step.
pub struct Scripted {
    path: PathBuf,
    steps: Arc<Vec<Step>>,
    taken: AtomicUsize,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for Scripted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scripted")
            .field("path", &self.path)
            .field("steps", &self.steps.len())
            .finish()
    }
}

impl Scripted {
    /// A provider serving `script`, read from `path`, whose pauses wait on
    /// `clock`. Its first request takes step 1.
    pub fn new(path: PathBuf, script: Script, clock: Arc<dyn Clock>) -> Self {
        Self {
            path,
            steps: Arc::new(script.steps),
            taken: AtomicUsize::new(0),
            clock,
        }
    }
}

impl Provider for Scripted {
    fn call(&self, _request: &ModelRequest) -> Box<dyn ModelCall> {
        let request = self.taken.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        Box::new(Call {
            path: self.path.clone(),
            steps: Arc::clone(&self.steps),
            request,
            clock: Arc::clone(&self.clock),
            pause: Arc::new(Pause::default()),
        })
    }
}

/// One request: its step, by number from 1.
struct Call {
    path: PathBuf,
    steps: Arc<Vec<Step>>,
    request: usize,
    clock: Arc<dyn Clock>,
    pause: Arc<Pause>,
}

/// The cancel flag and a sequence a clock move or a cancel bumps, under the
/// lock a pause waits on.
#[derive(Default)]
struct Pause {
    state: Mutex<PauseState>,
    cv: Condvar,
}

#[derive(Default)]
struct PauseState {
    cancelled: bool,
    seq: u64,
}

impl Pause {
    fn lock(&self) -> MutexGuard<'_, PauseState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Bumps the sequence, and sets the flag when `cancel`, under the lock,
    /// then wakes the pause.
    fn bump(&self, cancel: bool) {
        let mut state = self.lock();
        state.seq = state.seq.wrapping_add(1);
        state.cancelled |= cancel;
        drop(state);
        self.cv.notify_all();
    }
}

impl Wake for Pause {
    fn wake(&self) {
        self.bump(false);
    }
}

/// Usage carrying `tokens`, no generation and no request bytes: no request
/// body is built.
fn usage(tokens: Tokens) -> CallUsage {
    CallUsage {
        tokens,
        ..CallUsage::unnamed(InputSize::default())
    }
}

impl Call {
    /// Waits until `until` passes on the clock, or a cancel; true when the
    /// time passed, false on a cancel.
    fn wait(&self, until: Instant) -> bool {
        loop {
            let seen = {
                let state = self.pause.lock();
                if state.cancelled {
                    return false;
                }
                state.seq
            };
            if self.clock.now() >= until {
                return true;
            }
            // Held until the condvar wait, so a wake that lands after `seen`
            // was read is seen here instead of notifying nobody.
            let mut slot = Some(self.pause.lock());
            self.clock.wait_until(Some(until), &mut |bound| {
                let Some(guard) = slot.take() else {
                    return;
                };
                // A cancel bumps the sequence too.
                if guard.seq != seen {
                    slot = Some(guard);
                    return;
                }
                slot = Some(match bound {
                    Some(bound) => {
                        self.pause
                            .cv
                            .wait_timeout(guard, bound)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0
                    }
                    None => self
                        .pause
                        .cv
                        .wait(guard)
                        .unwrap_or_else(PoisonError::into_inner),
                });
            });
        }
    }

    #[allow(
        clippy::result_large_err,
        reason = "the seam returns the call's error by value"
    )]
    fn stream(&self, step: &ReplyStep, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        let cancelled = || CallError::Cancelled {
            usage: Box::new(usage(step.tokens.clone())),
        };
        let mut actions = Vec::new();
        if let Some(text) = &step.reasoning {
            sink(Delta::Reasoning(TextDelta { text: text.clone() }));
            actions.push(ReplyAction::Reasoning(ReasoningCompleted {
                text: text.clone(),
                provider_item: None,
            }));
        }
        self.clock
            .subscribe(Arc::downgrade(&(Arc::clone(&self.pause) as Arc<dyn Wake>)));
        for (index, fragment) in step.text.iter().enumerate() {
            // `every_ms` paces every fragment after the first; a wait of 0
            // returns at once.
            if index > 0
                && let Some(until) = self.clock.now().checked_add(step.every)
                && !self.wait(until)
            {
                return Err(cancelled());
            }
            sink(Delta::Text(TextDelta {
                text: fragment.clone(),
            }));
        }
        if !step.text.is_empty() {
            actions.push(ReplyAction::Text(TextCompleted {
                text: step.text.concat(),
                provider_item: None,
            }));
        }
        for (n, (name, arguments)) in step.tool_calls.iter().enumerate() {
            actions.push(ReplyAction::ToolCall(ToolCallRequested {
                name: name.clone(),
                arguments: arguments.clone(),
                provider_id: Some(ProviderCallId(format!(
                    "call_{}_{}",
                    self.request,
                    n.saturating_add(1)
                ))),
                repair: None,
                ran_by: None,
                provider_item: None,
            }));
        }
        Ok(Reply {
            actions,
            finish: Finish::Completed,
            generation_id: None,
            tokens: step.tokens.clone(),
            web_searches: None,
            cost: None,
            input_size: InputSize::default(),
        })
    }
}

impl ModelCall for Call {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        let zero = || {
            Box::new(usage(Tokens {
                input: 0,
                cache_read: 0,
                cache_write: Default::default(),
                output: 0,
            }))
        };
        if self.pause.lock().cancelled {
            return Err(CallError::Cancelled { usage: zero() });
        }
        match self.steps.get(self.request.saturating_sub(1)) {
            None => Err(CallError::Failed {
                failure: Failure {
                    code: ErrorCode::InvalidRequest,
                    message: format!(
                        "The script `{}` has no step for request {}; it has {} {}.",
                        self.path.display(),
                        self.request,
                        self.steps.len(),
                        if self.steps.len() == 1 {
                            "step"
                        } else {
                            "steps"
                        },
                    ),
                    retry_after_ms: None,
                    provider: None,
                },
                should_retry: None,
                usage: zero(),
            }),
            Some(Step::Error(failure)) => Err(CallError::Failed {
                failure: failure.clone(),
                should_retry: None,
                usage: zero(),
            }),
            Some(Step::Reply(step)) => self.stream(step, sink),
        }
    }

    fn cancel(&self) {
        self.pause.bump(true);
    }
}
