//! Tests for dragging the rail's and the panel's edges: starting,
//! moving, hiding below the floor, the grip, and the queued saves
//! (`docs/tui.md`, "Layout", "Shedding").

use std::path::PathBuf;

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::super::App;
use crate::home::Launch;
use crate::keys::{Button, Key, Mouse, MouseKind};
use crate::layout::Edge;
use crate::link::Line;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// The launch description: `/w`, outside git, default shares.
fn launch() -> Launch {
    Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: CARDS.map(str::to_owned).to_vec(),
        ..Default::default()
    }
}

/// An app with home state, attached, at `width` by `height`.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app
}

/// An app with home state, attached, with two live sessions.
fn two(width: u16, height: u16) -> App {
    let mut app = attached(width, height);
    app.on_line(status(SESSION, "one"));
    app.on_line(status(OTHER, "two"));
    app
}

/// A live idle `session_status` for `session` named `name`.
fn status(session: &str, name: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": name, "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// A `turn_started` with one message.
fn turn_started(text: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": text}]}]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// A left press at `col`, `row`.
fn press(col: u16, row: u16) -> Mouse {
    Mouse {
        kind: MouseKind::Press(Button::Left),
        col,
        row,
    }
}

/// A left drag to `col`, `row`.
fn drag(col: u16, row: u16) -> Mouse {
    Mouse {
        kind: MouseKind::Drag(Button::Left),
        col,
        row,
    }
}

/// The release.
fn release() -> Mouse {
    Mouse {
        kind: MouseKind::Release,
        col: 0,
        row: 0,
    }
}

/// The app drawn at its size: the screen and the targets.
fn draw(app: &App) -> (Buffer, Vec<crate::mouse::Target>) {
    let area = Rect::new(0, 0, app.screen.width(), app.screen.height());
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    (buf, targets)
}

/// Hides the rail, as ⌥R does.
fn hide_rail(app: &mut App) {
    app.on_key(Key::AltR, fakes::clock::FakeClock::new().now());
}

/// The launch shares: the rail's and the panel's.
fn shares(app: &App) -> (f64, f64) {
    let home = app.home.as_ref().expect("home");
    (home.launch.rail_share, home.launch.panel_share)
}

#[test]
fn a_press_on_the_rail_edge_starts_a_drag() {
    let mut app = two(200, 40);
    assert_eq!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.rail)
            .map(|rail| rail.width),
        Some(30)
    );
    app.on_drag(&press(29, 20));
    assert_eq!(app.dragging(), Some((Edge::Rail, 15.0)));
}

#[test]
fn a_press_on_the_panel_edge_starts_a_drag() {
    let mut app = two(200, 40);
    app.on_drag(&press(158, 20));
    assert_eq!(app.dragging(), Some((Edge::Panel, 21.0)));
}

#[test]
fn a_press_elsewhere_starts_none() {
    let mut app = two(200, 40);
    for col in [28, 30, 157, 159, 100] {
        app.on_drag(&press(col, 20));
        assert_eq!(app.dragging(), None, "column {col}");
    }
}

#[test]
fn a_right_press_on_an_edge_starts_none() {
    let mut app = two(200, 40);
    for col in [29, 158] {
        app.on_drag(&Mouse {
            kind: MouseKind::Press(Button::Right),
            col,
            row: 20,
        });
        assert_eq!(app.dragging(), None, "column {col}");
    }
}

#[test]
fn a_drag_report_without_a_press_does_nothing() {
    let mut app = two(200, 40);
    app.on_drag(&drag(30, 20));
    assert_eq!(app.dragging(), None);
    assert_eq!(shares(&app), (15.0, 21.0));
}

#[test]
fn dragging_follows_the_pointer_as_a_share() {
    let mut app = two(200, 40);
    app.on_drag(&press(29, 20));
    app.on_drag(&drag(30, 20));
    let layout = app.chrome().layout().expect("a layout");
    assert_eq!(layout.rail.map(|rail| rail.width), Some(31));
    assert_eq!(app.dragging(), Some((Edge::Rail, 15.5)));
}

#[test]
fn the_rail_stops_at_its_ceiling_and_where_the_conversation_keeps_84() {
    let mut app = two(400, 40);
    app.on_drag(&press(47, 20));
    app.on_drag(&drag(100, 20));
    assert_eq!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.rail)
            .map(|rail| rail.width),
        Some(48)
    );
    let mut app = two(160, 40);
    assert_eq!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.rail)
            .map(|rail| rail.width),
        Some(24)
    );
    app.on_drag(&press(23, 20));
    app.on_drag(&drag(100, 20));
    assert_eq!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.rail)
            .map(|rail| rail.width),
        Some(42)
    );
}

