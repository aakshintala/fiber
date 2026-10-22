//! Tests for one-question interactions on the request panel: keys,
//! digits, their lines with styled runs, and the shared queue.

use contract::shapes::Choice;
use ratatui::style::{Modifier, Style};
use serde_json::{Value, json};

use super::OneQuestion;
use crate::approvals::Title;
use crate::approvals::form::Spot;
use crate::approvals::{Panel, PanelKey, Queue};
use crate::keys::{Edit, Key};
use crate::theme::Role;

/// In secondary: the cursor's slot, a chosen mark.
fn secondary() -> Style {
    Style::new().fg(Role::Secondary.color())
}

/// Dim: numbers, the hint that is not there, descriptions, unchosen marks.
fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// Bold: the question, "Review" is not here, typed words.
fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// Reversed: " Submit ".
fn reversed() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// Underlined: a label under the cursor.
fn underlined() -> Style {
    Style::default().add_modifier(Modifier::UNDERLINED)
}

/// Dim and reversed: the placeholder's first character with the cursor on
/// its row.
fn first() -> Style {
    Style::default()
        .add_modifier(Modifier::DIM)
        .add_modifier(Modifier::REVERSED)
}

/// " Chat about this ": on the rule tint.
fn chat() -> Style {
    Style::new().bg(Role::Rule.color())
}

fn choice(label: &str, description: Option<&str>) -> Choice {
    Choice {
        label: label.to_owned(),
        description: description.map(str::to_owned),
    }
}

fn options() -> Vec<Choice> {
    vec![
        choice("alpha", Some("first option")),
        choice("beta", None),
        choice("gamma", None),
    ]
}

fn select() -> OneQuestion {
    OneQuestion::select("Pick one?".to_owned(), options())
}

fn multi_select() -> OneQuestion {
    OneQuestion::multi_select("Pick some?".to_owned(), options())
}

/// The title for a one-question panel from `main`, nothing else waiting.
fn title() -> Title {
    Title {
        session: "main".to_owned(),
        place: None,
        questions: None,
    }
}

/// The title row at 80 columns.
fn title_row() -> String {
    format!("Question from main{}Esc: chat about this", " ".repeat(42))
}

fn panel(question: &OneQuestion) -> Panel {
    question.panel(&title(), 80)
}

fn lines(question: &OneQuestion) -> Vec<String> {
    panel(question).lines
}

fn answer(question: &OneQuestion) -> Value {
    serde_json::to_value(question.answer()).unwrap_or_default()
}

fn press(question: &mut OneQuestion, keys: &[Key]) {
    for key in keys {
        assert_eq!(question.on_key(key), Some(PanelKey::Handled), "{key:?}");
    }
}

fn type_text(question: &mut OneQuestion, text: &str) {
    for ch in text.chars() {
        press(question, &[Key::Char(ch)]);
    }
}

#[test]
fn each_kind_draws_one_question_without_a_tab_row() {
    let confirm = OneQuestion::confirm("Continue?".to_owned());
    assert_eq!(
        lines(&confirm).as_slice(),
        [
            title_row().as_str(),
            "",
            "Continue?",
            "",
            "▸ 1 ( ) yes",
            "  2 ( ) no",
            "   Chat about this ",
        ]
    );

    let select = OneQuestion::select(
        "Pick\none?".to_owned(),
        vec![
            choice("alpha\nversion", Some("first\nchoice")),
            choice("beta", None),
        ],
    );
    assert_eq!(
        lines(&select).as_slice(),
        [
            title_row().as_str(),
            "",
            "Pick one?",
            "",
            "▸ 1 ( ) alpha version  first choice",
            "  2 ( ) beta",
            "   Chat about this ",
        ]
    );

    let multi = OneQuestion::multi_select(
        "Pick some?".to_owned(),
        vec![choice("alpha", Some("first option")), choice("beta", None)],
    );
    assert_eq!(
        lines(&multi).as_slice(),
        [
            title_row().as_str(),
            "",
            "Pick some?",
            "choose any",
            "",
            "▸ 1 [ ] alpha  first option",
            "  2 [ ] beta",
            "   Submit ",
            "   Chat about this ",
        ]
    );

    let text = OneQuestion::text_input("What should it say?".to_owned());
    assert_eq!(
        lines(&text).as_slice(),
        [
            title_row().as_str(),
            "",
            "What should it say?",
            "",
            "▸ 1 ✎ Type an answer",
            "   Submit ",
            "   Chat about this ",
        ]
    );
}

#[test]
fn one_question_titles_carry_no_question_count() {
    for question in [
        OneQuestion::confirm("Continue?".to_owned()),
        select(),
        multi_select(),
        OneQuestion::text_input("Words?".to_owned()),
    ] {
        let shown = panel(&question);
        assert!(!shown.lines[0].contains("questions"), "{}", shown.lines[0]);
        assert!(
            shown.lines[0].ends_with("Esc: chat about this"),
            "{}",
            shown.lines[0]
        );
    }
}

