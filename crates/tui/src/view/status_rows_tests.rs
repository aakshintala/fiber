//! Tests for the narrow layout's rows, rendered through the view: the
//! status line's order and cuts, the widget row and the shed order
//! (`docs/tui.md`, "The narrow layout").

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line as TextLine;

use crate::app::App;
use crate::app::panel::Spot;
use crate::home::Launch;
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::TargetId;
use contract::clock::Clock;
const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";

#[test]
fn quota_segments_use_short_names_colours_and_open_the_view() {
    use crate::app::items::testkit::{commands, hello};
    use serde_json::json;
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch(&["quota"]));
    app.on_line(hello());
    app.attach(contract::SessionId(SESSION.to_owned()));
    let out = commands(app.on_line(session_line(SESSION, "clients", json!({"clients": []}))));
    let id = out.iter().find(|v| v["command"] == "quota").unwrap()["id"]
        .as_str()
        .unwrap();
    app.on_line(session_line(SESSION, "command_accepted", json!({"command_id": id,
        "result": {"quota": [
            {"provider": "codex", "credential": "work", "windows": [{"name": "weekly", "percent_used": 80.0}]},
            {"provider": "openrouter", "credential": "work", "credit": {"remaining": 4.2}}
        ], "notice_at": 80.0}})));
    let shown = super::status(&app, 100);
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].line.to_string(), "Q codex 20% · openrouter 4.20");
    assert_eq!(
        shown[0].line.spans[0].style,
        crate::theme::style(crate::theme::Role::Muted)
    );
    let codex = shown[0]
        .line
        .spans
        .iter()
        .find(|s| s.content.contains("codex"))
        .unwrap();
    assert_eq!(codex.style, crate::theme::style(crate::theme::Role::Error));
    // The whole segment opens the quota view, which fetches every label.
    assert_eq!(shown[0].spots, [(0, 29, Spot::Quota)]);
    app.attach(contract::SessionId(SESSION.to_owned()));
    assert!(super::status(&app, 100).is_empty());
}

/// The launch description with `cards`, `/w`, outside git.
fn launch(cards: &[&str]) -> Launch {
    Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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
            "started": 0, "calls": 0, "since": 0,
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
            "state": "waiting", "started": 0, "calls": 0, "since": 0,
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
    context_status_for(SESSION, tokens, window)
}

