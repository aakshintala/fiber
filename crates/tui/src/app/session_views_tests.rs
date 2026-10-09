//! Tests for `/usage` on the app: its dispatch, keys, clicks and session fold
//! (`docs/tui.md`, "Swapped views").

use std::path::PathBuf;
use std::sync::Arc;

use crate::app::panel::Spot as PanelSpot;
use crate::app::{App, Effect, SessionView};
use crate::changed_files_view::{diff_command, ranked};
use crate::home::Launch;
use crate::keys::{Edit, Key};
use crate::link::Line;
use crate::mouse::{Target, TargetId};
use crate::swapped::Spot as ViewSpot;
use contract::clock::Clock;
use contract::{ActionId, SessionId, TurnId};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";

fn now() -> std::time::Instant {
    fakes::clock::FakeClock::new().now()
}

fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    app.attach(SessionId(SESSION.to_owned()));
    app
}

fn home_attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        panel_cards: vec!["session".to_owned()],
        ..Default::default()
    });
    app.attach(SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app
}

fn session_line(session: &str, kind: &str, turn: Option<&str>, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: turn.map(|id| TurnId(id.to_owned())),
        action_id: Some(ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

fn usage_line(session: &str, generation: &str, model: &str, turn: Option<&str>) -> Line {
    session_line(
        session,
        "usage_recorded",
        turn,
        json!({
            "generation_id": generation, "model": model,
            "tokens": {"input": 12, "cache_read": 3,
                "cache_write": {"5m": 4}, "output": 5},
            "input_bytes": 0, "cost": 0.25,
        }),
    )
}

fn turn_started(turn: &str) -> Line {
    session_line(
        SESSION,
        "turn_started",
        Some(turn),
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "work"}]}]}),
    )
}

fn screen(app: &App, width: u16, height: u16) -> (String, Vec<Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buffer, None);
    let text = (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");
    (text, targets)
}

fn slash_usage(app: &mut App) -> Effect {
    app.on_edit(Edit::Paste("/usage".to_owned()));
    app.on_key(Key::Enter, now())
}

fn open_usage(app: &mut App) {
    assert_eq!(app.open_session_view(SessionView::Usage), Effect::None);
}

fn with_settings_seam(app: &mut App) {
    let seam: Arc<dyn crate::Configure> = Arc::new(crate::configure_fake::Fake::new(Vec::new()));
    app.set_configure(Some(seam));
}

#[test]
fn slash_usage_opens_the_view_and_clears_the_draft() {
    let mut app = attached(80, 24);
    app.on_line(turn_started("t1"));
    app.on_line(usage_line(SESSION, "g1", "model/m", Some("t1")));
    assert_eq!(slash_usage(&mut app), Effect::None);
    assert!(app.session_view_open());
    assert!(app.input().is_empty());
    let shown = screen(&app, 80, 24).0;
    assert!(shown.contains("Usage"));
    assert!(shown.contains("turn 1"));
    assert!(shown.contains("model/m"));
}

#[test]
fn slash_usage_without_a_session_shows_a_notice_and_opens_nothing() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(80, 24);
    assert_eq!(slash_usage(&mut app), Effect::None);
    assert!(!app.session_view_open());
    assert!(screen(&app, 80, 24).0.contains("No session on screen."));
    assert_eq!(app.notices.newest(), Some("No session on screen."));
}

#[test]
fn escape_closes_the_usage_view_and_returns_to_the_conversation() {
    let mut app = attached(80, 24);
    open_usage(&mut app);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.session_view_open());
    assert!(!screen(&app, 80, 24).0.contains("Usage"));
}

#[test]
fn control_c_reaches_the_quit_flow_without_closing_the_view() {
    let mut app = attached(80, 24);
    open_usage(&mut app);
    assert_eq!(app.on_key(Key::CtrlC, now()), Effect::None);
    assert!(app.hint());
    assert!(app.session_view_open());
    assert!(screen(&app, 80, 24).0.contains("Usage"));
}

#[test]
fn an_input_only_action_does_not_run_in_the_open_view() {
    let mut app = attached(80, 24);
    open_usage(&mut app);
    let stroke = crate::stroke::Stroke::parse("ctrl+n").expect("valid stroke");
    app.on_press(stroke, now());
    assert!(app.session_view_open());
    assert_eq!(app.session(), Some(&SessionId(SESSION.to_owned())));
    assert!(screen(&app, 80, 24).0.contains("Usage"));
}