#[test]
fn initial_cursor_is_on_the_first_actionable_row() {
    for (question, at) in [
        (OneQuestion::confirm("Continue?".to_owned()), 4),
        (select(), 4),
        (multi_select(), 5),
    ] {
        assert_eq!(question.panel(&title(), 80).cursor, Some(at));
    }
    assert_eq!(
        OneQuestion::text_input("Words?".to_owned())
            .panel(&title(), 80)
            .cursor,
        Some(4)
    );
    assert_eq!(
        OneQuestion::select("None?".to_owned(), Vec::new())
            .panel(&title(), 80)
            .cursor,
        Some(4)
    );
    assert_eq!(
        OneQuestion::multi_select("None?".to_owned(), Vec::new())
            .panel(&title(), 80)
            .cursor,
        Some(5)
    );
    assert_eq!(
        lines(&OneQuestion::select("None?".to_owned(), Vec::new())).as_slice(),
        [title_row().as_str(), "", "None?", "", "▸  Chat about this "]
    );
    assert_eq!(
        lines(&OneQuestion::multi_select("None?".to_owned(), Vec::new())).as_slice(),
        [
            title_row().as_str(),
            "",
            "None?",
            "choose any",
            "",
            "▸  Submit ",
            "   Chat about this "
        ]
    );
}

#[test]
fn confirm_enter_sends_the_yes_or_no_option() {
    let mut question = OneQuestion::confirm("Continue?".to_owned());
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(answer(&question), json!({"confirmed": true}));
    press(&mut question, &[Key::Down]);
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(answer(&question), json!({"confirmed": false}));
}

#[test]
fn select_enter_sends_the_option_under_the_cursor() {
    let mut question = select();
    press(&mut question, &[Key::Down]);
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(answer(&question), json!({"labels": ["beta"]}));

    let mut question = select();
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(answer(&question), json!({"labels": ["alpha"]}));
}

#[test]
fn multi_select_toggles_and_sends_labels_in_option_order() {
    let mut question = multi_select();
    press(
        &mut question,
        &[Key::Char(' '), Key::Down, Key::Down, Key::Char(' ')],
    );
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(answer(&question), json!({"labels": ["alpha", "gamma"]}));

    let mut question = multi_select();
    press(&mut question, &[Key::Char(' '), Key::Char(' ')]);
    press(&mut question, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(answer(&question), json!({"labels": []}));
}

#[test]
fn multi_select_deduplicates_repeated_labels_in_option_order() {
    let mut repeated = OneQuestion::multi_select(
        "Pick?".to_owned(),
        vec![choice("a", None), choice("a", None)],
    );
    press(&mut repeated, &[Key::Char(' '), Key::Down, Key::Char(' ')]);
    assert_eq!(answer(&repeated), json!({"labels": ["a"]}));

    let mut repeated = OneQuestion::multi_select(
        "Pick?".to_owned(),
        vec![choice("a", None), choice("b", None), choice("a", None)],
    );
    press(
        &mut repeated,
        &[
            Key::Char(' '),
            Key::Down,
            Key::Char(' '),
            Key::Down,
            Key::Char(' '),
        ],
    );
    assert_eq!(answer(&repeated), json!({"labels": ["a", "b"]}));
}

#[test]
fn multi_select_enter_on_submit_sends_the_answer() {
    let mut question = multi_select();
    press(&mut question, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(
        lines(&question).get(8).map(String::as_str),
        Some("▸  Submit ")
    );
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));
}

#[test]
fn digits_toggle_multi_select_and_carry_the_labels() {
    let mut question = multi_select();
    assert_eq!(question.on_key(&Key::Char('2')), Some(PanelKey::Handled));
    assert_eq!(
        lines(&question).get(6).map(String::as_str),
        Some("▸ 2 [x] beta")
    );
    assert_eq!(answer(&question), json!({"labels": ["beta"]}));
    // Nothing is sent: Enter sends it.
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(question.on_key(&Key::Char('2')), Some(PanelKey::Handled));
    assert_eq!(
        lines(&question).get(6).map(String::as_str),
        Some("▸ 2 [ ] beta")
    );
}

#[test]
fn digits_move_the_cursor_on_confirm_and_select_and_send_nothing() {
    let mut question = OneQuestion::confirm("Continue?".to_owned());
    assert_eq!(question.on_key(&Key::Char('2')), Some(PanelKey::Handled));
    assert_eq!(
        lines(&question).get(5).map(String::as_str),
        Some("▸ 2 ( ) no")
    );
    assert_eq!(answer(&question), json!({"confirmed": false}));
    // Enter on the option sends it.
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));

    let mut question = select();
    assert_eq!(question.on_key(&Key::Char('3')), Some(PanelKey::Handled));
    assert_eq!(
        lines(&question).get(6).map(String::as_str),
        Some("▸ 3 ( ) gamma")
    );
    assert_eq!(answer(&question), json!({"labels": ["gamma"]}));
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));
}

