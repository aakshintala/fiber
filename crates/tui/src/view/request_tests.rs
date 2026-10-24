//! The request panel's frame: question form snapshots, scrolling, its click
//! targets and the text cursor.

use std::path::PathBuf;

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::super::{render, text};
use super::{draw, height};
use crate::app::{App, Effect};
use crate::approvals::form::Spot;
use crate::approvals::{Panel, PanelSpot, Queue};
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::{Target, TargetId, hit};
use crate::surface;
use crate::theme::Role;
use ratatui::style::{Color, Modifier};

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

/// Folds one interaction of `kind` over an app.
fn ask_one(app: &mut App, kind: &str) {
    let payload = match kind {
        "confirm" => json!({"request_id": "r_c", "kind": kind, "prompt": "Continue?"}),
        "select" => json!({"request_id": "r_s", "kind": kind, "prompt": "Pick one?",
            "options": [{"label": "alpha", "description": "the first choice"},
                {"label": "beta"}]}),
        "multi_select" => json!({"request_id": "r_m", "kind": kind, "prompt": "Pick some?",
            "options": [{"label": "alpha"}, {"label": "beta"}, {"label": "gamma"}]}),
        "text_input" => json!({"request_id": "r_t", "kind": kind, "prompt": "What?"}),
        _ => panic!("unknown one-question interaction: {kind}"),
    };
    app.on_line(session_line("interaction_requested", payload));
}

/// An app `width` by `height` showing one interaction of `kind`.
fn asking_one(kind: &str, width: u16, height: u16) -> App {
    let mut app = attached(width, height);
    ask_one(&mut app, kind);
    assert!(app.panel().is_some());
    app
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
    // The cursor sits on the words row, kept as the last shown row; a
    // question tab has no Chat row, and no target is drawn for a row
    // scrolled off.
    assert!(
        shown.lines().last().is_some_and(|row| row.contains("✎")),
        "{shown}"
    );
    assert_eq!(rect_of(&targets, Spot::Chat), None);
    assert!(spots(&targets).iter().all(|(_, rect)| rect.height > 0));
    // The first option's line wraps to three rows, showing only its last
    // two, and its target only those rows.
    assert_eq!(
        rect_of(&targets, Spot::Option(0)),
        Some(Rect::new(2, 1, 38, 2))
    );
    // Back up to the first option, the header shows again.
    for _ in 0..6 {
        press(&mut app, &[Key::Up]);
    }
    let (shown, _) = screen(&app, 40, 10);
    insta::assert_snapshot!("form_scrolled_back_to_the_top_at_40x10", shown);
    assert!(shown.contains("Question from"), "{shown}");
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
        marks: Vec::new(),
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
    assert_eq!(shown, "▌ l0\n▌ l1\n▌ l2\n▌ l3\n");
    assert_eq!(top, 0);
    assert_eq!(targets.len(), 1);
}

#[test]
fn one_row_taller_scrolls_by_one() {
    let (shown, top, targets) = drawn(&numbered(5, true), 4);
    assert_eq!(shown, "▌ l1\n▌ l2\n▌ l3\n▌ l4\n");
    assert_eq!(top, 0);
    // Line 0 is scrolled off, so its target is not drawn.
    assert!(targets.is_empty());
}

#[test]
fn a_panel_shorter_than_its_space_sits_at_the_bottom() {
    let (shown, top, _) = drawn(&numbered(2, true), 4);
    assert_eq!(shown, "▄▄▄▄▄▄▄▄▄▄\n▌ l0\n▌ l1\n▀▀▀▀▀▀▀▀▀▀\n");
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
    assert_eq!(shown, "▌ cccc\n▌ l1\n▌ l2\n");
    assert_eq!(
        targets.first().map(|target| target.rect),
        Some(Rect::new(2, 0, 8, 1))
    );
}

#[test]
fn a_click_on_a_tab_opens_it() {
    let mut app = asking(base_and_name(false), 60, 12);
    assert_eq!(click(&mut app, Spot::Tab(1)), Effect::None);
    assert_eq!(lines(&app).get(4).map(String::as_str), Some("What name?"));
    assert_eq!(
        lines(&app).get(6).map(String::as_str),
        Some("▸ 1 ✎ Type an answer")
    );
}

