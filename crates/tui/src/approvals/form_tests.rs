//! Tests for the question form: keys, paste, the answer and its lines, and
//! forms in the queue.

use serde_json::{Value, json};

use super::{Form, Spot};
use crate::approvals::{PanelKey, Queue};
use crate::keys::{Edit, Key};

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

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

/// The lines under the header `h`.
fn lines(form: &Form) -> Vec<String> {
    form.panel("h".to_owned(), 80).lines
}

/// The line at `row`.
fn line(form: &Form, row: usize) -> String {
    lines(form).get(row).cloned().unwrap_or_default()
}

/// The tab line.
fn tabs(form: &Form) -> String {
    line(form, 1)
}

/// The line the cursor `›` is on.
fn cursor(form: &Form) -> String {
    lines(form)
        .into_iter()
        .find(|line| line.starts_with('›'))
        .unwrap_or_default()
}

#[test]
fn the_first_question_shows_its_options_words_next_and_chat() {
    let form = base_and_name(true);
    assert_eq!(
        lines(&form),
        [
            "h",
            "[Base]  Name  Submit",
            "Which branch?",
            "› [ ] main (Recommended) · the default",
            "  [ ] dev",
            "  ✎ answer in words",
            "  Next →",
            "  Chat about this",
        ]
    );
}

#[test]
fn enter_on_a_single_choice_option_chooses_it_and_moves_on() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Down, Key::Enter]);
    assert_eq!(tabs(&form), "Base ✓  [Name]  Submit");
    assert_eq!(cursor(&form), "› ✎ answer in words");
    assert_eq!(answer(&form)["answers"][0], json!({"labels": ["dev"]}));
    // Enter on another option replaces the choice.
    press(&mut form, &[Key::BackTab, Key::Enter]);
    assert_eq!(
        answer(&form)["answers"][0],
        json!({"labels": ["main (Recommended)"]})
    );
    press(&mut form, &[Key::BackTab]);
    assert_eq!(line(&form, 3), "› (•) main (Recommended) · the default");
    assert_eq!(line(&form, 4), "  ( ) dev");
}

#[test]
fn space_toggles_a_multi_choice_option_and_enter_moves_on_without_toggling() {
    let mut form = three_options();
    press(&mut form, &[Key::Char(' ')]);
    assert_eq!(cursor(&form), "› [x] a");
    press(&mut form, &[Key::Char(' ')]);
    assert_eq!(cursor(&form), "› [ ] a");
    // Toggled out of order, the labels come back in option order.
    press(&mut form, &[Key::Down, Key::Down, Key::Char(' ')]);
    press(&mut form, &[Key::Up, Key::Up, Key::Char(' ')]);
    press(&mut form, &[Key::Down, Key::Enter]);
    assert_eq!(tabs(&form), "Pick ✓  [Submit]");
    assert_eq!(answer(&form)["answers"], json!([{"labels": ["a", "c"]}]));
}

#[test]
fn space_on_a_single_choice_option_chooses_it_and_stays_then_clears_it() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Down, Key::Char(' ')]);
    assert_eq!(cursor(&form), "› (•) dev");
    assert_eq!(answer(&form)["answers"][0], json!({"labels": ["dev"]}));
    // Space on another option moves the choice.
    press(&mut form, &[Key::Up, Key::Char(' ')]);
    assert_eq!(
        answer(&form)["answers"][0],
        json!({"labels": ["main (Recommended)"]})
    );
    press(&mut form, &[Key::Char(' ')]);
    assert_eq!(cursor(&form), "› ( ) main (Recommended) · the default");
    assert_eq!(answer(&form)["answers"][0], json!({"skipped": true}));
}

#[test]
fn a_character_off_the_words_row_is_typed_on_it() {
    // An option, `Next →` and "Chat about this".
    for downs in [0, 3, 4] {
        let mut form = base_and_name(true);
        press(&mut form, &vec![Key::Down; downs]);
        type_text(&mut form, "x");
        assert_eq!(cursor(&form), "› ✎ x", "{downs}");
        assert_eq!(
            answer(&form)["answers"][0],
            json!({"labels": [], "text": "x"})
        );
    }
}