#[test]
fn digits_on_text_input_are_typed() {
    let mut question = OneQuestion::text_input("Words?".to_owned());
    assert_eq!(question.on_key(&Key::Char('5')), Some(PanelKey::Handled));
    assert_eq!(lines(&question).get(4).map(String::as_str), Some("▸ 1 ✎ 5"));
    assert_eq!(answer(&question), json!({"text": "5"}));
}

#[test]
fn digits_past_the_last_option_do_nothing() {
    // Three options: '4' is past the words row that is not there, '0' is
    // never a row.
    let mut question = select();
    let before = lines(&question);
    assert_eq!(question.on_key(&Key::Char('4')), Some(PanelKey::Handled));
    assert_eq!(question.on_key(&Key::Char('0')), Some(PanelKey::Handled));
    assert_eq!(lines(&question), before);
    assert_eq!(answer(&question), json!({"labels": ["alpha"]}));

    let mut question = OneQuestion::confirm("Continue?".to_owned());
    let before = lines(&question);
    assert_eq!(question.on_key(&Key::Char('3')), Some(PanelKey::Handled));
    assert_eq!(question.on_key(&Key::Char('0')), Some(PanelKey::Handled));
    assert_eq!(lines(&question), before);

    let mut question = multi_select();
    let before = lines(&question);
    assert_eq!(question.on_key(&Key::Char('4')), Some(PanelKey::Handled));
    assert_eq!(question.on_key(&Key::Char('9')), Some(PanelKey::Handled));
    assert_eq!(lines(&question), before);
    assert_eq!(answer(&question), json!({"labels": []}));
}

#[test]
fn a_paste_of_digits_never_chooses() {
    for mut question in [
        OneQuestion::confirm("Continue?".to_owned()),
        select(),
        multi_select(),
    ] {
        let before = lines(&question);
        let before_answer = question.answer();
        question.on_edit(&Edit::Paste("12".to_owned()));
        assert_eq!(lines(&question), before);
        assert_eq!(question.answer(), before_answer);
    }
    let mut question = OneQuestion::text_input("Words?".to_owned());
    question.on_edit(&Edit::Paste("12".to_owned()));
    assert_eq!(
        lines(&question).get(4).map(String::as_str),
        Some("▸ 1 ✎ 12")
    );
}

#[test]
fn text_input_preserves_empty_whitespace_and_typed_text() {
    let mut question = OneQuestion::text_input("Words?".to_owned());
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(answer(&question), json!({"text": ""}));

    let mut question = OneQuestion::text_input("Words?".to_owned());
    type_text(&mut question, "hi");
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(answer(&question), json!({"text": "hi"}));

    let mut question = OneQuestion::text_input("Words?".to_owned());
    type_text(&mut question, "  ");
    assert_eq!(answer(&question), json!({"text": "  "}));
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(answer(&question), json!({"text": "  "}));
}

#[test]
fn text_input_edits_at_a_character_cursor_and_clamps_at_both_ends() {
    let mut question = OneQuestion::text_input("Words?".to_owned());
    type_text(&mut question, "ab");
    press(&mut question, &[Key::Backspace]);
    assert_eq!(lines(&question).get(4).map(String::as_str), Some("▸ 1 ✎ a"));
    question.on_edit(&Edit::Left);
    question.on_edit(&Edit::Left);
    type_text(&mut question, "x");
    assert_eq!(
        lines(&question).get(4).map(String::as_str),
        Some("▸ 1 ✎ xa")
    );
    question.on_edit(&Edit::Right);
    question.on_edit(&Edit::Right);
    assert_eq!(question.panel(&title(), 80).caret, Some((4, 8)));
    question.on_edit(&Edit::Left);
    assert_eq!(question.panel(&title(), 80).caret, Some((4, 7)));
    question.on_edit(&Edit::Left);
    assert_eq!(question.panel(&title(), 80).caret, Some((4, 6)));
}

#[test]
fn text_cursor_edits_do_nothing_off_the_words_row() {
    let mut question = OneQuestion::text_input("Words?".to_owned());
    type_text(&mut question, "ab");
    press(&mut question, &[Key::Down]);
    let submit = question.panel(&title(), 80);
    press(&mut question, &[Key::Backspace]);
    question.on_edit(&Edit::Left);
    question.on_edit(&Edit::Right);
    assert_eq!(question.panel(&title(), 80), submit);
    press(&mut question, &[Key::Down]);
    let chat = question.panel(&title(), 80);
    press(&mut question, &[Key::Backspace]);
    question.on_edit(&Edit::Left);
    question.on_edit(&Edit::Right);
    assert_eq!(question.panel(&title(), 80), chat);
}

