//! The local time of day under a prompt bubble (`docs/tui.md`, "Turns"):
//! the turn's start as a dim, right-aligned `HH:MM` row, in the pages'
//! zone.

use jiff::tz::TimeZone;

use crate::format;
use crate::turn::Row;

/// The wall-clock time of `ms` epoch milliseconds in `zone`, 24-hour and
/// zero-padded (`09:05`, `14:15`). `None` when `ms` is past the latest
/// instant jiff holds. The range is checked first: `from_nanosecond`
/// panics past it (a `debug_assert` in its bounds check) instead of
/// returning an error. `as_millisecond` truncates, so the bound is the
/// latest whole millisecond jiff holds; `from_millisecond` alone would
/// clip the 999 milliseconds below it.
pub(crate) fn time_of_day(ms: u64, zone: &TimeZone) -> Option<String> {
    let latest = u64::try_from(jiff::Timestamp::MAX.as_millisecond()).unwrap_or(u64::MAX);
    if ms > latest {
        return None;
    }
    let stamp = jiff::Timestamp::from_nanosecond(i128::from(ms) * 1_000_000).ok()?;
    Some(stamp.to_zoned(zone.clone()).strftime("%H:%M").to_string())
}

/// [`time_of_day`] as a dim, right-aligned row with no target; `None` when
/// there is no time to show.
pub(crate) fn under_bubble(ms: u64, zone: &TimeZone) -> Option<Row> {
    let time = time_of_day(ms, zone)?;
    Some((format::dim(time).right_aligned(), None))
}

#[cfg(test)]
#[path = "local_time_tests.rs"]
mod tests;
