//! Tests for the item fold and the screen swap: opening swaps a fresh
//! screen in, the attached session's lines fold into the stashed screen,
//! and closing swaps back with a `summary` wish the reconciler lowers.

use std::time::Duration;

use contract::clock::Clock;
use contract::{JobId, SessionId};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::super::{App, Effect};
use super::retry::RETRY;
use super::testkit::{
    DELEGATE_A, SESSION, ack_all, commands, complete, home, job_started, live, mark_tty, open,
    opened, opened_item, session_accepted, session_line, start_delegate, start_job, subscribes,
};
use crate::home::Level;
use crate::keys::{Key, Mouse, MouseKind};
use crate::link::Line;

const DELEGATE_B: &str = "s_eeeeeeeeeeeeeeee";

/// One envelope of another session.
fn other_line(session: &str, kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A session `command_rejected` for `id` from `session` with `code`.
fn session_refused(session: &str, id: &str, code: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_rejected".to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("code".to_owned(), Value::String(code.to_owned())),
            (
                "message".to_owned(),
                Value::String("no such session".to_owned()),
            ),
        ]
        .into_iter()
        .collect(),
    })
}

/// The resident conversation's rows as text.
fn shown(app: &App) -> Vec<String> {
    app.lines().iter().map(|line| line.to_string()).collect()
}

/// Drives the clock: the frame time `now` reads from `clock`.
fn tick(app: &mut App, clock: &std::sync::Arc<fakes::clock::FakeClock>) {
    app.set_now(clock.now(), contract::clock::wall_ms(clock.wall()));
}

fn renderer_turn(app: &mut App, session: &str) {
    for (seq, (kind, payload)) in [
        ("turn_started", json!({"input":[]})),
        ("step_started", json!({})),
        ("assistant_message_started", json!({})),
        (
            "tool_call_requested",
            json!({"name":"shell","arguments":{"command":"echo row"}}),
        ),
        (
            "assistant_message_completed",
            json!({"outcome":"completed"}),
        ),
        ("turn_completed", json!({"outcome":"completed"})),
    ]
    .into_iter()
    .enumerate()
    {
        let Line::Session(mut line) = other_line(session, kind, payload) else {
            unreachable!()
        };
        line.seq = Some(contract::Seq(seq as u64));
        app.on_line(Line::Session(line));
    }
}
fn rendered_text(app: &App) -> String {
    app.screen
        .pages()
        .page_text_open(0)
        .unwrap()
        .0
        .iter()
        .map(|(line, _)| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}
#[test]
fn delegate_gate_notice_redraw_and_reload_reach_both_screens() {
    let mut app = home();
    opened(&mut app);
    let mut fake = crate::ext::tests::SessionRenderer::new("DELEGATE-RENDER");
    fake.gated = true;
    let fake = std::rc::Rc::new(fake);
    app.use_renderer(fake.clone());
    renderer_turn(&mut app, SESSION);
    start_delegate(&mut app, "j_renderer", DELEGATE_A);
    open(&mut app, "j_renderer");
    renderer_turn(&mut app, DELEGATE_A);
    assert!(!rendered_text(&app).contains("DELEGATE-RENDER"));
    app.on_line(other_line(
        DELEGATE_A,
        "extensions_loaded",
        json!({"extensions":[]}),
    ));
    rendered_text(&app);
    assert!(app.drawn(&[]), "selection notice requests immediate redraw");
    assert!(!app.drawn(&[]));
    assert!(app.notices().iter().any(|line|matches!(line,crate::app::NoticeLine::Notice{code,..} if code=="extension_incompatible")));
    app.on_line(other_line(
        DELEGATE_A,
        "extensions_loaded",
        json!({"extensions":[{"name":"session-test","version":"v1.0.0"}]}),
    ));
    assert!(rendered_text(&app).contains("DELEGATE-RENDER"));
    assert!(
        fake.calls
            .borrow()
            .iter()
            .any(|session| session.as_deref() == Some(DELEGATE_A))
    );
    let parent_recounts = app.attached_screen().pages().recounts;
    let delegate_recounts = app.screen.pages().recounts;
    let loader = crate::ext::tests::TestLoad::new("RELOADED-DELEGATE");
    app.home.as_mut().unwrap().launch.extensions = Some(loader.clone());
    crate::app::commands::run_reload(&mut app, "");
    assert!(app.attached_screen().pages().recounts > parent_recounts);
    assert!(app.screen.pages().recounts > delegate_recounts);
    assert!(rendered_text(&app).contains("RELOADED-DELEGATE"));
    app.close_item();
    assert!(rendered_text(&app).contains("RELOADED-DELEGATE"));
}
#[test]
fn reload_preserves_delegate_selection_notice_before_parent_recount() {
    struct GatedLoad;
    impl crate::ext::Load for GatedLoad {
        fn load(&self) -> (std::rc::Rc<dyn crate::ext::Renderer>, Vec<(String, String)>) {
            let mut renderer = crate::ext::tests::SessionRenderer::new("replacement");
            renderer.gated = true;
            (std::rc::Rc::new(renderer), Vec::new())
        }
    }
    let mut app = home();
    opened(&mut app);
    app.use_renderer(std::rc::Rc::new(crate::ext::tests::SessionRenderer::new(
        "OLD-ROW",
    )));
    app.screen.pages_mut().toggle_ledgers();
    renderer_turn(&mut app, SESSION);
    assert!(
        shown(&app).iter().any(|row| row.contains("OLD-ROW")),
        "parent ledger is open"
    );
    start_delegate(&mut app, "j_reload_pending", DELEGATE_A);
    open(&mut app, "j_reload_pending");
    renderer_turn(&mut app, DELEGATE_A);
    assert!(
        shown(&app).iter().any(|row| row.contains("OLD-ROW")),
        "delegate ledger is open"
    );
    app.on_line(other_line(
        DELEGATE_A,
        "extensions_loaded",
        json!({"extensions":[]}),
    ));
    assert!(!app.drawn(&[]), "the old renderer queued no notices");
    app.home.as_mut().unwrap().launch.extensions = Some(std::sync::Arc::new(GatedLoad));
    crate::app::commands::run_reload(&mut app, "");
    // docs/tui.md: the incompatible half reports once, and the post-draw
    // drain requests a frame immediately, without another input or render.
    assert!(
        app.drawn(&[]),
        "reload recount's selection notice requests immediate redraw"
    );
    let notices = app.notices();
    assert_eq!(notices.iter().filter(|line| matches!(line, crate::app::NoticeLine::Notice {code,text,..} if code == "extension_incompatible" && text.contains("session-test") && text.contains("not loaded"))).count(), 1);
    shown(&app);
    assert!(
        !app.drawn(&[]),
        "producer dedup and draining keep delivery exactly once"
    );
    app.close_item();
    shown(&app);
    assert!(
        !app.drawn(&[]),
        "the unseen parent adds no notice after swapping back"
    );
}

#[test]
fn parent_page_pruning_cannot_erase_delegate_failure_before_app_drain() {
    let mut app = home();
    opened(&mut app);
    renderer_turn(&mut app, SESSION);
    let mut fake = crate::ext::tests::SessionRenderer::new("fail");
    fake.failure = Some(crate::ext::FailureKind::HookFailed);
    app.install_renderer(std::rc::Rc::new(fake));
    start_delegate(&mut app, "j_failure", DELEGATE_A);
    open(&mut app, "j_failure");
    renderer_turn(&mut app, DELEGATE_A);
    rendered_text(&app);
    app.attached_screen()
        .pages()
        .ext
        .cache
        .drop_page(crate::ext::cache::PageKey {
            session: SessionId(SESSION.into()),
            index: 0,
        });
    assert!(app.drawn(&[]));
    let notices = app.notices();
    // docs/tui.md:2734: the failure keeps one hook_failed per extension and slot.
    assert_eq!(notices.iter().filter(|line|matches!(line,crate::app::NoticeLine::Notice{code,text,..} if code=="hook_failed"&&text.contains("session-test"))).count(),1);
    assert!(!app.drawn(&[]));
}

#[test]
fn delegate_extensions_loaded_marks_swapped_transcript_changed() {
    // An `extensions_loaded` line marks the shown delegate transcript even
    // when the fold itself changes nothing: `||` (not `&&`) with the kind
    // check, while the delegate is swapped in.
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_changed", DELEGATE_A);
    open(&mut app, "j_changed");
    app.jump(1);
    assert!(app.screen.top().is_some());
    assert!(!app.screen.has_new());
    app.on_line(other_line(
        DELEGATE_A,
        "extensions_loaded",
        json!({"extensions": []}),
    ));
    assert!(
        app.screen.has_new(),
        "extensions_loaded marks the shown delegate transcript changed"
    );
    assert!(
        !app.attached_screen().has_new(),
        "the stashed parent transcript stays unchanged"
    );
}

#[test]
fn a_rename_reaches_the_stashed_parent_transcript() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_names", DELEGATE_A);
    open(&mut app, "j_names");
    // The parent folds a turn carrying the sender's message while its
    // screen is stashed behind the delegate's.
    app.on_line(other_line(
        SESSION,
        "turn_started",
        json!({"input": [
            {"type": "message", "source": "driver",
             "content": [{"type": "text", "text": "go"}]},
            {"type": "message", "source": "session",
             "from_session_id": crate::app::testkit::SENDER,
             "content": [{"type": "text", "text": "please check"}]},
        ]}),
    ));
    let parent_rows = |app: &App| {
        app.attached_screen()
            .pages()
            .rows()
            .iter()
            .map(|(line, _)| line.to_string())
            .collect::<Vec<_>>()
    };
    let rows = parent_rows(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains(crate::app::testkit::SENDER)),
        "the id labels the stashed message: {rows:?}"
    );
    app.on_line(crate::app::testkit::sender_status("worker", true));
    let rows = parent_rows(&app);
    assert!(
        rows.iter().any(|row| row.contains("worker · 00:00")),
        "the stashed parent follows the name: {rows:?}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.contains(crate::app::testkit::SENDER)),
        "the id label is gone: {rows:?}"
    );
}

