//! Tests for the narrow layout's rows, rendered through the view: the
//! status line's order and cuts, the widget row and the shed order
//! (`docs/tui.md`, "The narrow layout").

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::App;
use crate::app::panel::Spot;
use crate::home::Launch;
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::TargetId;
use contract::clock::Clock;
const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";

/// The launch description with `cards`, `/w`, outside git.
fn launch(cards: &[&str]) -> Launch {
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
        panel_cards: cards.iter().map(|card| (*card).to_owned()).collect(),
        ..Default::default()
    }
}

/// The default cards.
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// An app with home state, attached, at `width` by `height`.
fn attached(width: u16, height: u16) -> App {
    attached_with(width, height, &CARDS)
}

/// An app with home state, attached, with `cards`.
fn attached_with(width: u16, height: u16, cards: &[&str]) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch(cards));
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app
}

/// One envelope of `session`.
fn session_line(session: &str, kind: &str, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A live idle `session_status` for `session` named `name`.
fn idle(session: &str, name: &str) -> Line {
    session_line(
        session,
        "session_status",
        serde_json::json!({
            "name": name, "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 1.5, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    )
}

/// A waiting `session_status` for `session`.
fn waiting(session: &str) -> Line {
    session_line(
        session,
        "session_status",
        serde_json::json!({
            "name": "other", "workspace": "/w", "project": "-w",
            "state": "waiting", "since": 0,
            "waiting": {"request_id": "r_1", "kind": "approval",
                "summary": "Run cargo test"},
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    )
}

/// A `session_status` with `context` tokens of `window`.
fn context_status(tokens: u64, window: u64) -> Line {
    session_line(
        SESSION,
        "session_status",
        serde_json::json!({
            "name": "one", "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
            "context": {"tokens": tokens, "window": window},
        }),
    )
}

/// A `preamble_built` naming `model` with `window`.
fn preamble(model: &str, window: u64) -> Line {
    session_line(
        SESSION,
        "preamble_built",
        serde_json::json!({
            "reason": "start", "model": model, "context_window": window,
            "trigger_at": null, "thinking": null,
            "tool_choice": "auto", "cache_lifetime": "5m",
            "system_prompt": "", "tools": [],
        }),
    )
}

/// A `tool_call_completed` with `changes`.
fn changed_call(changes: serde_json::Value) -> Line {
    session_line(
        SESSION,
        "tool_call_completed",
        serde_json::json!({"status": "completed", "content": [], "changes": changes}),
    )
}

/// A `job_started` of `description`.
fn started(id: &str, description: &str) -> Line {
    session_line(
        SESSION,
        "job_started",
        serde_json::json!({"job_id": id, "description": description,
            "output_path": "/tmp/out"}),
    )
}

/// A `delegate_started` for `job`.
fn delegated(job: &str) -> Line {
    session_line(
        SESSION,
        "delegate_started",
        serde_json::json!({"job_id": job,
            "delegate_session_id": "s_cccccccccccccccc",
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    )
}

/// An `extension_ui` widget's lines.
fn widget(extension: &str, name: &str, lines: &[&str]) -> Line {
    session_line(
        SESSION,
        "extension_ui",
        serde_json::json!({"extension": extension, "widget": name, "lines": lines}),
    )
}

/// Draws `app` at `width` by `height`: the screen's text and the targets.
fn draw(app: &App, width: u16, height: u16) -> (String, Vec<crate::mouse::Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    (crate::view::text(&buf), targets)
}

#[test]
fn row_one_follows_the_card_order_and_is_cut_at_the_edge() {
    let mut app = attached_with(100, 30, &["changed_files", "session"]);
    app.on_line(changed_call(serde_json::json!([
        {"path": "src/a.rs", "added": 10, "removed": 2},
        {"path": "src/b.rs", "added": 3, "removed": 3},
    ])));
    app.on_line(idle(SESSION, "one"));
    app.on_line(preamble(
        "a-model-with-a-very-long-name-that-runs-past-the-columns-edge-and-keeps-going-and-going",
        100,
    ));
    let (screen, _) = draw(&app, 100, 30);
    let row: &str = screen.lines().next_back().unwrap_or_default();
    assert!(row.starts_with("2 files +13 \u{2212}5 · "), "{row}");
    assert!(!row.contains("and-going"), "{row}");
    assert!(crate::format::width(row) <= 100, "{row}");
}

#[test]
fn row_one_shows_the_context_share_and_a_zero_window_shows_none() {
    let mut app = attached(100, 30);
    app.on_line(context_status(50, 100));
    app.on_line(preamble("test/model", 100));
    let (screen, _) = draw(&app, 100, 30);
    assert!(screen.contains("50% context"), "{screen}");
    let mut app = attached(100, 30);
    app.on_line(context_status(50, 0));
    app.on_line(preamble("test/model", 0));
    let (screen, _) = draw(&app, 100, 30);
    assert!(!screen.contains("% context"), "{screen}");
}

#[test]
fn row_two_opens_with_delegates_and_jobs() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(started("j_1", "build"));
    app.on_line(started("j_2", "test"));
    app.on_line(started("j_3", "lint"));
    app.on_line(delegated("j_2"));
    app.on_line(delegated("j_3"));
    let (screen, _) = draw(&app, 100, 30);
    assert!(
        screen.contains("2 delegates running · 1 job running"),
        "{screen}"
    );
}

#[test]
fn counts_at_zero_are_left_out() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    let (screen, _) = draw(&app, 100, 30);
    assert!(!screen.contains("running"), "{screen}");
}

#[test]
fn an_unlisted_card_gives_no_count() {
    let mut app = attached_with(100, 30, &["session"]);
    app.on_line(idle(SESSION, "one"));
    app.on_line(started("j_1", "build"));
    app.on_line(delegated("j_1"));
    let (screen, _) = draw(&app, 100, 30);
    assert!(!screen.contains("running"), "{screen}");
}

#[test]
fn an_empty_row_two_takes_no_row() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    let (screen, _) = draw(&app, 100, 30);
    let rows: Vec<&str> = screen.lines().collect();
    let bottom = rows.len() - 1;
    assert!(rows[bottom].contains("$1.50"), "{}", screen);
    assert_eq!(rows[bottom - 1], ">", "{}", screen);
}

#[test]
fn n_waiting_leads_row_one_while_the_rail_is_not_drawn() {
    let mut app = attached(110, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(waiting(OTHER));
    app.on_key(Key::AltR, fakes::clock::FakeClock::new().now());
    let (screen, targets) = draw(&app, 110, 30);
    // The grip leaves column 0 blank, so the row starts with a space.
    let row: &str = screen.lines().next_back().unwrap_or_default().trim_start();
    assert!(row.starts_with("1 waiting · "), "{row}");
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::Panel(Spot::Waiting)),
        "{screen}"
    );
    // While the rail is drawn there is no waiting segment.
    app.on_key(Key::AltR, fakes::clock::FakeClock::new().now());
    let (screen, targets) = draw(&app, 110, 30);
    assert!(!screen.contains("waiting"), "{screen}");
    assert!(
        targets
            .iter()
            .all(|target| target.id != TargetId::Panel(Spot::Waiting)),
        "{screen}"
    );
}

#[test]
fn the_waiting_target_covers_only_its_segment() {
    let mut app = attached(110, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(waiting(OTHER));
    app.on_key(Key::AltR, fakes::clock::FakeClock::new().now());
    let (_, targets) = draw(&app, 110, 30);
    let waiting: Vec<Rect> = targets
        .iter()
        .filter(|target| target.id == TargetId::Panel(Spot::Waiting))
        .map(|target| target.rect)
        .collect();
    assert_eq!(waiting.len(), 1);
    // The rail's grip takes column 0, so the row starts in column 1.
    let column = app.chrome().layout().expect("a layout").column;
    assert_eq!(
        waiting[0],
        Rect::new(
            column.x,
            29,
            u16::try_from("1 waiting".len()).unwrap_or(u16::MAX),
            1
        )
    );
}

#[test]
fn the_widget_row_shows_the_first_widget_in_list_order() {
    let mut app = attached_with(100, 30, &["session", "other/list"]);
    app.on_line(idle(SESSION, "one"));
    app.on_line(widget("plan", "tasks", &["first arrived"]));
    app.on_line(widget("other", "list", &["listed first"]));
    let (screen, _) = draw(&app, 100, 30);
    assert!(screen.contains("▸ listed first"), "{screen}");
    assert!(!screen.contains("▸ first arrived"), "{screen}");
}

#[test]
fn no_widget_no_widget_row() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    let (screen, _) = draw(&app, 100, 30);
    assert!(!screen.contains('▸'), "{screen}");
    assert!(!screen.contains('▾'), "{screen}");
}

#[test]
fn short_screens_shed_row_two_then_row_one() {
    // A five-line widget, open, with both status rows wanted.
    let setup = |height: u16| {
        let mut app = attached(100, height);
        app.on_line(idle(SESSION, "one"));
        app.on_line(started("j_1", "build"));
        app.on_line(widget("ex", "wid", &["l1", "l2", "l3", "l4", "l5"]));
        app.on_click(TargetId::Panel(Spot::Widget));
        app
    };
    // Five widget rows leave no status row: 10 − (2 + 5 + 0) = 3.
    let (screen, _) = draw(&setup(10), 100, 10);
    assert!(!screen.contains("job running"), "{screen}");
    assert!(!screen.contains("$1.50"), "{screen}");
    assert!(screen.contains("l5"), "{screen}");
    // Four widget rows keep row 1 alone.
    let mut app = setup(10);
    app.on_line(widget("ex", "wid", &["l1", "l2", "l3", "l4"]));
    let (screen, _) = draw(&app, 100, 10);
    assert!(!screen.contains("job running"), "{screen}");
    assert!(screen.contains("$1.50"), "{screen}");
}

#[test]
fn narrow_100x30() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(preamble("test/model", 100));
    app.on_line(session_line(
        SESSION,
        "session_status",
        serde_json::json!({
            "name": "one", "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 1.5, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
            "context": {"tokens": 50, "window": 100},
        }),
    ));
    app.on_line(changed_call(serde_json::json!([
        {"path": "src/a.rs", "added": 10, "removed": 2},
        {"path": "src/b.rs", "added": 3, "removed": 3},
    ])));
    app.on_line(started("j_1", "build"));
    app.on_line(widget("ex", "wid", &["abc", "def"]));
    let (screen, _) = draw(&app, 100, 30);
    insta::assert_snapshot!("narrow_100x30", screen);
}

#[test]
fn narrow_with_widget_open() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(widget("ex", "wid", &["abc", "def"]));
    app.on_click(TargetId::Panel(Spot::Widget));
    let (screen, _) = draw(&app, 100, 30);
    insta::assert_snapshot!("narrow_with_widget_open", screen);
}

#[test]
fn narrow_short() {
    let mut app = attached(100, 10);
    app.on_line(idle(SESSION, "one"));
    app.on_line(started("j_1", "build"));
    let (screen, _) = draw(&app, 100, 10);
    insta::assert_snapshot!("narrow_short", screen);
}