/// A `session_status` with context for `session`.
fn context_status_for(session: &str, tokens: u64, window: u64) -> Line {
    session_line(
        session,
        "session_status",
        serde_json::json!({
            "name": "one", "workspace": "/w", "project": "-w", "state": "idle",
            "started": 0, "calls": 0, "since": 0,
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
    preamble_for(SESSION, model, window)
}

/// A `preamble_built` for `session` naming `model` and `window`.
fn preamble_for(session: &str, model: &str, window: u64) -> Line {
    session_line(
        session,
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
    // The draw lays the row one column in, past the grip; the slab
    // floor is last, so row one sits one up, carrying the box's `▌`
    // stripe ("The input box"). Cards keep their order, minus the
    // segments that moved to the status row.
    let row: &str = screen
        .lines()
        .rev()
        .nth(1)
        .unwrap_or_default()
        .trim_start()
        .trim_start_matches('\u{258c}');
    assert!(
        row.starts_with("\u{00b1} 2 files +13 \u{2212}5  \u{2502}  0 waiting  \u{2502}  $1.50  \u{2502}  cache 0%"),
        "{row}"
    );
    assert!(!row.contains("and-going"), "{row}");
    assert!(crate::format::width(row.trim_start()) <= 99, "{row}");
}

#[test]
fn row_one_shows_the_context_share_and_a_zero_window_shows_none() {
    let mut app = attached(100, 30);
    app.on_line(context_status(50, 100));
    app.on_line(preamble("test/model", 100));
    let (screen, _) = draw(&app, 100, 30);
    // Short context form: dim `ctx`, the 8-cell bar and the size.
    assert!(
        screen.contains("ctx \u{2586}\u{2586}\u{2586}\u{2586}\u{2586}\u{2586}\u{2586}\u{2586} 50"),
        "{screen}"
    );
    assert!(!screen.contains("% context"), "{screen}");
    let mut app = attached(100, 30);
    app.on_line(context_status(50, 0));
    app.on_line(preamble("test/model", 0));
    let (screen, _) = draw(&app, 100, 30);
    // With a zero window the input box's status row ("The input box")
    // keeps the `ctx` segment with its bar, size and share blank.
    assert!(
        screen.contains("ctx \u{2586}\u{2586}\u{2586}\u{2586}\u{2586}\u{2586}\u{2586}\u{2586} \u{2014} \u{00b7} \u{2014}"),
        "{screen}"
    );
    assert!(
        !screen
            .lines()
            .rev()
            .nth(1)
            .unwrap_or_default()
            .contains("ctx"),
        "{screen}"
    );
}

/// The input box's status row as text: the context bar lives there
/// (`docs/tui.md`, "The input box"), found through its status target
/// so the panel's cards cannot leak into the per-session asserts.
fn box_row(app: &App) -> String {
    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    let y = targets
        .iter()
        .filter(|target| matches!(target.id, TargetId::Panel(Spot::Status)))
        .map(|target| target.rect.y)
        .max()
        .expect("a status row");
    (0..100).map(|x| buf[(x, y)].symbol()).collect()
}

#[test]
fn status_row_context_comes_from_the_attached_session_only() {
    let mut app = attached(100, 30);
    app.on_line(context_status(50, 100));
    assert!(box_row(&app).contains("50%"), "{}", box_row(&app));

    // The box row always carries the ctx segment: unknown figures show
    // as dashes, so per-session sourcing asserts the figures, not the
    // label. SESSION's half-window share is 50%.
    app.attach(contract::SessionId(OTHER.to_owned()));
    app.on_line(preamble_for(OTHER, "test/model", 200));
    let unknown = box_row(&app);
    assert!(unknown.contains("—"), "{unknown:?}");
    assert!(!unknown.contains("50"), "{unknown:?}");

    app.on_line(context_status_for(OTHER, 12, 200));
    let refreshed = box_row(&app);
    assert!(refreshed.contains("6%"), "{refreshed:?}");
    assert!(!refreshed.contains("50"), "{refreshed:?}");
}

#[test]
fn the_context_status_segment_has_a_context_target() {
    let mut app = attached(100, 30);
    app.on_line(context_status(50, 100));
    app.on_line(preamble("test/model", 100));
    let (screen, targets) = draw(&app, 100, 30);
    let context = targets
        .iter()
        .find(|target| target.id == TargetId::Panel(Spot::Context))
        .expect("the context segment is clickable");
    let row = screen.lines().nth(usize::from(context.rect.y));
    assert!(
        row.is_some_and(|row| row.contains("ctx \u{2586}")),
        "{screen}"
    );
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
    // Each count opens its list, marked and never cut.
    assert!(
        screen.contains("\u{25c6} 2 delegates running  \u{2502}  \u{2699} 1 job running"),
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
    // The slab floor is last, so row one sits one up, above the status
    // row, the rule and the input row.
    assert!(rows[bottom - 1].contains("$1.50"), "{}", screen);
    assert_eq!(rows[bottom - 4], " ▌ › █", "{}", screen);
}

/// A `session_status` with billed `cost` and `subscription` spend.
fn spend_status(cost: serde_json::Value, subscription: f64) -> Line {
    session_line(
        SESSION,
        "session_status",
        serde_json::json!({
            "name": "one", "workspace": "/w", "project": "-w", "state": "idle",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": cost, "subscription_cost": subscription},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    )
}

/// A `preamble_built` setting `budget.usd`.
fn budget_preamble(budget: f64) -> Line {
    session_line(
        SESSION,
        "preamble_built",
        serde_json::json!({
            "reason": "start", "model": "test/model", "context_window": 1000,
            "tool_choice": "auto", "cache_lifetime": "5m",
            "system_prompt": "", "tools": [], "budget": budget,
        }),
    )
}

#[test]
fn the_spend_segment_reads_billed_spend_against_the_budget() {
    let mut app = attached(100, 30);
    app.on_line(budget_preamble(5.0));
    app.on_line(spend_status(serde_json::json!(1.25), 0.0));
    let (screen, _) = draw(&app, 100, 30);
    assert!(
        screen.lines().any(|row| row.contains("$1.25 of $5.00")),
        "{screen}"
    );

    let mut app = attached(100, 30);
    app.on_line(spend_status(serde_json::json!(1.25), 0.0));
    let (screen, _) = draw(&app, 100, 30);
    assert!(screen.lines().any(|row| row.contains("$1.25")), "{screen}");
    assert!(!screen.contains("of $"), "{screen}");

    // $1 billed plus $9 on subscription against a $5 budget: the
    // subscription bills nothing per call.
    let mut app = attached(100, 30);
    app.on_line(budget_preamble(5.0));
    app.on_line(spend_status(serde_json::json!(1.0), 9.0));
    let (screen, _) = draw(&app, 100, 30);
    assert!(
        screen.lines().any(|row| row.contains("$1.00 of $5.00")),
        "{screen}"
    );
    assert!(!screen.contains("$10.00"), "{screen}");

    // Subscription-only: billed $0 against the budget.
    let mut app = attached(100, 30);
    app.on_line(budget_preamble(5.0));
    app.on_line(spend_status(serde_json::json!(0.0), 9.0));
    let (screen, _) = draw(&app, 100, 30);
    assert!(
        screen.lines().any(|row| row.contains("$0.00 of $5.00")),
        "{screen}"
    );

    // A budget is set but the billed cost is not known yet.
    let mut app = attached(100, 30);
    app.on_line(budget_preamble(5.0));
    app.on_line(spend_status(serde_json::Value::Null, 0.0));
    let (screen, _) = draw(&app, 100, 30);
    assert!(
        screen.lines().any(|row| row.contains("unknown of $5.00")),
        "{screen}"
    );

    // A budget is set but no status parsed: the segment reads the
    // attached spend plain, with no comparison.
    let mut app = attached(100, 30);
    app.on_line(budget_preamble(5.0));
    app.on_line(session_line(
        SESSION,
        "session_status",
        serde_json::json!({"bogus": true}),
    ));
    let (screen, _) = draw(&app, 100, 30);
    assert!(screen.lines().any(|row| row.contains("$0.00")), "{screen}");
    assert!(!screen.contains("of $"), "{screen}");
}

#[test]
fn n_waiting_leads_row_one_while_the_rail_is_not_drawn() {
    let mut app = attached(110, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(waiting(OTHER));
    app.on_key(Key::AltR, fakes::clock::FakeClock::new().now());
    let (screen, targets) = draw(&app, 110, 30);
    // Row one sits inside the box now, so it carries the box's `▌`
    // stripe ("The input box"); the slab floor is last, so row one
    // sits one up.
    let row: &str = screen
        .lines()
        .rev()
        .nth(1)
        .unwrap_or_default()
        .trim_start()
        .trim_start_matches('\u{258c}');
    assert!(row.starts_with("1 waiting  \u{2502}  "), "{row}");
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
    // The rail's grip takes column 0 and the draw one more, so the row
    // starts in column 1.
    let column = app.chrome().layout().expect("a layout").column;
    assert_eq!(
        waiting[0],
        Rect::new(
            column.x.saturating_add(1),
            // The slab floor takes the last row, so row one sits one up.
            28,
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
fn no_narrow_rows_drawn_outside_the_narrow_layout() {
    // Wide, with a widget, a delegate and status segments wanted: the
    // panel shows them, and no row draws under the input box.
    let mut app = attached(200, 40);
    app.on_line(idle(SESSION, "one"));
    app.on_line(started("j_1", "alpha"));
    app.on_line(delegated("j_1"));
    app.on_line(widget("ex", "wid", &["abc", "def"]));
    let area = Rect::new(0, 0, 200, 40);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let screen = crate::view::text(&buf);
    assert!(!screen.contains('▸'), "{screen}");
    assert!(!screen.contains('▾'), "{screen}");
    // The rail takes 30 columns at this width, so the input box starts
    // there. The rule and status row sit under the input row, above
    // the slab floor.
    assert_eq!(
        screen.lines().rev().nth(3),
        Some("                              ▌ › █")
    );
    // The row above the input box is blank inside the column: past the
    // drawn cursor.
    assert_eq!(
        buf.cell((5, 36))
            .map(|cell| cell.symbol().to_owned())
            .as_deref(),
        Some(" ")
    );
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
    // Four widget rows keep row 1 alone at eleven rows, allowing for the
    // input box's two surface edges.
    let mut app = setup(11);
    app.on_line(widget("ex", "wid", &["l1", "l2", "l3", "l4"]));
    let (screen, _) = draw(&app, 100, 11);
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
            "started": 0, "calls": 0, "since": 0,
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
fn narrow_with_budget() {
    let mut app = attached(100, 30);
    app.on_line(budget_preamble(5.0));
    app.on_line(spend_status(serde_json::json!(1.25), 0.0));
    let (screen, _) = draw(&app, 100, 30);
    insta::assert_snapshot!("narrow_with_budget", screen);
}

#[test]
fn narrow_short() {
    let mut app = attached(100, 10);
    app.on_line(idle(SESSION, "one"));
    app.on_line(started("j_1", "build"));
    let (screen, _) = draw(&app, 100, 10);
    insta::assert_snapshot!("narrow_short", screen);
}

#[test]
fn delegate_rows_show_only_while_the_conversation_keeps_half() {
    // One delegate wants two rows; below the input box take two.
    let setup = |height: u16| {
        let mut app = attached(100, height);
        app.on_line(idle(SESSION, "one"));
        app.on_line(started("j_1", "alpha"));
        app.on_line(delegated("j_1"));
        app
    };
    // At 11 rows the conversation would keep 5 of 11, so the rows drop.
    let (screen, _) = draw(&setup(11), 100, 11);
    assert!(!screen.contains("alpha"), "{screen}");
    assert!(screen.contains("$1.50"), "{screen}");
    assert!(screen.contains("1 delegate running"), "{screen}");
    // At 16 rows it keeps half with the input box's two surface edges.
    let (screen, _) = draw(&setup(16), 100, 16);
    assert!(screen.contains("alpha"), "{screen}");
    assert!(screen.contains("$1.50"), "{screen}");
    assert!(screen.contains("1 delegate running"), "{screen}");
}

#[test]
fn delegate_rows_stop_at_four() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    for (id, description) in [("j_1", "alpha"), ("j_2", "beta"), ("j_3", "gamma")] {
        app.on_line(started(id, description));
        app.on_line(delegated(id));
    }
    let (screen, _) = draw(&app, 100, 30);
    assert!(screen.contains("alpha"), "{screen}");
    assert!(screen.contains("beta"), "{screen}");
    assert!(!screen.contains("gamma"), "{screen}");
    assert_eq!(crate::view::status_rows::delegates(&app, 100).len(), 4);
}

#[test]
fn a_delegate_row_count_matches_the_card() {
    let mut app = attached(200, 40);
    app.on_line(idle(SESSION, "one"));
    app.on_line(started("j_1", "one"));
    app.on_line(delegated("j_1"));
    app.on_line(started("j_2", "two"));
    app.on_line(delegated("j_2"));
    // Short descriptions are cut nowhere, so the rows match exactly.
    let card: Vec<TextLine> = crate::view::panel::delegates::rows(&app, 97)
        .into_iter()
        .map(|row| row.line)
        .collect();
    let status = crate::view::status_rows::delegates(&app, 100);
    assert_eq!(status.len(), 4);
    assert_eq!(
        status.into_iter().map(|(line, _)| line).collect::<Vec<_>>(),
        card
    );
}

#[test]
fn the_narrow_delegate_rows_carry_their_serials_as_targets() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(started("j_1", "alpha"));
    app.on_line(delegated("j_1"));
    app.on_line(started("j_2", "beta"));
    app.on_line(delegated("j_2"));
    let (_, targets) = draw(&app, 100, 30);
    let mut serials: Vec<u64> = targets
        .iter()
        .filter_map(|target| {
            if let TargetId::Panel(Spot::Delegate(serial)) = target.id {
                Some(serial)
            } else {
                None
            }
        })
        .collect();
    serials.sort_unstable();
    assert_eq!(serials, vec![1, 1, 2, 2]);
}

#[test]
fn a_zero_width_delegate_row_has_no_click_target() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(started("j_1", "alpha"));
    app.on_line(delegated("j_1"));
    let area = Rect::new(0, 0, 0, 10);
    let mut buf = Buffer::empty(area);
    let mut bottom = area.bottom();
    let mut targets = Vec::new();
    super::draw_delegates(&app, area, &mut buf, &mut bottom, &mut targets);
    assert!(targets.is_empty());
}

#[test]
fn narrow_with_delegates_and_widget() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(started("j_1", "alpha"));
    app.on_line(delegated("j_1"));
    app.on_line(widget("ex", "wid", &["abc", "def"]));
    let (screen, _) = draw(&app, 100, 30);
    insta::assert_snapshot!("narrow_with_delegates_and_widget", screen);
}

/// Renders `app` at `width` by `height`: the screen's rows, the terminal
/// cursor and the click targets.
fn rendered(
    app: &App,
    width: u16,
    height: u16,
) -> (
    Vec<String>,
    Option<ratatui::layout::Position>,
    Vec<crate::mouse::Target>,
) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    let rows: Vec<String> = crate::view::text(&buf).lines().map(str::to_owned).collect();
    (rows, crate::view::cursor(app, area), targets)
}

/// An app at 100 columns wanting both status rows: row 1 from the idle
/// session, row 2 from the running job.
fn two_rows(height: u16) -> App {
    let mut app = attached(100, height);
    app.on_line(idle(SESSION, "one"));
    app.on_line(started("j_1", "build"));
    app
}

#[test]
fn the_draft_cursor_sits_above_one_status_row() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    let (rows, cursor, _) = rendered(&app, 100, 30);
    assert_eq!(app.narrow_fit().map(|fit| fit.status), Some(1));
    let at = cursor.expect("a cursor on the input row");
    // The input row, the rule, the status row, then the one status
    // line row above the slab floor.
    assert_eq!(usize::from(at.y), rows.len() - 5);
    assert!(rows[usize::from(at.y)].contains('›'), "{rows:?}");
    assert!(rows[rows.len() - 2].contains("$1.50"), "{rows:?}");
}

#[test]
fn the_draft_cursor_sits_above_two_status_rows() {
    let app = two_rows(30);
    let (rows, cursor, _) = rendered(&app, 100, 30);
    assert_eq!(app.narrow_fit().map(|fit| fit.status), Some(2));
    let at = cursor.expect("a cursor on the input row");
    // The input row, the rule, the status row, then row 1 and row 2
    // above the slab floor.
    assert_eq!(usize::from(at.y), rows.len() - 6);
    assert!(rows[usize::from(at.y)].contains('›'), "{rows:?}");
    assert!(rows[rows.len() - 3].contains("$1.50"), "{rows:?}");
    assert!(rows[rows.len() - 2].contains("1 job running"), "{rows:?}");
}

#[test]
fn the_draft_cursor_tracks_the_kept_status_rows_on_short_screens() {
    // A five-line widget, open, sheds the status rows one by one as the
    // screen shortens, while the input row stays above them.
    let setup = |height: u16| {
        let mut app = two_rows(height);
        app.on_line(widget("ex", "wid", &["l1", "l2", "l3", "l4", "l5"]));
        app.on_click(TargetId::Panel(Spot::Widget));
        app
    };
    let mut kept = std::collections::BTreeSet::new();
    for height in 10..=14 {
        let app = setup(height);
        let (rows, cursor, _) = rendered(&app, 100, height);
        let keep = app.narrow_fit().map(|fit| fit.status).unwrap_or(0);
        kept.insert(keep);
        let at = cursor.expect("a cursor on the input row");
        // The cursor stays on the input row while the rule, the status
        // row and kept status rows move beneath it, above the floor.
        assert_eq!(usize::from(at.y), usize::from(height) - keep - 4);
        assert!(rows[usize::from(at.y)].contains('›'), "{height}: {rows:?}");
        let below = &rows[usize::from(at.y) + 1..];
        assert_eq!(below.len(), keep + 3, "{height}: {rows:?}");
        assert!(
            below[0].trim_start().starts_with("\u{258c}\u{2500}"),
            "{height}: {rows:?}"
        );
        assert!(below[1].contains("/w"), "{height}: {rows:?}");
        if keep >= 1 {
            assert!(below[2].contains("$1.50"), "{height}: {rows:?}");
        }
        if keep >= 2 {
            assert!(below[3].contains("1 job running"), "{height}: {rows:?}");
        }
        assert!(
            below[below.len() - 1]
                .trim_start()
                .chars()
                .all(|cell| cell == '▀'),
            "{height}: {rows:?}"
        );
    }
    // The sweep covers each side of both shedding boundaries.
    assert!(
        kept.contains(&0) && kept.contains(&1) && kept.contains(&2),
        "{kept:?}"
    );
}

/// The waiting spots of the status rows at `width`: their offset and width.
fn waiting_spots(app: &App, width: u16) -> Vec<(u16, u16)> {
    crate::view::status_rows::status(app, width)
        .into_iter()
        .flat_map(|row| row.spots)
        .filter(|(_, _, spot)| matches!(spot, Spot::Waiting))
        .map(|(start, wide, _)| (start, wide))
        .collect()
}

#[test]
fn a_status_segment_at_the_row_width_has_no_target() {
    // Row one opens with the waiting segment past the file totals:
    // "± 1 file +10 −2  │  1 waiting  │  …".
    let mut app = attached_with(110, 30, &["changed_files", "session"]);
    app.on_line(changed_call(serde_json::json!([
        {"path": "src/a.rs", "added": 10, "removed": 2},
    ])));
    app.on_line(idle(SESSION, "one"));
    app.on_line(waiting(OTHER));
    app.on_key(Key::AltR, fakes::clock::FakeClock::new().now());
    let whole = waiting_spots(&app, 110);
    assert_eq!(whole.len(), 1, "{whole:?}");
    let (start, wide) = whole[0];
    assert_eq!((start, wide), (20, 9), "{whole:?}");
    // A segment starting exactly at the row's width has no target.
    assert!(waiting_spots(&app, start + 1).is_empty(), "start {start}");
    // A segment starting beyond the row's width has no target either.
    assert!(waiting_spots(&app, start).is_empty(), "start {start}");
    // A row ending exactly past the segment keeps its full width.
    assert_eq!(waiting_spots(&app, start + wide + 1), vec![(start, wide)]);
}

#[test]
fn the_widget_target_sits_on_the_first_line_only() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(widget("ex", "wid", &["abc", "def"]));
    app.on_click(TargetId::Panel(Spot::Widget));
    let (screen, targets) = draw(&app, 100, 30);
    let rows: Vec<&str> = screen.lines().collect();
    let first = rows
        .iter()
        .position(|row| row.contains("▾ abc"))
        .expect("the open widget's first line");
    assert!(rows[first + 1].contains("def"), "{screen}");
    let at: Vec<Rect> = targets
        .iter()
        .filter(|target| target.id == TargetId::Panel(Spot::Widget))
        .map(|target| target.rect)
        .collect();
    assert_eq!(at.len(), 1, "{screen}");
    assert_eq!(
        at[0].y,
        u16::try_from(first).unwrap_or(u16::MAX),
        "{screen}"
    );
    assert_eq!(at[0].height, 1, "{screen}");
}

/// An approval panel asking to run `ls`.
fn approval() -> Line {
    session_line(
        SESSION,
        "permission_requested",
        serde_json::json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "standing_ask", "standing_rule": {"scope": "global", "prefix": "ls"}}),
    )
}

#[test]
fn the_approval_panel_draws_above_the_status_row() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(approval());
    assert!(app.panel().is_some());
    let (rows, cursor, _) = rendered(&app, 100, 30);
    // An approval has no text cursor.
    assert!(cursor.is_none());
    // The status line shows only while the panel does not ("The narrow
    // layout"), so the panel's bottom row is the slab floor.
    assert!(!rows.iter().any(|row| row.contains("$1.50")), "{rows:?}");
    assert!(
        rows[rows.len() - 1]
            .trim_start()
            .chars()
            .all(|cell| cell == '▀'),
        "{rows:?}"
    );
    assert!(
        rows[rows.len() - 4].contains("Deny  type to add feedback"),
        "{rows:?}"
    );
}

#[test]
fn the_approval_panel_takes_the_bottom_row_once_the_status_sheds() {
    // A five-line widget, open, leaves no status row at ten rows.
    let setup = || {
        let mut app = attached(100, 10);
        app.on_line(idle(SESSION, "one"));
        app.on_line(widget("ex", "wid", &["l1", "l2", "l3", "l4", "l5"]));
        app.on_click(TargetId::Panel(Spot::Widget));
        app.on_line(approval());
        app
    };
    assert!(setup().panel().is_some());
    assert_eq!(setup().narrow_fit().map(|fit| fit.status), Some(0));
    let (rows, cursor, _) = rendered(&setup(), 100, 10);
    assert!(cursor.is_none());
    // No status row survives, so the panel's bottom edge ends on the last row.
    assert!(!rows.iter().any(|row| row.contains("$1.50")), "{rows:?}");
    assert!(
        rows[rows.len() - 1]
            .trim_start()
            .chars()
            .all(|cell| cell == '▀'),
        "{rows:?}"
    );
    assert!(
        rows[rows.len() - 4].contains("Deny  type to add feedback"),
        "{rows:?}"
    );
}

/// A form asking which branch, then for a name in words.
fn form() -> Line {
    session_line(
        SESSION,
        "interaction_requested",
        serde_json::json!({"request_id": "r_4f", "kind": "form", "action_ids": ["a_1"],
            "fields": [
                {"header": "Base", "question": "Which branch?", "multiSelect": false,
                    "options": [{"label": "main", "description": "the default"},
                        {"label": "dev"}]},
                {"header": "Name", "question": "What name?"}]}),
    )
}

#[test]
fn the_question_caret_sits_inside_the_panel_above_the_status_row() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(form());
    assert!(app.panel().is_some());
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Tab, now);
    for key in "qx".chars().map(Key::Char) {
        app.on_key(key, fakes::clock::FakeClock::new().now());
    }
    let (rows, cursor, _) = rendered(&app, 100, 30);
    assert_eq!(app.narrow_fit().map(|fit| fit.status), Some(1));
    let at = cursor.expect("a caret in the words row");
    // The caret's row holds the typed words, above the panel's last
    // row; the status line shows only while the panel does not ("The
    // narrow layout"), so the slab floor is last.
    assert!(rows[usize::from(at.y)].contains("qx"), "{rows:?}");
    assert!(usize::from(at.y) < rows.len() - 3, "{rows:?}");
    assert!(
        rows[rows.len() - 1]
            .trim_start()
            .chars()
            .all(|cell| cell == '▀'),
        "{rows:?}"
    );
    assert!(
        rows[rows.len() - 2].contains("type to answer in words"),
        "{rows:?}"
    );
    assert!(!rows.iter().any(|row| row.contains("$1.50")), "{rows:?}");
}

#[test]
fn row_two_spots_cover_each_running_segment() {
    use crate::app::panel::Spot;
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(started("j_1", "build"));
    app.on_line(started("j_2", "test"));
    app.on_line(started("j_3", "lint"));
    app.on_line(delegated("j_2"));
    app.on_line(delegated("j_3"));
    // "◆ 2 delegates running  │  ⚙ 1 job running": each segment opens
    // its list, glyphs included.
    let rows = crate::view::status_rows::status(&app, 100);
    let row = rows.last().unwrap_or_else(|| panic!("row two"));
    assert_eq!(
        row.line.to_string(),
        "\u{25c6} 2 delegates running  \u{2502}  \u{2699} 1 job running"
    );
    assert_eq!(
        row.spots,
        [(0, 21, Spot::DelegateList), (26, 15, Spot::JobList),]
    );
    // A row exactly filling the width keeps every cell: no `…`.
    let rows = crate::view::status_rows::status(&app, 42);
    let row = rows.last().unwrap_or_else(|| panic!("row two"));
    assert_eq!(
        row.line.to_string(),
        "\u{25c6} 2 delegates running  \u{2502}  \u{2699} 1 job running"
    );
    // A cut row keeps only the visible part: the whole row is wider
    // than 22, so the job segment leaves no target while the delegate
    // segment keeps its width.
    let rows = crate::view::status_rows::status(&app, 22);
    let row = rows.last().unwrap_or_else(|| panic!("row two"));
    assert_eq!(row.line.to_string(), "\u{25c6} 2 delegates runnin\u{2026}");
    assert_eq!(row.spots, [(0, 21, Spot::DelegateList)]);
    let rows = crate::view::status_rows::status(&app, 30);
    let row = rows.last().unwrap_or_else(|| panic!("row two"));
    assert_eq!(
        row.spots,
        [(0, 21, Spot::DelegateList), (26, 3, Spot::JobList),]
    );
}

/// A `preamble_built` with a trigger and thinking level.
fn full_preamble() -> Line {
    session_line(
        SESSION,
        "preamble_built",
        serde_json::json!({
            "reason": "start", "model": "test/model",
            "context_window": 1000000, "trigger_at": 400000,
            "thinking": "high", "tool_choice": "auto",
            "cache_lifetime": "5m", "system_prompt": "", "tools": [],
        }),
    )
}

/// A `session_status` with git, context and spend for the status line.
fn full_status() -> Line {
    session_line(
        SESSION,
        "session_status",
        serde_json::json!({
            "name": "one", "workspace": "/Users/you/work/fiber",
            "project": "-w", "state": "idle", "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 900, "cache_read": 100,
                "cache_write": {"5m": 50}, "output": 200},
                "cost": 4.12, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0,
            "clients": 0, "git": {"branch": "main"},
            "context": {"tokens": 268000, "window": 1000000},
        }),
    )
}

