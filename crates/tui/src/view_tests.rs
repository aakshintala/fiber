//! Screen snapshots: the whole in-memory screen against stored files.

use super::{cursor, render, text};
use crate::app::App;
use crate::keys::{Edit, Key};
use crate::link::Line;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use std::path::PathBuf;

const WIDTH: u16 = 60;
const HEIGHT: u16 = 12;

/// Renders `app` on a 60x12 screen as text, rows trimmed of trailing spaces.
fn screen(app: &App) -> String {
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    text(&buf)
}

/// An empty app at 60x12.
fn empty() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(WIDTH, HEIGHT);
    app
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
        serde_json::json!({"input": [{
            "type": "message",
            "source": "driver",
            "content": [{"type": "text", "text": text}],
        }]}),
        None,
    )
}

/// A `turn_completed` envelope.
fn turn_completed(session: &str, outcome: &str) -> Line {
    let payload = if outcome == "failed" {
        serde_json::json!({"outcome": outcome, "error": {"code": "io_failed", "message": "boom"}})
    } else {
        serde_json::json!({"outcome": outcome})
    };
    session_line(session, "turn_completed", payload, None)
}

/// A delta for one action.
fn delta(session: &str, action: &str, text: &str) -> Line {
    session_line(
        session,
        "assistant_message_delta",
        serde_json::json!({"text": text}),
        Some(action),
    )
}

/// A steering message.
fn steered(session: &str, text: &str) -> Line {
    session_line(
        session,
        "steering_applied",
        serde_json::json!({"content": [{"type": "text", "text": text}], "source": "driver"}),
        None,
    )
}

#[test]
fn first_frame() {
    insta::assert_snapshot!("first_frame", screen(&empty()));
}

#[test]
fn streaming_reply() {
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    app.on_line(delta("s_aaaaaaaaaaaaaaaa", "a_1", "Hel"));
    insta::assert_snapshot!("streaming_reply", screen(&app));
}

#[test]
fn completed_turn_with_steer() {
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    app.on_line(steered("s_aaaaaaaaaaaaaaaa", "use x"));
    app.on_line(delta("s_aaaaaaaaaaaaaaaa", "a_1", "Hello."));
    app.on_line(turn_completed("s_aaaaaaaaaaaaaaaa", "completed"));
    insta::assert_snapshot!("completed_turn_with_steer", screen(&app));
}

#[test]
fn failed_turn() {
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    app.on_line(turn_completed("s_aaaaaaaaaaaaaaaa", "failed"));
    insta::assert_snapshot!("failed_turn", screen(&app));
}

#[test]
fn scrolled_up_with_overlay() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    for n in 1..=14 {
        app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", &format!("prompt {n}")));
        app.on_line(turn_completed("s_aaaaaaaaaaaaaaaa", "completed"));
    }
    app.on_key(Key::PageUp, now);
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "one more"));
    insta::assert_snapshot!("scrolled_up_with_overlay", screen(&app));
}

#[test]
fn quit_hint() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = empty();
    app.on_key(Key::CtrlC, clock.now());
    insta::assert_snapshot!("quit_hint", screen(&app));
}

#[test]
fn notice() {
    let mut app = empty();
    app.connect_failed("Could not reach the hub: refused".to_owned());
    insta::assert_snapshot!("notice", screen(&app));
}

#[test]
fn long_line_wrapped() {
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    let long: String = std::iter::repeat_n('w', 150).collect();
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", &long));
    insta::assert_snapshot!("long_line_wrapped", screen(&app));
}

#[test]
fn scrolled_up_the_view_stays_put_and_end_follows_again() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    for n in 1..=14 {
        app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", &format!("prompt {n}")));
    }
    app.on_key(Key::PageUp, now);
    let before = screen(&app);
    assert!(!before.contains("↓ New messages below"));
    app.on_line(delta("s_aaaaaaaaaaaaaaaa", "a_1", "streamed"));
    let after = screen(&app);
    // The rows above the overlay are the rows that were there.
    let kept: Vec<&str> = before.lines().take(9).collect();
    assert_eq!(after.lines().take(9).collect::<Vec<_>>(), kept);
    assert!(after.contains("↓ New messages below"));
    app.on_key(Key::End, now);
    let followed = screen(&app);
    assert!(!followed.contains("↓ New messages below"));
    assert!(followed.contains("streamed"));
}

#[test]
fn page_down_to_the_bottom_follows_again() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    for n in 1..=30 {
        app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", &format!("prompt {n}")));
    }
    let bottom = screen(&app);
    // The bottom shows prompts 20 to 30. A page is the conversation's 11
    // rows less one, and the top stops at the first row.
    app.on_key(Key::PageUp, now);
    assert!(screen(&app).starts_with(format!("{:>59}\n", "prompt 10").as_str()));
    // One page down lands exactly on the bottom, which follows again: new
    // output scrolls in with no overlay.
    app.on_key(Key::PageDown, now);
    assert_eq!(screen(&app), bottom);
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "prompt 31"));
    let followed = screen(&app);
    assert!(!followed.contains("↓ New messages below"));
    assert!(followed.contains(format!("{:>59}\n>", "prompt 31").as_str()));
    app.on_key(Key::PageUp, now);
    assert!(screen(&app).starts_with(format!("{:>59}\n", "prompt 11").as_str()));
    app.on_key(Key::PageUp, now);
    assert!(screen(&app).starts_with(format!("{:>59}\n", "prompt 1").as_str()));
    app.on_key(Key::PageDown, now);
    assert!(screen(&app).starts_with(format!("{:>59}\n", "prompt 11").as_str()));
    app.on_key(Key::PageDown, now);
    assert!(screen(&app).contains(format!("{:>59}\n>", "prompt 31").as_str()));
    // PageDown while following does nothing.
    app.on_key(Key::PageDown, now);
    assert!(screen(&app).contains(format!("{:>59}\n>", "prompt 31").as_str()));
}

