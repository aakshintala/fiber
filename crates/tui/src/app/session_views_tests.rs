//! Tests for `/usage` on the app: its dispatch, keys, clicks and session fold
//! (`docs/tui.md`, "Swapped views").

use std::path::PathBuf;
use std::sync::Arc;

use crate::app::panel::Spot as PanelSpot;
use crate::app::testkit::{
    SESSION, attached, long_turns, now, sent, session_line, session_line_in_turn,
};
use crate::app::{App, Effect, SessionView};
use crate::changed_files_view::{diff_command, ranked};
use crate::home::Launch;
use crate::keys::{Edit, Key, Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::{Target, TargetId};
use crate::swapped::Spot as ViewSpot;
use contract::SessionId;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

const OTHER: &str = "s_bbbbbbbbbbbbbbbb";

fn home_attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        panel_cards: vec!["session".to_owned()],
        ..Default::default()
    });
    app.attach(SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app
}

fn usage_line(session: &str, generation: &str, model: &str, turn: Option<&str>) -> Line {
    let payload = json!({
        "generation_id": generation, "model": model,
        "tokens": {"input": 12, "cache_read": 3,
            "cache_write": {"5m": 4}, "output": 5},
        "input_bytes": 0, "cost": 0.25,
    });
    match turn {
        Some(turn) => session_line_in_turn(session, "usage_recorded", turn, payload, Some("a_1")),
        None => session_line(session, "usage_recorded", payload, Some("a_1")),
    }
}

fn turn_started(turn: &str) -> Line {
    session_line_in_turn(
        SESSION,
        "turn_started",
        turn,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "work"}]}]}),
        Some("a_1"),
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
        json!({
            "name": "other", "workspace": "/w", "project": "-w", "state": "streaming",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2}, "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
        Some("a_1"),
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
        json!({
            "name": "work", "workspace": "/w", "project": "-w", "state": "idle",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2}, "cost": 1.25, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
        Some("a_1"),
    ));
    app.on_edit(Edit::Paste("keep this draft".to_owned()));
    let (shown, targets) = screen(&app, 160, 40);
    // The cost row reads dim `cost` with its figure right-aligned.
    assert!(shown.contains("$1.25"));
    assert!(targets.iter().any(|target| {
        target.id == TargetId::Panel(PanelSpot::Usage)
            && shown
                .lines()
                .nth(usize::from(target.rect.y))
                .is_some_and(|line| line.contains("cost") && line.contains("$1.25"))
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
        json!({
            "name": "work", "workspace": "/w", "project": "-w", "state": "idle",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2}, "cost": 1.25, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
        Some("a_1"),
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
        json!({
            "name": "work", "workspace": "/w", "project": "-w", "state": "idle",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2}, "cost": 1.25, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
        Some("a_1"),
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
        json!({"job_id": job, "description": description, "output_path": "/tmp/out"}),
        Some("a_1"),
    )
}

fn delegate_started_line(job: &str, delegate: &str, model: &str) -> Line {
    session_line(
        SESSION,
        "delegate_started",
        json!({"job_id": job, "delegate_session_id": delegate,
            "harness": "fiber", "model": model, "workspace": "/w"}),
        Some("a_1"),
    )
}

fn delegated_usage_line(origin: &str, generation: &str, model: &str) -> Line {
    session_line(
        SESSION,
        "usage_recorded",
        json!({
            "generation_id": generation, "model": model,
            "tokens": {"input": 12, "cache_read": 0,
                "cache_write": {"5m": 0}, "output": 5},
            "input_bytes": 0, "cost": 0.25, "origin_session_id": origin,
        }),
        Some("a_1"),
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
        json!({
            "reason": "start", "model": "model/m", "context_window": 100,
            "trigger_at": 80, "system_prompt": "sys",
            "tools": [{"name": "read", "registered_by": "builtin", "deferred": false,
                "definition": {"a": 1}}],
            "tool_choice": "auto", "cache_lifetime": "5m",
        }),
        Some("a_1"),
    )
}

fn context_status(tokens: u64, window: u64) -> Line {
    session_line(
        SESSION,
        "session_status",
        json!({
            "name": "work", "workspace": "/w", "project": "-w", "state": "idle",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 0}, "cost": 0.0, "subscription_cost": 0.0},
            "model": "model/m", "delegates": 0, "jobs": 0, "clients": 0,
            "context": {"tokens": tokens, "window": window},
        }),
        Some("a_1"),
    )
}

fn context_request_started() -> Line {
    session_line(SESSION, "assistant_message_started", json!({}), Some("a_1"))
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
    session_line(SESSION, "usage_recorded", payload, Some("a_1"))
}

fn context_tool(name: &str, result: &str) -> [Line; 2] {
    [
        session_line(
            SESSION,
            "tool_call_requested",
            json!({"name": name, "arguments": {}}),
            Some("a_1"),
        ),
        session_line(
            SESSION,
            "tool_call_completed",
            json!({"status": "completed", "content": [{"type": "text", "text": result}]}),
            Some("a_1"),
        ),
    ]
}

fn context_handoff() -> Line {
    session_line(
        SESSION,
        "handoff_completed",
        json!({"outcome": "completed"}),
        Some("a_1"),
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
    assert!(
        shown.contains("50 tokens · 50% of the 100 window"),
        "{shown}"
    );
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
        "50 tokens · 50% of the 100 window · automatic handoff at 80",
        "■ system prompt",
        "~3 6%",
        "■ tool definitions",
        "~7 14%",
        "■ tool results",
        "~5 10%",
        "■ messages",
        "~35 70%",
        "handoff 80",
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
        json!({"workspace": "/w", "variables": {},
            "forked_from": {"session_id": "s_parent", "seq": 8}}),
        Some("a_1"),
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
    assert!(shown.contains("■ tool results"), "{shown}");
    assert!(shown.contains("~12 12%"), "{shown}");
    assert!(shown.contains("■ messages"), "{shown}");
    assert!(shown.contains("~78 78%"), "{shown}");
    assert!(
        shown.contains("history before the fork counts under messages"),
        "{shown}"
    );
    assert!(shown.contains("read  ~5 tokens"), "{shown}");
    assert!(shown.contains("write  ~7 tokens"), "{shown}");
    assert!(!shown.contains("ancestor"), "{shown}");
    app.on_line(context_handoff());
    let after = screen(&app, 100, 40).0;
    assert!(after.contains("■ tool results"), "{after}");
    assert!(after.contains("~0 0%"), "{after}");
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
    // Short context form: dim `ctx`, the 8-cell bar and the size.
    assert!(
        shown.contains("ctx \u{2586}\u{2586}\u{2586}\u{2586}\u{2586}\u{2586}\u{2586}\u{2586} 50"),
        "{shown}"
    );
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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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
        json!({
            "status": "completed", "content": [],
            "changes": entries.iter().map(|(path, added, removed)| json!({
                "path": path, "added": added, "removed": removed,
            })).collect::<Vec<_>>(),
        }),
        Some("a_1"),
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
        json!({
            "command_id": id,
            "result": {"output": text, "process": {"exit_code": 1, "timed_out": false}},
        }),
        Some("a_1"),
    )
}

fn rejected_diff(session: &str, id: &str, message: &str) -> Line {
    session_line(
        session,
        "command_rejected",
        json!({"command_id": id, "code": "duplicate_command", "message": message}),
        Some("a_1"),
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
    let effect = app.on_click(TargetId::Panel(PanelSpot::ChangedFiles));
    let Effect::Send(lines) = effect else {
        panic!("opening the list sends the numstat query, got {effect:?}");
    };
    // docs/tui.md:830 promises one numstat query, run when the view opens.
    assert_eq!(lines.len(), 1);
    let request: Value = serde_json::from_str(&lines[0])
        .unwrap_or_else(|error| panic!("shell request JSON: {error}"));
    let paths: Vec<&str> = ranked(app.panel_state().changes())
        .into_iter()
        .map(|(path, _, _)| path)
        .collect();
    assert_eq!(
        request["args"]["command"],
        crate::changed_files_view::numstat_command(&paths)
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
    let paths: Vec<String> = frame
        .rows
        .iter()
        .map(|row| row.iter().map(|cell| cell.0.as_str()).collect())
        .collect();
    let expected: Vec<String> = ranked(app.panel_state().changes())
        .into_iter()
        .map(|(path, added, removed)| format!("{path} · +{added} −{removed}"))
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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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
    let effect = app.on_click(TargetId::Panel(PanelSpot::File(0)));
    let Effect::Send(lines) = effect else {
        panic!("opening a file sends the numstat query and its diff, got {effect:?}");
    };
    let requests: Vec<Value> = lines
        .iter()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|error| panic!("shell request JSON: {error}"))
        })
        .collect();
    let paths: Vec<&str> = ranked(app.panel_state().changes())
        .into_iter()
        .map(|(path, _, _)| path)
        .collect();
    let numstat = crate::changed_files_view::numstat_command(&paths);
    let fetch = diff_command(&expected);
    for request in &requests {
        assert!(
            request["args"]["command"] == numstat || request["args"]["command"] == fetch,
            "every request is the numstat query or the file's diff: {requests:?}"
        );
    }
    assert!(
        requests
            .iter()
            .any(|request| request["args"]["command"] == numstat),
        "one request is the numstat query: {requests:?}"
    );
    let diff = requests
        .iter()
        .find(|request| request["args"]["command"] == fetch)
        .unwrap_or_else(|| panic!("one request is the file's diff: {requests:?}"));
    assert!(app.session_view_open());
    assert_eq!(diff["command"], "shell");
    assert_eq!(diff["args"]["command"], fetch);
    assert_eq!(diff["args"]["send"], false);
    assert!(
        screen(&app, 160, 40)
            .0
            .contains(&format!("Changed files › {expected}"))
    );
}

