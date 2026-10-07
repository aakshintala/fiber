//! Home's frame: snapshots and the cursor.

use super::super::{cursor, render, text};
use crate::app::{App, QUIT_HINT};
use crate::home::{Launch, Spot};
use crate::keys::Key;
use crate::link::Line;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use std::path::PathBuf;

/// An app on home at `width` by `height`.
fn home(width: u16, height: u16) -> App {
    home_with_glyph(width, height, "⌇")
}

/// An app on home at `width` by `height` with the one-row logo's `glyph`.
fn home_with_glyph(width: u16, height: u16, glyph: &str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: glyph.to_owned(),
    });
    app.set_size(width, height);
    app
}

/// An app on home inside git at `width` by `height`: scoped to the
/// launch project.
fn git_home(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: true,
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
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
    // The pad is three rows, then the four-row logo and one blank row,
    // the ▄ edge and the draft's first row; the cursor sits after "> hi".
    assert_eq!(cursor(&app, area), Some(Position::new(4, 9)));
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

/// A live `session_status` for `session`, named `name`, in `workspace`.
fn status_in(session: &str, name: &str, workspace: &str) -> Line {
    let mut payload = serde_json::json!({
        "name": name,
        "workspace": workspace,
        "project": "-w",
        "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in live().as_object().cloned().unwrap_or_default() {
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

#[test]
fn the_picker_keeps_its_selection_drawn() {
    let mut app = home(80, 24);
    app.on_line(hello());
    for n in 0..9u8 {
        app.on_line(status_in(
            &format!("s_{n:016x}"),
            "fix the parser",
            &format!("/repo{n}"),
        ));
    }
    app.on_click(crate::mouse::TargetId::Home(Spot::Workspace));
    let now = fakes::clock::FakeClock::new().now();
    // Ten entries, fewer rows above the box: every selection draws,
    // scrolling the list around it.
    for selected in 0..10usize {
        let entry = app
            .home_screen()
            .and_then(|screen| screen.picker)
            .and_then(|(list, at)| (at == selected).then(|| list[selected].clone()))
            .unwrap_or_else(|| panic!("selection {selected}"));
        // A picker row can share its row with the logo it draws over,
        // so the entry heads its line.
        let drawn = screen(&app, 80, 24);
        assert!(
            drawn.lines().any(|row| row.starts_with(&entry)),
            "selection {selected} ({entry}) draws"
        );
        app.on_key(Key::Down, now);
    }
    insta::assert_snapshot!("home_picker_scrolled", screen(&app, 80, 24));
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
fn home_four_row_logo_80x24() {
    insta::assert_snapshot!("home_four_row_logo_80x24", screen(&home(80, 24), 80, 24));
}

#[test]
fn home_one_row_logo_at_the_height_limit() {
    insta::assert_snapshot!(
        "home_one_row_logo_at_the_height_limit",
        screen(&home(80, 16), 80, 16)
    );
}

#[test]
fn the_four_row_logo_needs_its_height_exactly() {
    // At 80 columns the threshold is 17 rows: a pad of 2, the four-row
    // logo, one blank row, the six-row box, three list rows, the foot.
    let tall = screen(&home(80, 17), 80, 17);
    assert!(tall.contains('█'), "four pixel rows at the threshold");
    let short = screen(&home(80, 16), 80, 16);
    assert!(!short.contains('█'), "one row below the threshold");
    assert!(short.contains("⌇ fiber 0.0.1"), "the one-row logo");
}

#[test]
fn a_screen_narrower_than_the_logo_gets_one_row() {
    let narrow = screen(&home(20, 24), 20, 24);
    assert!(!narrow.contains('█'), "no pixel rows without the width");
    assert!(narrow.contains("⌇ fiber 0.0.1"), "the one-row logo");
}

#[test]
fn the_logo_glyph_setting_changes_the_one_row_logo() {
    // The four-row logo's wave is drawn pixels, and never changes.
    let tall = screen(&home_with_glyph(80, 24, "≈"), 80, 24);
    assert!(tall.contains('█'), "four pixel rows");
    assert!(!tall.contains('≈'), "no glyph in the pixel logo");
    let short = screen(&home_with_glyph(80, 16, "≈"), 80, 16);
    assert!(short.contains("≈ fiber 0.0.1"), "the setting's glyph");
    assert!(!short.contains('⌇'), "no wave where the glyph goes");
}

#[test]
fn home_chips() {
    // The workspace, the model, the thinking level, and what Enter does.
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
        model: Some("test/model".to_owned()),
        thinking: Some("high".to_owned()),
        logo_glyph: "⌇".to_owned(),
    });
    app.set_size(80, 24);
    insta::assert_snapshot!("home_chips", screen(&app, 80, 24));
}

#[test]
fn home_picker_open() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_click(crate::mouse::TargetId::Home(Spot::Workspace));
    insta::assert_snapshot!("home_picker_open", screen(&app, 80, 24));
}

/// A waiting `session_status` for `session` in another project.
fn away_waiting() -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": "away",
            "workspace": "/lens",
            "project": "-other",
            "state": "waiting",
            "waiting": {"request_id": "r_1", "kind": "approval",
                "summary": "shell"},
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

#[test]
fn home_scoped_with_the_toggle() {
    let mut app = git_home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "here", idle()));
    app.on_line(away_waiting());
    insta::assert_snapshot!("home_scoped_with_the_toggle", screen(&app, 80, 24));
}

