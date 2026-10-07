//! Home's frame: snapshots and the cursor.

use super::super::{cursor, render, text};
use crate::app::{App, QUIT_HINT};
use crate::home::Launch;
use crate::keys::Key;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use std::path::PathBuf;

/// An app on home at `width` by `height`.
fn home(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
    });
    app.set_size(width, height);
    app
}

/// Renders `app` on a `width` by `height` screen as text.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    text(&buf)
}

/// Types `text` into the draft, `\n` as Shift+Enter.
fn type_draft(app: &mut App, text: &str) {
    let now = fakes::clock::FakeClock::new().now();
    for ch in text.chars() {
        if ch == '\n' {
            app.on_edit(crate::keys::Edit::ShiftEnter);
        } else {
            app.on_key(Key::Char(ch), now);
        }
    }
}

#[test]
fn home_first_frame_80x24() {
    insta::assert_snapshot!("home_first_frame_80x24", screen(&home(80, 24), 80, 24));
}

#[test]
fn home_first_frame_40x10() {
    insta::assert_snapshot!("home_first_frame_40x10", screen(&home(40, 10), 40, 10));
}

#[test]
fn home_with_a_draft_of_two_lines() {
    let mut app = home(80, 24);
    type_draft(&mut app, "first\nsecond");
    insta::assert_snapshot!("home_with_a_draft_of_two_lines", screen(&app, 80, 24));
}

#[test]
fn home_with_the_slash_panel_above_the_box() {
    let mut app = home(80, 24);
    type_draft(&mut app, "/");
    assert!(app.completions().is_some());
    insta::assert_snapshot!(
        "home_with_the_slash_panel_above_the_box",
        screen(&app, 80, 24)
    );
}

#[test]
fn home_with_a_notice() {
    let mut app = home(80, 24);
    app.connect_failed("Could not reach the hub: refused".to_owned());
    insta::assert_snapshot!("home_with_a_notice", screen(&app, 80, 24));
}

#[test]
fn home_quit_hint() {
    let mut app = home(80, 24);
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::CtrlC, now);
    assert!(
        app.home_screen()
            .is_some_and(|screen| screen.foot == QUIT_HINT)
    );
    insta::assert_snapshot!("home_quit_hint", screen(&app, 80, 24));
}

#[test]
fn home_cursor_sits_at_the_draft_cursor_in_the_box() {
    let mut app = home(80, 24);
    type_draft(&mut app, "hi");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    // The pad is three rows, then the logo and one blank row, the ▄ edge
    // and the draft's first row; the cursor sits after "> hi".
    assert_eq!(cursor(&app, area), Some(Position::new(4, 6)));
}

#[test]
fn a_draft_hides_the_placeholder() {
    let mut app = home(80, 24);
    assert!(app.home_screen().is_some_and(|screen| screen.placeholder));
    type_draft(&mut app, "x");
    assert!(app.home_screen().is_some_and(|screen| !screen.placeholder));
}

#[test]
fn the_placeholder_goes_after_a_start_is_sent() {
    let mut app = home(80, 24);
    app.on_line(crate::link::Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    type_draft(&mut app, "hi");
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Enter, now);
    assert!(app.home_screen().is_some_and(|screen| !screen.placeholder));
}
