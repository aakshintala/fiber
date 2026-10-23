//! Retrying a failed model call (`docs/model-routing.md`, "When a model call
//! fails"): whether the failure gets another attempt, and how long the wait
//! before it is. The policy lives here, in `loop`, which is the only module
//! that decides what happens next (`docs/architecture.md`, "The call
//! rules"); `provider` keeps the classification (`x-should-retry`,
//! `retry-after`).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use contract::clock::Wake;
use contract::events::{
    AssistantMessageCompleted, Empty, Event, MessageOutcome, RetryScheduled, TurnOutcome,
};
use contract::provider::{CallError, Finish, Input, ModelRequest, Reply, ReplyAction};
use contract::shapes::Failure;
use contract::tool::Cancel as _;
use contract::{ActionId, ErrorCode, TurnId};
use serde_json::Value;

use crate::progress::SharedWake;
use crate::{Error, Step};

/// How many times in a row a turn continues a paused reply before failing
/// it with `paused_too_long` (`docs/loop.md`, "A reply paused by a hosted
/// tool"). Picked, not measured.
const MAX_CONTINUATIONS: u32 = 5;

/// How a failed model call is retried (`docs/configuration.md`): `attempts`
/// retries after the first call, so up to `attempts + 1` calls, with the
/// wait before retry `n` (from 1) at `min(initial * 2^(n-1), max)`.
pub struct Retry {
    /// Retries after the first call. `0` means no retries.
    pub attempts: u32,
    /// The first backoff.
    pub initial: Duration,
    /// The cap on one backoff, and on a wait a server asks for.
    pub max: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            attempts: 3,
            initial: Duration::from_millis(2000),
            max: Duration::from_millis(60000),
        }
    }
}

/// What `decide` says about a failure.
pub enum Decision {
    /// Make another attempt after the wait.
    Retry(Duration),
    /// Record this failure; no further attempt.
    Fail(Failure),
}

impl Retry {
    /// Whether `failure` gets another attempt. `should_retry` is the
    /// response's `x-should-retry` header, as the provider crate sets it;
    /// `retries` is the retries already made.
    ///
    /// `Fail` carries the failure to record: the input unchanged, except an
    /// over-cap wait, which sets `code` to `rate_limited`
    /// (`docs/errors.md`, "A failed model call").
    pub fn decide(&self, failure: &Failure, should_retry: Option<bool>, retries: u32) -> Decision {
        if matches!(
            failure.code,
            ErrorCode::QuotaExceeded | ErrorCode::PausedTooLong | ErrorCode::UnknownStopReason
        ) {
            return Decision::Fail(failure.clone());
        }
        let retryable = match should_retry {
            Some(false) => false,
            Some(true) => true,
            None => matches!(
                failure.code,
                ErrorCode::RateLimited
                    | ErrorCode::ProviderUnavailable
                    | ErrorCode::ConnectionFailed
                    | ErrorCode::StreamIncomplete
            ),
        };
        if !retryable {
            return Decision::Fail(failure.clone());
        }
        if retries >= self.attempts {
            return Decision::Fail(failure.clone());
        }
        if let Some(ms) = failure.retry_after_ms
            && (ms == u64::MAX || Duration::from_millis(ms) > self.max)
        {
            // A wait longer than the cap fails at once as `rate_limited`,
            // so a person or a caller can decide. Whatever the original
            // code was; the asked wait is already on the error.
            let mut over = failure.clone();
            over.code = ErrorCode::RateLimited;
            return Decision::Fail(over);
        }
        let backoff = self.backoff(retries);
        match failure.retry_after_ms {
            Some(ms) => Decision::Retry(backoff.max(Duration::from_millis(ms))),
            None => Decision::Retry(backoff),
        }
    }

    /// The wait before retry `retries + 1`: `min(initial * 2^retries, max)`,
    /// saturating, so huge values never overflow.
    fn backoff(&self, retries: u32) -> Duration {
        self.initial
            .saturating_mul(1u32.checked_shl(retries).unwrap_or(u32::MAX))
            .min(self.max)
    }
}

