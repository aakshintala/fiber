//! Tests for `/usage` on the app: its dispatch, keys, clicks and session fold
//! (`docs/tui.md`, "Swapped views").

use std::path::PathBuf;
use std::sync::Arc;

use crate::app::panel::Spot as PanelSpot;
use crate::app::{App, Effect, SessionView};
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