#[test]
fn a_panel_file_rank_past_the_end_opens_the_list_sending_only_the_numstat_command() {
    let mut app = files_attached(160, 40);
    many_changes(&mut app, 1);
    let effect = app.on_click(TargetId::Panel(PanelSpot::File(5)));
    let Effect::Send(lines) = effect else {
        panic!("a rank past the end sends only the numstat query, got {effect:?}");
    };
    // docs/tui.md:830 promises one numstat query, run when the view opens.
    assert_eq!(lines.len(), 1);
    let request: Value = serde_json::from_str(&lines[0])
        .unwrap_or_else(|error| panic!("shell request JSON: {error}"));
    let paths: Vec<&str> = ranked(app.panel_state().changes())
        .into_iter()
        .map(|(path, _, _)| path)
        .collect();
    assert_eq!(
        request["args"]["command"],
        crate::changed_files_view::numstat_command(&paths)
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
    let effect = app.on_click(TargetId::Panel(PanelSpot::ChangedFiles));
    let Effect::Send(lines) = effect else {
        panic!("opening the list sends the numstat query, got {effect:?}");
    };
    // docs/tui.md:830 promises one numstat query, run when the view opens.
    assert_eq!(lines.len(), 1);
    let request: Value = serde_json::from_str(&lines[0])
        .unwrap_or_else(|error| panic!("shell request JSON: {error}"));
    let paths: Vec<&str> = ranked(app.panel_state().changes())
        .into_iter()
        .map(|(path, _, _)| path)
        .collect();
    assert_eq!(
        request["args"]["command"],
        crate::changed_files_view::numstat_command(&paths)
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
        json!({"job_id": job, "description": format!("task {job}"),
            "output_path": "/tmp/out"}),
        Some("a_1"),
    ));
}

