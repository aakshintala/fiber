use contract::clock::Clock;

use crate::run::System;
use crate::screen::Screen;

use super::Recorder;
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

fn recorder(cols: usize, rows: usize) -> (Screen, Recorder) {
    (Screen::new(cols, rows), Recorder::new(cols))
}

fn first_line(text: &str) -> &str {
    text.split('\n').next().unwrap_or_default()
}

#[test]
fn two_frames_in_one_chunk_record_both() {
    let (mut screen, mut recorder) = recorder(10, 3);
    let now = System.now();
    recorder.push(
        &mut screen,
        b"\x1b[?2026h\x1b[1;1Hhi\x1b[?2026l\x1b[?2026h\x1b[2;1Hyo\x1b[?2026l",
        now,
    );
    assert_eq!(recorder.frames.len(), 2);
    for frame in &recorder.frames {
        assert_eq!(frame.bytes, 8 + 6 + 2 + 8);
        assert_eq!(frame.at, now);
    }
    assert!(first_line(&recorder.frames[0].text).starts_with("hi"));
    let mut second = recorder.frames[1].text.split('\n');
    second.next();
    assert!(second.next().unwrap_or_default().starts_with("yo"));
    assert_ne!(recorder.frames[0].text, recorder.frames[1].text);
}

#[test]
fn a_marker_split_across_chunks_joins() {
    let (mut screen, mut recorder) = recorder(10, 3);
    let now = System.now();
    recorder.push(&mut screen, b"\x1b[?2026h\x1b[1;1Hhi\x1b[?2026", now);
    assert!(recorder.frames.is_empty());
    let later = System.now();
    recorder.push(&mut screen, b"l", later);
    assert_eq!(recorder.frames.len(), 1);
    assert_eq!(recorder.frames[0].bytes, 8 + 6 + 2 + 8);
    assert_eq!(recorder.frames[0].at, later);
    assert!(first_line(&recorder.frames[0].text).starts_with("hi"));
}

#[test]
fn a_begin_marker_split_across_chunks_joins() {
    let (mut screen, mut recorder) = recorder(10, 3);
    let now = System.now();
    recorder.push(&mut screen, b"\x1b[?2", now);
    assert!(recorder.frames.is_empty());
    recorder.push(&mut screen, b"026h\x1b[1;1Hok\x1b[?2026l", now);
    assert_eq!(recorder.frames.len(), 1);
    assert_eq!(recorder.frames[0].bytes, 8 + 6 + 2 + 8);
    assert!(first_line(&recorder.frames[0].text).starts_with("ok"));
}

#[test]
fn bytes_outside_any_block_belong_to_no_frame() {
    let (mut screen, mut recorder) = recorder(10, 3);
    let now = System.now();
    recorder.push(&mut screen, b"hello\x1b[?2026h\x1b[2;1Hhi\x1b[?2026l", now);
    assert_eq!(recorder.frames.len(), 1);
    assert_eq!(recorder.frames[0].bytes, 8 + 6 + 2 + 8);
    recorder.push(&mut screen, b"\x1b[?2026l", now);
    assert_eq!(recorder.frames.len(), 1);
    assert!(screen.holds("hello"));
    assert!(screen.holds("hi"));
}
