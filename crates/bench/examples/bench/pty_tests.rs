#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failure is the test's"
)]

use std::time::Duration;

use contract::clock::Clock;

use crate::run::System;
use crate::screen::Screen;
use crate::test_support::loopback;

use super::Recorder;
use super::timeout_note;
use super::{OFFER_NEEDLE, Offer, Terminal, offer_state};

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

/// A screen holding `text` on its first row.
fn showing(text: &str) -> Screen {
    let mut screen = Screen::new(60, 12);
    screen.feed(text.as_bytes());
    screen
}

#[test]
fn the_offer_head_means_up() {
    assert_eq!(offer_state(&showing(OFFER_NEEDLE)), Offer::Up);
}

#[test]
fn the_prompt_alone_means_gone() {
    assert_eq!(offer_state(&showing("› /? for shortcuts")), Offer::Gone);
}

#[test]
fn a_blank_screen_means_waiting() {
    assert_eq!(offer_state(&Screen::new(60, 12)), Offer::Waiting);
}

#[test]
fn the_offer_wins_over_the_prompt_it_covers() {
    assert_eq!(
        offer_state(&showing("Import settings › /? for shortcuts")),
        Offer::Up
    );
}

/// What the loopback terminal drew, as raw bytes.
fn drawn(terminal: &Terminal) -> Vec<u8> {
    terminal
        .output
        .lock()
        .expect("reading what the terminal drew")
        .raw
        .clone()
}

/// Waits until what the terminal drew holds `byte`: keystrokes echo
/// back through the pty after the write returns, so the test fixes the
/// order with a wait, never a sleep.
fn wait_drawn(terminal: &Terminal, byte: u8) {
    let until = System.now() + DISMISS_WITHIN;
    while !drawn(terminal).contains(&byte) {
        let wait =
            crate::run::left(&System, until, "the echoed keystroke").expect("the echo arrives");
        #[allow(
            clippy::disallowed_methods,
            reason = "the bench test's wait, computed from its injected clock"
        )]
        let woke = terminal.wakes.recv_timeout(wait);
        match woke {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the terminal closed before the echoed keystroke")
            }
        }
    }
}

/// How long the loopback dismissal may take: the bytes round-trip
/// through `cat` in milliseconds, so seconds are generous.
const DISMISS_WITHIN: Duration = Duration::from_secs(10);

#[test]
fn dismiss_sends_esc_once_the_offer_draws() {
    use std::io::Write;

    let (mut terminal, mut drawer) = loopback();
    drawer
        .write_all(OFFER_NEEDLE.as_bytes())
        .expect("showing the offer");
    terminal
        .wait_for(&System, System.now() + DISMISS_WITHIN, OFFER_NEEDLE)
        .expect("the offer draws");
    terminal
        .dismiss_first_run_offer(&System, System.now() + DISMISS_WITHIN)
        .expect("dismissing the offer");
    wait_drawn(&terminal, 0x1b);
    assert!(drawn(&terminal).contains(&0x1b), "the dismissal sends Esc");
}

#[test]
fn dismiss_sends_nothing_when_the_prompt_is_already_there() {
    use std::io::Write;

    let prompt = "› /? for shortcuts";
    let (mut terminal, mut drawer) = loopback();
    drawer
        .write_all(prompt.as_bytes())
        .expect("showing the prompt");
    // The prompt is on screen before the dismissal runs: it returns at
    // once down the gone path, and no keystroke is ever in flight.
    terminal
        .wait_for(&System, System.now() + DISMISS_WITHIN, prompt)
        .expect("the prompt draws");
    terminal
        .dismiss_first_run_offer(&System, System.now() + DISMISS_WITHIN)
        .expect("nothing to dismiss");
    assert!(
        !drawn(&terminal).contains(&0x1b),
        "no Esc goes out without an offer"
    );
}

#[test]
fn dismiss_times_out_when_neither_marker_ever_draws() {
    let (mut terminal, _) = loopback();
    let err = terminal
        .dismiss_first_run_offer(&System, System.now() + Duration::from_millis(50))
        .expect_err("nothing ever draws");
    assert!(err.contains("timed out"), "{err}");
}
