//! The dropped connection (`docs/tui.md`, "A dropped connection"): the
//! backoff between attempts, the banner that counts them, and which
//! failure says so in a notice.

use std::time::Duration;

use super::{App, Link};

/// The delay after the first failure; each later one doubles it.
const FIRST: Duration = Duration::from_millis(500);

/// The longest delay between attempts.
const CAP: Duration = Duration::from_secs(30);

/// The connection's history since the hub was last reached.
#[derive(Debug, Default)]
pub(crate) struct Reconnect {
    /// Failed connects and ended connections since the hub was last
    /// reached.
    failures: u32,
    /// The hub was reached once in this run.
    was_up: bool,
}

impl App {
    /// One more failure: the delay before the next attempt, doubling from
    /// half a second to at most thirty. A hub refused for its schema is
    /// never tried again.
    pub(crate) fn next_retry(&mut self) -> Option<Duration> {
        if self.link == Link::Refused {
            return None;
        }
        self.reconnect.failures = self.reconnect.failures.saturating_add(1);
        let shift = self.reconnect.failures.saturating_sub(1);
        let factor = 1u32.checked_shl(shift).unwrap_or(u32::MAX);
        Some(FIRST.saturating_mul(factor).min(CAP))
    }

    /// The banner in the working line's place while the terminal
    /// reconnects. Before the hub was first reached, the first connect was
    /// attempt 1, so the first retry is attempt 2.
    pub(crate) fn banner(&self) -> Option<String> {
        let failures = self.reconnect.failures;
        (self.link == Link::Down && failures >= 1).then(|| {
            let attempt = failures.saturating_add(u32::from(!self.reconnect.was_up));
            format!("Connection lost · reconnecting (attempt {attempt})…")
        })
    }

    /// The hub answered `hub_hello`: the count starts again.
    pub(super) fn hub_reached(&mut self) {
        self.reconnect.failures = 0;
    }

    /// A `hub_hello` this terminal reads: the hub is up, and was up once.
    pub(super) fn reconnected(&mut self) -> Vec<String> {
        self.hub_reached();
        self.reconnect.was_up = true;
        Vec::new()
    }

    /// Whether a failed connect says so in a notice: only the first of a
    /// run of failures does, and the banner counts the rest.
    pub(super) fn notice_due(&self) -> bool {
        self.reconnect.failures == 0
    }
}

#[cfg(test)]
#[path = "reconnect_tests.rs"]
mod tests;
