//! The session's usage ledger (`docs/events.md`, `usage` and
//! `usage_recorded`). One implementation folds `fiber_exited` and the
//! spending budget, so the two cannot drift.

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError};

use contract::events::{Event, Notice, UsageRecorded};
use contract::provider::{CallUsage, Cost, CostLookup, Tier};
use contract::shapes::{Tokens, Usage};
use contract::{ActionId, TurnId};
use contract::{ErrorCode, GenerationId};

/// The call's cost in US dollars (`docs/model-routing.md`, "Cost").
///
/// The prompt is `input` plus `cache_read` plus every `cache_write`, each
/// sum saturating so a malformed count cannot abort the session. The tier
/// with the highest `input_tokens_above` strictly below that count supplies
/// every price; a count equal to a threshold stays on the tier below it.
/// Two tiers with the same threshold: the first listed wins. Below every
/// threshold, the base prices apply, and a missing cache price is 0.
pub(crate) fn price(cost: &Cost, tokens: &Tokens) -> f64 {
    let written = tokens
        .cache_write
        .values()
        .copied()
        .fold(0, u64::saturating_add);
    let prompt = tokens
        .input
        .saturating_add(tokens.cache_read)
        .saturating_add(written);
    let (input, output, cache_read, cache_write) = match tier(cost, prompt) {
        Some(tier) => (tier.input, tier.output, tier.cache_read, tier.cache_write),
        None => (
            cost.input,
            cost.output,
            cost.cache_read.unwrap_or(0.0),
            cost.cache_write.unwrap_or(0.0),
        ),
    };
    (input * tokens.input as f64
        + cache_read * tokens.cache_read as f64
        + cache_write * written as f64
        + output * tokens.output as f64)
        / 1_000_000.0
}

/// The tier whose threshold is the highest one `prompt` exceeds. Reversing
/// first makes an equal threshold keep the tier listed earlier: `max_by_key`
/// returns the last maximum.
fn tier(cost: &Cost, prompt: u64) -> Option<&Tier> {
    cost.tiers
        .iter()
        .rev()
        .filter(|tier| tier.input_tokens_above < prompt)
        .max_by_key(|tier| tier.input_tokens_above)
}

/// The cost one call is recorded at (`docs/model-routing.md`, "Cost"): the
/// vendor's own figure where the reply reports one, otherwise the model's
/// declared prices applied to the call's tokens, else `None`.
pub(crate) fn call_cost(
    inline: Option<f64>,
    prices: Option<&Cost>,
    tokens: &Tokens,
) -> Option<f64> {
    inline.or_else(|| prices.map(|prices| price(prices, tokens)))
}

/// A call's `usage_recorded`, and whether its cost may be looked up later.
pub(crate) struct Recorded {
    /// The line.
    pub(crate) line: UsageRecorded,
    /// True for a call without the vendor's own figure whose id the provider
    /// named: only such a call is looked up (`docs/model-routing.md`,
    /// "Cost").
    pub(crate) lookable: bool,
}

/// One `usage_recorded` from what a call reported (`docs/events.md`,
/// `usage_recorded`). A partial record carries no vendor figure, so its
/// `cost` is the declared prices applied to its tokens. A call the provider
/// named no generation for, or an empty one, gets an id Fiber mints,
/// starting `fiber-`, once, here: its copies and its fold all carry that id.
/// Such a call's `cost` is `null` and is never looked up, because the vendor
/// never named the generation (`docs/model-routing.md`, "Cost").
pub(crate) fn recorded(
    usage: CallUsage,
    inline_cost: Option<f64>,
    model: &str,
    prices: Option<&Cost>,
    subscription: bool,
) -> Recorded {
    let named = usage.generation_id.filter(|id| !id.0.is_empty());
    let cost = match named {
        Some(_) => call_cost(inline_cost, prices, &usage.tokens),
        None => None,
    };
    let lookable = named.is_some() && inline_cost.is_none();
    let line = UsageRecorded {
        generation_id: named.unwrap_or_else(|| GenerationId(crate::mint("fiber-"))),
        model: model.to_owned(),
        tokens: usage.tokens,
        input_bytes: usage.input_size.bytes,
        input_media: usage.input_size.media.then_some(true),
        web_searches: usage.web_searches,
        cost,
        subscription: subscription.then_some(true),
        extension: None,
        origin_session_id: None,
        reviewer: None,
    };
    Recorded { line, lookable }
}

