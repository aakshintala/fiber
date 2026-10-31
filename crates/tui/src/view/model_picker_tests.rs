//! Tests for the model picker's overlay drawing: snapshots, cell
//! colours, the bar, and the scroll window (`docs/tui.md`, "Swapped
//! views", "Look", "Overlays").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use std::path::PathBuf;

use super::{
    body, display_rows, focus_window_top, id_spans, keep_focus_visible_top, model_fit, page_step,
    picker_wheel_window, window,
};
use crate::app::{App, Effect};
use crate::catalogue::{Catalogue, LeftOutGroup, LeftOutModel, ModelEntry, Price};
use crate::keys::{Edit, Key, Mouse, MouseKind};
use crate::model_picker::{InstallRow, PickerView, Progress};
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
        installable: Vec::new(),
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
                    input: 3_000_000,
                    output: 15_000_000,
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
                    input: 2_000_000,
                    output: 8_000_000,
                    tiers: vec![crate::catalogue::PriceTier {
                        input_tokens_above: 100_000,
                        micros_per_mtok: 3_000_000,
                        input: 3_000_000,
                        output: 12_000_000,
                    }],
                }),
            },
        ],
        notices: Vec::new(),
        lists: Vec::new(),
        left_out: Vec::new(),
        ..Default::default()
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

/// Priced, free, unpriced and tiered models with no role or level cells.
fn priced() -> Catalogue {
    let fixture = catalogue();
    let prices = [
        fixture.models[0].price.clone(),
        Some(Price {
            micros_per_mtok: 0,
            input: 0,
            output: 0,
            tiers: vec![],
        }),
        None,
        fixture.models[3].price.clone(),
    ];
    Catalogue {
        models: prices
            .into_iter()
            .enumerate()
            .map(|(at, price)| {
                let id = format!("p{}", at + 1);
                ModelEntry {
                    reference: format!("acme/{id}"),
                    provider: "acme".to_owned(),
                    id,
                    name: None,
                    levels: vec![],
                    default_level: None,
                    configured: None,
                    roles: vec![],
                    price,
                }
            })
            .collect(),
        ..Default::default()
    }
}

/// An attached session with a last call only when `usage` names its input.
fn priced_session(usage: Option<u64>) -> App {
    let mut app = attached(160, 48);
    let mut event = contract::Envelope {
        kind: "preamble_built".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "reason": "start", "model": "acme/p1", "context_window": 200000,
            "trigger_at": 150000, "thinking": null, "tool_choice": "auto",
            "cache_lifetime": "5m", "system_prompt": "", "tools": [],
        })
        .as_object()
        .unwrap()
        .clone(),
    };
    app.on_line(crate::link::Line::Session(event.clone()));
    if let Some(input) = usage {
        event.kind = "usage_recorded".to_owned();
        event.payload = serde_json::json!({
            "generation_id": "g_1", "model": "acme/p1",
            "tokens": {"input": input, "cache_read": 0, "cache_write": {}, "output": 0},
            "input_bytes": 1, "cost": null,
        })
        .as_object()
        .unwrap()
        .clone();
        app.on_line(crate::link::Line::Session(event));
    }
    app.on_models(Ok(priced()));
    open(&mut app);
    app
}

#[test]
fn no_conversation_shows_prices_and_no_switch_cost() {
    let app = priced_session(None);
    let text = screen(&app, 160, 48);
    let (_, p1) = find_row(&text, "p1");
    let (_, p2) = find_row(&text, "p2");
    let (_, p3) = find_row(&text, "p3");
    let (_, p4) = find_row(&text, "p4");
    assert!(p1.contains("$3.00 in · $15.00 out /M"), "{p1}");
    assert!(p2.contains("free"), "{p2}");
    assert!(!p2.contains("/M"), "{p2}");
    assert!(!p3.contains("/M"), "{p3}");
    assert!(!p3.contains("free"), "{p3}");
    assert!(p4.contains("$2.00 in · $8.00 out /M"), "{p4}");
    assert!(!p4.contains("tier"), "{p4}");
    for row in [p1, p2, p3, p4] {
        assert!(!row.contains("switch"), "{row}");
        assert!(!row.contains('—'), "{row}");
    }
    insta::assert_snapshot!("prices-empty", text);
}

#[test]
fn a_conversation_labels_the_switch_cost() {
    let app = priced_session(Some(84_000));
    let text = screen(&app, 160, 48);
    let (_, p1) = find_row(&text, "p1");
    let (_, p2) = find_row(&text, "p2");
    let (_, p3) = find_row(&text, "p3");
    let (_, p4) = find_row(&text, "p4");
    assert!(p1.ends_with('—'), "{p1}");
    assert!(!p1.contains("switch"), "{p1}");
    assert!(p1.contains("$3.00 in · $15.00 out /M"), "{p1}");
    assert!(p2.contains("free"), "{p2}");
    assert!(p2.contains("switch ~84k tokens · $0.00"), "{p2}");
    assert!(p3.contains("switch ~84k tokens"), "{p3}");
    assert!(!p3.contains('$'), "{p3}");
    assert!(p4.contains("$2.00 in · $8.00 out /M"), "{p4}");
    assert!(p4.contains("switch ~84k tokens · $0.17"), "{p4}");
    insta::assert_snapshot!("prices", text);
}

