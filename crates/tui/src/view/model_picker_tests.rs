//! Tests for the model picker's overlay drawing: snapshots, cell
//! colours, the bar, and the scroll window (`docs/tui.md`, "Swapped
//! views", "Look", "Overlays").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use std::path::PathBuf;

use super::{body, id_spans, model_fit, page_step, window};
use crate::app::{App, Effect};
use crate::catalogue::{Catalogue, ModelEntry, Price};
use crate::keys::{Edit, Key};
use crate::mouse::TargetId;
use crate::swapped::Spot;
use crate::theme::Role;
use contract::clock::Clock;

/// An app at `width` by `height`, connected to the hub and attached to
/// a session.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    app.on_line(crate::link::Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app
}

/// Four models over two providers: `acme/m1` with two levels, roles and
/// a price, `acme/m2` with none, `zeta/z1` with one, `zeta/z2` with two
/// and a tiered price. No list times: every section reads unknown.
fn catalogue() -> Catalogue {
    Catalogue {
        models: vec![
            ModelEntry {
                reference: "acme/m1".to_owned(),
                provider: "acme".to_owned(),
                id: "m1".to_owned(),
                levels: vec!["low".to_owned(), "high".to_owned()],
                default_level: Some("high".to_owned()),
                configured: None,
                roles: vec!["deep".to_owned(), "review".to_owned()],
                name: None,
                price: Some(Price {
                    micros_per_mtok: 3_750_000,
                    tiers: Vec::new(),
                }),
            },
            ModelEntry {
                reference: "acme/m2".to_owned(),
                provider: "acme".to_owned(),
                id: "m2".to_owned(),
                levels: Vec::new(),
                default_level: None,
                configured: None,
                roles: Vec::new(),
                name: None,
                price: None,
            },
            ModelEntry {
                reference: "zeta/z1".to_owned(),
                provider: "zeta".to_owned(),
                id: "z1".to_owned(),
                levels: vec!["low".to_owned()],
                default_level: None,
                configured: Some("low".to_owned()),
                roles: Vec::new(),
                name: None,
                price: None,
            },
            ModelEntry {
                reference: "zeta/z2".to_owned(),
                provider: "zeta".to_owned(),
                id: "z2".to_owned(),
                levels: vec!["low".to_owned(), "high".to_owned()],
                default_level: Some("low".to_owned()),
                configured: Some("high".to_owned()),
                roles: vec!["chat".to_owned()],
                name: None,
                price: Some(Price {
                    micros_per_mtok: 2_000_000,
                    tiers: vec![(100_000, 3_000_000)],
                }),
            },
        ],
        notices: Vec::new(),
        lists: Vec::new(),
    }
}

/// Opens the picker.
fn open(app: &mut App) {
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::CtrlL, now), Effect::None);
    assert!(app.model_picker_open());
}

/// Renders `app` on a `width` by `height` screen as text.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

/// Renders `app` on a `width` by `height` screen.
fn buffer(app: &App, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    buf
}

/// The rows with a cell on the selection bar: the bar covers the whole
/// row in `accent`.
fn barred(buf: &Buffer) -> Vec<u16> {
    let area = buf.area;
    (area.top()..area.bottom())
        .filter(|y| (area.left()..area.right()).any(|x| buf[(x, *y)].bg == Role::Accent.color()))
        .collect()
}

/// The screen row holding `needle`, and its text.
fn find_row(text: &str, needle: &str) -> (u16, String) {
    let (at, line) = text
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains(needle))
        .unwrap_or_else(|| panic!("no row holds {needle:?}: {text}"));
    (u16::try_from(at).unwrap_or(u16::MAX), (*line).to_owned())
}

/// The cell column of `needle` in `line`: byte indexes and cell
/// columns part on multibyte text.
fn cell_x(line: &str, needle: &str) -> u16 {
    let byte = line
        .find(needle)
        .unwrap_or_else(|| panic!("no {needle:?} in {line:?}"));
    u16::try_from(crate::format::width(&line[..byte])).unwrap_or(u16::MAX)
}