#[test]
fn a_long_draft_shows_its_end() {
    let mut app = empty();
    for ch in std::iter::repeat_n('a', 70).chain("end".chars()) {
        app.on_key(Key::Char(ch), fakes::clock::FakeClock::new().now());
    }
    // The draft wraps: its start on the row above, its end on the last.
    let shown = screen(&app);
    let rows: Vec<&str> = shown.lines().collect();
    assert_eq!(rows.get(10).map(|row| row.chars().count()), Some(60));
    assert!(rows.get(10).is_some_and(|row| row.starts_with("> a")));
    assert_eq!(rows.last().copied(), Some("  aaaaaaaaaaaaend"));
}

#[test]
fn draw_folds_an_events_file() {
    let events = [
        serde_json::json!({"kind": "turn_started", "session_id": "s_aaaaaaaaaaaaaaaa", "ts": 0,
            "schema_version": contract::SCHEMA_VERSION, "seq": 1,
            "payload": {"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "hi"}]}]}}),
        serde_json::json!({"kind": "assistant_message_delta", "session_id": "s_aaaaaaaaaaaaaaaa",
            "ts": 0, "schema_version": contract::SCHEMA_VERSION, "action_id": "a_1",
            "payload": {"text": "Hello."}}),
    ]
    .map(|line| line.to_string())
    .join("\n");
    let shown = crate::draw(&events, 20, 4).unwrap_or_else(|error| panic!("draw: {error}"));
    assert_eq!(shown, format!("\n{:>19}\nHello.\n>\n", "hi"));
    assert_eq!(
        crate::draw("not json", 20, 4).map_err(|e| e.starts_with("line 1:")),
        Err(true)
    );
}

/// Renders `app` at `width` by `height` as text.
fn sized(app: &mut App, width: u16, height: u16) -> String {
    app.set_size(width, height);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    text(&buf)
}

#[test]
fn a_short_screen_keeps_the_input_line_last() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "one"));
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "two"));
    app.connect_failed("lost".to_owned());
    app.on_key(Key::CtrlC, now);
    // The input line wins the last row, then the hint, then the notice;
    // the conversation gets what is left.
    assert_eq!(sized(&mut app, 20, 1), ">\n");
    assert_eq!(sized(&mut app, 20, 2), "Press Ctrl+C again t\n>\n");
    assert_eq!(sized(&mut app, 20, 3), "lost\nPress Ctrl+C again t\n>\n");
    assert_eq!(
        sized(&mut app, 20, 4),
        format!("{:>19}\nlost\nPress Ctrl+C again t\n>\n", "two")
    );
}

#[test]
fn the_overlay_never_covers_the_input_line() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.set_size(30, 1);
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "one"));
    app.on_key(Key::PageUp, now);
    app.on_line(delta("s_aaaaaaaaaaaaaaaa", "a_1", "streamed"));
    assert!(app.has_new());
    assert_eq!(sized(&mut app, 30, 1), ">\n");
    assert_eq!(sized(&mut app, 30, 2), "     ↓ New messages below\n>\n");
}

const S_A: &str = "s_aaaaaaaaaaaaaaaa";
const S_B: &str = "s_bbbbbbbbbbbbbbbb";

/// Renders `app` on an 80x12 screen, returning the text and the buffer.
fn wide(app: &mut App) -> (String, Buffer) {
    app.set_size(80, HEIGHT);
    let area = Rect::new(0, 0, 80, HEIGHT);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    (text(&buf), buf)
}

/// A connected app attached to `S_A`, with a conversation line.
fn asked() -> App {
    let mut app = empty();
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    attach(&mut app, S_A);
    app.on_line(turn_started(S_A, "run it"));
    app
}

/// A `permission_requested` with `keys` beside its request id.
fn request(session: &str, action: &str, request: &str, keys: serde_json::Value) -> Line {
    let mut payload = serde_json::json!({"request_id": request, "effects": ["executes"]});
    if let (Some(into), Some(from)) = (payload.as_object_mut(), keys.as_object()) {
        into.extend(from.clone());
    }
    session_line(session, "permission_requested", payload, Some(action))
}

/// A standing ask on `echo hi`.
fn standing(session: &str, action: &str, id: &str) -> Line {
    request(
        session,
        action,
        id,
        serde_json::json!({"reversible": true, "step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": "echo hi"}}),
    )
}

/// A shell call.
fn shell(session: &str, action: &str, command: &str) -> Line {
    session_line(
        session,
        "tool_call_requested",
        serde_json::json!({"name": "shell", "arguments": {"command": command}}),
        Some(action),
    )
}

