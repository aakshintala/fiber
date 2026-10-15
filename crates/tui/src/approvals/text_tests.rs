//! Tests for the question panel's stateless text rows: the character
//! cursor's edges and multi-byte text.

use super::{cells, clean, delete_before, insert_at, move_caret, words_row};

#[test]
fn inserting_moves_past_the_new_character() {
    let (mut text, mut caret) = ("ad".to_owned(), 1);
    insert_at(&mut text, &mut caret, 'b');
    assert_eq!((text, caret), ("abd".to_owned(), 2));
    // A multi-byte character moves one character, past its bytes.
    let (mut text, mut caret) = ("a".to_owned(), 1);
    insert_at(&mut text, &mut caret, '名');
    assert_eq!((text, caret), ("a名".to_owned(), 2));
}

#[test]
fn deleting_at_the_start_does_nothing() {
    let (mut text, mut caret) = ("ab".to_owned(), 0);
    delete_before(&mut text, &mut caret);
    assert_eq!((text, caret), ("ab".to_owned(), 0));
    let (mut text, mut caret) = ("a名".to_owned(), 2);
    delete_before(&mut text, &mut caret);
    assert_eq!((text, caret), ("a".to_owned(), 1));
}

#[test]
fn the_character_cursor_stops_at_both_ends() {
    let mut caret = 0;
    move_caret("ab", &mut caret, false);
    assert_eq!(caret, 0);
    move_caret("ab", &mut caret, true);
    move_caret("ab", &mut caret, true);
    move_caret("ab", &mut caret, true);
    assert_eq!(caret, 2);
}

#[test]
fn cleaning_turns_every_control_into_a_space() {
    assert_eq!(clean("a\nb\rc\td\x1b[2J"), "a b c d [2J");
    assert_eq!(clean(""), "");
}

#[test]
fn cells_count_wide_characters_twice() {
    assert_eq!(cells("ab"), 2);
    assert_eq!(cells("名前"), 4);
    assert_eq!(cells("▸ 1 ✎ "), 6);
}

#[test]
fn the_words_row_clips_at_the_width_and_keeps_the_caret_last() {
    assert_eq!(
        words_row("abcdefgh", 8, true, 12),
        ("› ✎ bcdefgh".to_owned(), Some(11))
    );
    assert_eq!(
        words_row("abcdefgh", 8, false, 8),
        ("  ✎ abcd".to_owned(), None)
    );
    assert_eq!(
        words_row("", 0, true, 60),
        ("› ✎ answer in words".to_owned(), Some(4))
    );
}
