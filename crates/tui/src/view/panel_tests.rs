//! Tests for the panel's card order and widget cards.

use super::{cards, rows};
use crate::app::App;
use crate::home::Launch;
use crate::link::Line;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::path::PathBuf;

const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// The default card list.
fn list(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// An app with home state, attached, at `width` by `height`, drawing the
/// default card list.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: CARDS.map(str::to_owned).to_vec(),
        ..Default::default()
    });
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app.set_size(width, height);
    app
}

/// One envelope of the attached session.
fn session_line(kind: &str, payload: serde_json::Value) -> Line {
    timed_line(kind, 0, payload)
}

/// One envelope of the attached session at `ts`.
fn timed_line(kind: &str, ts: u64, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Folds a widget of `lines` into `app`.
fn fold_widget(app: &mut App, extension: &str, widget: &str, lines: &[&str]) {
    app.on_line(session_line(
        "extension_ui",
        serde_json::json!({"extension": extension, "widget": widget, "lines": lines}),
    ));
}

/// A `session_status` with `spend` and `context`.
fn status_line(spend: serde_json::Value, context: Option<serde_json::Value>) -> Line {
    let mut payload = serde_json::json!({
        "name": "work", "workspace": "/Users/you/work/fiber", "project": "-w",
        "state": "idle", "since": 0, "spend": spend,
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    if let Some(context) = context {
        payload["context"] = context;
    }
    session_line("session_status", payload)
}

/// A spend with `input`, `read`, `written`, `output`, billed `cost` and
/// `subscription` cost.
fn spend(
    input: u64,
    read: u64,
    written: u64,
    output: u64,
    cost: serde_json::Value,
    subscription: f64,
) -> serde_json::Value {
    serde_json::json!({"tokens": {"input": input, "cache_read": read,
        "cache_write": {"5m": written}, "output": output},
        "cost": cost, "subscription_cost": subscription})
}

/// A `preamble_built` with `trigger`: `None` leaves automatic handoff
/// off.
fn preamble_line(trigger: Option<u64>) -> Line {
    let mut payload = serde_json::json!({
        "reason": "start", "model": "test/model", "context_window": 1000,
        "thinking": "high", "tool_choice": "auto", "cache_lifetime": "5m",
        "system_prompt": "", "tools": [],
    });
    if let Some(trigger) = trigger {
        payload["trigger_at"] = trigger.into();
    }
    session_line("preamble_built", payload)
}

/// The drawn rows' text at `width`.
fn texts(app: &App, width: u16) -> Vec<String> {
    rows(app, width)
        .iter()
        .map(|row| row.line.to_string())
        .collect()
}

#[test]
fn cache_hits_need_input() {
    let mut app = attached(160, 40);
    app.on_line(preamble_line(Some(800)));
    app.on_line(status_line(
        spend(0, 0, 0, 5, serde_json::json!(0.0), 0.0),
        None,
    ));
    assert!(
        texts(&app, 40)
            .iter()
            .all(|row| !row.starts_with("cache hits"))
    );
    app.on_line(status_line(
        spend(1, 0, 0, 5, serde_json::json!(0.0), 0.0),
        None,
    ));
    assert!(texts(&app, 40).iter().any(|row| row == "cache hits  0%"));
}

#[test]
fn pct_and_bar_floor() {
    let mut app = attached(160, 40);
    app.on_line(preamble_line(Some(800)));
    app.on_line(status_line(
        spend(109, 10, 0, 5, serde_json::json!(0.0), 0.0),
        Some(serde_json::json!({"tokens": 119, "window": 1000})),
    ));
    let drawn = texts(&app, 40);
    // Two of 17 cells fill (119 of the 800 trigger), then the marker
    // and the size right-aligned.
    let bar = "▆▆".to_owned() + &"░".repeat(15) + "│" + &" ".repeat(16) + "119";
    assert!(drawn.iter().any(|row| row == &bar), "{drawn:?}");
    // The handoff row fits with three of pad, nothing cut.
    assert!(
        drawn
            .iter()
            .any(|row| row == "handoff at 800          11% of window"),
        "{drawn:?}"
    );
    assert!(
        drawn
            .iter()
            .any(|row| row == "then a summary, fresh context"),
        "{drawn:?}"
    );
}

#[test]
fn context_rows_need_a_window_and_a_status() {
    let mut app = attached(160, 40);
    app.on_line(preamble_line(Some(800)));
    assert!(
        texts(&app, 40).iter().all(|row| !row.contains('▆')
            && !row.contains("handoff")
            && !row.contains("of window"))
    );
}

#[test]
fn a_zero_context_window_has_no_context_rows() {
    let mut app = attached(160, 40);
    app.on_line(session_line(
        "preamble_built",
        serde_json::json!({
            "reason": "start", "model": "test/model", "context_window": 0,
            "thinking": "high", "tool_choice": "auto", "cache_lifetime": "5m",
            "system_prompt": "", "tools": [],
        }),
    ));
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::json!(0.0), 0.0),
        Some(serde_json::json!({"tokens": 1, "window": 1000})),
    ));
    let drawn = texts(&app, 40);
    assert!(
        drawn
            .iter()
            .any(|row| row == "model  test/model · thinking high")
    );
    assert!(
        drawn.iter().all(|row| !row.contains('▆')
            && !row.contains("handoff")
            && !row.contains("of window"))
    );
}