/// Folds a fiber delegate as `job` with session `delegate`.
fn start_delegate(app: &mut App, job: &str, delegate: &str) {
    start_job(app, job);
    app.on_line(session_line(
        SESSION,
        "delegate_started",
        json!({"job_id": job,
            "delegate_session_id": delegate,
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
        Some("a_1"),
    ));
}

/// Completes `job`.
fn complete_job(app: &mut App, job: &str) {
    app.on_line(session_line(
        SESSION,
        "job_completed",
        json!({"job_id": job, "status": "completed"}),
        Some("a_1"),
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

/// Opens the all-delegates list.
fn open_all_delegates(app: &mut App) {
    assert_eq!(
        app.open_session_view(SessionView::AllDelegates),
        Effect::None
    );
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
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(selected(&app), 3);
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
    assert_eq!(app.on_click(TargetId::View(ViewSpot::Row(4))), Effect::None);
    assert!(!app.item_open());
    assert!(app.session_view_open());
    assert_eq!(selected(&app), 4);
}

#[test]
fn a_non_row_click_in_a_usage_quota_or_context_view_selects_nothing() {
    for view in [SessionView::Usage, SessionView::Quota, SessionView::Context] {
        let mut app = attached(80, 24);
        app.outbox.link = crate::app::Link::Up;
        app.on_line(usage_line(SESSION, "g1", "model/m", None));
        app.on_line(context_preamble());
        app.on_line(context_status(50, 100));
        if view == SessionView::Quota {
            let lines = sent(slash_quota(&mut app));
            let id = lines[0]["id"].as_str().unwrap().to_owned();
            app.on_line(session_line(
                SESSION,
                "command_accepted",
                json!({"command_id": id,
                    "result": {"quota": [
                        {"provider": "codex", "credential": "default",
                            "windows": [{"name": "weekly", "percent_used": 38.0}]},
                    ], "notice_at": 80.0}}),
                Some("a_1"),
            ));
        }
        assert_eq!(app.open_session_view(view), Effect::None);
        app.on_click(TargetId::View(ViewSpot::Row(1)));
        let selected = app.session_view_screen(80).unwrap().list.selected();
        assert_eq!(selected, 1);
        // A click that names no row selects nothing: only a row click moves.
        assert_eq!(
            app.on_click(TargetId::View(ViewSpot::Item(7))),
            Effect::None
        );
        assert!(app.session_view_open());
        assert_eq!(app.session_view_screen(80).unwrap().list.selected(), 1);
    }
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
fn all_delegates_list_draws_every_delegate_with_no_heading() {
    let mut app = attached(80, 24);
    start_delegate(&mut app, "j_1", "s_dddddddddddddddd");
    start_delegate(&mut app, "j_2", "s_eeeeeeeeeeeeeeee");
    open_all_delegates(&mut app);
    let (shown, _) = screen(&app, 80, 24);
    assert!(shown.contains("Delegates"));
    assert!(shown.contains("task j_1"));
    assert!(shown.contains("task j_2"));
    assert!(!shown.contains("Running delegates"));
    assert!(!shown.contains("Nothing running."));
    // Row data starts at once: the first frame row holds the first
    // delegate (docs/tui.md, "Swapped views").
    let frame = app.session_view_screen(80).expect("the view is open");
    assert_eq!(frame.title, "Delegates");
    assert_eq!(frame.command, "/delegates");
    assert!(text_of(&frame.rows[0]).contains("task j_1"));
    insta::assert_snapshot!("all_delegates_list", shown);
}

/// One frame row's text.
fn text_of(cells: &[(String, Option<ViewSpot>, crate::swapped::Ink)]) -> String {
    cells.iter().map(|cell| cell.0.as_str()).collect()
}

#[test]
fn down_twice_then_enter_opens_the_next_delegate_and_closes_the_list() {
    let mut app = attached(80, 24);
    start_delegate(&mut app, "j_1", "s_dddddddddddddddd");
    start_delegate(&mut app, "j_2", "s_eeeeeeeeeeeeeeee");
    open_all_delegates(&mut app);
    // Two rows per delegate (docs/tui.md, "The panel"): rows 0-1 hold
    // the first delegate, so two Downs reach the second.
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
fn four_delegates_scroll_to_a_later_one_and_open_it() {
    let mut app = attached(80, 12);
    start_delegate(&mut app, "j_1", "s_dddddddddddddddd");
    start_delegate(&mut app, "j_2", "s_eeeeeeeeeeeeeeee");
    start_delegate(&mut app, "j_3", "s_ffffffffffffffff");
    start_delegate(&mut app, "j_4", "s_0000000000000000");
    open_all_delegates(&mut app);
    // Two rows per delegate (docs/tui.md, "The panel"): seven Downs
    // reach the fourth delegate's second row.
    for _ in 0..7 {
        assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    }
    assert_eq!(selected(&app), 7);
    // The fourth delegate's rows start below the first window: the list
    // scrolled to keep them shown (docs/tui.md, "Swapped views").
    let frame = app.session_view_screen(80).expect("the view is open");
    assert!(frame.list.top() > 0);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.item_open());
    assert!(!app.session_view_open());
    assert_eq!(
        app.item_view().map(|view| view.description),
        Some("task j_4".to_owned())
    );
}

#[test]
fn clicking_a_finished_row_opens_its_replay() {
    let mut app = attached(80, 24);
    start_delegate(&mut app, "j_1", "s_dddddddddddddddd");
    app.on_line(session_line(
        SESSION,
        "job_completed",
        json!({"job_id": "j_1", "status": "completed"}),
        Some("a_1"),
    ));
    open_all_delegates(&mut app);
    let (shown, _) = screen(&app, 80, 24);
    assert!(shown.contains("✓ task j_1"));
    let serial = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    // The link is down, so opening sends nothing; the finished
    // delegate still opens its replay view.
    assert_eq!(
        app.on_click(TargetId::View(ViewSpot::Item(serial))),
        Effect::None
    );
    assert!(app.item_open());
    assert!(!app.session_view_open());
    assert_eq!(
        app.item_view().map(|view| view.description),
        Some("task j_1".to_owned())
    );
}

#[test]
fn esc_and_the_cross_close_the_all_delegates_list() {
    let mut app = attached(80, 24);
    start_delegate(&mut app, "j_1", "s_dddddddddddddddd");
    open_all_delegates(&mut app);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.session_view_open());
    open_all_delegates(&mut app);
    assert_eq!(app.on_click(TargetId::View(ViewSpot::Close)), Effect::None);
    assert!(!app.session_view_open());
}

#[test]
fn an_empty_all_delegates_view_says_no_delegates_and_enter_does_nothing() {
    let mut app = attached(80, 24);
    open_all_delegates(&mut app);
    let (shown, _) = screen(&app, 80, 24);
    assert!(shown.contains("No delegates"));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.session_view_open());
    assert!(!app.item_open());
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

fn slash_quota(app: &mut App) -> Effect {
    app.on_edit(Edit::Paste("/quota".to_owned()));
    app.on_key(Key::Enter, now())
}

#[test]
fn slash_quota_opens_the_view_and_fetches_with_fetch_true() {
    let mut app = attached(80, 24);
    app.outbox.link = crate::app::Link::Up;
    let lines = sent(slash_quota(&mut app));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "quota");
    assert_eq!(lines[0]["args"], json!({"fetch": true}));
    assert_eq!(lines[0]["session_id"], SESSION);
    assert!(app.session_view_open());
    assert!(app.input().is_empty());
    let shown = screen(&app, 80, 24).0;
    assert!(shown.contains("Quota"));
    assert!(shown.contains("fetching"));
    // A second opening shares the in-flight fetch: the view reopens with
    // no new line.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.session_view_open());
    assert!(sent(slash_quota(&mut app)).is_empty());
    assert!(app.session_view_open());
    assert!(screen(&app, 80, 24).0.contains("fetching"));
}

#[test]
fn slash_quota_without_a_session_shows_a_notice_and_sends_nothing() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(80, 24);
    assert!(sent(slash_quota(&mut app)).is_empty());
    assert!(!app.session_view_open());
    assert!(screen(&app, 80, 24).0.contains("No session on screen."));
    assert_eq!(app.notices.newest(), Some("No session on screen."));
}

#[test]
fn the_fetch_answer_redraws_the_view_and_clears_fetching() {
    let mut app = attached(80, 24);
    app.outbox.link = crate::app::Link::Up;
    let lines = sent(slash_quota(&mut app));
    let id = lines[0]["id"].as_str().unwrap().to_owned();
    app.on_line(session_line(
        SESSION,
        "command_accepted",
        json!({"command_id": id,
            "result": {"quota": [
                {"provider": "codex", "credential": "default",
                    "windows": [{"name": "weekly", "percent_used": 38.0}]},
            ], "notice_at": 80.0}}),
        Some("a_1"),
    ));
    let shown = screen(&app, 80, 24).0;
    assert!(!shown.contains("fetching"), "{shown}");
    assert!(shown.contains("codex · default"), "{shown}");
    assert!(shown.contains("62%"), "{shown}");
}

#[test]
fn the_quota_view_renders_reset_times_against_the_injected_wall_clock() {
    const WALL_MS: u64 = 1_700_000_000_000;
    let mut app = attached(80, 24);
    app.outbox.link = crate::app::Link::Up;
    app.set_now(now(), WALL_MS);
    let lines = sent(slash_quota(&mut app));
    let id = lines[0]["id"].as_str().unwrap().to_owned();
    app.on_line(session_line(
        SESSION,
        "command_accepted",
        json!({"command_id": id,
            "result": {"quota": [
                {"provider": "codex", "credential": "default",
                    "windows": [{"name": "weekly", "percent_used": 38.0,
                        "resets_at": WALL_MS + 192 * 60_000}]},
            ], "notice_at": 80.0}}),
        Some("a_1"),
    ));
    let shown = screen(&app, 80, 24).0;
    assert!(shown.contains("resets in 3h12m"), "{shown}");
}

fn accepted_hooks(id: &str, hooks: serde_json::Value) -> Line {
    session_line(
        SESSION,
        "command_accepted",
        json!({"command_id": id, "result": {"hooks": hooks}}),
        Some("a_1"),
    )
}

fn open_hooks(app: &mut App) -> Value {
    command(app.open_session_view(SessionView::Hooks))
}

/// A mismatched or late hooks answer folds nowhere: once `/hooks` sends a
/// newer query its older answer is stale, and once another view replaces it
/// the late answer belongs to no open view. Flipping the id check would
/// show stale rows; dropping the view check would fill the wrong view.
#[test]
fn a_stale_or_replaced_hooks_answer_folds_nowhere() {
    let mut app = files_attached(80, 24);
    let first = open_hooks(&mut app);
    let first_id = first["id"].as_str().unwrap().to_owned();
    assert_eq!(first["command"], "hooks");
    let second = open_hooks(&mut app);
    let second_id = second["id"].as_str().unwrap().to_owned();
    assert_ne!(first_id, second_id, "a newer query mints a newer id");
    // The older answer arrives after the newer query: it folds nowhere,
    // so the view keeps waiting instead of showing stale rows.
    app.on_line(accepted_hooks(
        &first_id,
        json!([{"kind": "hook", "extension": "stale", "point": "before_tool", "on_failure": "blocking"}]),
    ));
    assert!(
        screen(&app, 80, 24).0.contains("No hooks"),
        "{}",
        screen(&app, 80, 24).0
    );
    // The newer answer fills the view with all three columns.
    app.on_line(accepted_hooks(
        &second_id,
        json!([
            {"kind": "hook", "extension": "ext", "point": "before_tool", "on_failure": "blocking"},
            {"kind": "watcher", "extension": "ext", "kinds": ["turn_completed"]},
        ]),
    ));
    let shown = screen(&app, 80, 24).0;
    assert!(shown.contains("Hooks"), "{shown}");
    assert!(shown.contains("ext"), "{shown}");
    assert!(shown.contains("before_tool"), "{shown}");
    assert!(shown.contains("blocking"), "{shown}");
    assert!(shown.contains("watcher"), "{shown}");
    // Another view replaces `/hooks`: the late answer folds nowhere.
    assert_eq!(app.open_session_view(SessionView::Usage), Effect::None);
    app.on_line(accepted_hooks(
        &second_id,
        json!([{"kind": "hook", "extension": "late", "point": "after_tool", "on_failure": "non-blocking"}]),
    ));
    let shown = screen(&app, 80, 24).0;
    assert!(!shown.contains("late"), "{shown}");
    assert!(shown.contains("Usage"), "{shown}");
}

/// A matching hooks rejection drops the pending query while a mismatched
/// one keeps it: the next answer folds only for the live query. Dropping
/// the clear would fill a refused view with a late answer; flipping the id
/// check would fill it for the wrong query and drop the live one.
#[test]
fn a_matching_hooks_rejection_drops_the_query_and_a_mismatched_one_keeps_it() {
    let mut app = files_attached(80, 24);
    let first = open_hooks(&mut app);
    let first_id = first["id"].as_str().unwrap().to_owned();
    // The matching rejection drops the wait: the late answer folds nowhere.
    app.on_line(rejected_diff(SESSION, &first_id, "gone"));
    app.on_line(accepted_hooks(
        &first_id,
        json!([{"kind": "hook", "extension": "late", "point": "before_tool", "on_failure": "blocking"}]),
    ));
    let shown = screen(&app, 80, 24).0;
    assert!(!shown.contains("late"), "{shown}");
    assert!(shown.contains("No hooks"), "{shown}");
    // A newer query waits again; a mismatched rejection keeps it, so its
    // answer fills the view.
    let second = open_hooks(&mut app);
    let second_id = second["id"].as_str().unwrap().to_owned();
    app.on_line(rejected_diff(SESSION, "c_other", "wrong query"));
    app.on_line(accepted_hooks(
        &second_id,
        json!([{"kind": "hook", "extension": "ext", "point": "before_tool", "on_failure": "blocking"}]),
    ));
    let shown = screen(&app, 80, 24).0;
    assert!(shown.contains("ext"), "{shown}");
    assert!(shown.contains("before_tool"), "{shown}");
}

/// A wheel report at (`col`, `row`): down steps toward the tail, up away
/// from it, over the conversation column the views take.
fn wheel(col: u16, row: u16, down: bool) -> Mouse {
    Mouse {
        kind: if down {
            MouseKind::WheelDown
        } else {
            MouseKind::WheelUp
        },
        col,
        row,
    }
}

/// The wheel over the usage view shifts its visible rows three, never
/// the conversation under it (`docs/tui.md`, "Swapped views"; see
/// #3578). The selection sits mid-window after three Downs, so the
/// window moves under it.
#[test]
fn wheel_over_usage_scrolls_the_list_not_the_conversation() {
    let mut app = attached(80, 24);
    long_turns(&mut app, &mut 0, 24);
    for at in 0..30 {
        app.on_line(usage_line(
            SESSION,
            &format!("g{at}"),
            &format!("model/{at:02}"),
            None,
        ));
    }
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    open_usage(&mut app);
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let frame = app.session_view_screen(0).expect("usage frame");
    assert!(frame.rows.len() >= 4, "the list fits three steps");
    assert_eq!(frame.list.selected(), 3);
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.session_view_screen(0).expect("usage frame");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        frame.list.selected(),
        3,
        "a selection still on screen stays where it was"
    );
}

/// The wheel over the quota view scrolls the view's list, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_quota_scrolls_the_list_not_the_conversation() {
    let mut app = attached(80, 24);
    long_turns(&mut app, &mut 0, 24);
    // Scroll first, while the wheel still reaches the conversation: the
    // slash opens the view, which covers it.
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    app.outbox.link = crate::app::Link::Up;
    let lines = sent(slash_quota(&mut app));
    let id = lines[0]["id"].as_str().unwrap_or_default();
    let windows = |prefix: &str| {
        (1..=14)
            .map(|at| json!({"name": format!("{prefix}{at}"), "percent_used": 10.0 * (at % 9) as f64}))
            .collect::<Vec<_>>()
    };
    app.on_line(session_line(
        SESSION,
        "command_accepted",
        json!({"command_id": id, "result": {"quota": [
            {"provider": "codex", "credential": "default", "windows": windows("w")},
            {"provider": "other", "credential": "c", "windows": windows("v")},
        ], "notice_at": 80.0}}),
        Some("a_1"),
    ));
    assert_eq!(app.open_session_view(SessionView::Quota), Effect::None);
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let frame = app.session_view_screen(80).expect("quota frame");
    assert!(frame.rows.len() >= 4, "the list fits three steps");
    assert_eq!(frame.list.selected(), 3);
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.session_view_screen(80).expect("quota frame");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        frame.list.selected(),
        3,
        "a selection still on screen stays where it was"
    );
}