/// The background of the cell at column 0 of `row`.
fn bg(buf: &Buffer, row: u16) -> Option<ratatui::style::Color> {
    buf.cell((0, row)).map(|cell| cell.bg)
}

#[test]
fn approval_standing_ask() {
    let mut app = asked();
    app.on_line(shell(S_A, "a_1", "echo hi"));
    app.on_line(standing(S_A, "a_1", "r_1"));
    let (shown, buf) = wide(&mut app);
    insta::assert_snapshot!("approval_standing_ask", shown);
    // Header to deny, five rows, take the approval tint; the row above
    // does not.
    for row in 7..12 {
        assert_eq!(bg(&buf, row), super::APPROVAL_TINT.bg, "row {row}");
    }
    assert_eq!(bg(&buf, 6), Some(ratatui::style::Color::Reset));
}

#[test]
fn approval_review_escalation_with_rule() {
    let mut app = asked();
    app.on_line(shell(S_A, "a_1", "npm test --watch"));
    app.on_line(request(
        S_A,
        "a_1",
        "r_1",
        serde_json::json!({"reversible": true, "step": "review",
            "escalation": {"cause": "consecutive_blocks", "reason": "it blocked three in a row"},
            "rule": {"subject": "npm test --watch", "prefix": "npm test"}}),
    ));
    let (shown, buf) = wide(&mut app);
    insta::assert_snapshot!("approval_review_escalation_with_rule", shown);
    assert_eq!(bg(&buf, 11), super::ALERT_TINT.bg);
    assert_ne!(super::ALERT_TINT.bg, super::APPROVAL_TINT.bg);
}

#[test]
fn approval_reviewer_failed() {
    let mut app = asked();
    app.on_line(request(
        S_A,
        "a_1",
        "r_1",
        serde_json::json!({"reversible": true, "step": "review",
            "escalation": {"cause": "reviewer_failed",
                "error": {"code": "io_failed", "message": "the reviewer model did not answer"}}}),
    ));
    insta::assert_snapshot!("approval_reviewer_failed", wide(&mut app).0);
}

#[test]
fn approval_irreversible_header() {
    let mut app = asked();
    app.on_line(request(
        S_A,
        "a_1",
        "r_1",
        serde_json::json!({"reversible": false, "step": "standing_ask",
            "standing_rule": {"scope": "project", "prefix": "git push"}}),
    ));
    insta::assert_snapshot!("approval_irreversible_header", wide(&mut app).0);
}

#[test]
fn approval_two_of_three_across_sessions() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = asked();
    app.on_line(standing(S_A, "a_1", "r_1"));
    app.on_line(shell(S_B, "a_1", "echo hi"));
    app.on_line(standing(S_B, "a_1", "r_1"));
    app.on_line(standing(S_A, "a_2", "r_2"));
    app.on_key(Key::AltA, now);
    insta::assert_snapshot!("approval_two_of_three_across_sessions", wide(&mut app).0);
}

#[test]
fn approval_badge_with_the_panel_closed() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = asked();
    app.on_line(standing(S_A, "a_1", "r_1"));
    app.on_line(standing(S_A, "a_2", "r_2"));
    app.on_key(Key::Esc, now);
    app.on_key(Key::Esc, now);
    for ch in "draft".chars() {
        app.on_key(Key::Char(ch), now);
    }
    insta::assert_snapshot!("approval_badge_with_the_panel_closed", wide(&mut app).0);
}

#[test]
fn approval_feedback_typed_on_deny() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = asked();
    app.on_line(shell(S_A, "a_1", "echo hi"));
    app.on_line(standing(S_A, "a_1", "r_1"));
    for ch in "use printf".chars() {
        app.on_key(Key::Char(ch), now);
    }
    insta::assert_snapshot!("approval_feedback_typed_on_deny", wide(&mut app).0);
}

#[test]
fn a_short_screen_drops_the_badge_after_the_hint_and_notice() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = asked();
    app.on_line(standing(S_A, "a_1", "r_1"));
    app.on_key(Key::Esc, now);
    app.connect_failed("lost".to_owned());
    app.on_key(Key::CtrlC, now);
    let badge = "! 1 waiting · /approvals or ⌥A";
    assert_eq!(sized(&mut app, 40, 1), ">\n");
    assert_eq!(sized(&mut app, 40, 2), format!("{badge}\n>\n"));
    assert_eq!(
        sized(&mut app, 40, 3),
        format!("Press Ctrl+C again to quit\n{badge}\n>\n")
    );
    assert_eq!(
        sized(&mut app, 40, 4),
        format!("lost\nPress Ctrl+C again to quit\n{badge}\n>\n")
    );
}

#[test]
fn a_panel_taller_than_the_screen_keeps_its_header() {
    let mut app = asked();
    app.on_line(standing(S_A, "a_1", "r_1"));
    assert_eq!(
        sized(&mut app, 40, 2),
        format!("approval · {S_A} · 1 of 1\nasked by a global rule: echo hi\n")
    );
}

/// Types `text` into the draft, `\n` as Shift+Enter.
fn type_draft(app: &mut App, text: &str) {
    let now = fakes::clock::FakeClock::new().now();
    for ch in text.chars() {
        if ch == '\n' {
            app.on_edit(Edit::ShiftEnter);
        } else {
            app.on_key(Key::Char(ch), now);
        }
    }
}

