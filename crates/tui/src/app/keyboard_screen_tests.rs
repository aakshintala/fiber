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
use crate::app::testkit::{SESSION, hello, live, now};
use crate::configure::KeyEdit;
use crate::configure_fake::Fake;
use crate::home::Launch;
use crate::keys::{Edit, Key};
use crate::link::Line;
use crate::stroke::Stroke;

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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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

/// The first display line of action `at` at the test width: the screen
/// wraps at the 80-wide column, so every frame index is a line.
fn first_line(app: &App, at: usize) -> usize {
    crate::rebind::heights(app.keys(), 80).iter().take(at).sum()
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
    down(&mut app, 21);
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
    down(&mut app, 17);
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
    let map = app.keymap().expect("open");
    assert!(
        map.visible(app.keys())
            .iter()
            .any(|binding| app.keys().shown(binding).contains("unbound")),
        "{map:?}"
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
    // No code under test waits on the clock here; it moves the frame time the app reads (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
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
    app.on_line(live(contract::SessionId(SESSION.to_owned())));
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
    app.notices.push("extension_failed", "first".to_owned());
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    assert_eq!(tap(&mut app, "esc"), Effect::None);
    assert!(app.notice_overlay().is_none());
    assert!(app.keys_screen_open());
    assert_eq!(tap(&mut app, "esc"), Effect::None);
    assert!(!app.keys_screen_open());
}

#[test]
fn the_report_above_the_screen_takes_the_strokes_first() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = homed(Some(Arc::clone(&fake)));
    slash_keys(&mut app);
    app.open_import_report(None, Default::default(), Vec::new());
    assert_eq!(app.top_surface(), crate::app::Surface::Report);
    // A letter reaches neither the screen nor the draft.
    assert_eq!(tap(&mut app, "x"), Effect::None);
    assert_eq!(app.draft(), "");
    assert!(fake.keys_saved().is_empty());
    // Esc closes the report, and the screen is the next Esc's.
    assert_eq!(tap(&mut app, "esc"), Effect::None);
    assert!(!app.import_report_open());
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
    app.notices.push("extension_failed", "first".to_owned());
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
    app.notices.push("extension_failed", "first".to_owned());
    app.open_more_notices();
    assert_eq!(tap_at(&mut app, "ctrl+c", clock.now()), Effect::None);
    assert!(app.hint());
    // No code under test waits on the clock here; it moves the frame time the app reads (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(
        QUIT_WINDOW
            .checked_sub(Duration::from_millis(1))
            .unwrap_or(QUIT_WINDOW),
    );
    assert_eq!(tap_at(&mut app, "ctrl+c", clock.now()), Effect::Quit);
}

/// A wheel report at (`col`, `row`): down steps toward the tail, up away
/// from it, over the conversation column the `/keys` screen takes.
fn wheel(col: u16, row: u16, down: bool) -> crate::keys::Mouse {
    crate::keys::Mouse {
        kind: if down {
            crate::keys::MouseKind::WheelDown
        } else {
            crate::keys::MouseKind::WheelUp
        },
        col,
        row,
    }
}

/// The wheel over the `/keys` screen moves three display lines a step,
/// never the conversation under it (`docs/tui.md:584`). Three Downs move
/// the selection three actions down, and the wheel steps the window under it.
#[test]
fn wheel_over_keys_steps_browse_not_the_conversation() {
    let mut app = homed(Some(Arc::new(Fake::new(Vec::new()))));
    // Scroll first, while the wheel still reaches the conversation: the
    // screen covers it once open.
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    app.open_keys();
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        tap(&mut app, "Down");
    }
    let frame = app.keys_frame(24).expect("the screen is open");
    assert_eq!(frame.list.selected(), first_line(&app, 3));
    let shown_top = frame.list.top();
    // The first two actions each wrap to two display lines at this
    // width, so three display lines from the top land on display line
    // 3, inside the second action, where the window rests
    // (`docs/tui.md`, "Bindings").
    let grown = crate::rebind::heights(app.keys(), 80);
    assert!(
        grown[0] > 1 && grown[1] > 1,
        "wrapped actions sit in view: {grown:?}"
    );
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.keys_frame(24).expect("the screen is open");
    assert_eq!(
        frame.list.top(),
        3,
        "one wheel step moves three display lines, not three actions: {grown:?}"
    );
    assert_eq!(shown_top, first_line(&app, 0));
    assert_eq!(
        frame.list.selected(),
        first_line(&app, 3),
        "a selection still on screen stays where it was"
    );
}

/// The wheel over the open key map is swallowed across the content
/// area it replaces: the scrolled conversation under it keeps its
/// scroll and the map stays open (`docs/tui.md`, "Key map"; see
/// #3578). Points near the area's corners stand in for the whole
/// drawn area.
#[test]
fn wheel_over_keymap_leaves_a_scrolled_conversation() {
    let mut app = homed(Some(Arc::new(Fake::new(Vec::new()))));
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    assert_eq!(app.on_key(Key::F1, now()), Effect::None);
    assert!(app.keymap_top().is_some(), "the key map opened");
    let top = app.top();
    for (col, row) in [(1u16, 1u16), (10, 5), (40, 12), (70, 20)] {
        assert_eq!(
            app.on_wheel(&wheel(col, row, false)),
            Effect::None,
            "the wheel at {col},{row} is swallowed"
        );
        assert_eq!(
            app.on_wheel(&wheel(col, row, true)),
            Effect::None,
            "the wheel at {col},{row} is swallowed"
        );
    }
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert!(app.keymap_top().is_some(), "the key map stays open");
}

