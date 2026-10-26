//! Tests for the question form: keys, digits, paste, the answer, its
//! panel lines with their styled runs, and forms in the queue.

use ratatui::style::{Modifier, Style};
use serde_json::{Value, json};

use super::{Form, Spot};
use crate::approvals::{Panel, PanelKey, Queue, Title};
use crate::keys::{Edit, Key};
use crate::theme::{Role, style};

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

/// In secondary.
fn secondary() -> Style {
    style(Role::Secondary)
}

/// Bold in secondary.
fn secondary_bold() -> Style {
    style(Role::Secondary).add_modifier(Modifier::BOLD)
}

/// In accent.
fn accent() -> Style {
    style(Role::Accent)
}

/// Accent and reversed.
fn accent_reversed() -> Style {
    style(Role::Accent).add_modifier(Modifier::REVERSED)
}

/// Dim.
fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// Bold.
fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// Reversed.
fn reversed() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// Underlined.
fn underlined() -> Style {
    Style::default().add_modifier(Modifier::UNDERLINED)
}

/// Bold and underlined.
fn bold_underlined() -> Style {
    Style::default()
        .add_modifier(Modifier::BOLD)
        .add_modifier(Modifier::UNDERLINED)
}

/// Dim and reversed: the placeholder's first character with the cursor on
/// its row.
fn first() -> Style {
    Style::default()
        .add_modifier(Modifier::DIM)
        .add_modifier(Modifier::REVERSED)
}

/// " Chat about this " on the Submit tab: on the rule tint.
fn chat() -> Style {
    Style::new().bg(Role::Rule.color())
}

/// A form over `fields`, parsed as the line carries them.
fn form(fields: Value) -> Form {
    Form::new(serde_json::from_value(fields).unwrap_or_default())
}

/// A `Base` with two options, multi-choice when `multi`, then a free-text
/// `Name`.
fn base_and_name(multi: bool) -> Form {
    form(json!([
        {"header": "Base", "question": "Which branch?", "multiSelect": multi, "options": [
            {"label": "main (Recommended)", "description": "the default"},
            {"label": "dev"}]},
        {"header": "Name", "question": "What name?"}
    ]))
}

/// One multi-choice question with three options.
fn three_options() -> Form {
    form(json!([
        {"header": "Pick", "question": "Which?", "multiSelect": true,
         "options": [{"label": "a"}, {"label": "b"}, {"label": "c"}]}
    ]))
}

/// A single-choice question with `n` options labelled `o1`..`on`.
fn n_options(n: usize, multi: bool) -> Form {
    form(json!([
        {"header": "Pick", "question": "Which?", "multiSelect": multi,
         "options": (1..=n).map(|i| json!({"label": format!("o{i}")})).collect::<Vec<_>>()}
    ]))
}

/// The title for a form with `questions` questions from `main`, nothing
/// else waiting.
fn title(questions: usize) -> Title {
    Title {
        session: "main".to_owned(),
        place: None,
        questions: Some(questions),
    }
}

/// The panel at 80 columns.
fn panel(form: &Form) -> Panel {
    form.panel(&title(form.question_count()), 80)
}

/// The lines under the title.
fn lines(form: &Form) -> Vec<String> {
    panel(form).lines
}

/// The line at `row`.
fn line(form: &Form, row: usize) -> String {
    lines(form).get(row).cloned().unwrap_or_default()
}

/// The tab line.
fn tabs(form: &Form) -> String {
    line(form, 2)
}

/// The line the cursor is on.
fn cursor_line(form: &Form) -> String {
    let shown = panel(form);
    shown
        .cursor
        .and_then(|at| shown.lines.get(at).cloned())
        .unwrap_or_default()
}

/// Presses each key, each handled by the form.
fn press(form: &mut Form, keys: &[Key]) {
    for key in keys {
        assert_eq!(form.on_key(key), Some(PanelKey::Handled), "{key:?}");
    }
}

/// Types `text` one key at a time.
fn type_text(form: &mut Form, text: &str) {
    for ch in text.chars() {
        press(form, &[Key::Char(ch)]);
    }
}

/// The reply's answer as JSON.
fn answer(form: &Form) -> Value {
    serde_json::to_value(form.answer()).unwrap_or_default()
}

#[test]
fn the_first_question_shows_its_options_words_and_next() {
    let form = base_and_name(true);
    assert_eq!(
        lines(&form),
        [
            "Question from main · 2 questions, any can be skipped        Esc: chat about this",
            "",
            "←  ☐ Base    ☐ Name    ✔ Submit  →",
            "",
            "Which branch?",
            "choose any",
            "",
            "▸ 1 [ ] main (Recommended)  the default",
            "  2 [ ] dev",
            "  3 ✎ Type an answer, alone or with the options",
            "  Next →",
            "",
            "←→ question · ↑↓ choose · Enter chooses and moves on · Space toggles · type to answer in words",
        ]
    );
}

#[test]
fn a_single_choice_question_has_no_choose_any_and_no_next() {
    let form = base_and_name(false);
    assert_eq!(
        lines(&form),
        [
            "Question from main · 2 questions, any can be skipped        Esc: chat about this",
            "",
            "←  ☐ Base    ☐ Name    ✔ Submit  →",
            "",
            "Which branch?",
            "",
            "▸ 1 ( ) main (Recommended)  the default",
            "  2 ( ) dev",
            "  3 ✎ Type an answer, alone or with the options",
            "",
            "←→ question · ↑↓ choose · Enter chooses and moves on · type to answer in words",
        ]
    );
}

#[test]
fn enter_on_a_single_choice_option_chooses_it_and_moves_on() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Down, Key::Enter]);
    assert_eq!(tabs(&form), "←  ✔ Base    ☐ Name    ✔ Submit  →");
    assert_eq!(cursor_line(&form), "▸ 1 ✎ Type an answer");
    assert_eq!(answer(&form)["answers"][0], json!({"labels": ["dev"]}));
    // Enter on another option replaces the choice.
    press(&mut form, &[Key::BackTab, Key::Enter]);
    assert_eq!(
        answer(&form)["answers"][0],
        json!({"labels": ["main (Recommended)"]})
    );
    press(&mut form, &[Key::BackTab]);
    assert_eq!(line(&form, 6), "▸ 1 (•) main (Recommended)  the default");
    assert_eq!(line(&form, 7), "  2 ( ) dev");
}

#[test]
fn space_toggles_a_multi_choice_option_and_enter_moves_on_without_toggling() {
    let mut form = three_options();
    press(&mut form, &[Key::Char(' ')]);
    assert_eq!(cursor_line(&form), "▸ 1 [x] a");
    press(&mut form, &[Key::Char(' ')]);
    assert_eq!(cursor_line(&form), "▸ 1 [ ] a");
    // Toggled out of order, the labels come back in option order.
    press(&mut form, &[Key::Down, Key::Down, Key::Char(' ')]);
    press(&mut form, &[Key::Up, Key::Up, Key::Char(' ')]);
    press(&mut form, &[Key::Down, Key::Enter]);
    // Enter moves on to Submit without toggling: the cursor lands on the
    // buttons row.
    assert!(cursor_line(&form).starts_with(" Submit "));
    assert_eq!(answer(&form)["answers"], json!([{"labels": ["a", "c"]}]));
}