#[test]
fn a_full_context_fills_every_cell() {
    let mut app = attached(160, 40);
    app.on_line(preamble_line(None));
    app.on_line(status_line(
        spend(1000, 0, 0, 0, serde_json::json!(0.0), 0.0),
        Some(serde_json::json!({"tokens": 1000, "window": 1000})),
    ));
    let drawn = texts(&app, 40);
    // All 17 cells fill, no marker without a trigger, then the size.
    let bar = "▆".repeat(17) + &" ".repeat(16) + "1.0k";
    assert!(drawn.iter().any(|row| row == &bar), "{drawn:?}");
    // With no trigger the share stands alone, left-aligned.
    assert!(drawn.iter().any(|row| row == "100% of window"), "{drawn:?}");
}

#[test]
fn the_marker_follows_the_seventeen_cells() {
    let mut app = attached(160, 40);
    app.on_line(preamble_line(Some(1000)));
    app.on_line(status_line(
        spend(500, 0, 0, 0, serde_json::json!(0.0), 0.0),
        Some(serde_json::json!({"tokens": 500, "window": 1000})),
    ));
    let drawn = texts(&app, 40);
    let bar = "▆".repeat(8) + &"░".repeat(9) + "│" + &" ".repeat(16) + "500";
    assert!(drawn.iter().any(|row| row == &bar), "{drawn:?}");
    // The handoff row fits with two of pad, nothing cut.
    assert!(
        drawn
            .iter()
            .any(|row| row == "handoff at 1.0k         50% of window"),
        "{drawn:?}"
    );
}

#[test]
fn the_fill_clamps_at_zero_the_trigger_and_past_it() {
    assert_eq!(super::bar_fill(0, 1000, Some(800)), 0);
    assert_eq!(super::bar_fill(800, 1000, Some(800)), 17);
    assert_eq!(super::bar_fill(900, 1000, Some(800)), 17);
    assert_eq!(super::bar_fill(500, 1000, None), 8);
    // A zero goal never divides: empty, not a panic.
    assert_eq!(super::bar_fill(5, 0, None), 0);
    assert_eq!(super::bar_fill(5, 1000, Some(0)), 0);
}

#[test]
fn sides_pads_exact_fit_and_cuts_one_short() {
    use ratatui::text::Span;
    let fit = super::sides(vec![Span::raw("ab")], Span::raw("cd"), 5);
    assert_eq!(fit.to_string(), "ab cd");
    let cut = super::sides(vec![Span::raw("ab")], Span::raw("cd"), 4);
    assert_eq!(cut.to_string(), "… cd");
}

#[test]
fn cost_rows() {
    for (cost, subscription, billed, on_subscription) in [
        (serde_json::json!(0.0), 0.0, false, false),
        (serde_json::json!(0.01), 0.0, true, false),
        (serde_json::json!(0.41), 1.10, true, true),
        (serde_json::Value::Null, 0.0, true, false),
    ] {
        let mut app = attached(160, 40);
        app.on_line(status_line(spend(1, 0, 0, 2, cost, subscription), None));
        let drawn = texts(&app, 40);
        assert_eq!(
            drawn.iter().any(|row| row.starts_with("cost billed")),
            billed,
            "{drawn:?}"
        );
        assert_eq!(
            drawn
                .iter()
                .any(|row| row.starts_with("cost on subscription")),
            on_subscription,
            "{drawn:?}"
        );
    }
    let mut app = attached(160, 40);
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::json!(0.01), 0.0),
        None,
    ));
    assert!(
        texts(&app, 40)
            .iter()
            .any(|row| row == "cost billed  $0.01")
    );
    let mut app = attached(160, 40);
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::Value::Null, 0.0),
        None,
    ));
    assert!(
        texts(&app, 40)
            .iter()
            .any(|row| row == "cost billed  unknown")
    );
}