/// The wheel over the context view scrolls the view's list, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_context_scrolls_the_list_not_the_conversation() {
    // A short screen: the fixed context rows outnumber the shown rows,
    // so the window has room to move three.
    let mut app = attached(80, 16);
    long_turns(&mut app, &mut 0, 16);
    app.on_line(context_preamble());
    app.on_line(context_status(50, 100));
    app.on_line(context_request_started());
    app.on_line(context_usage(None));
    for at in 0..5 {
        for line in context_tool(&format!("read-{at}"), "12345") {
            app.on_line(line);
        }
    }
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    open_context(&mut app);
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let frame = app.session_view_screen(0).expect("context frame");
    assert!(frame.rows.len() >= 4, "the list fits three steps");
    assert_eq!(frame.list.selected(), 3);
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.session_view_screen(0).expect("context frame");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        frame.list.selected(),
        3,
        "a selection still on screen stays where it was"
    );
}

/// The wheel over the status view scrolls the view's list, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_status_scrolls_the_list_not_the_conversation() {
    // A short screen: the fixed status rows outnumber the shown rows,
    // so the window has room to move three.
    let mut app = attached(80, 12);
    long_turns(&mut app, &mut 0, 12);
    app.on_line(session_line(
        SESSION,
        "session_status",
        json!({
            "name": "other", "workspace": "/w", "project": "-w", "state": "streaming",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2}, "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
        Some("a_1"),
    ));
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    assert_eq!(app.open_session_view(SessionView::Status), Effect::None);
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let frame = app.session_view_screen(0).expect("status frame");
    assert!(frame.rows.len() >= 4, "the list fits three steps");
    assert_eq!(frame.list.selected(), 3);
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.session_view_screen(0).expect("status frame");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        frame.list.selected(),
        3,
        "a selection still on screen stays where it was"
    );
}