/// One `usage_recorded` for an extension's `host.model` call: `recorded`
/// with `extension` set, so the loop ledger counts it under the calling
/// extension (`docs/extensions.md`, "Host calls").
pub fn extension_line(
    usage: CallUsage,
    inline_cost: Option<f64>,
    model: &str,
    prices: Option<&Cost>,
    subscription: bool,
    extension: &str,
) -> UsageRecorded {
    let mut recorded = recorded(usage, inline_cost, model, prices, subscription);
    recorded.line.extension = Some(extension.to_owned());
    recorded.line
}

impl crate::Loop {
    /// Writes and ledgers a call's usage that ended without a reply.
    pub(crate) fn write_unfinished(
        &mut self,
        usage: &CallUsage,
        turn: &TurnId,
        message: &ActionId,
    ) -> Result<(), crate::Error> {
        let model = self.model.reference.clone();
        let prices = self.model.cost.clone();
        let subscription = self.model.subscription;
        let recorded = recorded(usage.clone(), None, &model, prices.as_ref(), subscription);
        let lookup = self.provider.cost_lookup();
        self.write_usage(recorded, lookup, Some(turn), Some(message))
    }

    /// Writes and ledgers one call's `usage_recorded`: the one path every
    /// call's usage takes. A `lookable` record whose provider has a lookup
    /// is looked up once, later (`docs/model-routing.md`, "Cost").
    pub(crate) fn write_usage(
        &mut self,
        recorded: Recorded,
        lookup: Option<Arc<dyn CostLookup>>,
        turn: Option<&TurnId>,
        action: Option<&ActionId>,
    ) -> Result<(), crate::Error> {
        let Recorded { line, lookable } = recorded;
        self.put_usage(&line, turn, action)?;
        let Some(lookup) = lookup.filter(|_| lookable) else {
            return Ok(());
        };
        let clock = Arc::clone(self.log.clock());
        let scheduled =
            self.late_cost
                .schedule(lookup, line, turn.cloned(), action.cloned(), &clock);
        if let Err(e) = scheduled {
            self.log.append(
                &Event::Notice(Notice {
                    code: ErrorCode::IoFailed,
                    message: format!(
                        "The cost lookup could not start: {e}. The call's cost stays as first recorded."
                    ),
                    extension: None,
                }),
                turn.cloned(),
                None,
            )?;
        }
        Ok(())
    }

    /// Writes every record whose cost settled late, each in its first
    /// record's turn and action (`docs/events.md`, "Usage and notices").
    pub(crate) fn write_settled(&mut self) -> Result<(), crate::Error> {
        for settled in self.late_cost.take_settled() {
            self.put_usage(
                &settled.record,
                settled.turn.as_ref(),
                settled.action.as_ref(),
            )?;
        }
        Ok(())
    }

    /// At the session's end: stops the lookups, so nothing settles after
    /// this, then writes what already settled. A lookup still waiting is
    /// never made.
    pub(crate) fn write_last_settled(&mut self) -> Result<(), crate::Error> {
        self.late_cost.stop();
        self.write_settled()
    }

    fn put_usage(
        &mut self,
        recorded: &UsageRecorded,
        turn: Option<&TurnId>,
        action: Option<&ActionId>,
    ) -> Result<(), crate::Error> {
        self.write(&Event::UsageRecorded(recorded.clone()), turn, action)?;
        self.ledger.record(recorded);
        self.record_usage(recorded);
        Ok(())
    }

    /// Copies a delegate's `usage_recorded` into this log: sets
    /// `origin_session_id` to the sending delegate when absent, preserves
    /// it when present so a grandchild's line still names the originator,
    /// then writes and ledgers it with no turn and no action. Copies are
    /// never looked up, so they never schedule a late cost. A copy that
    /// claims the one-way trip stops delegates at once, as a tap arrival
    /// does: foreign spend arriving mid-turn or mid-wait is stopped
    /// promptly, while the loop's own spend waits for the request-time
    /// budget end.
    pub(crate) fn copy_usage(
        &mut self,
        _job: &contract::JobId,
        session: &contract::SessionId,
        line: UsageRecorded,
    ) -> Result<(), crate::Error> {
        let mut copied = line;
        if copied.origin_session_id.is_none() {
            copied.origin_session_id = Some(session.clone());
        }
        self.put_usage(&copied, None, None)?;
        self.publish_usage(&copied);
        Ok(())
    }