#[test]
fn the_panel_stops_at_its_floor_and_ceiling() {
    let mut app = two(160, 40);
    app.on_drag(&press(126, 20));
    app.on_drag(&drag(140, 20));
    assert_eq!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.panel)
            .map(|panel| panel.width),
        Some(30)
    );
    let mut app = two(400, 40);
    let edge = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .map(|panel| panel.x)
        .expect("a panel");
    assert_eq!(edge, 340);
    app.on_drag(&press(340, 20));
    app.on_drag(&drag(200, 20));
    assert_eq!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.panel)
            .map(|panel| panel.width),
        Some(60)
    );
}

#[test]
fn a_rail_dragged_below_22_hides_on_release_and_keeps_its_share() {
    let mut app = two(200, 40);
    app.on_drag(&press(29, 20));
    app.on_drag(&drag(20, 20));
    // Below the floor the split keeps the share while the drag runs.
    assert_eq!(app.dragging(), Some((Edge::Rail, 10.5)));
    app.on_drag(&release());
    let layout = app.chrome().layout().expect("a layout");
    assert_eq!((layout.rail, layout.grip.is_some()), (None, true));
    assert_eq!(shares(&app), (15.0, 21.0));
    assert!(app.take_saves().is_empty());
    // ⌥R shows it again at the old share.
    app.on_key(Key::AltR, fakes::clock::FakeClock::new().now());
    assert_eq!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.rail)
            .map(|rail| rail.width),
        Some(30)
    );
    assert!(app.take_saves().is_empty());
}

#[test]
fn a_rail_dragged_to_exactly_22_stays() {
    let mut app = two(200, 40);
    app.on_drag(&press(29, 20));
    app.on_drag(&drag(21, 20));
    app.on_drag(&release());
    assert_eq!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.rail)
            .map(|rail| rail.width),
        Some(22)
    );
    assert_eq!(app.take_saves(), vec![("tui.rail.width", 11.0)]);
}

#[test]
fn dragging_the_grip_out_shows_the_rail() {
    let mut app = two(200, 40);
    hide_rail(&mut app);
    assert_eq!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.grip)
            .map(|grip| grip.width),
        Some(1)
    );
    app.on_drag(&press(0, 20));
    // A grip below the floor draws no pill.
    assert_eq!(app.dragging(), None);
    app.on_drag(&drag(20, 20));
    assert_eq!(app.chrome().layout().and_then(|layout| layout.rail), None);
    app.on_drag(&drag(21, 20));
    assert_eq!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.rail)
            .map(|rail| rail.width),
        Some(22)
    );
    assert_eq!(app.dragging(), Some((Edge::Rail, 11.0)));
    // It follows as the rail edge from here.
    app.on_drag(&drag(30, 20));
    assert_eq!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.rail)
            .map(|rail| rail.width),
        Some(31)
    );
    app.on_drag(&release());
    assert_eq!(app.take_saves(), vec![("tui.rail.width", 15.5)]);
}

#[test]
fn dragging_the_grip_out_where_both_do_not_fit_hides_the_panel() {
    let mut app = two(130, 40);
    let layout = app.chrome().layout().expect("a layout");
    assert_eq!((layout.rail, layout.panel.is_some()), (None, true));
    app.on_drag(&press(0, 20));
    app.on_drag(&drag(21, 20));
    // Showing the rail hides the panel, as ⌥P does.
    assert_eq!(app.chrome().layout().and_then(|layout| layout.panel), None);
    // ⌥P brings the panel back and the rail sheds.
    app.on_key(Key::AltP, fakes::clock::FakeClock::new().now());
    let layout = app.chrome().layout().expect("a layout");
    assert_eq!((layout.rail, layout.panel.is_some()), (None, true));
    assert!(layout.grip.is_some());
}

#[test]
fn the_grip_cannot_show_a_rail_with_no_room() {
    let mut app = two(105, 40);
    let layout = app.chrome().layout().expect("a layout");
    assert!(layout.narrow);
    app.on_drag(&press(0, 20));
    app.on_drag(&drag(40, 20));
    let layout = app.chrome().layout().expect("a layout");
    assert_eq!((layout.rail, layout.panel), (None, None));
    assert_eq!(shares(&app), (15.0, 21.0));
    assert!(app.take_saves().is_empty());
}

#[test]
fn releasing_the_grip_short_of_the_floor_changes_nothing() {
    let mut app = two(200, 40);
    hide_rail(&mut app);
    app.on_drag(&press(0, 20));
    app.on_drag(&drag(20, 20));
    app.on_drag(&release());
    let layout = app.chrome().layout().expect("a layout");
    assert_eq!((layout.rail, layout.grip.is_some()), (None, true));
    assert_eq!(shares(&app), (15.0, 21.0));
    assert!(app.take_saves().is_empty());
}