#[test]
fn backspace_and_arrows_off_words_leave_text_and_caret_unchanged() {
    let mut backspace = OneQuestion::text_input("Words?".to_owned());
    type_text(&mut backspace, "ab");
    press(&mut backspace, &[Key::Down, Key::Backspace]);
    type_text(&mut backspace, "x");
    assert_eq!(
        lines(&backspace).get(4).map(String::as_str),
        Some("▸ 1 ✎ abx")
    );

    let mut left = OneQuestion::text_input("Words?".to_owned());
    type_text(&mut left, "ab");
    press(&mut left, &[Key::Down]);
    left.on_edit(&Edit::Left);
    type_text(&mut left, "x");
    assert_eq!(lines(&left).get(4).map(String::as_str), Some("▸ 1 ✎ abx"));

    let mut right = OneQuestion::text_input("Words?".to_owned());
    type_text(&mut right, "ab");
    right.on_edit(&Edit::Left);
    press(&mut right, &[Key::Down]);
    right.on_edit(&Edit::Right);
    type_text(&mut right, "x");
    assert_eq!(lines(&right).get(4).map(String::as_str), Some("▸ 1 ✎ axb"));
}

#[test]
fn text_input_paste_turns_control_characters_into_spaces() {
    let mut question = OneQuestion::text_input("Words?".to_owned());
    question.on_edit(&Edit::Paste("a\nb\tc".to_owned()));
    assert_eq!(
        lines(&question).get(4).map(String::as_str),
        Some("▸ 1 ✎ a b c")
    );
    press(&mut question, &[Key::Char(' ')]);
    assert_eq!(
        lines(&question).get(4).map(String::as_str),
        Some("▸ 1 ✎ a b c ")
    );
}

#[test]
fn text_input_enter_on_submit_sends_the_answer() {
    let mut question = OneQuestion::text_input("Words?".to_owned());
    press(&mut question, &[Key::Down]);
    assert_eq!(
        lines(&question).get(5).map(String::as_str),
        Some("▸  Submit ")
    );
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));
}

#[test]
fn esc_and_chat_decline_each_kind() {
    let constructors: [fn() -> OneQuestion; 4] = [
        || OneQuestion::confirm("Continue?".to_owned()),
        select,
        multi_select,
        || OneQuestion::text_input("Words?".to_owned()),
    ];
    for make in constructors {
        let mut question = make();
        assert_eq!(question.on_key(&Key::Esc), Some(PanelKey::Decline));

        let mut question = make();
        let last = question.rows().len().saturating_sub(1);
        for _ in 0..last {
            press(&mut question, &[Key::Down]);
        }
        assert!(
            lines(&question)
                .last()
                .is_some_and(|line| line.starts_with("▸ "))
        );
        assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Decline));
    }
}

#[test]
fn up_and_down_stop_at_the_first_and_last_row_for_each_kind() {
    let constructors: [fn() -> OneQuestion; 4] = [
        || OneQuestion::confirm("Continue?".to_owned()),
        select,
        multi_select,
        || OneQuestion::text_input("Words?".to_owned()),
    ];
    for make in constructors {
        let mut question = make();
        let before = lines(&question);
        press(&mut question, &[Key::Up]);
        assert_eq!(lines(&question), before);
        for _ in 0..question.rows().len() {
            press(&mut question, &[Key::Down]);
        }
        let last = lines(&question);
        press(&mut question, &[Key::Down]);
        assert_eq!(lines(&question), last);
    }
}

#[test]
fn tabs_are_inert_and_typing_on_option_kinds_changes_nothing() {
    for mut question in [OneQuestion::confirm("Continue?".to_owned()), select()] {
        let before = lines(&question);
        let before_answer = question.answer();
        press(&mut question, &[Key::Tab, Key::BackTab]);
        assert_eq!(lines(&question), before);
        question.on_edit(&Edit::Paste("x\ny".to_owned()));
        press(
            &mut question,
            &[Key::Char('x'), Key::Char(' '), Key::Backspace],
        );
        question.on_edit(&Edit::Left);
        question.on_edit(&Edit::Right);
        assert_eq!(lines(&question), before);
        assert_eq!(question.answer(), before_answer);
    }

    let mut question = multi_select();
    let before = lines(&question);
    let before_answer = question.answer();
    press(
        &mut question,
        &[Key::Tab, Key::BackTab, Key::Char('x'), Key::Backspace],
    );
    question.on_edit(&Edit::Paste("x\ny".to_owned()));
    question.on_edit(&Edit::Left);
    question.on_edit(&Edit::Right);
    assert_eq!(lines(&question), before);
    assert_eq!(question.answer(), before_answer);
}

