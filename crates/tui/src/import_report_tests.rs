//! The accepted-import report overlay (`docs/tui.md`, "Notices",
//! "Overlays"): one scrollable overlay per accepted import, no notices.
//!
//! The ticket's screen test: a 120-line import answer opens one overlay
//! with both counts and no notice rows; scrolled to the bottom the last
//! row and the footer show; Esc closes it with no notices left behind.

use ratatui::backend::TestBackend;
use serde_json::{Value, json};

use crate::app::commands::commands_testkit::{attached, enter, now, sent, session_line};
use crate::app::{App, Effect};
use crate::keys::Key;
use crate::link::Line;

/// The 120-line import answer: 60 copied names across the 4 kinds and
/// 60 left-behind items, each one row in the report.
fn big_result() -> Value {
    let names = |prefix: &str| -> Vec<Value> {
        (1..=15)
            .map(|n| Value::String(format!("{prefix}-{n:02}")))
            .collect()
    };
    let left: Vec<Value> = (1..=60)
        .map(|n| {
            json!({"item": format!("left-{n:02}.json"), "reason": "credentials are never imported"})
        })
        .collect();
    json!({
        "imported": {
            "mcp_servers": names("server"),
            "skills": names("skill"),
            "commands": names("command"),
            "instructions": names("AGENTS.md"),
        },
        "left_behind": left,
    })
}

/// `app` painted whole on an 80x24 in-memory screen.
fn painted(app: &mut App) -> crate::screen::Screen<TestBackend> {
    painted_sized(app, 80, 24)
}

/// `app` painted whole on a `width` by `height` in-memory screen; the
/// app is resized to match first, so paging and framing agree.
fn painted_sized(app: &mut App, width: u16, height: u16) -> crate::screen::Screen<TestBackend> {
    app.set_size(width, height);
    let vars = [("COLORTERM".to_owned(), "truecolor".to_owned())];
    let (look, _) = crate::look::Look::new(crate::ThemeSetting::Follow, &|name| {
        vars.iter()
            .find(|(set, _)| set == name)
            .map(|(_, value)| value.clone())
    });
    let mut screen = crate::screen::Screen::new(TestBackend::new(width, height), width, height)
        .expect("a screen");
    screen.set_look(look);
    screen
        .draw_with(app, None, crate::view::render)
        .expect("a draw");
    screen
}

/// A painted screen's text, one row per line.
fn painted_text(screen: &crate::screen::Screen<TestBackend>) -> String {
    let buf = screen.backend().buffer();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn an_accepted_import_with_120_lines_opens_one_report_and_adds_no_notices() {
    let mut app = attached();
    app.set_size(80, 24);
    let lines = sent(enter(&mut app, "/import codex"));
    let id = lines[0]["id"].clone();
    let accepted = session_line(
        "command_accepted",
        json!({"command_id": id, "result": big_result()}),
    );
    assert!(app.on_line(accepted).is_empty());
    // No notices: the report carries the whole answer. The report opens
    // scrolled to the top naming the source and its first section count;
    // the other count shows after scrolling down.
    assert!(
        app.notices().is_empty(),
        "an accepted import adds no notices"
    );
    let text = painted_text(&painted(&mut app));
    assert!(
        text.contains("Imported from Codex"),
        "the report names the source:\n{text}"
    );
    assert!(
        text.contains("Copied · 60"),
        "the copied section counts its items:\n{text}"
    );
    insta::assert_snapshot!("import_report_120_lines", text);
    // One tall paint shows the whole overlay with both counts.
    app.set_size(80, 140);
    let whole = painted_text(&painted_sized(&mut app, 80, 140));
    assert!(
        whole.contains("Left behind · 60"),
        "the left-behind section counts its items:\n{whole}"
    );
    assert!(
        whole.contains("left-01.json") && whole.contains("left-60.json"),
        "the whole report shows at once:\n{whole}"
    );
    // Back at 80x24, scrolled to the bottom the last row shows with
    // the footer.
    app.set_size(80, 24);
    for _ in 0..30 {
        assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    }
    let bottom = painted_text(&painted(&mut app));
    assert!(
        bottom.contains("left-60.json"),
        "the last left-behind row shows at the bottom:\n{bottom}"
    );
    assert!(
        bottom.contains("Imported items apply on the next /reload · Esc closes"),
        "the footer stays visible at the bottom:\n{bottom}"
    );
    // Esc closes the overlay and leaves no notices behind.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(
        app.notices().is_empty(),
        "closing the report leaves no notices"
    );
    let closed = painted_text(&painted(&mut app));
    assert!(
        !closed.contains("Imported from Codex"),
        "Esc closes the report:\n{closed}"
    );
}