/// An app with every row-1 segment: git, thinking, context, spend,
/// cache, a down server and changed files.
fn full_app() -> App {
    let mut app = attached(160, 30);
    app.on_line(full_preamble());
    app.on_line(full_status());
    app.on_line(session_line(
        SESSION,
        "mcp_server_failed",
        serde_json::json!({"server": "github", "reason": "died",
            "will_restart": false,
            "error": {"code": "mcp_server_unavailable", "message": "died"}}),
    ));
    app.on_line(changed_call(serde_json::json!([
        {"path": "src/a.rs", "added": 10, "removed": 2},
        {"path": "src/b.rs", "added": 3, "removed": 3},
    ])));
    app
}

/// The style of the span holding `content` in `row`.
fn style_of(row: &crate::view::status_rows::StatusRow, content: &str) -> ratatui::style::Style {
    row.line
        .spans
        .iter()
        .find(|span| span.content == content)
        .unwrap_or_else(|| panic!("a {content:?} span in {}", row.line))
        .style
}

/// Row 1 at 160: every segment in card order, each in its roles, with
/// the spots on the clickable parts (`docs/tui.md`, "The narrow
/// layout").
#[test]
fn status_row_one_lists_every_segment_in_its_roles() {
    use crate::theme::{Role, style};
    let app = full_app();
    let rows = crate::view::status_rows::status(&app, 160);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    // The working directory, model, thinking level, permission mode and
    // context live in the input box's status row ("The input box"), so
    // the status line leaves them out ("The narrow layout").
    assert_eq!(
        row.line.to_string(),
        "git main  \u{2502}  $4.12  \u{2502}  cache 9%  \u{2502}  tools \u{2717} github down  \u{2502}  \u{00b1} 2 files +13 \u{2212}5"
    );
    // The joiner is dim.
    assert_eq!(style_of(row, "  \u{2502}  "), style(Role::Muted));
    // "git" dim, the branch in secondary, spotted alone.
    assert_eq!(style_of(row, "git"), style(Role::Muted));
    assert_eq!(style_of(row, "main"), style(Role::Secondary));
    // Dim `cache` with its share, dim `tools` with its error value,
    // dim `±` with the added and removed totals.
    assert_eq!(style_of(row, "cache"), style(Role::Muted));
    assert_eq!(style_of(row, "tools"), style(Role::Muted));
    assert_eq!(style_of(row, "\u{2717} github down"), style(Role::Error));
    assert_eq!(style_of(row, "\u{00b1}"), style(Role::Muted));
    assert_eq!(style_of(row, "+13"), style(Role::Added));
    assert_eq!(style_of(row, "\u{2212}5"), style(Role::Removed));
    assert_eq!(
        row.spots,
        [
            (4, 4, Spot::Branch),
            (13, 5, Spot::Usage),
            (29, 2, Spot::Usage),
            (36, 19, Spot::Tools),
            (60, 16, Spot::ChangedFiles),
        ]
    );
}

