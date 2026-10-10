//! Warming an idle session's prompt cache (`docs/prompt-cache.md`,
//! "Warming while idle"): in the wait between turns, the last step's
//! request is resent with a one-token output cap shortly before the cache
//! lifetime ends, until `cache.warm_cap` lifetimes after the last turn.

use std::time::{Duration, Instant};

use contract::events::{CacheLifetime, Event, Notice};
use contract::provider::CallError;

use crate::{Error, Loop};

/// How long before the cache lifetime ends a refresh is sent (`picked`:
/// one request's latency).
const MARGIN: Duration = Duration::from_secs(30);

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
        self.warm = cap;
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
        let cap = self.warm?;
        let (request, _) = self.last_request.as_ref()?;
        if !self.provider.warms(request) {
            return None;
        }
        start.checked_add(self.lifetime()?.checked_mul(cap)?)
    }

    /// When the next refresh is due: `MARGIN` before the lifetime counted
    /// from the last send ends. `None` when it would not come before
    /// `stop`, so warming is over.
    pub(crate) fn warm_due(&self, stop: Instant) -> Option<Instant> {
        let (_, sent) = self.last_request.as_ref()?;
        let due = sent.checked_add(self.lifetime()?.saturating_sub(MARGIN))?;
        (due < stop).then_some(due)
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
        let (Some((request, sent)), Some(lifetime)) = (&self.last_request, self.lifetime()) else {
            return Ok(Refreshed::Stopped(now));
        };
        // A cache that expired while a job ran pays a rebuild, not a read.
        let expired = sent.checked_add(lifetime).is_none_or(|end| now >= end);
        let spent = self
            .budget
            .is_some_and(|limit| self.ledger.spend() >= limit);
        if expired || now >= stop || spent {
            return Ok(Refreshed::Stopped(now));
        }
        let mut request = request.clone();
        request.max_output_tokens = Some(1);
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
                if let Some((_, sent)) = &mut self.last_request {
                    *sent = now;
                }
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