#[test]
fn the_tier_in_force_turns_over_past_its_threshold() {
    for usage in [99_999, 100_000, 100_001] {
        let app = priced_session(Some(usage));
        let (_, row) = find_row(&screen(&app, 160, 48), "p4");
        if usage == 100_001 {
            assert!(
                row.contains("$3.00 in · $12.00 out /M · tier 100k+"),
                "{row}"
            );
            assert!(row.contains("switch ~100k · $0.30"), "{row}");
        } else {
            assert!(row.contains("$2.00 in · $8.00 out /M"), "{row}");
            assert!(!row.contains("tier"), "{row}");
            assert!(row.contains("switch ~100k tokens · $0.20"), "{row}");
        }
    }
}

#[test]
fn a_narrow_screen_drops_the_prices_whole() {
    let app = priced_session(Some(84_000));
    let view = app.model_picker_view().unwrap();
    for (width, priced) in [(63, false), (64, true), (65, true)] {
        let rows = body(&view, width, 48);
        let text: String = rows
            .iter()
            .flat_map(|row| row.spans.iter().chain(&row.right))
            .map(|span| span.content.as_ref())
            .collect();
        for id in ["p1", "p2", "p3", "p4"] {
            assert!(text.contains(id), "{text}");
        }
        assert!(text.contains("switch"), "{text}");
        assert_eq!(text.contains("/M"), priced, "{text}");
        assert_eq!(text.contains("free"), priced, "{text}");
    }
}

#[test]
fn the_prices_cell_takes_no_click_target() {
    let app = priced_session(Some(84_000));
    let view = app.model_picker_view().unwrap();
    for model in [&view.sections[0].models[0], &view.sections[0].models[2]] {
        let row = super::model_first_row(model, false, true);
        assert_eq!(
            row.targets,
            [
                (0, u16::MAX, TargetId::View(Spot::Row(model.at))),
                (2, 4, TargetId::View(Spot::Cell(model.at, 0))),
            ]
        );
    }
}