#[test]
fn escape_in_the_quit_question_does_not_close_the_view() {
    let mut app = home_attached(80, 24);
    app.on_line(session_line(
        OTHER,
        "session_status",
        None,
        json!({
            "name": "other", "workspace": "/w", "project": "-w", "state": "streaming",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2}, "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    ));
    open_usage(&mut app);
    assert_eq!(app.quit(), Effect::None);
    assert!(app.quit_open());
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.quit_open());
    assert!(app.session_view_open());
    assert!(screen(&app, 80, 24).0.contains("Usage"));
}

#[test]
fn down_and_pages_move_the_list_and_page_down_stops_at_the_last_row() {
    let mut app = attached(80, 24);
    for at in 0..30 {
        app.on_line(usage_line(
            SESSION,
            &format!("g{at}"),
            &format!("model/{at:02}"),
            None,
        ));
    }
    open_usage(&mut app);
    assert_eq!(app.session_view_screen(0).unwrap().list.selected(), 0);
    app.on_key(Key::Down, now());
    assert_eq!(app.session_view_screen(0).unwrap().list.selected(), 1);
    app.on_key(Key::PageDown, now());
    assert!(app.session_view_screen(0).unwrap().list.selected() > 1);
    let before = app.session_view_screen(0).unwrap();
    let page = crate::swapped::rows_height(&before, app.conversation_height())
        .saturating_sub(1)
        .max(1);
    let remaining = before
        .rows
        .len()
        .saturating_sub(1)
        .saturating_sub(before.list.selected());
    let pages_past_end = remaining.div_ceil(page).saturating_add(2);
    for _ in 0..pages_past_end {
        app.on_key(Key::PageDown, now());
    }
    let frame = app.session_view_screen(0).unwrap();
    let last = frame.rows.len().saturating_sub(1);
    assert_eq!(frame.list.selected(), last);
    app.on_key(Key::PageDown, now());
    assert_eq!(app.session_view_screen(0).unwrap().list.selected(), last);
    assert!(screen(&app, 80, 24).0.contains("Usage"));
}

#[test]
fn the_close_target_closes_the_view() {
    let mut app = attached(80, 24);
    open_usage(&mut app);
    let (shown, targets) = screen(&app, 80, 24);
    assert!(shown.contains("Usage"));
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::View(ViewSpot::Close))
    );
    assert_eq!(app.on_click(TargetId::View(ViewSpot::Close)), Effect::None);
    assert!(!app.session_view_open());
    assert!(!screen(&app, 80, 24).0.contains("Usage"));
}

#[test]
fn clicking_a_row_selects_it() {
    let mut app = attached(80, 24);
    app.on_line(usage_line(SESSION, "g1", "model/m", None));
    open_usage(&mut app);
    let (_, targets) = screen(&app, 80, 24);
    assert!(
        targets
            .iter()
            .any(|target| { target.id == TargetId::View(ViewSpot::Row(1)) })
    );
    app.on_click(TargetId::View(ViewSpot::Row(1)));
    assert_eq!(app.session_view_screen(80).unwrap().list.selected(), 1);
    assert!(screen(&app, 80, 24).0.contains("model/m"));
}

#[test]
fn editing_a_draft_does_not_pass_through_the_open_view() {
    let mut app = attached(80, 24);
    app.on_edit(Edit::Paste("keep this draft".to_owned()));
    open_usage(&mut app);
    app.on_key(Key::Char('x'), now());
    assert_eq!(app.input().expand(), "keep this draft");
    assert!(screen(&app, 80, 24).0.contains("Usage"));
}

#[test]
fn opening_usage_closes_an_open_settings_view() {
    let mut app = attached(80, 24);
    with_settings_seam(&mut app);
    app.open_config_view(crate::app::ConfigView::Settings);
    assert!(app.config_view_open());
    assert!(screen(&app, 80, 24).0.contains("Settings"));
    open_usage(&mut app);
    assert!(!app.config_view_open());
    assert!(app.session_view_open());
    assert!(screen(&app, 80, 24).0.contains("Usage"));
}

#[test]
fn opening_usage_closes_the_model_picker() {
    let mut app = attached(80, 24);
    app.open_model_picker(crate::model_picker::Mode::Choose);
    assert!(app.model_picker_open());
    open_usage(&mut app);
    assert!(!app.model_picker_open());
    assert!(app.session_view_open());
    assert!(screen(&app, 80, 24).0.contains("Usage"));
}

#[test]
fn opening_the_model_picker_closes_usage() {
    let mut app = attached(80, 24);
    open_usage(&mut app);
    app.open_model_picker(crate::model_picker::Mode::Choose);
    assert!(!app.session_view_open());
    assert!(app.model_picker_open());
}

#[test]
fn opening_settings_closes_usage() {
    let mut app = attached(80, 24);
    with_settings_seam(&mut app);
    open_usage(&mut app);
    app.open_config_view(crate::app::ConfigView::Settings);
    assert!(!app.session_view_open());
    assert!(app.config_view_open());
    assert!(screen(&app, 80, 24).0.contains("Settings"));
}

#[test]
fn going_home_closes_the_view_and_clears_its_fold() {
    let mut app = attached(80, 24);
    app.on_line(usage_line(SESSION, "g1", "model/m", None));
    open_usage(&mut app);
    app.go_home();
    assert!(!app.session_view_open());
    assert!(!screen(&app, 80, 24).0.contains("Usage"));
    app.attach(SessionId(SESSION.to_owned()));
    open_usage(&mut app);
    assert!(screen(&app, 80, 24).0.contains("No model calls yet."));
}

#[test]
fn a_usage_line_from_another_session_is_not_folded() {
    let mut app = attached(80, 24);
    app.on_line(usage_line(OTHER, "g1", "model/other", None));
    open_usage(&mut app);
    assert!(screen(&app, 80, 24).0.contains("No model calls yet."));
}

