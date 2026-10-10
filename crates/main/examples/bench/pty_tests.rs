use crate::screen::Screen;

use super::timeout_note;

fn screen_with(contents: &[u8]) -> Screen {
    let mut screen = Screen::new(60, 12);
    screen.feed(contents);
    screen
}

#[test]
fn a_timeout_note_reports_the_byte_count_the_tail_and_the_screen() {
    let screen = screen_with(b"hello world");
    let note = timeout_note(
        "timed out waiting for \"quokkas\" on the terminal".to_owned(),
        b"hello world",
        &screen,
    );
    assert!(note.contains("timed out waiting"), "{note}");
    assert!(note.contains("drew 11 bytes"), "{note}");
    assert!(note.contains("hello world"), "{note}");
    assert!(note.contains("screen:"), "{note}");
}

#[test]
fn a_timeout_note_keeps_only_the_last_3000_bytes_and_replaces_invalid_ones() {
    let mut output = vec![b'q'; 3000];
    output.extend_from_slice(b"\xff\xfeTAIL");
    let screen = screen_with(&output);
    let note = timeout_note("timed out".to_owned(), &output, &screen);
    assert!(note.contains("drew 3006 bytes"), "{note}");
    assert!(note.contains("TAIL"), "{note}");
    assert!(!note.contains(&"q".repeat(2995)), "{note}");
    assert!(note.contains(&"q".repeat(2994)), "{note}");
}
