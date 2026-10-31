//! Tests for the `/keys` screen's frame: what it draws on home and
//! attached, the capture and the clash prompt, its targets and the hidden
//! cursor (`docs/tui.md`, "Swapped views").

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use contract::clock::Clock;

use crate::app::{App, Effect};
use crate::home::Launch;
use crate::keys::Key;
use crate::mouse::TargetId;
use crate::stroke::Stroke;
use crate::swapped::Spot;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
fn now() -> std::time::Instant {
    fakes::clock::FakeClock::new().now()
}

/// The stroke `name` names.
fn stroke(name: &str) -> Stroke {
    Stroke::parse(name).unwrap_or_else(|err| panic!("{name}: {err}"))
}

/// An app on home at `width` by `height`.
fn home(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    app.set_size(width, height);
    app
}

/// An app attached to [`SESSION`] at `width` by `height`.
fn attached(width: u16, height: u16) -> App {
    let mut app = home(width, height);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// An app attached to [`SESSION`] before sizing, so the layout takes
/// the conversation column: at 160 by 40 the column is 102 wide, as in
/// the chrome layout tests.
fn attached_sized(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        rail_share: 15.0,
        panel_share: 21.0,
        ..Default::default()
    });
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app
}

/// Types `/keys` and presses Enter.
fn slash_keys(app: &mut App) {
    for ch in "/keys".chars() {
        assert_eq!(app.on_key(Key::Char(ch), now()), Effect::None);
    }
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.keys_screen_open());
}

/// Presses the stroke `name` names.
fn tap(app: &mut App, name: &str) {
    assert_eq!(app.on_press(stroke(name), now()), Effect::None);
}

/// Renders `app` on a `width` by `height` screen as text.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

/// Renders `app` on a `width` by `height` screen, returning its targets.
fn drawn(app: &App, width: u16, height: u16) -> Vec<crate::mouse::Target> {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None)
}

#[test]
fn keys_attached_80x24() {
    let mut app = attached(80, 24);
    slash_keys(&mut app);
    insta::assert_snapshot!("keys_attached_80x24", screen(&app, 80, 24));
}

#[test]
fn keys_on_home_80x24() {
    let mut app = home(80, 24);
    slash_keys(&mut app);
    insta::assert_snapshot!("keys_on_home_80x24", screen(&app, 80, 24));
}

#[test]
fn keys_attached_40x12() {
    let mut app = attached(40, 12);
    slash_keys(&mut app);
    insta::assert_snapshot!("keys_attached_40x12", screen(&app, 40, 12));
}

#[test]
fn keys_capture_80x24() {
    let mut app = attached(80, 24);
    slash_keys(&mut app);
    for _ in 0..5 {
        tap(&mut app, "down");
    }
    tap(&mut app, "enter");
    insta::assert_snapshot!("keys_capture_80x24", screen(&app, 80, 24));
}

#[test]
fn keys_clash_80x24() {
    let mut app = attached(80, 24);
    slash_keys(&mut app);
    tap(&mut app, "enter");
    tap(&mut app, "ctrl+o");
    insta::assert_snapshot!("keys_clash_80x24", screen(&app, 80, 24));
}

#[test]
fn keys_failed_save_80x24() {
    use std::sync::Arc;
    let fake = Arc::new(crate::configure_fake::Fake::new(Vec::new()));
    if let Ok(mut refuse) = fake.refuse.lock() {
        *refuse = Some("disk full".to_owned());
    }
    let mut app = attached(80, 24);
    app.set_configure(Some(fake as Arc<dyn crate::Configure>));
    slash_keys(&mut app);
    for _ in 0..5 {
        tap(&mut app, "down");
    }
    tap(&mut app, "enter");
    tap(&mut app, "ctrl+t");
    insta::assert_snapshot!("keys_failed_save_80x24", screen(&app, 80, 24));
}