/// A choosing app with the catalogue read and the picker open.
fn choosing(width: u16, height: u16) -> App {
    let mut app = attached(width, height);
    app.on_models(Ok(catalogue()));
    open(&mut app);
    app
}

#[test]
fn list() {
    let app = choosing(160, 48);
    insta::assert_snapshot!("list", screen(&app, 160, 48));
}

#[test]
fn narrow_list() {
    let app = choosing(100, 40);
    insta::assert_snapshot!("narrow-list", screen(&app, 100, 40));
}

#[test]
fn levels_current_and_saved() {
    let mut app = attached(160, 48);
    app.on_line(crate::link::Line::Session(contract::Envelope {
        kind: "preamble_built".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "reason": "start", "model": "acme/m1", "context_window": 200000,
            "trigger_at": 150000, "thinking": "high",
            "tool_choice": "auto", "cache_lifetime": "5m",
            "system_prompt": "", "tools": [],
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
    app.on_line(crate::link::Line::Session(contract::Envelope {
        kind: "usage_recorded".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "generation_id": "g_1", "model": "acme/m1",
            "tokens": {"input": 1000, "cache_read": 200000,
                "cache_write": {"1h": 34}, "output": 5},
            "input_bytes": 1, "cost": null,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
    app.on_models(Ok(catalogue()));
    open(&mut app);
    insta::assert_snapshot!("levels", screen(&app, 160, 48));
}

#[test]
fn narrow_levels() {
    let mut app = choosing(100, 40);
    for _ in 0..3 {
        let now = fakes::clock::FakeClock::new().now();
        assert_eq!(app.on_key(Key::Down, now), Effect::None);
    }
    insta::assert_snapshot!("narrow-levels", screen(&app, 100, 40));
}

#[test]
fn scoped() {
    let mut app = attached(160, 48);
    app.set_home(crate::home::Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        scoped_models: vec!["acme/m1".to_owned(), "zeta/z2".to_owned()],
        ..Default::default()
    });
    app.on_models(Ok(catalogue()));
    open(&mut app);
    insta::assert_snapshot!("scoped", screen(&app, 160, 48));
}

#[test]
fn narrow_scoped() {
    let mut app = attached(100, 40);
    app.set_home(crate::home::Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        scoped_models: vec!["acme/m1".to_owned(), "zeta/z2".to_owned()],
        ..Default::default()
    });
    app.on_models(Ok(catalogue()));
    open(&mut app);
    insta::assert_snapshot!("narrow-scoped", screen(&app, 100, 40));
}

#[test]
fn scoped_all() {
    let mut app = attached(160, 48);
    app.set_home(crate::home::Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        scoped_models: vec!["acme/m1".to_owned(), "zeta/z2".to_owned()],
        ..Default::default()
    });
    app.on_models(Ok(catalogue()));
    open(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Tab, now), Effect::None);
    insta::assert_snapshot!("scoped-all", screen(&app, 160, 48));
}

#[test]
fn refreshing() {
    let mut app = choosing(160, 48);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::CtrlR, now), Effect::None);
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Every));
    insta::assert_snapshot!("refreshing", screen(&app, 160, 48));
}

#[test]
fn narrow_refreshing() {
    let mut app = choosing(100, 40);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::CtrlR, now), Effect::None);
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Every));
    insta::assert_snapshot!("narrow-refreshing", screen(&app, 100, 40));
}

/// Chooses the selected row for this session only, answering its
/// command, and reopens: the row draws its third row.
fn choose_session_only(app: &mut App) {
    assert!(app.model_picker_open());
    let now = fakes::clock::FakeClock::new().now();
    let pressed = crate::stroke::Stroke::parse("ctrl+s").unwrap();
    let Effect::Send(lines) = app.on_press(pressed, now) else {
        panic!("a session-only choice sends one line");
    };
    assert_eq!(lines.len(), 1);
    let line: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    let id = line["id"].as_str().expect("a command id").to_owned();
    app.on_line(crate::link::Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [("command_id".to_owned(), serde_json::json!(id))]
            .into_iter()
            .collect(),
    }));
    open(app);
}