/// Row 1 cut at 61: the shorter row still ends in `…` keeping its
/// styles, past the terminal's right edge, and only the visible
/// targets keep.
#[test]
fn status_row_one_cut_at_the_edge() {
    let app = full_app();
    let rows = crate::view::status_rows::status(&app, 61);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(crate::format::width(&row.line.to_string()), 60);
    assert!(row.line.to_string().ends_with('\u{2026}'));
    assert!(row.line.to_string().starts_with("git main"));
    for (start, wide, _) in &row.spots {
        assert!(usize::from(*start) < 60, "{}", row.line);
        assert!(start.saturating_add(*wide) <= 60, "{}", row.line);
    }
    // The branch survives the cut; the changed files do not.
    assert!(row.spots.iter().any(|(_, _, spot)| *spot == Spot::Branch));
    assert!(
        row.spots
            .iter()
            .all(|(_, _, spot)| *spot != Spot::ChangedFiles)
    );
}

/// Row 1 at the 30 and 22 floors: cut mid-span with the cut styled as
/// the span it cuts, targets clamped.
#[test]
fn status_row_one_at_the_floors() {
    let app = full_app();
    for width in [30u16, 22] {
        let text = usize::from(width) - 1;
        let rows = crate::view::status_rows::status(&app, width);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(crate::format::width(&row.line.to_string()), text);
        for (start, wide, _) in &row.spots {
            assert!(usize::from(*start) < text);
            assert!(usize::from(start.saturating_add(*wide)) <= text);
        }
    }
}

