//! The local time of day under a prompt bubble (`docs/tui.md`, "Turns"):
//! the turn's start as a dim, right-aligned `HH:MM` row, in the pages'
//! zone.

use jiff::tz::TimeZone;

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

/// `America/New_York`, looked up by name: the same lookup the system zone
/// needs.
#[cfg(test)]
pub(crate) fn new_york() -> TimeZone {
    TimeZone::get("America/New_York").expect("America/New_York in the system zoneinfo")
}

/// A `turn_started` envelope with one message at `ts` milliseconds.
#[cfg(test)]
pub(crate) fn turn_started_at(session: &str, text: &str, ts: u64) -> crate::link::Line {
    crate::link::Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"input": [{
            "type": "message",
            "source": "driver",
            "content": [{"type": "text", "text": text}],
        }]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

#[cfg(test)]
#[path = "local_time_tests.rs"]
mod tests;
