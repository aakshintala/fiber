//! Tests for one-question interactions on the request panel.

use contract::commands::ReplyAnswer;
use contract::shapes::Choice;
use serde_json::{Value, json};

use super::OneQuestion;
use crate::approvals::form::Spot;
use crate::approvals::PanelKey;
use crate::keys::{Edit, Key};

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

fn lines(question: &OneQuestion) -> Vec<String> {
    question.panel("h".to_owned(), 80).lines
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
        lines(&confirm),
        [
            "h",
            "Continue?",
            "› ( ) yes",
            "  ( ) no",
            "  Chat about this",
        ]
    );

    let select = OneQuestion::select(
        "Pick\none?".to_owned(),
        vec![choice("alpha\nversion", Some("first\nchoice")), choice("beta", None)],
    );
    assert_eq!(
        lines(&select),
        [
            "h",
            "Pick one?",
            "› ( ) alpha version · first choice",
            "  ( ) beta",
            "  Chat about this",
        ]
    );

    let multi = OneQuestion::multi_select(
        "Pick some?".to_owned(),
        vec![choice("alpha", Some("first option")), choice("beta", None)],
    );
    assert_eq!(
        lines(&multi),
        [
            "h",
            "Pick some?",
            "› [ ] alpha · first option",
            "  [ ] beta",
            "  Submit",
            "  Chat about this",
        ]
    );

    let text = OneQuestion::text_input("What should it say?".to_owned());
    assert_eq!(
        lines(&text),
        [
            "h",
            "What should it say?",
            "› ✎ answer in words",
            "  Submit",
            "  Chat about this",
        ]
    );
}