/// The 8-cell bar fills toward the handoff point with no marker: at
/// zero, below, at and past the trigger, and against the window with
/// none. It lives in the input box's status row ("The input box"), so
/// row one carries no bar and no `ctx`.
#[test]
fn the_narrow_bar_fills_without_a_marker() {
    use crate::theme::{Role, style};
    let mut app = attached(100, 30);
    app.on_line(full_preamble());
    // Five of eight at 268k of the 400k handoff, and no attention
    // marker anywhere in the row: every `│` is the dim joiner.
    app.on_line(full_status());
    let row = &crate::view::status_rows::status(&app, 100)[0];
    assert!(
        row.line.spans.iter().all(|span| span.content != "\u{2586}"),
        "{}",
        row.line
    );
    assert!(!row.line.to_string().contains("ctx"), "{}", row.line);
    assert!(
        row.line
            .spans
            .iter()
            .all(|span| { span.content != "\u{2502}" || span.style == style(Role::Muted) }),
        "{}",
        row.line
    );
    // The bar fills in the input box's status row instead.
    let (screen, _) = draw(&app, 100, 30);
    assert!(
        screen.contains("ctx \u{2586}\u{2586}\u{2586}\u{2586}\u{2586}\u{2586}\u{2586}\u{2586}"),
        "{screen}"
    );
}

