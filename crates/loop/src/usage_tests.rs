use contract::GenerationId;
use contract::events::UsageRecorded;
use contract::provider::{Cost, Tier};
use contract::shapes::Tokens;

use super::{Ledger, price};

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
    assert_eq!(price(&cost, &counted), 8.4);
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
    assert_eq!(price(&cost, &counted), 18.0);
}

#[test]
fn the_highest_exceeded_tier_prices_the_whole_call() {
    let cost = tiered();
    // Below every threshold: the base prices.
    assert_eq!(
        price(&cost, &tokens(99_999, 0, &[], 0)),
        99_999.0 * 2.0 / 1_000_000.0
    );
    // Equal to a threshold stays on the tier below it.
    assert_eq!(
        price(&cost, &tokens(100_000, 0, &[], 0)),
        100_000.0 * 2.0 / 1_000_000.0
    );
    assert_eq!(
        price(&cost, &tokens(100_001, 0, &[], 1_000)),
        (100_001.0 * 3.0 + 1_000.0 * 12.0) / 1_000_000.0
    );
    assert_eq!(
        price(&cost, &tokens(272_000, 0, &[], 0)),
        272_000.0 * 3.0 / 1_000_000.0
    );
    // The higher threshold is listed first; the highest exceeded still wins.
    let above = tokens(272_001, 0, &[], 1_000);
    assert_eq!(
        price(&cost, &above),
        (272_001.0 * 4.0 + 1_000.0 * 15.0) / 1_000_000.0
    );
    // Output tokens do not move the threshold.
    assert_eq!(
        price(&cost, &tokens(50, 0, &[], 1_000_000)),
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
        price(&cost, &tokens(0, 101, &[], 0)),
        101.0 * 1.0 / 1_000_000.0
    );
    // Cache writes of every lifetime count, and are priced together.
    assert_eq!(
        price(&cost, &tokens(40, 30, &[("5m", 20), ("1h", 11)], 0)),
        (40.0 * 8.0 + 30.0 * 1.0 + 31.0 * 4.0) / 1_000_000.0
    );
    // One short of the threshold, cache writes included, stays on the base.
    assert_eq!(
        price(&cost, &tokens(40, 30, &[("5m", 20), ("1h", 10)], 0)),
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
        price(&cost, &tokens(272_001, 10, &[("5m", 10)], 1_000)),
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
        price(&cost, &tokens(11, 0, &[], 0)),
        11.0 * 5.0 / 1_000_000.0
    );
}

#[test]
fn an_overflowing_prompt_prices_the_highest_tier() {
    let cost = Cost {
        input: 2.0,
        output: 0.0,
        cache_read: Some(3.0),
        cache_write: Some(5.0),
        tiers: vec![
            Tier {
                input_tokens_above: 100,
                input: 4.0,
                output: 0.0,
                cache_read: 4.0,
                cache_write: 4.0,
            },
            Tier {
                input_tokens_above: u64::MAX - 1,
                input: 8.0,
                output: 0.0,
                cache_read: 7.0,
                cache_write: 6.0,
            },
        ],
    };
    // `u64::MAX` input plus one cache read does not panic, and the saturated
    // prompt still exceeds the highest threshold.
    assert_eq!(
        price(&cost, &tokens(u64::MAX, 1, &[], 0)),
        (u64::MAX as f64 * 8.0 + 7.0) / 1_000_000.0
    );
    // Cache writes that do not fit in `u64` saturate and take that same tier.
    assert_eq!(
        price(&cost, &tokens(0, 0, &[("5m", u64::MAX), ("1h", 1)], 0)),
        u64::MAX as f64 * 6.0 / 1_000_000.0
    );
}

fn recorded(id: &str, input: u64, cache_read: u64, written: u64, output: u64) -> UsageRecorded {
    UsageRecorded {
        generation_id: GenerationId(id.into()),
        model: "fake/m".into(),
        tokens: tokens(input, cache_read, &[("1h", written)], output),
        web_searches: None,
        cost: Some(1.0),
        subscription: None,
        extension: None,
        origin_session_id: None,
    }
}

#[test]
fn usage_totals_saturate_instead_of_overflowing() {
    let mut ledger = Ledger::default();
    ledger.record(&recorded("g1", u64::MAX, u64::MAX, u64::MAX, u64::MAX));
    ledger.record(&recorded("g2", 1, 1, 1, 1));

    let usage = ledger.usage();
    assert_eq!(usage.tokens.input, u64::MAX);
    assert_eq!(usage.tokens.cache_read, u64::MAX);
    assert_eq!(usage.tokens.output, u64::MAX);
    assert_eq!(usage.tokens.cache_write["1h"], u64::MAX);
}