#[test]
fn session_only() {
    let mut app = choosing(160, 48);
    choose_session_only(&mut app);
    insta::assert_snapshot!("session-only", screen(&app, 160, 48));
}

#[test]
fn narrow_session_only() {
    let mut app = choosing(100, 40);
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..3 {
        assert_eq!(app.on_key(Key::Down, now), Effect::None);
    }
    choose_session_only(&mut app);
    insta::assert_snapshot!("narrow-session-only", screen(&app, 100, 40));
}

#[test]
fn filtered() {
    let mut app = choosing(160, 48);
    let now = fakes::clock::FakeClock::new().now();
    for ch in ['m', '1'] {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    insta::assert_snapshot!("filtered", screen(&app, 160, 48));
}

#[test]
fn narrow_filtered() {
    let mut app = choosing(100, 40);
    let now = fakes::clock::FakeClock::new().now();
    for ch in ['m', '1'] {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    insta::assert_snapshot!("narrow-filtered", screen(&app, 100, 40));
}

#[test]
fn filtered_empty() {
    let mut app = choosing(160, 48);
    let now = fakes::clock::FakeClock::new().now();
    for ch in ['q', 'q', 'q'] {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    insta::assert_snapshot!("filtered-empty", screen(&app, 160, 48));
}

#[test]
fn narrow_filtered_empty() {
    let mut app = choosing(100, 40);
    let now = fakes::clock::FakeClock::new().now();
    for ch in ['q', 'q', 'q'] {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    insta::assert_snapshot!("narrow-filtered-empty", screen(&app, 100, 40));
}

#[test]
fn scope() {
    let mut app = attached(160, 48);
    app.on_models(Ok(catalogue()));
    assert_eq!(app.scoped_models_command(), Effect::None);
    let view = app.model_picker_view().expect("open");
    assert_eq!(
        view.footer,
        [("Space", "mark"), ("Enter", "save"), ("Esc", "back")]
    );
    insta::assert_snapshot!("scope", screen(&app, 160, 48));
}

#[test]
fn narrow_scope() {
    let mut app = attached(100, 40);
    app.on_models(Ok(catalogue()));
    assert_eq!(app.scoped_models_command(), Effect::None);
    insta::assert_snapshot!("narrow-scope", screen(&app, 100, 40));
}

#[test]
fn exactly_one_row_bars_while_a_model_shows() {
    let app = choosing(160, 48);
    let view = app.model_picker_view().expect("open");
    assert_eq!(view.focused, Some(3));
    let bars = body(&view, 160, 48).iter().filter(|row| row.barred).count();
    assert_eq!(bars, 1);
    // Moving the selection moves the bar with it.
    let mut app = choosing(160, 48);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    let view = app.model_picker_view().expect("open");
    assert_eq!(view.focused, Some(4));
    let bars: Vec<usize> = body(&view, 160, 48)
        .iter()
        .enumerate()
        .filter(|(_, row)| row.barred)
        .map(|(at, _)| at)
        .collect();
    assert_eq!(bars.len(), 1);
    // On screen the bar covers exactly one row.
    assert_eq!(barred(&buffer(&app, 160, 48)).len(), 1);
}

#[test]
fn no_bar_with_only_a_message() {
    let mut app = choosing(80, 24);
    let now = fakes::clock::FakeClock::new().now();
    for ch in ['q', 'q', 'q'] {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    let view = app.model_picker_view().expect("open");
    assert!(view.no_match);
    assert_eq!(
        body(&view, 80, 24).iter().filter(|row| row.barred).count(),
        0
    );
    assert!(barred(&buffer(&app, 80, 24)).is_empty());
}

#[test]
fn the_window_keeps_the_focused_model_whole() {
    let mut app = attached(80, 10);
    app.on_models(Ok(catalogue()));
    open(&mut app);
    let view = app.model_picker_view().expect("open");
    // Ten rows hold the filter, the buttons, the heading and one model:
    // the first, focused one.
    assert_eq!(view.focused, Some(3));
    let (start, end) = window(&view, 3);
    assert_eq!((start, end), (0, 1));
    // Past the first model the window follows the focus: `z1` with its
    // heading is all three rows hold.
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    let view = app.model_picker_view().expect("open");
    let (start, end) = window(&view, 3);
    assert_eq!((start, end), (2, 3));
}

#[test]
fn page_steps_cover_the_window_less_one() {
    let app = choosing(160, 48);
    let view = app.model_picker_view().expect("open");
    // Four models fit: a page holds four.
    assert_eq!(page_step(&view, 48), 4);
    // A short screen fits one model: a page moves one.
    assert_eq!(page_step(&view, 10), 1);
}

#[test]
fn ids_draw_in_accent_with_the_query_bold() {
    let mut app = choosing(80, 24);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Char('z'), now), Effect::None);
    let buf = buffer(&app, 80, 24);
    let text = crate::view::text(&buf);
    let (y, line) = find_row(&text, "z2");
    // Off the bar the id reads in `accent`: the hit bold, the rest
    // plain. (`z1` holds the bar, which bolds its whole row.)
    let x = cell_x(&line, "z2");
    assert_eq!(buf[(x, y)].fg, Role::Accent.color());
    assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
    assert_eq!(buf[(x.saturating_add(1), y)].fg, Role::Accent.color());
    assert!(
        !buf[(x.saturating_add(1), y)]
            .modifier
            .contains(Modifier::BOLD)
    );
}

#[test]
fn current_levels_and_refreshing_draw_in_their_colours() {
    let mut app = attached(80, 24);
    app.on_models(Ok(catalogue()));
    open(&mut app);
    // The focused model's levels draw in `attention`, its chip bold.
    let buf = buffer(&app, 80, 24);
    let text = crate::view::text(&buf);
    let (y, line) = find_row(&text, "thinking");
    let x = cell_x(&line, "low");
    assert_eq!(buf[(x, y)].fg, Role::Attention.color());
    // `m1` preselects `high`: the chip reads bold in brackets.
    let x = cell_x(&line, "[high]");
    assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
    // A refreshing provider reads in `attention`.
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::CtrlR, now), Effect::None);
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Every));
    let buf = buffer(&app, 80, 24);
    let text = crate::view::text(&buf);
    let (y, line) = find_row(&text, "⟳ refreshing");
    let x = cell_x(&line, "⟳");
    assert_eq!(buf[(x, y)].fg, Role::Attention.color());
}