#[test]
fn space_on_a_single_choice_option_chooses_it_and_stays_then_clears_it() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Down, Key::Char(' ')]);
    assert_eq!(cursor_line(&form), "▸ 2 (•) dev");
    assert_eq!(answer(&form)["answers"][0], json!({"labels": ["dev"]}));
    // Space on another option moves the choice.
    press(&mut form, &[Key::Up, Key::Char(' ')]);
    assert_eq!(
        answer(&form)["answers"][0],
        json!({"labels": ["main (Recommended)"]})
    );
    press(&mut form, &[Key::Char(' ')]);
    assert_eq!(
        cursor_line(&form),
        "▸ 1 ( ) main (Recommended)  the default"
    );
    assert_eq!(answer(&form)["answers"][0], json!({"skipped": true}));
}

#[test]
fn a_digit_on_a_multi_choice_option_toggles_it_and_moves_the_cursor() {
    let mut form = three_options();
    assert_eq!(form.on_key(&Key::Char('3')), Some(PanelKey::Handled));
    assert_eq!(cursor_line(&form), "▸ 3 [x] c");
    assert_eq!(answer(&form)["answers"], json!([{"labels": ["c"]}]));
    // The tab stays: a digit never moves on.
    assert_eq!(tabs(&form), "←  ✔ Pick    ✔ Submit  →");
    assert_eq!(form.on_key(&Key::Char('1')), Some(PanelKey::Handled));
    assert_eq!(cursor_line(&form), "▸ 1 [x] a");
    assert_eq!(answer(&form)["answers"], json!([{"labels": ["a", "c"]}]));
}

#[test]
fn a_digit_on_a_single_choice_option_sets_the_pick_without_moving_on() {
    let mut form = base_and_name(false);
    assert_eq!(form.on_key(&Key::Char('2')), Some(PanelKey::Handled));
    assert_eq!(cursor_line(&form), "▸ 2 (•) dev");
    assert_eq!(answer(&form)["answers"][0], json!({"labels": ["dev"]}));
    assert!(tabs(&form).starts_with("←  ✔ Base"));
    // Pressing the same digit again keeps it chosen.
    assert_eq!(form.on_key(&Key::Char('2')), Some(PanelKey::Handled));
    assert_eq!(cursor_line(&form), "▸ 2 (•) dev");
    assert_eq!(answer(&form)["answers"][0], json!({"labels": ["dev"]}));
    assert_eq!(form.on_key(&Key::Char('1')), Some(PanelKey::Handled));
    assert_eq!(
        answer(&form)["answers"][0],
        json!({"labels": ["main (Recommended)"]})
    );
}

#[test]
fn a_digit_on_the_next_row_chooses() {
    let mut form = three_options();
    press(&mut form, &[Key::Down, Key::Down, Key::Down, Key::Down]);
    assert_eq!(cursor_line(&form), "▸ Review →");
    assert_eq!(form.on_key(&Key::Char('2')), Some(PanelKey::Handled));
    assert_eq!(cursor_line(&form), "▸ 2 [x] b");
    assert_eq!(answer(&form)["answers"], json!([{"labels": ["b"]}]));
}

#[test]
fn a_digit_on_the_words_row_is_typed() {
    let mut form = three_options();
    press(&mut form, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(form.on_key(&Key::Char('2')), Some(PanelKey::Handled));
    assert_eq!(cursor_line(&form), "▸ 4 ✎ 2");
    assert_eq!(
        answer(&form)["answers"],
        json!([{"labels": [], "text": "2"}])
    );
}

#[test]
fn a_digit_on_the_submit_note_is_typed() {
    let mut form = three_options();
    press(&mut form, &[Key::Tab]);
    assert_eq!(form.on_key(&Key::Char('5')), Some(PanelKey::Handled));
    assert_eq!(cursor_line(&form), "▸ note 5");
    assert_eq!(answer(&form)["note"], json!("5"));
}

#[test]
fn digits_reach_only_the_first_nine_rows() {
    // Eight options: '8' toggles the eighth, '9' lands on the words row and
    // types nothing.
    let mut form = n_options(8, true);
    assert_eq!(form.on_key(&Key::Char('8')), Some(PanelKey::Handled));
    assert_eq!(cursor_line(&form), "▸ 8 [x] o8");
    assert_eq!(form.on_key(&Key::Char('9')), Some(PanelKey::Handled));
    assert_eq!(
        cursor_line(&form),
        "▸ 9 ✎ Type an answer, alone or with the options"
    );
    assert_eq!(answer(&form)["answers"], json!([{"labels": ["o8"]}]));
    // Nine options: '9' toggles the ninth.
    let mut form = n_options(9, true);
    assert_eq!(form.on_key(&Key::Char('9')), Some(PanelKey::Handled));
    assert_eq!(cursor_line(&form), "▸ 9 [x] o9");
    // Ten options: '9' toggles the ninth, '0' does nothing.
    let mut form = n_options(10, true);
    assert_eq!(form.on_key(&Key::Char('9')), Some(PanelKey::Handled));
    assert_eq!(cursor_line(&form), "▸ 9 [x] o9");
    let before = lines(&form);
    assert_eq!(form.on_key(&Key::Char('0')), Some(PanelKey::Handled));
    assert_eq!(lines(&form), before);
    // Past the words row a digit does nothing: options, words, beyond.
    let mut form = n_options(2, true);
    assert_eq!(form.on_key(&Key::Char('2')), Some(PanelKey::Handled));
    assert_eq!(cursor_line(&form), "▸ 2 [x] o2");
    assert_eq!(form.on_key(&Key::Char('3')), Some(PanelKey::Handled));
    assert_eq!(
        cursor_line(&form),
        "▸ 3 ✎ Type an answer, alone or with the options"
    );
    press(&mut form, &[Key::Up]);
    let before = lines(&form);
    assert_eq!(form.on_key(&Key::Char('4')), Some(PanelKey::Handled));
    assert_eq!(cursor_line(&form), "▸ 2 [x] o2");
    assert_eq!(lines(&form), before);
}

#[test]
fn a_character_off_the_words_row_is_typed_on_it() {
    // An option and `Next →`.
    for downs in [0, 1, 3] {
        let mut form = base_and_name(true);
        press(&mut form, &vec![Key::Down; downs]);
        type_text(&mut form, "x");
        assert_eq!(cursor_line(&form), "▸ 3 ✎ x", "{downs}");
        assert_eq!(
            answer(&form)["answers"][0],
            json!({"labels": [], "text": "x"})
        );
    }
}

#[test]
fn space_on_next_does_nothing_and_on_the_words_row_types() {
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Down, Key::Down, Key::Down]);
    let before = lines(&form);
    press(&mut form, &[Key::Char(' ')]);
    assert_eq!(lines(&form), before);
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Down, Key::Down]);
    type_text(&mut form, "a b");
    assert_eq!(cursor_line(&form), "▸ 3 ✎ a b");
}

#[test]
fn backspace_deletes_on_the_words_row_only() {
    let mut form = base_and_name(true);
    type_text(&mut form, "ab");
    press(&mut form, &[Key::Backspace]);
    assert_eq!(cursor_line(&form), "▸ 3 ✎ a");
    press(&mut form, &[Key::Up, Key::Backspace]);
    assert_eq!(line(&form, 9), "  3 ✎ a");
    press(&mut form, &[Key::Down, Key::Down, Key::Backspace]);
    assert_eq!(line(&form, 9), "  3 ✎ a");
}

