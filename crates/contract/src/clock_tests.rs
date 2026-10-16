use std::time::{Duration, SystemTime};

use super::{utc_date, utc_date_of_secs, utc_time, wall_ms};

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

#[test]
fn utc_date_pins_epoch_leap_days_and_century_years() {
    let cases = [
        (0_u64, (1970, 1, 1)),
        (86_399, (1970, 1, 1)),
        (86_400, (1970, 1, 2)),
        (946_598_400, (1999, 12, 31)),
        (951_782_400, (2000, 2, 29)),
        (951_868_800, (2000, 3, 1)),
        (1_709_164_800, (2024, 2, 29)),
        (1_735_603_200, (2024, 12, 31)),
        (4_107_456_000, (2100, 2, 28)),
        (4_107_542_400, (2100, 3, 1)),
        (13_574_563_200, (2400, 2, 29)),
        (13_574_649_600, (2400, 3, 1)),
        (253_402_214_400, (9999, 12, 31)),
    ];
    for (secs, date) in cases {
        assert_eq!(utc_date_of_secs(secs), date, "{secs}");
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        assert_eq!(utc_date(wall), date, "{secs}");
    }
}

#[test]
fn utc_date_before_the_epoch_is_the_epoch_date() {
    let wall = SystemTime::UNIX_EPOCH - Duration::from_secs(86_400 * 400);
    assert_eq!(utc_date(wall), (1970, 1, 1));
}

#[test]
fn utc_time_prints_seconds_since_the_epoch() {
    for (seconds, printed) in [
        (0, "1970-01-01T00:00:00Z"),
        (86_399, "1970-01-01T23:59:59Z"),
        (86_400, "1970-01-02T00:00:00Z"),
        (951_868_799, "2000-02-29T23:59:59Z"),
        (946_684_799, "1999-12-31T23:59:59Z"),
        (946_684_800, "2000-01-01T00:00:00Z"),
        (1_791_367_203, "2026-10-07T10:00:03Z"),
        (4_107_542_400, "2100-03-01T00:00:00Z"),
    ] {
        assert_eq!(utc_time(seconds), printed, "{seconds}");
    }
}
