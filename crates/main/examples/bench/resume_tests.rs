use super::{Fixture, LARGE, SMALL};
use crate::busy::BYTES_PER_TOKEN;

/// The session's tokens at 4 bytes per token.
fn tokens(fixture: &Fixture) -> usize {
    fixture.turns * fixture.reply_bytes / BYTES_PER_TOKEN
}

#[test]
fn the_fixtures_reply_sizes_sum_to_their_token_targets() {
    assert_eq!(tokens(&SMALL), 20_000);
    assert_eq!(tokens(&LARGE), 2_000_000);
    assert_eq!(SMALL.handoffs, 0);
    // A handoff after every turn but the last.
    assert_eq!(LARGE.handoffs, LARGE.turns - 1);
    assert_eq!(LARGE.handoffs, 7);
}

#[test]
fn the_script_holds_each_reply_each_note_and_the_measured_reply() {
    assert_eq!(SMALL.script().len(), 2);
    assert_eq!(LARGE.script().len(), 8 + 7 + 1);
}
