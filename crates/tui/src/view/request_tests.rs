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
    assert_eq!(shown.lines().last(), Some("  › Chat about this"));
    assert_eq!(rect_of(&targets, Spot::Chat), Some(Rect::new(2, 9, 38, 1)));
    assert!(spots(&targets).iter().all(|(_, rect)| rect.height > 0));
    // The first option shows only its second row, and its target only
    // that row.
    assert_eq!(
        rect_of(&targets, Spot::Option(0)),
        Some(Rect::new(2, 0, 38, 1))
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
    assert_eq!(shown, "  l0\n  l1\n  l2\n  l3\n");
    assert_eq!(top, 0);
    assert_eq!(targets.len(), 1);
}

#[test]
fn one_row_taller_scrolls_by_one() {
    let (shown, top, targets) = drawn(&numbered(5, true), 4);
    assert_eq!(shown, "  l1\n  l2\n  l3\n  l4\n");
    assert_eq!(top, 0);
    // Line 0 is scrolled off, so its target is not drawn.
    assert!(targets.is_empty());
}

#[test]
fn a_panel_shorter_than_its_space_sits_at_the_bottom() {
    let (shown, top, _) = drawn(&numbered(2, true), 4);
    assert_eq!(shown, "▄▄▄▄▄▄▄▄▄▄\n  l0\n  l1\n▀▀▀▀▀▀▀▀▀▀\n");
    assert_eq!(top, 0);
}

#[test]
fn an_approval_taller_than_its_space_keeps_its_top() {
    let (shown, top, _) = drawn(&numbered(5, false), 4);
    assert_eq!(shown, "▌ l0\n▌ l1\n▌ l2\n▌ l3\n");
    assert_eq!(top, 0);
}