#[test]
fn delegate_other_line_leaves_swapped_transcript_unchanged() {
    // Any other line kind leaves the shown delegate transcript alone when
    // the fold changes nothing: the kind check is `==` (not `!=`), while
    // the delegate is swapped in.
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_quiet", DELEGATE_A);
    open(&mut app, "j_quiet");
    app.jump(1);
    assert!(app.screen.top().is_some());
    assert!(!app.screen.has_new());
    app.on_line(other_line(DELEGATE_A, "step_started", json!({})));
    assert!(
        !app.screen.has_new(),
        "a non-extensions_loaded line marks nothing when the fold is quiet"
    );
}

#[test]
fn opening_swaps_in_a_fresh_screen() {
    let mut app = home();
    opened(&mut app);
    app.on_line(session_line(
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "fix it"}]}]}),
    ));
    assert!(!shown(&app).is_empty());
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    assert!(app.item_open());
    // A fresh screen: the parent's turn is not shown.
    assert!(shown(&app).is_empty());
    assert_eq!(subscribes(&out).len(), 1);
    assert_eq!(out[0]["args"]["level"], "full");
}

#[test]
fn an_attached_turn_while_open_lands_in_the_parent_and_shows_after_close() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    ack_all(&mut app, &out);
    let before = shown(&app).len();
    app.on_line(session_line(
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "meanwhile"}]}]}),
    ));
    // The delegate's screen is untouched by the attached session's line.
    assert_eq!(shown(&app).len(), before);
    app.close_item();
    assert!(shown(&app).len() > before);
}

#[test]
fn a_resize_while_open_applies_to_both_screens() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    open(&mut app, "j_1");
    app.set_size(100, 30);
    app.close_item();
    assert_eq!(app.conversation_area().width, 98);
}

#[test]
fn go_home_closes_the_view_first() {
    let mut app = home();
    opened(&mut app);
    app.on_line(session_line(
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "fix it"}]}]}),
    ));
    start_delegate(&mut app, "j_1", DELEGATE_A);
    open(&mut app, "j_1");
    assert!(app.item_open());
    app.go_home();
    assert!(!app.item_open());
}

#[test]
fn disconnected_closes_the_view() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    open(&mut app, "j_1");
    assert!(app.item_open());
    app.disconnected();
    assert!(!app.item_open());
}

#[test]
fn a_close_then_items_due_lowers_while_running() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    ack_all(&mut app, &out);
    app.close_item();
    let due = commands(app.items_due(clock.now()));
    let lowered = subscribes(&due);
    assert_eq!(lowered.len(), 1);
    assert_eq!(lowered[0]["session_id"], DELEGATE_A);
    assert_eq!(lowered[0]["args"]["level"], "summary");
}

#[test]
fn a_close_then_items_due_lowers_without_the_card_listed() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    app.home
        .as_mut()
        .unwrap_or_else(|| panic!("home"))
        .launch
        .panel_cards = vec!["session".to_owned()];
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    ack_all(&mut app, &out);
    app.close_item();
    let due = commands(app.items_due(clock.now()));
    assert_eq!(subscribes(&due).len(), 1);
}

#[test]
fn a_close_then_items_due_sends_nothing_once_completed() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    ack_all(&mut app, &out);
    complete(&mut app, "j_1");
    app.close_item();
    assert!(commands(app.items_due(clock.now())).is_empty());
}

#[test]
fn opening_closing_and_reopening_sends_full_summary_full() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let first = open(&mut app, "j_1");
    assert_eq!(subscribes(&first).len(), 1);
    ack_all(&mut app, &first);
    app.close_item();
    let lowered = commands(app.items_due(clock.now()));
    assert_eq!(subscribes(&lowered).len(), 1);
    ack_all(&mut app, &lowered);
    let second = open(&mut app, "j_1");
    let raised = subscribes(&second);
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0]["args"]["level"], "full");
    // The reopened view replays: a `turn_started` after the second `full`
    // acknowledgement shows.
    ack_all(&mut app, &second);
    app.on_line(other_line(
        DELEGATE_A,
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "again"}]}]}),
    ));
    assert!(!shown(&app).is_empty());
}

#[test]
fn reopening_before_the_lowering_goes_out_sends_the_pair() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let first = open(&mut app, "j_1");
    ack_all(&mut app, &first);
    app.close_item();
    // No `items_due` runs between the close and the reopen: the
    // connection still holds `full`, so the pair goes out in one batch.
    let second = open(&mut app, "j_1");
    let pair = subscribes(&second);
    assert_eq!(pair.len(), 2);
    assert_eq!(pair[0]["args"]["level"], "summary");
    assert_eq!(pair[1]["args"]["level"], "full");
}

#[test]
fn opening_sends_nothing_while_a_subscribe_is_in_flight() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    // The card's `summary` subscribe goes out and stays in flight.
    let card = commands(app.on_line(live(SESSION, json!({"state": "streaming"}))));
    assert_eq!(subscribes(&card).len(), 1);
    let out = open(&mut app, "j_1");
    assert!(app.item_open());
    assert!(subscribes(&out).is_empty());
}

#[test]
fn opening_a_second_item_closes_the_first() {
    let mut app = home();
    opened(&mut app);
    app.on_line(session_line(
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "fix it"}]}]}),
    ));
    let parent_rows = shown(&app).len();
    assert!(parent_rows > 0);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    start_delegate(&mut app, "j_2", DELEGATE_B);
    open(&mut app, "j_1");
    open(&mut app, "j_2");
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert_eq!(view.description, "task j_2");
    // The first view closed: closing the second restores the parent.
    app.close_item();
    assert_eq!(shown(&app).len(), parent_rows);
}