#[test]
fn up_and_down_stop_at_the_first_and_last_rows() {
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Up]);
    assert_eq!(
        cursor_line(&form),
        "▸ 1 [ ] main (Recommended)  the default"
    );
    press(&mut form, &[Key::Down, Key::Down]);
    assert_eq!(
        cursor_line(&form),
        "▸ 3 ✎ Type an answer, alone or with the options"
    );
    press(&mut form, &[Key::Down, Key::Down]);
    assert_eq!(cursor_line(&form), "▸ Next →");
    press(&mut form, &[Key::Tab, Key::Tab]);
    press(&mut form, &[Key::Up]);
    assert_eq!(cursor_line(&form), "▸ note Add a note on the whole form");
    press(&mut form, &[Key::Down, Key::Down]);
    let shown = panel(&form);
    assert_eq!(shown.cursor, Some(8));
    assert!(shown.lines[8].starts_with(" Submit "));
}

#[test]
fn tab_and_shift_tab_move_between_tabs_and_stop_at_the_ends() {
    let mut form = base_and_name(true);
    press(&mut form, &[Key::BackTab]);
    assert_eq!(tabs(&form), "←  ☐ Base    ☐ Name    ✔ Submit  →");
    // From any row, onto the next tab's landing row: the free-text Name
    // lands on its words row, numbered 1 with no options.
    press(&mut form, &[Key::Down, Key::Down, Key::Tab]);
    assert_eq!(line(&form, 4), "What name?");
    assert_eq!(cursor_line(&form), "▸ 1 ✎ Type an answer");
    press(&mut form, &[Key::Tab]);
    let shown = panel(&form);
    assert_eq!(shown.cursor, Some(8));
    assert!(shown.lines[8].starts_with(" Submit "));
    press(&mut form, &[Key::Tab]);
    assert_eq!(panel(&form).cursor, Some(8));
    press(&mut form, &[Key::BackTab]);
    assert_eq!(cursor_line(&form), "▸ 1 ✎ Type an answer");
    press(&mut form, &[Key::BackTab]);
    assert_eq!(
        cursor_line(&form),
        "▸ 1 [ ] main (Recommended)  the default"
    );
}

#[test]
fn a_free_text_question_lands_on_its_words_row_and_has_no_options() {
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Tab]);
    assert_eq!(
        lines(&form),
        [
            "Question from main · 2 questions, any can be skipped        Esc: chat about this",
            "",
            "←  ☐ Base    ☐ Name    ✔ Submit  →",
            "",
            "What name?",
            "",
            "▸ 1 ✎ Type an answer",
            "",
            "←→ question · ↑↓ choose · Enter chooses and moves on · type to answer in words",
        ]
    );
}

#[test]
fn the_last_question_reads_review_and_enter_on_it_lands_on_submit() {
    let mut form = three_options();
    press(&mut form, &[Key::Down, Key::Down, Key::Down, Key::Down]);
    assert_eq!(cursor_line(&form), "▸ Review →");
    press(&mut form, &[Key::Enter]);
    let shown = panel(&form);
    assert_eq!(shown.cursor, Some(7));
    assert!(shown.lines[7].starts_with(" Submit "));
    assert_eq!(shown.lines[2], "Review");
}

#[test]
fn the_answer_sends_labels_text_or_skipped_and_the_note() {
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Char(' '), Key::Down, Key::Char(' ')]);
    type_text(&mut form, "and tags");
    press(&mut form, &[Key::Enter, Key::Enter]);
    type_text(&mut form, "by friday");
    assert_eq!(
        answer(&form),
        json!({"answers": [{"labels": ["main (Recommended)", "dev"], "text": "and tags"},
            {"skipped": true}], "note": "by friday"})
    );
    // Label only, text only.
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Enter]);
    type_text(&mut form, "fiber-cli");
    assert_eq!(
        answer(&form),
        json!({"answers": [{"labels": ["main (Recommended)"]},
            {"labels": [], "text": "fiber-cli"}]})
    );
}

#[test]
fn blank_words_and_a_blank_note_are_not_sent() {
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Down, Key::Down]);
    type_text(&mut form, "  ");
    press(&mut form, &[Key::Tab, Key::Tab]);
    type_text(&mut form, " ");
    assert_eq!(
        answer(&form),
        json!({"answers": [{"skipped": true}, {"skipped": true}]})
    );
    // Words that are not blank go exactly as typed.
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Down, Key::Down]);
    type_text(&mut form, " x ");
    assert_eq!(
        answer(&form)["answers"][0],
        json!({"labels": [], "text": " x "})
    );
}

#[test]
fn a_multi_choice_visited_and_left_empty_is_skipped() {
    let mut form = three_options();
    press(&mut form, &[Key::Char(' '), Key::Char(' '), Key::Enter]);
    assert_eq!(answer(&form), json!({"answers": [{"skipped": true}]}));
    assert!(cursor_line(&form).starts_with(" Submit "));
}

#[test]
fn a_tab_is_marked_when_its_answer_is_not_skipped() {
    let mut form = base_and_name(true);
    assert!(tabs(&form).starts_with("←  ☐ Base"));
    press(&mut form, &[Key::Char(' ')]);
    assert!(tabs(&form).starts_with("←  ✔ Base"));
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "x");
    assert!(tabs(&form).contains("✔ Name"));
    press(&mut form, &[Key::Backspace]);
    assert!(!tabs(&form).contains("✔ Name"));
}

#[test]
fn the_submit_tab_shows_every_answer_and_takes_a_note() {
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Char(' '), Key::Down, Key::Char(' ')]);
    type_text(&mut form, "and tags");
    press(&mut form, &[Key::Tab, Key::Tab]);
    assert_eq!(
        lines(&form),
        [
            "Question from main · 2 questions, any can be skipped        Esc: chat about this",
            "",
            "Review",
            "",
            "Base         main (Recommended), dev, \"and tags\"",
            "Name         skipped",
            "  note Add a note on the whole form",
            "",
            " Submit ",
            " Chat about this ",
            "  declines and ends the turn, so you can answer in your own words",
            "",
            "←→ question · Enter submits · type to add the note · Esc: chat about this",
        ]
    );
    // A character on `Send` goes to the note, Space included.
    type_text(&mut form, "by");
    press(&mut form, &[Key::Down, Key::Down, Key::Char(' ')]);
    type_text(&mut form, "x");
    assert_eq!(cursor_line(&form), "▸ note by x");
    press(&mut form, &[Key::Backspace]);
    assert_eq!(cursor_line(&form), "▸ note by ");
    // Backspace off the note row does nothing.
    press(&mut form, &[Key::Down, Key::Backspace]);
    assert_eq!(line(&form, 6), "  note by ");
}

#[test]
fn enter_on_the_note_and_on_submit_answers() {
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Tab, Key::Tab]);
    assert_eq!(form.on_key(&Key::Enter), Some(PanelKey::Answer));
    press(&mut form, &[Key::Up]);
    assert_eq!(form.on_key(&Key::Enter), Some(PanelKey::Answer));
}