#[test]
fn initial_cursor_is_on_the_first_actionable_row() {
    for question in [
        OneQuestion::confirm("Continue?".to_owned()),
        select(),
        multi_select(),
    ] {
        assert_eq!(question.panel("h".to_owned(), 80).cursor, Some(2));
    }
    assert_eq!(
        OneQuestion::text_input("Words?".to_owned())
            .panel("h".to_owned(), 80)
            .cursor,
        Some(2)
    );
    assert_eq!(
        OneQuestion::select("None?".to_owned(), Vec::new()).panel("h".to_owned(), 80).cursor,
        Some(2)
    );
    assert_eq!(
        OneQuestion::multi_select("None?".to_owned(), Vec::new())
            .panel("h".to_owned(), 80)
            .cursor,
        Some(2)
    );
    assert_eq!(
        lines(&OneQuestion::select("None?".to_owned(), Vec::new())),
        ["h", "None?", "› Chat about this"]
    );
    assert_eq!(
        lines(&OneQuestion::multi_select("None?".to_owned(), Vec::new())),
        ["h", "None?", "› Submit", "  Chat about this"]
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
        &[
            Key::Char(' '),
            Key::Down,
            Key::Down,
            Key::Char(' '),
        ],
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
    assert_eq!(lines(&question).get(5).map(String::as_str), Some("› Submit"));
    assert_eq!(question.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(answer(&question), json!({"labels": []}));
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
    assert_eq!(lines(&question).get(2).map(String::as_str), Some("› ✎ a"));
    question.on_edit(&Edit::Left);
    question.on_edit(&Edit::Left);
    type_text(&mut question, "x");
    assert_eq!(lines(&question).get(2).map(String::as_str), Some("› ✎ xa"));
    question.on_edit(&Edit::Right);
    question.on_edit(&Edit::Right);
    assert_eq!(
        question.panel("h".to_owned(), 80).caret,
        Some((2, 6))
    );
    question.on_edit(&Edit::Left);
    assert_eq!(
        question.panel("h".to_owned(), 80).caret,
        Some((2, 5))
    );
    question.on_edit(&Edit::Left);
    assert_eq!(
        question.panel("h".to_owned(), 80).caret,
        Some((2, 4))
    );
}

#[test]
fn text_cursor_edits_do_nothing_off_the_words_row() {
    let mut question = OneQuestion::text_input("Words?".to_owned());
    type_text(&mut question, "ab");
    press(&mut question, &[Key::Down]);
    let submit = question.panel("h".to_owned(), 80);
    press(&mut question, &[Key::Backspace]);
    question.on_edit(&Edit::Left);
    question.on_edit(&Edit::Right);
    assert_eq!(question.panel("h".to_owned(), 80), submit);
    press(&mut question, &[Key::Down]);
    let chat = question.panel("h".to_owned(), 80);
    press(&mut question, &[Key::Backspace]);
    question.on_edit(&Edit::Left);
    question.on_edit(&Edit::Right);
    assert_eq!(question.panel("h".to_owned(), 80), chat);
}

#[test]
fn text_input_paste_turns_control_characters_into_spaces() {
    let mut question = OneQuestion::text_input("Words?".to_owned());
    question.on_edit(&Edit::Paste("a\nb\tc".to_owned()));
    assert_eq!(lines(&question).get(2).map(String::as_str), Some("› ✎ a b c"));
    press(&mut question, &[Key::Char(' ')]);
    assert_eq!(lines(&question).get(2).map(String::as_str), Some("› ✎ a b c "));
}

#[test]
fn text_input_enter_on_submit_sends_the_answer() {
    let mut question = OneQuestion::text_input("Words?".to_owned());
    press(&mut question, &[Key::Down]);
    assert_eq!(lines(&question).get(3).map(String::as_str), Some("› Submit"));
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
        assert_eq!(lines(&question).last().map(String::as_str), Some("› Chat about this"));
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
        press(&mut question, &[Key::Char('x'), Key::Char(' '), Key::Backspace]);
        assert_eq!(lines(&question), before);
        assert_eq!(question.answer(), before_answer);
    }

    let mut question = multi_select();
    let before = lines(&question);
    let before_answer = question.answer();
    press(&mut question, &[Key::Tab, Key::BackTab, Key::Char('x'), Key::Backspace]);
    question.on_edit(&Edit::Paste("x\ny".to_owned()));
    assert_eq!(lines(&question), before);
    assert_eq!(question.answer(), before_answer);
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
    assert!(lines(&question).get(3).is_some_and(|line| line.starts_with("› [x] beta")));
    assert_eq!(question.click(Spot::Send), PanelKey::Answer);
    assert_eq!(answer(&question), json!({"labels": ["beta"]}));

    let mut question = OneQuestion::text_input("Words?".to_owned());
    type_text(&mut question, "hi");
    question.on_edit(&Edit::Left);
    assert_eq!(question.click(Spot::Words), PanelKey::Handled);
    assert_eq!(question.panel("h".to_owned(), 80).caret, Some((2, 6)));
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
            let before = question.panel("h".to_owned(), 80);
            let before_answer = question.answer();
            assert_eq!(question.click(spot), PanelKey::Handled, "{spot:?}");
            assert_eq!(question.panel("h".to_owned(), 80), before, "{spot:?}");
            assert_eq!(question.answer(), before_answer, "{spot:?}");
        }
    }

    let mut multi = multi_select();
    for spot in [Spot::Tab(0), Spot::Next, Spot::Note, Spot::Words, Spot::Option(9)] {
        let before = multi.panel("h".to_owned(), 80);
        let before_answer = multi.answer();
        assert_eq!(multi.click(spot), PanelKey::Handled, "{spot:?}");
        assert_eq!(multi.panel("h".to_owned(), 80), before, "{spot:?}");
        assert_eq!(multi.answer(), before_answer, "{spot:?}");
    }

    let mut text = OneQuestion::text_input("Words?".to_owned());
    for spot in [Spot::Tab(0), Spot::Next, Spot::Note, Spot::Option(9)] {
        let before = text.panel("h".to_owned(), 80);
        let before_answer = text.answer();
        assert_eq!(text.click(spot), PanelKey::Handled, "{spot:?}");
        assert_eq!(text.panel("h".to_owned(), 80), before, "{spot:?}");
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
    let panel = confirm.panel("h".to_owned(), 80);
    assert_eq!(panel.cursor, Some(2));
    assert_eq!(panel.caret, None);
    assert_eq!(
        panel
            .spots
            .iter()
            .map(|spot| (spot.line, spot.cols, spot.spot))
            .collect::<Vec<_>>(),
        [
            (2, None, Spot::Option(0)),
            (3, None, Spot::Option(1)),
            (4, None, Spot::Chat),
        ]
    );

    let question = select();
    let panel = question.panel("h".to_owned(), 80);
    assert_eq!(panel.cursor, Some(2));
    assert_eq!(panel.caret, None);
    assert_eq!(
        panel
            .spots
            .iter()
            .map(|spot| (spot.line, spot.cols, spot.spot))
            .collect::<Vec<_>>(),
        [
            (2, None, Spot::Option(0)),
            (3, None, Spot::Option(1)),
            (4, None, Spot::Option(2)),
            (5, None, Spot::Chat),
        ]
    );

    let multi = multi_select();
    let panel = multi.panel("h".to_owned(), 80);
    assert_eq!(panel.cursor, Some(2));
    assert_eq!(panel.caret, None);
    assert_eq!(
        panel
            .spots
            .iter()
            .map(|spot| (spot.line, spot.cols, spot.spot))
            .collect::<Vec<_>>(),
        [
            (2, None, Spot::Option(0)),
            (3, None, Spot::Option(1)),
            (4, None, Spot::Option(2)),
            (5, None, Spot::Send),
            (6, None, Spot::Chat),
        ]
    );

    let mut text = OneQuestion::text_input("Words?".to_owned());
    type_text(&mut text, "hi");
    let panel = text.panel("h".to_owned(), 80);
    assert_eq!(panel.cursor, Some(2));
    assert_eq!(panel.caret, Some((2, 6)));
    assert_eq!(
        panel
            .spots
            .iter()
            .map(|spot| (spot.line, spot.cols, spot.spot))
            .collect::<Vec<_>>(),
        [
            (2, None, Spot::Words),
            (3, None, Spot::Send),
            (4, None, Spot::Chat),
        ]
    );
}

#[test]
fn answer_variant_types_match_the_kind() {
    let confirm = OneQuestion::confirm("Continue?".to_owned());
    assert!(matches!(confirm.answer(), Some(ReplyAnswer::Confirmed { .. })));
    let select = select();
    assert!(matches!(select.answer(), Some(ReplyAnswer::Labels { .. })));
    let multi = multi_select();
    assert!(matches!(multi.answer(), Some(ReplyAnswer::Labels { .. })));
    let text = OneQuestion::text_input("Words?".to_owned());
    assert!(matches!(text.answer(), Some(ReplyAnswer::Text { .. })));
}