#[test]
fn a_pair_refused_invalid_arguments_shows_the_message() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let first = open(&mut app, "j_1");
    ack_all(&mut app, &first);
    app.close_item();
    let second = open(&mut app, "j_1");
    let pair = subscribes(&second);
    assert_eq!(pair.len(), 2);
    let summary_id = pair[0]["id"].as_str().unwrap_or_else(|| panic!("an id"));
    let full_id = pair[1]["id"].as_str().unwrap_or_else(|| panic!("an id"));
    app.on_line(session_refused(DELEGATE_A, summary_id, "invalid_arguments"));
    app.on_line(session_refused(DELEGATE_A, full_id, "invalid_arguments"));
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert!(
        view.message
            .as_deref()
            .is_some_and(|message| message.contains("Could not open"))
    );
}

#[test]
fn a_pair_refused_session_not_found_retries_while_running() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let first = open(&mut app, "j_1");
    ack_all(&mut app, &first);
    app.close_item();
    let second = open(&mut app, "j_1");
    let pair = subscribes(&second);
    assert_eq!(pair.len(), 2);
    let summary_id = pair[0]["id"].as_str().unwrap_or_else(|| panic!("an id"));
    let full_id = pair[1]["id"].as_str().unwrap_or_else(|| panic!("an id"));
    tick(&mut app, &clock);
    app.on_line(session_refused(DELEGATE_A, summary_id, "session_not_found"));
    app.on_line(session_refused(DELEGATE_A, full_id, "session_not_found"));
    // The retry waits: nothing goes out before its delay.
    assert!(commands(app.items_due(clock.now())).is_empty());
    // No code under test waits on the clock here; it moves the frame time the app reads (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_millis(500));
    tick(&mut app, &clock);
    let resent = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(resent.len(), 1);
    assert_eq!(resent[0]["args"]["level"], "full");
}

#[test]
fn a_refusal_after_completion_sets_no_retry() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let first = open(&mut app, "j_1");
    let full_id = subscribes(&first)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    complete(&mut app, "j_1");
    app.on_line(session_refused(DELEGATE_A, &full_id, "session_not_found"));
    assert!(commands(app.items_due(clock.now())).is_empty());
    assert!(app.items_wake().is_none());
}

#[test]
fn a_summary_held_then_a_full_refused_shows_the_message() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let first = open(&mut app, "j_1");
    ack_all(&mut app, &first);
    app.close_item();
    let second = open(&mut app, "j_1");
    let pair = subscribes(&second);
    assert_eq!(pair.len(), 2);
    let summary_id = pair[0]["id"].as_str().unwrap_or_else(|| panic!("an id"));
    let full_id = pair[1]["id"].as_str().unwrap_or_else(|| panic!("an id"));
    app.on_line(session_accepted(DELEGATE_A, summary_id));
    app.on_line(session_refused(DELEGATE_A, full_id, "invalid_arguments"));
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert!(
        view.message
            .as_deref()
            .is_some_and(|message| message.contains("Could not open"))
    );
    assert_eq!(
        app.subscribed_level(&SessionId(DELEGATE_A.to_owned())),
        Some(Level::Summary)
    );
}

#[test]
fn leaving_with_full_held_lowers_after() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    ack_all(&mut app, &out);
    app.leave();
    let due = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0]["session_id"], DELEGATE_A);
    assert_eq!(due[0]["args"]["level"], "summary");
}

#[test]
fn leaving_with_full_in_flight_lowers_once_on_its_acknowledgement() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let full_id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    app.leave();
    // While the `full` is in flight the lowering waits and asks no wake.
    assert!(commands(app.items_due(clock.now())).is_empty());
    app.on_line(session_accepted(DELEGATE_A, &full_id));
    let due = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0]["args"]["level"], "summary");
    ack_all(&mut app, &due);
    assert!(commands(app.items_due(clock.now())).is_empty());
}

#[test]
fn a_refused_lowering_past_leaving_drops_with_no_retry() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let full_id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    app.leave();
    app.on_line(session_accepted(DELEGATE_A, &full_id));
    let due = commands(app.items_due(clock.now()));
    let lowering = subscribes(&due);
    assert_eq!(lowering.len(), 1);
    let lowering_id = lowering[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("an id"))
        .to_owned();
    app.on_line(session_refused(
        DELEGATE_A,
        &lowering_id,
        "session_not_found",
    ));
    assert!(commands(app.items_due(clock.now())).is_empty());
}

#[test]
fn closing_while_full_is_in_flight_lowers_after_home_on_its_acknowledgement() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let full_id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    // Esc closes the view while the `full` is in flight.
    app.on_key(Key::Esc, clock.now());
    assert!(!app.item_open());
    app.leave();
    app.on_line(session_accepted(DELEGATE_A, &full_id));
    let due = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0]["session_id"], DELEGATE_A);
}

#[test]
fn opening_another_item_then_leaving_lowers_both_once_acknowledged() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    start_delegate(&mut app, "j_2", DELEGATE_B);
    let first = open(&mut app, "j_1");
    let first_id = subscribes(&first)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    // Opening the second closes the first while its `full` is in flight.
    let second = open(&mut app, "j_2");
    let second_id = subscribes(&second)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    assert_eq!(subscribes(&second).len(), 1);
    app.leave();
    // While either `full` is in flight its lowering waits.
    assert!(commands(app.items_due(clock.now())).is_empty());
    app.on_line(session_accepted(DELEGATE_A, &first_id));
    let due = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0]["session_id"], DELEGATE_A);
    app.on_line(session_accepted(DELEGATE_B, &second_id));
    let due = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0]["session_id"], DELEGATE_B);
    assert_eq!(due[0]["args"]["level"], "summary");
}

#[test]
fn the_delegate_turn_draws_in_the_view_and_leaves_the_parent_idle() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.on_line(other_line(
        DELEGATE_A,
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "review it"}]}]}),
    ));
    app.on_line(other_line(
        DELEGATE_A,
        "text_completed",
        json!({"text": "looks good"}),
    ));
    assert!(shown(&app).iter().any(|row| row.contains("looks good")));
    // A delegate line never changes the attached busy flag.
    assert!(matches!(
        app.phase,
        super::super::Phase::Attached { busy: false, .. }
    ));
}

#[test]
fn enter_sends_exactly_one_steer_naming_the_delegate() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.draft.set("focus on the tests");
    let out = match app.on_key(Key::Enter, clock.now()) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_)
        | Effect::Feedback { .. } => panic!("Enter sends"),
    };
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["command"], "steer");
    assert_eq!(out[0]["session_id"], DELEGATE_A);
    assert_eq!(
        out[0]["args"],
        json!({"content": [{"type": "text", "text": "focus on the tests"}]})
    );
    assert!(app.draft.is_empty());
}

#[test]
fn a_rejected_steer_shows_its_notice_and_puts_the_draft_back() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.draft.set("focus on the tests");
    let out = match app.on_key(Key::Enter, clock.now()) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_)
        | Effect::Feedback { .. } => panic!("Enter sends"),
    };
    let id = out[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("an id"))
        .to_owned();
    app.on_line(session_refused(DELEGATE_A, &id, "busy"));
    assert_eq!(app.draft(), "focus on the tests");
}

#[test]
fn a_slash_close_in_the_view_closes_the_parent_and_the_view() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.draft.set("/close");
    let out = match app.on_key(Key::Enter, clock.now()) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_)
        | Effect::Feedback { .. } => panic!("/close sends"),
    };
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["command"], "close");
    assert_eq!(out[0]["session_id"], SESSION);
    assert_eq!(out[0]["args"], json!({"now": true}));
    assert!(!app.item_open());
}

