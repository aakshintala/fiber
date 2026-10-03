use std::collections::BTreeMap;

use serde_json::json;

use super::*;
use crate::events::{ReasoningCompleted, TextCompleted, ToolCallRequested};
use crate::shapes::Tokens;
use crate::{GenerationId, ProviderCallId};

fn reply(actions: Vec<ReplyAction>) -> Reply {
    Reply {
        actions,
        finish: Finish::Completed,
        generation_id: GenerationId("g".into()),
        tokens: Tokens {
            input: 0,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 0,
        },
    }
}

#[test]
fn text_joins_text_parts_in_order_and_skips_the_rest() {
    let mixed = reply(vec![
        ReplyAction::Text(TextCompleted {
            text: "A".into(),
            provider_item: None,
        }),
        ReplyAction::Reasoning(ReasoningCompleted {
            text: "think".into(),
            provider_item: None,
        }),
        ReplyAction::ToolCall(ToolCallRequested {
            name: "get_weather".into(),
            arguments: json!({}),
            provider_id: Some(ProviderCallId("c".into())),
            repair: None,
        }),
        ReplyAction::Text(TextCompleted {
            text: "B".into(),
            provider_item: None,
        }),
    ]);
    assert_eq!(mixed.text(), "AB");

    let none = reply(vec![
        ReplyAction::Reasoning(ReasoningCompleted {
            text: "think".into(),
            provider_item: None,
        }),
        ReplyAction::ToolCall(ToolCallRequested {
            name: "get_weather".into(),
            arguments: json!({}),
            provider_id: None,
            repair: None,
        }),
    ]);
    assert_eq!(none.text(), "");
}

fn tokens(input: u64, cache_read: u64, cache_write: &[(&str, u64)], output: u64) -> Tokens {
    Tokens {
        input,
        cache_read,
        cache_write: cache_write
            .iter()
            .map(|(lifetime, n)| ((*lifetime).to_owned(), *n))
            .collect(),
        output,
    }
}

fn dollars(cost: &Cost, counted: &Tokens) -> f64 {
    cost.price(counted)
}

/// Base prices, and two tiers listed with the higher threshold first.
fn tiered() -> Cost {
    Cost {
        input: 2.0,
        output: 10.0,
        cache_read: Some(0.2),
        cache_write: Some(1.0),
        tiers: vec![
            Tier {
                input_tokens_above: 272_000,
                input: 4.0,
                output: 15.0,
                cache_read: 0.4,
                cache_write: 0.0,
            },
            Tier {
                input_tokens_above: 100_000,
                input: 3.0,
                output: 12.0,
                cache_read: 0.3,
                cache_write: 0.5,
            },
        ],
    }
}

#[test]
fn each_kind_is_priced_at_its_own_price_per_million() {
    let cost = Cost {
        input: 2.0,
        output: 10.0,
        cache_read: Some(0.2),
        cache_write: Some(2.5),
        tiers: Vec::new(),
    };
    let counted = tokens(
        1_000_000,
        2_000_000,
        &[("5m", 100_000), ("1h", 300_000)],
        500_000,
    );
    // 2 + 0.4 + (400_000 * 2.5) / 1e6 + 5
    assert_eq!(dollars(&cost, &counted), 8.4);
}

#[test]
fn missing_cache_prices_are_zero() {
    let cost = Cost {
        input: 3.0,
        output: 15.0,
        cache_read: None,
        cache_write: None,
        tiers: Vec::new(),
    };
    let counted = tokens(1_000_000, 1_000_000, &[("5m", 1_000_000)], 1_000_000);
    assert_eq!(dollars(&cost, &counted), 18.0);
}

#[test]
fn the_highest_exceeded_tier_prices_the_whole_call() {
    let cost = tiered();
    // Below every threshold: the base prices.
    assert_eq!(
        dollars(&cost, &tokens(99_999, 0, &[], 0)),
        99_999.0 * 2.0 / 1_000_000.0
    );
    // Equal to a threshold stays on the tier below it.
    assert_eq!(
        dollars(&cost, &tokens(100_000, 0, &[], 0)),
        100_000.0 * 2.0 / 1_000_000.0
    );
    assert_eq!(
        dollars(&cost, &tokens(100_001, 0, &[], 1_000)),
        (100_001.0 * 3.0 + 1_000.0 * 12.0) / 1_000_000.0
    );
    assert_eq!(
        dollars(&cost, &tokens(272_000, 0, &[], 0)),
        272_000.0 * 3.0 / 1_000_000.0
    );
    // The higher threshold is listed first; the highest exceeded still wins.
    let above = tokens(272_001, 0, &[], 1_000);
    assert_eq!(
        dollars(&cost, &above),
        (272_001.0 * 4.0 + 1_000.0 * 15.0) / 1_000_000.0
    );
    // Output tokens do not move the threshold.
    assert_eq!(
        dollars(&cost, &tokens(50, 0, &[], 1_000_000)),
        (50.0 * 2.0 + 1_000_000.0 * 10.0) / 1_000_000.0
    );
}

#[test]
fn cache_tokens_count_toward_the_tier_threshold() {
    let cost = Cost {
        input: 2.0,
        output: 10.0,
        cache_read: Some(0.2),
        cache_write: Some(1.0),
        tiers: vec![Tier {
            input_tokens_above: 100,
            input: 8.0,
            output: 20.0,
            cache_read: 1.0,
            cache_write: 4.0,
        }],
    };
    // Cache reads alone push the whole prompt over the threshold.
    assert_eq!(
        dollars(&cost, &tokens(0, 101, &[], 0)),
        101.0 * 1.0 / 1_000_000.0
    );
    // Cache writes of every lifetime count, and are priced together.
    assert_eq!(
        dollars(&cost, &tokens(40, 30, &[("5m", 20), ("1h", 11)], 0)),
        (40.0 * 8.0 + 30.0 * 1.0 + 31.0 * 4.0) / 1_000_000.0
    );
    // One short of the threshold, cache writes included, stays on the base.
    assert_eq!(
        dollars(&cost, &tokens(40, 30, &[("5m", 20), ("1h", 10)], 0)),
        (40.0 * 2.0 + 30.0 * 0.2 + 30.0 * 1.0) / 1_000_000.0
    );
}

#[test]
fn a_model_with_no_tiers_uses_its_base_prices() {
    let cost = Cost {
        input: 2.0,
        output: 10.0,
        cache_read: Some(0.2),
        cache_write: None,
        tiers: Vec::new(),
    };
    assert_eq!(
        dollars(&cost, &tokens(272_001, 10, &[("5m", 10)], 1_000)),
        (272_001.0 * 2.0 + 10.0 * 0.2 + 1_000.0 * 10.0) / 1_000_000.0
    );
}

#[test]
fn two_tiers_with_the_same_threshold_use_the_first() {
    let cost = Cost {
        input: 1.0,
        output: 1.0,
        cache_read: None,
        cache_write: None,
        tiers: vec![
            Tier {
                input_tokens_above: 10,
                input: 5.0,
                output: 5.0,
                cache_read: 5.0,
                cache_write: 5.0,
            },
            Tier {
                input_tokens_above: 10,
                input: 9.0,
                output: 9.0,
                cache_read: 9.0,
                cache_write: 9.0,
            },
        ],
    };
    assert_eq!(
        dollars(&cost, &tokens(11, 0, &[], 0)),
        11.0 * 5.0 / 1_000_000.0
    );
}