#[test]
fn checklist_rows_carry_prices_and_the_switch_label() {
    let mut app = priced_session(Some(84_000));
    assert_eq!(app.scoped_models_command(), Effect::None);
    let text = screen(&app, 160, 48);
    let (_, p2) = find_row(&text, "p2");
    assert!(p2.contains("free"), "{p2}");
    assert!(p2.contains("switch ~84k tokens · $0.00"), "{p2}");
    let (_, p1) = find_row(&text, "p1");
    assert!(p1.ends_with('—'), "{p1}");
    assert!(p1.contains("$3.00 in · $15.00 out /M"), "{p1}");
    let view = app.model_picker_view().unwrap();
    let narrow = body(&view, 63, 48);
    let text: String = narrow
        .iter()
        .flat_map(|row| &row.spans)
        .map(|span| span.content.as_ref())
        .collect();
    assert!(!text.contains("/M"), "{text}");
    assert!(!text.contains("free"), "{text}");
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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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
        [
            ("↑↓", "move"),
            ("Enter", "toggle"),
            ("Ctrl+S", "save"),
            ("Esc", "close")
        ]
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
fn failed_install_error_rows_take_space_in_the_focus_window() {
    // A failed install keeps its error row below the provider (`docs/tui.md:740-742`).
    let view = PickerView {
        filter: String::new(),
        count: String::new(),
        toggle: None,
        buttons_at: 1,
        sections: Vec::new(),
        no_match: false,
        status: Vec::new(),
        footer: Vec::new(),
        refresh: true,
        spinner: String::new(),
        focused: Some(2),
        top: 0,
        logged_out: Vec::new(),
        installs: vec![
            InstallRow {
                at: 2,
                provider: "failed".to_owned(),
                hits: Vec::new(),
                progress: Some(Progress::Failed("no network".to_owned())),
            },
            InstallRow {
                at: 3,
                provider: "next".to_owned(),
                hits: Vec::new(),
                progress: None,
            },
        ],
        outcomes: Vec::new(),
    };

    assert_eq!(window(&view, 2), (0, 1));
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
        Some(TargetId::View(Spot::Cell(3, 1)))
    );
    assert_eq!(
        crate::mouse::hit(&targets, roles_at, y),
        Some(TargetId::View(Spot::Cell(3, 2)))
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

/// The wheel moves the picker top by three body rows (`docs/tui.md:576`).
#[test]
fn wheel_moves_the_picker_top_by_three_body_rows() {
    let mut app = attached(100, 18);
    let mut catalogue = catalogue();
    catalogue.models = (0..20)
        .map(|at| ModelEntry {
            reference: format!("acme/m{at}"),
            provider: "acme".to_owned(),
            id: format!("m{at}"),
            levels: vec!["low".to_owned()],
            default_level: Some("low".to_owned()),
            configured: None,
            roles: Vec::new(),
            name: None,
            price: None,
        })
        .collect();
    app.on_models(Ok(catalogue.clone()));
    open(&mut app);
    assert_eq!(app.model_picker_view().unwrap().top, 0);

    app.on_wheel(&Mouse {
        kind: MouseKind::WheelDown,
        col: 50,
        row: 8,
    });

    assert_eq!(app.model_picker_view().unwrap().top, 3);

    // The renderer and top clamp keep the first row of a focused
    // multi-row model visible when it touches either viewport edge.
    let mut view = app.model_picker_view().unwrap();
    let height = u16::try_from(app.conversation_area_height())
        .unwrap_or(u16::MAX)
        .saturating_sub(1);
    let fit = model_fit(&view, height);
    assert!(fit > 0);
    let display = display_rows(&view, false);
    let first = view.sections[0].models.first().expect("first model").at;
    let first_row = display
        .rows
        .iter()
        .position(|row| row.focus == Some(first))
        .expect("first model row");
    view.focused = Some(first);
    view.top = first_row;
    assert_eq!(keep_focus_visible_top(&view, height), first_row);

    let last = view.sections[0].models.last().expect("last model").at;
    let last_row = display
        .rows
        .iter()
        .position(|row| row.focus == Some(last))
        .expect("last model row");
    view.focused = Some(last);
    view.top = last_row + 1 - fit;
    assert_eq!(keep_focus_visible_top(&view, height), view.top);

    // With more body rows, navigation and a catalogue refresh retain a
    // still-valid wheel top; changing the query resets to the focus window.
    app.set_size(100, 24);
    assert_eq!(
        app.on_key(Key::Down, fakes::clock::FakeClock::new().now()),
        Effect::None
    );
    assert_eq!(app.model_picker_view().unwrap().top, 3);
    app.on_models(Ok(catalogue));
    assert_eq!(app.model_picker_view().unwrap().top, 3);
    assert_eq!(
        app.on_key(Key::Char('m'), fakes::clock::FakeClock::new().now()),
        Effect::None
    );
    let view = app.model_picker_view().unwrap();
    let height = u16::try_from(app.conversation_area_height())
        .unwrap_or(u16::MAX)
        .saturating_sub(1);
    assert_eq!(
        view.top,
        focus_window_top(&view, height),
        "filter changes restore the focus-derived window"
    );
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
fn the_checklist_mark_is_bold_on_focus_and_dim_unmarked() {
    let mut app = attached(80, 24);
    app.on_models(Ok(catalogue()));
    assert_eq!(app.scoped_models_command(), Effect::None);
    let now = fakes::clock::FakeClock::new().now();
    // Mark `m2`: the focused `m1` stays unmarked.
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    let buf = buffer(&app, 80, 24);
    let text = crate::view::text(&buf);
    // The focused mark is bold and black on `accent`, and dim while
    // unmarked: the bar keeps a span's dim.
    let (y, line) = find_row(&text, "[ ] m1");
    let x = cell_x(&line, "[ ]");
    assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
    assert!(buf[(x, y)].modifier.contains(Modifier::DIM));
    assert_eq!(buf[(x, y)].fg, crate::look::BAR_TEXT);
    assert_eq!(buf[(x, y)].bg, Role::Accent.color());
    // The focused id reads bold in black on `accent`.
    let x = cell_x(&line, "m1");
    assert_eq!(buf[(x, y)].fg, crate::look::BAR_TEXT);
    assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
    // An unfocused marked mark is neither bold nor dim.
    let (y, line) = find_row(&text, "[x] m2");
    let x = cell_x(&line, "[x]");
    assert!(!buf[(x, y)].modifier.contains(Modifier::BOLD));
    assert!(!buf[(x, y)].modifier.contains(Modifier::DIM));
    // An unfocused unmarked mark is dim and not bold, and so is its id.
    let (y, line) = find_row(&text, "[ ] z1");
    let x = cell_x(&line, "[ ]");
    assert!(buf[(x, y)].modifier.contains(Modifier::DIM));
    assert!(!buf[(x, y)].modifier.contains(Modifier::BOLD));
    let x = cell_x(&line, "z1");
    assert_eq!(buf[(x, y)].fg, Role::Accent.color());
    assert!(!buf[(x, y)].modifier.contains(Modifier::BOLD));
}

#[test]
fn the_checklist_thinking_row_is_dim_with_no_chip_targets() {
    let mut app = attached(80, 24);
    app.on_models(Ok(catalogue()));
    assert_eq!(app.scoped_models_command(), Effect::None);
    let view = app.model_picker_view().expect("open");
    let at = view.sections[0].models[0].at;
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    let text = crate::view::text(&buf);
    // `m1` is focused, yet its thinking row reads dim throughout, with
    // no bracketed chip.
    let (y, line) = find_row(&text, "thinking low high");
    assert!(!line.contains('['), "{line}");
    for needle in ["low", "high"] {
        let x = cell_x(&line, needle);
        assert!(buf[(x, y)].modifier.contains(Modifier::DIM), "{line}");
    }
    // Every level's first column selects the row: no chip cell sits
    // there.
    for needle in ["low", "high"] {
        let x = cell_x(&line, needle);
        assert_eq!(
            crate::mouse::hit(&targets, x, y),
            Some(TargetId::View(Spot::Row(at)))
        );
    }
    // No session-only row is drawn on the checklist.
    assert!(!text.contains("ⓢ"), "{text}");
}

#[test]
fn a_long_checklist_draws_no_refresh_button() {
    let mut app = attached(160, 48);
    app.on_models(Ok(catalogue()));
    assert_eq!(app.scoped_models_command(), Effect::None);
    let view = app.model_picker_view().expect("open");
    assert!(!view.refresh);
    assert_eq!(view.buttons_at, 1);
    let area = Rect::new(0, 0, 160, 48);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    let text = crate::view::text(&buf);
    assert!(!text.contains("⟳ refresh all"), "{text}");
    let ids: Vec<TargetId> = targets.iter().map(|target| target.id).collect();
    assert!(
        ids.iter().all(|id| *id != TargetId::View(Spot::Cell(1, 0))),
        "a refresh target is drawn"
    );
}

#[test]
fn scope_filtered() {
    let mut app = attached(160, 48);
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        scoped_models: vec!["acme/m1".to_owned(), "zeta/z2".to_owned()],
        ..Default::default()
    });
    app.on_models(Ok(catalogue()));
    assert_eq!(app.scoped_models_command(), Effect::None);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Char('z'), now), Effect::None);
    let view = app.model_picker_view().expect("open");
    assert_eq!(view.count, " 2 of 4 models · 2 marked ");
    assert_eq!(
        view.footer,
        [
            ("↑↓", "move"),
            ("Enter", "toggle"),
            ("Ctrl+S", "save"),
            ("Esc", "close")
        ]
    );
    let text = screen(&app, 160, 48);
    assert!(text.contains("[ ] z1"), "{text}");
    assert!(text.contains("[x] z2"), "{text}");
    insta::assert_snapshot!("scope-filtered", text);
}