#[test]
fn enter_on_a_completed_delegate_keeps_the_draft() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    complete(&mut app, "j_1");
    app.draft.set("one more thing");
    let effect = app.on_key(Key::Enter, clock.now());
    assert_eq!(effect, Effect::None);
    assert_eq!(app.draft(), "one more thing");
}

/// Feeds `text` as `job`'s delta.
fn job_text(app: &mut App, job: &str, text: &str) {
    app.on_line(session_line(
        "job_delta",
        json!({"job_id": job, "text": text}),
    ));
}

/// Renders `app` at `width` by `height`: the screen's rows and the click
/// targets.
fn rendered(app: &App, width: u16, height: u16) -> (Vec<String>, Vec<crate::mouse::Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    let rows: Vec<String> = crate::view::text(&buf).lines().map(str::to_owned).collect();
    (rows, targets)
}

/// The wall time of `clock`, in milliseconds.
fn wall_ms(clock: &std::sync::Arc<fakes::clock::FakeClock>) -> u64 {
    contract::clock::wall_ms(clock.wall())
}

#[test]
fn running_tty_job_shows_its_cleared_and_redrawn_screen() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    mark_tty(&mut app);
    job_started(
        &mut app,
        "j_9",
        "run the editor",
        wall_ms(&clock).saturating_sub(11_000),
        "a_1",
    );
    job_text(
        &mut app,
        "j_9",
        "stale output\nmore stale\x1b[2J\x1b[H$ edit file.txt\n",
    );
    let _ = open(&mut app, "j_9");
    assert!(app.item_open());
    let (rows, _) = rendered(&app, 80, 24);
    assert!(rows.iter().any(|row| row.contains("$ edit file.txt")));
    assert!(!rows.iter().any(|row| row.contains("stale")));
    insta::assert_snapshot!("running_tty_job_screen", rows.join("\n"));
}

#[test]
fn ordinary_job_shows_the_tail_of_its_output() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    job_started(
        &mut app,
        "j_2",
        "build the workspace",
        wall_ms(&clock).saturating_sub(11_000),
        "a_1",
    );
    job_text(&mut app, "j_2", "compiling one\ncompiling two\ndone\n");
    let _ = open(&mut app, "j_2");
    assert!(app.item_open());
    let (rows, _) = rendered(&app, 80, 24);
    assert!(rows.iter().any(|row| row.contains("done")));
    insta::assert_snapshot!("ordinary_job_tail", rows.join("\n"));
}

#[test]
fn a_completed_job_folds_its_protected_paths_report() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    job_started(
        &mut app,
        "j_2",
        "build the workspace",
        wall_ms(&clock),
        "a_1",
    );
    app.on_line(session_line(
        "job_completed",
        json!({"job_id": "j_2", "status": "completed",
            "protected_paths": {"restored": [".fiber"], "failed": []}}),
    ));
    let record = app
        .items
        .jobs
        .get(&JobId("j_2".to_owned()))
        .expect("a record");
    assert_eq!(
        record.protected,
        vec![
            "Protected path .fiber changed while a confined call ran; Fiber restored it."
                .to_owned()
        ],
        "the fold keeps the warning lines"
    );
}

#[test]
fn completed_job_keeps_its_output_and_loses_its_stop() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    job_started(
        &mut app,
        "j_2",
        "build the workspace",
        wall_ms(&clock).saturating_sub(11_000),
        "a_1",
    );
    job_text(&mut app, "j_2", "done\n");
    let _ = open(&mut app, "j_2");
    assert!(app.item_open());
    let started = wall_ms(&clock).saturating_sub(11_000);
    app.on_line(Line::Session(contract::Envelope {
        kind: "job_completed".to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: started.saturating_add(5_000),
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"job_id": "j_2", "status": "completed"})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }));
    let (rows, targets) = rendered(&app, 80, 24);
    assert!(rows.iter().any(|row| row.contains("completed")));
    assert!(rows.iter().any(|row| row.contains("5s")));
    assert!(rows.iter().any(|row| row.contains("done")));
    assert!(
        targets
            .iter()
            .all(|target| target.id != crate::mouse::TargetId::Item(crate::app::items::Spot::Stop)),
        "a completed job draws no stop target"
    );
    insta::assert_snapshot!("completed_job_body", rows.join("\n"));
}

#[test]
fn enter_in_a_job_view_keeps_the_draft_with_its_notice() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_1");
    let _ = open(&mut app, "j_1");
    app.draft.set("do it");
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
    assert_eq!(app.draft(), "do it");
    assert_eq!(app.notice(), Some("A job takes no input here."));
}

#[test]
fn enter_in_an_other_harness_delegate_view_keeps_the_draft_with_its_notice() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_1");
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": "j_1",
            "delegate_session_id": DELEGATE_A,
            "harness": "claude", "model": "other/model", "workspace": "/w"}),
    ));
    let _ = open(&mut app, "j_1");
    app.draft.set("steer this");
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
    assert_eq!(app.draft(), "steer this");
    assert_eq!(app.notice(), Some("This delegate takes no steering here."));
}

#[test]
fn a_resumed_job_opens_from_its_row_again() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let serial = app
        .serial_of_job(&JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let out = open(&mut app, "j_1");
    ack_all(&mut app, &out);
    app.close_item();
    let lowered = commands(app.items_due(clock.now()));
    ack_all(&mut app, &lowered);
    complete(&mut app, "j_1");
    // Completed: the row replays the finished run.
    let finished = open_serial_of(&mut app, serial);
    assert_eq!(subscribes(&finished).len(), 1);
    ack_all(&mut app, &finished);
    assert!(app.item_open());
    app.close_item();
    // The same job id runs again: the row is clickable and opens, and
    // the replay's held `full` lowers and raises again.
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let resumed = match app.open_serial(serial) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_)
        | Effect::Feedback { .. } => panic!("resuming opens"),
    };
    let resumed_subscribes = subscribes(&resumed);
    let levels: Vec<&str> = resumed_subscribes
        .iter()
        .map(|line| {
            line["args"]["level"]
                .as_str()
                .unwrap_or_else(|| panic!("a level"))
        })
        .collect();
    assert_eq!(levels, ["summary", "full"]);
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert!(view.running);
}

#[test]
fn an_open_view_runs_and_steers_again_after_its_job_restarts() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    complete(&mut app, "j_1");
    assert!(!app.item_view().unwrap_or_else(|| panic!("a view")).running);
    // The same job id runs again: the open view returns to running.
    start_delegate(&mut app, "j_1", DELEGATE_A);
    assert!(app.item_view().unwrap_or_else(|| panic!("a view")).running);
    app.draft.set("focus on the tests");
    let out = match app.on_key(Key::Enter, clock.now()) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_)
        | Effect::Feedback { .. } => panic!("Enter sends"),
    };
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["command"], "steer");
    assert_eq!(out[0]["session_id"], DELEGATE_A);
}

#[test]
fn opening_a_completed_delegate_swaps_and_subscribes() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    complete(&mut app, "j_1");
    // The finished view swaps in and subscribes at `full`: the hub
    // replays the delegate's transcript into it.
    let out = open(&mut app, "j_1");
    let full = subscribes(&out);
    assert_eq!(full.len(), 1);
    assert_eq!(full[0]["args"]["level"], "full");
    assert_eq!(full[0]["session_id"], DELEGATE_A);
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert!(view.has_transcript);
    assert!(!view.running);
    ack_all(&mut app, &out);
    // The replayed fold lands on the swapped screen, not the parent's.
    app.on_line(other_line(
        DELEGATE_A,
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "deep"}]}]}),
    ));
    assert!(shown(&app).iter().any(|row| row.contains("deep")));
}