#[test]
fn a_line_partly_scrolled_off_shows_its_last_rows() {
    // Line 0 wraps to three rows at the inset width of 8; the cursor's
    // line 2 needs two rows scrolled off, so line 0 shows only its last
    // row.
    let mut panel = numbered(3, true);
    if let Some(first) = panel.lines.first_mut() {
        *first = "aaaa bbbb cccc".to_owned();
    }
    let (shown, _, targets) = drawn(&panel, 3);
    assert_eq!(shown, "  cccc\n  l1\n  l2\n");
    assert_eq!(
        targets.first().map(|target| target.rect),
        Some(Rect::new(2, 0, 8, 1))
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
    assert!(first.starts_with("  › ( ) one"), "{shown}");
}

#[test]
fn tab_targets_are_drawn_only_while_the_tab_line_fits() {
    // "[Base]  Name  Submit" is 20 cells, so at the inset width it fits
    // one row from width 22.
    for (width, tabs) in [(21, 0), (22, 3)] {
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
        Some("  [名 前 ]  Name  Submit")
    );
    assert_eq!(rect_of(&targets, Spot::Tab(0)), Some(Rect::new(2, y, 6, 1)));
    assert_eq!(
        rect_of(&targets, Spot::Tab(1)),
        Some(Rect::new(10, y, 4, 1))
    );
    assert_eq!(
        rect_of(&targets, Spot::Tab(2)),
        Some(Rect::new(16, y, 6, 1))
    );
}

#[test]
fn form_words_row_scrolled_to_the_caret_at_30x12() {
    let mut app = asking(base_and_name(false), 30, 12);
    press(&mut app, &[Key::Tab]);
    let words: Vec<Key> = "a long answer that runs past thirty columns"
        .chars()
        .map(Key::Char)
        .collect();
    press(&mut app, &words);
    let (shown, _) = screen(&app, 30, 12);
    insta::assert_snapshot!("form_words_row_scrolled_to_the_caret_at_30x12", shown);
    let area = Rect::new(0, 0, 30, 12);
    let row = shown
        .lines()
        .position(|row| row.trim_start().starts_with("› ✎"))
        .and_then(|row| u16::try_from(row).ok());
    assert_eq!(
        super::super::cursor(&app, area).map(|at| (at.x, Some(at.y))),
        Some((29, row))
    );
}

#[test]
fn the_caret_stops_at_the_last_column() {
    let mut panel = numbered(2, true);
    panel.caret = Some((1, 12));
    let area = Rect::new(0, 0, 10, 4);
    assert_eq!(
        super::caret(&panel, area, area.bottom()),
        Some(ratatui::layout::Position::new(9, 2))
    );
    panel.caret = Some((1, 9));
    assert_eq!(
        super::caret(&panel, area, area.bottom()),
        Some(ratatui::layout::Position::new(9, 2))
    );
}

#[test]
fn a_caret_on_a_line_scrolled_off_is_not_drawn() {
    let mut panel = numbered(5, true);
    panel.caret = Some((0, 1));
    let area = Rect::new(0, 0, 10, 4);
    assert_eq!(super::caret(&panel, area, area.bottom()), None);
    panel.caret = Some((1, 1));
    assert_eq!(
        super::caret(&panel, area, area.bottom()),
        Some(ratatui::layout::Position::new(3, 0))
    );
}

/// An approval panel of `lines`, escalated when `alert`: no cursor, so the
/// stripe draws.
fn approval(lines: &[&str], alert: bool) -> Panel {
    Panel {
        lines: lines.iter().map(|line| (*line).to_owned()).collect(),
        alert,
        spots: Vec::new(),
        cursor: None,
        caret: None,
    }
}

/// Draws `panel` into a 40-wide, `height`-row area, returning the rows, the
/// buffer and the targets.
fn surfaced(panel: &Panel, height: u16) -> (String, Buffer, Vec<Target>) {
    let area = Rect::new(0, 0, 40, height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    draw(panel, area, area.bottom(), &mut buf, &mut targets);
    (text(&buf), buf, targets)
}

#[test]
fn approval_with_stripe_and_edges() {
    let (shown, _, _) = surfaced(
        &approval(
            &["approve shell echo hi?", "allow once · for this session"],
            false,
        ),
        12,
    );
    insta::assert_snapshot!("approval_with_stripe_and_edges", shown);
}

#[test]
fn alert_with_stripe_and_edges() {
    let (shown, _, _) = surfaced(
        &approval(&["the reviewer escalated", "allow once · deny"], true),
        12,
    );
    insta::assert_snapshot!("alert_with_stripe_and_edges", shown);
}

#[test]
fn the_panel_tint_covers_its_rows_and_edges() {
    use crate::theme::Role;
    for (alert, tint, stripe) in [
        (false, Role::Approval, Role::Attention),
        (true, Role::Alert, Role::Error),
    ] {
        let (_, buf, _) = surfaced(&approval(&["line one", "line two"], alert), 12);
        // Two text rows with an edge above and below.
        for y in [9, 10] {
            for x in 0..40 {
                assert_eq!(buf[(x, y)].bg, tint.color(), "alert {alert}, ({x}, {y})");
            }
            assert_eq!(buf[(0, y)].symbol(), "▌");
            assert_eq!(buf[(0, y)].fg, stripe.color());
        }
        for (y, edge) in [(8, "▄"), (11, "▀")] {
            for x in 0..40 {
                assert_eq!(buf[(x, y)].symbol(), edge);
                assert_eq!(buf[(x, y)].fg, tint.color(), "alert {alert}, ({x}, {y})");
            }
        }
    }
}

#[test]
fn a_form_has_edges_and_no_stripe() {
    let (_, buf, _) = surfaced(&numbered(3, true), 12);
    // Three text rows with an edge above and below, and no stripe cell.
    for y in [8, 9, 10] {
        for x in 0..10 {
            assert_eq!(buf[(x, y)].bg, crate::theme::Role::Approval.color());
        }
    }
    for cell in &buf.content {
        assert_ne!(cell.symbol(), "▌");
    }
    for x in 0..10 {
        assert_eq!(buf[(x, 7)].symbol(), "▄");
        assert_eq!(buf[(x, 11)].symbol(), "▀");
    }
}

#[test]
fn spots_and_the_caret_shift_by_the_inset() {
    let mut panel = numbered(2, true);
    panel.spots = vec![PanelSpot {
        line: 0,
        cols: Some((1, 4)),
        spot: Spot::Chat,
    }];
    panel.caret = Some((0, 5));
    let area = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    draw(&panel, area, area.bottom(), &mut buf, &mut targets);
    // Past the stripe and the gap: two columns in.
    assert_eq!(rect_of(&targets, Spot::Chat), Some(Rect::new(3, 9, 3, 1)));
    assert_eq!(
        super::caret(&panel, area, area.bottom()),
        Some(ratatui::layout::Position::new(7, 9))
    );
}

#[test]
fn height_counts_rows_at_the_inset_and_the_edges() {
    use super::height;
    let panel = approval(&["abcdefgh"], false);
    // The word wraps at the inset: 4 rows at width 2, 8 at width 3, 3 at
    // width 5.
    for (width, total) in [(2, 4), (3, 8), (5, 3)] {
        assert_eq!(height(&panel, width, total + 1), total, "width {width}");
        assert_eq!(height(&panel, width, total + 2), total + 2, "width {width}");
        assert_eq!(height(&panel, width, total + 4), total + 2, "width {width}");
    }
}

#[test]
fn a_narrow_approval_has_no_stripe() {
    let panel = approval(&["hi"], false);
    let area = Rect::new(0, 0, 2, 6);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    draw(&panel, area, area.bottom(), &mut buf, &mut targets);
    for cell in &buf.content {
        assert_ne!(cell.symbol(), "▌");
    }
}
