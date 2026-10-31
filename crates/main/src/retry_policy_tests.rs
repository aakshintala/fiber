//! `retry.attempts` from configuration reaches the loop's retry policy.

use super::retry_policy;
use crate::test_support;

fn config(overrides: Vec<String>) -> config::Config {
    test_support::config("fiber-retry-policy", overrides)
}

#[test]
fn retry_policy_clamps_huge_attempts() {
    let retry = retry_policy(&config(vec!["retry.attempts=18446744073709551615".into()]));
    assert_eq!(retry.attempts, u32::MAX);
}

#[test]
fn retry_policy_reads_a_set_initial_delay() {
    let retry = retry_policy(&config(vec!["retry.initial_delay_ms=250".into()]));
    assert_eq!(retry.initial, std::time::Duration::from_millis(250));
}

#[test]
fn retry_policy_reads_a_set_max_delay() {
    let retry = retry_policy(&config(vec!["retry.max_delay_ms=9000".into()]));
    assert_eq!(retry.max, std::time::Duration::from_millis(9000));
}
