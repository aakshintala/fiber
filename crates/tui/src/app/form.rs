//! Answering and declining the request the panel shows (`docs/tui.md`,
//! "Approvals and questions", "A question form").

use super::{App, Effect, Kind, Link, mint};
use crate::approvals::Queue;

impl App {
    /// Sends the shown request's answer. With the link down nothing goes
    /// out and the request stays.
    pub(super) fn answer(&mut self) -> Effect {
        self.reply(Queue::answer)
    }

    /// Declines the shown form with `reply` `declined` (`docs/tui.md`, "A
    /// question form"). With the link down nothing goes out and the form
    /// stays.
    pub(super) fn decline(&mut self) -> Effect {
        self.reply(Queue::decline)
    }

    /// Sends the `reply` line `make` builds for the shown request under a
    /// new command id, pending until the hub answers it.
    fn reply(&mut self, make: fn(&mut Queue, &str) -> Option<String>) -> Effect {
        if self.link != Link::Up {
            return Effect::None;
        }
        let id = mint();
        let Some(line) = make(&mut self.queue, &id) else {
            return Effect::None;
        };
        self.pending.insert(id, (Kind::Reply, String::new()));
        Effect::Send(vec![line])
    }
}

#[cfg(test)]
#[path = "form_tests.rs"]
mod tests;