#[test]
fn scope_filtered_empty() {
    let mut app = attached(160, 48);
    app.on_models(Ok(catalogue()));
    assert_eq!(app.scoped_models_command(), Effect::None);
    let now = fakes::clock::FakeClock::new().now();
    for ch in ['q', 'q', 'q'] {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    let view = app.model_picker_view().expect("open");
    assert_eq!(view.count, " 0 of 4 models · 0 marked ");
    assert!(view.no_match);
    assert!(view.sections.is_empty());
    let text = screen(&app, 160, 48);
    assert!(text.contains("No models match"), "{text}");
    insta::assert_snapshot!("scope-filtered-empty", text);
}

#[test]
fn a_huge_checklist_id_saturates_its_click_targets() {
    let mut cat = catalogue();
    cat.models.truncate(1);
    cat.models[0].id = "x".repeat(65_530);
    cat.models[0].roles = vec!["chat".to_owned()];
    let mut app = attached(160, 48);
    app.on_models(Ok(cat));
    assert_eq!(app.scoped_models_command(), Effect::None);
    let view = app.model_picker_view().expect("open");
    assert_eq!(view.sections[0].models[0].id.len(), 65_530);
    // Rendering the rows never overflows: every endpoint saturates.
    let rows = body(&view, 160, 48);
    let barred: Vec<_> = rows.iter().filter(|row| row.barred).collect();
    assert_eq!(barred.len(), 1);
    let at = view.sections[0].models[0].at;
    assert_eq!(
        barred[0].targets,
        vec![
            (0, u16::MAX, TargetId::View(Spot::Row(at))),
            (2, 6, TargetId::View(Spot::Cell(at, 0))),
            (6, u16::MAX, TargetId::View(Spot::Cell(at, 1))),
            (u16::MAX, u16::MAX, TargetId::View(Spot::Cell(at, 2))),
        ]
    );
    for row in &rows {
        for (start, end, _) in &row.targets {
            assert!(start <= end);
        }
    }
    // The whole frame renders too, within its 160 columns.
    let area = Rect::new(0, 0, 160, 48);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    for line in crate::view::text(&buf).lines() {
        assert!(crate::format::width(line) <= 160, "{line:.80}...");
    }
}

/// The non-blank body lines hold `needles` in order; every other
/// assertion reads the same text.
fn body_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_short_overlay_keeps_the_focused_model_row() {
    // A Choose open and the checklist open, each with the first model
    // focused over `catalogue()`: ten rows hold the filter, the
    // buttons, the heading and the focused row, eleven add its
    // thinking row, and nine cut the heading rather than the focused
    // row. The checklist differs only in its count.
    for (choose, count) in [(true, "4 models"), (false, "0 of 4 marked")] {
        for (height, heading, thinking) in
            [(10u16, true, false), (11, true, true), (9, false, false)]
        {
            let mut app = attached(80, height);
            app.on_models(Ok(catalogue()));
            if choose {
                open(&mut app);
            } else {
                assert_eq!(app.scoped_models_command(), Effect::None);
            }
            let view = app.model_picker_view().expect("open");
            let area = Rect::new(0, 0, 80, height);
            let mut buf = Buffer::empty(area);
            super::draw(&view, area, &mut buf, &mut Vec::new());
            let text = crate::view::text(&buf);
            let lines = body_lines(&text);
            let mut at = 0;
            for needle in ["Type to search", count, "m1"] {
                let pos = lines
                    .iter()
                    .skip(at)
                    .position(|line| line.contains(needle))
                    .unwrap_or_else(|| panic!("{needle:?} missing in order: {text}"));
                at += pos + 1;
            }
            assert_eq!(
                lines.iter().any(|line| line.contains("acme")),
                heading,
                "{text}"
            );
            assert_eq!(
                lines.iter().any(|line| line.contains("thinking")),
                thinking,
                "{text}"
            );
            // The focused row sits on the bar.
            let (y, _) = find_row(&text, "m1");
            assert!(
                (0..area.width).any(|x| buf[(x, y)].bg == Role::Accent.color()),
                "no bar on {text}"
            );
        }
    }
}

#[test]
fn the_window_extends_upward_to_exactly_the_rows_that_fit() {
    let mut app = choosing(160, 48);
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..2 {
        assert_eq!(app.on_key(Key::Down, now), Effect::None);
    }
    let view = app.model_picker_view().expect("open");
    // `m2` continues its provider with no heading and `z1` opens one:
    // 2 + 2 + 2 is six rows, exactly what fits, so `m2` stays in.
    assert_eq!(window(&view, 6), (1, 3));
    // One row fewer drops `m2`.
    assert_eq!(window(&view, 5), (2, 4));
}

/// A drag over a chosen chip copies the level without its brackets: a
/// span that is nothing but `[text]` is a chosen chip, and any other
/// brackets stay as drawn (`docs/tui.md`, "Selection and copy").
#[test]
fn a_drag_over_a_chosen_chip_copies_the_level_without_brackets() {
    use crate::keys::{Button, Mouse, MouseKind};
    let mut app = choosing(80, 24);
    let area = Rect::new(0, 0, 80, 24);
    let text = crate::view::text(&buffer(&app, 80, 24));
    let (y, line) = find_row(&text, "[high]");
    let x = cell_x(&line, "[high]");
    let mut pointer = crate::mouse::Pointer::default();
    let now = fakes::clock::FakeClock::new().now();
    let mut report = |kind: MouseKind, col: u16, row: u16| {
        let mut buf = Buffer::empty(area);
        let targets = crate::view::render(&app, area, &mut buf, None);
        app.on_mouse(&Mouse { kind, col, row }, &targets, &mut pointer, true, now)
    };
    let _ = report(MouseKind::Press(Button::Left), x, y);
    let _ = report(MouseKind::Drag(Button::Left), x + 5, y);
    assert_eq!(
        report(MouseKind::Release, x + 5, y),
        crate::app::Effect::Copy("high".to_owned())
    );
}

/// The fixture catalogue with left-out groups: `acme` counts two
/// models under its credential label, `opencode` counts one behind a
/// login and lists nothing.
fn left_out_catalogue() -> Catalogue {
    let mut catalogue = catalogue();
    catalogue.left_out = vec![
        LeftOutGroup {
            provider: "acme".to_owned(),
            extension: "acme-ext".to_owned(),
            file: "config/acme-ext.json".to_owned(),
            setting: "api_key".to_owned(),
            login: false,
            models: vec![
                LeftOutModel {
                    id: "m9".to_owned(),
                    name: Some("Acme Nine".to_owned()),
                },
                LeftOutModel {
                    id: "m10".to_owned(),
                    name: None,
                },
            ],
        },
        LeftOutGroup {
            provider: "opencode".to_owned(),
            extension: "github.com/aakshintala/fiber/providers/opencode".to_owned(),
            file: "config/opencode.json".to_owned(),
            setting: "workspace".to_owned(),
            login: true,
            models: vec![LeftOutModel {
                id: "o1".to_owned(),
                name: None,
            }],
        },
    ];
    catalogue
}

/// A choosing app with the left-out catalogue read and the picker open.
fn choosing_left_out(width: u16, height: u16) -> App {
    let mut app = attached(width, height);
    app.on_models(Ok(left_out_catalogue()));
    open(&mut app);
    app
}

#[test]
fn count_rows_draw_dim_with_a_four_column_indent_and_no_targets() {
    // One count row per group after its heading, before its models (`docs/tui.md:661-668`).
    let app = choosing_left_out(160, 48);
    let view = app.model_picker_view().expect("open");
    let body = body(&view, 160, 48);
    let rows: Vec<_> = body
        .iter()
        .filter(|row| {
            row.spans
                .iter()
                .any(|span| span.content.contains("api_key") || span.content.contains("workspace"))
        })
        .collect();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert!(row.targets.is_empty());
        assert!(!row.barred);
        assert_eq!(row.spans.len(), 2);
        assert_eq!(row.spans[0].content.as_ref(), "    ");
        assert_eq!(row.spans[0].style, Style::new());
        for span in &row.spans[1..] {
            assert_eq!(span.style, Style::new().add_modifier(Modifier::DIM));
        }
    }
}