/// Where the cursor shows on the 60x12 screen.
fn cursor_at(app: &App) -> Option<Position> {
    cursor(app, Rect::new(0, 0, WIDTH, HEIGHT))
}

#[test]
fn a_draft_of_three_lines() {
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    type_draft(&mut app, "first\nsecond\nthird");
    insta::assert_snapshot!("draft_of_three_lines", screen(&app));
    assert_eq!(cursor_at(&app), Some(Position::new(7, 11)));
}

#[test]
fn a_draft_taller_than_a_third_scrolls_with_the_cursor() {
    let mut app = empty();
    type_draft(&mut app, "l1\nl2\nl3\nl4\nl5\nl6");
    // 12 rows: the box shows 4, the last four while the cursor is there.
    insta::assert_snapshot!("draft_taller_than_the_cap", screen(&app));
    assert_eq!(cursor_at(&app), Some(Position::new(4, 11)));
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..5 {
        app.on_key(Key::Up, now);
    }
    // On the first line, the box shows the first four.
    insta::assert_snapshot!("draft_scrolled_to_its_top", screen(&app));
    assert_eq!(cursor_at(&app), Some(Position::new(4, 8)));
}

#[test]
fn a_paste_token_in_the_draft() {
    let mut app = empty();
    type_draft(&mut app, "see ");
    let pasted: Vec<String> = (1..=312).map(|n| format!("line {n}")).collect();
    app.on_edit(Edit::Paste(pasted.join("\n")));
    insta::assert_snapshot!("draft_with_a_paste_token", screen(&app));
    assert_eq!(cursor_at(&app), Some(Position::new(34, 11)));
}

#[test]
fn a_draft_wider_than_the_screen_wraps() {
    let mut app = empty();
    let long: String = std::iter::repeat_n('w', 70).collect();
    type_draft(&mut app, &long);
    let shown = screen(&app);
    let rows: Vec<&str> = shown.lines().collect();
    assert_eq!(
        rows.get(10).copied(),
        Some(format!("> {}", "w".repeat(58)).as_str())
    );
    assert_eq!(
        rows.get(11).copied(),
        Some(format!("  {}", "w".repeat(12)).as_str())
    );
    assert_eq!(cursor_at(&app), Some(Position::new(14, 11)));
}

#[test]
fn the_cursor_hides_while_the_panel_is_open() {
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(cursor_at(&app), Some(Position::new(2, 11)));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "permission_requested",
        serde_json::json!({"request_id": "r_1", "effects": ["executes"],
            "reversible": true, "step": "review"}),
        Some("a_1"),
    ));
    assert!(app.panel().is_some());
    assert_eq!(cursor_at(&app), None);
}

#[test]
fn the_cursor_stays_on_a_screen_one_column_wide() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(1, 3);
    type_draft(&mut app, "ab");
    let area = Rect::new(0, 0, 1, 3);
    assert_eq!(cursor(&app, area).map(|at| at.x), Some(0));
}

/// One session envelope at `ts` milliseconds.
fn at(kind: &str, action: Option<&str>, ts: u64, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A turn with a thought, a read and an edit in one step and a failed
/// command in the next, a reply, and its usage, closed after 38 seconds.
fn tool_turn(app: &mut App) {
    use serde_json::json;
    attach(app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "fix the failing test"));
    app.on_line(at("step_started", None, 0, json!({})));
    app.on_line(at("reasoning_started", Some("a_t"), 0, json!({})));
    app.on_line(at(
        "reasoning_completed",
        Some("a_t"),
        4_000,
        json!({"text": "## Find the test\nIt is in a.rs."}),
    ));
    for (action, name, arguments, ts) in [
        ("a_1", "read", json!({"path": "src/a.rs"}), 5_000),
        ("a_2", "edit", json!({"path": "src/a.rs"}), 6_000),
        ("a_3", "shell", json!({"command": "cargo test"}), 9_000),
    ] {
        if action == "a_3" {
            app.on_line(at("step_started", None, ts, json!({})));
        }
        app.on_line(at(
            "tool_call_requested",
            Some(action),
            ts,
            json!({"name": name, "arguments": arguments}),
        ));
    }
    app.on_line(at(
        "tool_call_completed",
        Some("a_1"),
        9_000,
        json!({"status": "completed", "content": [{"type": "text", "text": "fn a() {}"}]}),
    ));
    app.on_line(at(
        "tool_call_completed",
        Some("a_2"),
        9_000,
        json!({"status": "completed", "content": [{"type": "text", "text": "Edited."}],
            "details": {"diff": "-assert!(false)\n+assert!(true)"},
            "changes": [{"path": "src/a.rs", "added": 3, "removed": 1}]}),
    ));
    app.on_line(at(
        "tool_call_completed",
        Some("a_3"),
        12_000,
        json!({"status": "failed", "content": [],
            "error": {"code": "nonzero_exit", "message": "exit status 101"}}),
    ));
    app.on_line(at(
        "text_completed",
        Some("a_m"),
        12_000,
        json!({"text": "Fixed."}),
    ));
    for (id, cost, subscription) in [("g1", 0.41, false), ("g2", 1.1, true)] {
        let mut usage = json!({"generation_id": id, "model": "fake/m",
            "tokens": {"input": 9_100, "cache_read": 0, "cache_write": {}, "output": 0},
            "cost": cost});
        if subscription && let Some(usage) = usage.as_object_mut() {
            usage.insert("subscription".to_owned(), json!(true));
        }
        app.on_line(at("usage_recorded", Some("a_m"), 12_000, usage));
    }
    app.on_line(at(
        "turn_completed",
        None,
        38_000,
        json!({"outcome": "completed"}),
    ));
}

