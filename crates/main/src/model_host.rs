//! The session's routing and credentials behind `host.model`
//! (`docs/extensions.md`, "Host calls"): one model request through the
//! session's provider with the session's retry policy, serialized per
//! session by a budget gate.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak, mpsc};
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::events::{CacheLifetime, UsageRecorded};
use contract::model::{Bound, HostMessage, HostModel, HostModelReply, HostModelRequest, HostRole};
use contract::provider::{CallError, CallUsage, Delta, ImageRef, Input, ModelCall, ModelRequest};
use contract::shapes::{ContentPart, Failure};
use contract::tool::Cancel;
use contract::{ErrorCode, GenerationId, SessionId};

use crate::switch::{Reach, Reached};

/// One model request through the session's routing and credentials, made by
/// an extension's `host.model` call. Blocking; called from the worker thread
/// the admit arm spawns.
pub(crate) struct ModelHost {
    /// How this host reaches its model reference: resolving it against
    /// the registry and connecting it with the session's current
    /// credential label, once per call, right after the budget gate and
    /// before the first budget check. The reached route serves every
    /// attempt and retry of that call.
    reach: Reach,
    /// `budget.usd`, or none when the key is absent or not a number.
    budget: Option<f64>,
    /// How a failed model call is retried.
    retry: r#loop::Retry,
    /// The session directory, whose log the spend folds.
    dir: PathBuf,
    /// The session's id, naming the request's cache key.
    id: SessionId,
    /// The gate serializing one session's calls.
    gate: Arc<Gate>,
}

/// Serializes one session's `host.model` calls: the in-flight flag, and the
/// lines this host has produced.
struct Gate {
    /// Whether a call holds the session's send slot.
    in_flight: Mutex<bool>,
    /// Woken when the slot clears and when the call's wake fires.
    changed: Condvar,
    /// Every line this host produced, in order, however the log lags.
    lines: Mutex<Vec<UsageRecorded>>,
}

/// Clears the gate when the call returns, however it returns.
struct Guard<'a> {
    gate: &'a Gate,
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        *self
            .gate
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = false;
        self.gate.changed.notify_one();
    }
}

/// Wakes a gate waiter when the session clock moves or the callback is
/// cancelled, under the gate's lock, so no wake is lost.
struct GateWake {
    gate: Weak<Gate>,
}

impl Wake for GateWake {
    fn wake(&self) {
        if let Some(gate) = self.gate.upgrade() {
            let _guard = gate
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            gate.changed.notify_all();
        }
    }
}

/// Wakes one waiter when the session clock moves or the callback is
/// cancelled, under that waiter's lock, so no wake is lost.
struct WaitWake {
    state: Weak<(Mutex<bool>, Condvar)>,
}

impl Wake for WaitWake {
    fn wake(&self) {
        if let Some(state) = self.state.upgrade() {
            let _guard = state.0.lock().unwrap_or_else(PoisonError::into_inner);
            state.1.notify_all();
        }
    }
}

/// One attempt's end, its line already passed on.
enum Attempt {
    /// A reply came back: its text and counts.
    Replied {
        text: String,
        tokens: contract::shapes::Tokens,
        cost: Option<f64>,
    },
    /// The provider failed: the retry policy decides.
    Failed {
        failure: Failure,
        should_retry: Option<bool>,
    },
    /// The callback ended first: the deadline passed or it was cancelled.
    TimedOut,
}