#[test]
fn count_rows_name_the_supplied_files() {
    // One count row per group after its heading, before its models (`docs/tui.md:661-668`).
    let app = choosing_left_out(160, 48);
    let text = screen(&app, 160, 48);
    let (_, api) = find_row(&text, "api_key");
    assert!(
        api.contains("2 models need api_key: set it in `config/acme-ext.json`"),
        "{api}"
    );
    let (_, workspace) = find_row(&text, "needs workspace");
    assert!(
        workspace.contains("1 model needs workspace: run `fiber login opencode`"),
        "{workspace}"
    );
    // The row never derives a path from the full extension name.
    assert!(!text.contains("github.com"), "{text}");
}

#[test]
fn count_rows_scroll_with_their_sections_window() {
    // One count row per group after its heading, before its models (`docs/tui.md:661-668`).
    let app = choosing_left_out(160, 48);
    let view = app.model_picker_view().expect("open");
    // Items in draw order: the `acme` count, its two models, the
    // `opencode` count, then the two `zeta` models. The focus sits on
    // `acme/m1`.
    assert_eq!(view.focused, Some(4));
    assert_eq!(window(&view, 2), (1, 2));
    assert_eq!(window(&view, 4), (0, 2));
    assert_eq!(window(&view, 6), (0, 3));
    // Headings, counts, model rows and blanks sum exactly: 6 + 1 + 5
    // + 1 + 2, past the filter and the buttons.
    assert_eq!(body(&view, 160, 48).len(), 2 + 15);
}

/// Wheel offsets count headings and blank rows as displayed rows (`docs/tui.md:576`).
#[test]
fn wheel_offsets_count_headings_and_blank_rows_as_displayed_rows() {
    let mut app = choosing_left_out(160, 18);
    let view = app.model_picker_view().expect("open");
    let height = u16::try_from(app.conversation_area_height())
        .unwrap_or(u16::MAX)
        .saturating_sub(1);
    let display = display_rows(&view, false);
    assert!(
        display.rows[1]
            .row
            .spans
            .iter()
            .any(|span| span.content.contains("api_key"))
    );
    // The wheel advances three displayed rows per step (`docs/tui.md:576`);
    // headings, counts and models all count as displayed rows here.
    let (top, _) = picker_wheel_window(&view, false, height);
    assert_eq!(
        top, 3,
        "heading, count and model rows each consume an offset"
    );
    app.model_picker_wheel(false);
    assert_eq!(app.model_picker_view().unwrap().top, top);
}

