//! Tests for the question panel's stateless text rows: the character
//! cursor's edges and multi-byte text.

use crate::keys::Edit;

use super::{
    cells, delete_before, delete_to_line_start, delete_word, edit_line, insert_at, line_end,
    line_start, move_caret, word_left, word_right,
};

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
fn cells_count_wide_characters_twice() {
    assert_eq!(cells("ab"), 2);
    assert_eq!(cells("名前"), 4);
    assert_eq!(cells("▸ 1 ✎ "), 6);
}

#[test]
fn word_moves_stop_at_gaps_and_ends() {
    let mut caret = 7;
    word_left("one two", &mut caret);
    assert_eq!(caret, 4);
    word_left("one two", &mut caret);
    assert_eq!(caret, 0);
    word_left("one two", &mut caret);
    assert_eq!(caret, 0);
    word_right("one two", &mut caret);
    assert_eq!(caret, 3);
    word_right("one two", &mut caret);
    assert_eq!(caret, 7);
    word_right("one two", &mut caret);
    assert_eq!(caret, 7);
}

#[test]
fn word_moves_treat_underscore_as_a_word_character() {
    let mut caret = 7;
    word_left("a_b c_d", &mut caret);
    assert_eq!(caret, 4);
    let mut caret = 0;
    word_right("a_b c_d", &mut caret);
    assert_eq!(caret, 3);
}

#[test]
fn word_moves_step_over_multi_byte_characters() {
    let mut caret = 5;
    word_left("名前 ab", &mut caret);
    assert_eq!(caret, 3);
    let mut caret = 0;
    word_right("名前 ab", &mut caret);
    assert_eq!(caret, 2);
}

#[test]
fn delete_word_removes_the_previous_word() {
    let (mut text, mut caret) = ("one two".to_owned(), 7);
    delete_word(&mut text, &mut caret);
    assert_eq!((text, caret), ("one ".to_owned(), 4));
}

#[test]
fn delete_word_at_the_start_does_nothing() {
    let (mut text, mut caret) = ("one two".to_owned(), 0);
    delete_word(&mut text, &mut caret);
    assert_eq!((text, caret), ("one two".to_owned(), 0));
}

#[test]
fn delete_to_line_start_clears_to_the_row_start() {
    let (mut text, mut caret) = ("one two three".to_owned(), 13);
    delete_to_line_start(&mut text, &mut caret);
    assert_eq!((text, caret), (String::new(), 0));
}

#[test]
fn line_start_and_end_move_to_the_row_ends() {
    let mut caret = 2;
    line_start(&mut caret);
    assert_eq!(caret, 0);
    line_end("one two", &mut caret);
    assert_eq!(caret, 7);
}

#[test]
fn line_edits_leave_breaks_paste_and_forward_delete_alone() {
    let (mut text, mut caret) = ("ab".to_owned(), 2);
    for edit in [
        Edit::ShiftEnter,
        Edit::CtrlJ,
        Edit::Delete,
        Edit::Paste("x".to_owned()),
    ] {
        edit_line(&mut text, &mut caret, &edit);
        assert_eq!((text.as_str(), caret), ("ab", 2), "{edit:?}");
    }
    edit_line(&mut text, &mut caret, &Edit::WordLeft);
    assert_eq!(caret, 0);
}