/// How the model calls of one request ended.
pub(crate) enum Attempted {
    /// A reply came back: the caller writes it.
    Replied {
        reply: Reply,
        reasoning: VecDeque<ActionId>,
        message: ActionId,
    },
    /// The retries ran out, or the failure is not retried.
    Failed(Failure),
    /// A person cancelled the turn.
    Interrupted,
}

impl crate::Loop {
    /// One step's model calls with retries, written as the step's reply
    /// (`docs/model-routing.md`, "When a model call fails"). The request,
    /// the budget check and `sent` are the step's, not per retry: a retry
    /// resends the same request (`docs/loop.md`, "What the model is sent").
    pub(crate) fn attempt(&mut self, request: &ModelRequest, turn: &TurnId) -> Result<Step, Error> {
        // Stamped once per step: a retry's backoff only delays the real
        // send, so a refresh counted from here comes early, never late.
        self.warming.record(request, self.log.clock().now());
        // A step's paused replies and continuations, ending with the step:
        // each resend carries every paused reply's blocks so far, and the
        // fifth continuation is the last one the turn takes.
        let mut request = request.clone();
        let mut paused: Vec<Value> = Vec::new();
        let mut continuations = 0u32;
        loop {
            match self.call_with_retries(&request, turn)? {
                Attempted::Replied {
                    mut reply,
                    reasoning,
                    message,
                } => {
                    Self::resolve_orphans(&mut reply, &paused);
                    // One match on the reply's end, here: a pause with a
                    // resendable block and no client tool call continues the
                    // turn, and anything else takes the normal path. The
                    // guards stay for a scripted provider, which may build a
                    // `Paused` the decoder never would (empty, or with a
                    // client call a pause never carries).
                    let blocks = match &reply.finish {
                        Finish::Paused { content }
                            if !content.is_empty()
                                && !reply
                                    .actions
                                    .iter()
                                    .any(|action| matches!(action, ReplyAction::ToolCall(_))) =>
                        {
                            content.clone()
                        }
                        Finish::Completed | Finish::OutputLimit | Finish::Paused { .. } => {
                            return self.record(reply, reasoning, turn, &message);
                        }
                    };
                    self.write_paused(reply, reasoning, turn, &message)?;
                    // A pause continuation appends the paused reply's blocks,
                    // so it rebuilds the shared conversation once around the
                    // longer copy; the held request keeps the step's own.
                    let mut conversation = request.conversation.to_vec();
                    conversation.extend(blocks.iter().map(|block| Input::Assistant {
                        model: self.model.reference.clone(),
                        text: String::new(),
                        provider_item: Some(block.clone()),
                    }));
                    request.conversation = conversation.into();
                    paused.extend(blocks);
                    if self.turn_cancelled() {
                        return Ok(Step::Ended(crate::ended(TurnOutcome::Interrupted, None)));
                    }
                    if let Some(completed) = self.over_budget() {
                        return Ok(Step::Ended(self.budget_end(completed)));
                    }
                    if continuations >= MAX_CONTINUATIONS {
                        return Ok(Step::Ended(crate::ended(
                            TurnOutcome::Failed,
                            Some(Failure {
                                code: ErrorCode::PausedTooLong,
                                message: format!(
                                    "The reply was still paused (`pause_turn`) after {MAX_CONTINUATIONS} continuations."
                                ),
                                retry_after_ms: None,
                                provider: None,
                            }),
                        )));
                    }
                    continuations += 1;
                }
                Attempted::Failed(failure) if failure.code == ErrorCode::ContextOverflow => {
                    return self.overflowed(turn, Some(failure));
                }
                Attempted::Failed(failure) => {
                    return Ok(Step::Ended(crate::ended(
                        TurnOutcome::Failed,
                        Some(failure),
                    )));
                }
                Attempted::Interrupted => {
                    return Ok(Step::Ended(crate::ended(TurnOutcome::Interrupted, None)));
                }
            };
        }
    }