#[test]
fn a_finished_item_shows_the_outcome_where_the_stop_target_would_be() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    complete(&mut app, "j_1");
    let out = open(&mut app, "j_1");
    assert_eq!(subscribes(&out).len(), 1);
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert!(!view.running);
    assert_eq!(view.word, "completed");
    assert_eq!(view.glyph, "▣");
}

#[test]
fn enter_in_a_finished_item_keeps_the_draft_with_the_finished_notice() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    complete(&mut app, "j_1");
    let out = open(&mut app, "j_1");
    assert_eq!(subscribes(&out).len(), 1);
    // Steering goes nowhere: Enter keeps the draft with the notice.
    app.draft.set("hello?");
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
    assert_eq!(app.draft(), "hello?");
}

#[test]
fn a_resume_while_open_rewants_full_without_a_second_subscribe() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    complete(&mut app, "j_1");
    let out = open(&mut app, "j_1");
    assert_eq!(subscribes(&out).len(), 1);
    // The resume lands while the replay subscribe is in flight: the
    // re-want holds its wish instead of sending beside it.
    start_delegate(&mut app, "j_1", DELEGATE_A);
    assert!(commands(app.items_due(clock.now())).is_empty());
    assert!(
        app.items
            .wants
            .contains_key(&SessionId(DELEGATE_A.to_owned())),
        "the resume re-wants the open delegate"
    );
    // The in-flight answer settles the wish without another send: the
    // held level already answers the new run.
    ack_all(&mut app, &out);
    assert!(commands(app.items_due(clock.now())).is_empty());
}

#[test]
fn closing_a_finished_view_before_its_ack_sends_nothing_further() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    complete(&mut app, "j_1");
    let out = open(&mut app, "j_1");
    let id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    // Closing with the replay subscribe in flight sends nothing: a
    // finished view leaves no `summary` wish behind.
    assert_eq!(app.close_item(), None);
    assert!(!app.item_open());
    assert!(commands(app.items_due(clock.now())).is_empty());
    // The late answer settles without touching the closed view.
    app.on_line(session_accepted(DELEGATE_A, &id));
    assert!(!app.item_open());
    assert!(commands(app.items_due(clock.now())).is_empty());
}

#[test]
fn opening_a_finished_delegate_with_the_link_down_holds_its_wish() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    complete(&mut app, "j_1");
    app.disconnected();
    // The open swaps in and holds its wish; nothing goes out.
    assert!(open(&mut app, "j_1").is_empty());
    assert!(app.item_open());
    assert!(
        app.items
            .wants
            .contains_key(&SessionId(DELEGATE_A.to_owned())),
        "the down link holds the wish"
    );
    // The reconciler drops the wish on a down link, as today.
    assert!(commands(app.items_due(clock.now())).is_empty());
    assert!(
        !app.items
            .wants
            .contains_key(&SessionId(DELEGATE_A.to_owned())),
        "the down link drops the wish"
    );
}

#[test]
fn an_accepted_delegate_steer_closes_its_pending_entry() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.draft.set("focus on the tests");
    let out = match app.on_key(Key::Enter, clock.now()) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_)
        | Effect::Feedback { .. } => panic!("Enter sends"),
    };
    let id = out[0]["id"].as_str().unwrap_or_else(|| panic!("an id"));
    assert!(app.outbox.pending.contains_key(id));
    app.on_line(session_accepted(DELEGATE_A, id));
    assert!(!app.outbox.pending.contains_key(id));
}

#[test]
fn a_cancelled_item_keeps_the_cancelled_status_glyph() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.on_line(session_line(
        "job_completed",
        json!({"job_id": "j_1", "status": "cancelled"}),
    ));
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert_eq!(view.word, "cancelled");
    assert_eq!(view.glyph, "■");
}

#[test]
fn the_delegate_call_count_counts_its_tool_calls() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.on_line(other_line(DELEGATE_A, "tool_call_completed", json!({})));
    app.on_line(other_line(DELEGATE_A, "tool_call_completed", json!({})));
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert_eq!(view.calls, Some(2));
}

#[test]
fn reopening_restarts_the_call_count_for_the_replay() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.on_line(other_line(DELEGATE_A, "tool_call_completed", json!({})));
    app.on_line(other_line(DELEGATE_A, "tool_call_completed", json!({})));
    assert_eq!(
        app.item_view().unwrap_or_else(|| panic!("a view")).calls,
        Some(2)
    );
    app.close_item();
    let lowered = commands(app.items_due(clock.now()));
    ack_all(&mut app, &lowered);
    opened_item(&mut app, "j_1");
    // The fresh screen replays the transcript: the same two
    // completions count once, not on top of the first viewing.
    app.on_line(other_line(DELEGATE_A, "tool_call_completed", json!({})));
    app.on_line(other_line(DELEGATE_A, "tool_call_completed", json!({})));
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert_eq!(view.calls, Some(2));
}

#[test]
fn paging_follows_the_open_delegate() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    // The shown screen is the delegate's: history pages for it.
    assert_eq!(
        app.paging_session(),
        Some(&SessionId(DELEGATE_A.to_owned()))
    );
    app.on_line(other_line(
        DELEGATE_A,
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "deep"}]}]}),
    ));
    assert!(shown(&app).iter().any(|row| row.contains("deep")));
    assert!(app.needs().is_empty());
    app.close_item();
    assert_eq!(app.paging_session(), Some(&SessionId(SESSION.to_owned())));
}

#[test]
fn clicking_a_delegate_row_opens_that_delegate() {
    use crate::app::panel::Spot as PanelSpot;
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    start_delegate(&mut app, "j_2", DELEGATE_B);
    // The second row of the second delegate names its serial.
    let serial = app
        .serial_of_job(&JobId("j_2".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let out = match app.on_click(crate::mouse::TargetId::Panel(PanelSpot::Delegate(serial))) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_)
        | Effect::Feedback { .. } => panic!("opening sends"),
    };
    // No card subscribe went out here, so the click sends the view's
    // `full` subscribe at once.
    let full = subscribes(&out);
    assert_eq!(full.len(), 1);
    assert_eq!(full[0]["args"]["level"], "full");
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert_eq!(view.description, "task j_2");
}

#[test]
fn clicking_the_first_drawn_row_with_the_card_scrolled_opens_the_second_delegate() {
    use crate::app::panel::Spot as PanelSpot;
    let mut app = home();
    opened(&mut app);
    app.set_size(160, 40);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    start_delegate(&mut app, "j_2", DELEGATE_B);
    start_delegate(&mut app, "j_3", "s_ffffffffffffffff");
    start_delegate(&mut app, "j_4", "s_0000000000000000");
    app.panel_state.scroll_delegates(false);
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel"));
    let area = ratatui::layout::Rect::new(0, 0, rect.width, rect.height);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let mut targets = Vec::new();
    crate::view::panel::draw(&app, area, &mut buf, &mut targets);
    let first = targets
        .iter()
        .filter_map(|target| {
            if let crate::mouse::TargetId::Panel(PanelSpot::Delegate(serial)) = target.id {
                Some(serial)
            } else {
                None
            }
        })
        .next()
        .unwrap_or_else(|| panic!("a delegate target"));
    assert_eq!(first, 2);
    app.on_click(crate::mouse::TargetId::Panel(PanelSpot::Delegate(first)));
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert_eq!(view.description, "task j_2");
}

