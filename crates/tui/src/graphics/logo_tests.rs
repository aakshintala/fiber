//! Tests for the logo's XTVERSION reply grammar: what holds, what
//! completes, and what reads as keys (`docs/tui.md`, "The logo").

use super::{Dcs, dcs};
use crate::keys::{Event, Reply};

/// A complete reply carries the version text and its length.
#[test]
fn dcs_done_names_the_version_and_its_length() {
    for (bytes, want) in [
        (b"\x1bP>|iTerm2 3.5.0\x1b\\".as_slice(), "iTerm2 3.5.0"),
        (b"\x1bP>|WezTerm 1\x07".as_slice(), "WezTerm 1"),
        // Empty text is a version too: it simply names no protocol.
        (b"\x1bP>|\x1b\\".as_slice(), ""),
    ] {
        let (events, used) = match dcs(bytes) {
            Dcs::Done(events, used) => (events, used),
            Dcs::Hold | Dcs::Off => panic!("{bytes:?} completes"),
        };
        assert_eq!(events, vec![Event::Reply(Reply::Version(want.to_owned()))]);
        assert_eq!(used, bytes.len());
    }
}

/// A prefix that fits so far holds for the next read.
#[test]
fn dcs_hold_keeps_a_fitting_prefix() {
    for prefix in [
        b"\x1b".as_slice(),
        b"\x1bP".as_slice(),
        b"\x1bP>".as_slice(),
        b"\x1bP>|".as_slice(),
        b"\x1bP>|iTerm2 ".as_slice(),
        b"\x1bP>|iTerm2 3.5.0\x1b".as_slice(),
    ] {
        assert!(matches!(dcs(prefix), Dcs::Hold), "{prefix:?} holds");
    }
}

/// The first byte off the grammar ends the candidate: it reads as
/// keys instead, never holding.
#[test]
fn dcs_off_rejects_off_grammar_bytes() {
    for bytes in [
        b"\x1bX>|iTerm2 3.5.0\x1b\\".as_slice(),
        b"\x1bP>|iTerm\x012\x1b\\".as_slice(),
        b"\x1bP>|text\x1bX".as_slice(),
    ] {
        assert!(matches!(dcs(bytes), Dcs::Off), "{bytes:?} is keys");
    }
}

/// A candidate past the cap resolves instead of holding forever.
#[test]
fn dcs_off_past_the_cap() {
    let mut candidate = b"\x1bP>|".to_vec();
    candidate.extend(std::iter::repeat_n(b'x', 300));
    assert!(matches!(dcs(&candidate), Dcs::Off));
}
