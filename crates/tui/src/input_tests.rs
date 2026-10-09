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
fn a_character_wider_than_the_row_starts_no_empty_row() {
    // Width 3 leaves 1 column after `> `: a wide character at a row's
    // start stays on that row rather than wrapping below an empty one.
    let draft = typed("日");
    assert_eq!(draft.rows(3), vec!["> 日", "  "]);
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

#[test]
fn a_mention_is_the_word_after_its_at_while_the_cursor_is_in_it() {
    let mut draft = typed("a @src x");
    // The cursor at the end is past the query.
    assert_eq!(draft.mention(2), None);
    for _ in 0.." x".len() {
        draft.left();
    }
    assert_eq!(draft.mention(2), Some(("src".to_owned(), 6)));
    draft.left();
    draft.left();
    draft.left();
    assert_eq!(draft.position(), 3);
    assert_eq!(draft.mention(2), Some(("src".to_owned(), 6)));
    // On the `@` itself, or a piece that is no `@`, there is none.
    draft.left();
    assert_eq!(draft.mention(2), None);
    assert_eq!(draft.mention(0), None);
    // A token ends the query.
    let mut draft = typed("@ab");
    draft.paste(&lines(11));
    draft.left();
    assert_eq!(draft.mention(0), Some(("ab".to_owned(), 3)));
}

#[test]
fn after_space_is_true_at_the_start_and_after_whitespace_only() {
    let mut draft = Draft::default();
    assert!(draft.after_space());
    draft.insert('a');
    assert!(!draft.after_space());
    draft.line_break();
    assert!(draft.after_space());
    draft.paste(&lines(11));
    assert!(!draft.after_space());
}

#[test]
fn replace_and_set_type_the_text_with_the_cursor_after_it() {
    let mut draft = typed("x @ab y");
    draft.replace(2..5, "file ");
    assert_eq!(shown(&draft), "x file | y");
    // A range past the end is cut to it.
    draft.replace(8..99, "!");
    assert_eq!(shown(&draft), "x file  !|");
    draft.set("/new");
    assert_eq!(shown(&draft), "/new|");
}

#[test]
fn the_token_at_the_cursor_prefers_the_one_before_it() {
    // Tokens are named by their number.
    let mut draft = typed("a");
    assert_eq!(draft.token_at_cursor(), None);
    draft.paste(&lines(11));
    draft.paste(&lines(12));
    // Directly after #2, directly before nothing: #2.
    assert_eq!(draft.token_at_cursor(), Some(2));
    assert_eq!(draft.token_text(2), Some(lines(12).as_str()));
    // Between #1 and #2: the one before wins.
    draft.left();
    assert_eq!(draft.token_at_cursor(), Some(1));
    assert_eq!(draft.token_text(1), Some(lines(11).as_str()));
    // Before #1, after a character: the one after.
    draft.left();
    assert_eq!(draft.token_at_cursor(), Some(1));
    // At the start, before a character: none.
    draft.left();
    assert_eq!(draft.token_at_cursor(), None);
    assert_eq!(draft.token_text(0), None);
}

#[test]
fn new_token_text_keeps_its_number_and_counts_its_lines_again() {
    let mut draft = typed("a");
    draft.paste(&lines(11));
    draft.insert('b');
    draft.left();
    draft.set_token(1, &lines(15));
    assert_eq!(draft.rows(80), vec!["> a[Pasted text #1 · 15 lines]b"]);
    assert_eq!(shown(&draft), format!("a{}|b", lines(15)));
    // A token's text with \r line breaks reads as a paste does.
    draft.set_token(1, &lines(12).replace('\n', "\r\n"));
    assert_eq!(draft.expand(), format!("a{}b", lines(12)));
}

#[test]
fn token_text_of_ten_lines_or_fewer_goes_inline() {
    let mut draft = typed("a");
    draft.paste(&lines(11));
    draft.insert('b');
    draft.left();
    draft.set_token(1, "x\ny");
    assert_eq!(draft.expand(), "ax\nyb");
    assert_eq!(draft.token_at_cursor(), None);
    // The cursor stays after the text that replaced the token.
    assert_eq!(shown(&draft), "ax\ny|b");
    // A later token keeps its own number.
    draft.paste(&lines(11));
    assert_eq!(
        draft.rows(80).last().map(String::as_str),
        Some("  y[Pasted text #2 · 11 lines]b")
    );
    // A cursor further on stays after the same text.
    let mut draft = typed("ab");
    draft.left();
    draft.paste(&lines(11));
    draft.right();
    draft.set_token(1, "z");
    assert_eq!(shown(&draft), "azb|");
    // A position with no token changes nothing.
    draft.set_token(0, "q");
    assert_eq!(shown(&draft), "azb|");
}

#[test]
fn ten_lines_go_inline_and_a_cursor_before_the_token_stays_put() {
    let mut draft = typed("ab");
    draft.left();
    draft.paste(&lines(11));
    draft.left();
    // The cursor is directly before the token.
    draft.set_token(1, &lines(10));
    assert_eq!(shown(&draft), format!("a|{}b", lines(10)));
    assert_eq!(draft.token_at_cursor(), None);
    // Eleven lines stay a token.
    let mut draft = typed("a");
    draft.paste(&lines(12));
    draft.set_token(1, &lines(11));
    assert_eq!(draft.rows(80), vec!["> a[Pasted text #1 · 11 lines]"]);
}

#[test]
fn two_tokens_on_one_row_are_two_spans() {
    let lines = |n: usize| {
        (1..=n)
            .map(|at| format!("l{at}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut draft = Draft::default();
    draft.paste(&lines(11));
    draft.paste(&lines(12));
    let spans: Vec<(usize, usize, u16, u16)> = draft
        .token_spans(80)
        .iter()
        .map(|span| (span.number, span.row, span.start, span.end))
        .collect();
    // "> [Pasted text #1 · 11 lines][Pasted text #2 · 12 lines]"
    assert_eq!(spans, vec![(1, 0, 2, 29), (2, 0, 29, 56)]);
}

/// Tests for images in the draft: content order, labels, editing, serials
/// and the editor round trip (`docs/tui.md`, "The input box").
use std::sync::Arc;

use contract::commands::SentPart;

/// A draft holding `text`, then the image `data`, then `more`.
fn with_image(text: &str, data: &str, more: &str) -> Draft {
    let mut draft = typed(text);
    draft.insert_image(Arc::from(data));
    for ch in more.chars() {
        draft.insert(ch);
    }
    draft
}

#[test]
fn content_keeps_the_order_of_text_and_images() {
    // I1: text, image, text; two adjacent images hold no empty text part;
    // a paste token beside an image joins its text run.
    let draft = with_image("a", "AAA", "bc");
    let mut two = Draft::default();
    two.insert_image(Arc::from("AAA"));
    two.insert_image(Arc::from("BBB"));
    assert_eq!(
        draft.content(),
        vec![
            SentPart::Text {
                text: "a".to_owned()
            },
            SentPart::Image {
                data: "AAA".to_owned(),
                mime_type: "image/png".to_owned()
            },
            SentPart::Text {
                text: "bc".to_owned()
            },
        ]
    );
    assert_eq!(
        two.content(),
        vec![
            SentPart::Image {
                data: "AAA".to_owned(),
                mime_type: "image/png".to_owned()
            },
            SentPart::Image {
                data: "BBB".to_owned(),
                mime_type: "image/png".to_owned()
            },
        ]
    );
    let mut token = Draft::default();
    token.paste(&lines(11));
    token.insert_image(Arc::from("AAA"));
    assert_eq!(token.content().len(), 2);
    assert!(matches!(
        token.content().first(),
        Some(SentPart::Text { .. })
    ));
}

#[test]
fn an_image_alone_is_one_part_and_an_empty_draft_has_no_parts() {
    // I2.
    let mut draft = Draft::default();
    draft.insert_image(Arc::from("AAA"));
    assert_eq!(
        draft.content(),
        vec![SentPart::Image {
            data: "AAA".to_owned(),
            mime_type: "image/png".to_owned()
        }]
    );
    assert_eq!(Draft::default().content(), Vec::new());
    assert!(draft.has_image());
    assert!(!typed("ab").has_image());
}

#[test]
fn labels_count_images_in_order() {
    // I3: deleting the first image renumbers the second to #1; a paste
    // token keeps its fixed number beside them.
    let mut draft = with_image("a", "AAA", "b");
    draft.insert_image(Arc::from("BBB"));
    assert_eq!(draft.rows(80), vec!["> a[Image #1]b[Image #2]"]);
    // Cursor at the end: two lefts stand between the images, backspace
    // drops the first whole.
    draft.left();
    draft.left();
    draft.backspace();
    assert_eq!(draft.rows(80), vec!["> ab[Image #1]"]);
    assert_eq!(draft.expand(), "ab[Image #1]");
    let mut token = Draft::default();
    token.paste(&lines(11));
    token.insert_image(Arc::from("AAA"));
    assert_eq!(
        token.rows(200),
        vec!["> [Pasted text #1 · 11 lines][Image #1]"]
    );
}

#[test]
fn backspace_and_delete_remove_an_image_whole() {
    // I4.
    let mut draft = with_image("a", "AAA", "b");
    draft.left();
    draft.backspace();
    assert_eq!(draft.expand(), "ab");
    let mut draft = with_image("a", "AAA", "b");
    draft.left();
    draft.left();
    draft.delete();
    assert_eq!(draft.expand(), "ab");
}

#[test]
fn word_moves_step_over_an_image() {
    // I5: an image is one word.
    let mut draft = with_image("ab", "AAA", "cd");
    assert_eq!(draft.position(), 5);
    draft.word_left();
    assert_eq!(draft.position(), 3);
    draft.word_left();
    assert_eq!(draft.position(), 2);
    draft.word_left();
    assert_eq!(draft.position(), 0);
    draft.word_right();
    assert_eq!(draft.position(), 2);
    draft.word_right();
    assert_eq!(draft.position(), 3);
    draft.word_right();
    assert_eq!(draft.position(), 5);
    // Deleting back over it drops the word before it, whole.
    let mut draft = with_image("ab", "AAA", "cd");
    draft.word_left();
    draft.delete_word();
    assert_eq!(draft.expand(), "abcd");
}

#[test]
fn at_after_an_image_opens_no_file_panel() {
    // I6.
    let mut draft = Draft::default();
    draft.insert_image(Arc::from("AAA"));
    assert!(!draft.after_space());
    assert!(typed("a ").after_space());
}

#[test]
fn token_spans_leave_images_out() {
    // I7.
    let mut draft = Draft::default();
    draft.paste(&lines(11));
    draft.insert_image(Arc::from("AAA"));
    let spans = draft.token_spans(200);
    assert!(!spans.is_empty());
    assert!(spans.iter().all(|span| span.number == 1));
    assert_eq!(
        draft.rows(200),
        vec!["> [Pasted text #1 · 11 lines][Image #1]"]
    );
    // An image beside the cursor is no token: Ctrl+G opens the draft.
    assert_eq!(draft.token_at_cursor(), None);
}

#[test]
fn edits_and_moves_renew_the_serial() {
    // I8: clear, set, edited and mem::take give new serials; put_back gives
    // a serial differing from both drafts', keeps pieces and images, and
    // puts the cursor at the end.
    let mut draft = with_image("ab", "AAA", "");
    let first = draft.serial();
    draft.clear();
    assert_ne!(draft.serial(), first);
    let cleared = draft.serial();
    draft.set("xy");
    assert_ne!(draft.serial(), cleared);
    let set = draft.serial();
    draft.edited("xy");
    assert_ne!(draft.serial(), set);
    let before = draft.serial();
    let taken = std::mem::take(&mut draft);
    assert_ne!(draft.serial(), before);
    assert_ne!(taken.serial(), draft.serial());
    let mut back = with_image("ab", "AAA", "cd");
    let back_serial = back.serial();
    let moved = std::mem::take(&mut back);
    let mut box_draft = typed("xy");
    let box_serial = box_draft.serial();
    box_draft.put_back(moved);
    assert_ne!(box_draft.serial(), back_serial);
    assert_ne!(box_draft.serial(), box_serial);
    assert_eq!(box_draft.expand(), "ab[Image #1]cd");
    assert_eq!(box_draft.position(), 5);
    assert!(box_draft.has_image());
}

#[test]
fn the_editor_return_relinks_image_labels() {
    // I9.
    let draft = with_image("look ", "AAA", " here");
    // Kept in place.
    let mut kept = with_image("look ", "AAA", " here");
    kept.edited("look [Image #1] here");
    assert_eq!(kept.content(), draft.content());
    // Moved.
    let mut moved = with_image("look ", "AAA", " here");
    moved.edited("here [Image #1] look");
    assert_eq!(
        moved.content(),
        vec![
            SentPart::Text {
                text: "here ".to_owned()
            },
            SentPart::Image {
                data: "AAA".to_owned(),
                mime_type: "image/png".to_owned()
            },
            SentPart::Text {
                text: " look".to_owned()
            },
        ]
    );
    // Deleted with its label.
    let mut dropped = with_image("look ", "AAA", " here");
    dropped.edited("look here");
    assert!(!dropped.has_image());
    assert_eq!(dropped.expand(), "look here");
    // A repeated label relinks once; the second stays text.
    let mut twice = with_image("", "AAA", "");
    twice.edited("[Image #1] and [Image #1]");
    assert_eq!(
        twice.content(),
        vec![
            SentPart::Image {
                data: "AAA".to_owned(),
                mime_type: "image/png".to_owned()
            },
            SentPart::Text {
                text: " and [Image #1]".to_owned()
            },
        ]
    );
    // Out of range on both sides stays text.
    let mut bounds = with_image("", "AAA", "");
    bounds.insert_image(Arc::from("BBB"));
    bounds.edited("[Image #0] x [Image #3]");
    assert!(!bounds.has_image());
    assert_eq!(bounds.expand(), "[Image #0] x [Image #3]");
}

#[test]
fn expand_shows_an_image_as_its_label() {
    // I10.
    assert_eq!(
        with_image("look ", "AAA", " here").expand(),
        "look [Image #1] here"
    );
}
