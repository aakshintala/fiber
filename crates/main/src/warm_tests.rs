//! `cache.warm_idle` and `cache.warm_cap` reach the loop's warming cap.

use super::warm;
use crate::test_support;

fn config(overrides: &[&str]) -> config::Config {
    test_support::config("fiber-warm-settings", overrides)
}

#[test]
fn warming_is_off_by_default_whatever_the_cap() {
    assert_eq!(warm(&config(&[])), None);
    assert_eq!(warm(&config(&["cache.warm_cap=5"])), None);
    assert_eq!(warm(&config(&["cache.warm_idle=false"])), None);
}

#[test]
fn warming_on_takes_the_default_cap_of_two_lifetimes() {
    assert_eq!(warm(&config(&["cache.warm_idle=true"])), Some(2));
}

#[test]
fn warming_on_takes_a_set_cap() {
    let eleven = config(&["cache.warm_idle=true", "cache.warm_cap=11"]);
    assert_eq!(warm(&eleven), Some(11));
    let zero = config(&["cache.warm_idle=true", "cache.warm_cap=0"]);
    assert_eq!(warm(&zero), Some(0));
}
