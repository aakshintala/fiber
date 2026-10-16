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
        "state": "idle", "started": 0, "calls": 0, "since": 0, "spend": spend,
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

/// A waiting `session_status` for another live session than the
/// attached one.
fn other_waiting(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: serde_json::json!({
            "name": "other", "workspace": "/Users/you/work/fiber",
            "project": "-w", "state": "waiting", "started": 0, "calls": 0, "since": 0,
            "waiting": {"request_id": "r_1", "kind": "approval",
                "summary": "shell ls"},
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0,
            "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// An idle `session_status` for another live session than the
/// attached one.
fn other_idle(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: serde_json::json!({
            "name": "other", "workspace": "/Users/you/work/fiber",
            "project": "-w", "state": "idle", "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0,
            "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// Hides the rail as ⌥R does, so the hidden rail's grip is drawn.
fn hide_rail(app: &mut App) {
    use contract::clock::Clock;
    app.on_key(crate::keys::Key::AltR, fakes::clock::FakeClock::new().now());
    assert!(
        app.chrome()
            .layout()
            .is_some_and(|layout| layout.rail.is_none())
    );
}

/// With the rail hidden and sessions waiting, the Session card ends with
/// "! N waiting · hidden · show" in attention with a bold "!", and a
/// click on it carries Spot::Waiting (`docs/tui.md`, "The panel").
#[test]
fn the_session_card_ends_with_the_hidden_rail_line() {
    use crate::app::panel::Spot;
    use crate::markdown::{Role, style};
    use ratatui::style::Modifier;
    let mut app = attached(160, 40);
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::json!(0.0), 0.0),
        None,
    ));
    app.on_line(other_waiting("s_bbbbbbbbbbbbbbbb"));
    app.on_line(other_waiting("s_cccccccccccccccc"));
    hide_rail(&mut app);
    let drawn = rows(&app, 40);
    // The card's edges frame its rows: the last text row is the line.
    assert!(drawn.len() > 2);
    let line = &drawn[drawn.len() - 2];
    assert_eq!(line.line.to_string(), "! 2 waiting · hidden · show");
    assert_eq!(line.spot, Some(Spot::Waiting));
    assert_eq!(line.line.spans.len(), 2);
    assert_eq!(line.line.spans[0].content, "!");
    assert_eq!(
        line.line.spans[0].style,
        style(Role::Attention).add_modifier(Modifier::BOLD)
    );
    assert_eq!(line.line.spans[1].content, " 2 waiting · hidden · show");
    assert_eq!(line.line.spans[1].style, style(Role::Attention));
    // The line ends the card: the first text row is the title, naming
    // the working directory with the permission mode right-aligned.
    assert!(drawn[1].line.to_string().ends_with("auto"));
}

/// With the rail hidden and three live sessions but none waiting,
/// `rail_waiting()` is `Some(0)` and the Session card shows no waiting
/// line: the `> 0` guard keeps the `>=` mutant's bogus "! 0 waiting ·
/// hidden · show" row out (`docs/tui.md`, "The panel").
#[test]
fn hidden_rail_with_no_waiting_sessions_shows_no_line() {
    let mut app = attached(160, 40);
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::json!(0.0), 0.0),
        None,
    ));
    app.on_line(other_idle("s_bbbbbbbbbbbbbbbb"));
    app.on_line(other_idle("s_cccccccccccccccc"));
    hide_rail(&mut app);
    assert_eq!(app.rail_waiting(), Some(0));
    assert!(
        rows(&app, 40)
            .iter()
            .all(|row| !row.line.to_string().contains("hidden · show"))
    );
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
    assert!(
        texts(&app, 40)
            .iter()
            .any(|row| *row == "cache hits".to_owned() + &" ".repeat(24) + "0%")
    );
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
    // Seventeen cells of `▆`, then the marker and the size
    // right-aligned: the text cannot tell fill from track, only the
    // styles do (see the roles test).
    let bar = "\u{2586}".repeat(17) + "\u{2502}" + &" ".repeat(15) + "119";
    assert!(drawn.contains(&bar), "{drawn:?}");
    // The handoff row fits with three of pad, nothing cut.
    assert!(
        drawn
            .iter()
            .any(|row| *row == "handoff at 800         11% of window"),
        "{drawn:?}"
    );
    assert!(
        drawn
            .iter()
            .any(|row| *row == "then a summary, fresh context"),
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
            .any(|row| *row == "test/model".to_owned() + &" ".repeat(22) + "high")
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
    let bar = "\u{2586}".repeat(17) + &" ".repeat(15) + "1.0k";
    assert!(drawn.contains(&bar), "{drawn:?}");
    // With no trigger the share stands alone, left-aligned.
    assert!(
        drawn.iter().any(|row| *row == "100% of window"),
        "{drawn:?}"
    );
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
    let bar = "\u{2586}".repeat(17) + "\u{2502}" + &" ".repeat(15) + "500";
    assert!(drawn.contains(&bar), "{drawn:?}");
    // The handoff row fits with two of pad, nothing cut.
    assert!(
        drawn
            .iter()
            .any(|row| *row == "handoff at 1.0k        50% of window"),
        "{drawn:?}"
    );
}

#[test]
fn the_fill_clamps_at_zero_the_trigger_and_past_it() {
    // Seventeen cells, against the trigger.
    assert_eq!(super::bar_fill(0, 1000, Some(800), 17), 0);
    assert_eq!(super::bar_fill(799, 1000, Some(800), 17), 16);
    assert_eq!(super::bar_fill(800, 1000, Some(800), 17), 17);
    assert_eq!(super::bar_fill(900, 1000, Some(800), 17), 17);
    // Eight cells, against the trigger.
    assert_eq!(super::bar_fill(0, 1000, Some(800), 8), 0);
    assert_eq!(super::bar_fill(799, 1000, Some(800), 8), 7);
    assert_eq!(super::bar_fill(800, 1000, Some(800), 8), 8);
    assert_eq!(super::bar_fill(900, 1000, Some(800), 8), 8);
    // Against the window with no trigger.
    assert_eq!(super::bar_fill(500, 1000, None, 17), 8);
    assert_eq!(super::bar_fill(268_000, 400_000, Some(400_000), 8), 5);
    // A zero goal never divides: empty, not a panic.
    assert_eq!(super::bar_fill(5, 0, None, 17), 0);
    assert_eq!(super::bar_fill(5, 1000, Some(0), 17), 0);
    assert_eq!(super::bar_fill(5, 1000, Some(0), 8), 0);
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
        // The cost row reads dim "cost" with its figure right-aligned;
        // the subscription row keeps its prose label.
        assert_eq!(
            drawn
                .iter()
                .any(|row| row.starts_with("cost ") && !row.starts_with("cost on")),
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
            .any(|row| *row == "cost".to_owned() + &" ".repeat(27) + "$0.01")
    );
    let mut app = attached(160, 40);
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::Value::Null, 0.0),
        None,
    ));
    assert!(
        texts(&app, 40)
            .iter()
            .any(|row| *row == "cost".to_owned() + &" ".repeat(25) + "unknown")
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
        (
            serde_json::json!(0.0),
            "cost".to_owned() + &" ".repeat(18) + "$0.00 of $5.00",
        ),
        (
            serde_json::json!(1.25),
            "cost".to_owned() + &" ".repeat(18) + "$1.25 of $5.00",
        ),
        (
            serde_json::Value::Null,
            "cost".to_owned() + &" ".repeat(16) + "unknown of $5.00",
        ),
    ] {
        let mut app = attached(160, 40);
        app.on_line(budget_preamble(5.0));
        app.on_line(status_line(spend(1, 0, 0, 2, cost, 0.0), None));
        assert!(texts(&app, 40).contains(&row), "{row}");
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
            .any(|row| *row == "cost".to_owned() + &" ".repeat(27) + "$1.25")
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
        drawn
            .iter()
            .any(|row| row.ends_with("$1.00 of $5.00") && row.starts_with("cost ")),
        "{drawn:?}"
    );
    assert!(
        drawn
            .iter()
            .any(|row| row.ends_with("$9.00") && row.starts_with("cost on subscription")),
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
        drawn
            .iter()
            .any(|row| row.ends_with("$0.00 of $5.00") && row.starts_with("cost ")),
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
    assert!(
        texts(&app, 40)
            .iter()
            .any(|row| *row == "turns".to_owned() + &" ".repeat(30) + "1")
    );
}

