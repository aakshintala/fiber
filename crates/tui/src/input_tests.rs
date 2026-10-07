//! Tests for the draft editor.

use super::Draft;

/// A draft holding `text`, typed: characters and line breaks.
fn typed(text: &str) -> Draft {
    let mut draft = Draft::default();
    for ch in text.chars() {
        if ch == '\n' {
            draft.line_break();
        } else {
            draft.insert(ch);
        }
    }
    draft
}

/// `n` numbered lines joined by line breaks.
fn lines(n: usize) -> String {
    (1..=n)
        .map(|at| format!("l{at}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The draft's text with `|` where the cursor is, reading the cursor
/// back by typing a marker and deleting it.
fn shown(draft: &Draft) -> String {
    let mut probe = draft.clone();
    probe.insert('|');
    probe.expand()
}

#[test]
fn typing_inserts_at_the_cursor() {
    let mut draft = typed("ac");
    draft.left();
    draft.insert('b');
    assert_eq!(draft.expand(), "abc");
    assert_eq!(shown(&draft), "ab|c");
}

#[test]
fn expand_of_typed_text_is_the_text() {
    for text in ["", "a", "two\nlines", "\n\n", "日本語 😀\nx"] {
        assert_eq!(typed(text).expand(), text);
    }
}

#[test]
fn empty_and_clear() {
    let mut draft = Draft::default();
    assert!(draft.is_empty());
    draft.line_break();
    assert!(!draft.is_empty());
    assert_eq!(draft.expand(), "\n");
    draft.clear();
    assert!(draft.is_empty());
    assert_eq!(draft.expand(), "");
    assert_eq!(shown(&draft), "|");
}

#[test]
fn control_characters_are_not_inserted() {
    let mut draft = Draft::default();
    draft.insert('\u{1b}');
    draft.insert('\n');
    assert!(draft.is_empty());
    draft.insert('\t');
    assert_eq!(draft.expand(), "\t");
}

#[test]
fn left_and_right_cross_line_breaks_and_stop_at_the_ends() {
    let mut draft = typed("a\nb");
    draft.left();
    assert_eq!(shown(&draft), "a\n|b");
    draft.left();
    assert_eq!(shown(&draft), "a|\nb");
    draft.left();
    draft.left();
    assert_eq!(shown(&draft), "|a\nb");
    draft.right();
    draft.right();
    assert_eq!(shown(&draft), "a\n|b");
    draft.right();
    draft.right();
    assert_eq!(shown(&draft), "a\nb|");
}

#[test]
fn backspace_and_delete_cross_line_breaks() {
    let mut draft = typed("a\nb");
    draft.left();
    draft.backspace();
    assert_eq!(shown(&draft), "a|b");
    draft.delete();
    assert_eq!(shown(&draft), "a|");
    draft.delete();
    assert_eq!(shown(&draft), "a|");
    draft.backspace();
    draft.backspace();
    assert_eq!(shown(&draft), "|");
}

#[test]
fn word_moves_go_to_word_starts_and_ends() {
    let mut draft = typed("one, two_2 three");
    draft.word_left();
    assert_eq!(shown(&draft), "one, two_2 |three");
    draft.word_left();
    assert_eq!(shown(&draft), "one, |two_2 three");
    draft.word_left();
    assert_eq!(shown(&draft), "|one, two_2 three");
    draft.word_left();
    assert_eq!(shown(&draft), "|one, two_2 three");
    draft.word_right();
    assert_eq!(shown(&draft), "one|, two_2 three");
    draft.word_right();
    assert_eq!(shown(&draft), "one, two_2| three");
    draft.word_right();
    draft.word_right();
    assert_eq!(shown(&draft), "one, two_2 three|");
}

#[test]
fn word_moves_cross_line_breaks() {
    let mut draft = typed("ab\ncd");
    draft.line_start();
    draft.word_left();
    assert_eq!(shown(&draft), "|ab\ncd");
    draft.word_right();
    draft.word_right();
    assert_eq!(shown(&draft), "ab\ncd|");
}

#[test]
fn delete_word_takes_the_word_before_the_cursor() {
    let mut draft = typed("one two  ");
    draft.delete_word();
    assert_eq!(shown(&draft), "one |");
    draft.delete_word();
    assert_eq!(shown(&draft), "|");
    // From inside a word, only its part before the cursor.
    let mut draft = typed("abc");
    draft.left();
    draft.delete_word();
    assert_eq!(shown(&draft), "|c");
}

#[test]
fn delete_word_stops_at_the_line_start_unless_it_is_there() {
    let mut draft = typed("one\n  ");
    draft.delete_word();
    assert_eq!(shown(&draft), "one\n|");
    draft.delete_word();
    assert_eq!(shown(&draft), "|");
    let mut draft = typed("one two\nthree");
    draft.line_start();
    draft.delete_word();
    assert_eq!(shown(&draft), "one |three");
}

#[test]
fn line_start_and_end_stay_on_the_logical_line() {
    let mut draft = typed("first\nsecond line\nthird");
    draft.up(80);
    assert_eq!(shown(&draft), "first\nsecon|d line\nthird");
    draft.line_start();
    assert_eq!(shown(&draft), "first\n|second line\nthird");
    draft.line_start();
    assert_eq!(shown(&draft), "first\n|second line\nthird");
    draft.line_end();
    assert_eq!(shown(&draft), "first\nsecond line|\nthird");
    draft.line_end();
    assert_eq!(shown(&draft), "first\nsecond line|\nthird");
    // A wrapped line is still one line.
    let mut draft = typed("abcdefghij");
    draft.line_start();
    assert_eq!(draft.cursor(6), (0, 2));
    draft.line_end();
    assert_eq!(shown(&draft), "abcdefghij|");
}

#[test]
fn a_paste_of_ten_lines_is_text() {
    let mut draft = typed("x");
    draft.paste(&lines(10));
    assert_eq!(draft.expand(), format!("x{}", lines(10)));
    // A trailing line break is no line.
    let mut draft = Draft::default();
    draft.paste(&format!("{}\n", lines(10)));
    assert_eq!(draft.expand(), format!("{}\n", lines(10)));
    assert_eq!(draft.rows(80).len(), 11);
}

#[test]
fn a_paste_of_eleven_lines_is_one_token() {
    let mut draft = typed("see ");
    let text = format!("{}\n", lines(11));
    draft.paste(&text);
    draft.insert('!');
    assert_eq!(draft.rows(80), vec!["> see [Pasted text #1 · 11 lines]!"]);
    assert_eq!(draft.expand(), format!("see {text}!"));
}

#[test]
fn a_paste_normalises_line_breaks_and_drops_control_characters() {
    let mut draft = Draft::default();
    draft.paste("a\r\nb\rc\u{1b}\td");
    assert_eq!(draft.expand(), "a\nb\nc\td");
    // Carriage returns count as line breaks for the threshold.
    let mut draft = Draft::default();
    draft.paste(&lines(11).replace('\n', "\r\n"));
    assert_eq!(draft.rows(80), vec!["> [Pasted text #1 · 11 lines]"]);
    let mut draft = Draft::default();
    draft.paste("");
    assert!(draft.is_empty());
}

#[test]
fn tokens_number_from_one_never_reuse_and_reset_on_clear() {
    let mut draft = Draft::default();
    draft.paste(&lines(11));
    draft.paste(&lines(12));
    assert_eq!(
        draft.rows(200),
        vec!["> [Pasted text #1 · 11 lines][Pasted text #2 · 12 lines]"]
    );
    draft.backspace();
    draft.paste(&lines(13));
    assert_eq!(
        draft.rows(200),
        vec!["> [Pasted text #1 · 11 lines][Pasted text #3 · 13 lines]"]
    );
    draft.clear();
    draft.paste(&lines(11));
    assert_eq!(draft.rows(200), vec!["> [Pasted text #1 · 11 lines]"]);
}

#[test]
fn a_token_is_one_unit_for_the_cursor_and_deletes() {
    let mut draft = typed("a");
    draft.paste(&lines(11));
    draft.insert('b');
    let token = lines(11);
    draft.left();
    draft.left();
    assert_eq!(shown(&draft), format!("a|{token}b"));
    draft.right();
    assert_eq!(shown(&draft), format!("a{token}|b"));
    draft.backspace();
    assert_eq!(shown(&draft), "a|b");
    let mut draft = typed("a");
    draft.paste(&lines(11));
    draft.left();
    draft.delete();
    assert_eq!(shown(&draft), "a|");
    // ⌥Backspace and word moves take a token alone.
    let mut draft = typed("word ");
    draft.paste(&lines(11));
    draft.word_left();
    assert_eq!(shown(&draft), format!("word |{token}"));
    draft.word_right();
    assert_eq!(shown(&draft), format!("word {token}|"));
    draft.delete_word();
    assert_eq!(shown(&draft), "word |");
}

#[test]
fn rows_wrap_at_the_width_with_the_prompt_prefix() {
    // Width 6 leaves 4 columns after `> `.
    let draft = typed("abcdefghij\nk");
    assert_eq!(draft.rows(6), vec!["> abcd", "  efgh", "  ij", "  k"]);
    assert_eq!(draft.cursor(6), (3, 3));
}

#[test]
fn a_row_exactly_the_width_puts_the_cursor_on_the_next_row() {
    let mut draft = typed("abcd");
    assert_eq!(draft.rows(6), vec!["> abcd", "  "]);
    assert_eq!(draft.cursor(6), (1, 2));
    // The rows do not depend on the cursor.
    draft.left();
    assert_eq!(draft.rows(6), vec!["> abcd", "  "]);
    assert_eq!(draft.cursor(6), (0, 5));
    // Before a line break, the same.
    let mut draft = typed("abcd\nx");
    draft.up(6);
    draft.line_end();
    assert_eq!(shown(&draft), "abcd|\nx");
    assert_eq!(draft.cursor(6), (1, 2));
    assert_eq!(draft.rows(6), vec!["> abcd", "  ", "  x"]);
}

#[test]
fn wide_characters_take_two_columns_and_wrap_whole() {
    let draft = typed("日本語");
    assert_eq!(draft.rows(7), vec!["> 日本", "  語"]);
    assert_eq!(draft.cursor(7), (1, 4));
    let mut draft = typed("a😀b");
    draft.left();
    assert_eq!(draft.cursor(80), (0, 5));
    let draft = typed("é");
    assert_eq!(draft.cursor(80), (0, 3));
}

#[test]
fn a_tab_shows_as_one_space() {
    let draft = typed("a\tb");
    assert_eq!(draft.rows(80), vec!["> a b"]);
    assert_eq!(draft.cursor(80), (0, 5));
}

#[test]
fn up_and_down_move_by_wrapped_row_keeping_the_column() {
    let mut draft = typed("abcdefghij");
    assert_eq!(draft.cursor(6), (2, 4));
    assert!(draft.up(6));
    assert_eq!(draft.cursor(6), (1, 4));
    assert_eq!(shown(&draft), "abcdef|ghij");
    assert!(draft.up(6));
    assert_eq!(shown(&draft), "ab|cdefghij");
    assert!(!draft.up(6));
    assert_eq!(shown(&draft), "ab|cdefghij");
    assert!(draft.down(6));
    assert!(draft.down(6));
    assert_eq!(shown(&draft), "abcdefghij|");
    assert!(!draft.down(6));
    assert_eq!(shown(&draft), "abcdefghij|");
}

#[test]
fn up_and_down_land_at_a_shorter_line_end() {
    let mut draft = typed("ab\nlonger");
    assert!(draft.up(80));
    assert_eq!(shown(&draft), "ab|\nlonger");
    assert!(draft.down(80));
    assert_eq!(shown(&draft), "ab\nlo|nger");
    assert!(!draft.down(80));
    draft.line_end();
    assert!(draft.up(80));
    assert_eq!(shown(&draft), "ab|\nlonger");
}

#[test]
fn an_empty_draft_is_on_its_first_and_last_row() {
    let mut draft = Draft::default();
    assert!(!draft.up(80));
    assert!(!draft.down(80));
    assert_eq!(draft.rows(80), vec!["> "]);
    assert_eq!(draft.cursor(80), (0, 2));
}

#[test]
fn a_token_wider_than_the_row_wraps_and_the_cursor_steps_over_it() {
    let mut draft = Draft::default();
    draft.paste(&lines(11));
    assert_eq!(
        draft.rows(12),
        vec!["> [Pasted te", "  xt #1 · 11", "   lines]"]
    );
    assert_eq!(draft.cursor(12), (2, 9));
    draft.left();
    assert_eq!(draft.cursor(12), (0, 2));
}