#[test]
fn a_session_card_spend_click_opens_usage_and_keeps_the_draft() {
    let mut app = home_attached(160, 40);
    app.on_line(session_line(
        SESSION,
        "session_status",
        None,
        json!({
            "name": "work", "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2}, "cost": 1.25, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    ));
    app.on_edit(Edit::Paste("keep this draft".to_owned()));
    let (shown, targets) = screen(&app, 160, 40);
    assert!(shown.contains("cost billed  $1.25"));
    assert!(targets.iter().any(|target| {
        target.id == TargetId::Panel(PanelSpot::Usage)
            && shown
                .lines()
                .nth(usize::from(target.rect.y))
                .is_some_and(|line| line.contains("cost billed"))
    }));
    assert_eq!(
        app.on_click(TargetId::Panel(PanelSpot::Usage)),
        Effect::None
    );
    assert!(app.session_view_open());
    assert_eq!(app.input().expand(), "keep this draft");
    assert!(screen(&app, 160, 40).0.contains("Usage"));
}

#[test]
fn a_panel_click_closes_the_keymap_and_routes_the_next_key_to_usage() {
    let mut app = home_attached(160, 40);
    app.on_line(usage_line(SESSION, "g1", "model/m", None));
    app.on_line(session_line(
        SESSION,
        "session_status",
        None,
        json!({
            "name": "work", "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2}, "cost": 1.25, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    ));
    app.on_key(Key::F1, now());
    assert!(app.keymap_top().is_some());
    let (_, targets) = screen(&app, 160, 40);
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::Panel(PanelSpot::Usage))
    );
    app.on_click(TargetId::Panel(PanelSpot::Usage));
    assert_eq!(app.keymap_top(), None);
    assert!(screen(&app, 160, 40).0.contains("Usage"));
    app.on_key(Key::Down, now());
    assert_eq!(app.session_view_screen(0).unwrap().list.selected(), 1);
    assert!(screen(&app, 160, 40).0.contains("Usage"));
}

#[test]
fn the_narrow_status_line_spend_segment_opens_usage() {
    let mut app = home_attached(100, 30);
    app.on_line(session_line(
        SESSION,
        "session_status",
        None,
        json!({
            "name": "work", "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2}, "cost": 1.25, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    ));
    app.on_edit(Edit::Paste("keep this draft".to_owned()));
    let (_, targets) = screen(&app, 100, 30);
    assert!(
        targets
            .iter()
            .any(|target| { target.id == TargetId::Panel(PanelSpot::Usage) })
    );
    app.on_click(TargetId::Panel(PanelSpot::Usage));
    assert!(app.session_view_open());
    assert_eq!(app.input().expand(), "keep this draft");
    assert!(screen(&app, 100, 30).0.contains("Usage"));
}

#[test]
fn an_open_usage_view_hides_the_input_cursor() {
    let mut app = attached(80, 24);
    app.on_edit(Edit::Paste("draft".to_owned()));
    let area = Rect::new(0, 0, 80, 24);
    assert!(crate::view::cursor(&app, area).is_some());
    open_usage(&mut app);
    assert_eq!(crate::view::cursor(&app, area), None);
    assert!(screen(&app, 80, 24).0.contains("Usage"));
}

const DELEGATE: &str = "s_cccccccccccccccc";

fn job_started_line(job: &str, description: &str) -> Line {
    session_line(
        SESSION,
        "job_started",
        None,
        json!({"job_id": job, "description": description, "output_path": "/tmp/out"}),
    )
}

fn delegate_started_line(job: &str, delegate: &str, model: &str) -> Line {
    session_line(
        SESSION,
        "delegate_started",
        None,
        json!({"job_id": job, "delegate_session_id": delegate,
            "harness": "fiber", "model": model, "workspace": "/w"}),
    )
}

fn delegated_usage_line(origin: &str, generation: &str, model: &str) -> Line {
    session_line(
        SESSION,
        "usage_recorded",
        None,
        json!({
            "generation_id": generation, "model": model,
            "tokens": {"input": 12, "cache_read": 0,
                "cache_write": {"5m": 0}, "output": 5},
            "input_bytes": 0, "cost": 0.25, "origin_session_id": origin,
        }),
    )
}

#[test]
fn a_job_started_line_labels_its_delegate_with_the_job_description() {
    let mut app = attached(100, 40);
    app.on_line(job_started_line("j_1", "review the diff"));
    app.on_line(delegate_started_line("j_1", DELEGATE, "model/d"));
    app.on_line(delegated_usage_line(DELEGATE, "g1", "model/d"));
    open_usage(&mut app);
    let shown = screen(&app, 100, 40).0;
    assert!(shown.contains("◆ review the diff · model/d"), "{shown}");
    assert!(!shown.contains("◆ j_1"), "{shown}");
}

#[test]
fn a_delegate_started_line_adds_a_delegate_row_for_its_calls() {
    let mut app = attached(100, 40);
    app.on_line(delegate_started_line("j_2", DELEGATE, "model/d"));
    app.on_line(delegated_usage_line(DELEGATE, "g1", "model/d"));
    open_usage(&mut app);
    let shown = screen(&app, 100, 40).0;
    assert!(shown.contains("◆ j_2 · model/d"), "{shown}");
    assert!(!shown.contains(&format!("session {DELEGATE}")), "{shown}");
}

fn context_preamble() -> Line {
    session_line(
        SESSION,
        "preamble_built",
        None,
        json!({
            "reason": "start", "model": "model/m", "context_window": 100,
            "trigger_at": 80, "system_prompt": "sys",
            "tools": [{"name": "read", "registered_by": "builtin", "deferred": false,
                "definition": {"a": 1}}],
            "tool_choice": "auto", "cache_lifetime": "5m",
        }),
    )
}

fn context_status(tokens: u64, window: u64) -> Line {
    session_line(
        SESSION,
        "session_status",
        None,
        json!({
            "name": "work", "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 0}, "cost": 0.0, "subscription_cost": 0.0},
            "model": "model/m", "delegates": 0, "jobs": 0, "clients": 0,
            "context": {"tokens": tokens, "window": window},
        }),
    )
}

fn context_request_started() -> Line {
    session_line(SESSION, "assistant_message_started", None, json!({}))
}

fn context_usage(input_media: Option<bool>) -> Line {
    let mut payload = json!({
        "generation_id": "g_context", "model": "model/m",
        "tokens": {"input": 10, "cache_read": 0, "cache_write": {}, "output": 0},
        "input_bytes": 10, "cost": 0.0,
    });
    if let Some(input_media) = input_media {
        payload["input_media"] = json!(input_media);
    }
    session_line(SESSION, "usage_recorded", None, payload)
}

fn context_tool(name: &str, result: &str) -> [Line; 2] {
    [
        session_line(
            SESSION,
            "tool_call_requested",
            None,
            json!({"name": name, "arguments": {}}),
        ),
        session_line(
            SESSION,
            "tool_call_completed",
            None,
            json!({"status": "completed", "content": [{"type": "text", "text": result}]}),
        ),
    ]
}

fn context_handoff() -> Line {
    session_line(
        SESSION,
        "handoff_completed",
        None,
        json!({"outcome": "completed"}),
    )
}

fn open_context(app: &mut App) {
    assert_eq!(app.open_session_view(SessionView::Context), Effect::None);
}

fn slash_context(app: &mut App) -> Effect {
    app.on_edit(Edit::Paste("/context".to_owned()));
    app.on_key(Key::Enter, now())
}

#[test]
fn slash_context_opens_the_view_and_clears_the_draft() {
    let mut app = attached(80, 24);
    app.on_line(context_preamble());
    app.on_line(context_status(50, 100));
    assert_eq!(slash_context(&mut app), Effect::None);
    assert!(app.session_view_open());
    assert!(app.input().is_empty());
    let shown = screen(&app, 80, 24).0;
    assert!(shown.contains("Context"), "{shown}");
    assert!(shown.contains("context  50 of 100 tokens"), "{shown}");
    app.on_key(Key::Down, now());
    assert_eq!(app.session_view_screen(0).unwrap().list.selected(), 1);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.session_view_open());
}

#[test]
fn context_folds_the_preamble_rate_total_handoff_and_tool_results() {
    let mut app = attached(100, 40);
    app.on_line(context_preamble());
    app.on_line(context_status(50, 100));
    app.on_line(context_request_started());
    app.on_line(context_usage(None));
    for line in context_tool("read", "12345") {
        app.on_line(line);
    }
    open_context(&mut app);
    let shown = screen(&app, 100, 40).0;
    for expected in [
        "context  50 of 100 tokens · 50%",
        "█ system prompt  3 tokens",
        "▓ tool definitions  7 tokens",
        "▒ tool results  5 tokens",
        "▆ messages  35 tokens",
        "handoff at 80 tokens",
        "read  ~5 tokens",
    ] {
        assert!(shown.contains(expected), "missing {expected:?} in {shown}");
    }
    let zero_width = app.session_view_screen(0).unwrap();
    let wide = app.session_view_screen(80).unwrap();
    assert_eq!(zero_width.rows.len(), wide.rows.len());
}

#[test]
fn clicking_a_context_row_selects_it() {
    let mut app = attached(100, 40);
    app.on_line(context_preamble());
    app.on_line(context_status(50, 100));
    open_context(&mut app);
    let (_, targets) = screen(&app, 100, 40);
    assert!(
        targets
            .iter()
            .any(|target| { target.id == TargetId::View(ViewSpot::Row(1)) })
    );
    app.on_click(TargetId::View(ViewSpot::Row(1)));
    assert_eq!(app.session_view_screen(100).unwrap().list.selected(), 1);
}

#[test]
fn a_request_with_input_media_keeps_result_sizes_in_bytes() {
    let mut app = attached(100, 40);
    app.on_line(context_preamble());
    app.on_line(context_request_started());
    app.on_line(context_usage(Some(true)));
    app.on_line(context_status(50, 100));
    for line in context_tool("read", "12345") {
        app.on_line(line);
    }
    open_context(&mut app);
    let shown = screen(&app, 100, 40).0;
    assert!(
        shown.contains("Breakdown after a request without images."),
        "{shown}"
    );
    assert!(shown.contains("read  5 bytes"), "{shown}");
    assert!(!shown.contains("system prompt  3 tokens"), "{shown}");
}

#[test]
fn a_fork_counts_its_own_results_and_completed_handoff_clears_them() {
    let mut app = attached(100, 40);
    app.on_line(session_line(
        SESSION,
        "session_started",
        None,
        json!({"workspace": "/w", "variables": {},
            "forked_from": {"session_id": "s_parent", "seq": 8}}),
    ));
    app.on_line(context_preamble());
    app.on_line(context_request_started());
    app.on_line(context_usage(None));
    app.on_line(context_status(100, 100));
    for line in context_tool("read", "12345") {
        app.on_line(line);
    }
    for line in context_tool("write", "1234567") {
        app.on_line(line);
    }
    open_context(&mut app);
    let shown = screen(&app, 100, 40).0;
    assert!(shown.contains("▒ tool results  12 tokens"), "{shown}");
    assert!(shown.contains("▆ messages  78 tokens"), "{shown}");
    assert!(
        shown.contains("history before the fork counts under messages"),
        "{shown}"
    );
    assert!(shown.contains("read  ~5 tokens"), "{shown}");
    assert!(shown.contains("write  ~7 tokens"), "{shown}");
    assert!(!shown.contains("ancestor"), "{shown}");
    app.on_line(context_handoff());
    let after = screen(&app, 100, 40).0;
    assert!(after.contains("▒ tool results  0 tokens"), "{after}");
    assert!(after.contains("none since the last handoff"), "{after}");
}

#[test]
fn going_home_clears_context_state_for_the_next_attachment() {
    let mut app = attached(100, 40);
    app.on_line(context_preamble());
    app.on_line(context_status(50, 100));
    for line in context_tool("read", "result") {
        app.on_line(line);
    }
    open_context(&mut app);
    app.go_home();
    assert!(!app.session_view_open());
    app.attach(SessionId(SESSION.to_owned()));
    open_context(&mut app);
    let shown = screen(&app, 100, 40).0;
    assert!(
        shown.contains("The context shows after the session's first request."),
        "{shown}"
    );
    assert!(!shown.contains("read"), "{shown}");
}

#[test]
fn the_session_card_context_rows_open_the_view_and_keep_the_draft() {
    let mut app = home_attached(160, 40);
    app.on_line(context_preamble());
    app.on_line(context_status(50, 100));
    app.on_edit(Edit::Paste("keep this draft".to_owned()));
    let (_, targets) = screen(&app, 160, 40);
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::Panel(PanelSpot::Context))
    );
    assert_eq!(
        app.on_click(TargetId::Panel(PanelSpot::Context)),
        Effect::None
    );
    assert!(app.session_view_open());
    assert_eq!(app.input().expand(), "keep this draft");
    assert!(screen(&app, 160, 40).0.contains("Context"));
}

#[test]
fn the_narrow_context_segment_opens_the_view_and_keeps_the_draft() {
    let mut app = home_attached(100, 30);
    app.on_line(context_preamble());
    app.on_line(context_status(50, 100));
    app.on_edit(Edit::Paste("keep this draft".to_owned()));
    let (shown, targets) = screen(&app, 100, 30);
    assert!(shown.contains("50% context"), "{shown}");
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::Panel(PanelSpot::Context))
    );
    assert_eq!(
        app.on_click(TargetId::Panel(PanelSpot::Context)),
        Effect::None
    );
    assert!(app.session_view_open());
    assert_eq!(app.input().expand(), "keep this draft");
    assert!(screen(&app, 100, 30).0.contains("Context"));
}

fn files_attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        panel_cards: vec!["changed_files".to_owned()],
        ..Default::default()
    });
    app.attach(SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app
}

fn changes_line(entries: &[(&str, u64, u64)]) -> Line {
    session_line(
        SESSION,
        "tool_call_completed",
        None,
        json!({
            "status": "completed", "content": [],
            "changes": entries.iter().map(|(path, added, removed)| json!({
                "path": path, "added": added, "removed": removed,
            })).collect::<Vec<_>>(),
        }),
    )
}

fn many_changes(app: &mut App, count: usize) {
    let entries: Vec<(String, u64, u64)> = (0..count)
        .map(|at| {
            (
                format!("src/file-{at}.rs"),
                u64::try_from(count - at).unwrap_or(u64::MAX),
                0,
            )
        })
        .collect();
    let borrowed: Vec<(&str, u64, u64)> = entries
        .iter()
        .map(|(path, added, removed)| (path.as_str(), *added, *removed))
        .collect();
    app.on_line(changes_line(&borrowed));
}

fn command(effect: Effect) -> Value {
    let Effect::Send(lines) = effect else {
        panic!("expected one shell command, got {effect:?}");
    };
    assert_eq!(lines.len(), 1);
    serde_json::from_str(&lines[0]).unwrap_or_else(|error| panic!("command JSON: {error}"))
}

fn accepted_diff(id: &str, text: &str) -> Line {
    session_line(
        SESSION,
        "command_accepted",
        None,
        json!({
            "command_id": id,
            "result": {"output": text, "process": {"exit_code": 1, "timed_out": false}},
        }),
    )
}

fn rejected_diff(session: &str, id: &str, message: &str) -> Line {
    session_line(
        session,
        "command_rejected",
        None,
        json!({"command_id": id, "code": "duplicate_command", "message": message}),
    )
}

fn hub_rejected(id: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": id, "message": message})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

fn open_files_from_totals(app: &mut App, width: u16, height: u16) {
    let (_, targets) = screen(app, width, height);
    assert!(
        targets
            .iter()
            .any(|target| { target.id == TargetId::Panel(PanelSpot::ChangedFiles) })
    );
    assert_eq!(
        app.on_click(TargetId::Panel(PanelSpot::ChangedFiles)),
        Effect::None
    );
}

#[test]
fn the_totals_row_opens_every_changed_file_in_ranked_order() {
    let mut app = files_attached(160, 40);
    many_changes(&mut app, 7);
    open_files_from_totals(&mut app, 160, 40);
    let shown = screen(&app, 160, 40).0;
    assert!(shown.contains("Changed files"), "{shown}");
    let frame = app.session_view_screen(0).expect("changed-files frame");
    assert_eq!(frame.rows.len(), 7);
    let paths: Vec<String> = frame.rows.iter().map(|row| row[0].0.clone()).collect();
    let expected: Vec<String> = ranked(app.panel_state().changes())
        .into_iter()
        .map(|(path, added, removed)| format!("{path}  +{added} −{removed}"))
        .collect();
    assert_eq!(paths, expected);
}

#[test]
fn enter_sends_one_unsent_shell_command_for_the_selected_file() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 2);
    open_files_from_totals(&mut app, 120, 35);
    let expected_path = ranked(app.panel_state().changes())[0].0.to_owned();
    let value = command(app.on_key(Key::Enter, now()));
    assert_eq!(value["command"], "shell");
    assert_eq!(value["session_id"], SESSION);
    assert_eq!(value["args"]["command"], diff_command(&expected_path));
    assert_eq!(value["args"]["send"], false);
    assert!(screen(&app, 120, 35).0.contains("Reading the diff…"));
}

#[test]
fn the_matching_shell_answer_fills_the_diff_without_a_conversation_item() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 1);
    open_files_from_totals(&mut app, 120, 35);
    let request = command(app.on_key(Key::Enter, now()));
    let before = app.lines();
    app.on_line(accepted_diff(
        request["id"].as_str().unwrap_or_default(),
        "diff line\n",
    ));
    let shown = screen(&app, 120, 35).0;
    assert!(shown.contains("diff line"), "{shown}");
    assert_eq!(app.lines(), before);
}

