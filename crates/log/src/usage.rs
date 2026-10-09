//! Folds model-call usage into the session's `usage` shape (`docs/events.md`,
//! `usage` and `usage_recorded`).

use std::collections::BTreeMap;

use contract::events::UsageRecorded;
use contract::shapes::{Tokens, Usage};

/// The `usage` shape over the supplied model calls.
pub fn usage<'a>(calls: impl IntoIterator<Item = &'a UsageRecorded>) -> Usage {
    let mut tokens = Tokens {
        input: 0,
        cache_read: 0,
        cache_write: BTreeMap::new(),
        output: 0,
    };
    let mut billed = false;
    let mut cost = None;
    let mut subscription_cost = 0.0;
    for call in calls {
        tokens.input = tokens.input.saturating_add(call.tokens.input);
        tokens.cache_read = tokens.cache_read.saturating_add(call.tokens.cache_read);
        tokens.output = tokens.output.saturating_add(call.tokens.output);
        for (lifetime, count) in &call.tokens.cache_write {
            let total = tokens.cache_write.entry(lifetime.clone()).or_default();
            *total = total.saturating_add(*count);
        }
        if call.subscription == Some(true) {
            subscription_cost += call.cost.unwrap_or(0.0);
        } else {
            billed = true;
            if let Some(known) = call.cost {
                *cost.get_or_insert(0.0) += known;
            }
        }
    }
    Usage {
        tokens,
        // `docs/events.md`, `usage`: 0 with no billed call, null when billed
        // calls exist and none had a known cost.
        cost: if billed { cost } else { Some(0.0) },
        subscription_cost,
    }
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