#[test]
fn a_press_and_release_on_different_serials_opens_nothing() {
    use crate::app::panel::Spot as PanelSpot;
    use crate::keys::{Button, Mouse, MouseKind};
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    start_delegate(&mut app, "j_2", DELEGATE_B);
    let cell = ratatui::layout::Rect::new(4, 8, 10, 1);
    let before = vec![crate::mouse::Target {
        id: crate::mouse::TargetId::Panel(PanelSpot::Delegate(1)),
        rect: cell,
    }];
    let after = vec![crate::mouse::Target {
        id: crate::mouse::TargetId::Panel(PanelSpot::Delegate(2)),
        rect: cell,
    }];
    let mut pointer = crate::mouse::Pointer::default();
    // A press on delegate A, A's `job_completed`, a redraw that puts B
    // on that row, and a release on it: the ids differ, so no click.
    assert_eq!(
        pointer.on_mouse(
            &Mouse {
                kind: MouseKind::Press(Button::Left),
                col: 4,
                row: 8
            },
            &before,
            false
        ),
        None
    );
    complete(&mut app, "j_1");
    assert_eq!(
        pointer.on_mouse(
            &Mouse {
                kind: MouseKind::Release,
                col: 4,
                row: 8
            },
            &after,
            false
        ),
        None
    );
    assert!(!app.item_open());
}

#[test]
fn a_click_on_a_completed_job_serial_opens_and_subscribes() {
    use crate::app::panel::Spot as PanelSpot;
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    complete(&mut app, "j_1");
    let serial = app
        .serial_of_job(&JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let out = match app.on_click(crate::mouse::TargetId::Panel(PanelSpot::Delegate(serial))) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_)
        | Effect::Feedback { .. } => panic!("opening sends"),
    };
    let full = subscribes(&out);
    assert_eq!(full.len(), 1);
    assert_eq!(full[0]["args"]["level"], "full");
    assert!(app.item_open());
}

#[test]
fn enter_on_a_finished_delegate_row_opens_and_subscribes() {
    use crate::app::panel::Spot as PanelSpot;
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    complete(&mut app, "j_1");
    let serial = app
        .serial_of_job(&JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    // Enter on the focused Delegates-card row is the click's twin: it
    // opens through the same `open_serial` seam.
    app.focus = Some(crate::mouse::TargetId::Panel(PanelSpot::Delegate(serial)));
    let out = match app.on_key(Key::Enter, clock.now()) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_)
        | Effect::Feedback { .. } => panic!("opening sends"),
    };
    let full = subscribes(&out);
    assert_eq!(full.len(), 1);
    assert_eq!(full[0]["args"]["level"], "full");
    assert!(app.item_open());
    ack_all(&mut app, &out);
    // The replayed fold lands on the swapped screen, not the parent's.
    app.on_line(other_line(
        DELEGATE_A,
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "deep"}]}]}),
    ));
    assert!(shown(&app).iter().any(|row| row.contains("deep")));
}

/// Opens `serial`'s view, returning the parsed lines going out.
fn open_serial_of(app: &mut App, serial: u64) -> Vec<Value> {
    match app.open_serial(serial) {
        Effect::Send(lines) => commands(lines),
        Effect::None => Vec::new(),
        Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_)
        | Effect::Feedback { .. } => panic!("opening sends or nothing"),
    }
}

#[test]
fn another_harness_delegate_opens_without_a_subscribe() {
    use crate::app::panel::Spot as PanelSpot;
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    app.on_line(session_line(
        "job_started",
        json!({"job_id": "j_1", "description": "task j_1", "output_path": "/tmp/other.out"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": "j_1",
            "delegate_session_id": DELEGATE_A,
            "harness": "claude", "model": "other/model", "workspace": "/w"}),
    ));
    let serial = app
        .serial_of_job(&JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Panel(PanelSpot::Delegate(serial))),
        Effect::None
    );
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert!(!view.has_transcript);
    assert_eq!(view.output_path, "/tmp/other.out");
    // Steering goes nowhere: Enter keeps the draft with the notice.
    app.draft.set("hello?");
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
    assert_eq!(app.draft(), "hello?");
}

#[test]
fn reopening_the_same_item_keeps_its_replayed_transcript() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let first = open(&mut app, "j_1");
    assert_eq!(subscribes(&first).len(), 1);
    ack_all(&mut app, &first);
    app.on_line(other_line(DELEGATE_A, "tool_call_completed", json!({})));
    assert_eq!(
        app.item_view().unwrap_or_else(|| panic!("a view")).calls,
        Some(1)
    );
    assert!(open(&mut app, "j_1").is_empty());
    assert_eq!(
        app.item_view().unwrap_or_else(|| panic!("a view")).calls,
        Some(1)
    );
}

#[test]
fn stop_on_a_completed_item_sends_nothing() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    complete(&mut app, "j_1");
    assert_eq!(app.item_click(super::Spot::Stop), Effect::None);
}

#[test]
fn an_empty_draft_enter_in_the_view_sends_nothing() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    assert!(app.draft.is_empty());
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
}

#[test]
fn with_the_link_down_enter_and_stop_do_nothing() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.connect_failed("down".to_owned());
    app.draft.set("steer me");
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
    assert_eq!(app.draft(), "steer me");
    assert_eq!(app.item_click(super::Spot::Stop), Effect::None);
}

#[test]
fn open_serial_with_no_job_opens_nothing() {
    let mut app = home();
    opened(&mut app);
    assert_eq!(app.open_serial(999), Effect::None);
    assert!(!app.item_open());
}

#[test]
fn paging_stays_on_the_parent_without_a_fiber_transcript() {
    let mut app = home();
    opened(&mut app);
    app.on_line(session_line(
        "job_started",
        json!({"job_id": "j_1", "description": "task j_1", "output_path": "/tmp/other.out"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": "j_1",
            "delegate_session_id": DELEGATE_A,
            "harness": "claude", "model": "other/model", "workspace": "/w"}),
    ));
    let serial = app
        .serial_of_job(&JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    use crate::app::panel::Spot as PanelSpot;
    app.on_click(crate::mouse::TargetId::Panel(PanelSpot::Delegate(serial)));
    assert_eq!(app.paging_session(), Some(&SessionId(SESSION.to_owned())));
}

#[test]
fn take_wake_prefers_the_earlier_retry_over_the_status_tick() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    let refused_at = clock.now();
    tick(&mut app, &clock);
    app.on_line(session_refused(DELEGATE_A, &id, "session_not_found"));
    // Under reduced motion nothing else ticks: the draw asks the next
    // whole second for the running status row, and the retry 500 ms out
    // is earlier.
    app.set_reduced_motion(true);
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let _ = crate::view::render(&app, area, &mut buf, None);
    assert_eq!(app.take_wake(), Some(refused_at + RETRY));
}

#[test]
fn an_accepted_import_in_the_open_delegate_session_opens_the_report() {
    use super::super::Kind;
    use crate::import_report::ImportSource;
    use crate::input::Draft;

    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.outbox.insert(
        "c_1".to_owned(),
        Kind::Import(ImportSource::Codex),
        Draft::default(),
    );
    let accepted = other_line(
        DELEGATE_A,
        "command_accepted",
        json!({
            "command_id": "c_1",
            "result": {
                "imported": {
                    "mcp_servers": ["alpha"],
                    "skills": [],
                    "commands": [],
                    "instructions": [],
                },
                "left_behind": [],
            },
        }),
    );
    assert!(app.on_line(accepted).is_empty());
    let report = app
        .import_report()
        .expect("the item session's accept opens the report");
    assert_eq!(report.title(), "Imported from Codex");
    assert!(!app.outbox.pending.contains_key("c_1"));
    // A malformed accept for a fresh id on the item session adds one
    // notice and opens no second report.
    app.outbox.insert(
        "c_2".to_owned(),
        Kind::Import(ImportSource::Codex),
        Draft::default(),
    );
    let malformed = other_line(DELEGATE_A, "command_accepted", json!({"command_id": "c_2"}));
    assert!(app.on_line(malformed).is_empty());
    assert_eq!(app.notices().len(), 1);
    assert_eq!(app.notice(), Some("rejected"));
    assert_eq!(
        app.import_report()
            .expect("the first report stays open")
            .title(),
        "Imported from Codex"
    );
}