#[test]
fn a_wheel_with_no_focus_keeps_focus_absent() {
    let mut app = attached(80, 18);
    app.on_models(Ok(Catalogue::default()));
    open(&mut app);
    assert_eq!(app.model_picker_view().unwrap().focused, None);
    app.model_picker_wheel(false);
    let view = app.model_picker_view().expect("picker stays open");
    assert_eq!(view.focused, None);
    assert_eq!(view.top, 0);
}

#[test]
fn an_all_left_out_section_stays_visible_in_a_short_viewport() {
    // One count row per group after its heading, before its models (`docs/tui.md:661-668`).
    let catalogue = Catalogue {
        left_out: vec![LeftOutGroup {
            provider: "opencode".to_owned(),
            extension: "github.com/aakshintala/fiber/providers/opencode".to_owned(),
            file: "config/opencode.json".to_owned(),
            setting: "workspace".to_owned(),
            login: true,
            models: vec![LeftOutModel {
                id: "o1".to_owned(),
                name: None,
            }],
        }],
        ..Catalogue::default()
    };
    let mut app = attached(80, 10);
    app.on_models(Ok(catalogue.clone()));
    open(&mut app);
    // The opening read answered: the status below is the catalogue's
    // own, not the loading line.
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Stale));
    app.on_models(Ok(catalogue));
    let view = app.model_picker_view().expect("open");
    assert_eq!(window(&view, 2), (0, 1));
    // The windowed body keeps the heading and its count row: the
    // section is not dropped from the viewport accounting.
    let lines: Vec<String> = body(&view, 80, 10)
        .iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect())
        .collect();
    assert!(
        lines.iter().any(|line| line.contains("opencode")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("1 model needs workspace: run `fiber login opencode`")),
        "{lines:?}"
    );
    // End to end, the count row renders under its heading.
    let text = screen(&app, 80, 24);
    assert!(text.contains("opencode"), "{text}");
    assert!(
        text.contains("1 model needs workspace: run `fiber login opencode`"),
        "{text}"
    );
}

#[test]
fn paging_moves_across_count_rows_and_sections() {
    // One count row per group after its heading, before its models (`docs/tui.md:661-668`).
    let mut app = choosing_left_out(160, 10);
    // Six windowed items, two of them count rows: four focusable stops
    // make the page step four, not six.
    let view = app.model_picker_view().expect("open");
    assert_eq!(page_step(&view, 48), 4);
    // A short screen fits one model: a page moves one.
    assert_eq!(page_step(&view, 10), 1);
    // The opening read answered, so the loading line takes no row.
    let catalogue = left_out_catalogue();
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Stale));
    app.on_models(Ok(catalogue));
    // Two steps down lands on `zeta/z1`: the `acme` count row scrolled
    // out with its section.
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    let view = app.model_picker_view().expect("open");
    assert_eq!(window(&view, 2), (4, 5));
    // The windowed body keeps `zeta/z1` and drops the `acme` count row
    // with its section.
    let lines: Vec<String> = body(&view, 160, 12)
        .iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect())
        .collect();
    assert!(
        lines.iter().all(|line| !line.contains("api_key")),
        "{lines:?}"
    );
    assert!(lines.iter().any(|line| line.contains("z1")), "{lines:?}");
}

#[test]
fn page_keys_jump_over_count_rows() {
    // One count row per group after its heading, before its models (`docs/tui.md:661-668`).
    let mut app = choosing_left_out(160, 48);
    let catalogue = left_out_catalogue();
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Stale));
    app.on_models(Ok(catalogue));
    // Four focusable stops under two count rows: a full page holds
    // four, so PageDown moves by three stops and PageUp moves back.
    let view = app.model_picker_view().expect("open");
    assert_eq!(page_step(&view, 48), 4);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::PageDown, now), Effect::None);
    let view = app.model_picker_view().expect("open");
    let z2 = view
        .sections
        .iter()
        .flat_map(|section| section.models.iter())
        .find(|model| model.id == "z2")
        .expect("z2 is shown")
        .at;
    assert_eq!(view.focused, Some(z2));
    assert_eq!(app.on_key(Key::PageUp, now), Effect::None);
    let view = app.model_picker_view().expect("open");
    let m1 = view
        .sections
        .iter()
        .flat_map(|section| section.models.iter())
        .find(|model| model.id == "m1")
        .expect("m1 is shown")
        .at;
    assert_eq!(view.focused, Some(m1));
}

#[test]
fn a_first_count_row_needs_two_body_rows() {
    // One count row per group after its heading, before its models (`docs/tui.md:661-668`).
    let catalogue = Catalogue {
        left_out: vec![LeftOutGroup {
            provider: "opencode".to_owned(),
            extension: "github.com/aakshintala/fiber/providers/opencode".to_owned(),
            file: "config/opencode.json".to_owned(),
            setting: "workspace".to_owned(),
            login: true,
            models: vec![LeftOutModel {
                id: "o1".to_owned(),
                name: None,
            }],
        }],
        ..Catalogue::default()
    };
    let mut app = attached(80, 10);
    app.on_models(Ok(catalogue.clone()));
    open(&mut app);
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Stale));
    app.on_models(Ok(catalogue));
    let view = app.model_picker_view().expect("open");
    // Two body rows keep the heading and its count.
    let kept: Vec<String> = body(&view, 80, 10)
        .iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect())
        .collect();
    assert!(
        kept.iter().any(|line| line.contains("opencode")),
        "{kept:?}"
    );
    assert!(
        kept.iter()
            .any(|line| line.contains("1 model needs workspace: run `fiber login opencode`")),
        "{kept:?}"
    );
    // Fewer than two body rows omit that first count and its heading
    // together: a count never shows without its provider.
    let cut: Vec<String> = body(&view, 80, 9)
        .iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect())
        .collect();
    assert!(cut.iter().all(|line| !line.contains("opencode")), "{cut:?}");
    assert!(
        cut.iter()
            .all(|line| !line.contains("1 model needs workspace")),
        "{cut:?}"
    );
}

