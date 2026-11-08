//! Screen tests for a ledger row over a call a `before_tool` hook
//! rewrote (`docs/tui.md`, "Tool groups and the ledger"): the row draws
//! the arguments that ran and names the extensions that rewrote them.

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::{render, text};
use crate::app::{App, Target};
use crate::link::Line;
use contract::events::EventKind;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const WIDTH: u16 = 100;
const HEIGHT: u16 = 24;

/// Renders `app` on a 100x24 screen as text, rows trimmed of trailing spaces.
fn screen(app: &mut App) -> String {
    app.set_size(WIDTH, HEIGHT);
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    text(&buf)
}

/// One session envelope.
fn line(kind: &str, payload: Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: EventKind::from_name(kind),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// What a reviewed call's decision and a started line both carry when a
/// hook rewrote the call.
fn rewrite(changed_by: &[&str]) -> Value {
    json!({"effects": ["executes"], "reversible": false,
        "arguments": {"command": "cargo test --quiet"}, "changed_by": changed_by})
}

/// A turn holding one `shell` call, requested as `cargo test`. `between`
/// are the lines after the request and before the call completes, and
/// `status` is how it ends. The ledger is open.
fn turn(between: Vec<Line>, status: &str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "run the tests"}]}]}),
        None,
    ));
    app.on_line(line("step_started", json!({}), None));
    app.on_line(line(
        "tool_call_requested",
        json!({"name": "shell", "arguments": {"command": "cargo test"}}),
        Some("a_1"),
    ));
    for between in between {
        app.on_line(between);
    }
    let done = if status == "denied" {
        json!({"status": "denied", "reason": "no", "content": []})
    } else {
        json!({"status": "completed", "content": [{"type": "text", "text": "ok"}],
            "process": {"exit_code": 0, "timed_out": false}})
    };
    app.on_line(line("tool_call_completed", done, Some("a_1")));
    app.on_line(line(
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
    let group = app
        .targets()
        .into_iter()
        .find_map(|(_, target)| matches!(target, Target::Group(_)).then_some(target))
        .expect("a group target");
    app.open(group);
    app
}

/// A started line, rewritten by `changed_by`.
fn started(changed_by: &[&str]) -> Line {
    line("tool_call_started", rewrite(changed_by), Some("a_1"))
}

/// The screen rows holding `needle`.
fn rows_with(shown: &str, needle: &str) -> Vec<String> {
    shown
        .lines()
        .filter(|row| row.contains(needle))
        .map(|row| row.trim_end().to_owned())
        .collect()
}

/// The one ledger row of the shell call.
#[track_caller]
fn row(shown: &str) -> String {
    let rows = rows_with(shown, "shell ");
    assert_eq!(rows.len(), 1, "one shell row in {shown}");
    rows[0].clone()
}

#[test]
fn a_hook_rewrite_shows_the_arguments_that_ran() {
    let mut app = turn(vec![started(&["guard"])], "completed");
    let shown = screen(&mut app);
    let row = row(&shown);
    assert!(row.contains("cargo test --quiet"), "{shown}");
    assert!(!row.contains("cargo test  "), "{shown}");
}

#[test]
fn a_hook_rewrite_ledger_matches_its_snapshot() {
    let mut app = turn(vec![started(&["guard"])], "completed");
    let shown = screen(&mut app);
    let row = row(&shown);
    assert!(row.contains("cargo test --quiet"), "{shown}");
    assert!(row.ends_with("\u{b7} via guard"), "{shown}");
    insta::assert_snapshot!("hook_rewrite_ledger_100x24", shown);
}

#[test]
fn a_hook_rewrite_names_who_rewrote_it() {
    let mut app = turn(vec![started(&["guard"])], "completed");
    assert!(row(&screen(&mut app)).ends_with("\u{b7} via guard"));
    let mut app = turn(vec![started(&["guard", "audit"])], "completed");
    assert!(row(&screen(&mut app)).ends_with("\u{b7} via guard, audit"));
}

#[test]
fn a_reviewer_denied_hook_rewrite_shows_what_was_judged() {
    let mut judged = rewrite(&["guard"]);
    if let Some(object) = judged.as_object_mut() {
        object.extend(
            json!({"decision": "deny", "decided_by": "reviewer", "reason": "no",
                "reviewer": {"model": "m", "stage": 1}})
            .as_object()
            .cloned()
            .unwrap_or_default(),
        );
    }
    let resolved = line("permission_resolved", judged, Some("a_1"));
    let mut app = turn(vec![resolved], "denied");
    let shown = screen(&mut app);
    let row = row(&shown);
    assert!(row.contains("denied"), "{shown}");
    assert!(row.contains("cargo test --quiet"), "{shown}");
    assert!(row.ends_with("\u{b7} via guard"), "{shown}");
}

#[test]
fn a_reviewer_allowed_hook_rewrite_is_drawn_once() {
    let mut judged = rewrite(&["guard"]);
    if let Some(object) = judged.as_object_mut() {
        object.extend(
            json!({"decision": "allow", "decided_by": "reviewer",
                "reviewer": {"model": "m", "stage": 1}})
            .as_object()
            .cloned()
            .unwrap_or_default(),
        );
    }
    let resolved = line("permission_resolved", judged, Some("a_1"));
    let mut app = turn(vec![resolved, started(&["guard"])], "completed");
    let shown = screen(&mut app);
    let rows = rows_with(&shown, "cargo test --quiet");
    assert_eq!(rows.len(), 1, "{shown}");
    assert!(rows[0].ends_with("\u{b7} via guard"), "{shown}");
}

#[test]
fn no_hook_rewrite_draws_no_mark() {
    let plain = line(
        "tool_call_started",
        json!({"effects": [], "reversible": true}),
        Some("a_1"),
    );
    let mut app = turn(vec![plain], "completed");
    let shown = screen(&mut app);
    let row = row(&shown);
    assert!(row.contains("cargo test"), "{shown}");
    assert!(!row.contains("--quiet"), "{shown}");
    assert!(!row.contains("via"), "{shown}");
}