#[test]
fn item_view_carries_the_delegate_thinking_level() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    app.on_line(live(
        DELEGATE_A,
        json!({"parent": SESSION, "state": "streaming", "thinking": "high"}),
    ));
    let _ = open(&mut app, "j_1");
    assert!(app.item_open());
    assert_eq!(
        app.item_view().expect("the open item").level,
        Some("high".to_owned())
    );
}

#[test]
fn item_view_leaves_the_level_out_without_a_thinking_level() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let _ = open(&mut app, "j_1");
    // No status row held for the delegate: no level.
    assert_eq!(app.item_view().expect("the open item").level, None);
    // A status row with no thinking level: still no level.
    app.on_line(live(
        DELEGATE_A,
        json!({"parent": SESSION, "state": "streaming"}),
    ));
    assert_eq!(app.item_view().expect("the open item").level, None);
}

#[test]
fn item_view_carries_the_open_job_id() {
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_1");
    let _ = open(&mut app, "j_1");
    assert!(app.item_open());
    assert_eq!(
        app.item_view().expect("the open item").job_id,
        JobId("j_1".to_owned())
    );
}

#[test]
fn item_view_marks_only_a_tty_job() {
    let mut app = home();
    opened(&mut app);
    // A tty-marked shell job takes the flag.
    mark_tty(&mut app);
    job_started(&mut app, "j_9", "run the editor", 0, "a_1");
    // An ordinary job on an unmarked action does not.
    job_started(&mut app, "j_2", "build the workspace", 0, "a_2");
    // A delegate does not.
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let _ = open(&mut app, "j_9");
    assert!(app.item_view().expect("the tty job").tty);
    let _ = open(&mut app, "j_2");
    assert!(!app.item_view().expect("the ordinary job").tty);
    let _ = open(&mut app, "j_1");
    assert!(!app.item_view().expect("the delegate").tty);
}

#[test]
fn a_finished_tty_job_keeps_its_tty_mark_after_its_output_clears() {
    let mut app = home();
    opened(&mut app);
    mark_tty(&mut app);
    job_started(&mut app, "j_9", "run the editor", 0, "a_1");
    job_text(&mut app, "j_9", "$ edit file.txt\n");
    // Finishing while closed clears the live output ...
    complete(&mut app, "j_9");
    let record = app
        .items
        .jobs
        .get(&JobId("j_9".to_owned()))
        .expect("the folded job");
    assert!(record.output.is_none());
    // ... but opening afterward still sees the tty mark.
    let _ = open(&mut app, "j_9");
    let view = app.item_view().expect("the open job");
    assert!(view.tty);
    assert_eq!(view.job_id, JobId("j_9".to_owned()));
}

/// A wheel report at (`col`, `row`): down steps toward the tail, over the
/// item body the open item takes.
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

/// The wheel over a live tty item is swallowed: neither the tty's rows
/// nor the parent move (`docs/tui.md:576`, "Swapped views"; see #3578).
#[test]
fn wheel_over_a_live_tty_item_moves_nothing() {
    let mut app = home();
    opened(&mut app);
    mark_tty(&mut app);
    job_started(&mut app, "j_9", "run the editor", 0, "a_1");
    job_text(&mut app, "j_9", "$ edit file.txt\n");
    let _ = open(&mut app, "j_9");
    assert!(app.item_open());
    app.jump(1);
    let before = app.screen.top();
    assert!(before.is_some(), "the view starts scrolled");
    app.on_wheel(&wheel(20, 4, true));
    assert_eq!(
        app.screen.top(),
        before,
        "the wheel left the tty's rows and the parent"
    );
}

/// The wheel over a finished tty item is swallowed: neither the tty's
/// rows nor the parent move (`docs/tui.md:576`, "Swapped views"; see #3578).
#[test]
fn wheel_over_a_finished_tty_item_moves_nothing() {
    let mut app = home();
    opened(&mut app);
    mark_tty(&mut app);
    job_started(&mut app, "j_9", "run the editor", 0, "a_1");
    job_text(&mut app, "j_9", "$ edit file.txt\n");
    complete(&mut app, "j_9");
    let _ = open(&mut app, "j_9");
    assert!(app.item_open());
    app.jump(1);
    let before = app.screen.top();
    assert!(before.is_some(), "the view starts scrolled");
    app.on_wheel(&wheel(20, 4, true));
    assert_eq!(
        app.screen.top(),
        before,
        "the wheel left the tty's rows and the parent"
    );
}

/// The wheel over a finished non-tty job item is swallowed: neither the
/// job's rows nor the parent move (`docs/tui.md:576`, "Swapped views";
/// see #3578).
#[test]
fn wheel_over_a_finished_job_item_moves_nothing() {
    let mut app = home();
    opened(&mut app);
    job_started(&mut app, "j_2", "build the workspace", 0, "a_1");
    job_text(&mut app, "j_2", "done\n");
    complete(&mut app, "j_2");
    let _ = open(&mut app, "j_2");
    assert!(app.item_open());
    app.jump(1);
    let before = app.screen.top();
    assert!(before.is_some(), "the view starts scrolled");
    app.on_wheel(&wheel(20, 4, true));
    assert_eq!(
        app.screen.top(),
        before,
        "the wheel left the job's rows and the parent"
    );
}

/// The wheel over another harness's delegate item is swallowed: neither
/// the item's rows nor the parent move (`docs/tui.md:576`, "Swapped views";
/// see #3578).
#[test]
fn wheel_over_a_foreign_delegate_item_moves_nothing() {
    let mut app = home();
    opened(&mut app);
    app.on_line(session_line(
        "job_started",
        json!({"job_id": "j_1", "description": "task j_1", "output_path": "/tmp/other.out"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": "j_1",
            "delegate_session_id": DELEGATE_A,
            "harness": "claude", "model": "other/model", "workspace": "/w"}),
    ));
    let _ = open(&mut app, "j_1");
    assert!(app.item_open());
    assert!(
        !app.item_transcript(),
        "another harness swaps no transcript"
    );
    app.jump(1);
    let before = app.screen.top();
    assert!(before.is_some(), "the view starts scrolled");
    app.on_wheel(&wheel(20, 4, true));
    assert_eq!(
        app.screen.top(),
        before,
        "the wheel left the item's rows and the parent"
    );
}

/// Thirty-five text rows on the open delegate transcript: taller than the
/// window, so three steps down stay off the tail instead of following as
/// a short transcript does.
fn tall_transcript(app: &mut App) {
    app.on_line(other_line(DELEGATE_A, "turn_started", json!({"input": []})));
    for at in 0..35u64 {
        let Line::Session(mut line) = other_line(
            DELEGATE_A,
            "text_completed",
            json!({"text": format!("transcript row {at:02}")}),
        ) else {
            unreachable!()
        };
        line.seq = Some(contract::Seq(at + 1));
        app.on_line(Line::Session(line));
    }
}

/// The wheel over the fiber delegate's transcript scrolls it three rows,
/// leaving the stashed parent (`docs/tui.md:576`, "Swapped views"; see
/// #3578).
#[test]
fn wheel_over_the_fiber_transcript_scrolls_it_not_the_parent() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let _ = open(&mut app, "j_1");
    assert!(app.item_open());
    assert!(
        app.item_transcript(),
        "the fiber delegate swaps its transcript"
    );
    tall_transcript(&mut app);
    app.jump(1);
    let item = app.screen.top().expect("the transcript starts scrolled");
    let parent = app.attached_screen().top();
    app.on_wheel(&wheel(20, 5, true));
    assert_eq!(
        app.screen.top(),
        Some(item + 3),
        "one wheel step is three rows down the transcript"
    );
    assert_eq!(
        app.attached_screen().top(),
        parent,
        "the wheel left the stashed parent"
    );
}

