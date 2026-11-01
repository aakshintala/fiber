//! Screen tests for nested ledger calls (#3753): an outer call with
//! inner calls over a fixture session log.

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;

use super::{render, text};
use crate::app::{App, Target};
use crate::link::Line;

const WIDTH: u16 = 60;
const HEIGHT: u16 = 24;

/// Renders `app` on a 60x24 screen as text, rows trimmed of trailing spaces.
fn screen(app: &mut App) -> String {
    app.set_size(WIDTH, HEIGHT);
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    text(&buf)
}

/// An empty app.
fn empty() -> App {
    App::new(PathBuf::from("/w"))
}

/// Attaches the app to `session`.
fn attach(app: &mut App, session: &str) {
    app.attach(contract::SessionId(session.to_owned()));
}

/// One session envelope.
fn session_line(
    session: &str,
    kind: &str,
    payload: serde_json::Value,
    action: Option<&str>,
) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A `turn_started` envelope with one message.
fn turn_started(session: &str, text: &str) -> Line {
    session_line(
        session,
        "turn_started",
        json!({"input": [{
            "type": "message",
            "source": "driver",
            "content": [{"type": "text", "text": text}],
        }]}),
        None,
    )
}

/// A completed turn.
fn turn_completed(session: &str) -> Line {
    session_line(
        session,
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    )
}

/// The group target.
fn group_target(app: &App) -> Target {
    app.targets()
        .into_iter()
        .find_map(|(_, target)| matches!(target, Target::Group(_)).then_some(target))
        .expect("a group target")
}

/// The call target for `action`.
fn call_target(app: &App, action: &str) -> Target {
    let id = crate::turn::target_id(action);
    app.targets()
        .into_iter()
        .find_map(|(_, target)| {
            matches!(target, Target::Call(call) if call == id).then_some(target)
        })
        .expect("the call target")
}

/// A turn with a `codemode` outer call and three inner calls: a read,
/// an edit and a shell command.
fn nested_turn(app: &mut App) {
    let session = "s_aaaaaaaaaaaaaaaa";
    attach(app, session);
    app.on_line(turn_started(session, "run the checks"));
    app.on_line(session_line(session, "step_started", json!({}), None));
    app.on_line(session_line(
        session,
        "tool_call_requested",
        json!({"name": "codemode",
            "arguments": {"source": "print('hi')\nprint('bye')"}}),
        Some("a_1"),
    ));
    for (action, name, arguments) in [
        ("a_2", "read", json!({"path": "src/f.rs"})),
        ("a_3", "edit", json!({"path": "src/f.rs"})),
        ("a_4", "shell", json!({"command": "cargo test"})),
    ] {
        app.on_line(session_line(
            session,
            "tool_call_requested",
            json!({"name": name, "arguments": arguments,
                "ran_by": {"extension": "ext", "outer_action_id": "a_1"}}),
            Some(action),
        ));
    }
    app.on_line(session_line(
        session,
        "tool_call_completed",
        json!({"status": "completed",
            "content": [{"type": "text", "text": "fn f() {}"}]}),
        Some("a_2"),
    ));
    app.on_line(session_line(
        session,
        "tool_call_completed",
        json!({"status": "completed",
            "content": [{"type": "text", "text": "Edited."}],
            "details": {"diff": "-print('old')\n+print('new')"},
            "changes": [{"path": "src/f.rs", "added": 3, "removed": 1}]}),
        Some("a_3"),
    ));
    app.on_line(session_line(
        session,
        "tool_call_completed",
        json!({"status": "completed",
            "content": [{"type": "text", "text": "ok"}]}),
        Some("a_4"),
    ));
    app.on_line(session_line(
        session,
        "tool_call_completed",
        json!({"status": "completed",
            "content": [{"type": "text", "text": "done"}]}),
        Some("a_1"),
    ));
    app.on_line(session_line(
        session,
        "text_completed",
        json!({"text": "Done."}),
        Some("a_m"),
    ));
    app.on_line(turn_completed(session));
}

