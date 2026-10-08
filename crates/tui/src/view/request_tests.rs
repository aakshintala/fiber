//! The request panel's frame: question form snapshots, scrolling, its click
//! targets and the text cursor.

use std::path::PathBuf;

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::super::{render, text};
use super::draw;
use crate::app::{App, Effect};
use crate::approvals::form::Spot;
use crate::approvals::{Panel, PanelSpot};
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::{Target, TargetId};

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

/// A session line of `kind` from [`S_A`].
fn session_line(kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(S_A.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// An app connected, attached to [`S_A`], `width` by `height`.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(S_A.to_owned()));
    app.set_size(width, height);
    app
}

/// An app `width` by `height` showing the form `r_4f` over `fields`.
fn asking(fields: Value, width: u16, height: u16) -> App {
    let mut app = attached(width, height);
    ask(&mut app, fields);
    assert!(app.panel().is_some());
    app
}

/// Folds the form `r_4f` over `fields`.
fn ask(app: &mut App, fields: Value) {
    app.on_line(session_line(
        "interaction_requested",
        json!({"request_id": "r_4f", "kind": "form", "action_ids": ["a_1"], "fields": fields}),
    ));
}

/// `Base` with two options, multi-choice when `multi`, then free-text
/// `Name`.
fn base_and_name(multi: bool) -> Value {
    json!([
        {"header": "Base", "question": "Which branch?", "multiSelect": multi, "options": [
            {"label": "main (Recommended)", "description": "the default"},
            {"label": "dev"}]},
        {"header": "Name", "question": "What name?"}
    ])
}

/// One single-choice question with four options with long descriptions.
fn four_long_options() -> Value {
    let option = |label: &str| {
        json!({"label": label,
            "description": "a description long enough to wrap at forty columns"})
    };
    json!([{"header": "Pick", "question": "Which one?",
        "options": [option("one"), option("two"), option("three"), option("four")]}])
}

/// Draws `app` at `width` by `height`, returning the screen and the
/// targets.
fn screen(app: &App, width: u16, height: u16) -> (String, Vec<Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = render(app, area, &mut buf, None);
    (text(&buf), targets)
}

/// The form's targets drawn, in draw order.
fn spots(targets: &[Target]) -> Vec<(Spot, Rect)> {
    targets
        .iter()
        .filter_map(|target| {
            if let TargetId::Form(spot) = target.id {
                Some((spot, target.rect))
            } else {
                None
            }
        })
        .collect()
}

/// The cells of the target `spot`.
fn rect_of(targets: &[Target], spot: Spot) -> Option<Rect> {
    targets
        .iter()
        .find(|target| target.id == TargetId::Form(spot))
        .map(|target| target.rect)
}

/// Presses each key.
fn press(app: &mut App, keys: &[Key]) {
    for key in keys {
        app.on_key(key.clone(), fakes::clock::FakeClock::new().now());
    }
}

/// Draws `app` at 60 by 12 and clicks the target `spot`.
fn click(app: &mut App, spot: Spot) -> Effect {
    let (_, targets) = screen(app, 60, 12);
    assert!(rect_of(&targets, spot).is_some(), "{spot:?} not drawn");
    app.on_click(TargetId::Form(spot))
}

/// The panel's lines.
fn lines(app: &App) -> Vec<String> {
    app.panel().map(|panel| panel.lines).unwrap_or_default()
}

/// The single line `effect` sends, parsed, without its `id`.
fn sent(effect: Effect) -> Value {
    let Effect::Send(lines) = effect else {
        panic!("expected one line, got {effect:?}");
    };
    assert_eq!(lines.len(), 1, "{lines:?}");
    let mut value: Value =
        serde_json::from_str(lines.first().map(String::as_str).unwrap_or_default())
            .unwrap_or_default();
    value.as_object_mut().map(|line| line.remove("id"));
    value
}

#[test]
fn form_single_choice_first_question() {
    let app = asking(base_and_name(false), 60, 12);
    insta::assert_snapshot!("form_single_choice_first_question", screen(&app, 60, 12).0);
}

#[test]
fn form_multi_choice_with_two_toggled() {
    let mut app = asking(base_and_name(true), 60, 12);
    press(&mut app, &[Key::Char(' '), Key::Down, Key::Char(' ')]);
    insta::assert_snapshot!("form_multi_choice_with_two_toggled", screen(&app, 60, 12).0);
}

#[test]
fn form_last_question_reads_review() {
    let mut app = asking(base_and_name(false), 60, 12);
    press(&mut app, &[Key::Tab]);
    insta::assert_snapshot!("form_last_question_reads_review", screen(&app, 60, 12).0);
}

#[test]
fn form_submit_tab_with_answers_skipped_and_note() {
    let mut app = asking(base_and_name(true), 60, 12);
    press(&mut app, &[Key::Char(' '), Key::Down, Key::Char(' ')]);
    press(
        &mut app,
        &"and tags".chars().map(Key::Char).collect::<Vec<_>>(),
    );
    press(&mut app, &[Key::Tab, Key::Tab]);
    press(
        &mut app,
        &"by friday".chars().map(Key::Char).collect::<Vec<_>>(),
    );
    insta::assert_snapshot!(
        "form_submit_tab_with_answers_skipped_and_note",
        screen(&app, 60, 12).0
    );
}

#[test]
fn form_scrolled_to_the_cursor_at_40x10() {
    let mut app = asking(four_long_options(), 40, 10);
    for _ in 0..6 {
        press(&mut app, &[Key::Down]);
    }
    let (shown, targets) = screen(&app, 40, 10);
    insta::assert_snapshot!("form_scrolled_to_the_cursor_at_40x10", shown);
    // "Chat about this" is the panel's last row, and no target is drawn
    // for a row scrolled off.
    assert_eq!(shown.lines().last(), Some("› Chat about this"));
    assert_eq!(rect_of(&targets, Spot::Chat), Some(Rect::new(0, 9, 40, 1)));
    assert!(spots(&targets).iter().all(|(_, rect)| rect.height > 0));
    // The first option shows only its second row, and its target only
    // that row.
    assert_eq!(
        rect_of(&targets, Spot::Option(0)),
        Some(Rect::new(0, 0, 40, 1))
    );
    // Back up to the first option, the header shows again.
    for _ in 0..6 {
        press(&mut app, &[Key::Up]);
    }
    let (shown, _) = screen(&app, 40, 10);
    insta::assert_snapshot!("form_scrolled_back_to_the_top_at_40x10", shown);
    assert!(
        shown.contains(&format!("question · {S_A} · 1 of 1")),
        "{shown}"
    );
}

#[test]
fn form_two_of_two_behind_an_approval() {
    let mut app = attached(60, 12);
    app.on_line(session_line(
        "permission_requested",
        json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "standing_ask", "standing_rule": {"scope": "global", "prefix": "ls"}}),
    ));
    ask(&mut app, base_and_name(false));
    press(&mut app, &[Key::Esc]);
    insta::assert_snapshot!("form_two_of_two_behind_an_approval", screen(&app, 60, 12).0);
}