#[test]
fn esc_declines_from_any_row() {
    let mut form = base_and_name(true);
    assert_eq!(form.on_key(&Key::Esc), Some(PanelKey::Decline));
    press(&mut form, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(form.on_key(&Key::Enter), Some(PanelKey::Handled));
    assert_eq!(form.on_key(&Key::Esc), Some(PanelKey::Decline));
    press(&mut form, &[Key::Tab, Key::Tab, Key::Up]);
    assert_eq!(cursor_line(&form), "▸ note Add a note on the whole form");
    assert_eq!(form.on_key(&Key::Esc), Some(PanelKey::Decline));
}

#[test]
fn every_key_is_the_forms_or_passes_through() {
    let handled = Some(PanelKey::Handled);
    let keys = [
        (Key::Char('x'), handled),
        (Key::Char('2'), handled),
        (Key::Char('0'), handled),
        (Key::Backspace, handled),
        (Key::Enter, handled),
        (Key::Esc, Some(PanelKey::Decline)),
        (Key::CtrlC, None),
        (Key::CtrlO, None),
        (Key::PageUp, None),
        (Key::PageDown, None),
        (Key::End, None),
        (Key::Up, handled),
        (Key::Down, handled),
        (Key::AltA, handled),
        (Key::Tab, handled),
        (Key::BackTab, handled),
        (Key::F1, None),
        (Key::CtrlG, handled),
        (Key::CtrlR, handled),
        (Key::CtrlF, None),
        (Key::AltUp, handled),
        (Key::AltDown, handled),
        (Key::AltX, handled),
        (Key::AltP, None),
        (Key::AltR, None),
        (Key::AltDigit(1), None),
    ];
    for (key, expected) in keys {
        let mut form = base_and_name(true);
        let before = lines(&form);
        assert_eq!(form.on_key(&key), expected, "{key:?}");
        // The keys that do nothing on the form leave it as it was.
        let inert = matches!(
            key,
            Key::Char('0')
                | Key::AltA
                | Key::CtrlG
                | Key::CtrlR
                | Key::AltUp
                | Key::AltDown
                | Key::AltX
        );
        if inert || expected.is_none() {
            assert_eq!(lines(&form), before, "{key:?}");
        }
    }
}

#[test]
fn a_paste_is_typed_on_the_words_row_or_into_the_note() {
    let mut form = three_options();
    form.on_edit(&Edit::Paste(" a\nb".to_owned()));
    assert_eq!(cursor_line(&form), "▸ 4 ✎  a b");
    assert_eq!(line(&form, 7), "  1 [ ] a");
    press(&mut form, &[Key::Tab]);
    form.on_edit(&Edit::Paste("by\tnow".to_owned()));
    assert_eq!(cursor_line(&form), "▸ note by now");
}

#[test]
fn a_paste_of_digits_chooses_nothing() {
    let mut form = three_options();
    form.on_edit(&Edit::Paste("2".to_owned()));
    assert_eq!(cursor_line(&form), "▸ 4 ✎ 2");
    assert_eq!(
        answer(&form)["answers"],
        json!([{"labels": [], "text": "2"}])
    );
    let mut form = base_and_name(false);
    form.on_edit(&Edit::Paste("1".to_owned()));
    assert_eq!(
        answer(&form)["answers"][0],
        json!({"labels": [], "text": "1"})
    );
}

#[test]
fn every_other_edit_does_nothing() {
    let edits = [
        Edit::ShiftEnter,
        Edit::CtrlJ,
        Edit::WordLeft,
        Edit::WordRight,
        Edit::DeleteWord,
        Edit::LineStart,
        Edit::LineEnd,
        Edit::Delete,
    ];
    for edit in edits {
        let mut form = base_and_name(true);
        type_text(&mut form, "ab");
        let before = lines(&form);
        form.on_edit(&edit);
        assert_eq!(lines(&form), before, "{edit:?}");
    }
}

#[test]
fn control_characters_in_model_text_draw_as_spaces() {
    let form = form(json!([
        {"header": "Pi\nck", "question": "Which\rone?", "options": [
            {"label": "a\tb", "description": "x\u{1b}[2J"},
            {"label": "c\u{85}d"}]}
    ]));
    let shown = form.panel(&title(1), 80);
    assert_eq!(
        shown.lines,
        [
            "Question from main · 1 questions, any can be skipped        Esc: chat about this",
            "",
            "←  ☐ Pi ck    ✔ Submit  →",
            "",
            "Which one?",
            "",
            "▸ 1 ( ) a b  x [2J",
            "  2 ( ) c d",
            "  3 ✎ Type an answer, alone or with the options",
            "",
            "←→ question · ↑↓ choose · Enter chooses and moves on · type to answer in words",
        ]
    );
}

#[test]
fn an_empty_form_opens_on_submit_and_answers_nothing() {
    let mut form = form(json!([]));
    let shown = form.panel(&title(0), 80);
    assert_eq!(
        shown.lines,
        [
            "Question from main · 0 questions, any can be skipped        Esc: chat about this",
            "",
            "Review",
            "",
            "  note Add a note on the whole form",
            "",
            " Submit ",
            " Chat about this ",
            "  declines and ends the turn, so you can answer in your own words",
            "",
            "←→ question · Enter submits · type to add the note · Esc: chat about this",
        ]
    );
    assert_eq!(form.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(answer(&form), json!({"answers": []}));
}

/// The doc's mock at width 100: session `main`, three questions, hub
/// chosen on the first.
fn mocked() -> (Form, Title) {
    let mut form = form(json!([
        {"header": "Scope", "question": "Which crates should the change touch?",
         "multiSelect": true, "options": [
            {"label": "hub", "description": "the session host"},
            {"label": "tui", "description": "the terminal"}]},
        {"header": "Tests", "question": "Which tests cover it?",
         "options": [{"label": "unit"}, {"label": "pty"}]},
        {"header": "Docs", "question": "Which docs change?"}
    ]));
    press(&mut form, &[Key::Char(' ')]);
    let title = Title {
        session: "main".to_owned(),
        place: None,
        questions: Some(3),
    };
    (form, title)
}

#[test]
fn the_mock_draws_column_for_column() {
    let (form, title) = mocked();
    let shown = form.panel(&title, 100);
    let left = "Question from main · 3 questions, any can be skipped";
    let pad = 100usize
        .saturating_sub(left.chars().count() + "Esc: chat about this".len())
        .max(1);
    assert_eq!(
        shown.lines,
        [
            format!("{left}{}{}", " ".repeat(pad), "Esc: chat about this"),
            String::new(),
            "←  ✔ Scope    ☐ Tests    ☐ Docs    ✔ Submit  →".to_owned(),
            String::new(),
            "Which crates should the change touch?".to_owned(),
            "choose any".to_owned(),
            String::new(),
            "▸ 1 [x] hub  the session host".to_owned(),
            "  2 [ ] tui  the terminal".to_owned(),
            "  3 ✎ Type an answer, alone or with the options".to_owned(),
            "  Next →".to_owned(),
            String::new(),
            "←→ question · ↑↓ choose · Enter chooses and moves on · Space toggles · type to answer in words".to_owned(),
        ]
    );
}

#[test]
fn the_mock_title_marks_each_run() {
    let (form, title) = mocked();
    let shown = form.panel(&title, 100);
    let left = "Question from main · 3 questions, any can be skipped";
    let pad = 100usize
        .saturating_sub(left.chars().count() + "Esc: chat about this".len())
        .max(1);
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 0)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(
        marks,
        [
            (0..8, secondary_bold()),
            (8..18, secondary()),
            (18..left.len(), secondary()),
            (left.len() + pad..left.len() + pad + 20, dim()),
        ]
    );
}

#[test]
fn the_mock_title_with_a_place_marks_it_bold() {
    let (form, _) = mocked();
    let title = Title {
        session: "main".to_owned(),
        place: Some((1, 2)),
        questions: Some(3),
    };
    let shown = form.panel(&title, 100);
    let line = &shown.lines[0];
    assert!(line.contains(" · 1 of 2"), "{line}");
    assert!(!line.contains("Esc") || line.ends_with("Esc: chat about this"));
    let place: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 0 && mark.style == secondary_bold())
        .map(|mark| mark.bytes.clone())
        .collect();
    assert_eq!(place.len(), 2);
    assert_eq!(&line[place[1].clone()], " · 1 of 2");
}