#[test]
fn tool_group_nesting_summary() {
    let mut app = empty();
    nested_turn(&mut app);
    let shown = screen(&mut app);
    // The summary reads what the inner calls did; the script's own
    // call never counts as a kind of its own (`docs/tui.md:1136-1141`).
    assert!(
        shown.contains("Read 1 file, edited 1 file +3 −1, ran 1 command"),
        "{shown}"
    );
    assert!(!shown.contains("called"), "{shown}");
}

#[test]
fn tool_group_nesting_open_ledger() {
    let mut app = empty();
    nested_turn(&mut app);
    app.open(group_target(&app));
    app.open(call_target(&app, "a_1"));
    let shown = screen(&mut app);
    // One row for the outer, then its script source and one indented
    // row per inner call (`docs/tui.md:1130-1132`, `docs/tui.md:1142-1144`).
    assert!(shown.contains("✓ other"), "{shown}");
    assert!(shown.contains("print('hi')"), "{shown}");
    assert!(shown.contains("print('bye')"), "{shown}");
    assert!(shown.contains("        ✓ read"), "{shown}");
    assert!(shown.contains("        ✓ edit"), "{shown}");
    assert!(shown.contains("        ✓ shell"), "{shown}");
    assert!(shown.contains("src/f.rs"), "{shown}");
    assert!(shown.contains("+3 −1"), "{shown}");
    assert!(shown.contains("cargo test"), "{shown}");
}

#[test]
fn tool_group_nesting_open_edit() {
    let mut app = empty();
    nested_turn(&mut app);
    app.open(group_target(&app));
    app.open(call_target(&app, "a_1"));
    app.open(call_target(&app, "a_3"));
    let shown = screen(&mut app);
    // The edit row opens to its diff, as any call does
    // (`docs/tui.md:1145-1146`).
    assert!(shown.contains("print('old')"), "{shown}");
    assert!(shown.contains("print('new')"), "{shown}");
}

/// A turn with one inner and one orphan: the orphan names an outer
/// that is no call of the turn.
fn orphan_turn(app: &mut App) {
    let session = "s_aaaaaaaaaaaaaaaa";
    attach(app, session);
    app.on_line(turn_started(session, "check"));
    app.on_line(session_line(session, "step_started", json!({}), None));
    app.on_line(session_line(
        session,
        "tool_call_requested",
        json!({"name": "codemode", "arguments": {"source": "s"}}),
        Some("a_1"),
    ));
    app.on_line(session_line(
        session,
        "tool_call_requested",
        json!({"name": "read", "arguments": {"path": "in.rs"},
            "ran_by": {"extension": "ext", "outer_action_id": "a_1"}}),
        Some("a_2"),
    ));
    app.on_line(session_line(
        session,
        "tool_call_requested",
        json!({"name": "read", "arguments": {"path": "orf.rs"},
            "ran_by": {"extension": "ext", "outer_action_id": "a_9"}}),
        Some("a_3"),
    ));
    for (action, text) in [("a_2", "in"), ("a_3", "orf"), ("a_1", "done")] {
        app.on_line(session_line(
            session,
            "tool_call_completed",
            json!({"status": "completed",
                "content": [{"type": "text", "text": text}]}),
            Some(action),
        ));
    }
    app.on_line(session_line(
        session,
        "text_completed",
        json!({"text": "Done."}),
        Some("a_m"),
    ));
    app.on_line(turn_completed(session));
}

#[test]
fn tool_group_nesting_orphan_top_level() {
    let mut app = empty();
    orphan_turn(&mut app);
    app.open(group_target(&app));
    let shown = screen(&mut app);
    // The closed outer is one row with its inners hidden; the orphan
    // draws top-level, without the inner indent (`docs/tui.md:1130-1135`;
    // orphan rule `docs/tui.md:1134-1135`).
    assert!(shown.contains("✓ other"), "{shown}");
    assert!(!shown.contains("in.rs"), "{shown}");
    let orphan = shown
        .lines()
        .find(|line| line.contains("orf.rs"))
        .expect("the orphan row");
    assert!(!orphan.starts_with("        "), "{orphan}");
    assert!(orphan.contains('✓'), "{orphan}");
}