/// A `preamble_built` setting `budget.usd`.
fn budget_preamble(budget: f64) -> Line {
    session_line(
        "preamble_built",
        serde_json::json!({
            "reason": "start", "model": "test/model", "context_window": 1000,
            "thinking": "high", "tool_choice": "auto", "cache_lifetime": "5m",
            "system_prompt": "", "tools": [], "budget": budget,
        }),
    )
}

#[test]
fn cost_against_a_budget() {
    for (cost, row) in [
        (serde_json::json!(0.0), "cost billed  $0.00 of $5.00"),
        (serde_json::json!(1.25), "cost billed  $1.25 of $5.00"),
        (serde_json::Value::Null, "cost billed  unknown of $5.00"),
    ] {
        let mut app = attached(160, 40);
        app.on_line(budget_preamble(5.0));
        app.on_line(status_line(spend(1, 0, 0, 2, cost, 0.0), None));
        assert!(texts(&app, 40).iter().any(|drawn| drawn == row), "{row}");
    }
    // Without a budget the row keeps its rule: hidden at zero,
    // plain above it.
    let mut app = attached(160, 40);
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::json!(0.0), 0.0),
        None,
    ));
    assert!(
        texts(&app, 40)
            .iter()
            .all(|row| !row.starts_with("cost billed"))
    );
    let mut app = attached(160, 40);
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::json!(1.25), 0.0),
        None,
    ));
    assert!(
        texts(&app, 40)
            .iter()
            .any(|row| row == "cost billed  $1.25")
    );
}

#[test]
fn the_budget_comparison_counts_billed_spend_only() {
    // $1 billed plus $9 on subscription against a $5 budget: the
    // subscription bills nothing per call.
    let mut app = attached(160, 40);
    app.on_line(budget_preamble(5.0));
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::json!(1.0), 9.0),
        None,
    ));
    let drawn = texts(&app, 40);
    assert!(
        drawn.iter().any(|row| row == "cost billed  $1.00 of $5.00"),
        "{drawn:?}"
    );
    assert!(
        drawn.iter().any(|row| row == "cost on subscription  $9.00"),
        "{drawn:?}"
    );
    // Subscription-only: billed $0 against the budget.
    let mut app = attached(160, 40);
    app.on_line(budget_preamble(5.0));
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::json!(0.0), 9.0),
        None,
    ));
    let drawn = texts(&app, 40);
    assert!(
        drawn.iter().any(|row| row == "cost billed  $0.00 of $5.00"),
        "{drawn:?}"
    );
}

#[test]
fn turns_at_zero_are_left_out() {
    let mut app = attached(160, 40);
    assert!(rows(&app, 40).is_empty());
    app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": []}),
    ));
    assert!(texts(&app, 40).iter().any(|row| row == "turns  1"));
}

#[test]
fn the_model_falls_back_to_the_status() {
    let mut app = attached(160, 40);
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::json!(0.0), 0.0),
        None,
    ));
    assert!(texts(&app, 40).iter().any(|row| row == "model  test/model"));
    app.on_line(preamble_line(Some(800)));
    assert!(
        texts(&app, 40)
            .iter()
            .any(|row| row == "model  test/model · thinking high")
    );
}

/// Folds a session with every row present: directory, model and thinking,
/// context with its bar, marker and handoff, tokens, cache hits, both
/// costs, speed, turns and a server down.
fn full_session(app: &mut App) {
    app.on_line(preamble_line(Some(800)));
    app.on_line(status_line(
        spend(900, 100, 50, 200, serde_json::json!(0.41), 1.10),
        Some(serde_json::json!({"tokens": 500, "window": 1000})),
    ));
    app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": []}),
    ));
    app.on_line(timed_line(
        "assistant_message_started",
        1000,
        serde_json::json!({}),
    ));
    app.on_line(timed_line(
        "usage_recorded",
        3000,
        serde_json::json!({
            "generation_id": "g_1", "model": "test/model",
            "tokens": {"input": 10, "cache_read": 0,
                "cache_write": {}, "output": 500},
            "input_bytes": 0, "cost": 0.01,
        }),
    ));
    app.on_line(session_line(
        "mcp_server_failed",
        serde_json::json!({"server": "relay", "reason": "died",
            "will_restart": false,
            "error": {"code": "mcp_server_unavailable", "message": "died"}}),
    ));
}