#[test]
fn the_tab_row_marks_each_state() {
    let (mock, mock_title) = mocked();
    // Scope answered and current: accent and reversed.
    let shown = mock.panel(&mock_title, 100);
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 2)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(
        marks,
        [
            (0..4, dim()),
            (4..15, accent_reversed()),
            (42..54, dim()),
            (54..58, dim()),
        ]
    );
    // Nothing answered: no tab is accent.
    let plain = form(json!([
        {"header": "Scope", "question": "Which?", "multiSelect": true,
         "options": [{"label": "hub"}]},
        {"header": "Tests", "question": "Which?", "options": [{"label": "unit"}]}
    ]));
    let shown = plain.panel(&title(2), 80);
    // The Submit tab reads dim off the cursor; the unanswered question
    // tabs leave no mark.
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 2)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(
        marks,
        [
            (0..4, dim()),
            (4..15, reversed()),
            (30..42, dim()),
            (42..46, dim()),
        ]
    );
}

#[test]
fn an_option_row_marks_each_role() {
    let (form, title) = mocked();
    let shown = form.panel(&title, 100);
    // "▸ 1 [x] hub  the session host": cursor, chosen, multi.
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 7)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(
        marks,
        [
            (0..4, secondary()),
            (4..6, dim()),
            (6..9, secondary()),
            (10..13, bold_underlined()),
            (13..31, dim()),
        ]
    );
    // "  2 [ ] tui  the terminal": off the cursor, unchosen. The two
    // spaces and the plain label leave no mark.
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 8)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(marks, [(2..4, dim()), (4..7, dim()), (11..25, dim()),]);
}

#[test]
fn a_single_choice_option_marks_its_round_mark() {
    let form = base_and_name(false);
    let shown = panel(&form);
    // "▸ 1 ( ) main (Recommended)  the default": cursor, unchosen, single.
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 6)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(
        marks,
        [
            (0..4, secondary()),
            (4..6, dim()),
            (6..9, dim()),
            (10..28, underlined()),
            (28..41, dim()),
        ]
    );
}

#[test]
fn the_words_row_marks_the_placeholder_and_its_first_character() {
    let (form, title) = mocked();
    let shown = form.panel(&title, 100);
    // "  3 ✎ Type an answer, alone or with the options": off the cursor.
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 9)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(marks, [(2..4, dim()), (4..8, dim()), (8..49, dim()),]);
    assert_eq!(shown.caret, None);
    // With the cursor on the row: the first character reversed, the caret
    // on its cell.
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Down, Key::Down]);
    let shown = panel(&form);
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 9)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(
        marks,
        [
            (0..4, secondary()),
            (4..6, dim()),
            (6..10, dim()),
            (10..11, first()),
            (11..51, dim()),
        ]
    );
    assert_eq!(shown.caret, Some((9, 6)));
    assert_eq!(shown.cursor, Some(9));
}

#[test]
fn typed_words_draw_bold_with_the_caret_after_them() {
    let mut form = base_and_name(true);
    type_text(&mut form, "ab");
    let shown = panel(&form);
    assert_eq!(shown.lines[9], "▸ 3 ✎ ab");
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 9)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(
        marks,
        [
            (0..4, secondary()),
            (4..6, dim()),
            (6..10, dim()),
            (10..12, bold()),
        ]
    );
    assert_eq!(shown.caret, Some((9, 8)));
}

#[test]
fn next_is_bold_under_the_cursor_and_plain_away_from_it() {
    let (form, title) = mocked();
    let shown = form.panel(&title, 100);
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 10)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(marks, [(2..10, secondary())]);
    let mut form = three_options();
    press(&mut form, &[Key::Down, Key::Down, Key::Down, Key::Down]);
    let shown = panel(&form);
    let at = shown.cursor.unwrap_or_default();
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == at)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(marks, [(0..4, secondary()), (4..14, secondary_bold())]);
}

#[test]
fn the_single_choice_hint_has_no_space_toggles() {
    let form = base_and_name(false);
    let shown = panel(&form);
    let hint = shown.lines.last().cloned().unwrap_or_default();
    assert_eq!(
        hint,
        "←→ question · ↑↓ choose · Enter chooses and moves on · type to answer in words"
    );
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == shown.lines.len() - 1)
        .map(|mark| mark.style)
        .collect();
    assert_eq!(marks, [dim()]);
}

#[test]
fn the_submit_tab_marks_answers_note_and_buttons() {
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Char(' '), Key::Down, Key::Char(' ')]);
    type_text(&mut form, "and tags");
    press(&mut form, &[Key::Tab, Key::Tab]);
    let shown = panel(&form);
    // "Base" padded to 13 cells in accent, the answer bold.
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 4)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(marks, [(0..13, accent()), (13..48, bold())]);
    // "Name" padded to 13, "skipped" dim.
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 5)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(marks, [(0..13, accent()), (13..20, dim())]);
    // The note row off the cursor: "note" in accent, the hint dim.
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 6)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(marks, [(2..6, accent()), (6..35, dim())]);
    // The buttons stack below the width one row fits, so each button
    // stays a whole-line click target: " Submit " reversed,
    // " Chat about this " on the rule tint, the rest dim.
    assert_eq!(shown.lines[8], " Submit ");
    assert_eq!(shown.lines[9], " Chat about this ");
    let rest = "  declines and ends the turn, so you can answer in your own words";
    assert_eq!(shown.lines[10], rest);
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 8)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(marks, [(0..8, reversed())]);
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 9)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(marks, [(0..17, chat())]);
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 10)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(marks, [(0..rest.len(), dim())]);
}

#[test]
fn the_submit_buttons_share_a_row_at_91_cells_and_stack_below_it() {
    let send = " Submit ";
    let chat = " Chat about this ";
    let rest = "  declines and ends the turn, so you can answer in your own words";
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab, Key::Tab]);
    let heading = title(form.question_count());
    // At 91 cells the two buttons and the rest share line 8, and the
    // cursor on Submit marks that line.
    let wide = form.panel(&heading, 91);
    assert_eq!(wide.lines[8], format!("{send} {chat}{rest}"));
    assert_eq!(wide.cursor, Some(8));
    // At 90 cells they stack, one per row, and the cursor still marks
    // the Submit button's line.
    let narrow = form.panel(&heading, 90);
    assert_eq!(narrow.lines[8], send);
    assert_eq!(narrow.lines[9], chat);
    assert_eq!(narrow.lines[10], rest);
    assert_eq!(narrow.cursor, Some(8));
    // On the note row, the cursor marks the note line at either width.
    press(&mut form, &[Key::Up]);
    assert_eq!(form.panel(&heading, 91).cursor, Some(6));
    assert_eq!(form.panel(&heading, 90).cursor, Some(6));
}