#[test]
fn clicks_cover_the_buttons_chips_and_marks() {
    let app = choosing(80, 24);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let ids: Vec<TargetId> = crate::view::render(&app, area, &mut buf, None)
        .iter()
        .map(|target| target.id)
        .collect();
    // The ✕, the refresh button, the name, roles and chips take clicks.
    // Layout rows: 0 the filter, 1 the buttons, 2 the `acme` heading,
    // 3 `acme/m1` with its name, roles and two chips.
    assert!(ids.contains(&TargetId::View(Spot::Close)));
    assert!(ids.contains(&TargetId::View(Spot::Cell(1, 0))));
    assert!(ids.contains(&TargetId::View(Spot::Cell(3, 0))));
    assert!(ids.contains(&TargetId::View(Spot::Cell(3, 1))));
    assert!(ids.contains(&TargetId::View(Spot::Cell(3, 2))));
    assert!(ids.contains(&TargetId::View(Spot::Cell(3, 3))));
    // The filter and the heading take none of their own.
    assert!(ids.iter().all(|id| *id != TargetId::View(Spot::Cell(0, 0))));
    assert!(ids.iter().all(|id| *id != TargetId::View(Spot::Cell(2, 0))));
    assert!(ids.iter().all(|id| *id != TargetId::View(Spot::Cell(2, 1))));
}

