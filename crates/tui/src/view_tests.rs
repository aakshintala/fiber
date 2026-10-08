//! Screen snapshots: the whole in-memory screen against stored files.

use super::{cursor, render, text};
use crate::app::App;
use crate::keys::{Edit, Key};
use crate::link::Line;
use crate::local_time::{new_york, turn_started_at};
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Position, Rect};
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

/// A row of `buf` as text, trailing spaces kept.
fn row_text(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| buf[(x, y)].symbol().to_owned())
        .collect()
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
fn prompt_bubble_shows_the_local_time_under_it() {
    let mut app = empty();
    app.set_zone(new_york());
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    // 2026-10-08T14:15Z, 10:15 in New York.
    app.on_line(turn_started_at("s_aaaaaaaaaaaaaaaa", "go", 1791468900000));
    let texts: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
    assert_eq!(texts, vec![" go ".to_owned(), "10:15".to_owned()]);
    let time = app
        .lines()
        .into_iter()
        .find(|line| line.to_string() == "10:15")
        .unwrap_or_default();
    assert!(time.style.add_modifier.contains(Modifier::DIM));
    assert_eq!(time.alignment, Some(Alignment::Right));
    // The rendered row ends in the conversation's last column, on the row
    // under the bubble.
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let at = (0..HEIGHT)
        .find(|y| row_text(&buf, *y).contains("10:15"))
        .expect("the time row is drawn");
    assert!(row_text(&buf, at).ends_with("10:15"));
    assert!(row_text(&buf, at.saturating_sub(1)).contains(" go "));
    insta::assert_snapshot!("prompt_bubble_with_time", screen(&app));
}

#[test]
fn prompt_bubble_defaults_to_utc() {
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started_at("s_aaaaaaaaaaaaaaaa", "go", 1791468900000));
    let texts: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
    assert_eq!(texts, vec![" go ".to_owned(), "14:15".to_owned()]);
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
    // The bottom shows prompt 25's time through prompt 30: two rows a
    // turn over sixty rows, eleven of conversation. A page is those 11
    // rows less one, and the top stops at the first row.
    let time = format!("{:>60}", "00:00");
    let prompt = |n: u32| format!("{:>59}", format!("prompt {n}"));
    app.on_key(Key::PageUp, now);
    let shown = screen(&app);
    let shown: Vec<&str> = shown.lines().collect();
    assert_eq!(shown[0], time, "one page up moves ten rows");
    assert_eq!(shown[1], prompt(21));
    // One page down lands exactly on the bottom, which follows again: new
    // output scrolls in with no overlay.
    app.on_key(Key::PageDown, now);
    assert_eq!(screen(&app), bottom);
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "prompt 31"));
    let followed = screen(&app);
    assert!(!followed.contains("↓ New messages below"));
    assert!(followed.contains(format!("{}\n{}\n>", prompt(31), time).as_str()));
    // Six pages up reaches the top, where another stops: 51, 41, 31, 21,
    // 11, 1, then clamped to 0.
    for _ in 0..6 {
        app.on_key(Key::PageUp, now);
    }
    assert!(screen(&app).starts_with(format!("{}\n", prompt(1)).as_str()));
    app.on_key(Key::PageUp, now);
    assert!(screen(&app).starts_with(format!("{}\n", prompt(1)).as_str()));
    // Six pages down lands exactly on the bottom: 10, 20, 30, 40, 50,
    // then clamped to 51. Odd tops show a turn's time first, even tops
    // its bubble: row 41 is prompt 21's time, row 10 prompt 6's bubble.
    app.on_key(Key::PageDown, now);
    assert!(screen(&app).starts_with(format!("{}\n", prompt(6)).as_str()));
    for _ in 0..5 {
        app.on_key(Key::PageDown, now);
    }
    assert!(screen(&app).contains(format!("{}\n{}\n>", prompt(31), time).as_str()));
    // PageDown while following does nothing.
    app.on_key(Key::PageDown, now);
    assert!(screen(&app).contains(format!("{}\n{}\n>", prompt(31), time).as_str()));
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
    assert_eq!(shown, format!("{:>19}\n{:>20}\nHello.\n>\n", "hi", "00:00"));
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
    // The input line wins the last row, then the hint; the conversation
    // gets what is left, the notice floating over its top row.
    let notice = "lost   ✕";
    assert_eq!(sized(&mut app, 20, 1), ">\n");
    assert_eq!(sized(&mut app, 20, 2), "Press Ctrl+C again t\n>\n");
    assert_eq!(
        sized(&mut app, 20, 3),
        format!("{notice:>20}\nPress Ctrl+C again t\n>\n")
    );
    assert_eq!(
        sized(&mut app, 20, 4),
        format!("{notice:>20}\n{:>20}\nPress Ctrl+C again t\n>\n", "00:00")
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
fn a_short_screen_drops_the_badge_after_the_hint() {
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
        format!(
            "{:>40}\nPress Ctrl+C again to quit\n{badge}\n>\n",
            "lost           ✕"
        )
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
            "input_bytes": 0, "cost": cost});
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
            crate::app::Target::Group(_)
            | crate::app::Target::Thought(_)
            | crate::app::Target::Login
            | crate::app::Target::Note(_)
            | crate::app::Target::Orphans(_)
            | crate::app::Target::Copy { .. } => None,
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
            "input_bytes": 0, "cost": null}),
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
fn slash_panel_with_the_sessions_commands() {
    let mut app = asked_nothing();
    let asked = app.on_line(session_line(
        S_A,
        "reloaded",
        serde_json::json!({"servers": {"kept": [], "restarted": [], "started": [],
            "stopped": []}, "extensions": []}),
        None,
    ));
    let asked: serde_json::Value = serde_json::from_str(&asked[0]).unwrap_or_default();
    app.on_line(session_line(
        S_A,
        "command_accepted",
        serde_json::json!({"command_id": asked["id"], "result": {"commands": [
            {"name": "review", "description": "Reviews a diff.", "argument_hint": "[base]",
             "tag": "template"},
            {"name": "refactor", "description": "Refactors a module.", "tag": "skill"}]}}),
        None,
    ));
    let now = fakes::clock::FakeClock::new().now();
    for ch in "/re".chars() {
        app.on_key(Key::Char(ch), now);
    }
    insta::assert_snapshot!(
        "slash_panel_with_the_sessions_commands",
        sized(&mut app, 80, 24)
    );
}