/// Without a status the row count matches the base's: the waiting
/// segment draws one row, and nothing held draws no row. The model
/// lives in the input box's status row ("The input box"), not here.
#[test]
fn without_a_status_the_row_count_matches_the_base() {
    // The preamble's model with no status: one row with the waiting
    // segment, while the rail is not drawn.
    let mut app = attached(100, 30);
    app.on_line(preamble("test/model", 100));
    let rows = crate::view::status_rows::status(&app, 100);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].line.to_string(), "0 waiting");
    // The waiting segment opens the row and carries its target.
    assert_eq!(waiting_spots(&app, 100), vec![(0, 9)]);
    // Nothing held: only the waiting segment's row, while the rail is
    // not drawn.
    let app = attached(100, 30);
    let rows = crate::view::status_rows::status(&app, 100);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].line.to_string(), "0 waiting");
}

/// A status with nothing else takes one row: the waiting segment
/// opens it. The working directory names its segment in the input
/// box's status row ("The input box"), not here ("The narrow layout").
#[test]
fn a_status_with_nothing_else_takes_one_row() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    let rows = crate::view::status_rows::status(&app, 100);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].line.to_string().starts_with("0 waiting"));
    assert!(!rows[0].line.to_string().contains("/w"));
}

/// The status rows draw on the slab's `surface` tint one column in;
/// with no colour no tint is laid.
#[test]
fn status_rows_draw_on_the_panel_tint() {
    use crate::theme::Role;
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    // The slab floor takes the last row, so row one sits one up, on
    // the slab's surface tint ("The input box" is a `surface` slab).
    let bottom = buf.area.bottom().saturating_sub(2);
    // The row's cells sit on the surface tint from column 1.
    assert_eq!(buf[(1, bottom)].bg, Role::Surface.color());
    assert_eq!(buf[(98, bottom)].bg, Role::Surface.color());
    // With no colour the tint goes and the text stays.
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let mut bare = ratatui::buffer::Buffer::empty(area);
    crate::view::render(&app, area, &mut bare, None);
    crate::surface::init(&|_: &str| None);
    assert_ne!(bare[(1, bottom)].bg, Role::Panel.color());
    let row: String = (1..99).map(|x| bare[(x, bottom)].symbol()).collect();
    assert!(row.contains("$1.50"), "{row}");
}