#[test]
fn session_card_full() {
    use crate::app::panel::Spot;
    use crate::mouse::TargetId;
    let mut app = attached(160, 40);
    full_session(&mut app);
    let (buf, targets) = draw_targets(&app);
    insta::assert_snapshot!("session_card_full", super::super::text(&buf));
    let ids: Vec<TargetId> = targets.iter().map(|target| target.id).collect();
    assert_eq!(
        ids,
        vec![
            TargetId::Panel(Spot::Context),
            TargetId::Panel(Spot::Context),
            TargetId::Panel(Spot::Context),
            TargetId::Panel(Spot::Usage),
            TargetId::Panel(Spot::Usage),
            TargetId::Panel(Spot::Usage),
            TargetId::Panel(Spot::Usage),
            TargetId::Panel(Spot::Tools),
        ]
    );
}

#[test]
fn session_card_without_trigger() {
    let mut app = attached(160, 40);
    app.on_line(preamble_line(None));
    app.on_line(status_line(
        spend(900, 100, 50, 200, serde_json::json!(0.41), 0.0),
        Some(serde_json::json!({"tokens": 500, "window": 1000})),
    ));
    let (buf, _) = draw_targets(&app);
    insta::assert_snapshot!("session_card_without_trigger", super::super::text(&buf));
}

#[test]
fn session_card_with_budget() {
    let mut app = attached(160, 40);
    app.on_line(budget_preamble(5.0));
    app.on_line(status_line(
        spend(900, 100, 50, 200, serde_json::json!(0.41), 1.10),
        Some(serde_json::json!({"tokens": 500, "window": 1000})),
    ));
    insta::assert_snapshot!("session_card_with_budget", screen(&app, 160, 40));
}

#[test]
fn every_context_card_row_opens_the_context_view() {
    use crate::app::panel::Spot;
    let mut app = attached(160, 40);
    app.on_line(preamble_line(Some(800)));
    app.on_line(status_line(
        spend(500, 0, 0, 0, serde_json::json!(0.0), 0.0),
        Some(serde_json::json!({"tokens": 500, "window": 1000})),
    ));
    let drawn = rows(&app, 40);
    let context_rows: Vec<&super::Row> = drawn
        .iter()
        .filter(|row| row.spot == Some(Spot::Context))
        .collect();
    // The bar, the handoff row and the summary row, each opening the
    // context breakdown.
    assert_eq!(context_rows.len(), 3);
    assert!(
        context_rows
            .iter()
            .all(|row| row.spot == Some(Spot::Context))
    );
}

#[test]
fn context_bar_marker_at_the_trigger() {
    let mut app = attached(160, 40);
    app.on_line(preamble_line(Some(800)));
    app.on_line(status_line(
        spend(500, 0, 0, 0, serde_json::json!(0.0), 0.0),
        Some(serde_json::json!({"tokens": 500, "window": 1000})),
    ));
    let (buf, targets) = draw_targets(&app);
    assert!(targets.iter().any(|target| {
        target.id == crate::mouse::TargetId::Panel(crate::app::panel::Spot::Usage)
    }));
    insta::assert_snapshot!(
        "context_bar_marker_at_the_trigger",
        super::super::text(&buf)
    );
}

#[test]
fn session_card_at_the_panel_floor() {
    let mut app = attached(140, 24);
    full_session(&mut app);
    let (buf, targets) = draw_targets(&app);
    insta::assert_snapshot!("session_card_at_the_panel_floor", super::super::text(&buf));
    // The `tools  relay down` row draws at this height, so it is a target.
    assert!(
        targets.iter().any(
            |target| target.id == crate::mouse::TargetId::Panel(crate::app::panel::Spot::Tools)
        ),
        "{targets:?}"
    );
}

#[test]
fn cards_in_configured_order() {
    let mut app = attached(160, 40);
    full_session(&mut app);
    fold_changes(&mut app, &[("src/a.rs", 10, 2)]);
    fold_job(&mut app, "j_1", "build");
    fold_widget(&mut app, "plan", "tasks", &["one"]);
    let (buf, _) = draw_targets(&app);
    insta::assert_snapshot!("cards_in_configured_order", super::super::text(&buf));
}