#[test]
fn choice_rows_open_with_a_gutter() {
    let app = choosing(80, 24);
    let buf = buffer(&app, 80, 24);
    let text = crate::view::text(&buf);
    // The barred row carries "› " in its gutter...
    let (_, m1) = find_row(&text, "m1");
    assert!(m1.contains("› m1"), "{m1}");
    // ...every other choice row two blank columns, with the levels hung
    // under the id.
    let (_, m2) = find_row(&text, "m2");
    let at = m2.find("m2").unwrap();
    assert!(m2[..at].ends_with("  "), "{m2}");
    assert!(
        text.lines().any(|line| line.contains("      thinking")),
        "{text}"
    );
    // A click on the id still selects its row.
    let area = Rect::new(0, 0, 80, 24);
    let mut scratch = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut scratch, None);
    let (y, line) = find_row(&text, "m2");
    let x = cell_x(&line, "m2");
    assert_eq!(
        crate::mouse::hit(&targets, x, y),
        Some(TargetId::View(Spot::Cell(4, 0)))
    );
}

/// Each span's text, in order.
fn texts(spans: &[ratatui::text::Span<'_>]) -> Vec<String> {
    spans.iter().map(|span| span.content.to_string()).collect()
}

#[test]
fn a_hit_after_plain_text_keeps_the_text_order() {
    let spans = id_spans("m1", &[false, true]);
    assert_eq!(texts(&spans), ["m", "1"]);
    assert_eq!(
        spans[1].style,
        Style::new()
            .fg(Role::Accent.color())
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::UNDERLINED)
    );
}

#[test]
fn a_plain_run_between_two_hits_splits_them() {
    let spans = id_spans("a1b", &[true, false, true]);
    assert_eq!(texts(&spans), ["a", "1", "b"]);
    let accent = Style::new().fg(Role::Accent.color());
    let bold = accent
        .add_modifier(Modifier::BOLD)
        .add_modifier(Modifier::UNDERLINED);
    assert_eq!(spans[0].style, bold);
    assert_eq!(spans[1].style, accent);
    assert_eq!(spans[2].style, bold);
}

#[test]
fn a_long_id_puts_the_roles_cell_after_its_name() {
    let mut cat = catalogue();
    cat.models[0].id = "longname1".to_owned();
    let mut app = attached(80, 24);
    app.on_models(Ok(cat));
    open(&mut app);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    let text = crate::view::text(&buf);
    let (y, line) = find_row(&text, "longname1");
    let roles_at = cell_x(&line, " [deep]");
    assert_eq!(
        crate::mouse::hit(&targets, roles_at - 1, y),
        Some(TargetId::View(Spot::Cell(3, 0)))
    );
    assert_eq!(
        crate::mouse::hit(&targets, roles_at, y),
        Some(TargetId::View(Spot::Cell(3, 1)))
    );
}

#[test]
fn the_level_chips_take_clicks_past_the_thinking_label() {
    let app = choosing(80, 24);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    let text = crate::view::text(&buf);
    let (y, line) = find_row(&text, "thinking");
    let low = cell_x(&line, "low");
    let high = cell_x(&line, "[high]");
    assert_eq!(
        crate::mouse::hit(&targets, low, y),
        Some(TargetId::View(Spot::Cell(3, 2)))
    );
    assert_eq!(
        crate::mouse::hit(&targets, high, y),
        Some(TargetId::View(Spot::Cell(3, 3)))
    );
}

#[test]
fn an_unfocused_row_draws_its_saved_level_dim() {
    let app = choosing(80, 24);
    let buf = buffer(&app, 80, 24);
    let text = crate::view::text(&buf);
    let (y, _) = find_row(&text, "z2 [chat]");
    let below = text
        .lines()
        .nth(usize::from(y) + 1)
        .expect("z2's second row");
    let x = cell_x(below, "[high]");
    let cell = &buf[(x, y + 1)];
    assert_ne!(cell.fg, Role::Attention.color());
    assert!(!cell.modifier.contains(Modifier::BOLD));
}

