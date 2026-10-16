//! A `before_handoff` answer that changed nothing (`docs/testing.md`,
//! "Fakes"): every `Hooks` fake that never writes a note returns it.

use contract::hook::{BeforeHandoffAnswer, BeforeHandoffOutcome};

/// A `before_handoff` answer with no note and no notices.
pub fn no_note() -> BeforeHandoffAnswer {
    BeforeHandoffAnswer {
        outcome: BeforeHandoffOutcome::NoNote,
        notices: Vec::new(),
    }
}