#[test]
fn a_click_on_a_multi_choice_option_toggles_it_and_stays() {
    let mut app = asking(base_and_name(true), 60, 12);
    assert_eq!(click(&mut app, Spot::Option(1)), Effect::None);
    assert_eq!(lines(&app).get(8).map(String::as_str), Some("▸ 2 [x] dev"));
    click(&mut app, Spot::Option(1));
    assert_eq!(lines(&app).get(8).map(String::as_str), Some("▸ 2 [ ] dev"));
}

#[test]
fn a_click_on_a_single_choice_option_chooses_it_and_moves_on() {
    let mut app = asking(base_and_name(false), 60, 12);
    assert_eq!(click(&mut app, Spot::Option(1)), Effect::None);
    assert_eq!(lines(&app).get(4).map(String::as_str), Some("What name?"));
}

#[test]
fn a_click_on_review_lands_on_submit() {
    let mut app = asking(
        json!([{"header": "Pick", "question": "Which?", "multiSelect": true,
            "options": [{"label": "a"}]}]),
        60,
        12,
    );
    assert_eq!(click(&mut app, Spot::Next), Effect::None);
    assert_eq!(lines(&app).get(2).map(String::as_str), Some("Review"));
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
    assert_eq!(click(&mut app, Spot::Tab(2)), Effect::None);
    let line = sent(click(&mut app, Spot::Chat));
    assert_eq!(
        line,
        json!({"command": "reply", "session_id": S_A,
            "args": {"request_id": "r_4f", "declined": true}})
    );
}

#[test]
fn a_narrow_submit_tab_keeps_chat_about_this_clickable() {
    // Below the width the buttons share one row they stack, so each
    // button stays a whole-line target through wrapping: at 24 columns
    // a click on the "Chat about this" label declines.
    let mut app = asking(base_and_name(false), 24, 40);
    press(&mut app, &[Key::Tab, Key::Tab]);
    let (shown, targets) = screen(&app, 24, 40);
    let row = shown
        .lines()
        .position(|row| row.contains("Chat about this"))
        .expect("chat row drawn");
    let line = shown.lines().nth(row).unwrap_or_default();
    let col = line.find("Chat").unwrap_or_default().saturating_add(1);
    assert_eq!(
        hit(
            &targets,
            u16::try_from(col).unwrap_or(u16::MAX),
            u16::try_from(row).unwrap_or(u16::MAX)
        ),
        Some(TargetId::Form(Spot::Chat)),
        "{shown}"
    );
    let line = sent(app.on_click(TargetId::Form(Spot::Chat)));
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
        lines(&app).get(8).map(String::as_str),
        Some("▸ 3 ✎ Type an answer, alone or with the options")
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
    assert!(first.starts_with("▌ ▸ 1 ( ) one"), "{shown}");
}