#[test]
fn a_first_party_file_row_names_its_config_file() {
    // One count row per group after its heading, before its models (`docs/tui.md:661-668`).
    let catalogue = Catalogue {
        models: Vec::new(),
        installable: Vec::new(),
        notices: Vec::new(),
        lists: Vec::new(),
        left_out: vec![LeftOutGroup {
            provider: "opencode".to_owned(),
            extension: "github.com/aakshintala/fiber/providers/opencode".to_owned(),
            file: "config/opencode.json".to_owned(),
            setting: "api_key".to_owned(),
            login: false,
            models: vec![LeftOutModel {
                id: "o1".to_owned(),
                name: None,
            }],
        }],
        ..Default::default()
    };
    let mut app = attached(160, 48);
    app.on_models(Ok(catalogue));
    open(&mut app);
    let text = screen(&app, 160, 48);
    assert!(
        text.contains("1 model needs api_key: set it in `config/opencode.json`"),
        "{text}"
    );
    assert!(!text.contains("github.com"), "{text}");
}

#[test]
fn a_filtered_count_row_renders_through_the_screen() {
    // The filtered count shows only the matched count (`docs/tui.md:642-644`),
    // one count row per group after its heading, before its models
    // (`docs/tui.md:661-668`).
    let mut app = choosing_left_out(160, 48);
    let now = fakes::clock::FakeClock::new().now();
    for ch in ['m', '9'] {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    let view = app.model_picker_view().expect("open");
    assert_eq!(view.sections.len(), 1);
    assert_eq!(view.sections[0].provider, "acme");
    let text = screen(&app, 160, 48);
    assert!(
        text.contains("1 model needs api_key: set it in `config/acme-ext.json`"),
        "{text}"
    );
    assert!(!text.contains("2 models need api_key"), "{text}");
    assert!(!text.contains("needs workspace"), "{text}");
}

#[test]
fn a_query_matching_no_count_row_renders_no_match_through_the_screen() {
    // No row shows when none matches (`docs/tui.md:642-644`); a query
    // matching no model, no count row and no not-installed row shows one
    // "No models match" line and no provider sections
    // (`docs/tui.md:691-693`), with the count row placed after its heading
    // (`docs/tui.md:661-668`).
    let catalogue = Catalogue {
        left_out: vec![LeftOutGroup {
            provider: "opencode".to_owned(),
            extension: "github.com/aakshintala/fiber/providers/opencode".to_owned(),
            file: "config/opencode.json".to_owned(),
            setting: "workspace".to_owned(),
            login: true,
            models: vec![LeftOutModel {
                id: "o1".to_owned(),
                name: None,
            }],
        }],
        ..Catalogue::default()
    };
    let mut app = attached(160, 48);
    app.on_models(Ok(catalogue.clone()));
    open(&mut app);
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Stale));
    app.on_models(Ok(catalogue));
    let now = fakes::clock::FakeClock::new().now();
    for ch in ['z', 'z', 'z'] {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    let view = app.model_picker_view().expect("open");
    assert!(view.no_match);
    assert!(view.sections.is_empty());
    let text = screen(&app, 160, 48);
    assert!(text.contains("No models match"), "{text}");
    assert!(!text.contains("needs workspace"), "{text}");
    assert!(!text.contains("opencode"), "{text}");
}

/// A chip is `[text]` with a non-empty spaceless middle: `[]`, open
/// brackets, spaced middles and bare text all copy as drawn
/// (`docs/tui.md`, "Selection and copy").
#[test]
fn chips_need_a_spaceless_middle() {
    assert!(super::is_chip("[high]"));
    assert!(!super::is_chip("[]"));
    assert!(!super::is_chip("[a b]"));
    assert!(!super::is_chip("high"));
    assert!(!super::is_chip("["));
    assert!(!super::is_chip("]"));
}

/// A headed model never trims with its heading, while a headed count does
/// (`docs/tui.md:576`).
#[test]
fn trim_count_only_for_headed_counts() {
    let app = choosing(160, 48);
    let view = app.model_picker_view().expect("open");
    let display = display_rows(&view, false);
    let mut headings = 0;
    for row in &display.rows {
        let is_heading = row.focus.is_none()
            && row.row.spans.first().is_some_and(|span| {
                span.content.as_ref() == "acme" || span.content.as_ref() == "zeta"
            });
        if is_heading {
            headings += 1;
            assert!(
                !row.trim_count,
                "a headed model never trims with its heading"
            );
        }
    }
    assert!(headings > 0, "the catalogue has provider headings");
    let app = choosing_left_out(160, 48);
    let view = app.model_picker_view().expect("open");
    let display = display_rows(&view, false);
    assert!(
        display.rows.iter().any(|row| row.trim_count),
        "a headed count trims"
    );
}

/// A far focus resets to a nonzero window offset (`docs/tui.md:576`).
#[test]
fn focus_window_top_is_nonzero_for_a_far_focus() {
    let mut app = choosing(160, 10);
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..40 {
        app.on_key(Key::Down, now);
    }
    let view = app.model_picker_view().expect("open");
    assert!(view.focused.is_some(), "a far focus is set");
    let top = focus_window_top(&view, 10);
    assert_ne!(top, 0, "a far focus resets past the start");
}

