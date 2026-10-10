use super::Screen;

fn fed(chunks: &[&[u8]]) -> Screen {
    let mut screen = Screen::new(60, 12);
    for chunk in chunks {
        screen.feed(chunk);
    }
    screen
}

#[test]
fn a_redraw_that_skips_an_unchanged_cell_still_holds_the_word() {
    let screen = fed(&[b"\x1b[6;1Hquokkas", b"\x1b[6;1Hqu\x1b[6;4Hkkas"]);
    assert!(screen.holds("quokkas"), "screen:\n{}", screen.text());
}

#[test]
fn a_cursor_move_overwrites_the_cell() {
    let screen = fed(&[b"hello\x1b[1;1HJ"]);
    assert!(screen.holds("Jello"), "screen:\n{}", screen.text());
}

#[test]
fn a_feed_split_inside_an_escape_sequence_still_moves() {
    let screen = fed(&[b"\x1b[6;", b"1Hquokkas"]);
    assert!(screen.holds("quokkas"), "screen:\n{}", screen.text());
}

#[test]
fn a_feed_split_inside_a_multibyte_char_still_draws_it() {
    let mark = "▣".as_bytes();
    let screen = fed(&[&mark[..1], &mark[1..]]);
    assert!(screen.holds("▣"), "screen:\n{}", screen.text());
}

#[test]
fn an_osc_sequence_leaves_no_cells() {
    let screen = fed(&[b"\x1b]0;quokkas\x07"]);
    assert!(!screen.holds("quokkas"), "screen:\n{}", screen.text());
}

#[test]
fn entering_the_alternate_screen_clears_the_grid() {
    let screen = fed(&[b"hello\x1b[?1049h"]);
    assert!(!screen.holds("hello"), "screen:\n{}", screen.text());
}

#[test]
fn drawing_past_the_last_column_overwrites_it() {
    let mut screen = Screen::new(3, 1);
    screen.feed(b"abcdef");
    assert!(screen.holds("abf"), "screen:\n{}", screen.text());
}

#[test]
fn a_needle_not_on_screen_does_not_match() {
    let screen = fed(&[b"hello"]);
    assert!(!screen.holds("bye"), "screen:\n{}", screen.text());
}

#[test]
fn an_empty_needle_matches_nothing() {
    let screen = fed(&[b"hello"]);
    assert!(!screen.holds(""), "screen:\n{}", screen.text());
}

#[test]
fn the_cursor_does_not_wrap_to_the_next_row() {
    let mut screen = Screen::new(3, 2);
    screen.feed(b"abcde");
    assert!(!screen.holds("de"), "screen:\n{}", screen.text());
}

#[test]
fn an_sgr_sequence_moves_nothing() {
    let screen = fed(&[b"a\x1b[31mb"]);
    assert!(screen.holds("ab"), "screen:\n{}", screen.text());
}

#[test]
fn a_space_matches_only_a_space_cell() {
    let screen = fed(&[b"turn0"]);
    assert!(!screen.holds("turn 0"), "screen:\n{}", screen.text());
}

#[test]
fn a_word_with_a_space_matches_the_row() {
    let screen = fed(&[b"turn 0"]);
    assert!(screen.holds("turn 0"), "screen:\n{}", screen.text());
}

#[test]
fn erase_to_end_of_line_clears_from_the_cursor() {
    let screen = fed(&[b"hello\x1b[1;1H\x1b[K"]);
    assert!(!screen.holds("hello"), "screen:\n{}", screen.text());
}

#[test]
fn erase_in_display_clears_the_grid() {
    let screen = fed(&[b"hello\x1b[2J"]);
    assert!(!screen.holds("hello"), "screen:\n{}", screen.text());
}