/// The small report: 20 copied names and 3 left behind, 30 body rows,
/// 14 past the 16-row frame on an 80x24 screen.
fn small_result() -> Value {
    let names = |prefix: &str| -> Vec<Value> {
        (1..=5)
            .map(|n| Value::String(format!("{prefix}-{n:02}")))
            .collect()
    };
    json!({
        "imported": {
            "mcp_servers": names("server"),
            "skills": names("skill"),
            "commands": names("command"),
            "instructions": names("AGENTS.md"),
        },
        "left_behind": [
            {"item": "auth.json", "reason": "credentials are never imported"},
            {"item": "other.json", "reason": "unknown"},
            {"item": "third.json", "reason": "unknown"},
        ],
    })
}

/// The open report's scrolled-to first body row.
fn top_of(app: &App) -> usize {
    app.import_report().expect("open report").top()
}

/// Accepts `/import codex` with the small report on an attached app.
fn open_small_report(app: &mut App) {
    let lines = sent(enter(app, "/import codex"));
    let id = lines[0]["id"].clone();
    let accepted = session_line(
        "command_accepted",
        json!({"command_id": id, "result": small_result()}),
    );
    assert!(app.on_line(accepted).is_empty());
    assert!(app.import_report().is_some());
}

#[test]
fn scroll_keys_move_the_top_and_esc_closes_it() {
    let mut app = attached();
    app.set_size(80, 24);
    open_small_report(&mut app);
    assert_eq!(top_of(&app), 0);
    // Up at the top holds.
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(top_of(&app), 0);
    // Arrows move by rows, pages move further, and the bottom holds:
    // five pages reach the clamp however tall the conversation is.
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(top_of(&app), 1);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(top_of(&app), 2);
    assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    assert!(top_of(&app) > 3, "a page moves past one row");
    let held = top_of(&app);
    for _ in 0..5 {
        assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    }
    let bottom = top_of(&app);
    assert!(bottom >= held, "pages only move down");
    assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    assert_eq!(top_of(&app), bottom, "the bottom holds");
    // Pages back up reach the top exactly.
    for _ in 0..6 {
        assert_eq!(app.on_key(Key::PageUp, now()), Effect::None);
    }
    assert_eq!(top_of(&app), 0);
    // A character key sends nothing and changes nothing: the draft
    // keeps nothing typed into the report.
    assert_eq!(app.on_key(Key::Char('x'), now()), Effect::None);
    assert_eq!(top_of(&app), 0);
    assert_eq!(app.draft(), "");
    assert!(app.import_report().is_some());
    // Esc closes the report and leaves no notices behind.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.import_report().is_none());
    assert!(app.notices().is_empty());
}

#[test]
fn ctrl_c_passes_through_to_quit_while_the_report_is_open() {
    let mut app = attached();
    app.set_size(80, 24);
    open_small_report(&mut app);
    assert_eq!(app.on_key(Key::CtrlC, now()), Effect::None);
    assert!(app.import_report().is_some());
    assert_eq!(app.on_key(Key::CtrlC, now()), Effect::Quit);
    assert!(app.import_report().is_some());
}

#[test]
fn wheel_scrolls_the_open_report_over_a_session() {
    use crate::keys::{Mouse, MouseKind};
    let mut app = attached();
    app.set_size(80, 24);
    open_small_report(&mut app);
    let wheel = |down: bool| Mouse {
        kind: if down {
            MouseKind::WheelDown
        } else {
            MouseKind::WheelUp
        },
        col: 40,
        row: 12,
    };
    app.on_wheel(&wheel(true));
    assert_eq!(top_of(&app), 3);
    app.on_wheel(&wheel(true));
    assert_eq!(top_of(&app), 6);
    app.on_wheel(&wheel(false));
    assert_eq!(top_of(&app), 3);
}

