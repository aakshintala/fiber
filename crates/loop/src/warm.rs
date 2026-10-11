//! Warming an idle session's prompt cache (`docs/prompt-cache.md`,
//! "Warming while idle"): in the wait between turns, the last step's
//! request is resent with a one-token output cap shortly before the cache
//! lifetime ends, until `cache.warm_cap` lifetimes after the last turn.
//! [`Warming`] owns that state: the cap, the held request and its send,
//! and when a switch stopped warming.

use std::time::{Duration, Instant};

use contract::events::{CacheLifetime, Event, Notice};
use contract::provider::{CallError, ModelRequest};

use crate::{Error, Loop};

/// How long before the cache lifetime ends a refresh is sent (`picked`:
/// one request's latency).
const MARGIN: Duration = Duration::from_secs(30);

/// An idle wait's warming state: the cap, the held request and when a
/// switch stopped warming (`docs/prompt-cache.md`, "Warming while idle").
#[derive(Debug, Default)]
pub(crate) struct Warming {
    /// `cache.warm_cap`: how many cache lifetimes after the last turn an
    /// idle wait keeps the cache warm. `None` never warms.
    cap: Option<u32>,
    /// The last step's request and when it was handed to the provider, or
    /// the last refresh's send: what a refresh resends and counts from.
    /// Kept only while warming is on.
    last: Option<(ModelRequest, Instant)>,
    /// When a switch cleared the last request while warming.
    stopped: Option<Instant>,
}

impl Warming {
    /// Sets `cache.warm_cap`. The held request and the stop stamp do not
    /// change.
    pub(crate) fn set_cap(&mut self, cap: Option<u32>) {
        self.cap = cap;
    }

    /// Holds a clone of `request`, sent at `now`, only when warming is on;
    /// otherwise it does nothing.
    pub(crate) fn record(&mut self, request: &ModelRequest, now: Instant) {
        if self.cap.is_some() {
            self.last = Some((request.clone(), now));
        }
    }

    /// A model switch. When a request is held, it drops it and records
    /// `now` as the stop. With nothing held it does nothing.
    pub(crate) fn stop(&mut self, now: Instant) {
        if self.last.take().is_some() {
            self.stopped = Some(now);
        }
    }

    /// The switch's stop instant, taken once.
    pub(crate) fn take_stopped(&mut self) -> Option<Instant> {
        self.stopped.take()
    }

    /// The held request, when warming is on and one is held.
    pub(crate) fn held(&self) -> Option<&ModelRequest> {
        self.cap?;
        self.last.as_ref().map(|(request, _)| request)
    }

    /// When warming stops for an idle wait that began at `start`: `cap`
    /// lifetimes later. `None` when warming is off, nothing is held, or
    /// the arithmetic overflows.
    pub(crate) fn stop_at(&self, start: Instant, lifetime: Duration) -> Option<Instant> {
        let cap = self.cap?;
        self.last.as_ref()?;
        start.checked_add(lifetime.checked_mul(cap)?)
    }

    /// When the next refresh is due: `MARGIN` before the lifetime counted
    /// from the last send ends. `None` when it would not come before
    /// `stop`, so warming is over.
    pub(crate) fn due(&self, stop: Instant, lifetime: Duration) -> Option<Instant> {
        let (_, sent) = self.last.as_ref()?;
        let due = sent.checked_add(lifetime.saturating_sub(MARGIN))?;
        (due < stop).then_some(due)
    }

    /// The held request with its output capped at one token. `None` when
    /// nothing is held, when `now >= send + lifetime` (or the sum
    /// overflows), or when `now >= stop`.
    pub(crate) fn resend(
        &self,
        now: Instant,
        stop: Instant,
        lifetime: Duration,
    ) -> Option<ModelRequest> {
        let (request, sent) = self.last.as_ref()?;
        let end = sent.checked_add(lifetime)?;
        if now >= end || now >= stop {
            return None;
        }
        let mut request = request.clone();
        request.max_output_tokens = Some(1);
        Some(request)
    }

    /// Moves the held request's send to `now`. The stored request does not
    /// change.
    pub(crate) fn sent(&mut self, now: Instant) {
        if let Some((_, sent)) = self.last.as_mut() {
            *sent = now;
        }
    }
}