/// A connected app attached to `S_A`, with nothing on screen.
fn asked_nothing() -> App {
    let mut app = empty();
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    attach(&mut app, S_A);
    app
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

#[test]
fn steering_rows_above_the_input() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = empty();
    attach(&mut app, S_A);
    app.on_line(turn_started(S_A, "hi"));
    app.on_line(session_line(
        S_A,
        "steering_queue",
        serde_json::json!({"messages": [
            {"content": [{"type": "text", "text": "use the parser"}], "source": "driver", "command_id": "c_1"},
            {"content": [{"type": "text", "text": "and test it"}], "source": "driver", "command_id": "c_2"},
        ]}),
        None,
    ));
    app.on_key(Key::AltUp, clock.now());
    insta::assert_snapshot!("steering_rows_above_the_input", screen(&app));
}

#[test]
fn zero_width_draws_no_steering_drop_target() {
    use crate::mouse::TargetId;
    let mut app = empty();
    attach(&mut app, S_A);
    app.on_line(turn_started(S_A, "hi"));
    app.on_line(session_line(
        S_A,
        "steering_queue",
        serde_json::json!({"messages": [
            {"content": [{"type": "text", "text": "use the parser"}], "source": "driver", "command_id": "c_1"},
        ]}),
        None,
    ));
    assert!(
        app.steering_drops().iter().any(|drop| *drop),
        "the fixture needs a selectable steering row"
    );
    let area = Rect::new(0, 0, 0, HEIGHT);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    assert!(
        targets
            .iter()
            .all(|target| !matches!(target.id, TargetId::DropSteering(_))),
        "{targets:?}"
    );
    for target in &targets {
        assert!(
            target.rect.x >= area.x
                && target.rect.right() <= area.right()
                && target.rect.y >= area.y
                && target.rect.bottom() <= area.bottom(),
            "{target:?} outside {area:?}"
        );
    }
}