#[test]
fn a_header_wider_than_13_cells_keeps_one_space() {
    for (header, padded) in [
        ("123456789012", "123456789012 "),
        ("1234567890123", "1234567890123 "),
        ("12345678901234", "12345678901234 "),
    ] {
        let unanswered = form(json!([
            {"header": header, "question": "Which?", "multiSelect": true,
             "options": [{"label": "a"}]}
        ]));
        let mut form = unanswered;
        press(&mut form, &[Key::Char(' '), Key::Tab]);
        let shown = form.panel(&title(1), 80);
        assert_eq!(shown.lines[4], format!("{padded}a"), "{header}");
        let marks: Vec<_> = shown
            .marks
            .iter()
            .filter(|mark| mark.line == 4)
            .map(|mark| (mark.bytes.clone(), mark.style))
            .collect();
        assert_eq!(
            marks,
            [
                (0..padded.len(), accent()),
                (padded.len()..padded.len() + 1, bold())
            ]
        );
    }
}

#[test]
fn the_tab_spots_start_after_the_arrows() {
    let form = base_and_name(false);
    let shown = panel(&form);
    let spots: Vec<_> = shown
        .spots
        .iter()
        .filter(|spot| matches!(spot.spot, Spot::Tab(_)))
        .map(|spot| (spot.line, spot.cols, spot.spot))
        .collect();
    assert_eq!(
        spots,
        [
            (2, Some((2, 10)), Spot::Tab(0)),
            (2, Some((12, 20)), Spot::Tab(1)),
            (2, Some((22, 32)), Spot::Tab(2)),
        ]
    );
}

#[test]
fn the_panel_marks_the_cursor_line_and_each_rows_spot() {
    let form = base_and_name(false);
    let shown = panel(&form);
    assert_eq!(shown.cursor, Some(6));
    let spots: Vec<_> = shown
        .spots
        .iter()
        .map(|spot| (spot.line, spot.cols, spot.spot))
        .collect();
    assert_eq!(
        spots,
        [
            (2, Some((2, 10)), Spot::Tab(0)),
            (2, Some((12, 20)), Spot::Tab(1)),
            (2, Some((22, 32)), Spot::Tab(2)),
            (6, None, Spot::Option(0)),
            (7, None, Spot::Option(1)),
            (8, None, Spot::Words),
        ]
    );
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab, Key::Tab, Key::Up]);
    let shown = panel(&form);
    assert_eq!(shown.cursor, Some(6));
    let rows: Vec<(usize, Spot)> = shown
        .spots
        .iter()
        .filter(|spot| spot.cols.is_none())
        .map(|spot| (spot.line, spot.spot))
        .collect();
    assert_eq!(rows, [(6, Spot::Note), (8, Spot::Send), (9, Spot::Chat)]);
    let sends: Vec<_> = shown
        .spots
        .iter()
        .filter(|spot| spot.spot == Spot::Send || spot.spot == Spot::Chat)
        .map(|spot| (spot.line, spot.cols))
        .collect();
    // Stacked below one row's width, each button is a whole-line
    // target, so a click on the wrapped label still declines.
    assert_eq!(sends, [(8, None), (9, None)]);
}

#[test]
fn a_long_words_text_clips_at_the_width_with_the_caret_on_the_last_column() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "abcdefgh");
    let shown = form.panel(&title(2), 15);
    // The text cursor after `h` is at column 14: the "▸ 1 ✎ " prefix is
    // 6 cells.
    assert_eq!(shown.lines[6], "▸ 1 ✎ abcdefgh");
    assert_eq!(shown.caret, Some((6, 14)));
    let shown = form.panel(&title(2), 14);
    assert_eq!(shown.lines[6], "▸ 1 ✎ bcdefgh");
    assert_eq!(shown.caret, Some((6, 13)));
    let shown = form.panel(&title(2), 13);
    assert_eq!(shown.lines[6], "▸ 1 ✎ cdefgh");
    assert_eq!(shown.caret, Some((6, 12)));
}

#[test]
fn a_narrow_title_keeps_the_hint_mark() {
    let form = base_and_name(true);
    let shown = form.panel(&title(2), 20);
    assert!(shown.lines[0].contains("Esc: chat about this"));
    assert!(
        shown
            .marks
            .iter()
            .any(|mark| mark.line == 0 && mark.style == dim())
    );
}

/// Two single-choice questions `A` and `B`, each with options `x` and `y`.
fn two_choices() -> Form {
    form(json!([
        {"header": "A", "question": "A?", "options": [{"label": "x"}, {"label": "y"}]},
        {"header": "B", "question": "B?", "options": [{"label": "x"}, {"label": "y"}]}
    ]))
}

/// Presses ← (`false`) or → (`true`) for each entry.
fn arrows(form: &mut Form, right: &[bool]) {
    for right in right {
        form.on_edit(if *right { &Edit::Right } else { &Edit::Left });
    }
}

/// The words row's line and the text cursor's column at `cols` columns.
fn words_at(form: &Form, cols: u16) -> (String, Option<u16>) {
    let shown = form.panel(&title(form.question_count()), cols);
    let line = shown
        .spots
        .iter()
        .find(|spot| spot.spot == Spot::Words)
        .and_then(|spot| shown.lines.get(spot.line).cloned())
        .unwrap_or_default();
    (line, shown.caret.map(|(_, col)| col))
}

#[test]
fn left_and_right_on_an_option_row_move_between_tabs_and_stop_at_the_ends() {
    let mut form = two_choices();
    arrows(&mut form, &[true]);
    assert_eq!(line(&form, 4), "B?");
    assert_eq!(cursor_line(&form), "▸ 1 ( ) x");
    arrows(&mut form, &[true, true]);
    let shown = panel(&form);
    assert_eq!(shown.cursor, Some(8));
    assert!(shown.lines[8].starts_with(" Submit "));
    arrows(&mut form, &[false]);
    assert_eq!(line(&form, 4), "B?");
    arrows(&mut form, &[false, false]);
    assert_eq!(line(&form, 4), "A?");
    assert_eq!(cursor_line(&form), "▸ 1 ( ) x");
}

#[test]
fn left_and_right_on_next_moves_between_tabs() {
    let mut form = three_options();
    press(&mut form, &[Key::Down, Key::Down, Key::Down, Key::Down]);
    assert_eq!(cursor_line(&form), "▸ Review →");
    arrows(&mut form, &[true]);
    let shown = panel(&form);
    assert_eq!(shown.cursor, Some(7));
    assert!(shown.lines[7].starts_with(" Submit "));
}

#[test]
fn left_and_right_on_the_note_move_the_note_cursor_and_keep_the_tab() {
    let mut form = two_choices();
    press(&mut form, &[Key::Tab, Key::Tab, Key::Up]);
    type_text(&mut form, "ab");
    assert_eq!(cursor_line(&form), "▸ note ab");
    arrows(&mut form, &[false]);
    assert_eq!(line(&form, 2), "Review");
    type_text(&mut form, "x");
    assert_eq!(cursor_line(&form), "▸ note axb");
    arrows(&mut form, &[false, false, false, false]);
    arrows(&mut form, &[true, true, true, true, true, true]);
    assert_eq!(line(&form, 2), "Review");
    type_text(&mut form, "z");
    assert_eq!(cursor_line(&form), "▸ note axbz");
    arrows(&mut form, &[false]);
    type_text(&mut form, "w");
    assert_eq!(cursor_line(&form), "▸ note axbwz");
}