    /// Records one ledgered line on the spend tripwire without claiming
    /// the trip: the loop's own spend waits for the request-time budget
    /// end, so an over-budget turn stops delegates exactly once there.
    /// Only copies and tap arrivals claim, for a prompt stop while the
    /// loop is parked or mid-turn on foreign spend.
    fn record_usage(&self, recorded: &UsageRecorded) {
        self.spend_trip
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(recorded);
    }

    /// Publishes one ledgered line to the spend tripwire, stopping
    /// delegates at once when it claims the one-way trip. The tripwire
    /// lock is released before the stop, and the stop tolerates ended
    /// jobs, so a trip racing a job end stops nothing twice.
    fn publish_usage(&self, recorded: &UsageRecorded) {
        let tripped = {
            let mut trip = self
                .spend_trip
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            trip.apply(recorded)
        };
        if tripped && let Some(jobs) = &self.ending.jobs {
            jobs.stop_delegates();
        }
    }
}

/// The latest `usage_recorded` per `generation_id`, in first-recorded
/// order, with the fold's running state beside it. A later line with an id
/// already recorded replaces the earlier one in every total, so a copy of
/// a copy is still one call (`docs/events.md`, `usage_recorded`). After
/// any sequence of records the usage equals [`log::usage`] over the held
/// lines exactly: a new call takes one step, and a correction folds the
/// whole ledger again.
#[derive(Debug)]
pub(crate) struct Ledger {
    calls: Vec<UsageRecorded>,
    index: BTreeMap<GenerationId, usize>,
    tokens: Tokens,
    billed: bool,
    cost: Option<f64>,
    subscription_cost: f64,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            calls: Vec::new(),
            index: BTreeMap::new(),
            tokens: Tokens {
                input: 0,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 0,
            },
            billed: false,
            cost: None,
            subscription_cost: 0.0,
        }
    }
}

impl Ledger {
    /// Keeps `line`, replacing any earlier line with its `generation_id`
    /// in place, keeping first-recorded order. True when it replaced one:
    /// `line` corrects a call already counted.
    pub(crate) fn record(&mut self, line: &UsageRecorded) -> bool {
        match self.index.get(&line.generation_id).copied() {
            Some(at) => {
                // An identical correction changes nothing; any other one
                // folds the whole ledger again, so only corrections pay
                // O(n). The index only holds positions of held lines.
                if self.calls.get(at).is_some_and(|held| held != line)
                    && let Some(held) = self.calls.get_mut(at)
                {
                    *held = line.clone();
                    self.recount();
                }
                true
            }
            None => {
                self.index
                    .insert(line.generation_id.clone(), self.calls.len());
                self.calls.push(line.clone());
                self.add(line);
                false
            }
        }
    }

    /// One step of the fold [`log::usage`] runs: what a new call adds.
    fn add(&mut self, call: &UsageRecorded) {
        self.tokens.input = self.tokens.input.saturating_add(call.tokens.input);
        self.tokens.cache_read = self
            .tokens
            .cache_read
            .saturating_add(call.tokens.cache_read);
        self.tokens.output = self.tokens.output.saturating_add(call.tokens.output);
        for (lifetime, count) in &call.tokens.cache_write {
            let total = self.tokens.cache_write.entry(lifetime.clone()).or_default();
            *total = total.saturating_add(*count);
        }
        if call.subscription == Some(true) {
            self.subscription_cost += call.cost.unwrap_or(0.0);
        } else {
            self.billed = true;
            if let Some(known) = call.cost {
                *self.cost.get_or_insert(0.0) += known;
            }
        }
    }

    /// Folds the held lines again from scratch.
    fn recount(&mut self) {
        let calls = std::mem::take(&mut self.calls);
        self.tokens = Tokens {
            input: 0,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 0,
        };
        self.billed = false;
        self.cost = None;
        self.subscription_cost = 0.0;
        for call in &calls {
            self.add(call);
        }
        self.calls = calls;
    }

