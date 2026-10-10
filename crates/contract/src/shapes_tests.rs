//! `Usage`'s default.

use super::*;

#[test]
fn default_usage_is_zero_counts_with_a_known_zero_cost() {
    // Field by field: a derived `Default` would leave `cost` as
    // `None`, an unknown cost, instead of a known `0`.
    let usage = Usage::default();
    assert_eq!(usage.tokens.input, 0);
    assert_eq!(usage.tokens.cache_read, 0);
    assert_eq!(usage.tokens.cache_write, BTreeMap::new());
    assert_eq!(usage.tokens.output, 0);
    assert_eq!(usage.cost, Some(0.0));
    assert_eq!(usage.subscription_cost, 0.0);
}
