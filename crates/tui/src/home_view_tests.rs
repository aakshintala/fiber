//! Home's frame: snapshots and the cursor.

use super::super::{cursor, render, text};
use crate::app::{App, QUIT_HINT};
use crate::home::Launch;
use crate::keys::Key;
use crate::link::Line;
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
fn home_with_live_and_exited_rows() {
    let mut app = home(80, 24);
    let lines: Vec<serde_json::Value> = app
        .on_line(hello())
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "tidy docs", idle()));
    app.on_line(accepted(
        recent,
        serde_json::json!({"sessions": [
            recent_row("s_cccccccccccccccc", "old work", "exited"),
            recent_row("s_dddddddddddddddd", "dead work", "crashed"),
        ]}),
    ));
    insta::assert_snapshot!("home_with_live_and_exited_rows", screen(&app, 80, 24));
}

#[test]
fn home_with_more_rows_than_fit() {
    let mut app = home(80, 24);
    app.on_line(hello());
    for n in 0..15u8 {
        let session = format!("s_{n:016x}");
        app.on_line(status(&session, "fix the parser", idle()));
    }
    assert_eq!(app.home_screen().map(|screen| screen.rows.len()), Some(15));
    insta::assert_snapshot!("home_with_more_rows_than_fit", screen(&app, 80, 24));
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

/// The hub's answer to the `recent` command `id`.
fn accepted(id: &str, result: serde_json::Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            (
                "command_id".to_owned(),
                serde_json::Value::String(id.to_owned()),
            ),
            ("result".to_owned(), result),
        ]
        .into_iter()
        .collect(),
    })
}

/// A live `session_status` for `session`, named `name`, in `state`.
fn status(session: &str, name: &str, state: serde_json::Value) -> Line {
    let mut payload = serde_json::json!({
        "name": name,
        "workspace": "/w",
        "project": "-w",
        "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// One exited `recent` row.
fn recent_row(session: &str, name: &str, how: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": session,
        "ts": 0,
        "project": "-w",
        "workspace": "/w",
        "name": name,
        "how": how,
    })
}

/// A streaming state.
fn live() -> serde_json::Value {
    serde_json::json!({"state": "streaming"})
}

/// An idle state.
fn idle() -> serde_json::Value {
    serde_json::json!({"state": "idle"})
}

#[test]
fn home_cursor_hides_while_navigating() {
    let mut app = home(80, 24);
    // Pasted text past the token line count becomes one paste token, a
    // click target focus can move to.
    let pasted: Vec<String> = (1..=312).map(|n| format!("line {n}")).collect();
    app.on_edit(crate::keys::Edit::Paste(pasted.join("\n")));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::BackTab, now);
    assert!(app.focused().is_some());
    assert_eq!(cursor(&app, area), None);
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

#[test]
fn a_focused_row_below_the_fold_is_drawn_last() {
    let mut app = home(80, 24);
    app.on_line(hello());
    for n in 0..15u8 {
        let session = format!("s_{n:016x}");
        app.on_line(status(&session, "fix the parser", idle()));
    }
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    app.drawn(&targets);
    // Twelve rows fit under the box: twelve steps focus the last drawn
    // row, and the thirteenth moves below the fold.
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..13 {
        app.on_key(Key::Down, now);
    }
    let keys: Vec<u64> = app
        .home_screen()
        .map(|screen| screen.rows.into_iter().map(|(key, _, _)| key).collect())
        .unwrap_or_default();
    assert_eq!(keys.len(), 15);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    let drawn: Vec<u64> = targets
        .iter()
        .filter_map(|target| {
            if let crate::mouse::TargetId::Home(crate::home::Spot::Entry(key)) = target.id {
                Some(key)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(drawn.len(), 12);
    assert_eq!(drawn.last(), Some(&keys[12]));
    assert!(!drawn.contains(&keys[0]));
}

#[test]
fn home_with_a_focused_row() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "tidy docs", idle()));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    app.drawn(&targets);
    app.on_key(Key::Down, fakes::clock::FakeClock::new().now());
    insta::assert_snapshot!("home_with_a_focused_row", screen(&app, 80, 24));
}