#[test]
fn tool_group_collapsed() {
    let mut app = empty();
    tool_turn(&mut app);
    insta::assert_snapshot!("tool_group_collapsed", screen(&app));
}

#[test]
fn tool_group_ledger_open_with_a_call_open() {
    let mut app = empty();
    tool_turn(&mut app);
    app.on_key(Key::CtrlO, fakes::clock::FakeClock::new().now());
    let edit = app
        .targets()
        .into_iter()
        .filter_map(|(_, target)| match target {
            crate::app::Target::Call(_) => Some(target),
            crate::app::Target::Group(_) | crate::app::Target::Thought(_) => None,
        })
        .nth(1);
    if let Some(edit) = edit {
        app.open(edit);
    }
    insta::assert_snapshot!(
        "tool_group_ledger_open_with_a_call_open",
        sized(&mut app, 60, 16)
    );
}

#[test]
fn tool_group_running_with_raw_arguments() {
    use serde_json::json;
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "look around"));
    app.on_line(at("step_started", None, 0, json!({})));
    app.on_line(at(
        "tool_call_requested",
        Some("a_1"),
        0,
        json!({"name": "read", "arguments": {"path": "README.md"}}),
    ));
    app.on_line(at(
        "tool_call_arguments_delta",
        Some("a_m"),
        0,
        json!({"index": 1, "name": "shell", "text": "{\"command\": \"cargo"}),
    ));
    app.on_line(at("reasoning_started", Some("a_t"), 0, json!({})));
    app.on_line(at(
        "reasoning_delta",
        Some("a_t"),
        0,
        json!({"text": "**Check the build**"}),
    ));
    insta::assert_snapshot!("tool_group_running_with_raw_arguments", screen(&app));
}

/// A completed turn that thought, then replied, with token-only usage.
fn thought_turn(app: &mut App) {
    use serde_json::json;
    attach(app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "why"));
    app.on_line(at("reasoning_started", Some("a_t"), 0, json!({})));
    app.on_line(at(
        "reasoning_completed",
        Some("a_t"),
        22_000,
        json!({"text": "# Plan the fix\nRead a.rs first."}),
    ));
    app.on_line(at(
        "text_completed",
        Some("a_m"),
        23_000,
        json!({"text": "Because."}),
    ));
    app.on_line(at(
        "usage_recorded",
        Some("a_m"),
        23_000,
        json!({"generation_id": "g1", "model": "fake/m",
            "tokens": {"input": 300, "cache_read": 0, "cache_write": {}, "output": 40},
            "cost": null}),
    ));
    app.on_line(at(
        "turn_completed",
        None,
        24_000,
        json!({"outcome": "completed"}),
    ));
}

#[test]
fn thought_line_opened_and_tokens_only() {
    let mut app = empty();
    thought_turn(&mut app);
    let before = screen(&app);
    if let Some((_, thought)) = app.targets().into_iter().next() {
        app.open(thought);
    }
    insta::assert_snapshot!("thought_line", before);
    insta::assert_snapshot!("thought_line_opened", screen(&app));
}

#[test]
fn styles_reach_the_screen() {
    use ratatui::style::Modifier;
    let mut app = empty();
    tool_turn(&mut app);
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let rows: Vec<String> = text(&buf).lines().map(str::to_owned).collect();
    let row = |start: &str| {
        rows.iter()
            .position(|row| row.trim_start().starts_with(start))
            .and_then(|at| u16::try_from(at).ok())
            .unwrap_or(u16::MAX)
    };
    let cell = |x: u16, y: u16| {
        buf.cell((x, y))
            .map(|cell| cell.style())
            .unwrap_or_default()
    };
    // The summary line is dim from its dot on; the reply is not.
    let summary = row("• Read");
    assert!(cell(0, summary).add_modifier.contains(Modifier::DIM));
    assert!(cell(0, summary + 1).add_modifier.contains(Modifier::DIM));
    assert!(!cell(0, row("Fixed.")).add_modifier.contains(Modifier::DIM));
    // The bubble is tinted to the right edge, and blank to its left.
    let bubble = row("fix the failing test");
    let reset = Some(ratatui::style::Color::Reset);
    assert_ne!(cell(WIDTH - 1, bubble).bg, reset);
    assert_eq!(cell(0, bubble).bg, reset);
}

#[test]
fn slash_panel() {
    let mut app = empty();
    let now = fakes::clock::FakeClock::new().now();
    for ch in "/h".chars() {
        app.on_key(Key::Char(ch), now);
    }
    insta::assert_snapshot!("slash_panel", sized(&mut app, 80, 24));
}