#[test]
fn space_on_next_and_chat_does_nothing_and_on_the_words_row_types() {
    for downs in [3, 4] {
        let mut form = base_and_name(true);
        press(&mut form, &vec![Key::Down; downs]);
        let before = lines(&form);
        press(&mut form, &[Key::Char(' ')]);
        assert_eq!(lines(&form), before, "{downs}");
    }
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Down, Key::Down]);
    type_text(&mut form, "a b");
    assert_eq!(cursor(&form), "› ✎ a b");
}

#[test]
fn backspace_deletes_on_the_words_row_only() {
    let mut form = base_and_name(true);
    type_text(&mut form, "ab");
    press(&mut form, &[Key::Backspace]);
    assert_eq!(cursor(&form), "› ✎ a");
    press(&mut form, &[Key::Up, Key::Backspace]);
    assert_eq!(line(&form, 5), "  ✎ a");
    press(&mut form, &[Key::Down, Key::Down, Key::Backspace]);
    assert_eq!(line(&form, 5), "  ✎ a");
}

#[test]
fn up_and_down_stop_at_the_first_and_last_rows() {
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Up]);
    assert_eq!(cursor(&form), "› [ ] main (Recommended) · the default");
    // ↓ from the last option reaches the words row.
    press(&mut form, &[Key::Down, Key::Down]);
    assert_eq!(cursor(&form), "› ✎ answer in words");
    press(&mut form, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(cursor(&form), "› Chat about this");
    press(&mut form, &[Key::Tab, Key::Tab]);
    press(&mut form, &[Key::Up, Key::Up]);
    assert_eq!(cursor(&form), "› note · type to add a note");
    press(&mut form, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(cursor(&form), "› Chat about this");
}

#[test]
fn tab_and_shift_tab_move_between_tabs_and_stop_at_the_ends() {
    let mut form = base_and_name(true);
    press(&mut form, &[Key::BackTab]);
    assert_eq!(tabs(&form), "[Base]  Name  Submit");
    // From any row, onto the next tab's landing row.
    press(&mut form, &[Key::Down, Key::Down, Key::Tab]);
    assert_eq!(tabs(&form), "Base  [Name]  Submit");
    assert_eq!(cursor(&form), "› ✎ answer in words");
    press(&mut form, &[Key::Tab]);
    assert_eq!(tabs(&form), "Base  Name  [Submit]");
    assert_eq!(cursor(&form), "› Submit");
    press(&mut form, &[Key::Tab]);
    assert_eq!(tabs(&form), "Base  Name  [Submit]");
    press(&mut form, &[Key::BackTab]);
    assert_eq!(tabs(&form), "Base  [Name]  Submit");
    press(&mut form, &[Key::BackTab]);
    assert_eq!(cursor(&form), "› [ ] main (Recommended) · the default");
}

#[test]
fn a_free_text_question_lands_on_its_words_row_and_has_no_options() {
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Tab]);
    assert_eq!(
        lines(&form),
        [
            "h",
            "Base  [Name]  Submit",
            "What name?",
            "› ✎ answer in words",
            "  Review →",
            "  Chat about this",
        ]
    );
}