/// The wheel over the changed-files view scrolls the file list, never
/// the conversation under it (`docs/tui.md`, "Swapped views"; see
/// #3578).
#[test]
fn wheel_over_files_scrolls_the_list_not_the_conversation() {
    let mut app = files_attached(160, 40);
    long_turns(&mut app, &mut 0, 24);
    many_changes(&mut app, 40);
    // Column 30 clears the rail at this width; the view region is the
    // conversation column.
    app.on_wheel(&wheel(30, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    open_files_from_totals(&mut app, 160, 40);
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let frame = app.session_view_screen(0).expect("changed-files frame");
    assert!(frame.rows.len() >= 4, "the list fits three steps");
    assert_eq!(frame.list.selected(), 3);
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(30, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.session_view_screen(0).expect("changed-files frame");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        frame.list.selected(),
        3,
        "a selection still on screen stays where it was"
    );
}

/// The wheel over an open diff scrolls the diff, never the conversation
/// under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_files_diff_scrolls_the_diff_not_the_conversation() {
    let mut app = files_attached(120, 35);
    long_turns(&mut app, &mut 0, 24);
    many_changes(&mut app, 2);
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    open_files_from_totals(&mut app, 120, 35);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    let request = command(app.on_key(Key::Enter, now()));
    assert_eq!(request["args"]["command"], diff_command("src/file-1.rs"));
    let text = (1..=60)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    app.on_line(accepted_diff(
        request["id"].as_str().unwrap_or_default(),
        &text,
    ));
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let frame = app.session_view_screen(0).expect("diff frame");
    assert!(frame.rows.len() >= 4, "the diff fits three steps");
    assert_eq!(frame.list.selected(), 3);
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.session_view_screen(0).expect("diff frame");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        frame.list.selected(),
        3,
        "a selection still on screen stays where it was"
    );
}