#[test]
fn shift_tab_and_tab_leave_the_note_row() {
    let mut form = two_choices();
    press(&mut form, &[Key::Tab, Key::Tab, Key::Up]);
    press(&mut form, &[Key::BackTab]);
    assert_eq!(line(&form, 4), "B?");
    press(&mut form, &[Key::Tab]);
    let shown = panel(&form);
    assert_eq!(shown.cursor, Some(8));
    assert!(shown.lines[8].starts_with(" Submit "));
}

#[test]
fn backspace_on_the_note_removes_the_char_before_the_cursor() {
    let mut form = two_choices();
    press(&mut form, &[Key::Tab, Key::Tab, Key::Up]);
    type_text(&mut form, "abc");
    arrows(&mut form, &[false]);
    press(&mut form, &[Key::Backspace]);
    assert_eq!(cursor_line(&form), "▸ note ac");
    arrows(&mut form, &[false, false, false]);
    press(&mut form, &[Key::Backspace]);
    assert_eq!(cursor_line(&form), "▸ note ac");
}

#[test]
fn left_and_right_on_the_words_row_move_the_text_cursor_and_stop_at_the_ends() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "abc");
    assert_eq!(words_at(&form, 60), ("▸ 1 ✎ abc".to_owned(), Some(9)));
    arrows(&mut form, &[true]);
    assert_eq!(words_at(&form, 60).1, Some(9));
    arrows(&mut form, &[false, false]);
    assert_eq!(words_at(&form, 60).1, Some(7));
    arrows(&mut form, &[false, false]);
    assert_eq!(words_at(&form, 60).1, Some(6));
    assert!(tabs(&form).contains("✔ Name"));
    arrows(&mut form, &[true]);
    assert_eq!(words_at(&form, 60).1, Some(7));
}

#[test]
fn typing_and_a_paste_insert_at_the_text_cursor() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "ad");
    arrows(&mut form, &[false]);
    type_text(&mut form, "b");
    form.on_edit(&Edit::Paste("c\n".to_owned()));
    assert_eq!(words_at(&form, 60), ("▸ 1 ✎ abc d".to_owned(), Some(10)));
    assert_eq!(
        answer(&form)["answers"][1],
        json!({"labels": [], "text": "abc d"})
    );
}

#[test]
fn backspace_removes_the_character_before_the_text_cursor() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "abc");
    arrows(&mut form, &[false]);
    press(&mut form, &[Key::Backspace]);
    assert_eq!(words_at(&form, 60), ("▸ 1 ✎ ac".to_owned(), Some(7)));
    arrows(&mut form, &[false]);
    press(&mut form, &[Key::Backspace]);
    assert_eq!(words_at(&form, 60), ("▸ 1 ✎ ac".to_owned(), Some(6)));
}

#[test]
fn a_click_on_the_words_row_puts_the_text_cursor_at_the_end() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "abc");
    arrows(&mut form, &[false, false]);
    press(&mut form, &[Key::Down]);
    assert_eq!(form.click(Spot::Words), PanelKey::Handled);
    assert_eq!(words_at(&form, 60).1, Some(9));
}

#[test]
fn the_words_row_scrolls_only_when_the_text_cursor_would_reach_the_width() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "abcdefgh");
    // The text cursor after `h` is at column 14: the "▸ 1 ✎ " prefix is
    // 6 cells.
    assert_eq!(words_at(&form, 17), ("▸ 1 ✎ abcdefgh".to_owned(), Some(14)));
    assert_eq!(words_at(&form, 15), ("▸ 1 ✎ abcdefgh".to_owned(), Some(14)));
    assert_eq!(words_at(&form, 14), ("▸ 1 ✎ bcdefgh".to_owned(), Some(13)));
}

#[test]
fn a_wide_character_counts_two_cells_on_the_words_row() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "名前");
    assert_eq!(words_at(&form, 12), ("▸ 1 ✎ 名前".to_owned(), Some(10)));
    assert_eq!(words_at(&form, 10), ("▸ 1 ✎ 前".to_owned(), Some(8)));
}

#[test]
fn the_panel_places_the_caret_on_the_words_line() {
    let mut form = base_and_name(false);
    assert_eq!(form.panel(&title(2), 60).caret, None);
    press(&mut form, &[Key::Down, Key::Down]);
    assert_eq!(form.panel(&title(2), 60).caret, Some((8, 6)));
}