#[test]
fn the_selected_completion_is_reversed_and_the_others_are_not() {
    let mut app = empty();
    let now = fakes::clock::FakeClock::new().now();
    for ch in "/h".chars() {
        app.on_key(Key::Char(ch), now);
    }
    let (shown, buf) = wide(&mut app);
    let rows: Vec<&str> = shown.lines().collect();
    assert!(rows[HEIGHT as usize - 4].starts_with("/home"), "{shown}");
    let reversed = |row: u16| {
        buf.cell((0, row))
            .is_some_and(|cell| cell.modifier.contains(Modifier::REVERSED))
    };
    assert!(reversed(HEIGHT - 4), "{shown}");
    assert!(!reversed(HEIGHT - 3), "{shown}");
    assert!(!reversed(HEIGHT - 2), "{shown}");
}

#[test]
fn key_map() {
    let mut app = empty();
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::F1, now);
    insta::assert_snapshot!("key_map_80x24", sized(&mut app, 80, 24));
    insta::assert_snapshot!("key_map_40x12", sized(&mut app, 40, 12));
}

#[test]
fn file_panel() {
    let mut app = empty();
    let now = fakes::clock::FakeClock::new().now();
    for ch in "look at @ma".chars() {
        app.on_key(Key::Char(ch), now);
    }
    let found = ["src/main.rs", "crates/tui/src/main.rs"].map(str::to_owned);
    app.on_files(app.generation(), Ok(found.to_vec()));
    insta::assert_snapshot!("file_panel", sized(&mut app, 80, 24));
}

#[test]
fn search_panel() {
    let mut app = empty();
    let now = fakes::clock::FakeClock::new().now();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    for prompt in [
        "fix the build",
        "run the tests",
        "Fix the docs\nand the build",
    ] {
        app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", prompt));
    }
    for ch in "draft".chars() {
        app.on_key(Key::Char(ch), now);
    }
    app.on_key(Key::CtrlR, now);
    for ch in "fix".chars() {
        app.on_key(Key::Char(ch), now);
    }
    app.on_key(Key::Down, now);
    insta::assert_snapshot!("search_panel", sized(&mut app, 80, 24));
    let (shown, buf) = wide(&mut app);
    let rows: Vec<&str> = shown.lines().collect();
    assert_eq!(rows[HEIGHT as usize - 4], "search prompts: fix", "{shown}");
    let reversed = |row: u16| {
        buf.cell((0, row))
            .is_some_and(|cell| cell.modifier.contains(Modifier::REVERSED))
    };
    assert!(!reversed(HEIGHT - 4), "{shown}");
    assert!(!reversed(HEIGHT - 3), "{shown}");
    assert!(reversed(HEIGHT - 2), "{shown}");
    // Nothing matching says so.
    app.on_key(Key::Char('z'), now);
    let (shown, _) = wide(&mut app);
    assert!(shown.contains("no matching prompts"), "{shown}");
}

/// Renders `app` at `width` by `height` with the pointer at `pointer`,
/// returning the buffer and the click targets.
fn pointed(
    app: &mut App,
    width: u16,
    height: u16,
    pointer: Option<(u16, u16)>,
) -> (Buffer, Vec<crate::mouse::Target>) {
    app.set_size(width, height);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = render(app, area, &mut buf, pointer);
    (buf, targets)
}

/// An app with one request put aside, so the badge shows.
fn badged() -> App {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = asked();
    app.on_line(standing(S_A, "a_1", "r_1"));
    app.on_key(Key::Esc, now);
    app
}

#[test]
fn the_badge_is_a_target_over_the_cells_it_drew() {
    use crate::mouse::{Target, TargetId};
    let mut app = badged();
    // The badge's 30 characters on the row above the input line.
    let (_, targets) = pointed(&mut app, 40, 12, None);
    let badge = Target {
        id: TargetId::Badge,
        rect: Rect::new(0, 10, 30, 1),
    };
    assert_eq!(targets, vec![badge]);
    // Narrower than its text, it takes the whole row.
    let (_, targets) = pointed(&mut app, 20, 12, None);
    assert_eq!(targets.first().map(|target| target.rect.width), Some(20));
    // A screen with no row for the badge draws no target.
    let (_, targets) = pointed(&mut app, 40, 1, None);
    assert!(targets.is_empty());
}

#[test]
fn new_messages_below_is_a_target_over_the_cells_it_drew() {
    use crate::mouse::{Target, TargetId};
    let now = fakes::clock::FakeClock::new().now();
    let mut app = empty();
    attach(&mut app, S_A);
    app.on_line(turn_started(S_A, "one"));
    app.on_key(Key::PageUp, now);
    app.on_line(delta(S_A, "a_1", "streamed"));
    // "↓ New messages below" is 20 cells, centred on the row above the
    // input line.
    let (_, targets) = pointed(&mut app, 30, 2, None);
    let below = Target {
        id: TargetId::NewBelow,
        rect: Rect::new(5, 0, 20, 1),
    };
    assert_eq!(targets, vec![below]);
    let (_, targets) = pointed(&mut app, 10, 2, None);
    assert_eq!(
        targets.first().map(|target| target.rect),
        Some(Rect::new(0, 0, 10, 1))
    );
    // No conversation row, no overlay and no target.
    let (_, targets) = pointed(&mut app, 30, 1, None);
    assert!(targets.is_empty());
}

