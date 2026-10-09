//! Tests for the `/keys` screen on the app: opening it, saving through
//! the seam, and the keys it takes ahead of everything behind it
//! (`docs/tui.md`, "Bindings").

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use contract::clock::Clock;

use super::super::{App, Effect, QUIT_WINDOW};
use crate::Configure;
use crate::configure::KeyEdit;
use crate::configure_fake::Fake;
use crate::home::Launch;
use crate::keys::{Edit, Key};
use crate::link::Line;
use crate::stroke::Stroke;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// The stroke `name` names.
fn stroke(name: &str) -> Stroke {
    Stroke::parse(name).unwrap_or_else(|err| panic!("{name}: {err}"))
}

/// Presses the stroke `name` names at `at`.
fn tap_at(app: &mut App, name: &str, at: Instant) -> Effect {
    app.on_press(stroke(name), at)
}

/// Presses the stroke `name` names.
fn tap(app: &mut App, name: &str) -> Effect {
    tap_at(app, name, now())
}

/// Types `text` into the app, one character per key.
fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        assert_eq!(app.on_key(Key::Char(ch), now()), Effect::None);
    }
}

/// Types `/keys` and presses Enter.
fn slash_keys(app: &mut App) {
    type_text(app, "/keys");
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
}

/// The launch description: `/w`, outside git, default shares.
fn launch() -> Launch {
    Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        ..Default::default()
    }
}

/// An app on home at 80x24 with `seam`.
fn home(seam: Option<Arc<Fake>>) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch());
    app.set_configure(seam.map(|seam| seam as Arc<dyn Configure>));
    app.set_size(80, 24);
    app
}

/// An app with home state, attached, at 80x24, with `seam`.
fn homed(seam: Option<Arc<Fake>>) -> App {
    let mut app = home(seam);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// Loads `entries` as the app's `keys`.
fn keyed(app: &mut App, entries: &[(&str, Value)]) {
    let mut user = serde_json::Map::new();
    for (id, value) in entries {
        user.insert((*id).to_owned(), value.clone());
    }
    app.set_keys(crate::KeysSetup { user });
}

/// A `hub_hello` this terminal reads.
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// One session envelope on [`SESSION`].
fn envelope(kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A live `session_status` for [`SESSION`], streaming.
fn live() -> Line {
    envelope(
        "session_status",
        json!({
            "name": "fix the parser",
            "workspace": "/w",
            "project": "-w",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model",
            "delegates": 0,
            "jobs": 0,
            "clients": 0,
            "state": "streaming",
        }),
    )
}

/// A turn starting on [`SESSION`]: the session is busy.
fn started() -> Line {
    envelope(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    )
}

/// The screen as text.
fn rendered(app: &App) -> String {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

/// Moves the selection down `n` rows.
fn down(app: &mut App, n: usize) {
    for _ in 0..n {
        assert_eq!(tap(app, "down"), Effect::None);
    }
}

#[test]
fn slash_keys_opens_the_screen_with_the_draft_cleared() {
    for attached in [false, true] {
        let fake = Arc::new(Fake::new(Vec::new()));
        let mut app = if attached {
            homed(Some(Arc::clone(&fake)))
        } else {
            home(Some(Arc::clone(&fake)))
        };
        slash_keys(&mut app);
        assert!(app.keys_screen_open());
        assert_eq!(app.draft(), "");
    }
}

#[test]
fn opening_keys_closes_the_key_map_and_the_picker() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = homed(Some(Arc::clone(&fake)));
    assert_eq!(app.on_key(Key::F1, now()), Effect::None);
    assert!(app.keymap_top().is_some());
    assert_eq!(app.open_keys(), Effect::None);
    assert!(app.keys_screen_open());
    assert!(app.keymap_top().is_none());
    assert_eq!(tap(&mut app, "esc"), Effect::None);
    assert!(!app.keys_screen_open());
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert!(app.model_picker_open());
    assert_eq!(app.open_keys(), Effect::None);
    assert!(app.keys_screen_open());
    assert!(!app.model_picker_open());
}

#[test]
fn rebinding_new_session_saves_one_edit_then_answers_the_new_key() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = homed(Some(Arc::clone(&fake)));
    slash_keys(&mut app);
    down(&mut app, 5);
    assert_eq!(tap(&mut app, "enter"), Effect::None);
    assert_eq!(tap(&mut app, "ctrl+t"), Effect::None);
    assert_eq!(
        fake.keys_saved(),
        [vec![KeyEdit {
            id: "new_session".to_owned(),
            keys: Some(vec!["ctrl+t".to_owned()]),
        }]]
    );
    assert_eq!(tap(&mut app, "esc"), Effect::None);
    assert!(!app.keys_screen_open());
    assert_eq!(tap(&mut app, "ctrl+t"), Effect::None);
    assert!(app.session().is_none());
    assert_eq!(tap(&mut app, "ctrl+n"), Effect::None);
    assert_eq!(app.draft(), "");
}

#[test]
fn a_swap_saves_both_edits_in_one_call() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = homed(Some(Arc::clone(&fake)));
    slash_keys(&mut app);
    assert_eq!(tap(&mut app, "enter"), Effect::None);
    assert_eq!(tap(&mut app, "ctrl+o"), Effect::None);
    assert_eq!(tap(&mut app, "enter"), Effect::None);
    assert_eq!(
        fake.keys_saved(),
        [vec![
            KeyEdit {
                id: "send".to_owned(),
                keys: Some(vec!["ctrl+o".to_owned()]),
            },
            KeyEdit {
                id: "toggle_ledgers".to_owned(),
                keys: Some(Vec::new()),
            },
        ]]
    );
}