#[test]
fn space_on_multi_select_submit_and_chat_does_nothing() {
    let mut question = multi_select();
    press(&mut question, &[Key::Down, Key::Down, Key::Down]);
    let submit = lines(&question);
    press(&mut question, &[Key::Char(' ')]);
    assert_eq!(lines(&question), submit);
    press(&mut question, &[Key::Down]);
    let chat = lines(&question);
    press(&mut question, &[Key::Char(' ')]);
    assert_eq!(lines(&question), chat);
}

#[test]
fn layout_keys_pass_through_and_do_nothing_keys_are_handled() {
    let layout = [
        Key::PageUp,
        Key::PageDown,
        Key::End,
        Key::CtrlC,
        Key::CtrlF,
        Key::F1,
        Key::CtrlO,
        Key::AltP,
        Key::AltR,
        Key::AltDigit(1),
    ];
    for key in layout {
        let mut question = select();
        assert_eq!(question.on_key(&key), None, "{key:?}");
    }

    let inert = [
        Key::CtrlG,
        Key::CtrlR,
        Key::CtrlV,
        Key::CtrlL,
        Key::AltA,
        Key::AltUp,
        Key::AltDown,
        Key::AltX,
    ];
    for key in inert {
        let mut question = select();
        let before = lines(&question);
        assert_eq!(question.on_key(&key), Some(PanelKey::Handled), "{key:?}");
        assert_eq!(lines(&question), before, "{key:?}");
    }
}

#[test]
fn clicks_match_the_rows_and_actions() {
    let mut question = OneQuestion::confirm("Continue?".to_owned());
    assert_eq!(question.click(Spot::Option(0)), PanelKey::Answer);
    assert_eq!(answer(&question), json!({"confirmed": true}));
    let mut question = OneQuestion::confirm("Continue?".to_owned());
    assert_eq!(question.click(Spot::Option(1)), PanelKey::Answer);
    assert_eq!(answer(&question), json!({"confirmed": false}));

    let mut question = select();
    assert_eq!(question.click(Spot::Option(0)), PanelKey::Answer);
    assert_eq!(answer(&question), json!({"labels": ["alpha"]}));
    let mut question = select();
    assert_eq!(question.click(Spot::Option(1)), PanelKey::Answer);
    assert_eq!(answer(&question), json!({"labels": ["beta"]}));

    let mut question = multi_select();
    assert_eq!(question.click(Spot::Option(1)), PanelKey::Handled);
    assert!(
        lines(&question)
            .get(6)
            .is_some_and(|line| line.starts_with("▸ 2 [x] beta"))
    );
    assert_eq!(question.click(Spot::Send), PanelKey::Answer);
    assert_eq!(answer(&question), json!({"labels": ["beta"]}));

    let mut question = OneQuestion::text_input("Words?".to_owned());
    type_text(&mut question, "hi");
    question.on_edit(&Edit::Left);
    assert_eq!(question.click(Spot::Words), PanelKey::Handled);
    assert_eq!(question.panel(&title(), 80).caret, Some((4, 8)));
    // Typed words draw bold.
    let shown = question.panel(&title(), 80);
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 4)
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
    assert_eq!(question.click(Spot::Send), PanelKey::Answer);
    assert_eq!(answer(&question), json!({"text": "hi"}));

    for mut question in [
        OneQuestion::confirm("Continue?".to_owned()),
        select(),
        multi_select(),
        OneQuestion::text_input("Words?".to_owned()),
    ] {
        assert_eq!(question.click(Spot::Chat), PanelKey::Decline);
    }
}

#[test]
fn spots_without_a_row_for_the_kind_do_nothing() {
    for mut question in [OneQuestion::confirm("Continue?".to_owned()), select()] {
        for spot in [
            Spot::Tab(0),
            Spot::Next,
            Spot::Note,
            Spot::Words,
            Spot::Send,
            Spot::Option(9),
        ] {
            let before = question.panel(&title(), 80);
            let before_answer = question.answer();
            assert_eq!(question.click(spot), PanelKey::Handled, "{spot:?}");
            assert_eq!(question.panel(&title(), 80), before, "{spot:?}");
            assert_eq!(question.answer(), before_answer, "{spot:?}");
        }
    }

    let mut multi = multi_select();
    for spot in [
        Spot::Tab(0),
        Spot::Next,
        Spot::Note,
        Spot::Words,
        Spot::Option(9),
    ] {
        let before = multi.panel(&title(), 80);
        let before_answer = multi.answer();
        assert_eq!(multi.click(spot), PanelKey::Handled, "{spot:?}");
        assert_eq!(multi.panel(&title(), 80), before, "{spot:?}");
        assert_eq!(multi.answer(), before_answer, "{spot:?}");
    }

    let mut text = OneQuestion::text_input("Words?".to_owned());
    for spot in [Spot::Tab(0), Spot::Next, Spot::Note, Spot::Option(9)] {
        let before = text.panel(&title(), 80);
        let before_answer = text.answer();
        assert_eq!(text.click(spot), PanelKey::Handled, "{spot:?}");
        assert_eq!(text.panel(&title(), 80), before, "{spot:?}");
        assert_eq!(text.answer(), before_answer, "{spot:?}");
    }
}