/// An up wheel that lands the focus exactly on the new exclusive bottom
/// keeps it offscreen and clamps to the nearest visible row, never the
/// row at the edge (`docs/tui.md:576`).
#[test]
fn wheel_up_with_focus_on_the_new_exclusive_bottom_clamps_inside() {
    let mut app = attached(160, 11);
    let mut catalogue = catalogue();
    catalogue.models = (0..20)
        .map(|at| ModelEntry {
            reference: format!("acme/m{at}"),
            provider: "acme".to_owned(),
            id: format!("m{at}"),
            levels: vec!["low".to_owned()],
            default_level: Some("low".to_owned()),
            configured: None,
            roles: Vec::new(),
            name: None,
            price: None,
        })
        .collect();
    app.on_models(Ok(catalogue));
    open(&mut app);
    let height = 11u16;
    let view = app.model_picker_view().expect("open");
    let fit = model_fit(&view, height);
    assert_eq!(fit, 3, "height 11 fits exactly three body rows");
    let display = display_rows(&view, false);
    let focusables: Vec<usize> = display
        .rows
        .iter()
        .enumerate()
        .filter_map(|(at, row)| row.focus.map(|_| at))
        .collect();
    // Focus a row with room above it, then park the window far below so
    // the focus sits above: one up step lands it exactly on the new end.
    let target = focusables[2];
    assert!(target >= 3, "the focused row has room above it");
    let mut view = app.model_picker_view().expect("open");
    view.focused = display.rows[target].focus;
    view.top = display.rows.len();
    let effective = keep_focus_visible_top(&view, height);
    assert_eq!(effective, target, "an above focus pulls the window to it");
    let (top, focus) = picker_wheel_window(&view, true, height);
    assert_eq!(top, target.saturating_sub(3), "one step up moves three");
    let end = top.saturating_add(fit);
    let current = display
        .rows
        .iter()
        .position(|row| row.focus == view.focused);
    assert_eq!(current, Some(end), "the focus sits exactly on the new end");
    let nearest = focus.expect("an offscreen focus clamps to a visible row");
    assert_ne!(
        nearest,
        view.focused.expect("focused"),
        "the edge row stays outside"
    );
    let nearest_row = display
        .rows
        .iter()
        .position(|row| row.focus == Some(nearest))
        .expect("nearest is displayed");
    assert!(
        nearest_row >= top && nearest_row < end,
        "the clamp lands inside the new window"
    );
    // A focus strictly below the new end also clamps inside, never to the
    // row past the edge.
    let mut below = app.model_picker_view().expect("open");
    let last = *focusables.last().expect("a last focusable row");
    below.focused = display.rows[last].focus;
    below.top = 0;
    let _effective = keep_focus_visible_top(&below, height);
    let (top, focus) = picker_wheel_window(&below, true, height);
    let end = top.saturating_add(fit);
    let current = display
        .rows
        .iter()
        .position(|row| row.focus == below.focused);
    assert!(
        current.is_some_and(|row| row > end),
        "the focus sits strictly below the new end"
    );
    let nearest = focus.expect("a below focus clamps to a visible row");
    let nearest_row = display
        .rows
        .iter()
        .position(|row| row.focus == Some(nearest))
        .expect("nearest is displayed");
    assert!(
        nearest_row >= top && nearest_row < end,
        "the below clamp lands inside the new window"
    );
}

/// An up wheel that resets the window to the start while the focus sits
/// just past the new end clamps to the nearest visible row: row zero can
/// never stand in for the focus, since only an exact focus match counts
/// (`docs/tui.md:576`).
#[test]
fn wheel_up_resetting_to_zero_clamps_past_a_nonmatching_first_row() {
    let mut app = attached(160, 11);
    let mut catalogue = catalogue();
    catalogue.models = (0..20)
        .map(|at| ModelEntry {
            reference: format!("acme/m{at}"),
            provider: "acme".to_owned(),
            id: format!("m{at}"),
            levels: vec!["low".to_owned()],
            default_level: Some("low".to_owned()),
            configured: None,
            roles: Vec::new(),
            name: None,
            price: None,
        })
        .collect();
    app.on_models(Ok(catalogue));
    open(&mut app);
    let height = 11u16;
    let view = app.model_picker_view().expect("open");
    let fit = model_fit(&view, height);
    assert_eq!(fit, 3, "height 11 fits exactly three body rows");
    let display = display_rows(&view, false);
    let focusables: Vec<usize> = display
        .rows
        .iter()
        .enumerate()
        .filter_map(|(at, row)| row.focus.map(|_| at))
        .collect();
    // Focus display row 5 with the stored window at 3: the focus-derived
    // start stays 3, so one up step resets the window to zero with the
    // focus just past its end. Rows 0 and 2 are blanks no model focuses.
    let target = focusables[2];
    assert_eq!(target, 5, "the third focusable row sits at display row 5");
    let mut view = app.model_picker_view().expect("open");
    view.focused = display.rows[target].focus;
    view.top = 3;
    let effective = keep_focus_visible_top(&view, height);
    assert_eq!(effective, 3, "the stored window already shows the focus");
    let (top, focus) = picker_wheel_window(&view, true, height);
    assert_eq!(top, 0, "one step up resets the window to zero");
    let end = top.saturating_add(fit);
    // Row zero never matches the focus, so it must not count as found:
    // the clamp is the nearest visible focusable row, row 1.
    assert_eq!(
        focus, display.rows[1].focus,
        "the clamp passes row zero for the nearest visible row"
    );
    let nearest_row = display
        .rows
        .iter()
        .position(|row| row.focus == focus)
        .expect("nearest is displayed");
    assert!(
        nearest_row >= top && nearest_row < end,
        "the clamp lands inside the new window"
    );
}