/// A panel of `count` one-row lines `l0`, `l1`, …, the cursor on the last
/// when `form`.
fn numbered(count: usize, form: bool) -> Panel {
    Panel {
        lines: (0..count).map(|line| format!("l{line}")).collect(),
        alert: false,
        spots: vec![PanelSpot {
            line: 0,
            cols: None,
            spot: Spot::Chat,
        }],
        cursor: form.then(|| count.saturating_sub(1)),
        caret: None,
    }
}

/// Draws `panel` into a `height`-row area 10 wide, returning the rows and
/// the panel's top.
fn drawn(panel: &Panel, height: u16) -> (String, u16, Vec<Target>) {
    let area = Rect::new(0, 0, 10, height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    let top = draw(panel, area, area.bottom(), &mut buf, &mut targets);
    (text(&buf), top, targets)
}

#[test]
fn a_panel_exactly_as_tall_as_its_space_does_not_scroll() {
    let (shown, top, targets) = drawn(&numbered(4, true), 4);
    assert_eq!(shown, "l0\nl1\nl2\nl3\n");
    assert_eq!(top, 0);
    assert_eq!(targets.len(), 1);
}

#[test]
fn one_row_taller_scrolls_by_one() {
    let (shown, top, targets) = drawn(&numbered(5, true), 4);
    assert_eq!(shown, "l1\nl2\nl3\nl4\n");
    assert_eq!(top, 0);
    // Line 0 is scrolled off, so its target is not drawn.
    assert!(targets.is_empty());
}

#[test]
fn a_panel_shorter_than_its_space_sits_at_the_bottom() {
    let (shown, top, _) = drawn(&numbered(2, true), 4);
    assert_eq!(shown, "\n\nl0\nl1\n");
    assert_eq!(top, 2);
}

#[test]
fn an_approval_taller_than_its_space_keeps_its_top() {
    let (shown, top, _) = drawn(&numbered(5, false), 4);
    assert_eq!(shown, "l0\nl1\nl2\nl3\n");
    assert_eq!(top, 0);
}

#[test]
fn a_line_partly_scrolled_off_shows_its_last_rows() {
    // Line 0 wraps to two rows at 10 columns; the cursor's line 2 needs
    // one row scrolled off, so line 0 shows only its second row.
    let mut panel = numbered(3, true);
    if let Some(first) = panel.lines.first_mut() {
        *first = "aaaa bbbb cccc".to_owned();
    }
    let (shown, _, targets) = drawn(&panel, 3);
    assert_eq!(shown, "cccc\nl1\nl2\n");
    assert_eq!(
        targets.first().map(|target| target.rect),
        Some(Rect::new(0, 0, 10, 1))
    );
}

#[test]
fn a_click_on_a_tab_opens_it() {
    let mut app = asking(base_and_name(false), 60, 12);
    assert_eq!(click(&mut app, Spot::Tab(1)), Effect::None);
    assert_eq!(
        lines(&app).get(1).map(String::as_str),
        Some("Base  [Name]  Submit")
    );
}

#[test]
fn a_click_on_a_multi_choice_option_toggles_it_and_stays() {
    let mut app = asking(base_and_name(true), 60, 12);
    assert_eq!(click(&mut app, Spot::Option(1)), Effect::None);
    assert_eq!(lines(&app).get(4).map(String::as_str), Some("› [x] dev"));
    click(&mut app, Spot::Option(1));
    assert_eq!(lines(&app).get(4).map(String::as_str), Some("› [ ] dev"));
}

#[test]
fn a_click_on_a_single_choice_option_chooses_it_and_moves_on() {
    let mut app = asking(base_and_name(false), 60, 12);
    assert_eq!(click(&mut app, Spot::Option(1)), Effect::None);
    assert_eq!(
        lines(&app).get(1).map(String::as_str),
        Some("Base ✓  [Name]  Submit")
    );
}

#[test]
fn a_click_on_review_lands_on_submit() {
    let mut app = asking(base_and_name(false), 60, 12);
    click(&mut app, Spot::Tab(1));
    assert_eq!(click(&mut app, Spot::Next), Effect::None);
    assert_eq!(
        lines(&app).get(1).map(String::as_str),
        Some("Base  Name  [Submit]")
    );
    assert_eq!(lines(&app).get(5).map(String::as_str), Some("› Submit"));
}

#[test]
fn a_click_on_submit_sends_the_reply() {
    let mut app = asking(base_and_name(false), 60, 12);
    click(&mut app, Spot::Option(0));
    click(&mut app, Spot::Tab(2));
    let line = sent(click(&mut app, Spot::Send));
    assert_eq!(
        line,
        json!({"command": "reply", "session_id": S_A, "args": {"request_id": "r_4f",
            "answers": [{"labels": ["main (Recommended)"]}, {"skipped": true}]}})
    );
    assert!(app.panel().is_none());
}

#[test]
fn a_click_on_chat_about_this_declines() {
    let mut app = asking(base_and_name(false), 60, 12);
    let line = sent(click(&mut app, Spot::Chat));
    assert_eq!(
        line,
        json!({"command": "reply", "session_id": S_A,
            "args": {"request_id": "r_4f", "declined": true}})
    );
}

#[test]
fn a_click_on_the_words_row_takes_the_cursor() {
    let mut app = asking(base_and_name(false), 60, 12);
    assert_eq!(click(&mut app, Spot::Words), Effect::None);
    assert_eq!(
        lines(&app).get(5).map(String::as_str),
        Some("› ✎ answer in words")
    );
}

#[test]
fn an_option_that_wraps_has_one_target_covering_both_rows() {
    let app = asking(four_long_options(), 40, 30);
    let (shown, targets) = screen(&app, 40, 30);
    let options: Vec<Rect> = spots(&targets)
        .into_iter()
        .filter(|(spot, _)| *spot == Spot::Option(0))
        .map(|(_, rect)| rect)
        .collect();
    let [rect] = options.as_slice() else {
        panic!("{options:?}");
    };
    assert_eq!(rect.height, 2, "{shown}");
    let rows: Vec<&str> = shown.lines().collect();
    let first = rows.get(usize::from(rect.y)).copied().unwrap_or_default();
    assert!(first.starts_with("› ( ) one"), "{shown}");
}

#[test]
fn tab_targets_are_drawn_only_while_the_tab_line_fits() {
    // "[Base]  Name  Submit" is 20 cells.
    for (width, tabs) in [(19, 0), (20, 3)] {
        let app = asking(base_and_name(false), width, 40);
        let (shown, targets) = screen(&app, width, 40);
        let drawn = spots(&targets)
            .iter()
            .filter(|(spot, _)| matches!(spot, Spot::Tab(_)))
            .count();
        assert_eq!(drawn, tabs, "{width}: {shown}");
    }
}

#[test]
fn each_tab_target_covers_its_cells() {
    let fields = json!([
        {"header": "名前", "question": "Which name?"},
        {"header": "Name", "question": "What name?"}
    ]);
    let app = asking(fields, 60, 12);
    let (shown, targets) = screen(&app, 60, 12);
    let rows: Vec<&str> = shown.lines().collect();
    let y = rect_of(&targets, Spot::Tab(0)).map_or(0, |rect| rect.y);
    // A wide character's second cell reads as a space in the screen's text.
    assert_eq!(
        rows.get(usize::from(y)).copied(),
        Some("[名 前 ]  Name  Submit")
    );
    assert_eq!(rect_of(&targets, Spot::Tab(0)), Some(Rect::new(0, y, 6, 1)));
    assert_eq!(rect_of(&targets, Spot::Tab(1)), Some(Rect::new(8, y, 4, 1)));
    assert_eq!(
        rect_of(&targets, Spot::Tab(2)),
        Some(Rect::new(14, y, 6, 1))
    );
}
