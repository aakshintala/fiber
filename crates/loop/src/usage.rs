//! The session's usage ledger (`docs/events.md`, `usage` and
//! `usage_recorded`). One implementation folds `fiber_exited` and the
//! spending budget, so the two cannot drift.

use std::collections::BTreeMap;

use contract::GenerationId;
use contract::events::UsageRecorded;
use contract::shapes::{Tokens, Usage};

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
            tokens.input += call.tokens.input;
            tokens.cache_read += call.tokens.cache_read;
            tokens.output += call.tokens.output;
            for (lifetime, n) in &call.tokens.cache_write {
                *tokens.cache_write.entry(lifetime.clone()).or_default() += n;
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