#[test]
fn a_press_after_a_lost_release_starts_afresh() {
    let mut app = two(200, 40);
    app.on_drag(&press(29, 20));
    assert_eq!(app.dragging(), Some((Edge::Rail, 15.0)));
    app.on_drag(&press(158, 20));
    assert_eq!(app.dragging(), Some((Edge::Panel, 21.0)));
    assert!(app.take_saves().is_empty());
}

#[test]
fn no_drag_on_home_or_below_the_floor() {
    let mut home = App::new(PathBuf::from("/w"));
    home.set_home(launch());
    home.set_size(200, 40);
    home.on_drag(&press(5, 5));
    assert_eq!(home.dragging(), None);
    let mut small = two(39, 24);
    assert_eq!(small.chrome().layout(), None);
    small.on_drag(&press(5, 5));
    assert_eq!(small.dragging(), None);
}

#[test]
fn a_drag_rewraps_the_conversation() {
    let long: String = std::iter::repeat_n('w', 400).collect();
    let mut app = two(200, 40);
    app.on_line(turn_started(&long));
    let before = app.scroll().1;
    app.on_drag(&press(158, 20));
    app.on_drag(&drag(140, 20));
    assert_ne!(app.scroll().1, before);
}

#[test]
fn a_press_on_an_edge_selects_nothing() {
    let long: String = std::iter::repeat_n('w', 400).collect();
    let mut app = two(200, 40);
    app.on_line(turn_started(&long));
    let (_, targets) = draw(&app);
    let area = app.conversation_area();
    // A press and drag inside the conversation selects.
    let inside = (area.x.saturating_add(5), area.bottom().saturating_sub(1));
    app.on_select(&press(inside.0, inside.1), &targets);
    app.on_select(&drag(inside.0.saturating_add(5), inside.1), &targets);
    assert!(!app.selection_cells(area).is_empty());
    app.on_select(&release(), &targets);
    // The same reports on each edge select nothing and copy nothing.
    for col in [29, 158] {
        let (_, targets) = draw(&app);
        app.on_select(&press(col, 20), &targets);
        app.on_select(&drag(col.saturating_add(1), 20), &targets);
        assert!(app.selection_cells(area).is_empty(), "column {col}");
        app.on_select(&release(), &targets);
        assert!(app.take_copy().is_none(), "column {col}");
    }
}

#[test]
fn release_queues_one_save_rounded_to_a_tenth() {
    let mut app = two(200, 40);
    app.on_drag(&press(29, 20));
    app.on_drag(&drag(30, 20));
    app.on_drag(&release());
    assert_eq!(app.take_saves(), vec![("tui.rail.width", 15.5)]);
    assert!(app.take_saves().is_empty());
}

#[test]
fn a_panel_drag_saves_tui_panel_width() {
    let mut app = two(200, 40);
    app.on_drag(&press(158, 20));
    app.on_drag(&drag(150, 20));
    app.on_drag(&release());
    assert_eq!(app.take_saves(), vec![("tui.panel.width", 25.0)]);
}

#[test]
fn a_press_and_release_without_moving_saves_nothing() {
    let mut app = two(200, 40);
    app.on_drag(&press(29, 20));
    app.on_drag(&release());
    assert!(app.take_saves().is_empty());
    // A release with no drag queues nothing either.
    app.on_drag(&release());
    assert!(app.take_saves().is_empty());
}

#[test]
fn two_drags_queue_two_saves_in_order() {
    let mut app = two(200, 40);
    app.on_drag(&press(29, 20));
    app.on_drag(&drag(30, 20));
    app.on_drag(&release());
    app.on_drag(&press(158, 20));
    app.on_drag(&drag(150, 20));
    app.on_drag(&release());
    assert_eq!(
        app.take_saves(),
        vec![("tui.rail.width", 15.5), ("tui.panel.width", 25.0)]
    );
}

#[test]
fn over_edge_on_an_edge_and_while_dragging() {
    let mut app = two(200, 40);
    assert!(app.over_edge(Some((29, 20))));
    assert!(app.over_edge(Some((158, 20))));
    assert!(!app.over_edge(Some((28, 20))));
    assert!(!app.over_edge(Some((100, 20))));
    assert!(!app.over_edge(None));
    // A drag running with the pointer elsewhere keeps the arrow.
    app.on_drag(&press(29, 20));
    assert!(app.over_edge(Some((100, 20))));
    assert!(app.over_edge(None));
    app.on_drag(&release());
    assert!(!app.over_edge(Some((100, 20))));
}

#[test]
fn over_edge_without_a_layout_is_false() {
    let mut home = App::new(PathBuf::from("/w"));
    home.set_home(launch());
    home.set_size(200, 40);
    assert!(!home.over_edge(Some((0, 0))));
}