#[test]
fn the_focused_saved_level_is_bold_when_another_chip_is_chosen() {
    let mut app = choosing(80, 24);
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..3 {
        assert_eq!(app.on_key(Key::Down, now), Effect::None);
    }
    app.on_edit(Edit::Left);
    let buf = buffer(&app, 80, 24);
    let text = crate::view::text(&buf);
    let (y, line) = find_row(&text, "[low] high");
    let x = cell_x(&line, "high");
    assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
    assert_eq!(buf[(x, y)].fg, Role::Attention.color());
}

#[test]
fn a_checklist_row_cuts_its_id_from_the_roles_cell() {
    let mut app = attached(160, 48);
    app.on_models(Ok(catalogue()));
    assert_eq!(app.scoped_models_command(), Effect::None);
    let area = Rect::new(0, 0, 160, 48);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    let text = crate::view::text(&buf);
    let (y, line) = find_row(&text, "m1 [deep]");
    let id_at = cell_x(&line, "m1");
    let roles_at = cell_x(&line, " [deep]");
    assert_eq!(
        crate::mouse::hit(&targets, id_at, y),
        Some(TargetId::View(Spot::Cell(2, 1)))
    );
    assert_eq!(
        crate::mouse::hit(&targets, roles_at, y),
        Some(TargetId::View(Spot::Cell(2, 2)))
    );
}

#[test]
fn a_window_starting_on_a_heading_takes_one_row_for_it() {
    let mut app = choosing(160, 48);
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..2 {
        assert_eq!(app.on_key(Key::Down, now), Effect::None);
    }
    let view = app.model_picker_view().expect("open");
    let z1 = view
        .sections
        .iter()
        .flat_map(|section| section.models.iter())
        .find(|model| model.id == "z1")
        .expect("z1 is shown")
        .at;
    assert_eq!(view.focused, Some(z1));
    assert_eq!(window(&view, 5), (2, 4));
}

#[test]
fn a_session_only_model_takes_three_rows_in_the_window() {
    let mut app = choosing(160, 48);
    choose_session_only(&mut app);
    let view = app.model_picker_view().expect("open");
    assert_eq!(view.focused, Some(3));
    assert_eq!(window(&view, 5), (0, 1));
    assert_eq!(window(&view, 6), (0, 2));
}

#[test]
fn a_refreshing_empty_filter_reserves_its_status_and_message_rows() {
    let mut app = choosing(160, 48);
    let now = fakes::clock::FakeClock::new().now();
    for ch in ['q', 'q', 'q'] {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    assert_eq!(app.on_key(Key::CtrlR, now), Effect::None);
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Every));
    let view = app.model_picker_view().expect("open");
    assert!(view.no_match);
    assert_eq!(view.status.len(), 1);
    // The filter row, the buttons row, the status line and the no-match line.
    assert_eq!(model_fit(&view, 24), usize::from(24 - super::CHROME) - 4);
}

#[test]
fn the_refresh_click_sits_under_its_text_on_a_long_checklist() {
    let mut cat = catalogue();
    cat.models.truncate(1);
    cat.models[0].id = "x".repeat(60);
    cat.models[0].roles = Vec::new();
    let mut app = attached(160, 48);
    app.on_models(Ok(cat));
    assert_eq!(app.scoped_models_command(), Effect::None);
    let area = Rect::new(0, 0, 160, 48);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    let text = crate::view::text(&buf);
    let (y, line) = find_row(&text, "⟳ refresh all");
    let x = cell_x(&line, "⟳");
    assert_eq!(
        crate::mouse::hit(&targets, x, y),
        Some(TargetId::View(Spot::Cell(0, 0)))
    );
}