#[test]
fn reset_saves_no_entry_for_the_defaults() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = homed(Some(Arc::clone(&fake)));
    keyed(&mut app, &[("copy_focused", json!(["c"]))]);
    slash_keys(&mut app);
    down(&mut app, 19);
    assert_eq!(tap(&mut app, "r"), Effect::None);
    assert_eq!(
        fake.keys_saved(),
        [vec![KeyEdit {
            id: "copy_focused".to_owned(),
            keys: None,
        }]]
    );
}

#[test]
fn delete_unbinds_and_the_key_goes_dead() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = homed(Some(Arc::clone(&fake)));
    slash_keys(&mut app);
    down(&mut app, 15);
    assert_eq!(tap(&mut app, "delete"), Effect::None);
    assert_eq!(
        fake.keys_saved(),
        [vec![KeyEdit {
            id: "toggle_ledgers".to_owned(),
            keys: Some(Vec::new()),
        }]]
    );
    assert_eq!(tap(&mut app, "esc"), Effect::None);
    assert_eq!(tap(&mut app, "ctrl+o"), Effect::None);
    assert_eq!(app.on_key(Key::F1, now()), Effect::None);
    assert!(
        crate::keymap::lines(app.keys())
            .iter()
            .any(|line| line.contains("unbound")),
        "{:?}",
        crate::keymap::lines(app.keys())
    );
}

#[test]
fn capturing_an_action_own_key_saves_nothing() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = homed(Some(Arc::clone(&fake)));
    slash_keys(&mut app);
    down(&mut app, 5);
    assert_eq!(tap(&mut app, "enter"), Effect::None);
    assert_eq!(tap(&mut app, "ctrl+n"), Effect::None);
    assert!(fake.keys_saved().is_empty());
}

#[test]
fn a_refused_save_keeps_the_old_keyset_on_screen() {
    let fake = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut refuse) = fake.refuse.lock() {
        *refuse = Some("disk full".to_owned());
    }
    let mut app = homed(Some(Arc::clone(&fake)));
    slash_keys(&mut app);
    down(&mut app, 5);
    assert_eq!(tap(&mut app, "enter"), Effect::None);
    assert_eq!(tap(&mut app, "ctrl+t"), Effect::None);
    assert!(
        rendered(&app).contains("Not saved: disk full"),
        "{}",
        rendered(&app)
    );
    assert_eq!(tap(&mut app, "esc"), Effect::None);
    assert!(!app.keys_screen_open());
    assert_eq!(tap(&mut app, "ctrl+t"), Effect::None);
    assert!(app.session().is_some());
}

#[test]
fn without_a_seam_a_rebind_applies_for_the_run() {
    let mut app = homed(None);
    slash_keys(&mut app);
    down(&mut app, 5);
    assert_eq!(tap(&mut app, "enter"), Effect::None);
    assert_eq!(tap(&mut app, "ctrl+t"), Effect::None);
    assert_eq!(tap(&mut app, "esc"), Effect::None);
    assert_eq!(tap(&mut app, "ctrl+t"), Effect::None);
    assert!(app.session().is_none());
}

#[test]
fn the_screen_takes_the_application_keys_ahead_of_the_draft() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = homed(Some(Arc::clone(&fake)));
    slash_keys(&mut app);
    for name in ["ctrl+o", "f1", "alt+p"] {
        assert_eq!(tap(&mut app, name), Effect::None, "{name}");
    }
    assert!(app.keymap_top().is_none());
    assert!(app.panel().is_none());
    assert_eq!(tap(&mut app, "esc"), Effect::None);
    assert!(!app.keys_screen_open());
}

