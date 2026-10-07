//! Screen snapshots: the whole in-memory screen against stored files.

use super::{cursor, render, text};
use crate::app::App;
use crate::keys::{Edit, Key};
use crate::link::Line;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use std::path::PathBuf;

const WIDTH: u16 = 60;
const HEIGHT: u16 = 12;

/// Renders `app` on a 60x12 screen as text, rows trimmed of trailing spaces.
fn screen(app: &App) -> String {
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf);
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
    assert!(screen(&app).starts_with("› prompt 10\n"));
    // One page down lands exactly on the bottom, which follows again: new
    // output scrolls in with no overlay.
    app.on_key(Key::PageDown, now);
    assert_eq!(screen(&app), bottom);
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "prompt 31"));
    let followed = screen(&app);
    assert!(!followed.contains("↓ New messages below"));
    assert!(followed.contains("› prompt 31\n>"));
    app.on_key(Key::PageUp, now);
    assert!(screen(&app).starts_with("› prompt 11\n"));
    app.on_key(Key::PageUp, now);
    assert!(screen(&app).starts_with("› prompt 1\n"));
    app.on_key(Key::PageDown, now);
    assert!(screen(&app).starts_with("› prompt 11\n"));
    app.on_key(Key::PageDown, now);
    assert!(screen(&app).contains("› prompt 31\n>"));
    // PageDown while following does nothing.
    app.on_key(Key::PageDown, now);
    assert!(screen(&app).contains("› prompt 31\n>"));
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
    assert_eq!(shown, "\n› hi\nHello.\n>\n");
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
    render(app, area, &mut buf);
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
        "› two\nlost\nPress Ctrl+C again t\n>\n"
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
    render(app, area, &mut buf);
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
