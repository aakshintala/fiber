//! The clock seam (`docs/testing.md`, "Values that change every run"):
//! monotonic time for deadlines, wall time for `ts` and expiries, and
//! waiting. Like [`crate::tool::Tool`] and [`crate::signing::Signer`], it
//! defines the seam and contains no behaviour. Its traits live in
//! [`support::clock`](support::clock) and are re-exported here
//! (`docs/architecture.md`, "The modules").

use std::time::SystemTime;

pub use support::clock::{Clock, Wake};

/// `wall` as milliseconds since the Unix epoch: 0 before it, `u64::MAX` past it.
pub fn wall_ms(wall: SystemTime) -> u64 {
    wall.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        .try_into()
        .unwrap_or(u64::MAX)
}

/// The UTC calendar date of `wall` as year, month and day. A time before
/// the epoch reads as the epoch's date.
pub fn utc_date(wall: SystemTime) -> (u64, u64, u64) {
    let secs = wall
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    utc_date_of_secs(secs)
}

/// The UTC calendar date of `secs` seconds after the epoch, in the proleptic
/// Gregorian calendar (Howard Hinnant's days-to-civil algorithm).
pub fn utc_date_of_secs(secs: u64) -> (u64, u64, u64) {
    let days = secs / 86_400;
    let era = (days + 719_468) / 146_097;
    let start = days + 719_468 - era * 146_097;
    let year = (start - start / 1_460 + start / 36_524 - start / 146_096) / 365;
    let ordinal = start - (365 * year + year / 4 - year / 100);
    let month = (5 * ordinal + 2) / 153;
    let day = ordinal - (153 * month + 2) / 5 + 1;
    let month = if month < 10 { month + 3 } else { month - 9 };
    let year = if month <= 2 {
        year + era * 400 + 1
    } else {
        year + era * 400
    };
    (year, month, day)
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