#[test]
fn answers_for_confirm_and_select_require_an_option_cursor() {
    let mut confirm = OneQuestion::confirm("Continue?".to_owned());
    press(&mut confirm, &[Key::Down, Key::Down]);
    assert_eq!(confirm.answer(), None);

    let mut select = select();
    for _ in 0..4 {
        press(&mut select, &[Key::Down]);
    }
    assert_eq!(select.answer(), None);
}

#[test]
fn panel_cursor_caret_and_spots_follow_the_selected_row() {
    let confirm = OneQuestion::confirm("Continue?".to_owned());
    let shown = panel(&confirm);
    assert_eq!(shown.cursor, Some(4));
    assert_eq!(shown.caret, None);
    assert_eq!(
        shown
            .spots
            .iter()
            .map(|spot| (spot.line, spot.cols, spot.spot))
            .collect::<Vec<_>>(),
        [
            (4, None, Spot::Option(0)),
            (5, None, Spot::Option(1)),
            (6, None, Spot::Chat),
        ]
    );

    let question = select();
    let shown = panel(&question);
    assert_eq!(shown.cursor, Some(4));
    assert_eq!(shown.caret, None);
    assert_eq!(
        shown
            .spots
            .iter()
            .map(|spot| (spot.line, spot.cols, spot.spot))
            .collect::<Vec<_>>(),
        [
            (4, None, Spot::Option(0)),
            (5, None, Spot::Option(1)),
            (6, None, Spot::Option(2)),
            (7, None, Spot::Chat),
        ]
    );

    let multi = multi_select();
    let shown = panel(&multi);
    assert_eq!(shown.cursor, Some(5));
    assert_eq!(shown.caret, None);
    assert_eq!(
        shown
            .spots
            .iter()
            .map(|spot| (spot.line, spot.cols, spot.spot))
            .collect::<Vec<_>>(),
        [
            (5, None, Spot::Option(0)),
            (6, None, Spot::Option(1)),
            (7, None, Spot::Option(2)),
            (8, None, Spot::Send),
            (9, None, Spot::Chat),
        ]
    );

    let mut text = OneQuestion::text_input("Words?".to_owned());
    type_text(&mut text, "hi");
    let shown = panel(&text);
    assert_eq!(shown.cursor, Some(4));
    assert_eq!(shown.caret, Some((4, 8)));
    assert_eq!(
        shown
            .spots
            .iter()
            .map(|spot| (spot.line, spot.cols, spot.spot))
            .collect::<Vec<_>>(),
        [
            (4, None, Spot::Words),
            (5, None, Spot::Send),
            (6, None, Spot::Chat),
        ]
    );
}

#[test]
fn option_rows_mark_each_role() {
    // "▸ 1 ( ) yes": cursor, unchosen, confirm.
    let confirm = OneQuestion::confirm("Continue?".to_owned());
    let shown = panel(&confirm);
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 4)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(
        marks,
        [
            (0..4, secondary()),
            (4..6, dim()),
            (6..9, dim()),
            (10..13, underlined()),
        ]
    );
    // A chosen multi option: bold and underlined under the cursor.
    let mut multi = multi_select();
    press(&mut multi, &[Key::Char(' ')]);
    let shown = panel(&multi);
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 5)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(
        marks,
        [
            (0..4, secondary()),
            (4..6, dim()),
            (6..9, secondary()),
            (
                10..15,
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::UNDERLINED)
            ),
            (15..29, dim()),
        ]
    );
}

#[test]
fn the_words_row_marks_the_placeholder_first_character() {
    let text = OneQuestion::text_input("Words?".to_owned());
    let shown = panel(&text);
    // "▸ 1 ✎ Type an answer": the cursor sits on the placeholder's first
    // character, drawn reversed, with the caret on its cell.
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 4)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(
        marks,
        [
            (0..4, secondary()),
            (4..6, dim()),
            (6..10, dim()),
            (10..11, first()),
            (11..24, dim()),
        ]
    );
    assert_eq!(shown.caret, Some((4, 6)));
    assert_eq!(shown.lines[4], "▸ 1 ✎ Type an answer");
}

#[test]
fn submit_and_chat_rows_mark_the_buttons() {
    let mut multi = multi_select();
    press(&mut multi, &[Key::Down, Key::Down, Key::Down]);
    let shown = panel(&multi);
    // "▸  Submit": the cursor's slot and the reversed button.
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 8)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(marks, [(0..4, secondary()), (4..12, reversed())]);
    assert_eq!(shown.cursor, Some(8));
    press(&mut multi, &[Key::Down]);
    let shown = panel(&multi);
    let marks: Vec<_> = shown
        .marks
        .iter()
        .filter(|mark| mark.line == 9)
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(marks, [(0..4, secondary()), (4..21, chat())]);
    assert_eq!(shown.lines[9], "▸  Chat about this ");
}

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

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