/// Attached wide, the screen wraps at the conversation column, not
/// the screen: at 160 by 40 the column is 102 wide, so the move_word
/// keys wrap inside the column's 18-wide keys instead of standing
/// whole as at 160 wide.
#[test]
fn keys_attached_160x40_wraps_at_the_column_width() {
    let mut app = attached_sized(160, 40);
    assert_eq!(app.column_width(), 102);
    // The columns at the column width differ from the screen width's:
    // natural widths stand at 160, the share runs at 102.
    assert_eq!(super::columns(app.keys(), 102), (18, 20, 60));
    assert_eq!(super::columns(app.keys(), 160), (27, 20, 91));
    slash_keys(&mut app);
    let rows: Vec<String> = screen(&app, 160, 40).lines().map(str::to_owned).collect();
    let at = rows
        .iter()
        .position(|row| row.contains("Ctrl+←") && !row.contains("Ctrl+→,"))
        .expect("a row wrapping the move_word keys");
    assert!(
        rows[at.saturating_add(1)].contains("Ctrl+→,"),
        "the continuation shows the wrapped keys: {:?}",
        rows
    );
}

#[test]
fn the_frame_targets_hold_the_cross_and_a_target_per_display_line() {
    // Home draws the swapped view over the whole area, tall enough
    // for every wrapped line to show: the draw clips to the area.
    let mut app = home(80, 60);
    slash_keys(&mut app);
    let targets = drawn(&app, 80, 60);
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::View(Spot::Close)),
        "{targets:?}"
    );
    // Every display line of every wrapped action takes a target, not
    // one per action.
    let grown = crate::rebind::heights(app.keys(), 80);
    let frame = app.keys_frame(60).expect("the screen is open");
    let shown = crate::swapped::rows_height(&frame, 60);
    let total: usize = grown.iter().sum();
    let rows = targets
        .iter()
        .filter(|target| matches!(target.id, TargetId::View(Spot::Row(_))))
        .count();
    assert_eq!(rows, total.min(shown), "{targets:?}");
}

#[test]
fn the_cursor_hides_while_the_screen_is_open() {
    let area = Rect::new(0, 0, 80, 24);
    let mut app = attached(80, 24);
    slash_keys(&mut app);
    assert_eq!(crate::view::cursor(&app, area), None);
    let mut app = home(80, 24);
    slash_keys(&mut app);
    assert_eq!(crate::view::cursor(&app, area), None);
}

#[test]
fn a_click_on_the_cross_closes_the_screen() {
    let mut app = attached(80, 24);
    slash_keys(&mut app);
    assert_eq!(app.on_click(TargetId::View(Spot::Close)), Effect::None);
    assert!(!app.keys_screen_open());
}

#[test]
fn home_80x24_wraps_delete_session_and_down_focuses_recall_prompt() {
    let mut app = home(80, 24);
    slash_keys(&mut app);
    for _ in 0..7 {
        tap(&mut app, "down");
    }
    let rows: Vec<String> = screen(&app, 80, 24).lines().map(str::to_owned).collect();
    // The delete_session row shows Delete, Backspace in full across two
    // display lines: the first ends its keys cell with the comma, the
    // second starts it with Backspace (`docs/tui.md`, "Bindings").
    let at = rows
        .iter()
        .position(|row| row.contains("Delete,"))
        .expect("a row showing the delete_session keys");
    assert!(
        !rows[at].contains("Backspace"),
        "the first wrapped line ends at the comma: {}",
        rows[at]
    );
    assert!(
        rows[at.saturating_add(1)].contains("Backspace"),
        "the second wrapped line shows Backspace: {:?}",
        rows
    );
    // One down-arrow from it focuses the next action, recall_prompt.
    tap(&mut app, "down");
    let rows: Vec<String> = screen(&app, 80, 24).lines().map(str::to_owned).collect();
    assert!(
        rows.iter().any(|row| row.starts_with("›")
            && row.contains("recall_prompt")
            && row.contains("Recall an earlier")),
        "recall_prompt focused after one down: {:?}",
        rows
    );
}

#[test]
fn the_screen_draws_first_on_home_and_attached() {
    let mut app = home(80, 24);
    slash_keys(&mut app);
    let rows: Vec<String> = screen(&app, 80, 24).lines().map(str::to_owned).collect();
    assert!(rows.iter().any(|row| row.starts_with(" Keys")), "{rows:?}");
    let mut app = attached(80, 24);
    slash_keys(&mut app);
    let rows: Vec<String> = screen(&app, 80, 24).lines().map(str::to_owned).collect();
    // Attached, the grip takes column 0, so the view starts one in.
    assert!(rows.iter().any(|row| row.starts_with("  Keys")), "{rows:?}");
}
