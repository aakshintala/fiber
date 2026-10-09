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
    assert_eq!(rows[bottom - 2], ">", "{}", screen);
}

/// A `session_status` with billed `cost` and `subscription` spend.
fn spend_status(cost: serde_json::Value, subscription: f64) -> Line {
    session_line(
        SESSION,
        "session_status",
        serde_json::json!({
            "name": "one", "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
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
fn the_spend_segment_reads_against_the_budget() {
    let mut app = attached(100, 30);
    app.on_line(budget_preamble(5.0));
    app.on_line(spend_status(serde_json::json!(1.25), 0.0));
    let (screen, _) = draw(&app, 100, 30);
    let rows: Vec<&str> = screen.lines().collect();
    assert!(rows[rows.len() - 1].contains("$1.25 of $5.00"), "{screen}");

    let mut app = attached(100, 30);
    app.on_line(spend_status(serde_json::json!(1.25), 0.0));
    let (screen, _) = draw(&app, 100, 30);
    let rows: Vec<&str> = screen.lines().collect();
    assert!(rows[rows.len() - 1].contains("$1.25"), "{screen}");
    assert!(!screen.contains("of $"), "{screen}");
}

#[test]
fn the_budget_comparison_counts_billed_spend_only() {
    // $1 billed plus $9 on subscription against a $5 budget: the
    // subscription bills nothing per call.
    let mut app = attached(100, 30);
    app.on_line(budget_preamble(5.0));
    app.on_line(spend_status(serde_json::json!(1.0), 9.0));
    let (screen, _) = draw(&app, 100, 30);
    let rows: Vec<&str> = screen.lines().collect();
    assert!(rows[rows.len() - 1].contains("$1.00 of $5.00"), "{screen}");
    assert!(!screen.contains("$10.00"), "{screen}");

    // Subscription-only: billed $0 against the budget.
    let mut app = attached(100, 30);
    app.on_line(budget_preamble(5.0));
    app.on_line(spend_status(serde_json::json!(0.0), 9.0));
    let (screen, _) = draw(&app, 100, 30);
    let rows: Vec<&str> = screen.lines().collect();
    assert!(rows[rows.len() - 1].contains("$0.00 of $5.00"), "{screen}");
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
    assert_eq!(screen.lines().rev().nth(1), Some(">"));
    // The row above the input box is blank inside the column.
    assert_eq!(
        buf.cell((2, 38))
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
    // Four widget rows keep row 1 alone at twelve rows, allowing for the
    // input box's two surface edges.
    let mut app = setup(12);
    app.on_line(widget("ex", "wid", &["l1", "l2", "l3", "l4"]));
    let (screen, _) = draw(&app, 100, 12);
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
    assert_eq!(status, card);
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
    // The input row, its lower surface edge, then the one status row.
    assert_eq!(usize::from(at.y), rows.len() - 3);
    assert!(rows[usize::from(at.y)].contains('>'), "{rows:?}");
    assert!(rows[rows.len() - 1].contains("$1.50"), "{rows:?}");
}

#[test]
fn the_draft_cursor_sits_above_two_status_rows() {
    let app = two_rows(30);
    let (rows, cursor, _) = rendered(&app, 100, 30);
    assert_eq!(app.narrow_fit().map(|fit| fit.status), Some(2));
    let at = cursor.expect("a cursor on the input row");
    // The input row, its lower surface edge, then row 1 and row 2.
    assert_eq!(usize::from(at.y), rows.len() - 4);
    assert!(rows[usize::from(at.y)].contains('>'), "{rows:?}");
    assert!(rows[rows.len() - 2].contains("$1.50"), "{rows:?}");
    assert!(rows[rows.len() - 1].contains("1 job running"), "{rows:?}");
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
        // The cursor stays on the input row while the lower surface edge
        // and kept status rows move beneath it.
        assert_eq!(usize::from(at.y), usize::from(height) - keep - 2);
        assert!(rows[usize::from(at.y)].contains('>'), "{height}: {rows:?}");
        let below = &rows[usize::from(at.y) + 1..];
        assert_eq!(below.len(), keep + 1, "{height}: {rows:?}");
        assert!(
            below[0].chars().all(|cell| cell == '▀'),
            "{height}: {rows:?}"
        );
        if keep >= 1 {
            assert!(below[1].contains("$1.50"), "{height}: {rows:?}");
        }
        if keep >= 2 {
            assert!(below[2].contains("1 job running"), "{height}: {rows:?}");
        }
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
    // Row one is "1 file +10 −2 · 1 waiting": the waiting segment starts
    // after the file totals and the separator.
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
    assert_eq!(wide, u16::try_from("1 waiting".len()).unwrap_or(u16::MAX));
    // A segment starting exactly at the row's width has no target.
    assert!(waiting_spots(&app, start).is_empty(), "start {start}");
    // A segment starting beyond the row's width has no target either.
    assert!(waiting_spots(&app, start - 1).is_empty(), "start {start}");
    // A segment ending exactly at the row's width keeps its full width.
    assert_eq!(waiting_spots(&app, start + wide), vec![(start, wide)]);
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
    // The panel's bottom edge sits above the status row.
    assert!(rows[rows.len() - 1].contains("$1.50"), "{rows:?}");
    assert!(
        rows[rows.len() - 2].chars().all(|cell| cell == '▀'),
        "{rows:?}"
    );
    assert!(
        rows[rows.len() - 3].contains("deny · type to add feedback"),
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
        rows[rows.len() - 1].chars().all(|cell| cell == '▀'),
        "{rows:?}"
    );
    assert!(
        rows[rows.len() - 2].contains("deny · type to add feedback"),
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
    // The caret's row holds the typed words, above the panel's last row,
    // its bottom edge, and the status row beneath it.
    assert!(rows[usize::from(at.y)].contains("qx"), "{rows:?}");
    assert!(usize::from(at.y) < rows.len() - 3, "{rows:?}");
    assert!(
        rows[rows.len() - 2].chars().all(|cell| cell == '▀'),
        "{rows:?}"
    );
    assert!(rows[rows.len() - 3].contains("Chat about this"), "{rows:?}");
    assert!(rows[rows.len() - 1].contains("$1.50"), "{rows:?}");
}