#[test]
fn the_target_under_the_pointer_gets_the_hover_background_only() {
    let mut app = badged();
    let (plain, _) = pointed(&mut app, 40, 12, None);
    let (hovered, _) = pointed(&mut app, 40, 12, Some((7, 10)));
    for y in 0..12 {
        for x in 0..40 {
            let (Some(before), Some(after)) = (plain.cell((x, y)), hovered.cell((x, y))) else {
                panic!("no cell at {x},{y}");
            };
            let mut expected = before.clone();
            if y == 10 && x < 30 {
                expected.bg = super::HOVER_TINT.bg.unwrap_or_default();
            }
            assert_eq!(after, &expected, "cell {x},{y}");
        }
    }
    // Off every target, the frame is the one with no pointer.
    for pointer in [(30, 10), (0, 11), (0, 9), (39, 0)] {
        let (off, _) = pointed(&mut app, 40, 12, Some(pointer));
        assert_eq!(off, plain, "{pointer:?}");
    }
}

/// The conversation-line targets drawn, as what each opens and its cells.
fn lines(targets: &[crate::mouse::Target]) -> Vec<(crate::app::Target, Rect)> {
    targets
        .iter()
        .filter_map(|target| match target.id {
            crate::mouse::TargetId::Line(line) => Some((line, target.rect)),
            crate::mouse::TargetId::Badge
            | crate::mouse::TargetId::NewBelow
            | crate::mouse::TargetId::Token(_) => None,
        })
        .collect()
}

