//! Tests for `usage`, the session's totals over model calls.

use super::usage;
use contract::GenerationId;
use contract::events::UsageRecorded;
use contract::shapes::{Tokens, Usage};

fn call(
    generation: &str,
    tokens: Tokens,
    cost: Option<f64>,
    subscription: bool,
) -> UsageRecorded {
    UsageRecorded {
        generation_id: GenerationId(generation.to_owned()),
        model: "provider/model".to_owned(),
        tokens,
        input_bytes: 0,
        input_media: None,
        web_searches: None,
        cost,
        subscription: subscription.then_some(true),
        extension: None,
        origin_session_id: None,
    }
}

fn tokens(input: u64, cache_read: u64, cache_write: &[(&str, u64)], output: u64) -> Tokens {
    Tokens {
        input,
        cache_read,
        cache_write: cache_write
            .iter()
            .map(|(lifetime, count)| ((*lifetime).to_owned(), *count))
            .collect(),
        output,
    }
}

fn empty_tokens() -> Tokens {
    tokens(0, 0, &[], 0)
}

fn expected(tokens: Tokens, cost: Option<f64>, subscription_cost: f64) -> Usage {
    Usage {
        tokens,
        cost,
        subscription_cost,
    }
}

#[test]
fn no_calls_have_zero_costs() {
    assert_eq!(usage([]), expected(empty_tokens(), Some(0.0), 0.0));
}

#[test]
fn a_billed_call_with_unknown_cost_has_null_cost() {
    let calls = [call("g1", empty_tokens(), None, false)];
    assert_eq!(usage(calls.iter()), expected(empty_tokens(), None, 0.0));
}

#[test]
fn a_known_billed_cost_survives_an_unknown_billed_cost() {
    let calls = [
        call("g1", empty_tokens(), Some(0.25), false),
        call("g2", empty_tokens(), None, false),
    ];
    assert_eq!(usage(calls.iter()), expected(empty_tokens(), Some(0.25), 0.0));
}

#[test]
fn subscription_calls_do_not_count_as_billed() {
    let calls = [
        call("g1", empty_tokens(), Some(0.5), true),
        call("g2", empty_tokens(), None, true),
    ];
    assert_eq!(usage(calls.iter()), expected(empty_tokens(), Some(0.0), 0.5));
}

#[test]
fn billed_and_subscription_costs_are_separate() {
    let calls = [
        call("g1", empty_tokens(), Some(0.25), false),
        call("g2", empty_tokens(), Some(0.5), true),
    ];
    assert_eq!(usage(calls.iter()), expected(empty_tokens(), Some(0.25), 0.5));
}

#[test]
fn cache_writes_sum_by_lifetime() {
    let calls = [
        call("g1", tokens(0, 0, &[("5m", 2), ("1h", 7)], 0), None, false),
        call("g2", tokens(0, 0, &[("5m", 3), ("1h", 11)], 0), None, false),
    ];
    assert_eq!(
        usage(calls.iter()),
        expected(
            tokens(0, 0, &[("5m", 5), ("1h", 18)], 0),
            None,
            0.0
        )
    );
}

#[test]
fn token_sums_saturate() {
    let calls = [
        call(
            "g1",
            tokens(u64::MAX - 1, u64::MAX - 1, &[("5m", u64::MAX - 1)], u64::MAX - 1),
            None,
            false,
        ),
        call("g2", tokens(3, 3, &[("5m", 3)], 3), None, false),
    ];
    assert_eq!(
        usage(calls.iter()),
        expected(
            tokens(u64::MAX, u64::MAX, &[("5m", u64::MAX)], u64::MAX),
            None,
            0.0
        )
    );
}