#[test]
fn home_with_blocker_lines() {
    // A rejected `start` draws its message above the box, wrapped at its
    // width.
    let mut app = home(80, 24);
    app.on_line(hello());
    type_draft(&mut app, "hi");
    let now = fakes::clock::FakeClock::new().now();
    let crate::app::Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter sends the start");
    };
    let start: serde_json::Value =
        serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    let id = start["id"].as_str().unwrap_or_else(|| panic!("start id"));
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), serde_json::Value::String(id.to_owned())),
            ("code".to_owned(), serde_json::Value::String("start_failed".to_owned())),
            (
                "message".to_owned(),
                serde_json::Value::String(
                    "No API key for test/model in this workspace. Run `fiber login` and press Enter to try again, or pick another model.\nA second line stays short."
                        .to_owned(),
                ),
            ),
        ]
        .into_iter()
        .collect(),
    }));
    insta::assert_snapshot!("home_with_blocker_lines", screen(&app, 80, 24));
}

#[test]
fn blockers_count_against_the_four_row_logo() {
    // Three blocker lines at the 17-row threshold leave no room for the
    // pixel logo: the one-row form draws, with the blockers above the
    // box.
    let mut app = home(80, 17);
    app.on_line(hello());
    type_draft(&mut app, "hi");
    let now = fakes::clock::FakeClock::new().now();
    let crate::app::Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter sends the start");
    };
    let start: serde_json::Value =
        serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    let id = start["id"].as_str().unwrap_or_else(|| panic!("start id"));
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            (
                "command_id".to_owned(),
                serde_json::Value::String(id.to_owned()),
            ),
            (
                "code".to_owned(),
                serde_json::Value::String("start_failed".to_owned()),
            ),
            (
                "message".to_owned(),
                serde_json::Value::String("one\ntwo\nthree".to_owned()),
            ),
        ]
        .into_iter()
        .collect(),
    }));
    let text = screen(&app, 80, 17);
    assert!(!text.contains('█'), "one row while the blockers show");
    assert!(text.contains("⌇ fiber 0.0.1"), "the one-row logo");
    assert_eq!(
        app.home_screen()
            .map(|screen| screen.blockers)
            .unwrap_or_default(),
        ["one", "two", "three"]
    );
}

#[test]
fn the_toggle_keeps_a_row_for_the_list() {
    // Nine rows fit under the box: with the toggle heading the list,
    // eight session rows draw.
    let mut app = git_home(80, 24);
    app.on_line(hello());
    for n in 0..10u8 {
        let session = format!("s_{n:016x}");
        app.on_line(status(&session, "here", idle()));
    }
    app.on_line(away_waiting());
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    let toggles = targets
        .iter()
        .filter(|target| target.id == crate::mouse::TargetId::Home(Spot::Toggle))
        .count();
    let entries = targets
        .iter()
        .filter(|target| matches!(target.id, crate::mouse::TargetId::Home(Spot::Entry(_))))
        .count();
    assert_eq!(toggles, 1);
    assert_eq!(entries, 8);
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
    // Nine rows fit under the four-row logo's box, each with its ✕:
    // nineteen steps focus the tenth row below the fold.
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..19 {
        app.on_key(Key::Down, now);
    }
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(crate::home::Spot::Entry(
            app.home_screen()
                .and_then(|screen| screen.rows.get(9).map(|(key, _, _)| *key))
                .unwrap_or_else(|| panic!("a tenth row"))
        )))
    );
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
    assert_eq!(drawn.len(), 9);
    assert_eq!(drawn.last(), Some(&keys[9]));
    assert!(!drawn.contains(&keys[0]));
    // One step reaches the row's ✕, and the list still ends there.
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    app.drawn(&targets);
    app.on_key(Key::Down, now);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(crate::home::Spot::Stop(
            keys[9]
        )))
    );
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
    assert_eq!(drawn.last(), Some(&keys[9]));
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

/// A hub `command_rejected` for `id`, with `code` and `message`.
fn refused(id: &str, code: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            (
                "command_id".to_owned(),
                serde_json::Value::String(id.to_owned()),
            ),
            (
                "code".to_owned(),
                serde_json::Value::String(code.to_owned()),
            ),
            (
                "message".to_owned(),
                serde_json::Value::String(message.to_owned()),
            ),
        ]
        .into_iter()
        .collect(),
    })
}

/// A `session_left` for `session`, ending `how`.
fn left(session: &str, how: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"session_id": session, "how": how})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// The first row's key.
fn first_key(app: &App) -> u64 {
    app.home_screen()
        .and_then(|screen| screen.rows.into_iter().next().map(|(key, _, _)| key))
        .unwrap_or_else(|| panic!("a row"))
}

/// Clicks the first row's ✕.
fn stop_first(app: &mut App) {
    let key = first_key(app);
    app.on_click(crate::mouse::TargetId::Home(Spot::Stop(key)));
}