#[test]
fn tab_targets_are_drawn_only_while_the_tab_line_fits() {
    // "←  ☐ Base    ☐ Name    ✔ Submit  →" is 36 cells, so the tab
    // targets draw from width 36.
    for (width, tabs) in [(35, 0), (36, 3)] {
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
        Some("▌ ←  ☐ 名 前     ☐ Name    ✔ Submit  →")
    );
    assert_eq!(rect_of(&targets, Spot::Tab(0)), Some(Rect::new(4, y, 8, 1)));
    assert_eq!(
        rect_of(&targets, Spot::Tab(1)),
        Some(Rect::new(14, y, 8, 1))
    );
    assert_eq!(
        rect_of(&targets, Spot::Tab(2)),
        Some(Rect::new(24, y, 10, 1))
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
        .position(|row| row.contains("✎ "))
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
        marks: Vec::new(),
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
fn edges_draw_only_where_they_fit() {
    // Two text rows in three rows: no room for the edges, so both edge
    // rows stay blank (`docs/tui.md`, "Look").
    let (_, buf, _) = surfaced(&approval(&["line one", "line two"], false), 3);
    for x in 0..40 {
        assert_eq!(buf[(x, 0)].symbol(), " ", "top edge at {x}");
    }
    // One row more: the edges fit, so both draw.
    let (_, edged, _) = surfaced(&approval(&["line one", "line two"], false), 4);
    for x in 0..40 {
        assert_eq!(edged[(x, 0)].symbol(), "▄", "top edge at {x}");
        assert_eq!(edged[(x, 3)].symbol(), "▀", "bottom edge at {x}");
    }
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
fn a_form_has_edges_and_a_secondary_stripe() {
    let (_, buf, _) = surfaced(&numbered(3, true), 12);
    // Three text rows with an edge above and below, and every row's
    // stripe cell in `secondary`.
    for y in [8, 9, 10] {
        for x in 0..10 {
            assert_eq!(buf[(x, y)].bg, crate::theme::Role::Approval.color());
        }
        assert_eq!(buf[(0, y)].symbol(), "▌");
        assert_eq!(buf[(0, y)].fg, crate::theme::Role::Secondary.color());
    }
    for x in 0..10 {
        assert_eq!(buf[(x, 7)].symbol(), "▄");
        assert_eq!(buf[(x, 11)].symbol(), "▀");
    }
    // An approval panel's stripe stays `attention`.
    let (_, buf, _) = surfaced(&approval(&["l0", "l1", "l2"], false), 12);
    for y in [8, 9, 10] {
        assert_eq!(buf[(0, y)].symbol(), "▌");
        assert_eq!(buf[(0, y)].fg, crate::theme::Role::Attention.color());
    }
    // A form narrower than 3 columns draws no stripe.
    let area = Rect::new(0, 0, 2, 6);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    draw(
        &numbered(1, true),
        area,
        area.bottom(),
        &mut buf,
        &mut targets,
    );
    for cell in &buf.content {
        assert_ne!(cell.symbol(), "▌");
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

#[test]
fn one_question_panels_have_no_tab_row_or_tab_target() {
    for kind in ["confirm", "select", "multi_select", "text_input"] {
        let app = asking_one(kind, 60, 12);
        let (shown, targets) = screen(&app, 60, 12);
        assert!(
            !targets
                .iter()
                .any(|target| matches!(target.id, TargetId::Form(Spot::Tab(_)))),
            "{shown}"
        );
        let prompt = match kind {
            "confirm" => "Continue?",
            "select" => "Pick one?",
            "multi_select" => "Pick some?",
            "text_input" => "What?",
            _ => panic!("unknown one-question interaction: {kind}"),
        };
        assert_eq!(
            lines(&app).get(2).map(String::as_str),
            Some(prompt),
            "{shown}"
        );
    }
}

#[test]
fn one_question_screens() {
    let confirm = asking_one("confirm", 60, 12);
    insta::assert_snapshot!("one_question_confirm", screen(&confirm, 60, 12).0);

    let select = asking_one("select", 60, 12);
    insta::assert_snapshot!("one_question_select", screen(&select, 60, 12).0);

    let mut multi = asking_one("multi_select", 60, 12);
    press(&mut multi, &[Key::Char(' ')]);
    insta::assert_snapshot!(
        "one_question_multi_select_with_one_toggled",
        screen(&multi, 60, 12).0
    );

    let mut text = asking_one("text_input", 60, 12);
    press(&mut text, &[Key::Char('h'), Key::Char('i')]);
    insta::assert_snapshot!("one_question_text_input_typed", screen(&text, 60, 12).0);
}

#[test]
fn clicks_on_one_question_rows_send_or_change_the_answer() {
    let mut confirm = asking_one("confirm", 60, 12);
    assert_eq!(
        sent(click(&mut confirm, Spot::Option(0))),
        json!({"command": "reply", "session_id": S_A,
            "args": {"request_id": "r_c", "confirmed": true}})
    );
    let mut confirm = asking_one("confirm", 60, 12);
    assert_eq!(
        sent(click(&mut confirm, Spot::Option(1))),
        json!({"command": "reply", "session_id": S_A,
            "args": {"request_id": "r_c", "confirmed": false}})
    );

    let mut select = asking_one("select", 60, 12);
    assert_eq!(
        sent(click(&mut select, Spot::Option(1))),
        json!({"command": "reply", "session_id": S_A,
            "args": {"request_id": "r_s", "labels": ["beta"]}})
    );

    let mut multi = asking_one("multi_select", 60, 12);
    assert_eq!(click(&mut multi, Spot::Option(1)), Effect::None);
    assert_eq!(
        lines(&multi).get(6).map(String::as_str),
        Some("▸ 2 [x] beta")
    );
    assert_eq!(
        sent(click(&mut multi, Spot::Send)),
        json!({"command": "reply", "session_id": S_A,
            "args": {"request_id": "r_m", "labels": ["beta"]}})
    );

    let mut text = asking_one("text_input", 60, 12);
    press(&mut text, &[Key::Char('h'), Key::Char('i')]);
    assert_eq!(click(&mut text, Spot::Words), Effect::None);
    press(&mut text, &[Key::Char('!')]);
    assert_eq!(
        sent(click(&mut text, Spot::Send)),
        json!({"command": "reply", "session_id": S_A,
            "args": {"request_id": "r_t", "text": "hi!"}})
    );
}

#[test]
fn clicking_chat_about_this_declines_every_one_question_kind() {
    for (kind, request_id) in [
        ("confirm", "r_c"),
        ("select", "r_s"),
        ("multi_select", "r_m"),
        ("text_input", "r_t"),
    ] {
        let mut app = asking_one(kind, 60, 12);
        assert_eq!(
            sent(click(&mut app, Spot::Chat)),
            json!({"command": "reply", "session_id": S_A,
                "args": {"request_id": request_id, "declined": true}}),
            "{kind}"
        );
    }
}

/// The NO_COLOR look: every role paints to the terminal's default.
fn no_color_look() -> crate::look::Look {
    let (look, notice) = crate::look::Look::new(crate::look::ThemeSetting::Dark, &|name: &str| {
        (name == "NO_COLOR").then(|| "1".to_owned())
    });
    assert_eq!(notice, None);
    look
}

/// `panel` drawn and painted without colour in a 40-wide area, with its
/// targets.
fn no_color_surfaced(panel: &Panel, height: u16) -> (Buffer, Vec<Target>) {
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let area = Rect::new(0, 0, 40, height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    draw(panel, area, area.bottom(), &mut buf, &mut targets);
    no_color_look().paint(&mut buf);
    (buf, targets)
}

#[test]
fn no_color_boxes_an_approval_and_an_escalation() {
    use ratatui::style::{Color, Modifier};
    for alert in [false, true] {
        let (buf, _) = no_color_surfaced(
            &approval(
                &["approve shell echo hi?", "allow once · for this session"],
                alert,
            ),
            12,
        );
        // Two text rows with a box above and below, and no stripe cell.
        for y in [9, 10] {
            for x in 0..40 {
                assert_eq!(
                    (buf[(x, y)].fg, buf[(x, y)].bg),
                    (Color::Reset, Color::Reset),
                    "({x}, {y})"
                );
            }
            assert_eq!(buf[(0, y)].symbol(), "│", "({y})");
            assert_eq!(buf[(39, y)].symbol(), "│", "({y})");
            assert_eq!(buf[(0, y)].modifier, Modifier::DIM);
        }
        let first: String = (1..39).map(|x| buf[(x, 9)].symbol()).collect();
        assert!(first.contains("approve shell echo hi?"), "{first:?}");
        for (y, left, right) in [(8, "┌", "┐"), (11, "└", "┘")] {
            for x in 0..40 {
                let cell = &buf[(x, y)];
                assert_eq!(
                    cell.symbol(),
                    if x == 0 {
                        left
                    } else if x == 39 {
                        right
                    } else {
                        "─"
                    }
                );
                assert_eq!(cell.modifier, Modifier::DIM, "({x}, {y})");
            }
        }
        for cell in &buf.content {
            assert_ne!(cell.symbol(), "▌");
        }
    }
}

#[test]
fn no_color_spots_and_the_caret_keep_clear_of_the_borders() {
    let mut panel = numbered(2, true);
    panel.spots = vec![PanelSpot {
        line: 0,
        cols: Some((1, 4)),
        spot: Spot::Chat,
    }];
    panel.caret = Some((0, 5));
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let area = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    draw(&panel, area, area.bottom(), &mut buf, &mut targets);
    // One column in, past the box's left side; the right column stays
    // reserved.
    assert_eq!(rect_of(&targets, Spot::Chat), Some(Rect::new(2, 9, 3, 1)));
    assert_eq!(
        super::caret(&panel, area, area.bottom()),
        Some(ratatui::layout::Position::new(6, 9))
    );
    no_color_look().paint(&mut buf);
    assert_eq!(buf[(0, 9)].symbol(), "│");
    assert_eq!(buf[(39, 9)].symbol(), "│");
}

#[test]
fn no_color_suppresses_box_edges_without_room() {
    // Two text rows in two rows: no room for the edges, so no corners
    // draw, but the sides still box the text.
    let (buf, _) = no_color_surfaced(&approval(&["line one", "line two"], false), 2);
    for y in 0..=1 {
        assert_eq!(buf[(0, y)].symbol(), "│");
        assert_eq!(buf[(39, y)].symbol(), "│");
    }
    for cell in &buf.content {
        assert!(
            !matches!(cell.symbol(), "┌" | "┐" | "└" | "┘"),
            "{}",
            cell.symbol()
        );
    }
    let first: String = (1..39).map(|x| buf[(x, 0)].symbol()).collect();
    assert!(first.contains("line one"), "{first:?}");
}

/// A `tool_call_requested` from [`S_A`] for `action` running `command`.
fn tool_call(action: &str, command: &str) -> contract::Envelope {
    contract::Envelope {
        kind: "tool_call_requested".to_owned(),
        session_id: contract::SessionId(S_A.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId(action.to_owned())),
        seq: None,
        payload: json!({"name": "shell", "arguments": {"command": command}})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }
}

/// A `permission_requested` from [`S_A`] for `id` about `action`, with
/// `keys` beside the declared effects.
fn approval_request(id: &str, action: &str, keys: Value) -> contract::Envelope {
    let mut payload = json!({"request_id": id, "effects": ["executes"], "reversible": true});
    if let (Some(into), Some(from)) = (payload.as_object_mut(), keys.as_object()) {
        into.extend(from.clone());
    }
    contract::Envelope {
        kind: "permission_requested".to_owned(),
        session_id: contract::SessionId(S_A.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId(action.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// A queue holding `lines`, folded in order.
fn queued(lines: &[contract::Envelope]) -> Queue {
    let mut queue = Queue::default();
    for line in lines {
        queue.fold(line);
    }
    queue
}

/// The open panel padded to the inset of `width` columns.
fn panel_at(queue: &Queue, width: u16) -> Panel {
    queue
        .panel(surface::inset(width))
        .expect("the panel is open")
}

/// Draws `panel` into a `width` by `height` area, returning the rows, the
/// buffer and the targets.
fn drawn_at(panel: &Panel, width: u16, height: u16) -> (String, Buffer, Vec<Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    draw(panel, area, area.bottom(), &mut buf, &mut targets);
    (text(&buf), buf, targets)
}

/// A standing ask for `shell echo hi`, padded for 80 columns.
fn standing_panel() -> Panel {
    queued(&[
        tool_call("a_1", "echo hi"),
        approval_request(
            "r_1",
            "a_1",
            json!({"step": "standing_ask",
                "standing_rule": {"scope": "global", "prefix": "echo hi"}}),
        ),
    ])
    .panel(surface::inset(80))
    .expect("the panel is open")
}

#[test]
fn a_standing_ask_draws_every_run_in_its_style() {
    let panel = standing_panel();
    let (_, buf, _) = drawn_at(&panel, 80, 12);
    let cell = |x: u16, y: u16| &buf[(x, y)];
    // The header: "Approval 1 of 1" bold in `attention`, the session in
    // `attention`, "Esc puts it aside" dim ending in the last column.
    for (x, symbol) in [(2, "A"), (16, "1")] {
        assert_eq!(cell(x, 3).symbol(), symbol);
        assert_eq!(cell(x, 3).fg, Role::Attention.color(), "({x}, 3)");
        assert!(cell(x, 3).modifier.contains(Modifier::BOLD), "({x}, 3)");
    }
    for (x, symbol) in [(17, " "), (20, "s"), (37, "a")] {
        assert_eq!(cell(x, 3).symbol(), symbol);
        assert_eq!(cell(x, 3).fg, Role::Attention.color(), "({x}, 3)");
        assert!(!cell(x, 3).modifier.contains(Modifier::BOLD), "({x}, 3)");
    }
    assert_eq!(cell(38, 3).symbol(), " ");
    assert_eq!(cell(38, 3).fg, Color::Reset);
    assert!(!cell(38, 3).modifier.contains(Modifier::DIM));
    for x in [63, 79] {
        assert!(cell(x, 3).modifier.contains(Modifier::DIM), "({x}, 3)");
    }
    assert_eq!(cell(63, 3).symbol(), "E");
    assert_eq!(cell(79, 3).symbol(), "e");
    // The tool's name bold in `info`, its argument bold.
    assert_eq!(cell(2, 4).symbol(), "s");
    assert_eq!(cell(2, 4).fg, Role::Info.color());
    assert!(cell(2, 4).modifier.contains(Modifier::BOLD));
    assert_eq!(cell(8, 4).symbol(), "e");
    assert_eq!(cell(8, 4).fg, Color::Reset);
    assert!(cell(8, 4).modifier.contains(Modifier::BOLD));
    // The facts line and the hint are dim.
    for (x, y) in [(2, 5), (30, 5), (2, 10), (40, 10)] {
        assert!(cell(x, y).modifier.contains(Modifier::DIM), "({x}, {y})");
    }
    // The focused choice: "▸ " in `attention", the number dim, the label
    // bold; the other choice's number dim and its prompt dim.
    assert_eq!(cell(2, 7).symbol(), "▸");
    assert_eq!(cell(2, 7).fg, Role::Attention.color());
    assert!(cell(4, 7).modifier.contains(Modifier::DIM));
    assert_eq!(cell(6, 7).symbol(), "A");
    assert!(cell(6, 7).modifier.contains(Modifier::BOLD));
    assert!(cell(4, 8).modifier.contains(Modifier::DIM));
    assert_eq!(cell(6, 8).symbol(), "D");
    assert!(!cell(6, 8).modifier.contains(Modifier::BOLD));
    assert!(!cell(6, 8).modifier.contains(Modifier::DIM));
    assert!(cell(10, 8).modifier.contains(Modifier::DIM));
    // The stripe marks every row including the blank ones, on the
    // approval tint.
    for y in 3..11 {
        assert_eq!(buf[(0, y)].symbol(), "▌", "row {y}");
        assert_eq!(buf[(0, y)].fg, Role::Attention.color(), "row {y}");
        for x in 0..80 {
            assert_eq!(buf[(x, y)].bg, Role::Approval.color(), "({x}, {y})");
        }
    }
    // The call's JSON never draws.
    let (shown, _, _) = drawn_at(&panel, 80, 12);
    assert!(!shown.contains("{\""), "{shown}");
}

#[test]
fn an_escalation_draws_alert_with_an_error_stripe() {
    let queue = queued(&[
        tool_call("a_1", "cargo mutants -p hub"),
        approval_request(
            "r_1",
            "a_1",
            json!({"reversible": false, "step": "review",
                "escalation": {"cause": "consecutive_blocks", "reason": "rm -rf"},
                "rule": {"subject": "cargo mutants -p hub", "prefix": "cargo mutants"}}),
        ),
    ]);
    let panel = panel_at(&queue, 80);
    assert!(panel.alert);
    let (_, buf, _) = drawn_at(&panel, 80, 16);
    let cell = |x: u16, y: u16| &buf[(x, y)];
    // Ten text rows with edges above and below: header, tool, facts,
    // blank, four choices, blank, hint.
    for y in 5..15 {
        for x in 0..80 {
            assert_eq!(buf[(x, y)].bg, Role::Alert.color(), "({x}, {y})");
        }
        assert_eq!(buf[(0, y)].symbol(), "▌", "row {y}");
        assert_eq!(buf[(0, y)].fg, Role::Error.color(), "row {y}");
    }
    // " · irreversible" in the header is bold in `error`: the session
    // takes cells 18..36, so the run takes 36..51.
    for dx in 36..51 {
        let x = dx + 2;
        assert!(cell(x, 5).modifier.contains(Modifier::BOLD), "({x}, 5)");
        assert_eq!(cell(x, 5).fg, Role::Error.color(), "({x}, 5)");
    }
    // A remembering choice ends with the prefix and " *" in `info`, and
    // the numbers stay dim: the second choice draws on row 10.
    let remembering: String = (2..80).map(|x| buf[(x, 10)].symbol()).collect();
    assert!(remembering.contains("Allow for this session cargo mutants *"));
    let star = remembering.find(" *").expect("the star") + 1;
    let x = u16::try_from(star).unwrap_or(u16::MAX).saturating_add(2);
    assert_eq!(cell(x, 10).fg, Role::Info.color());
    assert!(cell(4, 10).modifier.contains(Modifier::DIM));
    let (shown, _, _) = drawn_at(&panel, 80, 16);
    assert!(
        shown.contains("the reviewer blocked the last calls: rm -rf"),
        "{shown}"
    );
    assert!(!shown.contains("{\""), "{shown}");
}

#[test]
fn height_counts_the_drawn_rows_for_a_wrapped_argument_and_header() {
    // A 200-column command wraps the tool row; the drawn rows match.
    let queue = queued(&[
        tool_call("a_1", &"w ".repeat(100)),
        approval_request(
            "r_1",
            "a_1",
            json!({"step": "standing_ask",
                "standing_rule": {"scope": "global", "prefix": "w"}}),
        ),
    ]);
    let panel = panel_at(&queue, 80);
    let (shown, _, _) = drawn_at(&panel, 80, 60);
    let rows: Vec<&str> = shown.lines().collect();
    let edged = |row: &&str| !row.is_empty() && row.chars().all(|cell| cell == '▄' || cell == '▀');
    let top = rows.iter().position(edged);
    let bottom = rows.iter().rposition(edged);
    let (Some(top), Some(bottom)) = (top, bottom) else {
        panic!("edges in\n{shown}");
    };
    assert_eq!(height(&panel, 80, 60), bottom - top + 1);
    // A narrow panel wraps the header; the drawn rows match there too.
    let panel = panel_at(&queue, 30);
    let (shown, _, _) = drawn_at(&panel, 30, 60);
    let rows: Vec<&str> = shown.lines().collect();
    let top = rows.iter().position(edged);
    let bottom = rows.iter().rposition(edged);
    let (Some(top), Some(bottom)) = (top, bottom) else {
        panic!("edges in\n{shown}");
    };
    assert_eq!(height(&panel, 30, 60), bottom - top + 1);
}

#[test]
fn approval_standing_ask_at_80_columns() {
    let (shown, _, _) = drawn_at(&standing_panel(), 80, 12);
    insta::assert_snapshot!("approval_standing_ask_at_80_columns", shown);
}

#[test]
fn approval_review_with_remember_rows() {
    let queue = queued(&[
        tool_call("a_1", "cargo mutants -p hub"),
        approval_request(
            "r_1",
            "a_1",
            json!({"step": "review",
                "escalation": {"cause": "consecutive_blocks",
                    "reason": "it blocked three in a row"},
                "rule": {"subject": "cargo mutants -p hub",
                    "prefix": "cargo mutants"}}),
        ),
    ]);
    let (shown, _, _) = drawn_at(&panel_at(&queue, 80), 80, 16);
    insta::assert_snapshot!("approval_review_with_remember_rows", shown);
}

#[test]
fn no_color_boxes_the_approval_keeping_bold_and_dim() {
    let panel = panel_at(
        &queued(&[
            tool_call("a_1", "echo hi"),
            approval_request(
                "r_1",
                "a_1",
                json!({"step": "standing_ask",
                "standing_rule": {"scope": "global", "prefix": "echo hi"}}),
            ),
        ]),
        40,
    );
    let (buf, _) = no_color_surfaced(&panel, 20);
    // No stripe cell anywhere; the head stays bold and the facts dim.
    // The rows come from the drawn text: the header row and the facts
    // row by their content.
    for cell in &buf.content {
        assert_ne!(cell.symbol(), "▌");
    }
    assert!(buf.content.iter().any(|cell| cell.symbol() == "┌"));
    let rows: Vec<String> = (0..20)
        .map(|y| (0..40).map(|x| buf[(x, y)].symbol()).collect())
        .collect();
    let head = rows
        .iter()
        .position(|row| row.contains("Approval 1 of 1"))
        .expect("the header row");
    assert!(
        buf[(1, u16::try_from(head).unwrap_or(u16::MAX))]
            .modifier
            .contains(Modifier::BOLD)
    );
    let facts = rows
        .iter()
        .position(|row| row.contains("runs a command"))
        .expect("the facts row");
    assert!(
        buf[(1, u16::try_from(facts).unwrap_or(u16::MAX))]
            .modifier
            .contains(Modifier::DIM)
    );
}

#[test]
fn without_stripes_the_first_column_keeps_the_tint() {
    crate::surface::init(&|_: &str| None);
    let panel = standing_panel();
    let (_, buf, _) = drawn_at(&panel, 80, 12);
    // The same rows as with stripes, but no stripe cell: the first
    // column keeps the approval tint.
    for y in 3..11 {
        assert_ne!(buf[(0, y)].symbol(), "▌", "row {y}");
        assert_eq!(buf[(0, y)].bg, Role::Approval.color(), "row {y}");
    }
    let (shown, _, _) = drawn_at(&panel, 80, 12);
    assert!(shown.contains("▸ 1 Allow once"), "{shown}");
    crate::surface::init(&|name: &str| {
        if name == "TERM_PROGRAM" {
            Some("ghostty".to_owned())
        } else {
            None
        }
    });
}

/// A one-line panel of `text` with `marks`.
fn marked(text: &str, marks: Vec<crate::approvals::Mark>) -> Panel {
    Panel {
        lines: vec![text.to_owned()],
        marks,
        alert: false,
        spots: Vec::new(),
        cursor: None,
        caret: None,
    }
}

#[test]
fn a_run_from_the_line_start_adds_no_empty_plain_run() {
    let red = ratatui::style::Style::new().fg(Color::Red);
    let panel = marked(
        "abcdef",
        vec![crate::approvals::Mark {
            line: 0,
            bytes: 0..3,
            style: red,
        }],
    );
    assert_eq!(
        super::styled(&panel, 0, "abcdef"),
        ratatui::text::Line::from(vec![
            ratatui::text::Span::styled("abc", red),
            ratatui::text::Span::raw("def"),
        ])
    );
}

#[test]
fn a_run_to_the_line_end_adds_no_empty_plain_run() {
    let red = ratatui::style::Style::new().fg(Color::Red);
    let panel = marked(
        "abcdef",
        vec![crate::approvals::Mark {
            line: 0,
            bytes: 0..6,
            style: red,
        }],
    );
    assert_eq!(
        super::styled(&panel, 0, "abcdef"),
        ratatui::text::Line::from(vec![ratatui::text::Span::styled("abcdef", red)])
    );
}

#[test]
fn long_feedback_keeps_the_cursor_on_the_deny_row() {
    let mut queue = queued(&[approval_request(
        "r_1",
        "a_1",
        json!({"step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": "echo hi"}}),
    )]);
    for _ in 0..100 {
        queue.on_key(&crate::keys::Key::Char('y'));
    }
    // The prefix takes 8 cells of 78, so the row shows the feedback's
    // last 70 characters on one row, with the cursor on its last cell.
    let panel = panel_at(&queue, 80);
    assert_eq!(panel.caret, Some((4, 78)));
    let area = Rect::new(0, 0, 80, 12);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    draw(&panel, area, area.bottom(), &mut buf, &mut targets);
    assert_eq!(
        super::caret(&panel, area, area.bottom()),
        Some(ratatui::layout::Position::new(79, 8))
    );
    // The deny row takes one visual row: the next row is the blank one.
    let (shown, _, _) = drawn_at(&panel, 80, 12);
    let rows: Vec<&str> = shown.lines().collect();
    for (y, row) in rows.iter().enumerate() {
        if row.contains("Deny") {
            assert_eq!(y, 8, "{shown}");
        }
    }
    assert!(rows[8].contains("Deny"), "{shown}");
    // The next row is the blank one: the stripe alone.
    assert_eq!(rows[9], "▌", "{shown}");
}

/// A drag over a question form copies its rows' text: the prompt's
/// wrapped rows join with the space the wrap dropped, and a blank row
/// between the prompt and the options starts its own line
/// (`docs/tui.md`, "Selection and copy").
#[test]
fn a_drag_over_a_question_form_copies_its_wrapped_prompt() {
    use crate::keys::{Button, Mouse, MouseKind};
    let mut app = asking(four_long_options(), 80, 24);
    let (screen, _) = screen(&app, 80, 24);
    let (y, line) = screen
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("description long"))
        .map(|(y, line)| (u16::try_from(y).unwrap_or(u16::MAX), line.to_owned()))
        .expect("the wrapped description draws");
    // The column counts cells, not bytes: the row's marks are multibyte.
    let prefix = line.split("description").next().unwrap_or_default();
    let x = crate::text::to_u16(crate::format::width(prefix));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let _ = screen;
    let mut pointer = crate::mouse::Pointer::default();
    let now = fakes::clock::FakeClock::new().now();
    let mut report = |kind: MouseKind, col: u16, row: u16| {
        let mut buf = Buffer::empty(area);
        let targets = crate::view::render(&app, area, &mut buf, None);
        app.on_mouse(&Mouse { kind, col, row }, &targets, &mut pointer, true, now)
    };
    let _ = report(MouseKind::Press(Button::Left), x, y);
    let _ = report(MouseKind::Drag(Button::Left), x + 10, y);
    assert_eq!(
        report(MouseKind::Release, x + 10, y),
        crate::app::Effect::Copy("description".to_owned())
    );
}
