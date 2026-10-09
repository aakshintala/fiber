use super::holds;

#[test]
fn a_needle_without_a_space_matches_one_run_of_bytes() {
    assert!(holds(b"ab>cd", b">"));
    assert!(holds(b"turn", b"turn"));
    assert!(!holds(b"tu\x1b[1;1Hrn", b"turn"));
    assert!(!holds(b"tur", b"turn"));
}

#[test]
fn a_space_matches_a_literal_space() {
    assert!(holds(b"x turn 0 y", b"turn 0"));
}

#[test]
fn a_space_matches_one_cursor_position_sequence() {
    assert!(holds(b"\x1b[10;4Hturn\x1b[10;9H0\x1b[10;60H", b"turn 0"));
    assert!(holds(b"a\x1b[1;2Hb\x1b[12;345Hc", b"a b c"));
}

#[test]
fn a_space_at_either_end_of_the_needle_still_needs_its_match() {
    assert!(holds(b"\x1b[1;1Hx", b" x"));
    assert!(holds(b"x ", b"x "));
    assert!(!holds(b"x", b" x"));
    assert!(!holds(b"x", b"x "));
}

#[test]
fn a_space_matches_nothing_else() {
    for output in [
        &b"turn0"[..],
        b"turn  0",
        b"turn\x1b[10;9H\x1b[10;9H0",
        b"turn\x1b[10;9H 0",
        b"turn\x1b[10H0",
        b"turn\x1b[;9H0",
        b"turn\x1b[10;H0",
        b"turn\x1b[10;9m0",
        b"turn\x1b10;9H0",
        b"turn\x1b[10;9",
        b"turn\x1b[1a;9H0",
    ] {
        assert!(!holds(output, b"turn 0"), "{output:?}");
    }
}

#[test]
fn the_match_may_start_anywhere_after_a_failed_start() {
    assert!(holds(b"turn\x1b[1;1Hx turn 0", b"turn 0"));
    assert!(holds(b"ttturn 0", b"turn 0"));
}

#[test]
fn an_empty_needle_matches_nothing() {
    assert!(!holds(b"abc", b""));
    assert!(!holds(b"", b"x"));
}

use super::timeout_note;

#[test]
fn a_timeout_note_reports_the_byte_count_and_the_tail() {
    let note = timeout_note(
        "timed out waiting for \"quokkas\" on the terminal".to_owned(),
        b"hello world",
    );
    assert!(note.contains("timed out waiting"), "{note}");
    assert!(note.contains("drew 11 bytes"), "{note}");
    assert!(note.contains("hello world"), "{note}");
}

#[test]
fn a_timeout_note_keeps_only_the_last_3000_bytes_and_replaces_invalid_ones() {
    let mut output = vec![b'q'; 3000];
    output.extend_from_slice(b"\xff\xfeTAIL");
    let note = timeout_note("timed out".to_owned(), &output);
    assert!(note.contains("drew 3006 bytes"), "{note}");
    assert!(note.contains("TAIL"), "{note}");
    assert!(!note.contains(&"q".repeat(2995)), "{note}");
    assert!(note.contains(&"q".repeat(2994)), "{note}");
}
