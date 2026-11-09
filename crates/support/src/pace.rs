//! The pacing rule for streamed output (`docs/tools.md`, "Progress"): the
//! interval a delta costs and the cap on held text. The loop's paced
//! `tool_call_delta` and the shell tool's paced `job_delta` and driver
//! `shell_command_delta` all use it.

use std::time::Duration;

/// At most one delta every 100 ms (`docs/tools.md`, "Progress").
pub const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// The byte rate the interval grows past the minimum with: 100 KiB/s
/// (`docs/tools.md`, "Progress").
pub const BYTES_PER_SECOND: u64 = 102_400;

/// The most delta text a holder keeps: 64 KiB (`docs/events.md`,
/// `tool_call_delta`). Past it the oldest bytes are dropped, so only the
/// result and its artifact hold the whole output.
pub const HELD_MAX: usize = 65_536;

/// How long a written delta of `bytes` encoded bytes holds back the next:
/// `max(100 ms, bytes ÷ 100 KiB/s)` (`docs/tools.md`, "Progress").
#[must_use]
pub fn interval(bytes: u64) -> Duration {
    let paced = Duration::from_nanos(bytes.saturating_mul(1_000_000_000) / BYTES_PER_SECOND);
    paced.max(MIN_INTERVAL)
}

/// Drops the oldest bytes past `HELD_MAX`, keeping the newest starting at
/// a character boundary, so the text stays valid UTF-8.
pub fn cap_held(text: &mut String) {
    let cut = text.len().saturating_sub(HELD_MAX);
    text.drain(..text.ceil_char_boundary(cut));
}

#[cfg(test)]
#[path = "pace_tests.rs"]
mod tests;