/// A `session_status` with `input` tokens in: the cache share's edge is
/// one token inside.
fn input_status(input: u64) -> Line {
    session_line(
        SESSION,
        "session_status",
        serde_json::json!({
            "name": "one", "workspace": "/w", "project": "-w", "state": "idle",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": input, "cache_read": 0,
                "cache_write": {}, "output": 0},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    )
}

/// Folds a down MCP `server` into `app`.
fn fail_server(app: &mut App, server: &str) {
    app.on_line(session_line(
        SESSION,
        "mcp_server_failed",
        serde_json::json!({"server": server, "reason": "died",
            "will_restart": false,
            "error": {"code": "mcp_server_unavailable", "message": "died"}}),
    ));
}

/// The cache segment needs a token inside: with none the row holds no
/// share, matching the card; with one it does.
#[test]
fn the_cache_segment_needs_tokens_inside() {
    let mut app = attached(100, 30);
    app.on_line(preamble("test/model", 100));
    app.on_line(input_status(0));
    let rows = crate::view::status_rows::status(&app, 100);
    assert_eq!(rows.len(), 1);
    assert!(
        !rows[0].line.to_string().contains("cache"),
        "{}",
        rows[0].line
    );
    let mut app = attached(100, 30);
    app.on_line(preamble("test/model", 100));
    app.on_line(input_status(1));
    let rows = crate::view::status_rows::status(&app, 100);
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].line.to_string().contains("cache 0%"),
        "{}",
        rows[0].line
    );
}