#[test]
fn notices_float_and_nothing_below_the_conversation_moves() {
    let mut app = empty();
    attach(&mut app, S_A);
    for n in 1..=4 {
        app.on_line(turn_started(S_A, &format!("prompt {n}")));
        app.on_line(turn_completed(S_A, "completed"));
    }
    let before = screen(&app);
    app.connect_failed("Could not reach the hub: the socket refused the connection".to_owned());
    for n in 1..=3 {
        app.on_line(session_line(
            S_A,
            "notice",
            serde_json::json!({"code": "extension_failed", "message": format!("Notice {n}.")}),
            None,
        ));
    }
    let after = screen(&app);
    let below = |screen: &str| screen.lines().last().map(str::to_owned);
    assert_eq!(below(&before), below(&after));
    assert_eq!(before.lines().count(), after.lines().count());
    insta::assert_snapshot!("notices_float", after);
    app.open_more_notices();
    insta::assert_snapshot!("notices_listed", screen(&app));
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

/// Renders `app` at `width` by `height`, returning the buffer.
fn buffer(app: &App, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    buf
}

/// Clicks the target drawn at `col`, `row` of `app` at `width` by
/// `height`, if any.
fn click(app: &mut App, width: u16, height: u16, col: u16, row: u16) {
    let (_, targets) = pointed(app, width, height, None);
    if let Some(target) = crate::mouse::hit(&targets, col, row) {
        app.on_click(target);
    }
}

/// A reply in markdown, as one model streams it.
const MARKDOWN: &str = "# Plan\n\n- read the **file**\n- write it\n\n```rust\nfn main() {\n    let x = 1;\n}\n```\n\n| step | ms |\n|---|---|\n| parse | 12 |\n| draw | 3 |";

/// An app at `width` by `height` with `text` streamed as one reply.
fn replying(width: u16, height: u16, text: &str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "plan it"));
    app.on_line(delta("s_aaaaaaaaaaaaaaaa", "a_1", text));
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
    // The open turn's prompt and its time are stops on the two rows above
    // the badge.
    let turn = Target {
        id: TargetId::Turn(0),
        rect: Rect::new(0, 8, 40, 2),
    };
    assert_eq!(targets, vec![badge, turn]);
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
            | crate::mouse::TargetId::Token(_)
            | crate::mouse::TargetId::Steering(_)
            | crate::mouse::TargetId::DropSteering(_)
            | crate::mouse::TargetId::Notice(_)
            | crate::mouse::TargetId::DismissNotice(_)
            | crate::mouse::TargetId::CloseOverlay
            | crate::mouse::TargetId::Home(_)
            | crate::mouse::TargetId::Offer(_)
            | crate::mouse::TargetId::Form(_)
            | crate::mouse::TargetId::Turn(_)
            | crate::mouse::TargetId::Link { .. }
            | crate::mouse::TargetId::FindCount
            | crate::mouse::TargetId::FindResult(_)
            | crate::mouse::TargetId::MoreNotices => None,
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
            Target::Login | Target::Note(_) | Target::Orphans(_) => 'o',
            Target::Copy { .. } => 'y',
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

#[test]
fn a_paste_token_below_the_box_or_with_no_room_is_no_target() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = empty();
    type_draft(&mut app, "1\n2\n3\n4\n");
    let pasted: Vec<String> = (1..=312).map(|n| format!("line {n}")).collect();
    app.on_edit(Edit::Paste(pasted.join("\n")));
    for _ in 0..4 {
        app.on_key(Key::Up, now);
    }
    // The box shows rows 0 to 3; the token is on row 4, just below it.
    let (_, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    assert!(token_rects(&targets).is_empty(), "{targets:?}");
    // Two columns wide, the label starts past the last column: no cells.
    let mut app = with_token();
    let (_, targets) = pointed(&mut app, 2, HEIGHT, None);
    assert!(token_rects(&targets).is_empty(), "{targets:?}");
}