#[test]
fn an_answer_for_another_request_id_does_not_fill_the_diff() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 1);
    open_files_from_totals(&mut app, 120, 35);
    let request = command(app.on_key(Key::Enter, now()));
    app.on_line(accepted_diff("c_stale", "stale diff\n"));
    let shown = screen(&app, 120, 35).0;
    assert!(shown.contains("Reading the diff…"), "{shown}");
    assert!(!shown.contains("stale diff"), "{shown}");
    assert_ne!(request["id"], "c_stale");
}

#[test]
fn choosing_a_second_file_makes_the_first_answer_stale() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 2);
    open_files_from_totals(&mut app, 120, 35);
    let first = command(app.on_key(Key::Enter, now()));
    assert_eq!(app.on_edit(Edit::Left), Effect::None);
    app.on_key(Key::Down, now());
    let second = command(app.on_key(Key::Enter, now()));
    assert_ne!(first["id"], second["id"]);
    app.on_line(accepted_diff(
        first["id"].as_str().unwrap_or_default(),
        "stale first diff\n",
    ));
    let shown = screen(&app, 120, 35).0;
    assert!(shown.contains("Reading the diff…"), "{shown}");
    assert!(!shown.contains("stale first diff"), "{shown}");
}

#[test]
fn a_matching_session_rejection_shows_its_message_and_another_id_does_not() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 1);
    open_files_from_totals(&mut app, 120, 35);
    let request = command(app.on_key(Key::Enter, now()));
    app.on_line(rejected_diff(SESSION, "c_other", "wrong rejection"));
    assert!(screen(&app, 120, 35).0.contains("Reading the diff…"));
    app.on_line(rejected_diff(
        SESSION,
        request["id"].as_str().unwrap_or_default(),
        "session refused",
    ));
    assert!(screen(&app, 120, 35).0.contains("session refused"));
}

