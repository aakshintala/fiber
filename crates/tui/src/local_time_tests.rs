//! Tests for the local time of day under a prompt bubble, in a fixed zone:
//! no test reads the real clock.

use jiff::tz::TimeZone;
use ratatui::layout::Alignment;
use ratatui::style::Modifier;

use super::{new_york, time_of_day};

#[test]
fn spring_forward_skips_the_second_hour() {
    let zone = new_york();
    // 2026-03-08T06:59Z, a minute before the clocks change.
    assert_eq!(time_of_day(1772953140000, &zone), Some("01:59".to_owned()));
    // 2026-03-08T07:00Z, when 02:00 becomes 03:00.
    assert_eq!(time_of_day(1772953200000, &zone), Some("03:00".to_owned()));
}

#[test]
fn fall_back_repeats_the_first_hour() {
    let zone = new_york();
    // 2026-11-01T05:59Z, a minute before the clocks change.
    assert_eq!(time_of_day(1793512740000, &zone), Some("01:59".to_owned()));
    // 2026-11-01T06:00Z, when 02:00 becomes 01:00.
    assert_eq!(time_of_day(1793512800000, &zone), Some("01:00".to_owned()));
}

#[test]
fn utc_and_new_york_differ_by_the_offset() {
    // 2026-10-08T14:15Z.
    assert_eq!(
        time_of_day(1791468900000, &TimeZone::UTC),
        Some("14:15".to_owned())
    );
    assert_eq!(
        time_of_day(1791468900000, &new_york()),
        Some("10:15".to_owned())
    );
}

#[test]
fn out_of_range_instants_have_no_time() {
    let zone = TimeZone::UTC;
    assert_eq!(time_of_day(u64::MAX, &zone), None);
    let max = u64::try_from(jiff::Timestamp::MAX.as_millisecond()).unwrap_or(u64::MAX);
    assert!(time_of_day(max, &zone).is_some());
    assert_eq!(time_of_day(max.saturating_add(1), &zone), None);
}

#[test]
fn the_time_row_is_a_dim_right_aligned_row_with_no_target() {
    use crate::rows::Rows;
    use crate::turn::Turn;

    let zone = TimeZone::UTC;
    let turn = Turn::new(vec!["go".to_owned()], 1791468900000);
    let mut out = Rows::default();
    turn.rows(80, &zone, &mut out);
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 4);
    let (line, target) = &rows[3];
    assert_eq!(line.to_string(), "14:15");
    assert!(line.alignment == Some(Alignment::Right));
    assert!(line.style.add_modifier.contains(Modifier::DIM));
    assert!(target.is_none());
    // Past the latest instant jiff holds, the bubble draws with no time
    // under it.
    let turn = Turn::new(vec!["go".to_owned()], u64::MAX);
    let mut out = Rows::default();
    turn.rows(80, &zone, &mut out);
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 3);
}