#[test]
fn the_model_falls_back_to_the_status() {
    let mut app = attached(160, 40);
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::json!(0.0), 0.0),
        None,
    ));
    assert!(texts(&app, 40).iter().any(|row| *row == "test/model"));
    app.on_line(preamble_line(Some(800)));
    assert!(
        texts(&app, 40)
            .iter()
            .any(|row| *row == "test/model".to_owned() + &" ".repeat(22) + "high")
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
            TargetId::Panel(Spot::Model),
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
    let text = usize::from(rect.width).saturating_sub(4);
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
    assert_eq!(crate::format::width(&drawn[0]), text + 3);
    assert_eq!(crate::format::width(&drawn[3]), text + 3);
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
    assert_eq!(shown.lines().nth(1).unwrap_or_default(), "   line 02");
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
    // The edge, the title, then the five paths.
    let files: Vec<&String> = drawn.iter().skip(2).take(5).collect();
    assert_eq!(files.len(), 5);
    for (row, path) in files
        .iter()
        .zip(["src/a.rs", "src/b.rs", "src/c.rs", "src/d.rs", "src/e.rs"])
    {
        assert!(row.starts_with(path), "{row}");
    }
    assert!(drawn[2].ends_with("+8 \u{2212}2"));
    assert!(drawn[6].ends_with("+4 \u{2212}2"));
    // Six files: five paths, then dim "… 1 more" with the totals over
    // every path right-aligned.
    let last = drawn.get(drawn.len() - 2).expect("a last row");
    assert_eq!(
        *last,
        "\u{2026} 1 more".to_owned() + &" ".repeat(21) + "+33 \u{2212}13"
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
    assert_eq!(drawn.len(), 9);
    // With five files or fewer the last row holds the totals alone,
    // right-aligned.
    let last = drawn.get(drawn.len() - 2).expect("a last row");
    assert_eq!(*last, " ".repeat(31) + "+5 \u{2212}0");
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
    assert!(line.contains("tools"), "{line}");
    assert!(line.contains("\u{2717} relay down"), "{line}");
}

#[test]
fn one_job_shows_the_card() {
    let mut app = attached(160, 40);
    fold_job(&mut app, "j_1", "build");
    fold_job(&mut app, "j_2", "test");
    assert_eq!(
        texts(&app, 40),
        vec![
            "\u{2584}".repeat(39),
            "Jobs                     2 running \u{25b8}".to_owned(),
            "\u{2580}".repeat(39)
        ]
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
    assert_eq!(targets[0].rect.x, 3);
    assert_eq!(targets[0].rect.y, 2);
    assert_eq!(targets[0].rect.width, 30);
    app.on_click(crate::mouse::TargetId::Panel(Spot::Jobs));
    let drawn = texts(&app, 40);
    assert_eq!(
        drawn,
        vec![
            "\u{2584}".repeat(39),
            "Jobs                     2 running \u{25b8}".to_owned(),
            "  build".to_owned(),
            "  test".to_owned(),
            "\u{2580}".repeat(39)
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
        let text = usize::from(width) - 4;
        let rows = texts(&app, width);
        for row in rows
            .iter()
            .filter(|row| !row.contains(['\u{2584}', '\u{2580}']))
        {
            assert!(crate::format::width(row) <= text, "{width}: {row:?}");
        }
        // The speed row is cut mid-label at the floor and whole at 60.
        let speed = rows
            .iter()
            .find(|row| row.contains("250 tokens/s"))
            .unwrap_or_else(|| panic!("{width}: {rows:?}"));
        if width == 30 {
            assert_eq!(*speed, "speed, last \u{2026} 250 tokens/s");
        } else {
            assert_eq!(
                *speed,
                "speed, last reply".to_owned() + &" ".repeat(27) + "250 tokens/s"
            );
        }
    }
}

/// `fit_rows` keeps a row at exactly `text` columns, and cuts one a column
/// longer to `text` columns ending in `…`.
#[test]
fn a_row_is_cut_only_past_the_card_width() {
    let text = "turns  12";
    let width = crate::format::width(text);
    let kept = super::fit_rows(vec![plain(text.to_owned())], width);
    assert_eq!(kept[0].line.to_string(), text);
    let cut = super::fit_rows(vec![plain(text.to_owned())], width - 1);
    assert_eq!(cut[0].line.to_string(), "turns  \u{2026}");
    let one = super::fit_rows(vec![plain(text.to_owned())], 1);
    assert_eq!(one[0].line.to_string(), "\u{2026}");
}

#[test]
fn a_cut_row_keeps_its_target_and_tint() {
    let mut row = targeted(
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
    let spans = super::bar_spans(500, 1000, Some(800), 17);
    // Ten of 17 cells fill: info over a rule track of the same cell,
    // then the marker in attention.
    assert_eq!(spans.len(), 18);
    assert!(spans.iter().take(10).all(|span| span.content == "\u{2586}"));
    assert_eq!(spans[0].style, style(Role::Info));
    assert_eq!(spans[9].style, style(Role::Info));
    assert_eq!(spans[10].style, style(Role::Rule));
    assert_eq!(spans[16].style, style(Role::Rule));
    assert_eq!(spans[16].content, "\u{2586}");
    assert_eq!(spans[17].content, "\u{2502}");
    assert_eq!(spans[17].style, style(Role::Attention));
    // The eight-cell narrow bar shares the fill: five of eight.
    assert_eq!(super::bar_fill(268_000, 400_000, Some(400_000), 8), 5);
}

/// The panel rect drawn and painted without colour, with its targets.
fn no_color_drawn(app: &App) -> (Buffer, Vec<crate::mouse::Target>) {
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"));
    let area = Rect::new(0, 0, rect.width, rect.height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw(app, area, &mut buf, &mut targets);
    let (look, notice) = crate::look::Look::new(crate::look::ThemeSetting::Dark, &|name: &str| {
        (name == "NO_COLOR").then(|| "1".to_owned())
    });
    assert_eq!(notice, None);
    look.paint(&mut buf);
    (buf, targets)
}

#[test]
fn no_color_boxes_a_sidebar_card_and_keeps_targets_inside() {
    use ratatui::style::{Color, Modifier};
    let mut app = attached(160, 40);
    fold_widget(&mut app, "ext", "one", &["hello"]);
    let (buf, targets) = no_color_drawn(&app);
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .expect("a panel rect");
    // The widget card: its top edge on row 1, two text rows, its bottom
    // edge, from the panel's second column.
    let (left, right) = (1, rect.width.saturating_sub(1));
    for x in left..=right {
        let top = &buf[(x, 1)];
        assert_eq!(
            top.symbol(),
            if x == left {
                "┌"
            } else if x == right {
                "┐"
            } else {
                "─"
            }
        );
        assert_eq!((top.fg, top.bg), (Color::Reset, Color::Reset));
        assert_eq!(top.modifier, Modifier::DIM);
        let bottom = &buf[(x, 4)];
        assert_eq!(
            bottom.symbol(),
            if x == left {
                "└"
            } else if x == right {
                "┘"
            } else {
                "─"
            }
        );
        assert_eq!(bottom.modifier, Modifier::DIM);
    }
    // Each text row sits between │ sides with its text readable inside.
    for y in 2..=3 {
        assert_eq!(buf[(left, y)].symbol(), "│");
        assert_eq!(buf[(right, y)].symbol(), "│");
        assert_eq!(buf[(left, y)].modifier, Modifier::DIM);
    }
    let title: String = ((left + 1)..right).map(|x| buf[(x, 2)].symbol()).collect();
    assert!(title.contains("ext · one"), "{title:?}");
    let body: String = ((left + 1)..right).map(|x| buf[(x, 3)].symbol()).collect();
    assert!(body.contains("hello"), "{body:?}");
    // No target reaches a border column.
    for target in &targets {
        assert!(target.rect.x > left, "{target:?}");
        assert!(target.rect.right() <= right, "{target:?}");
    }
    // Only the widget card draws, with no delegates span: the box keeps
    // the row indices the delegates span counts on.
    let (rows, span) = super::rows_and_delegates(&app, rect.width);
    assert_eq!(rows.len(), 4);
    assert_eq!(span, None);
}

/// A local targeted row: the production helper went with the old totals
/// row, and only these tests need one.
fn targeted(text: String, spot: crate::app::panel::Spot) -> super::Row {
    super::Row {
        line: ratatui::text::Line::raw(text),
        spot: Some(spot),
        tint: None,
        edge: false,
    }
}

/// A local plain row: the production helper went with the directory
/// row, and only these tests need one.
fn plain(text: String) -> super::Row {
    super::Row {
        line: ratatui::text::Line::raw(text),
        spot: None,
        tint: None,
        edge: false,
    }
}

/// Draws `app`'s panel at `width` columns: the buffer, row by row.
fn draw_at(app: &App, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    super::draw(app, area, &mut buf, &mut Vec::new());
    buf
}

/// An app with only a status: the Session card draws its title and the
/// model row the status names.
fn status_only() -> App {
    let mut app = attached(160, 40);
    app.on_line(status_line(
        spend(1, 0, 0, 2, serde_json::json!(0.0), 0.0),
        None,
    ));
    app
}

/// The Session card's title names the working directory in bold, with
/// the permission mode right-aligned in attention (`docs/tui.md`, "The
/// panel"): at the 34-column panel and at the 30-column floor.
#[test]
fn the_session_title_names_the_workspace_with_auto_right_aligned() {
    use crate::markdown::{Role, style};
    use ratatui::style::Modifier;
    for (width, title) in [
        (34u16, "/Users/you/work/fiber     auto"),
        (30u16, "/Users/you/work/fiber auto"),
    ] {
        let app = status_only();
        let drawn = rows(&app, width);
        // The top edge, the title, the model, tokens and cache rows,
        // the bottom edge.
        assert_eq!(drawn.len(), 6, "{width}");
        assert!(drawn[0].edge);
        assert_eq!(drawn[1].line.to_string(), title, "{width}");
        // A title row opens no view.
        assert_eq!(drawn[1].spot, None);
        assert_eq!(drawn[1].line.spans.len(), 3);
        assert_eq!(drawn[1].line.spans[0].content, "/Users/you/work/fiber");
        assert_eq!(
            drawn[1].line.spans[0].style,
            style(Role::Text).add_modifier(Modifier::BOLD)
        );
        assert_eq!(drawn[1].line.spans[2].content, "auto");
        assert_eq!(drawn[1].line.spans[2].style, style(Role::Attention));
    }
}

/// The slab starts at the panel's second column and its text two columns
/// past it, ending one column before the slab's right edge
/// (`docs/tui.md`, "The panel"): 30 text columns at 34.
#[test]
fn card_text_starts_two_columns_past_the_slab() {
    let app = status_only();
    let buf = draw_at(&app, 34, 20);
    // The slab covers columns 1 to 33; the title reads at 3 to 32.
    let title: String = (3..33).map(|x| buf[(x, 2)].symbol()).collect();
    assert_eq!(title.trim_end(), "/Users/you/work/fiber     auto");
    // The right value ends one column before the slab's right edge,
    // which keeps the tint but no text.
    assert_eq!(buf[(32, 2)].symbol(), "o");
    assert_eq!(buf[(33, 2)].symbol(), " ");
    assert_eq!(buf[(33, 2)].bg, crate::theme::Role::Surface.color());
}

/// Each card sits on its surface tint with half-block edges in the
/// surface colour over the panel colour (`docs/tui.md`, "Look").
#[test]
fn each_cards_edges_draw_in_surface() {
    let app = status_only();
    let buf = draw_at(&app, 34, 20);
    let surface = crate::theme::Role::Surface.color();
    // The top edge on row 1, four text rows, the bottom edge on row 6.
    for x in 1..34 {
        assert_eq!(buf[(x, 1)].symbol(), "\u{2584}", "top at {x}");
        assert_eq!(buf[(x, 1)].fg, surface, "top at {x}");
        assert_eq!(buf[(x, 6)].symbol(), "\u{2580}", "bottom at {x}");
        assert_eq!(buf[(x, 6)].fg, surface, "bottom at {x}");
    }
    for y in 2..=5 {
        for x in 1..34 {
            assert_eq!(buf[(x, y)].bg, surface, "text at ({x}, {y})");
        }
    }
}

/// The blank row after a card shows the panel's tint, not the card's:
/// the rows report no tint for it (`docs/tui.md`, "The panel").
#[test]
fn the_blank_row_after_a_card_shows_no_card_tint() {
    let mut app = status_only();
    fold_widget(&mut app, "ext", "one", &["hello"]);
    let drawn = rows(&app, 34);
    // The Session card's six rows, the blank row, the widget's four.
    assert_eq!(drawn.len(), 11);
    assert_eq!(drawn[6].line.to_string(), "");
    assert_eq!(drawn[6].tint, None);
    assert_eq!(drawn[6].spot, None);
    assert!(!drawn[6].edge);
    let buf = draw_at(&app, 34, 20);
    let surface = crate::theme::Role::Surface.color();
    assert_ne!(buf[(3, 7)].bg, surface);
}

/// A card with no content rows draws nothing: no edge, no title and no
/// blank row (`docs/tui.md`, "The panel").
#[test]
fn cards_without_content_draw_nothing() {
    let app = attached(160, 40);
    assert!(rows(&app, 40).is_empty());
    // Without a status the session content draws with no title, as the
    // base drew no directory row: the card opens on the model row.
    let mut app = attached(160, 40);
    app.on_line(preamble_line(Some(800)));
    let drawn = rows(&app, 40);
    assert_eq!(drawn.len(), 3);
    assert!(drawn[0].edge);
    assert_eq!(
        drawn[1].line.to_string(),
        "test/model".to_owned() + &" ".repeat(22) + "high"
    );
    assert!(
        drawn
            .iter()
            .all(|row| !row.line.to_string().contains("auto"))
    );
}

/// A session with every row present, on `main`, for the width-34 pin.
fn full_session_on_main(app: &mut App) {
    app.on_line(preamble_line(Some(800)));
    app.on_line(session_line(
        "session_status",
        serde_json::json!({
            "name": "work", "workspace": "/Users/you/work/fiber",
            "project": "-w", "state": "idle", "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 900, "cache_read": 100,
                "cache_write": {"5m": 50}, "output": 200},
                "cost": 0.41, "subscription_cost": 1.10},
            "model": "test/model", "delegates": 0, "jobs": 0,
            "clients": 0, "git": {"branch": "main"},
            "context": {"tokens": 500, "window": 1000},
        }),
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

/// The Session card's rows at width 34: the title, "git" and the branch,
/// the model and thinking, the 17-cell bar with its handoff rows, and
/// every figure with its label dim and its value right-aligned
/// (`docs/tui.md`, "The panel").
#[test]
fn session_card_rows_match_the_doc_at_width_34() {
    use crate::app::panel::Spot;
    use crate::markdown::{Role, style};
    use ratatui::style::Modifier;
    let mut app = attached(160, 40);
    full_session_on_main(&mut app);
    let drawn = rows(&app, 34);
    let lines: Vec<String> = drawn.iter().map(|row| row.line.to_string()).collect();
    let bar = "\u{2586}".repeat(10) + &"\u{2586}".repeat(7) + "\u{2502}" + &" ".repeat(9) + "500";
    assert_eq!(
        lines,
        vec![
            "\u{2584}".repeat(33),
            "/Users/you/work/fiber     auto".to_owned(),
            "git  main".to_owned(),
            "test/model".to_owned() + &" ".repeat(16) + "high",
            bar,
            "handoff at 800   50% of window".to_owned(),
            "then a summary, fresh context".to_owned(),
            "tokens in / out".to_owned() + &" ".repeat(5) + "1.1k / 200",
            "cache hits".to_owned() + &" ".repeat(18) + "9%",
            "cost".to_owned() + &" ".repeat(21) + "$0.41",
            "cost on subscription     $1.10".to_owned(),
            "speed, last reply 250 tokens/s".to_owned(),
            "turns".to_owned() + &" ".repeat(24) + "1",
            "tools".to_owned() + &" ".repeat(13) + "\u{2717} relay down",
            "\u{2580}".repeat(33),
        ]
    );
    let spots: Vec<Option<Spot>> = drawn.iter().map(|row| row.spot).collect();
    assert_eq!(
        spots,
        vec![
            None,
            None,
            Some(Spot::Branch),
            Some(Spot::Model),
            Some(Spot::Context),
            Some(Spot::Context),
            Some(Spot::Context),
            Some(Spot::Usage),
            Some(Spot::Usage),
            Some(Spot::Usage),
            Some(Spot::Usage),
            None,
            None,
            Some(Spot::Tools),
            None,
        ]
    );
    // "git" dim, the branch in secondary.
    assert_eq!(drawn[2].line.spans.len(), 3);
    assert_eq!(drawn[2].line.spans[0].content, "git");
    assert_eq!(drawn[2].line.spans[0].style, style(Role::Muted));
    assert_eq!(drawn[2].line.spans[2].content, "main");
    assert_eq!(drawn[2].line.spans[2].style, style(Role::Secondary));
    // The model in accent, the thinking level in attention.
    assert_eq!(drawn[3].line.spans[0].content, "test/model");
    assert_eq!(drawn[3].line.spans[0].style, style(Role::Accent));
    assert_eq!(drawn[3].line.spans[2].content, "high");
    assert_eq!(drawn[3].line.spans[2].style, style(Role::Attention));
    // A figure row: its label dim, its value in the text colour.
    assert_eq!(drawn[7].line.spans[0].content, "tokens in / out");
    assert_eq!(drawn[7].line.spans[0].style, style(Role::Muted));
    assert_eq!(drawn[7].line.spans[2].content, "1.1k / 200");
    assert_eq!(drawn[7].line.spans[2].style, style(Role::Text));
    // The title keeps the card's name bold with the figure right-aligned.
    assert_eq!(
        drawn[1].line.spans[0].style,
        style(Role::Text).add_modifier(Modifier::BOLD)
    );
}

/// No row reads the empty track: the bar rests in `▆` in `rule`, never
/// `░`, anywhere in the panel (`docs/tui.md`, "The panel").
#[test]
fn no_row_reads_the_empty_track() {
    let mut app = attached(140, 24);
    full_session(&mut app);
    for width in [34u16, 30] {
        for row in rows(&app, width) {
            assert!(
                !row.line.to_string().contains('\u{2591}'),
                "{width}: {}",
                row.line
            );
        }
    }
}

/// Folds a down MCP `server` into `app`.
fn fail_server(app: &mut App, server: &str) {
    app.on_line(session_line(
        "mcp_server_failed",
        serde_json::json!({"server": server, "reason": "died",
            "will_restart": false,
            "error": {"code": "mcp_server_unavailable", "message": "died"}}),
    ));
}

/// The tools row shows only while a server is down: dim "tools" with
/// one `✗ <server> down` in error per server, cut from the left when
/// too long for the card (`docs/tui.md`, "The panel").
#[test]
fn the_tools_row_shows_only_while_a_server_is_down() {
    use crate::app::panel::Spot;
    use crate::markdown::{Role, style};
    let app = status_only();
    assert!(
        rows(&app, 40)
            .iter()
            .all(|row| !row.line.to_string().contains('\u{2717}'))
    );
    let mut app = status_only();
    fail_server(&mut app, "relay");
    let row = rows(&app, 40)
        .into_iter()
        .find(|row| row.spot == Some(Spot::Tools))
        .expect("a tools row");
    assert_eq!(
        row.line.to_string(),
        "tools".to_owned() + &" ".repeat(19) + "\u{2717} relay down"
    );
    assert_eq!(row.line.spans.len(), 3);
    assert_eq!(row.line.spans[0].content, "tools");
    assert_eq!(row.line.spans[0].style, style(Role::Muted));
    assert_eq!(row.line.spans[2].content, "\u{2717} relay down");
    assert_eq!(row.line.spans[2].style, style(Role::Error));
    // Two names overflow the card: the value is cut from the left and
    // keeps the error style past the cut.
    let mut app = status_only();
    fail_server(&mut app, "alpha");
    fail_server(&mut app, "beta-server");
    let row = rows(&app, 40)
        .into_iter()
        .find(|row| row.spot == Some(Spot::Tools))
        .expect("a tools row");
    let text = row.line.to_string();
    assert_eq!(crate::format::width(&text), 36);
    assert!(text.starts_with("tools \u{2026}"), "{text}");
    assert!(text.ends_with("\u{2717} beta-server down"), "{text}");
    assert_eq!(
        row.line.spans.last().map(|span| span.style),
        Some(style(Role::Error))
    );
}

/// Seven files: five paths, then dim "… 2 more" with the totals over
/// every path, and the title counts every file dim (`docs/tui.md`, "The
/// panel").
#[test]
fn seven_files_count_two_more() {
    use crate::app::panel::Spot;
    use crate::markdown::{Role, style};
    use ratatui::style::Modifier;
    let mut app = attached(160, 40);
    fold_changes(
        &mut app,
        &[
            ("src/a.rs", 10, 1),
            ("src/b.rs", 10, 1),
            ("src/c.rs", 10, 1),
            ("src/d.rs", 10, 1),
            ("src/e.rs", 10, 1),
            ("src/f.rs", 10, 1),
            ("src/g.rs", 10, 1),
        ],
    );
    let drawn = rows(&app, 40);
    // The edge, the title, five paths, the last row, the edge.
    assert_eq!(drawn.len(), 9);
    let title = &drawn[1];
    assert_eq!(
        title.line.to_string(),
        "Changed files".to_owned() + &" ".repeat(22) + "7"
    );
    assert_eq!(title.spot, None);
    assert_eq!(title.line.spans.len(), 3);
    assert_eq!(title.line.spans[0].content, "Changed files");
    assert_eq!(
        title.line.spans[0].style,
        style(Role::Text).add_modifier(Modifier::BOLD)
    );
    assert_eq!(title.line.spans[2].content, "7");
    assert_eq!(title.line.spans[2].style, style(Role::Muted));
    let last = &drawn[7];
    assert_eq!(
        last.line.to_string(),
        "\u{2026} 2 more".to_owned() + &" ".repeat(22) + "+70 \u{2212}7"
    );
    assert_eq!(last.spot, Some(Spot::ChangedFiles));
    assert_eq!(last.line.spans.len(), 5);
    assert_eq!(last.line.spans[0].content, "\u{2026} 2 more");
    assert_eq!(last.line.spans[0].style, style(Role::Muted));
    assert_eq!(last.line.spans[2].content, "+70");
    assert_eq!(last.line.spans[2].style, style(Role::Added));
    assert_eq!(last.line.spans[4].content, "\u{2212}7");
    assert_eq!(last.line.spans[4].style, style(Role::Removed));
}

/// The Jobs title counts the running jobs in attention with a dim `▸`,
/// and keeps the toggle target: closed draws the title alone, open adds
/// one row per job (`docs/tui.md`, "The panel").
#[test]
fn the_jobs_title_counts_running_with_a_toggle() {
    use crate::app::panel::Spot;
    use crate::markdown::{Role, style};
    let mut app = attached(160, 40);
    fold_job(&mut app, "j_1", "build");
    fold_job(&mut app, "j_2", "test");
    let drawn = rows(&app, 40);
    assert_eq!(drawn.len(), 3);
    let title = &drawn[1];
    assert_eq!(
        title.line.to_string(),
        "Jobs".to_owned() + &" ".repeat(21) + "2 running \u{25b8}"
    );
    assert_eq!(title.spot, Some(Spot::Jobs));
    assert_eq!(title.line.spans.len(), 4);
    assert_eq!(title.line.spans[2].content, "2 running");
    assert_eq!(title.line.spans[2].style, style(Role::Attention));
    assert_eq!(title.line.spans[3].content, " \u{25b8}");
    assert_eq!(title.line.spans[3].style, style(Role::Muted));
    // Open: the title, then one row per job.
    app.on_click(crate::mouse::TargetId::Panel(Spot::Jobs));
    let drawn = rows(&app, 40);
    assert_eq!(drawn.len(), 5);
    assert_eq!(drawn[2].line.to_string(), "  build");
    assert_eq!(drawn[3].line.to_string(), "  test");
}