fn confirm_request(id: &str) -> contract::Envelope {
    envelope(
        "interaction_requested",
        json!({"request_id": id, "kind": "confirm", "prompt": "Continue?"}),
    )
}

fn select_request(id: &str) -> contract::Envelope {
    envelope(
        "interaction_requested",
        json!({"request_id": id, "kind": "select", "prompt": "Pick one?",
            "options": [{"label": "a"}, {"label": "b"}]}),
    )
}

fn multi_select_request(id: &str) -> contract::Envelope {
    envelope(
        "interaction_requested",
        json!({"request_id": id, "kind": "multi_select", "prompt": "Pick some?",
            "options": [{"label": "a"}, {"label": "b"}, {"label": "c"}]}),
    )
}

fn text_input_request(id: &str) -> contract::Envelope {
    envelope(
        "interaction_requested",
        json!({"request_id": id, "kind": "text_input", "prompt": "What?"}),
    )
}

fn form_request(id: &str) -> contract::Envelope {
    envelope(
        "interaction_requested",
        json!({"request_id": id, "kind": "form", "fields": []}),
    )
}

fn approval_request(id: &str) -> contract::Envelope {
    envelope(
        "permission_requested",
        json!({"request_id": id, "effects": ["executes"], "reversible": true,
            "step": "standing_ask", "standing_rule": {"scope": "global", "prefix": "ls"}}),
    )
}

fn queue_with(request: contract::Envelope) -> Queue {
    let mut queue = Queue::default();
    queue.fold(&request);
    queue
}

/// A one-question title at 80 columns: no question count, no place suffix
/// with one request waiting.
fn one_title() -> String {
    let left = format!("Question from {S_A}");
    let pad = 80_usize
        .saturating_sub(crate::format::width(&left).saturating_add("Esc: chat about this".len()))
        .max(1);
    format!("{left}{}{}", " ".repeat(pad), "Esc: chat about this")
}

fn header(queue: &Queue) -> String {
    queue
        .panel(80)
        .and_then(|shown| shown.lines.first().cloned())
        .unwrap_or_default()
}

fn reply(line: &str) -> Value {
    serde_json::from_str(line).unwrap_or_default()
}

#[test]
fn each_one_question_kind_opens_in_the_shared_queue() {
    for request in [
        confirm_request("r_c"),
        select_request("r_s"),
        multi_select_request("r_m"),
        text_input_request("r_t"),
    ] {
        let queue = queue_with(request);
        assert!(queue.open());
        assert_eq!(header(&queue), one_title());
    }
}

#[test]
fn a_one_question_waits_behind_a_put_aside_approval() {
    let mut queue = queue_with(approval_request("r_a"));
    assert_eq!(queue.on_key(&Key::Esc), Some(PanelKey::Handled));
    queue.fold(&confirm_request("r_c"));
    assert!(!queue.open());
    assert_eq!(
        queue.badge(0).as_deref(),
        Some("  ⚠ 2 approvals waiting · click to reopen")
    );
}

#[test]
fn a_one_question_joins_the_queue_after_a_form_and_moves_up_when_answered() {
    let mut queue = queue_with(form_request("r_f"));
    queue.fold(&confirm_request("r_c"));
    let left = format!("Question from {S_A} · 0 questions, any can be skipped · 1 of 2");
    let pad = 80_usize
        .saturating_sub(crate::format::width(&left).saturating_add("Esc: chat about this".len()))
        .max(1);
    assert_eq!(
        header(&queue),
        format!("{left}{}{}", " ".repeat(pad), "Esc: chat about this")
    );
    assert!(queue.answer("c_f").is_some());
    assert_eq!(header(&queue), one_title());
}