#[test]
fn a_matching_hub_rejection_shows_its_message_and_another_id_does_not() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 1);
    open_files_from_totals(&mut app, 120, 35);
    let request = command(app.on_key(Key::Enter, now()));
    app.on_line(hub_rejected("c_other", "wrong hub rejection"));
    assert!(screen(&app, 120, 35).0.contains("Reading the diff…"));
    app.on_line(hub_rejected(
        request["id"].as_str().unwrap_or_default(),
        "hub refused",
    ));
    assert!(screen(&app, 120, 35).0.contains("hub refused"));
}

#[test]
fn left_returns_to_the_same_file_selection_and_drops_a_late_answer() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 2);
    open_files_from_totals(&mut app, 120, 35);
    app.on_key(Key::Down, now());
    let request = command(app.on_key(Key::Enter, now()));
    assert_eq!(app.on_edit(Edit::Left), Effect::None);
    assert_eq!(app.session_view_screen(0).unwrap().list.selected(), 1);
    assert!(screen(&app, 120, 35).0.contains("Changed files"));
    app.on_line(accepted_diff(
        request["id"].as_str().unwrap_or_default(),
        "late diff\n",
    ));
    let shown = screen(&app, 120, 35).0;
    assert!(!shown.contains("late diff"), "{shown}");
    assert!(shown.contains("src/file-1.rs"), "{shown}");
}

