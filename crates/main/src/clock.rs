//! The process clock (`docs/architecture.md`, "The call rules"): built once
//! by `main` and passed down. The only production caller of the process
//! clock.

use std::sync::Weak;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use contract::clock::{Clock, Wake};

/// The operating system's clock.
pub(crate) struct System;

impl Clock for System {
    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::now"
    )]
    fn now(&self) -> Instant {
        Instant::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::wall"
    )]
    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::sleep"
    )]
    fn sleep(&self, d: Duration) {
        thread::sleep(d);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        let bound = match until {
            None => None,
            Some(until) => {
                let now = self.now();
                if until <= now {
                    Some(Duration::ZERO)
                } else {
                    Some(until.saturating_duration_since(now))
                }
            }
        };
        wait(bound);
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