/// An attached app with release notes holding `bullets` bullets under
/// version 9.9.9.
fn notes_attached(bullets: &[&str]) -> App {
    let mut notes = String::from("## 9.9.9\n");
    for bullet in bullets {
        notes.push_str(bullet);
        notes.push('\n');
    }
    // The launch owns the only 'static store here: leaking keeps the
    // reference the view reads.
    let notes: &'static str = Box::leak(notes.into_boxed_str());
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        version: "9.9.9".to_owned(),
        release_notes: notes,
        panel_cards: vec!["session".to_owned()],
        ..Default::default()
    });
    app.attach(SessionId(SESSION.to_owned()));
    app.set_size(80, 24);
    app
}

/// The wheel over the release-notes view scrolls the view's list, never
/// the conversation under it (`docs/tui.md`, "Swapped views"; see
/// #3578).
#[test]
fn wheel_over_release_notes_scrolls_the_list_not_the_conversation() {
    let mut app = notes_attached(&[
        "- one",
        "- two",
        "- three",
        "- four",
        "- five",
        "- six",
        "- seven",
        "- eight",
        "- nine",
        "- ten",
        "- eleven",
        "- twelve",
        "- thirteen",
        "- fourteen",
        "- fifteen",
        "- sixteen",
        "- seventeen",
        "- eighteen",
        "- nineteen",
        "- twenty",
        "- twenty-one",
        "- twenty-two",
        "- twenty-three",
        "- twenty-four",
        "- twenty-five",
        "- twenty-six",
        "- twenty-seven",
        "- twenty-eight",
        "- twenty-nine",
        "- thirty",
        "- thirty-one",
        "- thirty-two",
    ]);
    long_turns(&mut app, &mut 0, 24);
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    assert_eq!(
        app.open_session_view(SessionView::ReleaseNotes),
        Effect::None
    );
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let frame = app.session_view_screen(80).expect("notes frame");
    assert!(frame.rows.len() >= 4, "the list fits three steps");
    assert_eq!(frame.list.selected(), 3);
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.session_view_screen(80).expect("notes frame");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        frame.list.selected(),
        3,
        "a selection still on screen stays where it was"
    );
}

/// The wheel over the delegates view scrolls the view's list, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_delegates_scrolls_the_list_not_the_conversation() {
    let mut app = attached(80, 24);
    long_turns(&mut app, &mut 0, 24);
    for n in 1..=26 {
        start_delegate(&mut app, &format!("j_{n}"), "s_dddddddddddddddd");
    }
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    open_delegates(&mut app);
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    assert_eq!(selected(&app), 3);
    let shown_top = app.session_view_screen(0).expect("a list frame").list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert_eq!(
        app.session_view_screen(0).expect("a list frame").list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        selected(&app),
        3,
        "a selection still on screen stays where it was"
    );
}

/// The wheel over the jobs view scrolls the view's list, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_jobs_scrolls_the_list_not_the_conversation() {
    let mut app = attached(80, 24);
    long_turns(&mut app, &mut 0, 24);
    for n in 1..=26 {
        start_job(&mut app, &format!("j_{n}"));
    }
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    open_jobs(&mut app);
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    assert_eq!(selected(&app), 3);
    let shown_top = app.session_view_screen(0).expect("a list frame").list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert_eq!(
        app.session_view_screen(0).expect("a list frame").list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        selected(&app),
        3,
        "a selection still on screen stays where it was"
    );
}