#[test]
fn esc_on_a_busy_session_closes_the_screen_sending_no_cancel() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut live_app = homed(Some(Arc::clone(&fake)));
    live_app.on_line(hello());
    live_app.on_line(started());
    assert!(matches!(live_app.on_key(Key::Esc, now()), Effect::Send(_)));
    let mut app = homed(Some(Arc::clone(&fake)));
    app.on_line(hello());
    app.on_line(started());
    slash_keys(&mut app);
    assert!(app.keys_screen_open());
    assert_eq!(tap(&mut app, "esc"), Effect::None);
    assert!(!app.keys_screen_open());
}

#[test]
fn ctrl_c_twice_within_the_window_quits_with_the_screen_open() {
    let clock = fakes::clock::FakeClock::new();
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = homed(Some(Arc::clone(&fake)));
    slash_keys(&mut app);
    assert!(app.keys_screen_open());
    assert_eq!(tap_at(&mut app, "ctrl+c", clock.now()), Effect::None);
    assert!(app.hint());
    clock.advance(
        QUIT_WINDOW
            .checked_sub(Duration::from_millis(1))
            .unwrap_or(QUIT_WINDOW),
    );
    assert_eq!(tap_at(&mut app, "ctrl+c", clock.now()), Effect::Quit);
}

#[test]
fn with_the_quit_question_open_a_key_goes_to_the_question() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = homed(Some(Arc::clone(&fake)));
    app.on_line(live());
    slash_keys(&mut app);
    down(&mut app, 5);
    assert_eq!(tap(&mut app, "enter"), Effect::None);
    app.quit();
    assert!(app.quit_open());
    assert_eq!(tap(&mut app, "x"), Effect::None);
    assert!(app.quit_open());
    assert!(app.keys_screen_open());
    assert!(fake.keys_saved().is_empty());
    assert_eq!(tap(&mut app, "enter"), Effect::Quit);
}

#[test]
fn a_paste_while_the_screen_is_open_leaves_the_draft_empty() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = homed(Some(Arc::clone(&fake)));
    slash_keys(&mut app);
    assert_eq!(app.on_edit(Edit::Paste("hello".to_owned())), Effect::None);
    assert_eq!(app.draft(), "");
}

#[test]
fn esc_closes_the_notice_overlay_ahead_of_the_screen() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = homed(Some(Arc::clone(&fake)));
    slash_keys(&mut app);
    app.notices.push("first".to_owned());
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    assert_eq!(tap(&mut app, "esc"), Effect::None);
    assert!(app.notice_overlay().is_none());
    assert!(app.keys_screen_open());
    assert_eq!(tap(&mut app, "esc"), Effect::None);
    assert!(!app.keys_screen_open());
}

#[test]
fn a_letter_over_the_overlay_is_neither_captured_nor_typed() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = homed(Some(Arc::clone(&fake)));
    slash_keys(&mut app);
    down(&mut app, 5);
    assert_eq!(tap(&mut app, "enter"), Effect::None);
    app.notices.push("first".to_owned());
    app.open_more_notices();
    assert_eq!(tap(&mut app, "x"), Effect::None);
    assert_eq!(app.draft(), "");
    assert!(fake.keys_saved().is_empty());
    assert_eq!(tap(&mut app, "esc"), Effect::None);
    assert!(app.notice_overlay().is_none());
    assert!(app.keys_screen_open());
    assert_eq!(tap(&mut app, "ctrl+t"), Effect::None);
    assert_eq!(
        fake.keys_saved(),
        [vec![KeyEdit {
            id: "new_session".to_owned(),
            keys: Some(vec!["ctrl+t".to_owned()]),
        }]]
    );
}

#[test]
fn ctrl_c_with_the_notice_overlay_open_still_clears_then_quits() {
    let clock = fakes::clock::FakeClock::new();
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = homed(Some(Arc::clone(&fake)));
    slash_keys(&mut app);
    app.notices.push("first".to_owned());
    app.open_more_notices();
    assert_eq!(tap_at(&mut app, "ctrl+c", clock.now()), Effect::None);
    assert!(app.hint());
    clock.advance(
        QUIT_WINDOW
            .checked_sub(Duration::from_millis(1))
            .unwrap_or(QUIT_WINDOW),
    );
    assert_eq!(tap_at(&mut app, "ctrl+c", clock.now()), Effect::Quit);
}