/// How a refresh that came due ended.
pub(crate) enum Refreshed {
    /// The refresh was sent and recorded; warming goes on.
    Sent,
    /// Warming stopped at this instant: the idle clock starts here.
    Stopped(Instant),
}

impl Loop {
    /// Keeps an idle session's cache warm for `cap` cache lifetimes after
    /// the last turn (`cache.warm_idle` and `cache.warm_cap`). `None` never
    /// warms, which is the default: `fiber ask` and a delegate pass it.
    pub fn warm(mut self, cap: Option<u32>) -> Self {
        self.warming.set_cap(cap);
        self
    }

    /// The preamble's cache lifetime. `None` before the preamble is built.
    fn lifetime(&self) -> Option<Duration> {
        self.preamble
            .as_ref()
            .map(|preamble| match preamble.cache_lifetime {
                CacheLifetime::FiveMinutes => Duration::from_secs(5 * 60),
                CacheLifetime::OneHour => Duration::from_secs(60 * 60),
            })
    }

    /// When warming stops for an idle wait that began at `start`: `cap`
    /// lifetimes later. `None` when this wait does not warm: warming is
    /// off, or no request was sent to resend.
    pub(crate) fn warm_stop(&self, start: Instant) -> Option<Instant> {
        let request = self.warming.held()?;
        if !self.provider.warms(request) {
            return None;
        }
        self.warming.stop_at(start, self.lifetime()?)
    }

    /// When the next refresh is due: `MARGIN` before the lifetime counted
    /// from the last send ends. `None` when it would not come before
    /// `stop`, so warming is over.
    pub(crate) fn warm_due(&self, stop: Instant) -> Option<Instant> {
        self.warming.due(stop, self.lifetime()?)
    }

    /// Sends the refresh that came due: the last request with its output
    /// capped at one token. None is sent once the cache has expired, the
    /// wait has reached `stop`, or the spend has reached `budget.usd`. No
    /// hook runs on it, and it writes only its `usage_recorded`, in no turn
    /// and no action. A failure is not retried: it is a `notice`, and
    /// warming stops.
    pub(crate) fn refresh(&mut self, stop: Instant) -> Result<Refreshed, Error> {
        let clock = std::sync::Arc::clone(self.log.clock());
        let now = clock.now();
        // A spent budget stops warming before anything is resent
        // (`docs/prompt-cache.md`, "Warming while idle").
        if self
            .budget
            .is_some_and(|limit| self.ledger.spend() >= limit)
        {
            return Ok(Refreshed::Stopped(now));
        }
        let Some(lifetime) = self.lifetime() else {
            return Ok(Refreshed::Stopped(now));
        };
        // A cache that expired while a job ran pays a rebuild, not a read.
        let Some(request) = self.warming.resend(now, stop, lifetime) else {
            return Ok(Refreshed::Stopped(now));
        };
        let call = self.provider.call(&request);
        let reply = crate::cancel::run_cancellable(&self.cancel, call, &mut |_| {});
        // A refresh writes its usage at once, however it ended, before the
        // notice a failure also writes (`docs/events.md`, "Usage and
        // notices").
        let (usage, inline) = match &reply {
            Ok(reply) => (reply.usage(), reply.cost),
            Err(error) => (error.usage().clone(), None),
        };
        let model = &self.model;
        let recorded = crate::usage::recorded(
            usage,
            inline,
            &model.reference,
            model.cost.as_ref(),
            model.subscription,
        );
        let lookup = self.provider.cost_lookup();
        self.write_usage(recorded, lookup, None, None)?;
        match reply {
            Ok(_) => {
                // The stored request stays the step's own; only the send
                // the next refresh counts from moves.
                self.warming.sent(now);
                Ok(Refreshed::Sent)
            }
            Err(CallError::Failed { failure, .. }) => {
                self.log.append(
                    &Event::Notice(Notice {
                        code: failure.code,
                        message: format!(
                            "Keeping the prompt cache warm failed: {}",
                            failure.message
                        ),
                        extension: None,
                    }),
                    None,
                    None,
                )?;
                Ok(Refreshed::Stopped(clock.now()))
            }
            Err(CallError::Cancelled { .. }) => Ok(Refreshed::Stopped(clock.now())),
        }
    }
}

#[cfg(test)]
#[path = "warm_tests.rs"]
mod tests;