/// The wheel over the all-delegates view scrolls the view's list, never
/// the conversation under it (`docs/tui.md`, "Swapped views"; see
/// #3578).
#[test]
fn wheel_over_all_delegates_scrolls_the_list_not_the_conversation() {
    let mut app = attached(80, 24);
    long_turns(&mut app, &mut 0, 24);
    for n in 1..=26 {
        start_delegate(&mut app, &format!("j_{n}"), "s_dddddddddddddddd");
    }
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    open_all_delegates(&mut app);
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let before = selected(&app);
    assert_eq!(before, 3);
    let frame = app.session_view_screen(0).expect("delegates frame");
    assert!(frame.rows.len() >= before + 4, "the list fits three steps");
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.session_view_screen(0).expect("delegates frame");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        selected(&app),
        before,
        "a selection still on screen stays where it was"
    );
}

/// The wheel over the hooks view scrolls the view's list, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_hooks_scrolls_the_list_not_the_conversation() {
    let mut app = attached(80, 24);
    long_turns(&mut app, &mut 0, 24);
    // Scroll first, while the wheel still reaches the conversation: the
    // view covers it once open.
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    app.outbox.link = crate::app::Link::Up;
    let query = open_hooks(&mut app);
    let id = query["id"].as_str().unwrap_or_else(|| panic!("a query id"));
    app.on_line(accepted_hooks(
        id,
        json!([
            {"kind": "hook", "extension": "a", "point": "before_tool", "on_failure": "blocking"},
            {"kind": "hook", "extension": "b", "point": "before_tool", "on_failure": "blocking"},
            {"kind": "hook", "extension": "c", "point": "after_tool", "on_failure": "non-blocking"},
            {"kind": "watcher", "extension": "d", "kinds": ["turn_completed"]},
            {"kind": "watcher", "extension": "e", "kinds": ["turn_completed"]},
            {"kind": "hook", "extension": "f", "point": "before_tool", "on_failure": "blocking"},
            {"kind": "hook", "extension": "g", "point": "after_tool", "on_failure": "non-blocking"},
            {"kind": "watcher", "extension": "h", "kinds": ["turn_completed"]},
            {"kind": "hook", "extension": "i", "point": "before_tool", "on_failure": "blocking"},
            {"kind": "watcher", "extension": "j", "kinds": ["turn_completed"]},
            {"kind": "hook", "extension": "k", "point": "before_tool", "on_failure": "blocking"},
            {"kind": "watcher", "extension": "l", "kinds": ["turn_completed"]},
            {"kind": "hook", "extension": "m", "point": "after_tool", "on_failure": "non-blocking"},
            {"kind": "watcher", "extension": "n", "kinds": ["turn_completed"]},
            {"kind": "hook", "extension": "o", "point": "before_tool", "on_failure": "blocking"},
            {"kind": "watcher", "extension": "p", "kinds": ["turn_completed"]},
            {"kind": "hook", "extension": "q", "point": "after_tool", "on_failure": "non-blocking"},
            {"kind": "watcher", "extension": "r", "kinds": ["turn_completed"]},
            {"kind": "hook", "extension": "s", "point": "before_tool", "on_failure": "blocking"},
            {"kind": "watcher", "extension": "t", "kinds": ["turn_completed"]},
            {"kind": "hook", "extension": "u", "point": "after_tool", "on_failure": "non-blocking"},
            {"kind": "watcher", "extension": "v", "kinds": ["turn_completed"]},
            {"kind": "hook", "extension": "w", "point": "before_tool", "on_failure": "blocking"},
            {"kind": "watcher", "extension": "x", "kinds": ["turn_completed"]},
            {"kind": "hook", "extension": "y", "point": "after_tool", "on_failure": "non-blocking"},
            {"kind": "watcher", "extension": "z", "kinds": ["turn_completed"]},
        ]),
    ));
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let before = selected(&app);
    assert_eq!(before, 3);
    let frame = app.session_view_screen(0).expect("hooks frame");
    assert!(frame.rows.len() >= before + 4, "the list fits three steps");
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.session_view_screen(0).expect("hooks frame");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        selected(&app),
        before,
        "a selection still on screen stays where it was"
    );
}

#[test]
fn the_matching_numstat_answer_shows_git_figures_in_the_rows() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 2);
    let (_, targets) = screen(&app, 120, 35);
    assert!(
        targets
            .iter()
            .any(|target| { target.id == TargetId::Panel(PanelSpot::ChangedFiles) })
    );
    let effect = app.on_click(TargetId::Panel(PanelSpot::ChangedFiles));
    let Effect::Send(lines) = effect else {
        panic!("opening the list sends the numstat query, got {effect:?}");
    };
    let request: Value = serde_json::from_str(&lines[0])
        .unwrap_or_else(|error| panic!("shell request JSON: {error}"));
    let shown = screen(&app, 120, 35).0;
    assert!(shown.contains("src/file-0.rs · +2 −0"), "{shown}");
    app.on_line(accepted_figures(
        request["id"].as_str().unwrap_or_default(),
        "5\t0\tsrc/file-0.rs\n0\t3\tsrc/file-1.rs\nExit code 0.\n",
        0,
        None,
        false,
    ));
    let shown = screen(&app, 120, 35).0;
    assert!(shown.contains("src/file-0.rs · +5 −0"), "{shown}");
    assert!(shown.contains("src/file-1.rs · +0 −3"), "{shown}");
}

#[test]
fn a_rejected_numstat_query_keeps_the_card_figures() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 1);
    let (_, targets) = screen(&app, 120, 35);
    assert!(
        targets
            .iter()
            .any(|target| { target.id == TargetId::Panel(PanelSpot::ChangedFiles) })
    );
    let effect = app.on_click(TargetId::Panel(PanelSpot::ChangedFiles));
    let Effect::Send(lines) = effect else {
        panic!("opening the list sends the numstat query, got {effect:?}");
    };
    let request: Value = serde_json::from_str(&lines[0])
        .unwrap_or_else(|error| panic!("shell request JSON: {error}"));
    app.on_line(rejected_diff(
        SESSION,
        request["id"].as_str().unwrap_or_default(),
        "hub refused",
    ));
    let shown = screen(&app, 120, 35).0;
    assert!(shown.contains("src/file-0.rs · +1 −0"), "{shown}");
}

