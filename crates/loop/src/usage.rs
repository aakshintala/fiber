//! The session's usage ledger (`docs/events.md`, `usage` and
//! `usage_recorded`). One implementation folds `fiber_exited` and the
//! spending budget, so the two cannot drift.

use std::collections::BTreeMap;

use contract::GenerationId;
use contract::events::{Event, UsageRecorded};
use contract::provider::{CallUsage, Cost, Tier};
use contract::shapes::{Tokens, Usage};
use contract::{ActionId, TurnId};

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

/// One `usage_recorded` from what a call reported (`docs/events.md`,
/// `usage_recorded`). A partial record carries no vendor figure, so its
/// `cost` is the declared prices applied to its tokens.
pub(crate) fn recorded(
    usage: CallUsage,
    inline_cost: Option<f64>,
    model: &str,
    prices: Option<&Cost>,
    subscription: bool,
) -> UsageRecorded {
    UsageRecorded {
        generation_id: usage.generation_id,
        model: model.to_owned(),
        tokens: usage.tokens.clone(),
        input_bytes: usage.input_size.bytes,
        input_media: usage.input_size.media.then_some(true),
        web_searches: usage.web_searches,
        cost: call_cost(inline_cost, prices, &usage.tokens),
        subscription: subscription.then_some(true),
        extension: None,
        origin_session_id: None,
    }
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
        self.append(&Event::UsageRecorded(recorded.clone()), turn, Some(message))?;
        self.ledger.record(&recorded);
        Ok(())
    }
}

/// The latest `usage_recorded` per `generation_id`. A later line with an id
/// already recorded replaces the earlier one in every total, so a copy of a
/// copy is still one call (`docs/events.md`, `usage_recorded`).
#[derive(Debug, Default)]
pub(crate) struct Ledger {
    calls: BTreeMap<GenerationId, UsageRecorded>,
}

impl Ledger {
    /// Keeps `line`, replacing any earlier line with its `generation_id`.
    pub(crate) fn record(&mut self, line: &UsageRecorded) {
        self.calls.insert(line.generation_id.clone(), line.clone());
    }

    /// The docs' `usage` shape over the lines kept.
    pub(crate) fn usage(&self) -> Usage {
        let mut tokens = Tokens {
            input: 0,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 0,
        };
        let mut billed = 0usize;
        let mut cost = None;
        let mut subscription_cost = 0.0;
        for call in self.calls.values() {
            tokens.input = tokens.input.saturating_add(call.tokens.input);
            tokens.cache_read = tokens.cache_read.saturating_add(call.tokens.cache_read);
            tokens.output = tokens.output.saturating_add(call.tokens.output);
            for (lifetime, n) in &call.tokens.cache_write {
                let total = tokens.cache_write.entry(lifetime.clone()).or_default();
                *total = total.saturating_add(*n);
            }
            if call.subscription == Some(true) {
                subscription_cost += call.cost.unwrap_or(0.0);
            } else {
                billed += 1;
                if let Some(known) = call.cost {
                    *cost.get_or_insert(0.0) += known;
                }
            }
        }
        Usage {
            tokens,
            // `docs/events.md`, `usage`: 0 with no billed call, null when
            // billed calls exist and none had a known cost.
            cost: if billed == 0 { Some(0.0) } else { cost },
            subscription_cost,
        }
    }

    /// US dollars billed per token. A `cost` still null counts as zero
    /// (`docs/loop.md`, "Spending budget"). Subscription calls are absent
    /// from the sum.
    pub(crate) fn spend(&self) -> f64 {
        self.usage().cost.unwrap_or(0.0)
    }
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