/// An app on home at 80x24, with no sessions.
fn home_app() -> App {
    let mut app = App::new(std::path::PathBuf::from("/w"));
    app.set_home(crate::Launch {
        location: crate::LaunchLocation::Local {
            workspace: std::path::PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// One live `session_status` line naming `name`.
fn status_line(session: &str, name: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({
            "name": name, "workspace": "/w", "project": "p",
            "state": "idle", "since": 0, "started": 0, "calls": 0,
            "delegates": 0, "jobs": 0, "clients": 0,
            "model": "test/model",
            "spend": {"tokens": {"input": 0, "output": 0, "cache_read": 0, "cache_write": {}}, "cost": 0.0, "subscription_cost": 0.0},
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// The small report opened straight onto `app`, naming `source`.
fn open_direct(app: &mut App, source: Option<&str>) {
    use contract::events::{ImportedNames, LeftBehindEntry};
    let result = small_result();
    let imported: ImportedNames =
        serde_json::from_value(result["imported"].clone()).expect("small report parses");
    let left_behind: Vec<LeftBehindEntry> =
        serde_json::from_value(result["left_behind"].clone()).expect("small report parses");
    app.open_import_report(source.map(str::to_owned), imported, left_behind);
}

#[test]
fn home_keys_route_to_the_report_while_it_is_open() {
    let mut app = home_app();
    open_direct(&mut app, Some("codex"));
    assert_eq!(
        app.import_report().expect("open report").title(),
        "Imported from Codex"
    );
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(top_of(&app), 1);
    // Typing reaches neither the box nor the list: the report keeps
    // the key.
    assert_eq!(app.on_key(Key::Char('j'), now()), Effect::None);
    assert_eq!(top_of(&app), 1);
    assert_eq!(app.draft(), "");
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.import_report().is_none());
    assert!(app.notices().is_empty());
}

#[test]
fn wheel_routes_to_the_list_when_closed_and_the_report_when_open() {
    use crate::keys::{Mouse, MouseKind};
    let mut app = home_app();
    // A short screen, so the six sessions overflow the list's window.
    app.set_size(60, 12);
    for (at, name) in ["one", "two", "three", "four", "five", "six"]
        .iter()
        .enumerate()
    {
        let session = format!("s_000000000000000{at}");
        assert!(app.on_line(status_line(&session, name)).is_empty());
    }
    let wheel = |down: bool| Mouse {
        kind: if down {
            MouseKind::WheelDown
        } else {
            MouseKind::WheelUp
        },
        col: 10,
        row: 5,
    };
    // With the report closed the wheel scrolls the session list.
    app.on_wheel(&wheel(true));
    assert_eq!(app.home_list_scroll(), 3);
    // With the report open the wheel scrolls the report instead, and
    // the list holds where it was.
    open_direct(&mut app, Some("codex"));
    app.on_wheel(&wheel(true));
    assert_eq!(app.home_list_scroll(), 3);
    assert_eq!(app.import_report().expect("open report").top(), 3);
}

#[test]
fn the_report_close_click_closes_the_report() {
    use crate::mouse::TargetId;
    let mut app = attached();
    app.set_size(80, 24);
    open_small_report(&mut app);
    let screen = painted(&mut app);
    let close = screen
        .targets()
        .iter()
        .find(|target| target.id == TargetId::CloseOverlay)
        .expect("the report draws its ✕")
        .id;
    assert_eq!(app.on_click(close), Effect::None);
    assert!(app.import_report().is_none());
    assert!(app.notices().is_empty());
    let closed = painted_text(&painted(&mut app));
    assert!(!closed.contains("Copied · 20"), "{closed}");
}

#[test]
fn home_paints_the_report_with_both_counts_and_a_swallow_target() {
    use crate::home::Spot;
    use crate::mouse::TargetId;
    let mut app = home_app();
    open_direct(&mut app, Some("codex"));
    let screen = painted(&mut app);
    let shown = painted_text(&screen);
    assert!(shown.contains("Imported from Codex"), "{shown}");
    assert!(shown.contains("Copied · 20"), "{shown}");
    assert!(shown.contains("Imported items apply"), "{shown}");
    assert!(
        screen
            .targets()
            .iter()
            .any(|target| target.id == TargetId::Home(Spot::Question)),
        "clicks on the report stay with the report:\n{shown}"
    );
    assert!(app.notices().is_empty());
}