fn files_attached_in(root: &std::path::Path, width: u16, height: u16) -> App {
    let mut app = App::new(root.to_path_buf());
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: root.to_path_buf(),
            project: "-w".to_owned(),
            git: false,
        },
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

fn open_figures(app: &mut App, width: u16, height: u16) -> String {
    let (_, targets) = screen(app, width, height);
    assert!(
        targets
            .iter()
            .any(|target| { target.id == TargetId::Panel(PanelSpot::ChangedFiles) })
    );
    let effect = app.on_click(TargetId::Panel(PanelSpot::ChangedFiles));
    let Effect::Send(lines) = effect else {
        panic!("opening the list sends the numstat query, got {effect:?}");
    };
    assert_eq!(lines.len(), 1, "one numstat command"); // docs/tui.md:830, run when the view opens.
    let request: Value = serde_json::from_str(&lines[0])
        .unwrap_or_else(|error| panic!("shell request JSON: {error}"));
    request["id"].as_str().unwrap_or_default().to_owned()
}

fn accepted_figures(
    id: &str,
    text: &str,
    exit_code: i32,
    signal: Option<&str>,
    timed_out: bool,
) -> Line {
    session_line(
        SESSION,
        "command_accepted",
        json!({
            "command_id": id,
            "result": {"output": text, "process": {"exit_code": exit_code, "signal": signal, "timed_out": timed_out}},
        }),
        Some("a_1"),
    )
}

#[test]
fn a_numstat_answer_ended_by_a_signal_keeps_the_card_figures() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 1);
    let id = open_figures(&mut app, 120, 35);
    app.on_line(accepted_figures(
        &id,
        "5\t0\tsrc/file-0.rs\n",
        0,
        Some("SIGKILL"),
        false,
    ));
    let shown = screen(&app, 120, 35).0;
    assert!(shown.contains("src/file-0.rs · +1 −0"), "{shown}");
}

#[test]
fn a_timed_out_numstat_answer_keeps_the_card_figures() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 1);
    let id = open_figures(&mut app, 120, 35);
    app.on_line(accepted_figures(
        &id,
        "5\t0\tsrc/file-0.rs\n",
        0,
        None,
        true,
    ));
    let shown = screen(&app, 120, 35).0;
    assert!(shown.contains("src/file-0.rs · +1 −0"), "{shown}");
}

#[test]
fn a_numstat_answer_with_a_failure_exit_keeps_the_card_figures() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 1);
    let id = open_figures(&mut app, 120, 35);
    app.on_line(accepted_figures(
        &id,
        "5\t0\tsrc/file-0.rs\n",
        1,
        None,
        false,
    ));
    let shown = screen(&app, 120, 35).0;
    assert!(shown.contains("src/file-0.rs · +1 −0"), "{shown}");
}

#[test]
fn an_empty_numstat_answer_with_exit_zero_counts_untracked_lines() {
    let dir = fakes::TempDir::new("tui-numstat-empty-success");
    std::fs::create_dir_all(dir.path().join("src"))
        .unwrap_or_else(|error| panic!("workspace src: {error}"));
    std::fs::write(dir.path().join("src/file-0.rs"), "a\nb\nc\nd\n")
        .unwrap_or_else(|error| panic!("workspace file: {error}"));
    let mut app = files_attached_in(dir.path(), 120, 35);
    many_changes(&mut app, 1);
    let id = open_figures(&mut app, 120, 35);
    app.on_line(accepted_figures(&id, "Exit code 0.\n", 0, None, false));
    let shown = screen(&app, 120, 35).0;
    assert!(shown.contains("untracked, wholly added"), "{shown}");
    assert!(shown.contains("1 file changed  +4 −0"), "{shown}");
}

#[test]
fn an_empty_numstat_answer_with_a_failure_exit_keeps_the_card_figures() {
    let dir = fakes::TempDir::new("tui-numstat-empty-failure");
    std::fs::create_dir_all(dir.path().join("src"))
        .unwrap_or_else(|error| panic!("workspace src: {error}"));
    std::fs::write(dir.path().join("src/file-0.rs"), "a\nb\nc\nd\n")
        .unwrap_or_else(|error| panic!("workspace file: {error}"));
    let mut app = files_attached_in(dir.path(), 120, 35);
    many_changes(&mut app, 1);
    let id = open_figures(&mut app, 120, 35);
    app.on_line(accepted_figures(&id, "", 128, None, false));
    let shown = screen(&app, 120, 35).0;
    assert!(shown.contains("src/file-0.rs · +1 −0"), "{shown}");
}

#[test]
fn a_refused_numstat_query_drops_its_late_answer() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 1);
    let id = open_figures(&mut app, 120, 35);
    app.on_line(rejected_diff(SESSION, &id, "hub refused"));
    app.on_line(accepted_figures(
        &id,
        "5\t0\tsrc/file-0.rs\nExit code 0.\n",
        0,
        None,
        false,
    ));
    let shown = screen(&app, 120, 35).0;
    assert!(shown.contains("src/file-0.rs · +1 −0"), "{shown}");
}

#[test]
fn a_refusal_for_another_command_keeps_the_numstat_query_pending() {
    let mut app = files_attached(120, 35);
    many_changes(&mut app, 1);
    let id = open_figures(&mut app, 120, 35);
    app.on_line(rejected_diff(SESSION, "c_other", "wrong command"));
    app.on_line(accepted_figures(
        &id,
        "5\t0\tsrc/file-0.rs\nExit code 0.\n",
        0,
        None,
        false,
    ));
    let shown = screen(&app, 120, 35).0;
    assert!(shown.contains("src/file-0.rs · +5 −0"), "{shown}");
}