impl ModelHost {
    /// Builds the host from the session's reach, where `drive_to` stands:
    /// each call resolves its own model reference against the registry
    /// and connects it with the session's current credential label. The
    /// retry policy arrives by reference: the session's later steps still
    /// own it (`Retry` has public fields and no `Clone`, so its fields
    /// are copied here).
    pub(crate) fn new(
        reach: Reach,
        budget: Option<f64>,
        retry: &r#loop::Retry,
        dir: PathBuf,
        id: SessionId,
    ) -> Self {
        Self {
            reach,
            budget,
            retry: r#loop::Retry {
                attempts: retry.attempts,
                initial: retry.initial,
                max: retry.max,
            },
            dir,
            id,
            gate: Arc::new(Gate {
                in_flight: Mutex::new(false),
                changed: Condvar::new(),
                lines: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Runs the request to completion with the session's retry policy.
    fn call_impl(
        &self,
        extension: &str,
        request: &HostModelRequest,
        bound: &Bound,
    ) -> Result<HostModelReply, Failure> {
        // Registered before the first check, so a cancel that lands first
        // still wakes the wait: the checks below read it after subscribing.
        let wake: Arc<dyn Wake> = Arc::new(GateWake {
            gate: Arc::downgrade(&self.gate),
        });
        bound.clock.subscribe(Arc::downgrade(&wake));
        bound.cancel.subscribe(Arc::downgrade(&wake));
        let _keep = &wake;
        self.acquire(bound)?;
        let _release = Guard { gate: &self.gate };
        // Once per call, after the gate and before the first budget check:
        // a reach failure (`model_unconfigured` for a reference that
        // cannot connect, or the read's own code) returns with no send
        // and the gate released. The `Reached` serves every attempt and
        // retry of this call.
        let reached = (self.reach)(&request.model)?;
        let mut retries: u32 = 0;
        loop {
            self.check_budget()?;
            match self.attempt_once(&reached, extension, request, bound) {
                Attempt::Replied { text, tokens, cost } => {
                    return Ok(HostModelReply { text, tokens, cost });
                }
                Attempt::Failed {
                    failure,
                    should_retry,
                } => match self.retry.decide(&failure, should_retry, retries) {
                    r#loop::Decision::Fail(failure) => return Err(failure),
                    r#loop::Decision::Retry(wait) => {
                        retries += 1;
                        self.wait_retry(wait, bound)?;
                    }
                },
                Attempt::TimedOut => return Err(timeout()),
            }
        }
    }

    /// Takes the session's send slot, waiting through the callback's bound.
    /// A cancelled or expired wait ends `timeout` with no send. With no
    /// deadline the wait blocks on the condvar itself: a fake clock's
    /// unbounded wait returns at once, so parking through it would spin.
    fn acquire(&self, bound: &Bound) -> Result<(), Failure> {
        let mut guard = self
            .gate
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            if !*guard {
                *guard = true;
                return Ok(());
            }
            if bound.cancel.is_cancelled() || expired(&bound.clock, bound.deadline) {
                return Err(timeout());
            }
            match bound.deadline {
                Some(_) => {
                    let parked = support::clock::park(
                        &*bound.clock,
                        bound.deadline,
                        None,
                        &self.gate.changed,
                        guard,
                        |_| false,
                    );
                    match parked {
                        Some(held) => guard = held,
                        None => {
                            guard = self
                                .gate
                                .in_flight
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner);
                        }
                    }
                }
                None => {
                    guard = self
                        .gate
                        .changed
                        .wait(guard)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
    }

    /// Fails `budget_exceeded` when the billed spend has reached the cap.
    /// The spend folds the log plus this host's own lines, so a line the
    /// inbox has not written yet still counts.
    fn check_budget(&self) -> Result<(), Failure> {
        let Some(limit) = self.budget else {
            return Ok(());
        };
        if self.spend()? < limit {
            return Ok(());
        }
        Err(Failure {
            code: ErrorCode::BudgetExceeded,
            message: format!("host.model: the budget of {limit:.2} USD is reached"),
            retry_after_ms: None,
            provider: None,
        })
    }

    /// The billed spend in US dollars: the log's `usage_recorded` lines plus
    /// this host's own lines, keeping the last line per `generation_id`, in
    /// order, folded as the loop's ledger folds them (`docs/events.md`,
    /// "Usage and notices"). A `log::read` error fails closed with the
    /// error's code and sends nothing.
    fn spend(&self) -> Result<f64, Failure> {
        let envelopes = log::read(&self.dir).map_err(|error| Failure {
            code: error.code(),
            message: error.to_string(),
            retry_after_ms: None,
            provider: None,
        })?;
        let mut by_id: BTreeMap<GenerationId, UsageRecorded> = BTreeMap::new();
        for (number, envelope) in envelopes.iter().enumerate() {
            if envelope.kind != "usage_recorded" {
                continue;
            }
            let line: UsageRecorded = serde_json::from_value(serde_json::Value::Object(
                envelope.payload.clone(),
            ))
            .map_err(|source| Failure {
                code: ErrorCode::LogCorrupt,
                // A folded line whose payload does not read as its kind is
                // corrupt, as the history fold treats one (`history.rs`).
                message: format!(
                    "host.model: the session log's line {} does not parse as its kind: {source}",
                    number + 1
                ),
                retry_after_ms: None,
                provider: None,
            })?;
            by_id.insert(line.generation_id.clone(), line);
        }
        for line in self
            .gate
            .lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            by_id.insert(line.generation_id.clone(), line.clone());
        }
        Ok(log::usage(by_id.values()).cost.unwrap_or(0.0))
    }

    /// Sends the request once and runs the attempt to its end, passing one
    /// line to `bound.lines` and keeping it, however the attempt ended.
    fn attempt_once(
        &self,
        reached: &Reached,
        extension: &str,
        request: &HostModelRequest,
        bound: &Bound,
    ) -> Attempt {
        let model_request = self.request(extension, request);
        let call: Arc<dyn ModelCall> = reached.provider.call(&model_request).into();
        // The attempt runs on its own thread; the watcher below cancels it
        // when the callback ends. A provider that ignores `cancel` keeps
        // this thread, and the gate keeps every other call bounded.
        let (tx, rx) = mpsc::channel();
        let running = Arc::clone(&call);
        std::thread::spawn(move || {
            let mut dropped = |_: Delta| {};
            let _sent = tx.send(running.run(&mut dropped));
        });
        let finished = Arc::new((Mutex::new(false), Condvar::new()));
        let watcher = std::thread::spawn({
            let finished = Arc::clone(&finished);
            let call = Arc::clone(&call);
            let clock = Arc::clone(&bound.clock);
            let cancel = Arc::clone(&bound.cancel);
            let deadline = bound.deadline;
            move || Self::watch(call, clock, cancel, deadline, finished)
        });
        let outcome = rx.recv();
        *finished.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
        finished.1.notify_all();
        let _joined = watcher.join();
        match outcome {
            // A run that never answers ends the callback at its bound.
            Err(_) => Attempt::TimedOut,
            Ok(Ok(reply)) => {
                let line = self.line(reached, extension, request, reply.usage(), reply.cost);
                let text = reply.text();
                let attempt = Attempt::Replied {
                    text,
                    tokens: line.tokens.clone(),
                    cost: line.cost,
                };
                self.emit(&line, bound);
                attempt
            }
            Ok(Err(CallError::Failed {
                failure,
                should_retry,
                usage,
            })) => {
                let line = self.line(reached, extension, request, *usage, None);
                self.emit(&line, bound);
                Attempt::Failed {
                    failure,
                    should_retry,
                }
            }
            Ok(Err(CallError::Cancelled { usage })) => {
                let line = self.line(reached, extension, request, *usage, None);
                self.emit(&line, bound);
                Attempt::TimedOut
            }
        }
    }

    /// Parks until the run ends, then returns; when the callback ends
    /// first, cancels the run and returns. With no deadline the watcher
    /// blocks on the condvar itself: a fake clock's unbounded wait returns
    /// at once, so parking through it would spin.
    fn watch(
        call: Arc<dyn ModelCall>,
        clock: Arc<dyn Clock>,
        cancel: Arc<dyn Cancel>,
        deadline: Option<Instant>,
        finished: Arc<(Mutex<bool>, Condvar)>,
    ) {
        let wake: Arc<dyn Wake> = Arc::new(WaitWake {
            state: Arc::downgrade(&finished),
        });
        clock.subscribe(Arc::downgrade(&wake));
        cancel.subscribe(Arc::downgrade(&wake));
        loop {
            let guard = finished.0.lock().unwrap_or_else(PoisonError::into_inner);
            if *guard {
                return;
            }
            if cancel.is_cancelled() || expired(&clock, deadline) {
                call.cancel();
                return;
            }
            match deadline {
                Some(_) => {
                    let parked =
                        support::clock::park(&*clock, deadline, None, &finished.1, guard, |_| {
                            false
                        });
                    if parked.is_none() {
                        continue;
                    }
                }
                None => {
                    let _waiting = finished
                        .1
                        .wait(guard)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
    }

    /// Waits out the retry backoff, ending at the callback's bound.
    /// A wait that would pass the deadline fails `timeout` with no send.
    fn wait_retry(&self, wait: Duration, bound: &Bound) -> Result<(), Failure> {
        let target = bound.clock.now() + wait;
        if bound.deadline.is_some_and(|deadline| target > deadline) {
            return Err(timeout());
        }
        let waiting = Arc::new((Mutex::new(false), Condvar::new()));
        let wake: Arc<dyn Wake> = Arc::new(WaitWake {
            state: Arc::downgrade(&waiting),
        });
        bound.clock.subscribe(Arc::downgrade(&wake));
        bound.cancel.subscribe(Arc::downgrade(&wake));
        loop {
            if bound.cancel.is_cancelled() || expired(&bound.clock, bound.deadline) {
                return Err(timeout());
            }
            if bound.clock.now() >= target {
                return Ok(());
            }
            let guard = waiting.0.lock().unwrap_or_else(PoisonError::into_inner);
            let parked =
                support::clock::park(&*bound.clock, Some(target), None, &waiting.1, guard, |_| {
                    false
                });
            if parked.is_none() {
                return Err(timeout());
            }
        }
    }

    /// The `ModelRequest` for one send (`docs/prompt-cache.md`, "What a
    /// request is built from"): no tools, so no hosted tool runs and no
    /// pause is reported; the drops-every-delta sink in `attempt_once`
    /// discards what streams.
    fn request(&self, extension: &str, request: &HostModelRequest) -> ModelRequest {
        let conversation: Vec<Input> = request
            .messages
            .iter()
            .map(|message| self.input(&request.model, message))
            .collect();
        // The images are read from the session's own directory, by the
        // loop's rule, so an image part reaches the model as it does in a
        // turn.
        let media = r#loop::resolve_media(&self.dir, &conversation);
        ModelRequest {
            system_prompt: request.system.clone().unwrap_or_default(),
            tools: Vec::new(),
            thinking: None,
            // `none` returns 200 without tools (`docs/model-routing.md`).
            tool_choice: "none".to_owned(),
            cache_lifetime: CacheLifetime::FiveMinutes,
            cache_key: format!("{}:extension:{extension}", self.id.0),
            conversation: conversation.into(),
            sent_tools: None,
            previous_end: None,
            max_output_tokens: Some(request.max_tokens),
            media,
        }
    }

    /// One message as the model reads it, mirroring the loop's own mapping
    /// (`conversation::user`): text joined, images referenced. A PDF part
    /// has no user input to ride, as there.
    fn input(&self, model: &str, message: &HostMessage) -> Input {
        match message.role {
            HostRole::User => Input::User {
                text: text(&message.content),
                images: images(&message.content),
            },
            HostRole::Assistant => Input::Assistant {
                model: model.to_owned(),
                text: text(&message.content),
                provider_item: None,
            },
        }
    }

    /// One `usage_recorded` for the attempt: the helper every attempt's line
    /// takes, with the calling extension set. The reached model's declared
    /// prices figure the line; a model with no declared cost keeps the
    /// vendor figure or null (`docs/events.md`, `usage_recorded`).
    fn line(
        &self,
        reached: &Reached,
        extension: &str,
        request: &HostModelRequest,
        usage: CallUsage,
        inline_cost: Option<f64>,
    ) -> UsageRecorded {
        r#loop::extension_line(
            usage,
            inline_cost,
            &request.model,
            reached.cost.as_ref(),
            reached.subscription,
            extension,
        )
    }

    /// Keeps the produced line, then passes it to the callback: the spend is
    /// real and counted before the inbox writes it.
    fn emit(&self, line: &UsageRecorded, bound: &Bound) {
        self.gate
            .lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(line.clone());
        (bound.lines)(line.clone());
    }
}

/// The text parts of `content`, joined.
fn text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Pdf(_) | ContentPart::Unknown => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The image parts of `content`, in order, as the references a request reads
/// the stored files by.
fn images(content: &[ContentPart]) -> Vec<ImageRef> {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Image {
                path,
                mime_type,
                width,
                height,
            } => Some(ImageRef {
                path: path.clone(),
                mime_type: mime_type.clone(),
                width: *width,
                height: *height,
            }),
            ContentPart::Text { .. } | ContentPart::Pdf(_) | ContentPart::Unknown => None,
        })
        .collect()
}

/// Whether the callback's deadline has passed on its clock.
fn expired(clock: &Arc<dyn Clock>, deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| clock.now() >= deadline)
}

/// The callback ended: its deadline passed or it was cancelled. No send.
fn timeout() -> Failure {
    Failure {
        code: ErrorCode::Timeout,
        message: "host.model: the callback's deadline passed".to_owned(),
        retry_after_ms: None,
        provider: None,
    }
}

impl HostModel for ModelHost {
    fn call(
        &self,
        extension: &str,
        request: HostModelRequest,
        bound: Bound,
    ) -> Result<HostModelReply, Failure> {
        self.call_impl(extension, &request, &bound)
    }
}