#[test]
fn the_last_question_reads_review_and_enter_on_it_lands_on_submit() {
    let mut form = base_and_name(true);
    press(&mut form, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(cursor(&form), "› Next →");
    press(&mut form, &[Key::Enter]);
    assert_eq!(tabs(&form), "Base  [Name]  Submit");
    press(&mut form, &[Key::Down]);
    assert_eq!(cursor(&form), "› Review →");
    press(&mut form, &[Key::Enter]);
    assert_eq!(tabs(&form), "Base  Name  [Submit]");
    assert_eq!(cursor(&form), "› Submit");
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
    assert_eq!(tabs(&form), "Pick  [Submit]");
}

#[test]
fn a_tab_is_marked_when_its_answer_is_not_skipped() {
    let mut form = base_and_name(true);
    assert_eq!(tabs(&form), "[Base]  Name  Submit");
    press(&mut form, &[Key::Char(' ')]);
    assert_eq!(tabs(&form), "[Base ✓]  Name  Submit");
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "x");
    assert_eq!(tabs(&form), "Base ✓  [Name ✓]  Submit");
    press(&mut form, &[Key::Backspace]);
    assert_eq!(tabs(&form), "Base ✓  [Name]  Submit");
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
            "h",
            "Base ✓  Name  [Submit]",
            r#"Base: main (Recommended), dev, "and tags""#,
            "Name: skipped",
            "  note · type to add a note",
            "› Submit",
            "  Chat about this",
        ]
    );
    // A character on `Submit` or "Chat about this" goes to the note, Space
    // included.
    type_text(&mut form, "by");
    press(&mut form, &[Key::Down, Key::Down, Key::Char(' ')]);
    press(&mut form, &[Key::Down, Key::Down]);
    type_text(&mut form, "x");
    assert_eq!(cursor(&form), r#"› note: "by x""#);
    press(&mut form, &[Key::Backspace]);
    assert_eq!(cursor(&form), r#"› note: "by ""#);
    // Backspace off the note row does nothing.
    press(&mut form, &[Key::Down, Key::Backspace]);
    assert_eq!(line(&form, 4), r#"  note: "by ""#);
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
fn esc_and_chat_about_this_decline() {
    let mut form = base_and_name(true);
    assert_eq!(form.on_key(&Key::Esc), Some(PanelKey::Decline));
    press(&mut form, &[Key::Down, Key::Down, Key::Down, Key::Down]);
    assert_eq!(cursor(&form), "› Chat about this");
    assert_eq!(form.on_key(&Key::Enter), Some(PanelKey::Decline));
    press(&mut form, &[Key::Tab, Key::Tab, Key::Down]);
    assert_eq!(cursor(&form), "› Chat about this");
    assert_eq!(form.on_key(&Key::Enter), Some(PanelKey::Decline));
    assert_eq!(form.on_key(&Key::Esc), Some(PanelKey::Decline));
}

#[test]
fn every_key_is_the_forms_or_passes_through() {
    let handled = Some(PanelKey::Handled);
    let keys = [
        (Key::Char('x'), handled),
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
            Key::AltA | Key::CtrlG | Key::CtrlR | Key::AltUp | Key::AltDown | Key::AltX
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
    assert_eq!(cursor(&form), "› ✎  a b");
    assert_eq!(line(&form, 3), "  [ ] a");
    press(&mut form, &[Key::Tab]);
    form.on_edit(&Edit::Paste("by\tnow".to_owned()));
    assert_eq!(cursor(&form), r#"› note: "by now""#);
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
    assert_eq!(
        lines(&form),
        [
            "h",
            "[Pi ck]  Submit",
            "Which one?",
            "› ( ) a b · x [2J",
            "  ( ) c d",
            "  ✎ answer in words",
            "  Review →",
            "  Chat about this",
        ]
    );
}

#[test]
fn an_empty_form_opens_on_submit_and_answers_nothing() {
    let mut form = form(json!([]));
    assert_eq!(
        lines(&form),
        [
            "h",
            "[Submit]",
            "  note · type to add a note",
            "› Submit",
            "  Chat about this",
        ]
    );
    assert_eq!(form.on_key(&Key::Enter), Some(PanelKey::Answer));
    assert_eq!(answer(&form), json!({"answers": []}));
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
fn panel(queue: &Queue) -> Vec<String> {
    queue.panel(80).map(|panel| panel.lines).unwrap_or_default()
}

/// The panel's header, or nothing when it is closed.
fn header(queue: &Queue) -> String {
    panel(queue).first().cloned().unwrap_or_default()
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
        Some("! 2 waiting · /approvals or ⌥A")
    );
    assert!(queue.open_first());
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 2"));
    assert_eq!(queue.on_key(&Key::Esc), Some(PanelKey::Handled));
    assert_eq!(header(&queue), format!("question · {S_A} · 2 of 2"));
    assert_eq!(queue.panel(80).map(|panel| panel.alert), Some(false));
    // ⌥A moves on from the form as from an approval.
    assert_eq!(queue.on_key(&Key::AltA), Some(PanelKey::Handled));
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 2"));
}

#[test]
fn a_resolved_form_leaves_the_queue() {
    let mut queue = folded(&[asked("r_4f", json!({}))]);
    assert_eq!(header(&queue), format!("question · {S_A} · 1 of 1"));
    queue.fold(&envelope(
        "interaction_resolved",
        json!({"request_id": "r_9", "by": "person", "declined": true}),
    ));
    assert_eq!(header(&queue), format!("question · {S_A} · 1 of 1"));
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
fn a_confirm_interaction_is_not_queued() {
    let options = json!([{"label": "a"}, {"label": "b"}]);
    for payload in [
        json!({"request_id": "r_1", "kind": "confirm", "prompt": "Sure?"}),
        json!({"request_id": "r_2", "kind": "select", "prompt": "One?", "options": options}),
        json!({"request_id": "r_3", "kind": "multi_select", "prompt": "Some?",
            "options": options}),
        json!({"request_id": "r_4", "kind": "text_input", "prompt": "What?"}),
    ] {
        let queue = folded(&[envelope("interaction_requested", payload.clone())]);
        assert!(queue.panel(80).is_none(), "{payload}");
        assert!(queue.badge(0).is_none(), "{payload}");
    }
}

#[test]
fn a_re_raised_form_keeps_what_was_typed() {
    let mut queue = folded(&[asked("r_4f", json!({"action_ids": ["a_1"]}))]);
    assert_eq!(queue.on_key(&Key::Char('x')), Some(PanelKey::Handled));
    queue.fold(&asked(
        "r_4f",
        json!({"action_ids": ["a_1"], "resumes": true}),
    ));
    let lines = panel(&queue);
    assert_eq!(lines.first(), Some(&format!("question · {S_A} · 1 of 1")));
    assert!(lines.contains(&"› ✎ x".to_owned()), "{lines:?}");
}

#[test]
fn a_form_from_an_extension_queues() {
    let queue = folded(&[asked("r_4f", json!({"extension": "deploy"}))]);
    assert_eq!(header(&queue), format!("question · {S_A} · 1 of 1"));
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
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 1"));
    let mut queue = folded(&[asked("r_4f", json!({}))]);
    let line = queue.decline("c_1").unwrap_or_default();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap_or_default(),
        json!({"id": "c_1", "command": "reply", "session_id": S_A,
            "args": {"request_id": "r_4f", "declined": true}})
    );
    assert!(queue.panel(80).is_none());
    // A rejected decline puts the form back and cancels nothing.
    queue.restore("c_1");
    assert_eq!(header(&queue), format!("question · {S_A} · 1 of 1"));
    assert_eq!(queue.declined("c_1"), None);
    // An accepted decline names its session once.
    assert!(queue.decline("c_2").is_some());
    assert_eq!(
        queue.declined("c_2"),
        Some(contract::SessionId(S_A.to_owned()))
    );
    assert_eq!(queue.declined("c_2"), None);
}

#[test]
fn a_shown_request_takes_every_edit() {
    assert!(!Queue::default().open());
    let mut queue = folded(&[asked("r_4f", json!({}))]);
    assert!(queue.open());
    queue.on_edit(&Edit::Paste("x".to_owned()));
    let lines = panel(&queue);
    assert!(lines.contains(&"› ✎ x".to_owned()), "{lines:?}");
}

#[test]
fn a_click_on_a_row_the_shown_tab_lacks_does_nothing() {
    let mut form = base_and_name(false);
    let before = lines(&form);
    for spot in [Spot::Option(2), Spot::Note, Spot::Send] {
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
    assert_eq!(cursor(&form), "› Submit");
    assert_eq!(form.click(Spot::Note), PanelKey::Handled);
    assert_eq!(cursor(&form), "› note · type to add a note");
    assert_eq!(form.click(Spot::Send), PanelKey::Answer);
    assert_eq!(form.click(Spot::Chat), PanelKey::Decline);
    assert_eq!(cursor(&form), "› Chat about this");
}

#[test]
fn the_panel_marks_the_cursor_line_and_each_rows_spot() {
    let form = base_and_name(false);
    let panel = form.panel("h".to_owned(), 80);
    assert_eq!(panel.cursor, Some(3));
    let spots: Vec<_> = panel
        .spots
        .iter()
        .map(|spot| (spot.line, spot.cols, spot.spot))
        .collect();
    assert_eq!(
        spots,
        [
            (1, Some((0, 6)), Spot::Tab(0)),
            (1, Some((8, 12)), Spot::Tab(1)),
            (1, Some((14, 20)), Spot::Tab(2)),
            (3, None, Spot::Option(0)),
            (4, None, Spot::Option(1)),
            (5, None, Spot::Words),
            (6, None, Spot::Next),
            (7, None, Spot::Chat),
        ]
    );
    let mut form = form;
    press(&mut form, &[Key::BackTab, Key::Tab, Key::Tab, Key::Up]);
    let panel = form.panel("h".to_owned(), 80);
    assert_eq!(panel.cursor, Some(4));
    let rows: Vec<(usize, Spot)> = panel
        .spots
        .iter()
        .filter(|spot| spot.cols.is_none())
        .map(|spot| (spot.line, spot.spot))
        .collect();
    assert_eq!(rows, [(4, Spot::Note), (5, Spot::Send), (6, Spot::Chat)]);
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
    let panel = form.panel("h".to_owned(), cols);
    let line = panel
        .spots
        .iter()
        .find(|spot| spot.spot == Spot::Words)
        .and_then(|spot| panel.lines.get(spot.line).cloned())
        .unwrap_or_default();
    (line, panel.caret.map(|(_, col)| col))
}

#[test]
fn left_and_right_on_an_option_row_move_between_tabs_and_stop_at_the_ends() {
    let mut form = two_choices();
    arrows(&mut form, &[true]);
    assert_eq!(tabs(&form), "A  [B]  Submit");
    assert_eq!(cursor(&form), "› ( ) x");
    arrows(&mut form, &[true, true]);
    assert_eq!(tabs(&form), "A  B  [Submit]");
    arrows(&mut form, &[false]);
    assert_eq!(tabs(&form), "A  [B]  Submit");
    arrows(&mut form, &[false, false]);
    assert_eq!(tabs(&form), "[A]  B  Submit");
}

#[test]
fn left_and_right_on_next_and_chat_move_between_tabs() {
    let mut form = two_choices();
    press(&mut form, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(cursor(&form), "› Next →");
    arrows(&mut form, &[true]);
    assert_eq!(tabs(&form), "A  [B]  Submit");
    press(&mut form, &[Key::Down, Key::Down, Key::Down, Key::Down]);
    assert_eq!(cursor(&form), "› Chat about this");
    arrows(&mut form, &[false]);
    assert_eq!(tabs(&form), "[A]  B  Submit");
}

#[test]
fn left_and_right_on_every_submit_row_move_between_tabs() {
    // `Tab` from the last question lands on Send; `Up` from Send lands on
    // the note, `Down` on Chat.
    let mut form = two_choices();
    press(&mut form, &[Key::Tab, Key::Tab]);
    arrows(&mut form, &[false]);
    assert_eq!(tabs(&form), "A  [B]  Submit");
    let mut form = two_choices();
    press(&mut form, &[Key::Tab, Key::Tab, Key::Down]);
    arrows(&mut form, &[false]);
    assert_eq!(tabs(&form), "A  [B]  Submit");
}

#[test]
fn left_and_right_on_the_note_move_the_note_cursor_and_keep_the_tab() {
    let mut form = two_choices();
    press(&mut form, &[Key::Tab, Key::Tab, Key::Up]);
    type_text(&mut form, "ab");
    assert_eq!(cursor(&form), r#"› note: "ab""#);
    arrows(&mut form, &[false]);
    assert_eq!(tabs(&form), "A  B  [Submit]");
    type_text(&mut form, "x");
    assert_eq!(cursor(&form), r#"› note: "axb""#);
    arrows(&mut form, &[false, false, false, false]);
    arrows(&mut form, &[true, true, true, true, true, true]);
    assert_eq!(tabs(&form), "A  B  [Submit]");
    type_text(&mut form, "z");
    assert_eq!(cursor(&form), r#"› note: "axbz""#);
    arrows(&mut form, &[false]);
    type_text(&mut form, "w");
    assert_eq!(cursor(&form), r#"› note: "axbwz""#);
}

#[test]
fn shift_tab_and_tab_leave_the_note_row() {
    let mut form = two_choices();
    press(&mut form, &[Key::Tab, Key::Tab, Key::Up]);
    press(&mut form, &[Key::BackTab]);
    assert_eq!(tabs(&form), "A  [B]  Submit");
    press(&mut form, &[Key::Tab]);
    assert_eq!(tabs(&form), "A  B  [Submit]");
    assert_eq!(cursor(&form), "› Submit");
}

#[test]
fn backspace_on_the_note_removes_the_char_before_the_cursor() {
    let mut form = two_choices();
    press(&mut form, &[Key::Tab, Key::Tab, Key::Up]);
    type_text(&mut form, "abc");
    arrows(&mut form, &[false]);
    press(&mut form, &[Key::Backspace]);
    assert_eq!(cursor(&form), r#"› note: "ac""#);
    arrows(&mut form, &[false, false, false]);
    press(&mut form, &[Key::Backspace]);
    assert_eq!(cursor(&form), r#"› note: "ac""#);
}

#[test]
fn left_and_right_on_the_words_row_move_the_text_cursor_and_stop_at_the_ends() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "abc");
    assert_eq!(words_at(&form, 60), ("› ✎ abc".to_owned(), Some(7)));
    arrows(&mut form, &[true]);
    assert_eq!(words_at(&form, 60).1, Some(7));
    arrows(&mut form, &[false, false]);
    assert_eq!(words_at(&form, 60).1, Some(5));
    arrows(&mut form, &[false, false]);
    assert_eq!(words_at(&form, 60).1, Some(4));
    assert_eq!(tabs(&form), "Base  [Name ✓]  Submit");
    arrows(&mut form, &[true]);
    assert_eq!(words_at(&form, 60).1, Some(5));
}

#[test]
fn typing_and_a_paste_insert_at_the_text_cursor() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "ad");
    arrows(&mut form, &[false]);
    type_text(&mut form, "b");
    form.on_edit(&Edit::Paste("c\n".to_owned()));
    assert_eq!(words_at(&form, 60), ("› ✎ abc d".to_owned(), Some(8)));
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
    assert_eq!(words_at(&form, 60), ("› ✎ ac".to_owned(), Some(5)));
    arrows(&mut form, &[false]);
    press(&mut form, &[Key::Backspace]);
    assert_eq!(words_at(&form, 60), ("› ✎ ac".to_owned(), Some(4)));
}

#[test]
fn a_click_on_the_words_row_puts_the_text_cursor_at_the_end() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "abc");
    arrows(&mut form, &[false, false]);
    press(&mut form, &[Key::Down]);
    assert_eq!(form.click(Spot::Words), PanelKey::Handled);
    assert_eq!(words_at(&form, 60).1, Some(7));
}

#[test]
fn the_words_row_scrolls_only_when_the_text_cursor_would_reach_the_width() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "abcdefgh");
    // The text cursor after `h` is at column 12.
    assert_eq!(words_at(&form, 14), ("› ✎ abcdefgh".to_owned(), Some(12)));
    assert_eq!(words_at(&form, 13), ("› ✎ abcdefgh".to_owned(), Some(12)));
    assert_eq!(words_at(&form, 12), ("› ✎ bcdefgh".to_owned(), Some(11)));
    // Off the row, the words show from their start, clipped.
    press(&mut form, &[Key::Down]);
    assert_eq!(words_at(&form, 8), ("  ✎ abcd".to_owned(), None));
}

#[test]
fn a_wide_character_counts_two_cells_on_the_words_row() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "名前");
    assert_eq!(words_at(&form, 9), ("› ✎ 名前".to_owned(), Some(8)));
    assert_eq!(words_at(&form, 8), ("› ✎ 前".to_owned(), Some(6)));
}

#[test]
fn a_words_row_narrower_than_its_mark_shows_the_mark_only() {
    let mut form = base_and_name(false);
    press(&mut form, &[Key::Tab]);
    type_text(&mut form, "ab");
    assert_eq!(words_at(&form, 3), ("› ✎".to_owned(), Some(4)));
}

#[test]
fn the_panel_places_the_caret_on_the_words_line() {
    let mut form = base_and_name(false);
    assert_eq!(form.panel("h".to_owned(), 60).caret, None);
    press(&mut form, &[Key::Down, Key::Down]);
    assert_eq!(form.panel("h".to_owned(), 60).caret, Some((5, 4)));
}