/// The notice overlay swallows the wheel only inside its drawn slab,
/// so the hidden `/keys` list does not move there and neither does
/// the conversation; outside the slab the wheel reaches the list.
#[test]
fn wheel_over_a_notice_over_keys_moves_neither_list_nor_conversation() {
    use crate::app::Surface;
    use crate::app::top::tests::open;
    let mut app = homed(Some(Arc::new(Fake::new(Vec::new()))));
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    app.open_keys();
    open(&mut app, Surface::Notice);
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    let slab = crate::view::frame_of(&app, Surface::Notice);
    assert!(!slab.is_empty(), "the notice overlay draws a slab");
    // The router swallows by the recorded slab: note the frame's slab
    // as a draw would.
    app.frames().note(Surface::Notice, slab);
    let before = app
        .keys_frame(24)
        .expect("the screen is open")
        .list
        .selected();
    app.on_wheel(&wheel(slab.x + 1, slab.y + 1, false));
    assert_eq!(
        app.keys_frame(24)
            .expect("the screen is open")
            .list
            .selected(),
        before,
        "the overlay swallows the wheel before the hidden list"
    );
    assert_eq!(app.top(), top, "the wheel left the conversation");
    // Above the slab the list shows: the wheel shifts its window three
    // display lines from the fresh top, resting inside the second
    // action (`docs/tui.md`, "Bindings").
    let frame = app.keys_frame(24).expect("the screen is open");
    assert_eq!(frame.list.selected(), before);
    assert_eq!(frame.list.top(), first_line(&app, 0));
    let grown = crate::rebind::heights(app.keys(), 80);
    assert!(
        grown[0] > 1 && grown[1] > 1,
        "wrapped actions sit in view: {grown:?}"
    );
    app.on_wheel(&wheel(slab.x, slab.y.saturating_sub(1), true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert_eq!(
        app.keys_frame(24).expect("the screen is open").list.top(),
        3,
        "outside the slab the wheel reaches the list"
    );
}

/// The wrapper shifts the Browse window three display lines per call,
/// keeping the selection while it shows: down from the top rests
/// inside the second action, moving the selection to the action at
/// the first visible line, and back up keeps it.
#[test]
fn keys_screen_wheel_shifts_browse_three_display_lines() {
    let mut app = homed(Some(Arc::new(Fake::new(Vec::new()))));
    app.open_keys();
    let grown = crate::rebind::heights(app.keys(), 80);
    assert!(
        grown[0] > 1 && grown[1] > 1,
        "wrapped actions sit in view: {grown:?}"
    );
    assert_eq!(app.keys_screen_wheel(false), Effect::None);
    let frame = app.keys_frame(24).expect("the screen is open");
    assert_eq!(frame.list.top(), 3);
    assert_eq!(
        frame.list.selected(),
        first_line(&app, 1),
        "the selection moves to the action at the first visible line"
    );
    assert_eq!(app.keys_screen_wheel(true), Effect::None);
    let frame = app.keys_frame(24).expect("the screen is open");
    assert_eq!(frame.list.top(), first_line(&app, 0));
    assert_eq!(
        frame.list.selected(),
        first_line(&app, 1),
        "a selection still on screen stays where it was"
    );
}

/// The wrapper scrolls through an open notice overlay's state: the
/// router's recorded slab owns the swallow, not the wrapper.
#[test]
fn keys_screen_wheel_through_an_open_notice_scrolls() {
    let mut app = homed(Some(Arc::new(Fake::new(Vec::new()))));
    app.open_keys();
    app.notices.push("extension_failed", "first".to_owned());
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    assert_eq!(app.keys_screen_wheel(false), Effect::None);
    assert_eq!(
        app.keys_frame(24).expect("the screen is open").list.top(),
        3,
        "the wrapper leaves the notice swallow to the router"
    );
}

/// The frame is there while the screen is open, with the keys
/// content, and gone once it closes: no frame, no rows, and one row
/// per display line with its join while open.
#[test]
fn keys_frame_is_some_with_the_keys_content_while_open() {
    let mut app = homed(Some(Arc::new(Fake::new(Vec::new()))));
    assert!(app.keys_frame(24).is_none());
    app.open_keys();
    let frame = app.keys_frame(24).expect("the screen is open");
    assert_eq!(frame.title, "Keys");
    assert_eq!(frame.command, "/keys");
    assert!(!frame.rows.is_empty());
    assert_eq!(frame.rows.len(), frame.joins.len());
    assert!(app.keys_screen_open());
}

/// The draw width mirrors the area the frame draws in: the screen's
/// on home, the attached conversation column's otherwise, so the
/// frame wraps at the width it draws. A zero or one width would wrap
/// every row into a line per character.
#[test]
fn keys_width_mirrors_the_area_the_frame_draws() {
    let mut app = home(None);
    app.open_keys();
    assert_eq!(app.keys_width(), 80);
    let mut app = homed(Some(Arc::new(Fake::new(Vec::new()))));
    app.open_keys();
    assert_eq!(app.keys_width(), usize::from(app.column_width()));
}

/// A click selects its row's action while browsing, and the cross
/// closes the screen: the wrapper answers both with no effect of its
/// own.
#[test]
fn keys_screen_click_selects_a_row_and_closes_on_the_cross() {
    use crate::swapped::Spot;
    let mut app = homed(Some(Arc::new(Fake::new(Vec::new()))));
    app.open_keys();
    let at = first_line(&app, 5);
    assert_eq!(app.keys_screen_click(Spot::Row(at)), Effect::None);
    assert!(app.keys_screen_open());
    let frame = app.keys_frame(24).expect("the screen is open");
    assert_eq!(frame.list.selected(), at);
    assert_eq!(app.keys_screen_click(Spot::Close), Effect::None);
    assert!(!app.keys_screen_open());
}