#[test]
fn escape_in_the_diff_closes_it_and_drops_a_late_answer() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 1);
    open_files_from_totals(&mut app, 120, 35);
    let request = command(app.on_key(Key::Enter, now()));
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.session_view_open());
    app.on_line(accepted_diff(
        request["id"].as_str().unwrap_or_default(),
        "late diff\n",
    ));
    assert!(!screen(&app, 120, 35).0.contains("late diff"));
}

#[test]
fn escape_in_the_file_list_closes_it() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 1);
    open_files_from_totals(&mut app, 120, 35);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.session_view_open());
}

#[test]
fn choosing_a_file_while_disconnected_says_not_connected_and_sends_nothing() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        panel_cards: vec!["changed_files".to_owned()],
        ..Default::default()
    });
    app.attach(SessionId(SESSION.to_owned()));
    app.set_size(120, 35);
    many_changes(&mut app, 1);
    // The panel click opens the list even while the link is down.
    app.on_click(TargetId::Panel(PanelSpot::ChangedFiles));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(screen(&app, 120, 35).0.contains("Not connected."));
}

#[test]
fn clicking_a_panel_file_row_opens_its_diff_and_sends_the_request() {
    let mut app = files_attached(160, 40);
    many_changes(&mut app, 2);
    let (_, targets) = screen(&app, 160, 40);
    assert!(
        targets
            .iter()
            .any(|target| { target.id == TargetId::Panel(PanelSpot::File(0)) })
    );
    let expected = ranked(app.panel_state().changes())[0].0.to_owned();
    let value = command(app.on_click(TargetId::Panel(PanelSpot::File(0))));
    assert!(app.session_view_open());
    assert_eq!(value["args"]["command"], diff_command(&expected));
    assert!(
        screen(&app, 160, 40)
            .0
            .contains(&format!("Changed files › {expected}"))
    );
}