/// One session envelope without an action.
fn envelope(kind: &str, payload: Value) -> contract::Envelope {
    contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(S_A.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// An `interaction_requested` form `id` with `extra` keys.
fn asked(id: &str, extra: Value) -> contract::Envelope {
    let mut payload = json!({"request_id": id, "kind": "form", "fields": [
        {"header": "Base", "question": "Which branch?",
         "options": [{"label": "main"}, {"label": "dev"}]}]});
    if let (Some(into), Some(from)) = (payload.as_object_mut(), extra.as_object()) {
        into.extend(from.clone());
    }
    envelope("interaction_requested", payload)
}

/// A standing-ask approval `id`.
fn approval(id: &str) -> contract::Envelope {
    envelope(
        "permission_requested",
        json!({"request_id": id, "effects": ["executes"], "reversible": true,
            "step": "standing_ask", "standing_rule": {"scope": "global", "prefix": "ls"}}),
    )
}

/// A queue holding `lines`, folded in order.
fn folded(lines: &[contract::Envelope]) -> Queue {
    let mut queue = Queue::default();
    for line in lines {
        queue.fold(line);
    }
    queue
}

/// The panel's lines, or none when it is closed.
fn panel_lines(queue: &Queue) -> Vec<String> {
    queue.panel(80).map(|panel| panel.lines).unwrap_or_default()
}

/// The panel's header, or nothing when it is closed.
fn header(queue: &Queue) -> String {
    panel_lines(queue).first().cloned().unwrap_or_default()
}

/// An approval's header for `k` of `n` at width 80.
fn approval_header(k: usize, n: usize) -> String {
    let left = format!("Approval {k} of {n} · {S_A}");
    let pad = 80_usize
        .saturating_sub(crate::format::width(&left).saturating_add("Esc puts it aside".len()))
        .max(1);
    format!("{left}{}{}", " ".repeat(pad), "Esc puts it aside")
}

/// A one-field form's title: no place suffix with one request waiting.
fn form_title(session: &str) -> String {
    let left = format!("Question from {session} · 1 questions, any can be skipped");
    let pad = 80_usize
        .saturating_sub(crate::format::width(&left).saturating_add("Esc: chat about this".len()))
        .max(1);
    format!("{left}{}{}", " ".repeat(pad), "Esc: chat about this")
}

#[test]
fn a_form_joins_the_queue_behind_an_approval() {
    let mut queue = folded(&[approval("r_1")]);
    assert_eq!(queue.on_key(&Key::Esc), Some(PanelKey::Handled));
    queue.fold(&asked("r_4f", json!({"action_ids": ["a_1"]})));
    // Behind a request put aside, the form waits under the badge.
    assert!(queue.panel(80).is_none());
    assert_eq!(
        queue.badge(0).as_deref(),
        Some("  ⚠ 2 approvals waiting · click to reopen")
    );
    assert!(queue.open_first());
    assert_eq!(header(&queue), approval_header(1, 2));
    assert_eq!(queue.on_key(&Key::Esc), Some(PanelKey::Handled));
    let lines = panel_lines(&queue);
    assert_eq!(lines.first().cloned().unwrap_or_default()[..8], *"Question");
    assert!(lines[0].contains(" · 2 of 2"), "{}", lines[0]);
    assert_eq!(queue.panel(80).map(|panel| panel.alert), Some(false));
    // ⌥A moves on from the form as from an approval.
    assert_eq!(queue.on_key(&Key::AltA), Some(PanelKey::Handled));
    assert_eq!(header(&queue), approval_header(1, 2));
}

#[test]
fn a_resolved_form_leaves_the_queue() {
    let mut queue = folded(&[asked("r_4f", json!({}))]);
    assert_eq!(header(&queue), form_title(S_A));
    queue.fold(&envelope(
        "interaction_resolved",
        json!({"request_id": "r_9", "by": "person", "declined": true}),
    ));
    assert_eq!(header(&queue), form_title(S_A));
    queue.fold(&envelope(
        "interaction_resolved",
        json!({"request_id": "r_4f", "by": "person",
            "answers": [{"labels": ["main"]}]}),
    ));
    assert!(queue.panel(80).is_none());
    assert!(queue.badge(0).is_none());
}

#[test]
fn a_form_resolved_by_fiber_leaves_the_queue() {
    let mut queue = folded(&[asked("r_4f", json!({}))]);
    queue.fold(&envelope(
        "interaction_resolved",
        json!({"request_id": "r_4f", "by": "fiber", "declined": true}),
    ));
    assert!(queue.panel(80).is_none());
    assert!(queue.badge(0).is_none());
}

#[test]
fn a_re_raised_form_keeps_what_was_typed() {
    let mut queue = folded(&[asked("r_4f", json!({"action_ids": ["a_1"]}))]);
    assert_eq!(queue.on_key(&Key::Char('x')), Some(PanelKey::Handled));
    queue.fold(&asked(
        "r_4f",
        json!({"action_ids": ["a_1"], "resumes": true}),
    ));
    let lines = panel_lines(&queue);
    assert_eq!(lines.first(), Some(&form_title(S_A)));
    assert!(lines.contains(&"▸ 3 ✎ x".to_owned()), "{lines:?}");
}

#[test]
fn a_form_from_an_extension_queues() {
    let queue = folded(&[asked("r_4f", json!({"extension": "deploy"}))]);
    assert_eq!(header(&queue), form_title(S_A));
}

#[test]
fn enter_on_submit_answers_with_the_form() {
    let mut queue = folded(&[asked("r_4f", json!({}))]);
    assert_eq!(queue.on_key(&Key::Enter), Some(PanelKey::Handled));
    for ch in "soon".chars() {
        assert_eq!(queue.on_key(&Key::Char(ch)), Some(PanelKey::Handled));
    }
    assert_eq!(queue.on_key(&Key::Enter), Some(PanelKey::Answer));
    let line = queue.answer("c_1").unwrap_or_default();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap_or_default(),
        json!({"id": "c_1", "command": "reply", "session_id": S_A,
            "args": {"request_id": "r_4f", "answers": [{"labels": ["main"]}],
                "note": "soon"}})
    );
    assert!(queue.panel(80).is_none());
}

#[test]
fn decline_names_only_a_form() {
    let mut queue = folded(&[approval("r_1")]);
    assert_eq!(queue.decline("c_1"), None);
    assert_eq!(header(&queue), approval_header(1, 1));
    let mut queue = folded(&[asked("r_4f", json!({"action_ids": ["a_1"]}))]);
    let line = queue.decline("c_1").unwrap_or_default();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap_or_default(),
        json!({"id": "c_1", "command": "reply", "session_id": S_A,
            "args": {"request_id": "r_4f", "declined": true}})
    );
    assert!(queue.panel(80).is_none());
    // A rejected decline puts the form back and cancels nothing.
    queue.restore("c_1");
    assert_eq!(header(&queue), form_title(S_A));
    assert_eq!(queue.declined("c_1"), None);
    // An accepted decline names its session once.
    assert!(queue.decline("c_2").is_some());
    assert_eq!(
        queue.declined("c_2"),
        Some(contract::SessionId(S_A.to_owned()))
    );
    assert_eq!(queue.declined("c_2"), None);
    // A form no tool call raised names no session: its decline cancels nothing.
    let mut queue = folded(&[asked("r_4f", json!({}))]);
    assert!(queue.decline("c_3").is_some());
    assert_eq!(queue.declined("c_3"), None);
}

#[test]
fn a_shown_request_takes_every_edit() {
    assert!(!Queue::default().open());
    let mut queue = folded(&[asked("r_4f", json!({}))]);
    assert!(queue.open());
    queue.on_edit(&Edit::Paste("x".to_owned()));
    let lines = panel_lines(&queue);
    assert!(lines.contains(&"▸ 3 ✎ x".to_owned()), "{lines:?}");
}

#[test]
fn a_click_on_a_row_the_shown_tab_lacks_does_nothing() {
    let mut form = base_and_name(false);
    let before = lines(&form);
    for spot in [Spot::Option(2), Spot::Note, Spot::Send, Spot::Chat] {
        assert_eq!(form.click(spot), PanelKey::Handled, "{spot:?}");
        assert_eq!(lines(&form), before, "{spot:?}");
    }
    press(&mut form, &[Key::Tab, Key::Tab]);
    let before = lines(&form);
    for spot in [Spot::Option(0), Spot::Words, Spot::Next] {
        assert_eq!(form.click(spot), PanelKey::Handled, "{spot:?}");
        assert_eq!(lines(&form), before, "{spot:?}");
    }
}

#[test]
fn clicks_on_the_submit_tab_take_the_note_send_and_decline() {
    let mut form = base_and_name(false);
    assert_eq!(form.click(Spot::Tab(2)), PanelKey::Handled);
    let shown = panel(&form);
    assert_eq!(shown.cursor, Some(8));
    assert!(shown.lines[8].starts_with(" Submit "));
    assert_eq!(form.click(Spot::Note), PanelKey::Handled);
    assert_eq!(cursor_line(&form), "▸ note Add a note on the whole form");
    assert_eq!(form.click(Spot::Send), PanelKey::Answer);
    assert_eq!(form.click(Spot::Chat), PanelKey::Decline);
}

#[test]
fn clicks_on_options_choose_single_and_toggle_multi() {
    let mut form = base_and_name(false);
    assert_eq!(form.click(Spot::Option(1)), PanelKey::Handled);
    // A single-choice click chooses and moves on to the next question.
    assert_eq!(answer(&form)["answers"][0], json!({"labels": ["dev"]}));
    assert_eq!(cursor_line(&form), "▸ 1 ✎ Type an answer");
    let mut form = three_options();
    assert_eq!(form.click(Spot::Option(1)), PanelKey::Handled);
    assert_eq!(cursor_line(&form), "▸ 2 [x] b");
}