/// The item's non-scrolling header swallows the wheel; it reaches neither
/// the transcript body nor the stashed parent (`docs/tui.md:576`, "Swapped
/// views"; see #3578).
#[test]
fn wheel_over_the_transcript_header_moves_nothing() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let _ = open(&mut app, "j_1");
    assert!(app.item_open());
    assert!(
        app.item_transcript(),
        "the fiber delegate swaps its transcript"
    );
    tall_transcript(&mut app);
    app.jump(1);
    let item = app.screen.top().expect("the transcript starts scrolled");
    let parent = app.attached_screen().top();
    app.on_wheel(&wheel(20, 2, true));
    assert_eq!(
        app.screen.top(),
        Some(item),
        "the item header has no rows to scroll"
    );
    assert_eq!(
        app.attached_screen().top(),
        parent,
        "the wheel left the stashed parent"
    );
}

/// The item owns its final drawn row, but the row immediately below the
/// content rectangle is not part of the item and cannot reach the hidden
/// parent.
/// (`docs/tui.md:576`).
#[test]
fn wheel_at_the_item_content_edge_stops_at_its_last_row() {
    let mut app = home();
    app.set_size(80, 24);
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let _ = open(&mut app, "j_1");
    tall_transcript(&mut app);
    app.jump(1);
    let layout = app.chrome().layout().expect("the attached layout");
    let x = layout.column.x.saturating_add(layout.column.width / 2);
    let last_y = layout
        .column
        .y
        .saturating_add(u16::try_from(app.conversation_area_height()).unwrap_or(u16::MAX))
        .saturating_sub(1);
    let below_y = last_y.saturating_add(1);
    let item = app.screen.top().expect("the transcript starts scrolled");
    let parent = app.attached_screen().top();

    app.on_wheel(&wheel(x, last_y, true));
    assert_eq!(
        app.screen.top(),
        Some(item + 3),
        "the final drawn item row belongs to the item"
    );
    assert_eq!(app.attached_screen().top(), parent);
    let after = app.screen.top();

    app.on_wheel(&wheel(x, below_y, true));
    assert_eq!(
        app.screen.top(),
        after,
        "the row immediately below the item's drawn content does not scroll it"
    );
    assert_eq!(
        app.attached_screen().top(),
        parent,
        "the row below the item does not reach the hidden parent"
    );
}

/// The wheel over a fiber transcript overlapping Offer and Results goes
/// to the top-drawn item view only: the transcript scrolls three rows
/// while the stashed parent, the offer and the results stay put
/// (`docs/tui.md:576`, "Swapped views"; see #3578).
#[test]
fn wheel_over_the_fiber_transcript_wins_over_offer_and_results() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let _ = open(&mut app, "j_1");
    assert!(app.item_open());
    assert!(
        app.item_transcript(),
        "the fiber delegate swaps its transcript"
    );
    tall_transcript(&mut app);
    // A three-row offer under the item view.
    app.on_line(session_line(
        "repository_code_offered",
        json!({"request_id": "r_1", "items": [
            {"kind": "mcp_server", "name": "a", "hash": "h",
             "required": false, "summary": "MCP server: a"},
            {"kind": "mcp_server", "name": "b", "hash": "h",
             "required": false, "summary": "MCP server: b"},
            {"kind": "mcp_server", "name": "c", "hash": "h",
             "required": false, "summary": "MCP server: c"}]}),
    ));
    assert!(app.offer_open(), "the offer opened under the item");
    // Empty results under the item view.
    app.open_results();
    assert!(app.results_open(), "the results opened under the item");
    app.jump(1);
    let item = app.screen.top().expect("the transcript starts scrolled");
    let parent = app.attached_screen().top();
    let offer = app.offer_rows(80).map(|(rows, top)| {
        rows.iter()
            .map(|row| {
                row.line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
            + &format!("#{top}")
    });
    assert!(offer.is_some(), "the offer shows rows");
    let results = app.find_results().map(|view| (view.selected, view.top));
    app.on_wheel(&wheel(20, 5, true));
    assert_eq!(
        app.screen.top(),
        Some(item + 3),
        "one wheel step is three rows down the transcript"
    );
    assert_eq!(
        app.attached_screen().top(),
        parent,
        "the wheel left the stashed parent"
    );
    assert_eq!(
        app.offer_rows(80).map(|(rows, top)| {
            rows.iter()
                .map(|row| {
                    row.line
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref().to_owned())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
                + &format!("#{top}")
        }),
        offer,
        "the wheel left the offer"
    );
    assert_eq!(
        app.find_results().map(|view| (view.selected, view.top)),
        results,
        "the wheel left the results"
    );
}

/// A swapped picker is drawn above the attached item and owns its wheel:
/// the picker moves three displayed rows while the item stays put.
/// (`docs/tui.md:576`).
#[test]
fn wheel_over_the_model_picker_above_an_item_moves_the_picker() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let _ = open(&mut app, "j_1");
    assert!(app.item_transcript());
    tall_transcript(&mut app);
    app.jump(1);
    let item = app.screen.top().expect("the transcript starts scrolled");
    app.on_models(Ok(crate::catalogue::Catalogue {
        installable: Vec::new(),
        models: (0..30)
            .map(|at| {
                let id = format!("m{at:02}");
                crate::catalogue::ModelEntry {
                    reference: format!("acme/{id}"),
                    provider: "acme".to_owned(),
                    id,
                    levels: Vec::new(),
                    default_level: None,
                    configured: None,
                    roles: Vec::new(),
                    name: None,
                    price: None,
                }
            })
            .collect(),
        notices: Vec::new(),
        lists: Vec::new(),
        left_out: Vec::new(),
    }));
    app.open_model_picker(crate::model_picker::Mode::Choose);
    let picker = app.model_picker_view().expect("the picker is open");
    assert_eq!(picker.top, 0);

    app.on_wheel(&wheel(20, 4, true));

    assert_eq!(
        app.model_picker_view().expect("the picker stays open").top,
        3,
        "the top-drawn picker owns a three-row wheel step"
    );
    assert_eq!(
        app.screen.top(),
        Some(item),
        "the wheel left the attached item untouched"
    );
}

/// A drawn Search or Completion slab above an item swallows wheel input
/// before it can reach the item transcript.
/// (`docs/tui.md:576`).
#[test]
fn wheel_inside_search_and_completion_slabs_over_an_item_moves_nothing() {
    for surface in [crate::app::Surface::Search, crate::app::Surface::Completion] {
        let mut app = home();
        opened(&mut app);
        start_delegate(&mut app, "j_1", DELEGATE_A);
        let _ = open(&mut app, "j_1");
        tall_transcript(&mut app);
        app.jump(1);
        let item = app.screen.top().expect("the transcript starts scrolled");
        if surface == crate::app::Surface::Search {
            app.open_search();
        } else {
            app.on_key(Key::Char('/'), fakes::clock::FakeClock::new().now());
        }
        let (width, height) = app.screen_size();
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        let targets = crate::view::render(&app, area, &mut buffer, None);
        app.drawn(&targets);
        let slab = app
            .frames()
            .slab_of(surface)
            .expect("the search surface is drawn");
        assert!(!slab.is_empty(), "{surface:?} draws a slab");

        assert_eq!(app.on_wheel(&wheel(slab.x, slab.y, true)), Effect::None);
        assert_eq!(
            app.screen.top(),
            Some(item),
            "{surface:?} owns the wheel above the item"
        );
    }
}