    /// The docs' `usage` shape over the lines kept.
    pub(crate) fn usage(&self) -> Usage {
        Usage {
            tokens: self.tokens.clone(),
            // `docs/events.md`, `usage`: 0 with no billed call, null when
            // billed calls exist and none had a known cost.
            cost: if self.billed { self.cost } else { Some(0.0) },
            subscription_cost: self.subscription_cost,
        }
    }

    /// US dollars billed per token. A `cost` still null counts as zero
    /// (`docs/loop.md`, "Spending budget"). Subscription calls are absent
    /// from the sum.
    pub(crate) fn spend(&self) -> f64 {
        self.usage().cost.unwrap_or(0.0)
    }

    /// The held lines, in first-recorded order: the tripwire starts from
    /// these, so a resume keeps its restored spend.
    pub(crate) fn lines(&self) -> &[UsageRecorded] {
        &self.calls
    }
}

/// The spend the loop has seen, shared with the delegate-socket reader:
/// one leaf mutex protecting the per-generation billed-cost map, the
/// accumulated billed spend, the optional budget and the one-way tripped
/// flag. Every access holds it; the stop and the inbox send run after it
/// is released, so the watcher never holds it across a stop or a send.
#[derive(Debug)]
pub(crate) struct Tripwire {
    billed: BTreeMap<GenerationId, f64>,
    sum: f64,
    budget: Option<f64>,
    tripped: bool,
}

impl Tripwire {
    /// The billed dollars one line counts: zero for subscription lines
    /// and for lines with no known cost.
    pub(crate) fn billed_cost(line: &UsageRecorded) -> f64 {
        if line.subscription == Some(true) {
            0.0
        } else {
            line.cost.unwrap_or(0.0)
        }
    }

    /// Starts from the ledger's held lines and the session budget.
    pub(crate) fn from_ledger(ledger: &Ledger, budget: Option<f64>) -> Self {
        let mut trip = Self {
            billed: BTreeMap::new(),
            sum: 0.0,
            budget,
            tripped: false,
        };
        for line in ledger.lines() {
            let cost = Self::billed_cost(line);
            trip.billed.insert(line.generation_id.clone(), cost);
            trip.sum += cost;
        }
        trip
    }

    /// Folds one line's delta (`billed(new) - billed(old)`), so a
    /// correction adds only its difference and a re-delivery adds
    /// nothing.
    fn fold(&mut self, line: &UsageRecorded) {
        let new = Self::billed_cost(line);
        let old = self.billed.get(&line.generation_id).copied().unwrap_or(0.0);
        let known = self.billed.contains_key(&line.generation_id);
        if !known || new != old {
            self.billed.insert(line.generation_id.clone(), new);
            self.sum += new - old;
        }
    }

    /// Records one ledgered line on the spend tripwire without claiming
    /// the trip: loop-side publication only reconciles the sum, leaving
    /// the claim to tap arrivals, copies and the request-time budget end.
    pub(crate) fn record(&mut self, line: &UsageRecorded) {
        self.fold(line);
    }

    /// Applies one line's delta, true when this call claims the one-way
    /// trip: billed spend reached the budget. A downward correction never
    /// untrips.
    pub(crate) fn apply(&mut self, line: &UsageRecorded) -> bool {
        self.fold(line);
        if self.tripped {
            return false;
        }
        if let Some(limit) = self.budget
            && self.sum >= limit
        {
            self.tripped = true;
            return true;
        }
        false
    }

    /// Publishes one restored ledger line the map lacks, preserving a
    /// watcher contribution not yet folded: a ledger snapshot older than a
    /// racing watcher update never overwrites it. Recording only, like
    /// every loop-side publication.
    pub(crate) fn publish_missing(&mut self, line: &UsageRecorded) {
        if self.billed.contains_key(&line.generation_id) {
            return;
        }
        let cost = Self::billed_cost(line);
        self.billed.insert(line.generation_id.clone(), cost);
        self.sum += cost;
    }

    /// Publishes the session budget. Recording only: the trip is
    /// claimed by spend arrivals and by the request-time budget end,
    /// so attaching a budget never stops delegates on its own.
    pub(crate) fn set_budget(&mut self, budget: Option<f64>) {
        self.budget = budget;
    }
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