#[test]
fn home_rows_with_their_x() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "tidy docs", idle()));
    app.on_line(left("s_bbbbbbbbbbbbbbbb", "exited"));
    // An unreadable row ends in no ✕.
    let mut unreadable = status("s_cccccccccccccccc", "future work", live());
    if let Line::Session(status) = &mut unreadable {
        status.schema_version = contract::SCHEMA_VERSION + 1;
    }
    app.on_line(unreadable);
    insta::assert_snapshot!("home_rows_with_their_x", screen(&app, 80, 24));
}

#[test]
fn home_delete_question() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited"));
    stop_first(&mut app);
    insta::assert_snapshot!("home_delete_question", screen(&app, 80, 24));
}

#[test]
fn home_cascade_question() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_0123456789abcdef", "fix the parser", live()));
    app.on_line(left("s_0123456789abcdef", "exited"));
    stop_first(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let crate::app::Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete: serde_json::Value =
        serde_json::from_str(lines.first().unwrap_or_else(|| panic!("a delete line")))
            .unwrap_or_else(|err| panic!("{err}"));
    let id = delete["id"].as_str().unwrap_or_else(|| panic!("delete id"));
    app.on_line(refused(
        id,
        "session_has_dependents",
        "Session `s_0123456789abcdef` has sessions that continue it: \
        `s_1111111111111111`, `s_2222222222222222`. `--cascade` deletes them too.",
    ));
    insta::assert_snapshot!("home_cascade_question", screen(&app, 80, 24));
}

/// Opens the cascade question naming `dependents`: the plain delete
/// goes out, and the hub refuses naming them all.
fn ask_cascade(app: &mut App, root: &str, dependents: &[String]) {
    stop_first(app);
    let now = fakes::clock::FakeClock::new().now();
    let crate::app::Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete: serde_json::Value =
        serde_json::from_str(lines.first().unwrap_or_else(|| panic!("a delete line")))
            .unwrap_or_else(|err| panic!("{err}"));
    let id = delete["id"].as_str().unwrap_or_else(|| panic!("delete id"));
    let names: Vec<String> = dependents
        .iter()
        .map(|session| format!("`{session}`"))
        .collect();
    app.on_line(refused(
        id,
        "session_has_dependents",
        &format!(
            "Session `{root}` has sessions that continue it: {}. \
            `--cascade` deletes them too.",
            names.join(", ")
        ),
    ));
}

#[test]
fn home_cascade_question_with_many_dependents_wraps_every_id() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_0123456789abcdef", "fix the parser", live()));
    app.on_line(left("s_0123456789abcdef", "exited"));
    let dependents: Vec<String> = (1..=8u8).map(|n| format!("s_{n:016x}")).collect();
    ask_cascade(&mut app, "s_0123456789abcdef", &dependents);
    let drawn = screen(&app, 80, 24);
    // The question wraps over as many rows as needed, naming every id
    // the delete adds: none is cut to one line.
    for session in std::iter::once(&"s_0123456789abcdef".to_owned()).chain(&dependents) {
        assert!(drawn.contains(session.as_str()), "{session} is drawn");
    }
    insta::assert_snapshot!("home_cascade_question_wrapped", drawn);
}

#[test]
fn the_delete_question_scrolls_past_the_screen() {
    let mut app = home(80, 14);
    app.on_line(hello());
    app.on_line(status("s_0123456789abcdef", "fix the parser", live()));
    app.on_line(left("s_0123456789abcdef", "exited"));
    let dependents: Vec<String> = (1..=20u8).map(|n| format!("s_{n:016x}")).collect();
    ask_cascade(&mut app, "s_0123456789abcdef", &dependents);
    // More wrapped rows than fit: the first id draws, the last does not.
    let first = screen(&app, 80, 14);
    assert!(first.contains("s_0000000000000001"), "the first id draws");
    assert!(
        !first.contains("s_0000000000000014"),
        "the last id is past the screen"
    );
    // Down scrolls until every id can be seen before Enter.
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..20 {
        app.on_key(Key::Down, now);
    }
    let last = screen(&app, 80, 14);
    assert!(last.contains("s_0000000000000014"), "the last id draws");
    // Up scrolls back to the first rows.
    for _ in 0..20 {
        app.on_key(Key::Up, now);
    }
    let back = screen(&app, 80, 14);
    assert!(back.contains("s_0000000000000001"), "the first id draws");
    assert!(
        !back.contains("s_0000000000000014"),
        "the last id is past the screen again"
    );
}
fn ask_quit(app: &mut App) {
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::CtrlC, now);
    app.on_key(Key::CtrlC, now);
}

#[test]
fn quit_prompt_on_home() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "tidy docs", idle()));
    ask_quit(&mut app);
    insta::assert_snapshot!("quit_prompt_on_home", screen(&app, 80, 24));
}

#[test]
fn quit_prompt_over_a_session() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.attach(contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()));
    ask_quit(&mut app);
    insta::assert_snapshot!("quit_prompt_over_a_session", screen(&app, 80, 24));
}