#[test]
fn a_collapsed_groups_line_is_a_target_over_its_rows() {
    use crate::app::Target;
    let mut app = empty();
    tool_turn(&mut app);
    // The summary wraps to rows 6 and 7 of `tool_group_collapsed`.
    let (_, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    let group = app.targets().first().map(|(_, target)| *target);
    assert!(matches!(group, Some(Target::Group(_))), "{group:?}");
    let drawn: Vec<_> = lines(&targets).into_iter().map(|(_, rect)| rect).collect();
    assert_eq!(drawn, vec![Rect::new(0, 6, WIDTH, 2)]);
    assert_eq!(
        lines(&targets).first().map(|(target, _)| Some(*target)),
        Some(group)
    );
}

#[test]
fn ledger_rows_are_targets_over_their_rows() {
    use crate::app::Target;
    let mut app = empty();
    tool_turn(&mut app);
    app.on_key(Key::CtrlO, fakes::clock::FakeClock::new().now());
    let edit = app.targets().get(3).map(|(_, target)| *target);
    if let Some(edit) = edit {
        app.open(edit);
    }
    // As `tool_group_ledger_open_with_a_call_open` draws them: the
    // summary, the thought, the read, the edit and the shell row; the
    // edit's diff below it opens nothing.
    let (_, targets) = pointed(&mut app, WIDTH, 16, None);
    let drawn = lines(&targets);
    let kinds: Vec<_> = drawn
        .iter()
        .map(|(target, _)| match target {
            Target::Group(_) => 'g',
            Target::Thought(_) => 't',
            Target::Call(_) => 'c',
        })
        .collect();
    assert_eq!(kinds, vec!['g', 't', 'c', 'c', 'c']);
    let rects: Vec<_> = drawn.iter().map(|(_, rect)| *rect).collect();
    assert_eq!(
        rects,
        vec![
            Rect::new(0, 4, WIDTH, 2),
            Rect::new(0, 6, WIDTH, 1),
            Rect::new(0, 7, WIDTH, 1),
            Rect::new(0, 8, WIDTH, 1),
            Rect::new(0, 11, WIDTH, 1),
        ]
    );
    let opens: Vec<_> = app
        .targets()
        .into_iter()
        .map(|(_, target)| target)
        .collect();
    let drawn: Vec<_> = drawn.into_iter().map(|(target, _)| target).collect();
    assert_eq!(drawn, opens);
}

#[test]
fn a_thought_line_is_a_target_over_its_row() {
    use crate::app::Target;
    let mut app = empty();
    thought_turn(&mut app);
    // Row 8 of `thought_line`.
    let (_, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    let drawn = lines(&targets);
    assert_eq!(drawn.len(), 1);
    assert!(matches!(drawn.first(), Some((Target::Thought(_), _))));
    assert_eq!(
        drawn.first().map(|(_, rect)| *rect),
        Some(Rect::new(0, 8, WIDTH, 1))
    );
}

#[test]
fn a_line_partly_scrolled_off_targets_only_its_rows_shown() {
    let mut app = empty();
    tool_turn(&mut app);
    // Four conversation rows: the summary's second row, the reply and the
    // two-row footer.
    let (_, targets) = pointed(&mut app, WIDTH, 5, None);
    let rects: Vec<_> = lines(&targets).into_iter().map(|(_, rect)| rect).collect();
    assert_eq!(rects, vec![Rect::new(0, 0, WIDTH, 1)]);
    // Three rows: the summary is wholly out of view.
    let (_, targets) = pointed(&mut app, WIDTH, 4, None);
    assert!(lines(&targets).is_empty());
}

#[test]
fn the_overlay_row_hits_new_messages_below_not_the_line_under_it() {
    use crate::mouse::{TargetId, hit};
    let now = fakes::clock::FakeClock::new().now();
    let mut app = empty();
    tool_turn(&mut app);
    app.set_size(WIDTH, 4);
    app.on_key(Key::PageUp, now);
    app.on_line(turn_started(S_A, "next"));
    assert!(app.has_new());
    // From the third row of the turn: the summary's two rows, the second
    // under the overlay.
    let (buf, targets) = pointed(&mut app, WIDTH, 3, None);
    assert!(text(&buf).contains("New messages below"), "{}", text(&buf));
    let rects: Vec<_> = lines(&targets).into_iter().map(|(_, rect)| rect).collect();
    assert_eq!(rects, vec![Rect::new(0, 0, WIDTH, 1)]);
    assert_eq!(hit(&targets, WIDTH / 2, 1), Some(TargetId::NewBelow));
    assert_eq!(hit(&targets, 0, 1), None);
    assert!(matches!(hit(&targets, 0, 0), Some(TargetId::Line(_))));
}

/// An empty app with `see ` and then a 312-line paste token typed.
fn with_token() -> App {
    let mut app = empty();
    type_draft(&mut app, "see ");
    let pasted: Vec<String> = (1..=312).map(|n| format!("line {n}")).collect();
    app.on_edit(Edit::Paste(pasted.join("\n")));
    app
}

/// The rects of the targets for paste token 1.
fn token_rects(targets: &[crate::mouse::Target]) -> Vec<Rect> {
    targets
        .iter()
        .filter(|target| target.id == crate::mouse::TargetId::Token(1))
        .map(|target| target.rect)
        .collect()
}

/// The text in `rects` of `buf`, in order.
fn cells(buf: &Buffer, rects: &[Rect]) -> String {
    rects
        .iter()
        .flat_map(|rect| rect.positions())
        .filter_map(|at| buf.cell(at).map(|cell| cell.symbol().to_owned()))
        .collect()
}

const LABEL: &str = "[Pasted text #1 · 312 lines]";

#[test]
fn a_paste_token_is_a_target_over_its_label_on_every_row() {
    let mut app = with_token();
    let (buf, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    let rects = token_rects(&targets);
    assert_eq!(rects, vec![Rect::new(6, 11, 28, 1)]);
    assert_eq!(cells(&buf, &rects), LABEL);
    // Wrapped, one target per row the label takes.
    let (buf, targets) = pointed(&mut app, 20, HEIGHT, None);
    let rects = token_rects(&targets);
    assert_eq!(rects.len(), 2, "{rects:?}");
    assert_eq!(cells(&buf, &rects), LABEL);
}

#[test]
fn a_paste_token_scrolled_out_of_the_box_is_no_target() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = with_token();
    type_draft(&mut app, "\n2\n3\n4\n5");
    // The box shows its last four rows; the token's row is above them.
    let (_, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    assert!(token_rects(&targets).is_empty(), "{targets:?}");
    for _ in 0..4 {
        app.on_key(Key::Up, now);
    }
    // Scrolled to its top, the token is on the box's first row.
    let (buf, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    let rects = token_rects(&targets);
    assert_eq!(rects, vec![Rect::new(6, 8, 28, 1)]);
    assert_eq!(cells(&buf, &rects), LABEL);
}

#[test]
fn a_click_on_a_token_opens_it_and_a_click_elsewhere_in_the_box_does_not() {
    use crate::app::Effect;
    use crate::keys::{Button, Mouse, MouseKind};
    use crate::mouse::Pointer;
    let mut app = with_token();
    let (_, targets) = pointed(&mut app, 20, HEIGHT, None);
    let mut pointer = Pointer::default();
    let mut click = |col: u16, row: u16| {
        let press = Mouse {
            kind: MouseKind::Press(Button::Left),
            col,
            row,
        };
        pointer.on_mouse(&press, &targets, true);
        let release = Mouse {
            kind: MouseKind::Release,
            ..press
        };
        pointer.on_mouse(&release, &targets, true)
    };
    // The prompt, `see ` and the cell after the label are no token.
    let rects = token_rects(&targets);
    let (first, last) = (rects[0], rects[rects.len() - 1]);
    for (col, row) in [(0, first.y), (first.x - 1, first.y), (last.right(), last.y)] {
        assert_eq!(click(col, row), None, "{col},{row}");
    }
    let text = app.input().token_text(1).unwrap_or_default().to_owned();
    for (col, row) in [(first.x, first.y), (last.right() - 1, last.y)] {
        let clicked = click(col, row);
        assert_eq!(clicked, Some(crate::mouse::TargetId::Token(1)));
        let effect = clicked.map(|target| app.on_click(target));
        let expected = Effect::Editor {
            target: crate::editor::Target::Token(1),
            text: text.clone(),
        };
        assert_eq!(effect, Some(expected), "{col},{row}");
    }
}

#[test]
fn hovering_a_token_tints_every_cell_of_its_label_only() {
    let mut app = with_token();
    let (plain, targets) = pointed(&mut app, 20, HEIGHT, None);
    let rects = token_rects(&targets);
    let first = rects[0];
    let (hovered, _) = pointed(&mut app, 20, HEIGHT, Some((first.x, first.y)));
    for y in 0..HEIGHT {
        for x in 0..20 {
            let (Some(before), Some(after)) = (plain.cell((x, y)), hovered.cell((x, y))) else {
                panic!("no cell at {x},{y}");
            };
            let mut expected = before.clone();
            if rects.iter().any(|rect| rect.contains(Position::new(x, y))) {
                expected.bg = super::HOVER_TINT.bg.unwrap_or_default();
            }
            assert_eq!(after, &expected, "cell {x},{y}");
        }
    }
}