#[test]
fn a_streaming_reply_renders_its_markdown_in_place() {
    // Mid-stream: the fence is open, so the rest is code.
    let cut = MARKDOWN.find("}\n```").unwrap_or_default();
    let mut app = replying(40, 18, &MARKDOWN[..cut]);
    insta::assert_snapshot!("markdown_reply_streaming", text(&buffer(&app, 40, 18)));
    app.on_line(delta("s_aaaaaaaaaaaaaaaa", "a_1", &MARKDOWN[cut..]));
    let buf = buffer(&app, 40, 18);
    insta::assert_snapshot!("markdown_reply", text(&buf));
    let fg = |x, y| buf.cell((x, y)).map(|cell| cell.fg);
    let bg = |x, y| buf.cell((x, y)).map(|cell| cell.bg);
    let shown = text(&buf);
    let row = |needle: &str| {
        let at = shown.lines().position(|line| line.contains(needle));
        u16::try_from(at.unwrap_or_else(|| panic!("{needle} on screen"))).unwrap_or(0)
    };
    use crate::markdown::Role;
    assert_eq!(fg(0, row("Plan")), Some(Role::Heading.color()));
    assert_eq!(fg(0, row("read the")), Some(Role::Accent.color()));
    assert_eq!(fg(2, row("read the")), Some(Role::Text.color()));
    let header = row("rust");
    assert_eq!(fg(36, header), Some(Role::Accent.color()));
    for x in 0..40 {
        assert_eq!(
            bg(x, header),
            Some(Role::CodeTint.color()),
            "header col {x}"
        );
        assert_eq!(
            bg(x, header + 2),
            Some(Role::CodeTint.color()),
            "code col {x}"
        );
    }
    assert_eq!(fg(8, header + 2), Some(Role::Keyword.color()));
    assert_eq!(fg(16, header + 2), Some(Role::Number.color()));
    assert_eq!(fg(0, row("────")), Some(Role::Dim.color()));
}

#[test]
fn a_resize_renders_the_reply_again_at_the_new_width() {
    let mut app = replying(40, 18, "```rust\nlet x = 1;\n```");
    assert!(text(&buffer(&app, 40, 18)).contains(&format!("rust{}copy", " ".repeat(32))));
    app.set_size(20, 18);
    assert!(text(&buffer(&app, 20, 18)).contains(&format!("rust{}copy", " ".repeat(12))));
}

#[test]
fn text_completed_replaces_and_renders_the_reply_again() {
    let mut app = replying(40, 8, "# Draft");
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "text_completed",
        serde_json::json!({"text": "- done"}),
        Some("a_1"),
    ));
    let shown = text(&buffer(&app, 40, 8));
    assert!(shown.contains("• done"));
    assert!(!shown.contains("Draft"));
}

#[test]
fn copied_shows_on_the_conversations_top_row_until_the_next_key() {
    let mut app = replying(30, 10, "```rust\nlet x = 1;\n```");
    let shown = text(&buffer(&app, 30, 10));
    let header = shown
        .lines()
        .position(|line| line.contains("rust"))
        .and_then(|at| u16::try_from(at).ok())
        .unwrap_or_default();
    click(&mut app, 30, 10, 27, header);
    let buf = buffer(&app, 30, 10);
    insta::assert_snapshot!("copied", text(&buf));
    assert_eq!(
        buf.cell((24, 0)).map(|cell| cell.fg),
        Some(crate::markdown::Role::Accent.color())
    );
    app.on_key(Key::Char('x'), fakes::clock::FakeClock::new().now());
    assert!(!text(&buffer(&app, 30, 10)).contains("Copied"));
}

#[test]
fn copied_needs_a_conversation_row_to_show_on() {
    let mut app = replying(30, 10, "```rust\nlet x = 1;\n```");
    assert!(
        text(&buffer(&app, 30, 10))
            .lines()
            .nth(7)
            .is_some_and(|row| row.starts_with("rust"))
    );
    click(&mut app, 30, 10, 27, 7);
    assert!(app.copied());
    app.set_size(30, 1);
    assert_eq!(text(&buffer(&app, 30, 1)), ">\n");
}