/// The tools segment joins one down server after another with a space:
/// the first opens the value, the second follows the joiner.
#[test]
fn the_tools_segment_joins_down_servers_with_a_space() {
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    fail_server(&mut app, "relay");
    let rows = crate::view::status_rows::status(&app, 100);
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0]
            .line
            .to_string()
            .contains("tools \u{2717} relay down"),
        "{}",
        rows[0].line
    );
    fail_server(&mut app, "gateway");
    let rows = crate::view::status_rows::status(&app, 100);
    assert_eq!(rows.len(), 1);
    // Sorted: gateway before relay, one space between the values; the
    // shorter row shows both values whole ("The narrow layout").
    assert!(
        rows[0]
            .line
            .to_string()
            .contains("tools \u{2717} gateway down \u{2717} relay down"),
        "{}",
        rows[0].line
    );
}

/// A row of ten cells for the cut_row boundary tests: no targets of its
/// own, so only the fed spots show.
fn cut_spots(spots: Vec<(u16, u16, Spot)>, width: usize) -> Vec<(u16, u16, Spot)> {
    use ratatui::text::Span;

    let joined = super::Joined {
        spans: vec![Span::raw("0123456789")],
        spots,
    };
    super::cut_row(joined, width).spots
}

/// cut_row drops a zero-width spot: it covers no column.
#[test]
fn cut_row_drops_a_zero_width_spot() {
    assert!(cut_spots(vec![(5, 0, Spot::Usage)], 100).is_empty());
}

/// cut_row keeps a spot ending one column past the text's last column:
/// it still covers that column.
#[test]
fn cut_row_keeps_a_spot_covering_the_last_column() {
    assert_eq!(
        cut_spots(vec![(98, 5, Spot::Usage)], 100),
        vec![(98, 1, Spot::Usage)]
    );
}

/// cut_row drops a spot starting at the text edge: past it covers
/// nothing.
#[test]
fn cut_row_drops_a_spot_starting_at_the_edge() {
    assert!(cut_spots(vec![(99, 5, Spot::Usage)], 100).is_empty());
}
