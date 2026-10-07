use std::time::{Duration, SystemTime};

use super::wall_ms;

#[test]
fn the_epoch_is_zero() {
    assert_eq!(wall_ms(SystemTime::UNIX_EPOCH), 0);
}

#[test]
fn a_fixed_time_counts_its_milliseconds() {
    let wall = SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
    assert_eq!(wall_ms(wall), 1_700_000_000_123);
}

#[test]
fn before_the_epoch_is_zero() {
    let wall = SystemTime::UNIX_EPOCH - Duration::from_secs(1);
    assert_eq!(wall_ms(wall), 0);
}

#[test]
fn past_u64_milliseconds_saturates() {
    let wall = SystemTime::UNIX_EPOCH + Duration::new(u64::MAX / 1_000 + 1, 0);
    assert_eq!(wall_ms(wall), u64::MAX);
}