#[test]
fn the_focused_target_is_drawn_reversed() {
    let mut app = empty();
    tool_turn(&mut app);
    tool_turn(&mut app);
    app.on_key(Key::BackTab, fakes::clock::FakeClock::new().now());
    let (buf, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    app.drawn(&targets);
    let focused = app.focused().expect("focus");
    let rect = targets
        .iter()
        .find(|target| target.id == focused)
        .map(|target| target.rect)
        .expect("the focused stop is drawn");
    for y in rect.top()..rect.bottom() {
        for x in rect.left()..rect.right() {
            assert!(
                buf.cell((x, y))
                    .is_some_and(|cell| cell.modifier.contains(Modifier::REVERSED)),
                "cell {x},{y} of the focused row is reversed"
            );
        }
    }
    let other = targets
        .iter()
        .find(|target| matches!(target.id, crate::mouse::TargetId::Line(_)) && target.id != focused)
        .map(|target| target.rect)
        .expect("another line stop");
    for y in other.top()..other.bottom() {
        for x in other.left()..other.right() {
            assert!(
                !buf.cell((x, y))
                    .is_some_and(|cell| cell.modifier.contains(Modifier::REVERSED)),
                "cell {x},{y} of the other row is not reversed"
            );
        }
    }
}

#[test]
fn the_cursor_hides_while_navigating() {
    let mut app = empty();
    tool_turn(&mut app);
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    assert!(cursor(&app, area).is_some());
    app.on_key(Key::BackTab, fakes::clock::FakeClock::new().now());
    let (_, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    app.drawn(&targets);
    assert_eq!(cursor(&app, area), None);
    app.on_key(Key::Esc, fakes::clock::FakeClock::new().now());
    let (_, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    app.drawn(&targets);
    assert!(cursor(&app, area).is_some());
}

#[test]
fn an_open_overlay_draws_its_cross_as_a_target() {
    use crate::mouse::TargetId;
    let cross = Rect::new(WIDTH - 1, 0, 1, 1);
    let mut app = empty();
    attach(&mut app, S_A);
    app.on_key(Key::F1, fakes::clock::FakeClock::new().now());
    let (buf, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::CloseOverlay && target.rect == cross),
        "the key map draws its cross"
    );
    assert_eq!(
        buf.cell((WIDTH - 1, 0)).map(|cell| cell.symbol()),
        Some("✕")
    );
    let mut app = empty();
    attach(&mut app, S_A);
    app.on_line(session_line(
        S_A,
        "notice",
        serde_json::json!({"code": "x", "message": "Saved."}),
        None,
    ));
    app.open_notice(0);
    let (buf, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::CloseOverlay && target.rect == cross),
        "the notice overlay draws its cross"
    );
    assert_eq!(
        buf.cell((WIDTH - 1, 0)).map(|cell| cell.symbol()),
        Some("✕")
    );
}

#[test]
fn no_cross_without_conversation_rows() {
    let mut app = empty();
    attach(&mut app, S_A);
    app.on_key(Key::F1, fakes::clock::FakeClock::new().now());
    let (_, targets) = pointed(&mut app, WIDTH, 1, None);
    assert!(
        targets
            .iter()
            .all(|target| target.id != crate::mouse::TargetId::CloseOverlay),
        "no cross on a screen with no conversation rows"
    );
}

#[test]
fn a_turn_spanning_drawn_lines_sums_their_heights() {
    use crate::mouse::TargetId;
    let mut app = empty();
    tool_turn(&mut app);
    let (_, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    let turn = targets
        .iter()
        .find(|target| target.id == TargetId::Turn(0))
        .expect("turn 0 is drawn");
    assert_eq!(turn.rect, Rect::new(0, 4, WIDTH, 7));
}

#[test]
fn a_turn_past_the_last_row_is_clipped_to_it() {
    use crate::mouse::TargetId;
    let now = fakes::clock::FakeClock::new().now();
    let mut app = empty();
    tool_turn(&mut app);
    app.set_size(WIDTH, 4);
    app.on_key(Key::PageUp, now);
    app.on_line(turn_started(S_A, "next"));
    assert!(app.has_new());
    let (_, targets) = pointed(&mut app, WIDTH, 3, None);
    let turn = targets
        .iter()
        .find(|target| target.id == TargetId::Turn(0))
        .expect("turn 0 is drawn");
    assert_eq!(turn.rect, Rect::new(0, 0, WIDTH, 1));
}
