//! Tests for kitty's graphics escapes: the place bytes pin `U=1`, the
//! transmit chunks, and the placeholder cells.

use ratatui::style::Style;

use super::{cell, delete, is_placeholder, place, transmit};

#[test]
fn place_pins_unicode_placement() {
    // The terminal moves the image with the text, through the Unicode
    // placeholders: always `U=1` under the image's id.
    assert_eq!(place(7, 32, 12), b"\x1b_Ga=p,U=1,i=7,c=32,r=12\x1b\\");
}

#[test]
fn delete_clears_the_image() {
    assert_eq!(delete(7), b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
}

#[test]
fn transmit_sends_one_chunk_with_no_more() {
    assert_eq!(
        transmit(7, "AAAA"),
        b"\x1b_Ga=t,U=1,f=100,i=7,q=2,m=0;AAAA\x1b\\"
    );
}

#[test]
fn transmit_chunks_at_4096() {
    let prefix = "\x1b_Ga=t,U=1,f=100,i=7,q=2,m=0;";
    // Exactly one chunk still sends `m=0` once.
    let full = "B".repeat(4096);
    let bytes = transmit(7, &full);
    assert_eq!(
        bytes,
        [prefix.as_bytes(), full.as_bytes(), b"\x1b\\"].concat()
    );
    assert!(transmit(7, "").is_empty());
}

#[test]
fn transmit_marks_every_chunk_but_the_last_with_more() {
    let payload = "B".repeat(4097);
    let text = String::from_utf8(transmit(7, &payload)).expect("escapes are text");
    let parts: Vec<&str> = text.split("\x1b\\").collect();
    assert_eq!(parts.len(), 3);
    assert!(parts[0].starts_with("\x1b_Ga=t,U=1,f=100,i=7,q=2,m=1;"));
    assert_eq!(
        parts[0].len(),
        "\x1b_Ga=t,U=1,f=100,i=7,q=2,m=1;".len() + 4096
    );
    assert_eq!(parts[1], "\x1b_Gm=0;B");
}

#[test]
fn placeholder_cells_carry_row_and_column() {
    let style = Style::new();
    let first = cell(0, 0, style);
    assert_eq!(first.content.as_ref(), "\u{10EEEE}\u{305}\u{305}");
    assert_eq!(first.style, style);
    let next = cell(1, 2, style);
    assert_eq!(next.content.as_ref(), "\u{10EEEE}\u{30D}\u{30E}");
    // Past the diacritic table reads as the bare base character, never
    // panicking.
    assert_eq!(
        cell(64, 64, style).content.as_ref(),
        "\u{10EEEE}\u{10EEEE}\u{10EEEE}"
    );
}

#[test]
fn is_placeholder_names_only_placeholder_cells() {
    assert!(is_placeholder("\u{10EEEE}\u{305}\u{305}"));
    assert!(!is_placeholder(" "));
    assert!(!is_placeholder("▣"));
    assert!(!is_placeholder(""));
}