    /// Calls the model with retries. Each attempt is a new action with its
    /// attempt number; each wait is on the log's clock; the call fails with
    /// the last error once the retries run out. The reply is the caller's to
    /// write.
    pub(crate) fn call_with_retries(
        &mut self,
        request: &ModelRequest,
        turn: &TurnId,
    ) -> Result<Attempted, Error> {
        let mut retries = 0u32;
        loop {
            let message = ActionId(crate::mint("a_"));
            self.append(
                &Event::AssistantMessageStarted(Empty {}),
                turn,
                Some(&message),
            )?;
            // The attempt counts the retries already made: the first call
            // builds at 1, saturating at the top so 0 stays
            // unrepresentable. A later logical request starts over at 1.
            let attempt = std::num::NonZeroU32::MIN.saturating_add(retries);
            let (reply, reasoning) = self.stream(request, turn, &message, attempt)?;
            // A call that ended without a reply writes its usage at once,
            // before anything else the failure or cancel causes
            // (`docs/events.md`, "Usage and notices").
            if let Err(error) = &reply {
                self.write_unfinished(error.usage(), turn, &message)?;
            }
            match reply {
                Ok(reply) => {
                    return Ok(Attempted::Replied {
                        reply,
                        reasoning,
                        message,
                    });
                }
                Err(CallError::Failed {
                    failure,
                    should_retry,
                    ..
                }) => {
                    let decision = self.retry.decide(&failure, should_retry, retries);
                    let attempt = retries.saturating_add(1);
                    let error = match &decision {
                        Decision::Retry(_) => &failure,
                        Decision::Fail(final_failure) => final_failure,
                    };
                    self.append(
                        &Event::AssistantMessageCompleted(AssistantMessageCompleted {
                            outcome: MessageOutcome::Failed,
                            error: Some(error.clone()),
                        }),
                        turn,
                        Some(&message),
                    )?;
                    match decision {
                        Decision::Retry(delay) => {
                            // A cancel that landed during the failing call ends
                            // the turn before any wait: no `retry_scheduled`
                            // follows a retry that never starts.
                            if self.turn_cancelled() {
                                return Ok(Attempted::Interrupted);
                            }
                            // The deadline is fixed before the line is
                            // appended, so an advance after `retry_scheduled`
                            // cannot stretch this wait (`docs/testing.md`,
                            // "Waits and timeouts").
                            let clock = self.log.clock();
                            let now = clock.now();
                            let until = now.checked_add(delay).unwrap_or(now);
                            self.append(
                                &Event::RetryScheduled(RetryScheduled {
                                    code: failure.code.clone(),
                                    attempt: attempt.saturating_add(1),
                                    last_attempt: self.retry.attempts.saturating_add(1),
                                    delay_ms: delay_ms(delay),
                                }),
                                turn,
                                Some(&message),
                            )?;
                            if self.wait_retry(until) {
                                return Ok(Attempted::Interrupted);
                            }
                            retries = retries.saturating_add(1);
                        }
                        Decision::Fail(final_failure) => {
                            return Ok(Attempted::Failed(final_failure));
                        }
                    }
                }
                // An interrupted reply has no `assistant_message_completed`
                // (`docs/architecture.md`, "Cancellation").
                Err(CallError::Cancelled { .. }) => return Ok(Attempted::Interrupted),
            }
        }
    }

    /// Parks the loop thread until `until` on the log's clock, woken by a
    /// clock move and by the turn's cancel. True when the turn was
    /// cancelled. The deadline is anchored when the failure is handled, so
    /// a clock move before the wait starts never shortens it. The wake is
    /// subscribed to the clock and the cancel first, then the deadline and
    /// the cancel are checked, and re-checked after every wake, so a bump
    /// that lands before the park is still seen.
    fn wait_retry(&self, until: std::time::Instant) -> bool {
        let wake = Arc::new(SharedWake::default());
        let keeper: Arc<dyn Wake> = wake.clone();
        let clock = self.log.clock().clone();
        clock.subscribe(Arc::downgrade(&keeper));
        self.cancel.subscribe(Arc::downgrade(&keeper));
        loop {
            if self.turn_cancelled() {
                return true;
            }
            if clock.now() >= until {
                return false;
            }
            wake.park(clock.as_ref(), Some(until));
        }
    }
}

/// A delay as whole milliseconds for `retry_scheduled.delay_ms`, saturating.
fn delay_ms(delay: Duration) -> u64 {
    u64::try_from(delay.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