#[test]
fn each_one_question_kind_answers_with_its_reply_key() {
    let mut confirm = queue_with(confirm_request("r_c"));
    assert_eq!(confirm.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(
        reply(&confirm.answer("c_c").unwrap_or_default()),
        json!({"id": "c_c", "command": "reply", "session_id": S_A,
            "args": {"request_id": "r_c", "confirmed": true}})
    );

    let mut select = queue_with(select_request("r_s"));
    assert_eq!(select.on_key(&Key::Down), Some(PanelKey::Handled));
    assert_eq!(select.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(
        reply(&select.answer("c_s").unwrap_or_default()),
        json!({"id": "c_s", "command": "reply", "session_id": S_A,
            "args": {"request_id": "r_s", "labels": ["b"]}})
    );

    let mut multi = queue_with(multi_select_request("r_m"));
    for key in [Key::Char(' '), Key::Down, Key::Down, Key::Char(' ')] {
        assert_eq!(multi.on_key(&key), Some(PanelKey::Handled), "{key:?}");
    }
    assert_eq!(multi.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(
        reply(&multi.answer("c_m").unwrap_or_default()),
        json!({"id": "c_m", "command": "reply", "session_id": S_A,
            "args": {"request_id": "r_m", "labels": ["a", "c"]}})
    );

    let mut text = queue_with(text_input_request("r_t"));
    for ch in "hi".chars() {
        assert_eq!(text.on_key(&Key::Char(ch)), Some(PanelKey::Handled));
    }
    assert_eq!(text.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(
        reply(&text.answer("c_t").unwrap_or_default()),
        json!({"id": "c_t", "command": "reply", "session_id": S_A,
            "args": {"request_id": "r_t", "text": "hi"}})
    );
}

#[test]
fn decline_sends_declined_and_records_each_one_question_session_once() {
    for request in [
        confirm_request("r_c"),
        select_request("r_s"),
        multi_select_request("r_m"),
        text_input_request("r_t"),
    ] {
        let request_id = request
            .payload
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        // One no tool call raised names no session: its decline cancels nothing.
        let mut outside = queue_with(request.clone());
        assert!(outside.decline("c_0").is_some());
        assert_eq!(outside.declined("c_0"), None);
        let mut request = request;
        request
            .payload
            .insert("action_ids".to_owned(), json!(["a_1"]));
        let mut queue = queue_with(request);
        assert_eq!(
            reply(&queue.decline("c_1").unwrap_or_default()),
            json!({"id": "c_1", "command": "reply", "session_id": S_A,
                "args": {"request_id": request_id, "declined": true}})
        );
        assert_eq!(
            queue.declined("c_1"),
            Some(contract::SessionId(S_A.to_owned()))
        );
        assert_eq!(queue.declined("c_1"), None);
    }
}

#[test]
fn restoring_a_text_input_reply_keeps_its_words() {
    let mut queue = queue_with(text_input_request("r_t"));
    queue.on_edit(&Edit::Paste("hi".to_owned()));
    assert!(queue.answer("c_t").is_some());
    queue.restore("c_t");
    assert!(queue.open());
    assert!(
        queue
            .panel(80)
            .is_some_and(|shown| shown.lines.contains(&"▸ 1 ✎ hi".to_owned()))
    );
}

#[test]
fn a_raised_again_text_input_keeps_its_words() {
    let mut queue = queue_with(text_input_request("r_t"));
    queue.on_edit(&Edit::Paste("hi".to_owned()));
    let mut raised = text_input_request("r_t");
    raised
        .payload
        .insert("resumes".to_owned(), Value::Bool(true));
    queue.fold(&raised);
    assert!(
        queue
            .panel(80)
            .is_some_and(|shown| shown.lines.contains(&"▸ 1 ✎ hi".to_owned()))
    );
}

#[test]
fn an_interaction_resolved_by_fiber_removes_a_one_question() {
    let mut queue = queue_with(text_input_request("r_t"));
    queue.fold(&envelope(
        "interaction_resolved",
        json!({"request_id": "r_t", "by": "fiber", "declined": true}),
    ));
    assert!(!queue.open());
    assert!(queue.badge(0).is_none());
}

#[test]
fn answering_a_select_from_chat_sends_nothing_and_keeps_it_waiting() {
    let mut queue = queue_with(select_request("r_s"));
    assert_eq!(queue.on_key(&Key::Down), Some(PanelKey::Handled));
    assert_eq!(queue.on_key(&Key::Down), Some(PanelKey::Handled));
    let before = header(&queue);
    assert_eq!(queue.answer("c_s"), None);
    assert!(queue.open());
    assert_eq!(header(&queue), before);
}

#[test]
fn a_non_digit_key_never_moves_the_cursor_to_an_option() {
    // ':' is the code past '9' that maps to the tenth option's index, so a
    // key that is not a digit must still do nothing.
    let choices: Vec<Choice> = (1..=10).map(|n| choice(&format!("opt{n}"), None)).collect();
    let mut question = OneQuestion::select("Pick one?".to_owned(), choices);
    let before = lines(&question);
    let before_answer = answer(&question);
    assert_eq!(question.on_key(&Key::Char(':')), Some(PanelKey::Handled));
    assert_eq!(lines(&question), before);
    assert_eq!(answer(&question), before_answer);
}

#[test]
fn the_words_row_scrolls_so_the_caret_stays_inside_the_width() {
    // Ten words after a six-cell prefix fill 16 columns exactly. The caret
    // needs one cell past them, so the row drops the first word.
    let mut question = OneQuestion::text_input("Words?".to_owned());
    type_text(&mut question, "abcdefghij");
    let width = crate::format::width("▸ 1 ✎ ") + 10;
    let shown = question.panel(&title(), u16::try_from(width).unwrap_or(u16::MAX));
    assert_eq!(
        shown.lines.get(4).map(String::as_str),
        Some("▸ 1 ✎ bcdefghij")
    );
}