#[test]
fn a_panel_file_rank_past_the_end_opens_the_list_without_sending() {
    let mut app = files_attached(160, 40);
    many_changes(&mut app, 1);
    assert_eq!(
        app.on_click(TargetId::Panel(PanelSpot::File(5))),
        Effect::None
    );
    assert!(app.session_view_open());
    assert!(screen(&app, 160, 40).0.contains("Changed files"));
    assert!(!screen(&app, 160, 40).0.contains("Reading the diff…"));
}

#[test]
fn the_narrow_changed_files_segment_opens_the_file_list() {
    let mut app = files_attached(100, 30);
    many_changes(&mut app, 2);
    let (shown, targets) = screen(&app, 100, 30);
    assert!(shown.contains("2 files +3 −0"), "{shown}");
    assert!(
        targets
            .iter()
            .any(|target| { target.id == TargetId::Panel(PanelSpot::ChangedFiles) })
    );
    assert_eq!(
        app.on_click(TargetId::Panel(PanelSpot::ChangedFiles)),
        Effect::None
    );
    assert!(screen(&app, 100, 30).0.contains("Changed files"));
}

#[test]
fn clicking_a_file_list_row_chooses_that_file() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 2);
    open_files_from_totals(&mut app, 120, 35);
    let expected = ranked(app.panel_state().changes())[1].0.to_owned();
    let (_, targets) = screen(&app, 120, 35);
    assert!(
        targets
            .iter()
            .any(|target| { target.id == TargetId::View(ViewSpot::Row(1)) })
    );
    let value = command(app.on_click(TargetId::View(ViewSpot::Row(1))));
    assert_eq!(value["args"]["command"], diff_command(&expected));
    assert!(
        screen(&app, 120, 35)
            .0
            .contains(&format!("Changed files › {expected}"))
    );
}

/// Folds a plain job as `job`.
fn start_job(app: &mut App, job: &str) {
    app.on_line(session_line(
        SESSION,
        "job_started",
        None,
        json!({"job_id": job, "description": format!("task {job}"),
            "output_path": "/tmp/out"}),
    ));
}

