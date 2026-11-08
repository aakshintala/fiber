//! A `before_compaction` answer that changed nothing (`docs/testing.md`,
//! "Fakes"): every `Hooks` fake that never writes a note returns it.

use contract::hook::{BeforeCompactionAnswer, BeforeCompactionOutcome};

/// A `before_compaction` answer with no note and no notices.
pub fn no_note() -> BeforeCompactionAnswer {
    BeforeCompactionAnswer {
        outcome: BeforeCompactionOutcome::NoNote,
        notices: Vec::new(),
    }
}