#[test]
fn cards_follow_the_list() {
    let widgets = [("plan", "tasks"), ("other", "list")];
    assert_eq!(
        cards(&list(&["plan/tasks", "other/list"]), &widgets),
        vec![super::Card::Widget(0), super::Card::Widget(1)]
    );
    assert_eq!(
        cards(&list(&["other/list", "plan/tasks"]), &widgets),
        vec![super::Card::Widget(1), super::Card::Widget(0)]
    );
    assert_eq!(
        cards(&list(&["session"]), &widgets),
        vec![
            super::Card::Session,
            super::Card::Widget(0),
            super::Card::Widget(1)
        ]
    );
    assert_eq!(
        cards(&list(&["session", "session"]), &widgets),
        vec![
            super::Card::Session,
            super::Card::Widget(0),
            super::Card::Widget(1)
        ]
    );
    assert_eq!(
        cards(&list(&["jobs", "session", "changed_files"]), &widgets),
        vec![
            super::Card::Jobs,
            super::Card::Session,
            super::Card::ChangedFiles,
            super::Card::Widget(0),
            super::Card::Widget(1)
        ]
    );
    assert_eq!(
        cards(&list(&["jobs", "jobs", "changed_files"]), &widgets),
        vec![
            super::Card::Jobs,
            super::Card::ChangedFiles,
            super::Card::Widget(0),
            super::Card::Widget(1)
        ]
    );
}

#[test]
fn a_listed_widget_takes_its_listed_place() {
    let widgets = [("plan", "tasks"), ("other", "list")];
    assert_eq!(
        cards(&list(&["other/list"]), &widgets),
        vec![super::Card::Widget(1), super::Card::Widget(0)]
    );
}

#[test]
fn an_unlisted_widget_shows_after_the_listed_cards_in_arrival_order() {
    let widgets = [("plan", "tasks"), ("other", "list")];
    assert_eq!(
        cards(&list(&[]), &widgets),
        vec![super::Card::Widget(0), super::Card::Widget(1)]
    );
    assert_eq!(
        cards(&list(&["other/list"]), &widgets),
        vec![super::Card::Widget(1), super::Card::Widget(0)]
    );
}

#[test]
fn quota_and_unknown_names_place_nothing() {
    let none: [(&str, &str); 0] = [];
    assert!(cards(&list(&["quota"]), &none).is_empty());
    assert!(cards(&list(&["nope"]), &none).is_empty());
    let widgets = [("plan", "tasks")];
    assert_eq!(
        cards(&list(&["quota", "nope", "plan/tasks"]), &widgets),
        vec![super::Card::Widget(0)]
    );
}

#[test]
fn a_listed_widget_that_is_absent_places_nothing() {
    let none: [(&str, &str); 0] = [];
    assert!(cards(&list(&["other/list"]), &none).is_empty());
    let widgets = [("plan", "tasks")];
    assert_eq!(
        cards(&list(&["other/list"]), &widgets),
        vec![super::Card::Widget(0)]
    );
}

#[test]
fn a_name_listed_twice_places_one_card() {
    let widgets = [("plan", "tasks")];
    assert_eq!(
        cards(&list(&["plan/tasks", "plan/tasks"]), &widgets),
        vec![super::Card::Widget(0)]
    );
}

/// Renders `app` on a `width` by `height` screen as text: the whole
/// in-memory screen, trailing spaces trimmed.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

/// Draws only the panel rect of `app`.
fn draw_panel(app: &App) -> Buffer {
    let (buf, targets) = draw_targets(app);
    assert!(targets.is_empty());
    buf
}