/// Folds a fiber delegate as `job` with session `delegate`.
fn start_delegate(app: &mut App, job: &str, delegate: &str) {
    start_job(app, job);
    app.on_line(session_line(
        SESSION,
        "delegate_started",
        None,
        json!({"job_id": job,
            "delegate_session_id": delegate,
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
}

/// Completes `job`.
fn complete_job(app: &mut App, job: &str) {
    app.on_line(session_line(
        SESSION,
        "job_completed",
        None,
        json!({"job_id": job, "status": "completed"}),
    ));
}

/// Opens the running delegates list.
fn open_delegates(app: &mut App) {
    assert_eq!(app.open_session_view(SessionView::Delegates), Effect::None);
    assert!(app.session_view_open());
}

/// Opens the running jobs list.
fn open_jobs(app: &mut App) {
    assert_eq!(app.open_session_view(SessionView::Jobs), Effect::None);
    assert!(app.session_view_open());
}

/// The open list's selected row.
fn selected(app: &App) -> usize {
    app.session_view_screen(0)
        .map(|frame| frame.list.selected())
        .unwrap_or_else(|| panic!("a list frame"))
}

#[test]
fn delegates_list_draws_two_rows_per_delegate() {
    let mut app = attached(80, 24);
    start_delegate(&mut app, "j_1", "s_dddddddddddddddd");
    start_delegate(&mut app, "j_2", "s_eeeeeeeeeeeeeeee");
    open_delegates(&mut app);
    let (shown, _) = screen(&app, 80, 24);
    assert!(shown.contains("Running delegates"));
    assert!(shown.contains("task j_1"));
    assert!(shown.contains("task j_2"));
    insta::assert_snapshot!("delegates_list", shown);
}

#[test]
fn down_then_enter_opens_the_second_item_and_closes_the_list() {
    let mut app = attached(80, 24);
    start_delegate(&mut app, "j_1", "s_dddddddddddddddd");
    start_delegate(&mut app, "j_2", "s_eeeeeeeeeeeeeeee");
    open_delegates(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(selected(&app), 2);
    // The link is down, so opening sends nothing; the view still swaps
    // in and the list closes.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.item_open());
    assert!(!app.session_view_open());
    assert_eq!(
        app.item_view().map(|view| view.description),
        Some("task j_2".to_owned())
    );
}

#[test]
fn clicking_a_delegate_row_opens_it_and_a_blank_tail_only_selects() {
    let mut app = attached(80, 24);
    start_delegate(&mut app, "j_1", "s_dddddddddddddddd");
    start_delegate(&mut app, "j_2", "s_eeeeeeeeeeeeeeee");
    open_delegates(&mut app);
    let serial = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    // The second row is the first delegate's description: its text opens it.
    // The link is down, so opening sends nothing.
    assert_eq!(
        app.on_click(TargetId::View(ViewSpot::Item(serial))),
        Effect::None
    );
    assert!(app.item_open());
    assert!(!app.session_view_open());
}

#[test]
fn clicking_a_row_tail_only_selects_the_row() {
    let mut app = attached(80, 24);
    start_delegate(&mut app, "j_1", "s_dddddddddddddddd");
    start_delegate(&mut app, "j_2", "s_eeeeeeeeeeeeeeee");
    open_delegates(&mut app);
    assert_eq!(app.on_click(TargetId::View(ViewSpot::Row(3))), Effect::None);
    assert!(!app.item_open());
    assert!(app.session_view_open());
    assert_eq!(selected(&app), 3);
}

#[test]
fn esc_and_the_cross_close_the_lists() {
    for view in [SessionView::Delegates, SessionView::Jobs] {
        let mut app = attached(80, 24);
        start_job(&mut app, "j_1");
        assert_eq!(app.open_session_view(view), Effect::None);
        assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
        assert!(!app.session_view_open());
        assert_eq!(app.open_session_view(view), Effect::None);
        assert_eq!(app.on_click(TargetId::View(ViewSpot::Close)), Effect::None);
        assert!(!app.session_view_open());
    }
}

#[test]
fn an_empty_list_says_nothing_running_and_enter_does_nothing() {
    let mut app = attached(80, 24);
    open_jobs(&mut app);
    let (shown, _) = screen(&app, 80, 24);
    assert!(shown.contains("Nothing running."));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.session_view_open());
    assert!(!app.item_open());
}

#[test]
fn jobs_list_draws_one_row_per_job_and_opens_on_click() {
    let mut app = attached(80, 24);
    start_job(&mut app, "j_1");
    start_job(&mut app, "j_2");
    open_jobs(&mut app);
    let (shown, _) = screen(&app, 80, 24);
    assert!(shown.contains("Running jobs"));
    assert!(shown.contains("task j_1"));
    insta::assert_snapshot!("jobs_list", shown);
    let serial = app
        .serial_of_job(&contract::JobId("j_2".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    assert_eq!(
        app.on_click(TargetId::View(ViewSpot::Item(serial))),
        Effect::None
    );
    assert!(app.item_open());
    assert!(!app.session_view_open());
    assert_eq!(
        app.item_view().map(|view| view.description),
        Some("task j_2".to_owned())
    );
}

#[test]
fn a_list_press_and_release_on_different_serials_opens_nothing() {
    use crate::keys::{Button, Mouse, MouseKind};
    let mut app = attached(80, 24);
    start_job(&mut app, "j_1");
    start_job(&mut app, "j_2");
    open_jobs(&mut app);
    let (_, before) = screen(&app, 80, 24);
    let first = before
        .iter()
        .find(|target| matches!(target.id, TargetId::View(ViewSpot::Item(_))))
        .cloned()
        .unwrap_or_else(|| panic!("an item target"));
    let (col, row) = (first.rect.x, first.rect.y);
    let mut pointer = crate::mouse::Pointer::default();
    // A press on the first job's text, its `job_completed`, a redraw that
    // puts the second job on that row, and a release on it: the ids
    // differ, so no click.
    assert_eq!(
        pointer.on_mouse(
            &Mouse {
                kind: MouseKind::Press(Button::Left),
                col,
                row
            },
            &before,
            false
        ),
        None
    );
    complete_job(&mut app, "j_1");
    let (_, after) = screen(&app, 80, 24);
    assert_eq!(
        pointer.on_mouse(
            &Mouse {
                kind: MouseKind::Release,
                col,
                row
            },
            &after,
            false
        ),
        None
    );
    assert!(!app.item_open());
}