/// Draws only the panel rect of `app`, with its click targets.
fn draw_targets(app: &App) -> (Buffer, Vec<crate::mouse::Target>) {
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"));
    let area = Rect::new(0, 0, rect.width, rect.height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw(app, area, &mut buf, &mut targets);
    (buf, targets)
}

#[test]
fn widget_card() {
    let mut app = attached(160, 40);
    fold_widget(&mut app, "plan", "tasks", &["one", "two", "three"]);
    let buf = draw_panel(&app);
    insta::assert_snapshot!("widget_card", super::super::text(&buf));
}

#[test]
fn widget_rows_are_cut_at_the_text_width() {
    let mut app = attached(160, 40);
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"));
    let text = usize::from(rect.width).saturating_sub(3);
    fold_widget(&mut app, "plan", "tasks", &["x".repeat(text + 10).as_str()]);
    let drawn: Vec<String> = rows(&app, rect.width)
        .iter()
        .map(|row| row.line.to_string())
        .collect();
    // The card's edges frame its two text rows.
    assert_eq!(drawn.len(), 4);
    assert!(drawn[1].starts_with("plan · tasks"));
    assert_eq!(crate::format::width(&drawn[2]), text);
    assert!(drawn[2].ends_with('\u{2026}'), "{:?}", drawn[2]);
    assert_eq!(crate::format::width(&drawn[0]), text + 2);
    assert_eq!(crate::format::width(&drawn[3]), text + 2);
}

/// Folds `paths` of `(path, added, removed)` as one completed call.
fn fold_changes(app: &mut App, paths: &[(&str, u64, u64)]) {
    let changes: Vec<serde_json::Value> = paths
        .iter()
        .map(|(path, added, removed)| {
            serde_json::json!({"path": path, "added": added, "removed": removed})
        })
        .collect();
    app.on_line(session_line(
        "tool_call_completed",
        serde_json::json!({"status": "completed", "content": [], "changes": changes}),
    ));
}

/// Folds a started job of `description`.
fn fold_job(app: &mut App, id: &str, description: &str) {
    app.on_line(session_line(
        "job_started",
        serde_json::json!({"job_id": id, "description": description,
            "output_path": "/tmp/out"}),
    ));
}

/// Folds a widget of sixty lines.
fn big_widget(app: &mut App) {
    let lines: Vec<String> = (0..60).map(|n| format!("line {n:02}")).collect();
    app.on_line(session_line(
        "extension_ui",
        serde_json::json!({"extension": "plan", "widget": "tasks", "lines": lines}),
    ));
}

/// A wheel-down over the panel's middle.
fn wheel_down(app: &mut App) {
    use crate::keys::{Mouse, MouseKind};
    app.on_wheel(&Mouse {
        kind: MouseKind::WheelDown,
        col: 140,
        row: 20,
    });
}

#[test]
fn panel_scrolled() {
    let mut app = attached(160, 40);
    big_widget(&mut app);
    wheel_down(&mut app);
    wheel_down(&mut app);
    assert_eq!(app.panel_state().scroll(), 6);
    let (buf, _) = draw_targets(&app);
    insta::assert_snapshot!("panel_scrolled", super::super::text(&buf));
}

#[test]
fn a_scroll_past_the_end_after_a_resize_is_clamped_when_drawn() {
    let mut app = attached(160, 40);
    big_widget(&mut app);
    for _ in 0..8 {
        wheel_down(&mut app);
    }
    assert_eq!(app.panel_state().scroll(), 24);
    app.set_size(160, 60);
    let (buf, _) = draw_targets(&app);
    let shown = super::super::text(&buf);
    assert_eq!(shown.lines().nth(1).unwrap_or_default(), "  line 02");
}

#[test]
fn the_top_five_by_lines_changed_ties_by_path() {
    let mut app = attached(160, 40);
    fold_changes(
        &mut app,
        &[
            ("src/f.rs", 3, 3),
            ("src/e.rs", 4, 2),
            ("src/d.rs", 5, 2),
            ("src/c.rs", 6, 2),
            ("src/b.rs", 7, 2),
            ("src/a.rs", 8, 2),
        ],
    );
    let drawn = texts(&app, 40);
    let files: Vec<&String> = drawn.iter().skip(1).take(5).collect();
    assert_eq!(files.len(), 5);
    for (row, path) in files
        .iter()
        .zip(["src/a.rs", "src/b.rs", "src/c.rs", "src/d.rs", "src/e.rs"])
    {
        assert!(row.starts_with(path), "{row}");
    }
    assert!(drawn[1].ends_with("+8 \u{2212}2"));
    assert!(drawn[5].ends_with("+4 \u{2212}2"));
    assert!(
        drawn
            .iter()
            .any(|row| row == "6 files changed  +33 \u{2212}13")
    );
}

#[test]
fn exactly_five_paths_all_show() {
    let mut app = attached(160, 40);
    fold_changes(
        &mut app,
        &[
            ("src/a.rs", 1, 0),
            ("src/b.rs", 1, 0),
            ("src/c.rs", 1, 0),
            ("src/d.rs", 1, 0),
            ("src/e.rs", 1, 0),
        ],
    );
    let drawn = texts(&app, 40);
    assert_eq!(drawn.len(), 8);
    assert!(
        drawn
            .iter()
            .any(|row| row == "5 files changed  +5 \u{2212}0")
    );
}

#[test]
fn no_changes_no_card() {
    let app = attached(160, 40);
    assert!(rows(&app, 40).is_empty());
}

#[test]
fn no_jobs_no_card() {
    let mut app = attached(160, 40);
    fold_job(&mut app, "j_1", "build");
    app.on_line(session_line(
        "job_completed",
        serde_json::json!({"job_id": "j_1", "status": "completed"}),
    ));
    assert!(rows(&app, 40).is_empty());
}

#[test]
fn the_tools_line_is_a_click_target() {
    use crate::app::panel::Spot;
    let mut app = attached(160, 40);
    app.on_line(session_line(
        "mcp_server_failed",
        serde_json::json!({"server": "relay", "reason": "died",
            "will_restart": false,
            "error": {"code": "mcp_server_unavailable", "message": "died"}}),
    ));
    let (buf, targets) = draw_targets(&app);
    let tools = targets
        .iter()
        .find(|target| target.id == crate::mouse::TargetId::Panel(Spot::Tools))
        .unwrap_or_else(|| panic!("no tools target in {targets:?}"));
    let line: String = (0..buf.area.width)
        .map(|x| buf[(x, tools.rect.y)].symbol())
        .collect::<String>()
        .trim_end()
        .to_owned();
    assert!(line.contains("tools  relay down"), "{line}");
}

#[test]
fn one_job_shows_the_card() {
    let mut app = attached(160, 40);
    fold_job(&mut app, "j_1", "build");
    fold_job(&mut app, "j_2", "test");
    assert_eq!(
        texts(&app, 40),
        vec!["▄".repeat(39), "2 jobs running".to_owned(), "▀".repeat(39)]
    );
}

#[test]
fn the_jobs_line_and_its_rows_are_targets() {
    use crate::app::panel::Spot;
    let mut app = attached(160, 40);
    fold_job(&mut app, "j_1", "build");
    fold_job(&mut app, "j_2", "test");
    let (_, targets) = draw_targets(&app);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].id, crate::mouse::TargetId::Panel(Spot::Jobs));
    assert_eq!(targets[0].rect.x, 2);
    assert_eq!(targets[0].rect.y, 2);
    assert_eq!(
        targets[0].rect.width,
        u16::try_from("2 jobs running".len()).unwrap_or(u16::MAX)
    );
    app.on_click(crate::mouse::TargetId::Panel(Spot::Jobs));
    let drawn = texts(&app, 40);
    assert_eq!(
        drawn,
        vec![
            "▄".repeat(39),
            "2 jobs running".to_owned(),
            "  build".to_owned(),
            "  test".to_owned(),
            "▀".repeat(39)
        ]
    );
    // Each listed job row carries its job's serial.
    let first = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let second = app
        .serial_of_job(&contract::JobId("j_2".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let (_, targets) = draw_targets(&app);
    assert_eq!(
        targets.iter().map(|target| target.id).collect::<Vec<_>>(),
        [
            crate::mouse::TargetId::Panel(Spot::Jobs),
            crate::mouse::TargetId::Panel(Spot::Job(first)),
            crate::mouse::TargetId::Panel(Spot::Job(second)),
        ]
    );
    // The second job row opens the second job.
    app.on_click(crate::mouse::TargetId::Panel(Spot::Job(second)));
    assert!(app.item_open());
    assert_eq!(
        app.item_view().map(|view| view.description),
        Some("test".to_owned())
    );
}

#[test]
fn changed_files_card() {
    let mut app = attached(160, 40);
    fold_changes(
        &mut app,
        &[
            ("src/long/path/to/the/parser.rs", 120, 15),
            ("src/a.rs", 3, 3),
        ],
    );
    let (buf, targets) = draw_targets(&app);
    assert_eq!(
        targets.iter().map(|target| target.id).collect::<Vec<_>>(),
        [
            crate::mouse::TargetId::Panel(crate::app::panel::Spot::File(0)),
            crate::mouse::TargetId::Panel(crate::app::panel::Spot::File(1)),
            crate::mouse::TargetId::Panel(crate::app::panel::Spot::ChangedFiles),
        ]
    );
    insta::assert_snapshot!("changed_files_card", super::super::text(&buf));
}

#[test]
fn jobs_card_collapsed_and_expanded() {
    let mut app = attached(160, 40);
    fold_job(&mut app, "j_1", "build the workspace");
    fold_job(&mut app, "j_2", "test the workspace");
    app.on_click(crate::mouse::TargetId::Panel(crate::app::panel::Spot::Jobs));
    let (buf, _) = draw_targets(&app);
    insta::assert_snapshot!("jobs_card_collapsed_and_expanded", super::super::text(&buf));
}

#[test]
fn each_card_sits_on_its_surface_with_edges() {
    use crate::theme::Role;
    let mut app = attached(100, 30);
    fold_widget(&mut app, "ext", "one", &["hello"]);
    fold_widget(&mut app, "ext", "two", &["world"]);
    let area = Rect::new(60, 0, 40, 30);
    let mut buf = Buffer::empty(area);
    super::draw(&app, area, &mut buf, &mut Vec::new());
    let surface = Role::Surface.color();
    // Two cards of two text rows: top edge, text, text, bottom edge, a
    // blank row between them, all from the panel's second column.
    for (top, title) in [(1, "ext · one"), (6, "ext · two")] {
        for x in 61..100 {
            assert_eq!(buf[(x, top)].fg, surface, "top edge at ({x}, {top})");
            assert_eq!(buf[(x, top)].symbol(), "▄");
            assert_eq!(
                buf[(x, top + 3)].fg,
                surface,
                "bottom edge at ({x}, {})",
                top + 3
            );
            assert_eq!(buf[(x, top + 3)].symbol(), "▀");
        }
        for y in [top + 1, top + 2] {
            for x in 61..100 {
                assert_eq!(buf[(x, y)].bg, surface, "text at ({x}, {y})");
            }
        }
        let row: String = (61..100)
            .map(|x| buf[(x, top + 1)].symbol().to_owned())
            .collect();
        assert!(row.contains(title), "{row:?}");
    }
    // The separator row between cards stays untinted.
    for x in 60..100 {
        assert_ne!(buf[(x, 5)].bg, surface, "separator at ({x}, 5)");
    }
}

/// At the panel floor and at 60, every Session card row fits the card and a
/// row too long for it ends in `…` (`docs/tui.md`, "The panel").
#[test]
fn session_card_rows_are_cut_with_an_ellipsis() {
    let mut app = attached(140, 24);
    full_session(&mut app);
    for width in [30u16, 60] {
        let text = usize::from(width) - 3;
        let rows = texts(&app, width);
        for row in rows
            .iter()
            .filter(|row| !row.contains(['\u{2584}', '\u{2580}']))
        {
            assert!(crate::format::width(row) <= text, "{width}: {row:?}");
        }
        let speed = rows
            .iter()
            .find(|row| row.starts_with("output speed"))
            .unwrap_or_else(|| panic!("{width}: {rows:?}"));
        if width == 30 {
            assert!(speed.ends_with('\u{2026}'), "{speed:?}");
        } else {
            assert!(speed.ends_with("tokens/s"), "{speed:?}");
        }
    }
}

/// `fit_rows` keeps a row at exactly `text` columns, and cuts one a column
/// longer to `text` columns ending in `…`.
#[test]
fn a_row_is_cut_only_past_the_card_width() {
    let text = "turns  12";
    let width = crate::format::width(text);
    let kept = super::fit_rows(vec![super::plain(text.to_owned())], width);
    assert_eq!(kept[0].line.to_string(), text);
    let cut = super::fit_rows(vec![super::plain(text.to_owned())], width - 1);
    assert_eq!(cut[0].line.to_string(), "turns  \u{2026}");
    let one = super::fit_rows(vec![super::plain(text.to_owned())], 1);
    assert_eq!(one[0].line.to_string(), "\u{2026}");
}

#[test]
fn a_cut_row_keeps_its_target_and_tint() {
    let mut row = super::targeted(
        "cost on subscription  $1.10".to_owned(),
        crate::app::panel::Spot::Usage,
    );
    row.tint = Some(crate::view::Role::Surface);
    let cut = super::fit_rows(vec![row], 20);
    assert_eq!(cut.len(), 1);
    assert_eq!(cut[0].spot, Some(crate::app::panel::Spot::Usage));
    assert_eq!(cut[0].tint, Some(crate::view::Role::Surface));
}

/// A row of several spans is cut inside the span that overflows; the spans
/// before it stay whole and the ones after it go.
#[test]
fn a_row_of_spans_is_cut_in_the_overflowing_span() {
    use ratatui::text::{Line, Span};
    let row = super::Row {
        line: Line::from(vec![
            Span::raw("run  "),
            Span::raw("claude-opus"),
            Span::raw("!"),
        ]),
        spot: None,
        tint: None,
        edge: false,
    };
    let cut = super::fit_rows(vec![row], 10);
    assert_eq!(cut[0].line.to_string(), "run  clau\u{2026}");
    assert_eq!(cut[0].line.spans.len(), 2);
}

/// The bar fills in `info`, rests in `rule`, and ends in `attention`'s
/// marker (`docs/tui.md`, "The panel").
#[test]
fn the_context_bar_is_drawn_in_its_roles() {
    use crate::markdown::{Role, style};
    let spans = super::bar_spans(500, 1000, Some(800));
    assert_eq!(spans.len(), 18);
    assert_eq!(spans[0].style, style(Role::Info));
    assert_eq!(spans[9].style, style(Role::Info));
    assert_eq!(spans[10].style, style(Role::Rule));
    assert_eq!(spans[16].style, style(Role::Rule));
    assert_eq!(spans[17].content, "│");
    assert_eq!(spans[17].style, style(Role::Attention));
}
