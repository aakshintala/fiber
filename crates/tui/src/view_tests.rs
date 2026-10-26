//! Screen snapshots: the whole in-memory screen against stored files.

use super::{conversation_rows, cursor, draw_row, focus_style, lift_hover, render, text};
use crate::app::App;
use crate::keys::{Edit, Key};
use crate::link::Line;
use crate::local_time::{new_york, turn_started_at};
use crate::markdown::Role;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Position, Rect};
use ratatui::style::{Modifier, Style};
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
    assert_eq!(
        texts,
        vec![
            "".to_owned(),
            "▄▄▄▄▄".to_owned(),
            " go ▐".to_owned(),
            "▀▀▀▀▀".to_owned(),
            "you 10:15 ".to_owned(),
            "".to_owned()
        ]
    );
    let time = app
        .lines()
        .into_iter()
        .find(|line| line.to_string() == "you 10:15 ")
        .unwrap_or_default();
    assert!(time.style.add_modifier.contains(Modifier::DIM));
    assert_eq!(time.alignment, Some(Alignment::Right));
    // The rendered row ends in the rows' last text column, with the bar's
    // column blank after it, on the row under the bubble.
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let at = (0..HEIGHT)
        .find(|y| row_text(&buf, *y).contains("10:15"))
        .expect("the time row is drawn");
    assert!(row_text(&buf, at).trim_end().ends_with("you 10:15"));
    // The time sits under the bubble's bottom edge, its text row above
    // that.
    assert!(
        row_text(&buf, at.saturating_sub(1))
            .trim()
            .chars()
            .all(|ch| ch == '▀')
    );
    assert!(row_text(&buf, at.saturating_sub(2)).contains(" go ▐"));
    insta::assert_snapshot!("prompt_bubble_with_time", screen(&app));
    // Without a set zone the same instant draws in UTC.
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started_at("s_aaaaaaaaaaaaaaaa", "go", 1791468900000));
    let texts: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
    assert_eq!(
        texts,
        vec![
            "".to_owned(),
            "▄▄▄▄▄".to_owned(),
            " go ▐".to_owned(),
            "▀▀▀▀▀".to_owned(),
            "you 14:15 ".to_owned(),
            "".to_owned()
        ]
    );
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
    // The rows above the overlay are the rows that were there, bar
    // column aside: the taller history moves the thumb.
    let kept: Vec<&str> = before.lines().take(8).collect();
    let kept_again: Vec<&str> = after.lines().take(8).collect();
    assert_eq!(
        kept_again
            .iter()
            .map(|row| row.trim_end_matches(['┃', '│', '█']))
            .collect::<Vec<_>>(),
        kept.iter()
            .map(|row| row.trim_end_matches(['┃', '│', '█']))
            .collect::<Vec<_>>()
    );
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
    // The bottom shows prompt 28's bubble through prompt 29: six rows
    // a turn over 180 rows, nine of conversation. A page is those 9
    // rows less one, and the top stops at the first row.
    app.on_key(Key::PageUp, now);
    let shown = screen(&app);
    let shown: Vec<&str> = shown.lines().collect();
    assert_eq!(
        shown[0],
        format!("{:>59}", "▄".repeat(12)),
        "one page up moves eight rows"
    );
    assert!(shown[1].contains("prompt 28"), "{shown:?}");
    assert_eq!(shown[2], format!("{:>59}", "▀".repeat(12)));
    assert_eq!(shown[3], format!("{:>58}", "you 00:00"));
    assert_eq!(shown[4], "");
    assert_eq!(shown[5], "");
    // One page down lands exactly on the bottom, which follows again: new
    // output scrolls in with no overlay.
    app.on_key(Key::PageDown, now);
    assert_eq!(screen(&app), bottom);
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "prompt 31"));
    let followed = screen(&app);
    assert!(!followed.contains("↓ New messages below"));
    assert!(followed.contains("prompt 31"));
    // Twenty-three pages up reaches the top, where another stops.
    for _ in 0..23 {
        app.on_key(Key::PageUp, now);
    }
    assert_eq!(app.top(), Some(0));
    let top = screen(&app);
    let top: Vec<&str> = top.lines().collect();
    // The first row is the blank before the first bubble, with the
    // thumb in the bar's column.
    assert_eq!(top[0], format!("{}┃", " ".repeat(59)));
    assert!(top[1].trim_start().starts_with('▄'), "{top:?}");
    assert!(top[2].contains("prompt 1"), "{top:?}");
    app.on_key(Key::PageUp, now);
    assert_eq!(app.top(), Some(0));
    // Twenty-three pages down lands exactly on the bottom.
    app.on_key(Key::PageDown, now);
    assert_eq!(app.top(), Some(8));
    for _ in 0..22 {
        app.on_key(Key::PageDown, now);
    }
    assert!(screen(&app).contains("prompt 31"));
    // PageDown while following does nothing.
    app.on_key(Key::PageDown, now);
    assert!(screen(&app).contains("prompt 31"));
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
    assert_eq!(rows.get(9).map(|row| row.chars().count()), Some(60));
    assert!(rows.get(9).is_some_and(|row| row.starts_with("▌ › a")));
    assert_eq!(rows.get(10).copied(), Some("▌   aaaaaaaaaaaaaaend█"));
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
    assert_eq!(
        shown,
        format!(
            "{}┃\n{}\n▌ › █\n{}\n",
            "▀".repeat(19),
            "▄".repeat(20),
            "▀".repeat(20)
        )
    );
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
    // The input line wins the last row with its edges where they fit,
    // then the notice; a screen too short for them all drops the hint
    // first, and the badge with the conversation. The edged input box
    // takes the notice's row before the hint's.
    assert_eq!(sized(&mut app, 20, 1), "▌ › █\n");
    assert_eq!(sized(&mut app, 20, 2), "  ⚠ connection_fa… ✕\n▌ › █\n");
    assert_eq!(
        sized(&mut app, 20, 3),
        "▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄\n▌ › █\n▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀\n"
    );
    assert_eq!(
        sized(&mut app, 20, 4),
        "  ⚠ connection_fa… ✕\n▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄\n▌ › █\n▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀\n"
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
    assert_eq!(sized(&mut app, 30, 1), "▌ › █\n");
    assert_eq!(
        sized(&mut app, 30, 2),
        "▐ ↓ New messages below · End ┃\n▌ › █\n"
    );
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
    // Header to hint, eight rows, take the approval tint with edges above
    // and below; the rows beyond the edges do not.
    for row in 3..11 {
        assert_eq!(bg(&buf, row), super::APPROVAL_TINT.bg, "row {row}");
    }
    assert_eq!(bg(&buf, 2), Some(ratatui::style::Color::Reset));
    assert_eq!(bg(&buf, 11), Some(ratatui::style::Color::Reset));
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
    assert_eq!(bg(&buf, 10), super::ALERT_TINT.bg);
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

/// The row and column where `needle` first shows in `shown`.
fn spot(shown: &str, needle: &str) -> (u16, u16) {
    shown
        .lines()
        .enumerate()
        .find_map(|(row, line)| {
            let col = line.find(needle)?;
            let col = line.get(..col)?.chars().count();
            Some((u16::try_from(row).ok()?, u16::try_from(col).ok()?))
        })
        .unwrap_or_else(|| panic!("{needle:?} in\n{shown}"))
}

#[test]
fn irreversible_is_bold_error_in_the_header() {
    use ratatui::style::Modifier;
    let mut app = asked();
    // The rule's line ends as the header does, and stays plain.
    app.on_line(request(
        S_A,
        "a_1",
        "r_1",
        serde_json::json!({"reversible": false, "step": "standing_ask",
            "standing_rule": {"scope": "project", "prefix": "git push · irreversible"}}),
    ));
    let (shown, buf) = wide(&mut app);
    let (row, col) = spot(&shown, "· irreversible");
    let word = col + u16::try_from("· ".chars().count()).expect("a width");
    for x in word..word + 12 {
        let cell = &buf[(x, row)];
        assert_eq!(cell.fg, crate::theme::Role::Error.color(), "col {x}");
        assert!(cell.modifier.contains(Modifier::BOLD), "col {x}");
        assert_eq!(cell.bg, crate::theme::Role::Approval.color(), "col {x}");
    }
    // The loud run starts at the " · " before it; the session ends in
    // `attention`, not bold.
    let loud = &buf[(word - 3, row)];
    assert!(loud.modifier.contains(Modifier::BOLD));
    assert_eq!(loud.fg, crate::theme::Role::Error.color());
    let session_end = &buf[(word - 4, row)];
    assert!(!session_end.modifier.contains(Modifier::BOLD));
    assert_eq!(session_end.fg, crate::theme::Role::Attention.color());
    let (rule, at) = spot(&shown, "push · irreversible");
    assert_eq!(rule, row + 1, "{shown}");
    let plain = &buf[(at + 7, rule)];
    assert!(!plain.modifier.contains(Modifier::BOLD), "{shown}");
    assert_eq!(plain.fg, ratatui::style::Color::Reset);
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
    assert_eq!(sized(&mut app, 40, 1), "▌ › █\n");
    assert_eq!(
        sized(&mut app, 40, 2),
        "  ⚠ connection_failed · lost           ✕\n▌ › █\n".to_owned()
    );
    // The input box's edges take the rows the hint and the badge lose.
    assert_eq!(
        sized(&mut app, 40, 3),
        format!("{}\n▌ › █\n{}\n", "▄".repeat(40), "▀".repeat(40))
    );
    assert_eq!(
        sized(&mut app, 40, 4),
        format!(
            "  ⚠ connection_failed · lost           ✕\n{}\n▌ › █\n{}\n",
            "▄".repeat(40),
            "▀".repeat(40)
        )
    );
    // Taller, the badge and the hint return above the notice.
    assert_eq!(
        sized(&mut app, 40, 5),
        format!(
            "  ⚠ 1 approvals waiting · click to reope\n  ⚠ connection_failed · lost           ✕\n{}\n▌ › █\n{}\n",
            "▄".repeat(40),
            "▀".repeat(40)
        )
    );
}

#[test]
fn a_panel_taller_than_the_screen_keeps_its_header() {
    let mut app = asked();
    app.on_line(standing(S_A, "a_1", "r_1"));
    assert_eq!(
        sized(&mut app, 40, 2),
        format!("▌ Approval 1 of 1 · {S_A}\n▌ Esc puts it aside\n")
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
    assert_eq!(cursor_at(&app), Some(Position::new(9, 10)));
}

#[test]
fn a_draft_taller_than_a_third_scrolls_with_the_cursor() {
    let mut app = empty();
    type_draft(&mut app, "l1\nl2\nl3\nl4\nl5\nl6");
    // 12 rows: the box shows 4, the last four while the cursor is there.
    insta::assert_snapshot!("draft_taller_than_the_cap", screen(&app));
    assert_eq!(cursor_at(&app), Some(Position::new(6, 10)));
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..5 {
        app.on_key(Key::Up, now);
    }
    // On the first line, the box shows the first four.
    insta::assert_snapshot!("draft_scrolled_to_its_top", screen(&app));
    assert_eq!(cursor_at(&app), Some(Position::new(6, 7)));
}

#[test]
fn a_paste_token_in_the_draft() {
    let mut app = empty();
    type_draft(&mut app, "see ");
    let pasted: Vec<String> = (1..=312).map(|n| format!("line {n}")).collect();
    app.on_edit(Edit::Paste(pasted.join("\n")));
    insta::assert_snapshot!("draft_with_a_paste_token", screen(&app));
    assert_eq!(cursor_at(&app), Some(Position::new(36, 10)));
}

#[test]
fn a_draft_wider_than_the_screen_wraps() {
    let mut app = empty();
    let long: String = std::iter::repeat_n('w', 70).collect();
    type_draft(&mut app, &long);
    let shown = screen(&app);
    let rows: Vec<&str> = shown.lines().collect();
    assert_eq!(
        rows.get(9).copied(),
        Some(format!("▌ › {}", "w".repeat(56)).as_str())
    );
    assert_eq!(
        rows.get(10).copied(),
        Some(format!("▌   {}█", "w".repeat(14)).as_str())
    );
    assert_eq!(cursor_at(&app), Some(Position::new(18, 10)));
}

#[test]
fn the_cursor_hides_while_the_panel_is_open() {
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(cursor_at(&app), Some(Position::new(4, 10)));
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
            | crate::app::Target::Image(_)
            | crate::app::Target::Note(_)
            | crate::app::Target::Orphans(_)
            | crate::app::Target::CallView(_)
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
fn ledger_with_an_image_line() {
    use serde_json::json;
    // A call whose result carried an image draws its one line under
    // its ledger row once the ledger opens (`docs/tui.md`, "Images").
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "look at this"));
    app.on_line(at("step_started", None, 0, json!({})));
    app.on_line(at(
        "tool_call_requested",
        Some("a_1"),
        0,
        json!({"name": "read", "arguments": {"path": "shot.png"}}),
    ));
    app.on_line(at(
        "tool_call_completed",
        Some("a_1"),
        1_000,
        json!({"status": "completed", "content": [
            {"type": "text", "text": "saved"},
            {"type": "image", "path": "artifacts/shot.png",
                "mime_type": "image/png", "width": 1280, "height": 800},
        ]}),
    ));
    app.on_line(at(
        "turn_completed",
        None,
        2_000,
        json!({"outcome": "completed"}),
    ));
    app.on_key(Key::CtrlO, fakes::clock::FakeClock::new().now());
    insta::assert_snapshot!("ledger_with_an_image_line", sized(&mut app, 80, 12));
}

/// A one-row buffer with a placeholder cell then a plain one.
fn placeholder_row() -> Buffer {
    let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
    buf.set_string(
        0,
        0,
        "\u{10EEEE}\u{305}\u{305}",
        Style::new().fg(ratatui::style::Color::Indexed(1)),
    );
    buf.set_string(1, 0, "x", Style::new());
    buf
}

#[test]
fn hover_leaves_placeholder_cells_alone() {
    let mut buf = placeholder_row();
    lift_hover(&mut buf, Rect::new(0, 0, 2, 1));
    // The placeholder keeps its id-encoding colours, while the plain
    // cell takes the hover tint.
    assert_eq!(buf[(0, 0)].fg, ratatui::style::Color::Indexed(1));
    assert_eq!(buf[(0, 0)].bg, ratatui::style::Color::Reset);
    assert_ne!(buf[(1, 0)].bg, ratatui::style::Color::Reset);
}

#[test]
fn focus_leaves_placeholder_cells_alone() {
    let mut buf = placeholder_row();
    focus_style(&mut buf, Rect::new(0, 0, 2, 1));
    // Reversing a placeholder would destroy the image id its colours
    // carry; the plain cell still takes the focus style.
    assert!(!buf[(0, 0)].modifier.contains(Modifier::REVERSED));
    assert!(buf[(1, 0)].modifier.contains(Modifier::REVERSED));
}

#[test]
fn ledger_with_an_inline_image() {
    use serde_json::json;
    // Where the terminal speaks kitty's graphics protocol, a call whose
    // result carried a PNG draws its placeholder grid under its ledger
    // row, then its caption line (`docs/tui.md`, "Images").
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: std::path::PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        inline_images: true,
        ..Default::default()
    });
    app.set_graphics();
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "look at this"));
    app.on_line(at("step_started", None, 0, json!({})));
    app.on_line(at(
        "tool_call_requested",
        Some("a_1"),
        0,
        json!({"name": "read", "arguments": {"path": "shot.png"}}),
    ));
    app.on_line(at(
        "tool_call_completed",
        Some("a_1"),
        1_000,
        json!({"status": "completed", "content": [
            {"type": "text", "text": "saved"},
            {"type": "image", "path": "artifacts/shot.png",
                "mime_type": "image/png", "width": 1280, "height": 800},
        ]}),
    ));
    app.on_line(at(
        "turn_completed",
        None,
        2_000,
        json!({"outcome": "completed"}),
    ));
    app.on_key(Key::CtrlO, fakes::clock::FakeClock::new().now());
    insta::assert_snapshot!("ledger_with_an_inline_image", sized(&mut app, 80, 24));
}

#[test]
fn clicking_a_placeholder_opens_the_image_in_the_viewer() {
    use serde_json::json;
    // A call whose result carried a PNG draws its placeholder grid under
    // its ledger row: clicking a placeholder cell opens the viewer, like
    // the caption does (`docs/tui.md`, "Images").
    let mut app = empty();
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: std::path::PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        inline_images: true,
        ..Default::default()
    });
    app.set_graphics();
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "look at this"));
    app.on_line(at("step_started", None, 0, json!({})));
    app.on_line(at(
        "tool_call_requested",
        Some("a_1"),
        0,
        json!({"name": "read", "arguments": {"path": "shot.png"}}),
    ));
    app.on_line(at(
        "tool_call_completed",
        Some("a_1"),
        1_000,
        json!({"status": "completed", "content": [
            {"type": "text", "text": "saved"},
            {"type": "image", "path": "artifacts/shot.png",
                "mime_type": "image/png", "width": 1280, "height": 800},
        ]}),
    ));
    app.on_line(at(
        "turn_completed",
        None,
        2_000,
        json!({"outcome": "completed"}),
    ));
    app.on_key(Key::CtrlO, fakes::clock::FakeClock::new().now());
    let (buf, targets) = pointed(&mut app, 80, 24, None);
    // The first placeholder cell: a grid cell, not the caption.
    let mut cell = None;
    for y in 0..24 {
        for x in 0..80 {
            if buf[(x, y)].symbol().starts_with('\u{10EEEE}') {
                cell = Some((x, y));
                break;
            }
        }
        if cell.is_some() {
            break;
        }
    }
    let (x, y) = cell.expect("the grid draws placeholder cells");
    let target = crate::mouse::hit(&targets, x, y).expect("the cell is a target");
    assert!(
        matches!(
            target,
            crate::mouse::TargetId::Line(crate::app::Target::Image(_))
        ),
        "the placeholder carries the image target: {target:?}"
    );
    let effect = app.on_click(target);
    if let crate::app::Effect::Send(lines) = effect {
        assert_eq!(lines.len(), 1);
        let line: serde_json::Value = serde_json::from_str(&lines[0]).expect("a command line");
        assert_eq!(
            line.get("command").and_then(serde_json::Value::as_str),
            Some("read_file")
        );
        assert_eq!(
            line.get("args")
                .and_then(|args| args.get("path"))
                .and_then(serde_json::Value::as_str),
            Some("artifacts/shot.png")
        );
    } else {
        panic!("the click opens the viewer: {effect:?}");
    }
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
    // The card's edges add rows, so the bubble needs a taller screen.
    let area = Rect::new(0, 0, WIDTH, 30);
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
    // The summary line is dim from its dot on; the reply is not. The dot
    // sits past the card's one-column padding. The summary is one row,
    // whatever runs, so the row after it is the reply's, not the summary's
    // second row.
    let summary = row("● Read");
    assert!(cell(1, summary).add_modifier.contains(Modifier::DIM));
    assert!(!cell(0, summary + 1).add_modifier.contains(Modifier::DIM));
    assert!(!cell(0, row("Fixed.")).add_modifier.contains(Modifier::DIM));
    // The bubble is tinted to the last text column, and blank to its
    // left.
    let bubble = row("fix the failing test");
    let reset = Some(ratatui::style::Color::Reset);
    assert_ne!(cell(WIDTH - 2, bubble).bg, reset);
    assert_eq!(cell(0, bubble).bg, reset);
    // The card's turn tint reaches the last text column: past the end
    // of a short card row, and on the split rows of a long one
    // (`docs/tui.md`, "Look"). The time row keeps the background.
    let turn = Some(crate::theme::Role::Turn.color());
    assert_eq!(cell(30, row("Fixed.")).bg, turn);
    assert_eq!(cell(WIDTH - 2, summary + 1).bg, turn);
    assert_eq!(cell(0, row("you 00:00")).bg, reset);
}

#[test]
fn a_card_row_wrapped_by_the_view_is_tinted_on_every_row() {
    // The ▣ closing line splits in the card at 17 columns; every row it
    // draws carries the card's tint (`docs/tui.md`, "Look").
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(20, 10);
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    for action in ["a_1", "a_2", "a_3", "a_4"] {
        app.on_line(session_line(
            "s_aaaaaaaaaaaaaaaa",
            "tool_call_requested",
            serde_json::json!({"name": "read", "arguments": {"path": "a.rs"}}),
            Some(action),
        ));
    }
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "text_completed",
        serde_json::json!({ "text": "ok" }),
        Some("a_m"),
    ));
    app.on_line(turn_completed("s_aaaaaaaaaaaaaaaa", "completed"));
    let area = Rect::new(0, 0, 20, 10);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let shown: Vec<String> = text(&buf).lines().map(str::to_owned).collect();
    let closing = shown
        .iter()
        .position(|row| row.contains("▣"))
        .expect("a closing line");
    // "▣ completed · 4 calls" is 22 columns: two card rows, the split
    // putting "calls" on the second whatever the bar draws
    // after it.
    assert!(shown[closing + 1].starts_with(" calls"), "{shown:?}");
    let turn = crate::theme::Role::Turn.color();
    let row = u16::try_from(closing).unwrap_or(u16::MAX);
    assert_eq!(buf[(0, row)].bg, turn);
    assert_eq!(buf[(0, row + 1)].bg, turn);
    assert_eq!(buf[(18, row + 1)].bg, turn);
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
fn the_focused_completion_sits_on_the_bar_and_the_others_do_not() {
    let mut app = empty();
    let now = fakes::clock::FakeClock::new().now();
    for ch in "/h".chars() {
        app.on_key(Key::Char(ch), now);
    }
    let (shown, buf) = wide(&mut app);
    // `/h` matches home, handoff and help first, then thinking: four
    // rows in the overlay above the input line, the first focused. Its
    // cells sit on the bar in `accent`; the rest keep the surface tint.
    let row = |y: u16| {
        (0..80)
            .map(|x| buf[(x, y)].symbol().to_owned())
            .collect::<String>()
    };
    let accent = crate::theme::Role::Accent.color();
    let surface = crate::theme::Role::Surface.color();
    for (name, barred) in [
        ("home", true),
        ("handoff", false),
        ("help", false),
        ("thinking", false),
    ] {
        let y = u16::try_from(
            shown
                .lines()
                .position(|line| line.contains(name))
                .unwrap_or_else(|| panic!("the {name} row in {shown}")),
        )
        .unwrap_or(u16::MAX);
        let text = row(y);
        // Cell columns, not byte indices: the focused row's gutter
        // holds a three-byte `›` in one cell.
        let at = text
            .find(name)
            .unwrap_or_else(|| panic!("the {name} in {shown}"));
        let x = u16::try_from(text[..at].chars().count()).unwrap_or(u16::MAX);
        assert_eq!(
            buf[(x, y)].bg,
            if barred { accent } else { surface },
            "{name} in {shown}"
        );
    }
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
    // The panel draws centred on the overlay frame, in no reserved rows.
    app.set_size(80, 24);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let shown = text(&buf);
    assert!(shown.contains("Prompt history"), "{shown}");
    assert!(shown.contains("› fix"), "{shown}");
    assert!(shown.contains("2 matches · newest first"), "{shown}");
    assert!(shown.contains("Enter recalls · Esc closes"), "{shown}");
    assert!(!shown.contains("this session"), "{shown}");
    // The draft waits in the input box, which keeps its own rows.
    assert!(shown.contains("draft"), "{shown}");
    // Exactly one barred row: the second match, recall order newest first.
    let bars: Vec<u16> = (0..24)
        .filter(|y| (0..80).any(|x| buf[(x, *y)].bg == crate::markdown::Role::Accent.color()))
        .collect();
    assert_eq!(bars.len(), 1, "{shown}");
    let row: String = (0..80)
        .map(|x| buf[(x, bars[0])].symbol().to_owned())
        .collect();
    assert!(row.contains("fix the build"), "{row:?}");
    // Nothing matching says so, with nothing barred.
    app.on_key(Key::Char('z'), now);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let shown = text(&buf);
    assert!(shown.contains("no matching prompts"), "{shown}");
    assert!(
        (0..24).all(|y| (0..80).all(|x| buf[(x, y)].bg != crate::markdown::Role::Accent.color())),
        "{shown}"
    );
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
        app.steering_rows().iter().any(|row| row.droppable),
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
fn notices_above_the_working_line() {
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
    // The input box stays on the last row; the conversation above it
    // gives up the notice rows.
    let below = |screen: &str| screen.lines().last().map(str::to_owned);
    assert_eq!(below(&before), below(&after));
    assert_eq!(before.lines().count(), after.lines().count());
    insta::assert_snapshot!("notices_above_the_working_line", after);
    app.open_more_notices();
    insta::assert_snapshot!("notices_listed", screen(&app));
}

/// An app with a running turn at `width` by `height`: home set, the
/// hub up, the session streaming and a requested call, so the working
/// line shows.
fn running(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    app.set_size(width, height);
    attach(&mut app, S_A);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.on_line(session_line(
        S_A,
        "session_status",
        serde_json::json!({
            "name": "work", "workspace": "/w", "project": "-w",
            "state": "streaming", "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0,
            "clients": 0}),
        None,
    ));
    app.on_line(turn_started(S_A, "go"));
    app.on_line(session_line(
        S_A,
        "step_started",
        serde_json::json!({}),
        None,
    ));
    app.on_line(session_line(
        S_A,
        "tool_call_requested",
        serde_json::json!({"name": "read", "arguments": {"path": "a.rs"}}),
        Some("a_1"),
    ));
    assert!(app.working_row_shown(), "a working line");
    app
}

/// Four session notices on `app`, "Notice 1." oldest.
fn raise_four(app: &mut App) {
    for n in 1..=4 {
        app.on_line(session_line(
            S_A,
            "notice",
            serde_json::json!({"code": "extension_failed", "message": format!("Notice {n}.")}),
            None,
        ));
    }
}

/// A row of `buf` as text, trailing spaces kept.
fn full_row(buf: &Buffer, y: u16, width: u16) -> String {
    (0..width)
        .map(|x| buf[(x, y)].symbol().to_owned())
        .collect()
}

/// The working line's row: the row running a turn draws.
fn working_row(buf: &Buffer, width: u16) -> u16 {
    (0..buf.area.height)
        .find(|y| full_row(buf, *y, width).contains("Working"))
        .unwrap_or_else(|| panic!("a working line"))
}

#[test]
fn four_notices_three_rows_and_more() {
    let mut app = running(100, 24);
    raise_four(&mut app);
    let buf = buffer(&app, 100, 24);
    let working = working_row(&buf, 100);
    // Three rows oldest first, then "+1 more" directly above the
    // working line.
    for (at, text) in [
        (working - 4, "   ⚠ extension_failed · Notice 1."),
        (working - 3, "   ⚠ extension_failed · Notice 2."),
        (working - 2, "   ⚠ extension_failed · Notice 3."),
        (working - 1, "   +1 more"),
    ] {
        let row = full_row(&buf, at, 100);
        assert!(row.starts_with(text), "row {at}: {row:?}");
    }
    // Each notice row ends in a dim ✕ in the last column.
    for row in working - 4..working - 1 {
        let row_text = full_row(&buf, row, 100);
        assert_eq!(crate::format::width(&row_text), 100, "row {row}");
        assert!(row_text.ends_with('✕'), "row {row}: {row_text:?}");
        assert_eq!(buf[(99, row)].symbol(), "✕");
    }
    // "⚠ extension_failed" reads in attention, the message dim.
    let mark = "⚠ extension_failed";
    let row = full_row(&buf, working - 4, 100);
    let at = row.find(mark).expect("the mark");
    let start = crate::format::width(&row[..at]);
    let start = u16::try_from(start).expect("a column");
    let width = u16::try_from(crate::format::width(mark)).expect("a width");
    for col in start..start + width {
        assert_eq!(
            buf[(col, working - 4)].fg,
            Role::Attention.color(),
            "col {col}"
        );
    }
    let message = row.find("Notice 1.").expect("the message");
    let message_col = crate::format::width(&row[..message]);
    let message_col = u16::try_from(message_col).expect("a column");
    assert!(
        buf[(message_col, working - 4)]
            .modifier
            .contains(Modifier::DIM),
        "row {}",
        working - 4
    );
    insta::assert_snapshot!("four_notices_three_rows_and_more", text(&buf));
}

#[test]
fn short_screens_draw_what_fits_above_the_working_line() {
    // Four notices raised on a running turn: the rows drawn are the
    // rows above the working line, up to four; `below_rows` counts
    // the same rows, so the conversation keeps the rest.
    for height in [14u16, 12, 11, 10] {
        let mut app = running(100, height);
        raise_four(&mut app);
        let buf = buffer(&app, 100, height);
        let working = working_row(&buf, 100);
        let drawn = (0..height)
            .filter(|y| buf[(99, *y)].symbol() == "✕")
            .count()
            + usize::from((0..height).any(|y| full_row(&buf, y, 100).contains("+1 more")));
        assert_eq!(drawn, 4.min(working as usize), "height {height}");
        assert_eq!(
            app.conversation_height(),
            usize::from(height).saturating_sub(app.below_rows()),
            "height {height}"
        );
    }
    // Below the 40×10 floor the screen keeps one line naming its
    // size; the row math still agrees and nothing panics.
    for height in [8u16, 6, 5, 4] {
        let mut app = running(100, height);
        raise_four(&mut app);
        let buf = buffer(&app, 100, height);
        assert!(
            text(&buf).contains("needs 40×10"),
            "height {height}: {}",
            text(&buf)
        );
        assert_eq!(
            app.conversation_height() + app.below_rows(),
            usize::from(height),
            "height {height}"
        );
    }
}

#[test]
fn the_fourth_notice_adds_exactly_the_more_row() {
    let mut app = running(100, 24);
    for n in 1..=2 {
        app.on_line(session_line(
            S_A,
            "notice",
            serde_json::json!({"code": "extension_failed", "message": format!("Notice {n}.")}),
            None,
        ));
    }
    assert_eq!(app.notices().len(), 2);
    let two = app.conversation_height();
    app.on_line(session_line(
        S_A,
        "notice",
        serde_json::json!({"code": "extension_failed", "message": "Notice 3."}),
        None,
    ));
    assert_eq!(app.notices().len(), 3);
    let three = app.conversation_height();
    assert_eq!(two - three, 1);
    app.on_line(session_line(
        S_A,
        "notice",
        serde_json::json!({"code": "extension_failed", "message": "Notice 4."}),
        None,
    ));
    // The fourth notice adds exactly the "+1 more" row.
    assert_eq!(app.notices().len(), 4);
    assert_eq!(three - app.conversation_height(), 1);
}

#[test]
fn a_long_notice_is_cut_with_ellipsis_before_its_cross() {
    let width = 100u16;
    let mut app = running(width, 24);
    let long: String = "x".repeat(usize::from(width) + 20);
    app.on_line(session_line(
        S_A,
        "notice",
        serde_json::json!({"code": "extension_failed", "message": long}),
        None,
    ));
    let buf = buffer(&app, width, 24);
    let row = (0..24)
        .map(|y| full_row(&buf, y, width))
        .find(|row| row.contains('…'))
        .expect("a cut row");
    let cells: Vec<char> = row.chars().collect();
    assert_eq!(cells[usize::from(width) - 3], '…');
    assert_eq!(cells[usize::from(width) - 1], '✕');
    // A message of exactly the text budget draws without "…": one less
    // past the grip, which takes column 0.
    let budget = usize::from(width) - 3 - crate::format::width("  ⚠ extension_failed · ");
    let mut app = running(width, 24);
    app.on_line(session_line(
        S_A,
        "notice",
        serde_json::json!({"code": "extension_failed", "message": "x".repeat(budget)}),
        None,
    ));
    let buf = buffer(&app, width, 24);
    let row = (0..24)
        .map(|y| full_row(&buf, y, width))
        .find(|row| row.contains("extension_failed"))
        .expect("a notice row");
    assert!(!row.contains('…'), "{row:?}");
    assert!(row.ends_with('✕'), "{row:?}");
}

#[test]
fn narrow_widths_draw_what_fits_and_never_panic() {
    for width in [12u16, 8, 4] {
        let mut app = running(width, 24);
        raise_four(&mut app);
        let buf = buffer(&app, width, 24);
        for y in 0..24 {
            assert!(
                crate::format::width(&full_row(&buf, y, width)) <= usize::from(width),
                "{width}x{y}"
            );
        }
    }
    // Width zero draws nothing.
    let mut app = running(100, 24);
    raise_four(&mut app);
    app.set_size(0, 24);
    let area = Rect::new(0, 0, 0, 24);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
}

#[test]
fn a_click_on_a_notice_cross_removes_that_row() {
    let mut app = running(100, 24);
    raise_four(&mut app);
    let buf = buffer(&app, 100, 24);
    let working = working_row(&buf, 100);
    // The first row's ✕ dismisses it; the rows below move up one.
    click(&mut app, 100, 24, 99, working - 4);
    let buf = buffer(&app, 100, 24);
    let working = working_row(&buf, 100);
    assert!(!full_row(&buf, working - 3, 100).contains("Notice 1."));
    assert!(full_row(&buf, working - 3, 100).contains("Notice 2."));
    assert!(full_row(&buf, working - 2, 100).contains("Notice 3."));
    assert!(full_row(&buf, working - 1, 100).contains("Notice 4."));
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

/// A press at `from`, a drag to `to` and its release: selecting copies.
fn select(app: &mut App, width: u16, height: u16, from: (u16, u16), to: (u16, u16)) {
    use crate::keys::{Button, Mouse, MouseKind};
    let report = |app: &mut App, kind: MouseKind, (col, row): (u16, u16)| {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let targets = render(app, area, &mut buf, None);
        app.on_select(&Mouse { kind, col, row }, &targets)
    };
    report(app, MouseKind::Press(Button::Left), from);
    report(app, MouseKind::Drag(Button::Left), to);
    report(app, MouseKind::Release, to);
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
    // The badge's 40 characters on the row above the input box.
    let (_, targets) = pointed(&mut app, 40, 12, None);
    let badge = Target {
        id: TargetId::Badge,
        rect: Rect::new(0, 8, 40, 1),
    };
    // The open turn's prompt, its time and its blank row are stops on
    // the rows above the badge.
    let turn = Target {
        id: TargetId::Turn(0),
        rect: Rect::new(0, 2, 39, 6),
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
    // The pill is 30 cells, centred over the rows area: at 30 columns
    // the rows are 29 wide, so it clips at the right edge with no ▌.
    let (buf, targets) = pointed(&mut app, 30, 2, None);
    let below = Target {
        id: TargetId::NewBelow,
        rect: Rect::new(0, 0, 29, 1),
    };
    assert_eq!(targets, vec![below]);
    assert_eq!(buf[(0, 0)].symbol(), "▐");
    assert!(row_text(&buf, 0).contains("↓ New messages below · End"));
    let (_, targets) = pointed(&mut app, 10, 2, None);
    assert_eq!(
        targets.first().map(|target| target.rect),
        Some(Rect::new(0, 0, 9, 1))
    );
    // No conversation row, no overlay and no target.
    let (_, targets) = pointed(&mut app, 30, 1, None);
    assert!(targets.is_empty());
}

#[test]
fn the_pill_is_centred_on_the_bottom_row_in_attention_on_rule() {
    use crate::mouse::TargetId;
    let now = fakes::clock::FakeClock::new().now();
    let mut app = empty();
    attach(&mut app, S_A);
    for n in 1..=14 {
        app.on_line(turn_started(S_A, &format!("prompt {n}")));
    }
    app.on_key(Key::PageUp, now);
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut plain = Buffer::empty(area);
    render(&app, area, &mut plain, None);
    app.on_line(turn_started(S_A, "one more"));
    // At 60 columns the rows are 59 wide: the 30-cell pill starts at 14.
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    let pill = text(&buf)
        .lines()
        .position(|row| row.contains("↓ New messages below · End"))
        .expect("the pill");
    let row = u16::try_from(pill).unwrap_or(u16::MAX);
    assert_eq!(buf[(14, row)].symbol(), "▐");
    assert_eq!(buf[(14, row)].fg, crate::theme::Role::Rule.color());
    assert_eq!(buf[(43, row)].symbol(), "▌");
    assert_eq!(buf[(43, row)].fg, crate::theme::Role::Rule.color());
    let label: String = (15..=42)
        .map(|x| buf[(x, row)].symbol().to_owned())
        .collect();
    assert_eq!(label, super::NEW_BELOW);
    for x in 15..=42 {
        assert_eq!(
            buf[(x, row)].fg,
            crate::theme::Role::Attention.color(),
            "col {x}"
        );
        assert_eq!(
            buf[(x, row)].bg,
            crate::theme::Role::Rule.color(),
            "col {x}"
        );
    }
    for x in 14..=43 {
        assert!(
            !buf[(x, row)]
                .symbol()
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_digit),
            "col {x}"
        );
    }
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::NewBelow
                && target.rect == Rect::new(14, row, 30, 1)),
        "{targets:?}"
    );
    // Either side of the pill the conversation's row shows through:
    // the row is never blanked.
    for x in (0..14).chain(44..WIDTH) {
        assert_eq!(buf[(x, row)].symbol(), plain[(x, row)].symbol(), "col {x}");
    }
    app.on_key(Key::End, now);
    let mut followed = Buffer::empty(area);
    render(&app, area, &mut followed, None);
    assert!(
        !text(&followed).contains("↓ New messages below"),
        "{}",
        text(&followed)
    );
}

#[test]
fn the_pill_clips_at_a_narrow_conversation() {
    use crate::mouse::TargetId;
    let now = fakes::clock::FakeClock::new().now();
    let mut app = empty();
    attach(&mut app, S_A);
    for n in 1..=14 {
        app.on_line(turn_started(S_A, &format!("prompt {n}")));
    }
    app.on_key(Key::PageUp, now);
    app.on_line(turn_started(S_A, "one more"));
    // At 20 columns the rows are 19 wide: the pill starts at the rows'
    // left edge and is cut at their right one, with no closing end.
    let (buf, targets) = pointed(&mut app, 20, HEIGHT, None);
    let pill = text(&buf)
        .lines()
        .position(|row| row.contains("↓ New messages"))
        .expect("the clipped pill");
    let row = u16::try_from(pill).unwrap_or(u16::MAX);
    assert_eq!(buf[(0, row)].symbol(), "▐");
    assert!(
        (0..20).all(|x| buf[(x, row)].symbol() != "▌"),
        "{}",
        row_text(&buf, row)
    );
    assert!(
        targets.iter().any(
            |target| target.id == TargetId::NewBelow && target.rect == Rect::new(0, row, 19, 1)
        ),
        "{targets:?}"
    );
}

#[test]
fn the_target_under_the_pointer_gets_the_hover_background_only() {
    let mut app = badged();
    let (plain, _) = pointed(&mut app, 40, 12, None);
    let (hovered, _) = pointed(&mut app, 40, 12, Some((7, 8)));
    for y in 0..12 {
        for x in 0..40 {
            let (Some(before), Some(after)) = (plain.cell((x, y)), hovered.cell((x, y))) else {
                panic!("no cell at {x},{y}");
            };
            let mut expected = before.clone();
            if y == 8 && x < 40 {
                expected.bg = Role::Hover.color();
            }
            assert_eq!(after, &expected, "cell {x},{y}");
        }
    }
    // Off every target, the frame is the one with no pointer.
    for pointer in [(39, 9), (0, 11), (0, 9), (39, 0)] {
        let (off, _) = pointed(&mut app, 40, 12, Some(pointer));
        assert_eq!(off, plain, "{pointer:?}");
    }
}

#[test]
fn hover_lifts_a_tinted_cell_and_keeps_hover_where_there_is_no_tint() {
    use ratatui::style::Color;
    let area = Rect::new(0, 0, 5, 1);
    let mut buf = Buffer::empty(area);
    buf[(0, 0)].set_bg(Role::Surface.color());
    buf[(1, 0)].set_bg(Color::Rgb(0x10, 0x10, 0x17));
    buf[(2, 0)].set_bg(Color::Reset);
    buf[(3, 0)].set_bg(Role::Background.color());
    buf[(4, 0)].set_bg(Role::Text.color());
    super::lift_hover(&mut buf, area);
    // A target on a tint draws it 0x14 lighter on each channel.
    assert_eq!(buf[(0, 0)].bg, Color::Indexed(89), "the surface tint lifts");
    assert_eq!(buf[(1, 0)].bg, Role::Hover.color(), "a literal tint lifts");
    // A target with no tint under it draws the hover colour.
    assert_eq!(buf[(2, 0)].bg, Role::Hover.color(), "no tint takes hover");
    assert_eq!(
        buf[(3, 0)].bg,
        Role::Hover.color(),
        "the background tint takes hover"
    );
    assert_eq!(
        buf[(4, 0)].bg,
        Role::Hover.color(),
        "a foreground role takes hover"
    );
}

/// The conversation-line targets drawn, as what each opens and its cells.
fn lines(targets: &[crate::mouse::Target]) -> Vec<(crate::app::Target, Rect)> {
    targets
        .iter()
        .filter_map(|target| match target.id {
            crate::mouse::TargetId::Line(line) => Some((line, target.rect)),
            crate::mouse::TargetId::Badge
            | crate::mouse::TargetId::NewBelow
            | crate::mouse::TargetId::Interrupt
            | crate::mouse::TargetId::Token(_)
            | crate::mouse::TargetId::Steering(_)
            | crate::mouse::TargetId::DropSteering(_)
            | crate::mouse::TargetId::Notice(_)
            | crate::mouse::TargetId::DismissNotice(_)
            | crate::mouse::TargetId::CloseOverlay
            | crate::mouse::TargetId::Home(_)
            | crate::mouse::TargetId::Offer(_)
            | crate::mouse::TargetId::Panel(_)
            | crate::mouse::TargetId::Item(_)
            | crate::mouse::TargetId::Rail(_)
            | crate::mouse::TargetId::Form(_)
            | crate::mouse::TargetId::Turn(_)
            | crate::mouse::TargetId::Link { .. }
            | crate::mouse::TargetId::FindCount
            | crate::mouse::TargetId::FindResult(_)
            | crate::mouse::TargetId::View(_)
            | crate::mouse::TargetId::MoreNotices => None,
        })
        .collect()
}

#[test]
fn a_collapsed_groups_line_is_a_target_over_its_rows() {
    use crate::app::Target;
    let mut app = empty();
    tool_turn(&mut app);
    // The summary is one row: its line is the target over that row.
    let (_, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    let group = app.targets().first().map(|(_, target)| *target);
    assert!(matches!(group, Some(Target::Group(_))), "{group:?}");
    let drawn: Vec<_> = lines(&targets).into_iter().map(|(_, rect)| rect).collect();
    assert_eq!(drawn, vec![Rect::new(0, 2, WIDTH - 1, 1)]);
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
            Target::Login
            | Target::Image(_)
            | Target::Note(_)
            | Target::Orphans(_)
            | Target::CallView(_) => 'o',
            Target::Copy { .. } => 'y',
        })
        .collect();
    assert_eq!(kinds, vec!['g', 't', 'c', 'c', 'c']);
    let rects: Vec<_> = drawn.iter().map(|(_, rect)| *rect).collect();
    assert_eq!(
        rects,
        vec![
            Rect::new(0, 0, WIDTH - 1, 1),
            Rect::new(0, 1, WIDTH - 1, 1),
            Rect::new(0, 2, WIDTH - 1, 1),
            Rect::new(0, 3, WIDTH - 1, 1),
            Rect::new(0, 6, WIDTH - 1, 1),
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
    // Row 3 of `thought_line`.
    let (_, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    let drawn = lines(&targets);
    assert_eq!(drawn.len(), 1);
    assert!(matches!(drawn.first(), Some((Target::Thought(_), _))));
    assert_eq!(
        drawn.first().map(|(_, rect)| *rect),
        Some(Rect::new(0, 3, WIDTH - 1, 1))
    );
}

#[test]
fn a_line_partly_scrolled_off_targets_only_its_rows_shown() {
    let mut app = empty();
    tool_turn(&mut app);
    // Seven conversation rows: the group summary whole, the reply, the
    // two-row footer and the blanks between them.
    let (_, targets) = pointed(&mut app, WIDTH, 10, None);
    let rects: Vec<_> = lines(&targets).into_iter().map(|(_, rect)| rect).collect();
    assert_eq!(rects, vec![Rect::new(0, 0, WIDTH - 1, 1)]);
    // Six rows: the summary is wholly out of view.
    let (_, targets) = pointed(&mut app, WIDTH, 9, None);
    assert!(lines(&targets).is_empty());
}

#[test]
fn the_overlay_row_hits_new_messages_below_not_the_line_under_it() {
    use crate::mouse::{TargetId, hit};
    let now = fakes::clock::FakeClock::new().now();
    let mut app = empty();
    tool_turn(&mut app);
    app.set_size(WIDTH, 6);
    app.on_key(Key::PageUp, now);
    app.on_line(turn_started(S_A, "next"));
    assert!(app.has_new());
    // The reply's row above the overlay, with the pill below it: the
    // pill's cells hit NewBelow, while the same row outside the pill
    // hits nothing, and the plain row above hits nothing either.
    let (buf, targets) = pointed(&mut app, WIDTH, 5, None);
    assert!(text(&buf).contains("New messages below"), "{}", text(&buf));
    assert_eq!(hit(&targets, WIDTH / 2, 1), Some(TargetId::NewBelow));
    assert_eq!(hit(&targets, 0, 1), None);
    assert_eq!(hit(&targets, 0, 0), None);
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
    assert_eq!(rects, vec![Rect::new(8, 10, 28, 1)]);
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
    assert_eq!(rects, vec![Rect::new(8, 7, 28, 1)]);
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
                expected.bg = crate::theme::hover_bg(before.bg);
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
    let mut app = replying(40, 26, &MARKDOWN[..cut]);
    insta::assert_snapshot!("markdown_reply_streaming", text(&buffer(&app, 40, 26)));
    app.on_line(delta("s_aaaaaaaaaaaaaaaa", "a_1", &MARKDOWN[cut..]));
    let buf = buffer(&app, 40, 26);
    insta::assert_snapshot!("markdown_reply", text(&buf));
    let fg = |x, y| buf.cell((x, y)).map(|cell| cell.fg);
    let bg = |x, y| buf.cell((x, y)).map(|cell| cell.bg);
    let shown = text(&buf);
    let row = |needle: &str| {
        let at = shown.lines().position(|line| line.contains(needle));
        u16::try_from(at.unwrap_or_else(|| panic!("{needle} on screen"))).unwrap_or(0)
    };
    use crate::markdown::Role;
    assert_eq!(fg(1, row("Plan")), Some(Role::Heading.color()));
    assert_eq!(fg(1, row("read the")), Some(Role::Accent.color()));
    assert_eq!(fg(3, row("read the")), Some(Role::Text.color()));
    let header = row("rust");
    assert_eq!(fg(36, header), Some(Role::Muted.color()));
    // The padding sits on the card's tint; the code rows sit on theirs.
    assert_eq!(bg(0, header), Some(Role::Turn.color()), "header pad");
    assert_eq!(bg(0, header + 2), Some(Role::Turn.color()), "code pad");
    for x in 1..38 {
        assert_eq!(bg(x, header), Some(Role::Code.color()), "header col {x}");
        assert_eq!(bg(x, header + 2), Some(Role::Code.color()), "code col {x}");
    }
    assert_eq!(fg(11, header + 2), Some(Role::Keyword.color()));
    assert_eq!(fg(19, header + 2), Some(Role::Number.color()));
    assert_eq!(fg(1, row("────")), Some(Role::Rule.color()));
}

#[test]
fn a_resize_renders_the_reply_again_at_the_new_width() {
    let mut app = replying(40, 18, "```rust\nlet x = 1;\n```");
    // At 40 columns the header renders at the card's inner width of 37.
    assert!(text(&buffer(&app, 40, 18)).contains(&format!("rust{}click to copy", " ".repeat(19))));
    app.set_size(20, 18);
    // At 20 columns its language name ellipsizes.
    assert!(text(&buffer(&app, 20, 18)).contains("r… click to copy"));
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
    // " Copied " in `info` on the `surface` tint, on the top row with
    // the bar closed.
    let row = text(&buf)
        .lines()
        .position(|line| line.contains("Copied"))
        .expect("Copied shows");
    assert_eq!(row, 0);
    let line = text(&buf).lines().nth(row).expect("its row").to_owned();
    let col = u16::try_from(
        line[..line.find("Copied").expect("its column")]
            .chars()
            .count()
            .saturating_sub(1),
    )
    .expect("a column");
    for dx in 0..8 {
        let cell = &buf[(col + dx, 0)];
        assert_eq!(cell.fg, Role::Info.color(), "Copied {dx}");
        assert_eq!(cell.bg, Role::Surface.color(), "Copied {dx}");
    }
    app.on_key(Key::Char('x'), fakes::clock::FakeClock::new().now());
    assert!(!text(&buffer(&app, 30, 10)).contains("Copied"));
}

#[test]
fn copied_sits_on_the_row_under_the_bar() {
    // (area width, area height, bar open, Copied's row): under the
    // five-row slab, under the bare row, on the top row with the bar
    // closed, and on the top row under the width floor, both sides of it.
    for (width, height, bar, want) in [
        (40, 10, false, 0),
        (60, 10, true, 5),
        (60, 4, true, 1),
        (5, 10, true, 5),
        (4, 10, true, 0),
    ] {
        let mut app = replying(60, 24, "copy this line");
        if bar {
            app.on_key(Key::CtrlF, fakes::clock::FakeClock::new().now());
            assert!(app.find_bar().is_some(), "the bar is open");
        }
        let shown = text(&buffer(&app, 60, 24));
        let row = u16::try_from(
            shown
                .lines()
                .position(|line| line.contains("copy this"))
                .expect("the reply"),
        )
        .expect("a row");
        select(&mut app, 60, 24, (0, row), (3, row));
        assert!(
            text(&buffer(&app, 60, 24)).contains("Copied"),
            "the drag copies"
        );
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let mut targets = Vec::new();
        conversation_rows(&app, area, &mut buf, &mut targets);
        // " Copied " is the only `info` on `surface`: its rows are its
        // row, even where the area clips its text.
        let rows: Vec<u16> = (0..height)
            .filter(|y| {
                (0..width).any(|x| {
                    let cell = &buf[(x, *y)];
                    cell.fg == Role::Info.color() && cell.bg == Role::Surface.color()
                })
            })
            .collect();
        assert_eq!(rows, vec![want], "{width}x{height} bar {bar}");
        let shown = text(&buf);
        let line = shown.lines().nth(usize::from(want)).expect("its row");
        assert!(line.contains("Cop"), "{width}x{height}: {line:?}");
    }
}

#[test]
fn copied_sits_on_the_top_row_while_the_results_view_is_open() {
    // The bar is open and the results view over it: the offset the bar
    // takes while the results are closed does not apply, so "Copied"
    // sits on the conversation's top row (`docs/tui.md`, "Search",
    // "Selection and copy"). Dropping the results check moves it under
    // the slab.
    let mut app = replying(60, 24, "copy this line");
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::CtrlF, now);
    assert!(app.find_bar().is_some(), "the bar is open");
    let shown = text(&buffer(&app, 60, 24));
    let row = u16::try_from(
        shown
            .lines()
            .position(|line| line.contains("copy this"))
            .expect("the reply"),
    )
    .expect("a row");
    // Anchor the selection while the conversation draws, open the
    // results view, then drag and release: the release copies with the
    // bar and the results view both open (a key clears an older copy,
    // but nothing clears one the release sets after it).
    use crate::keys::{Button, Mouse, MouseKind};
    let (_, targets) = pointed(&mut app, 60, 24, None);
    let _ = app.on_select(
        &Mouse {
            kind: MouseKind::Press(Button::Left),
            col: 0,
            row,
        },
        &targets,
    );
    // A second Ctrl+F opens the results view; the bar stays open.
    app.on_key(Key::CtrlF, now);
    assert!(app.results_open(), "the results view is open");
    let _ = app.on_select(
        &Mouse {
            kind: MouseKind::Drag(Button::Left),
            col: 3,
            row,
        },
        &targets,
    );
    let _ = app.on_select(
        &Mouse {
            kind: MouseKind::Release,
            col: 3,
            row,
        },
        &targets,
    );
    let area = Rect::new(0, 0, 60, 10);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    conversation_rows(&app, area, &mut buf, &mut targets);
    // " Copied " is the only `info` on `surface`: it sits on the top
    // row, with no bar offset while the results are open.
    let rows: Vec<u16> = (0..10)
        .filter(|y| {
            (0..60).any(|x| {
                let cell = &buf[(x, *y)];
                cell.fg == Role::Info.color() && cell.bg == Role::Surface.color()
            })
        })
        .collect();
    assert_eq!(rows, vec![0]);
}

#[test]
fn copied_stays_inside_a_short_or_narrow_area() {
    // The buffer around each area stays blank: with the bar open, a
    // five-row area omits "Copied" (its row is the next region's), a
    // six-row area draws it on row 5, and a four-column area clips it
    // to its width (`docs/tui.md`, "Selection and copy").
    for (area, want) in [
        (Rect::new(0, 0, 60, 5), None),
        (Rect::new(0, 0, 60, 6), Some(5)),
        (Rect::new(10, 2, 4, 6), Some(2)),
    ] {
        let mut app = replying(60, 24, "copy this line");
        app.on_key(Key::CtrlF, fakes::clock::FakeClock::new().now());
        assert!(app.find_bar().is_some(), "the bar is open");
        let shown = text(&buffer(&app, 60, 24));
        let row = u16::try_from(
            shown
                .lines()
                .position(|line| line.contains("copy this"))
                .expect("the reply"),
        )
        .expect("a row");
        select(&mut app, 60, 24, (0, row), (3, row));
        assert!(
            text(&buffer(&app, 60, 24)).contains("Copied"),
            "the drag copies"
        );
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 10));
        let mut targets = Vec::new();
        conversation_rows(&app, area, &mut buf, &mut targets);
        // "Copied" is the only `info` on `surface` here: its cells are
        // its row, and none sits outside the area.
        let marked: Vec<(u16, u16)> = (0..60)
            .flat_map(|x| (0..10).map(move |y| (x, y)))
            .filter(|(x, y)| {
                let cell = &buf[(*x, *y)];
                cell.fg == Role::Info.color() && cell.bg == Role::Surface.color()
            })
            .collect();
        match want {
            None => assert!(marked.is_empty(), "{area:?} omits Copied"),
            Some(y) => {
                assert!(!marked.is_empty(), "{area:?} draws Copied");
                for (x, cell_y) in &marked {
                    assert_eq!(*cell_y, y, "{area:?} draws on row {y}");
                    assert!(
                        area.contains(Position::new(*x, *cell_y)),
                        "{area:?} keeps ({x}, {cell_y})"
                    );
                }
            }
        }
    }
}

#[test]
fn copied_draws_nothing_on_a_zero_height_area() {
    // A conversation area with no rows draws no confirmation and takes
    // no target, even with `copied` set: there is no top row, and the
    // row guard omits the confirmation (`docs/tui.md`, "Selection and
    // copy"). The buffer around the area stays blank.
    let mut app = replying(60, 24, "copy this line");
    let shown = text(&buffer(&app, 60, 24));
    let row = u16::try_from(
        shown
            .lines()
            .position(|line| line.contains("copy this"))
            .expect("the reply"),
    )
    .expect("a row");
    select(&mut app, 60, 24, (0, row), (3, row));
    assert!(
        text(&buffer(&app, 60, 24)).contains("Copied"),
        "the drag copies"
    );
    let mut buf = Buffer::empty(Rect::new(0, 0, 60, 10));
    let mut targets = Vec::new();
    conversation_rows(&app, Rect::new(0, 0, 60, 0), &mut buf, &mut targets);
    assert!(targets.is_empty(), "no target on a zero-height area");
    for y in 0..10u16 {
        for x in 0..60u16 {
            assert_eq!(
                buf[(x, y)].bg,
                ratatui::style::Color::Reset,
                "({x}, {y}) stays blank"
            );
        }
    }
}

#[test]
fn a_zero_height_conversation_draws_no_pin() {
    // A conversation area with no rows pins nothing, even with the top
    // inside opened content: the height guard owns the pin, so widening
    // it pins row 0 of nothing (`docs/tui.md`, "Pinned call").
    let (mut app, _, call, _, first) = pinned_call(30, true);
    app.jump(first + 2);
    assert_eq!(
        app.pinned_row().map(|pin| pin.target),
        Some(call),
        "a pinned call"
    );
    let area = Rect::new(0, 0, 60, 0);
    let mut buf = Buffer::empty(Rect::new(0, 0, 60, 10));
    let mut targets = Vec::new();
    assert_eq!(
        conversation_rows(&app, area, &mut buf, &mut targets),
        None,
        "no pin from a zero-height area"
    );
}

#[test]
fn copied_needs_a_conversation_row_to_show_on() {
    let mut app = replying(30, 10, "```rust\nlet x = 1;\n```");
    let shown = text(&buffer(&app, 30, 10));
    let header = shown
        .lines()
        .position(|line| line.contains("rust"))
        .and_then(|at| u16::try_from(at).ok())
        .unwrap_or_default();
    click(&mut app, 30, 10, 27, header);
    assert!(app.copied());
    app.set_size(30, 1);
    assert_eq!(text(&buffer(&app, 30, 1)), "▌ › █\n");
}

#[test]
fn the_focused_target_is_drawn_reversed() {
    use serde_json::json;
    let mut app = empty();
    // Two small turns: the first with a group, completed, the second
    // open. Both fit with the first's group line above the focused turn,
    // outside it.
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "a"));
    app.on_line(at(
        "tool_call_requested",
        Some("a_1"),
        0,
        json!({"name": "read", "arguments": {"path": "src/a.rs"}}),
    ));
    app.on_line(at(
        "tool_call_completed",
        Some("a_1"),
        0,
        json!({"status": "completed",
            "content": [{"type": "text", "text": "fn a() {}"}]}),
    ));
    app.on_line(at(
        "turn_completed",
        None,
        0,
        json!({"outcome": "completed"}),
    ));
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "b"));
    app.on_line(at(
        "text_completed",
        Some("a_m"),
        0,
        json!({"text": "reply"}),
    ));
    app.on_key(Key::BackTab, fakes::clock::FakeClock::new().now());
    // The cards' edges add four rows, so both turns' stops fit on a
    // taller screen.
    let (buf, targets) = pointed(&mut app, WIDTH, 16, None);
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
    let mut app = empty();
    attach(&mut app, S_A);
    app.on_key(Key::F1, fakes::clock::FakeClock::new().now());
    let (buf, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    // The key map's ✕ sits on its title row's right end, closing it.
    let key_cross = targets
        .iter()
        .find(|target| target.id == TargetId::CloseOverlay)
        .expect("the key map draws its cross");
    assert_eq!(key_cross.rect.x, WIDTH - 3);
    assert_eq!(key_cross.rect.width, 1);
    let row: String = (0..WIDTH)
        .map(|x| buf[(x, key_cross.rect.y)].symbol().to_owned())
        .collect();
    assert!(row.contains("Key map"), "{row}");
    assert_eq!(
        buf.cell((WIDTH - 3, key_cross.rect.y))
            .map(|cell| cell.symbol()),
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
    // The notice overlay's ✕ sits on its title row's right end, closing it.
    let notice_cross = targets
        .iter()
        .find(|target| target.id == TargetId::CloseOverlay)
        .expect("the notice overlay draws its cross");
    let row: String = (0..WIDTH)
        .map(|x| buf[(x, notice_cross.rect.y)].symbol().to_owned())
        .collect();
    assert!(row.contains("Notice · x"), "{row}");
    assert_eq!(notice_cross.rect.width, 1);
    assert_eq!(
        buf.cell((notice_cross.rect.x, notice_cross.rect.y))
            .map(|cell| cell.symbol()),
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
fn a_handoff_note_row_keeps_its_sides_without_colour() {
    // The handoff tint takes the card's side bars when no colour is set
    // (`docs/tui.md`, "Look"): a note row sits on that tint alone.
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(80, 24);
    attach(&mut app, S_A);
    app.on_line(turn_started(S_A, "go"));
    app.on_line(session_line(
        S_A,
        "text_completed",
        serde_json::json!({"text": "Working."}),
        Some("a_1"),
    ));
    app.on_line(session_line(
        S_A,
        "handoff_started",
        serde_json::json!({"trigger": "auto"}),
        None,
    ));
    app.on_line(delta(S_A, "a_n", "## Note"));
    app.on_line(session_line(
        S_A,
        "text_completed",
        serde_json::json!({"text": "## Note\nkeep going"}),
        Some("a_n"),
    ));
    app.on_line(session_line(
        S_A,
        "handoff_completed",
        serde_json::json!({"outcome": "completed", "tokens_before": 1_000}),
        None,
    ));
    let note = app
        .targets()
        .into_iter()
        .find_map(|(_, target)| matches!(target, crate::app::Target::Note(_)).then_some(target));
    let Some(note) = note else {
        panic!("a note target: {:?}", app.lines());
    };
    app.open(note);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let row = (0..area.height)
        .find(|y| row_text(&buf, *y).contains("keep going"))
        .expect("a note row on screen");
    let text = row_text(&buf, row);
    assert!(text.contains('│'), "{text:?}");
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
    assert_eq!(turn.rect, Rect::new(0, 0, WIDTH - 1, 9));
}

#[test]
fn a_turn_past_the_last_row_is_clipped_to_it() {
    use crate::mouse::TargetId;
    let now = fakes::clock::FakeClock::new().now();
    let mut app = empty();
    tool_turn(&mut app);
    app.set_size(WIDTH, 6);
    app.on_key(Key::PageUp, now);
    app.on_line(turn_started(S_A, "next"));
    assert!(app.has_new());
    let (_, targets) = pointed(&mut app, WIDTH, 5, None);
    let turn = targets
        .iter()
        .find(|target| target.id == TargetId::Turn(0))
        .expect("turn 0 is drawn");
    assert_eq!(turn.rect, Rect::new(0, 0, WIDTH - 1, 1));
}

#[test]
fn input_with_an_image_token() {
    let mut app = empty();
    type_draft(&mut app, "look ");
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(
        app.on_key(Key::CtrlV, now),
        crate::app::Effect::ReadImage(0)
    );
    app.on_image(0, Ok("AAA".to_owned()));
    type_draft(&mut app, " here");
    for _ in 0..5 {
        app.on_edit(Edit::Left);
    }
    insta::assert_snapshot!("input_with_an_image_token", screen(&app));
    assert_eq!(cursor_at(&app), Some(Position::new(19, 10)));
}

/// An app on home with no session, as `/settings` finds it.
fn home_app() -> App {
    let mut app = empty();
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    app
}

#[test]
fn a_view_target_under_the_pointer_gets_the_hover_tint_alone() {
    let mut app = home_app();
    app.open_config_view(crate::app::ConfigView::Settings);
    let (hovered, _) = pointed(&mut app, WIDTH, HEIGHT, Some((WIDTH - 1, 0)));
    // The whole top row is the one close target, so the hover lifts
    // the whole row and no row under it (`docs/tui.md`, "Swapped views").
    let tint = Some(crate::theme::hover_bg(Role::Surface.color()));
    for x in 0..WIDTH {
        assert_eq!(
            hovered.cell((x, 0)).map(|cell| cell.bg),
            tint,
            "no hover tint at {x}"
        );
    }
    assert_ne!(hovered.cell((0, 1)).map(|cell| cell.bg), tint);
}

#[test]
fn a_focus_the_view_does_not_draw_is_not_drawn_on_it() {
    let mut app = home_app();
    let (_, targets) = pointed(&mut app, WIDTH, HEIGHT, None);
    app.drawn(&targets);
    app.on_key(Key::BackTab, fakes::clock::FakeClock::new().now());
    assert!(app.focused().is_some(), "a focused stop");
    app.open_config_view(crate::app::ConfigView::Settings);
    let (buf, _) = pointed(&mut app, WIDTH, HEIGHT, None);
    assert!(
        !buf.cell((WIDTH - 1, 0))
            .is_some_and(|cell| cell.modifier.contains(Modifier::REVERSED)),
        "the header's cross is not focused"
    );
}

/// Four questions for an `ask_user` call, within the tool's schema.
fn four_questions() -> serde_json::Value {
    serde_json::json!({"questions": [
        {"header": "Timeout", "question": "How long?",
         "options": [{"label": "1m"}, {"label": "5m"}]},
        {"header": "Scope", "question": "Which scope?",
         "options": [{"label": "a"}, {"label": "b"}]},
        {"header": "Name", "question": "What name?"},
        {"header": "Pick", "question": "Which one?",
         "options": [{"label": "x"}, {"label": "y"}]},
    ]})
}

/// The summary row holding `needle` in `buf`, trailing spaces trimmed.
fn summary_row(buf: &Buffer, needle: &str) -> String {
    (0..buf.area.height)
        .map(|y| row_text(buf, y).trim_end().to_owned())
        .find(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("no row holds {needle:?}\n{}", text(buf)))
}

#[test]
fn group_line_and_ledger_show_parsed_arguments_never_json() {
    use serde_json::json;
    const W: u16 = 160;
    const H: u16 = 24;
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(W, H);
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "go"));
    app.on_line(at("step_started", None, 0, json!({})));
    let arguments = four_questions();
    // While the call streams, the group line holds its raw text.
    app.on_line(at(
        "tool_call_arguments_delta",
        Some("a_m"),
        0,
        json!({"index": 0, "name": "ask_user", "text": arguments.to_string()}),
    ));
    let area = Rect::new(0, 0, W, H);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    assert!(text(&buf).contains("{\"questions\""), "{}", text(&buf));
    // Once requested, the parsed form draws as one row, never JSON.
    app.on_line(at(
        "tool_call_requested",
        Some("a_1"),
        0,
        json!({"name": "ask_user", "arguments": arguments}),
    ));
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let shown = text(&buf);
    assert!(shown.contains("ask_user 4 questions"), "{shown}");
    assert!(!shown.contains("{\""), "{shown}");
    let holding: Vec<String> = (0..H)
        .map(|y| row_text(&buf, y))
        .filter(|row| row.contains("4 questions"))
        .collect();
    assert_eq!(holding.len(), 1, "{shown}");
    // Ten calls in flight still draw one row, cut with "…".
    for n in 0..9 {
        app.on_line(at(
            "tool_call_requested",
            Some(format!("a_1{n}").as_str()),
            0,
            json!({"name": "read",
                "arguments": {"path": format!("src/some/long/file{n:02}.rs")}}),
        ));
    }
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let shown = text(&buf);
    assert!(shown.contains("ask_user 4 questions"), "{shown}");
    assert!(!shown.contains("file08.rs"), "{shown}");
    let row = summary_row(&buf, "ask_user 4 questions");
    assert!(row.contains('…'), "{row}");
    assert!(row.ends_with('▸'), "{row}");
    assert!(crate::format::width(&row) <= usize::from(W), "{row}");
    // At a narrow width the one row is cut there instead.
    app.set_size(40, H);
    let narrow = Rect::new(0, 0, 40, H);
    let mut buf = Buffer::empty(narrow);
    render(&app, narrow, &mut buf, None);
    let shown = text(&buf);
    let row = summary_row(&buf, "Read 9 files");
    assert!(row.contains('…'), "{row}");
    assert!(row.ends_with('▸'), "{row}");
    assert!(crate::format::width(&row) <= 40, "{row}");
    assert!(!shown.contains("{\""), "{shown}");
}

/// A finished call's ledger row shows its parsed summary, never JSON.
#[test]
fn ledger_rows_show_parsed_arguments_never_json() {
    use serde_json::json;
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "go"));
    app.on_line(at("step_started", None, 0, json!({})));
    for (action, name, arguments) in [
        ("a_1", "ask_user", four_questions()),
        ("a_2", "web_fetch", json!({"url": "https://example.com/x"})),
        ("a_3", "search", json!({"query": "rust errors"})),
    ] {
        app.on_line(at(
            "tool_call_requested",
            Some(action),
            0,
            json!({"name": name, "arguments": arguments}),
        ));
        app.on_line(at(
            "tool_call_completed",
            Some(action),
            0,
            json!({"status": "completed", "content": [{"type": "text", "text": "ok"}]}),
        ));
    }
    app.on_key(Key::CtrlO, fakes::clock::FakeClock::new().now());
    let shown = screen(&app);
    // The kind cells name the kind; the targets show the parsed form.
    assert!(shown.contains("ask      4 questions"), "{shown}");
    assert!(shown.contains("https://example.com/x"), "{shown}");
    assert!(shown.contains("rust errors"), "{shown}");
    assert!(!shown.contains("{\""), "{shown}");
}

#[test]
fn the_badge_warning_sign_is_split_from_the_rest_of_the_badge() {
    let attention = ratatui::style::Style::new().fg(crate::theme::Role::Attention.color());
    assert_eq!(
        super::badge_spans("⚠ 1 approvals waiting"),
        vec![
            ratatui::text::Span::styled("⚠", attention),
            ratatui::text::Span::styled(
                " 1 approvals waiting",
                attention.add_modifier(Modifier::BOLD),
            ),
        ]
    );
}

/// An app with one group holding one read call with `lines` lines of
/// output, the ledger and the call open. Returns the app, the group, the
/// call, the call row and the first opened content row. With `tail`, a
/// reply with a code block follows the turn.
fn pinned_call(
    lines: usize,
    tail: bool,
) -> (App, crate::app::Target, crate::app::Target, usize, usize) {
    use crate::app::Target as AppTarget;
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "go"));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "assistant_message_started",
        serde_json::json!({}),
        Some("a_m"),
    ));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "tool_call_requested",
        serde_json::json!({"name": "read", "arguments": {"path": "src/0.rs"}}),
        Some("a_c0"),
    ));
    let detail: Vec<String> = (1..=lines).map(|n| format!("L{n:04} of call")).collect();
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "tool_call_completed",
        serde_json::json!({"status": "completed",
            "content": [{"type": "text", "text": detail.join("\n")}]}),
        Some("a_c0"),
    ));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "assistant_message_completed",
        serde_json::json!({"outcome": "completed"}),
        Some("a_m"),
    ));
    app.on_line(turn_completed("s_aaaaaaaaaaaaaaaa", "completed"));
    if tail {
        // A tail reply with a code block, for the copy test.
        app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "tip"));
        app.on_line(session_line(
            "s_aaaaaaaaaaaaaaaa",
            "text_completed",
            serde_json::json!({"text": "Run it:\n\n```sh\necho hi\n```"}),
            Some("a_n"),
        ));
        app.on_line(turn_completed("s_aaaaaaaaaaaaaaaa", "completed"));
    }
    let group = app
        .targets()
        .into_iter()
        .find_map(|(_, target)| matches!(target, AppTarget::Group(_)).then_some(target))
        .expect("a group target");
    app.open(group);
    let call = app
        .targets()
        .into_iter()
        .find_map(|(_, target)| matches!(target, AppTarget::Call(_)).then_some(target))
        .expect("a call target");
    app.open(call);
    let (rows, texts) = app.pages().page_text(0).expect("the resident page");
    let width = app.pages().wrap_width();
    let mut row = 0usize;
    let mut header = None;
    let mut first = None;
    for ((line, target), text) in rows.iter().zip(&texts) {
        if *target == Some(call) {
            header = Some(row);
        }
        if first.is_none() && text.scopes.last() == Some(&call) {
            first = Some(row);
        }
        row += crate::view::rows(line.clone(), width);
    }
    let header = header.expect("the call row");
    let first = first.expect("opened rows");
    assert_eq!(first, header + 1, "the opened content follows the call row");
    (app, group, call, header, first)
}

/// The conversation rows area: the gutter dropped and split from the
/// bar column, exactly as the render path computes it (`view.rs`).
fn pin_rows(app: &App) -> Rect {
    let inset = crate::layout::past_gutter(app.conversation_area(), app.chrome().gutter());
    let (rows, _) = crate::view::scroll_bar::split(inset);
    rows
}

#[test]
fn the_pinned_call_row_draws_on_the_first_row_as_the_row_it_stands_for() {
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, group, call, header, first) = pinned_call(30, true);
    app.jump(first + 2);
    let pin = app.pinned_row().expect("a pinned call");
    assert_eq!(pin.target, call, "the call pins");
    let rows = pin_rows(&app);
    assert_eq!(rows.y, 0, "the pin sits on the first row");
    let (pinned_buf, _) = pointed(&mut app, W, H, None);
    // One row up the summary itself is the top row, so nothing pins,
    // and the call's own row shows below it for reference.
    app.jump(header - 1);
    assert!(app.pinned_row().is_none(), "the summary pins nothing");
    let _ = group;
    let (open_buf, _) = pointed(&mut app, W, H, None);
    let at = rows.y + 1;
    assert!(
        row_text(&open_buf, at).contains("src/0.rs"),
        "the call row shows below the summary: {:?}",
        row_text(&open_buf, at)
    );
    for x in rows.x..rows.right() {
        let (pinned, open) = (&pinned_buf[(x, rows.y)], &open_buf[(x, at)]);
        assert_eq!(
            pinned.symbol(),
            open.symbol(),
            "the same cell at column {x}"
        );
        assert_eq!(pinned.bg, open.bg, "the same background at column {x}");
    }
}

#[test]
fn the_pinned_row_keeps_the_cards_margin_and_leaves_the_bar_column_free() {
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, _, call, _, first) = pinned_call(30, true);
    app.jump(first + 2);
    assert!(app.pinned_row().is_some(), "a pinned call");
    let rows = pin_rows(&app);
    let (buf, _) = pointed(&mut app, W, H, None);
    // The card-pad column left of the rows stays blank.
    assert_eq!(buf[(rows.x.saturating_sub(1), rows.y)].symbol(), " ");
    // The bar column keeps the bar's cell: a thumb or a blank.
    assert!(
        [" ", "┃"].contains(&buf[(rows.right(), rows.y)].symbol()),
        "the bar column is the bar's: {:?}",
        row_text(&buf, rows.y)
    );
    let _ = call;
}

#[test]
fn the_pinned_row_is_a_line_target_over_row_zero_and_wins_the_hit() {
    use crate::mouse::TargetId;
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, _, call, _, first) = pinned_call(30, true);
    app.jump(first + 2);
    let rows = pin_rows(&app);
    let (_, targets) = pointed(&mut app, W, H, None);
    let drawn: Vec<(crate::app::Target, Rect)> = lines(&targets);
    let pin = *drawn.last().expect("a last line target");
    assert_eq!(
        pin,
        (call, Rect::new(rows.x, rows.y, rows.width, 1)),
        "the pin is the last line target, over row zero"
    );
    assert_eq!(
        crate::mouse::hit(&targets, rows.x, rows.y),
        Some(TargetId::Line(call)),
        "the pin wins the row-zero hit"
    );
}

#[test]
fn hover_lifts_the_pinned_call_row_like_any_target() {
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, _, _, _, first) = pinned_call(30, true);
    app.jump(first + 2);
    let rows = pin_rows(&app);
    let (plain, _) = pointed(&mut app, W, H, None);
    let (hovered, _) = pointed(&mut app, W, H, Some((rows.x + 2, rows.y)));
    let mut lifted = 0;
    for x in rows.x..rows.right() {
        let (was, is) = (plain[(x, rows.y)].bg, hovered[(x, rows.y)].bg);
        if plain[(x, rows.y)].symbol() != " " {
            assert_ne!(is, was, "a hovered cell lifts at column {x}");
            assert_eq!(is, hovered[(rows.x, rows.y)].bg, "the lift is uniform");
            lifted += 1;
        }
    }
    assert!(lifted > 0, "the pin row holds text to lift");
}

#[test]
fn a_row_that_is_not_pinned_is_not_drawn_pinned() {
    use crate::app::Target as AppTarget;
    const W: u16 = 60;
    const H: u16 = 12;
    // (a) The top row inside a closed ledger's area: nothing pins.
    let (mut app, group, call, header, first) = pinned_call(30, true);
    app.open(call);
    app.open(group);
    let _ = (call, first);
    let closed = app
        .pages()
        .page_text(0)
        .map(|(rows, _)| {
            rows.iter()
                .position(|(_, target)| *target == Some(group))
                .expect("the closed group row")
        })
        .expect("the resident page");
    app.jump(closed);
    assert!(app.pinned_row().is_none(), "a closed ledger pins nothing");
    let rows = pin_rows(&app);
    let (buf, targets) = pointed(&mut app, W, H, None);
    assert!(
        row_text(&buf, rows.y).contains('▸'),
        "row 0 is the covered summary: {:?}",
        row_text(&buf, rows.y)
    );
    let drawn: Vec<(AppTarget, Rect)> = lines(&targets);
    assert_eq!(
        drawn
            .iter()
            .filter(|(_, rect)| rect.y == rows.y)
            .map(|(target, _)| target)
            .collect::<Vec<_>>(),
        vec![&group],
        "only the row's own target sits at y 0"
    );
    // (b) The top row on an opened call's own row: the group pins, and
    // the call's row does not.
    app.open(group);
    app.open(call);
    app.jump(header);
    let pin = app.pinned_row().expect("the group pins");
    assert_eq!(pin.target, group, "the summary pins, not the call");
    let (buf, targets) = pointed(&mut app, W, H, None);
    assert!(
        row_text(&buf, rows.y).contains('▾'),
        "row 0 is the group summary: {:?}",
        row_text(&buf, rows.y)
    );
    let drawn: Vec<(AppTarget, Rect)> = lines(&targets);
    let at_zero: Vec<AppTarget> = drawn
        .iter()
        .filter(|(_, rect)| rect.y == rows.y)
        .map(|(target, _)| *target)
        .collect();
    assert_eq!(at_zero, vec![group], "only the summary pins row 0");
    // (c) The top row after the call's block: nothing pins, and row 0 is
    // the covered row.
    app.jump(first + 21);
    assert!(app.pinned_row().is_none(), "past the block pins nothing");
    let (buf, targets) = pointed(&mut app, W, H, None);
    let covered = row_text(&buf, rows.y);
    assert!(
        !covered.contains("src/0.rs"),
        "row 0 is past the call: {covered:?}"
    );
    let drawn: Vec<(AppTarget, Rect)> = lines(&targets);
    assert!(
        drawn.iter().all(|(_, rect)| rect.y != rows.y),
        "no line target sits at y 0"
    );
}

#[test]
fn following_draws_no_pin() {
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, _, _, _, first) = pinned_call(30, true);
    app.jump(first + 2);
    assert!(app.pinned_row().is_some(), "a pinned call");
    app.on_key(Key::End, fakes::clock::FakeClock::new().now());
    assert_eq!(app.top(), None, "following pins nothing");
    assert!(app.pinned_row().is_none(), "no pin while following");
    let rows = pin_rows(&app);
    let (buf, targets) = pointed(&mut app, W, H, None);
    // Row 0 is the window's own first row: the pin does not cover it.
    let (total, conv) = (app.scroll().1, app.conversation_height());
    let (page, _) = app.pages().page_text(0).expect("the resident page");
    let width = app.pages().wrap_width();
    let mut row = 0usize;
    let mut covered = None;
    for (line, target) in &page {
        let height = crate::view::rows(line.clone(), width);
        if row + height > total.saturating_sub(conv) && covered.is_none() {
            covered = Some((line.to_string(), *target));
        }
        row += height;
    }
    let (text, target) = covered.expect("a covered first row");
    assert_eq!(
        row_text(&buf, rows.y),
        format!("{text:<width$}", width = usize::from(buf.area.width))
    );
    let drawn: Vec<(crate::app::Target, Rect)> = lines(&targets);
    assert!(
        drawn
            .iter()
            .filter(|(_, rect)| rect.y == rows.y)
            .all(|(line_target, _)| Some(*line_target) == target),
        "only the covered row's own target sits at y 0"
    );
}

#[test]
fn rows_above_the_top_take_no_zero_height_target() {
    // The lines above the top draw no rows: their line and turn targets
    // take no hit, not even a zero-height one. Widening either height
    // guard pushes one (`docs/tui.md`, "Pinned call").
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, _, _, _, _) = pinned_call(30, true);
    // The tail reply below the opened block: a plain row on top, so no
    // pin, and no new output, so no pill.
    let width = app.pages().wrap_width();
    let (page, _) = app.pages().page_text(0).expect("the resident page");
    let mut row = 0usize;
    let mut tail = None;
    for (line, _) in &page {
        if line.to_string().contains("Run it") {
            tail = Some(row);
            break;
        }
        row += crate::view::rows(line.clone(), width);
    }
    let tail = tail.expect("the tail reply");
    app.jump(tail);
    assert!(app.pinned_row().is_none(), "a plain row pins nothing");
    let (_, targets) = pointed(&mut app, W, H, None);
    let empty: Vec<Rect> = targets
        .iter()
        .map(|target| target.rect)
        .filter(|rect| rect.height == 0)
        .collect();
    assert!(empty.is_empty(), "no zero-height target: {empty:?}");
}

#[test]
fn a_narrow_screen_still_pins_one_row() {
    const W: u16 = 40;
    const H: u16 = 12;
    let (mut app, _, call, _, _) = pinned_call(30, true);
    app.set_size(W, H);
    // The rows shift when the width wraps them: find the opened content
    // at this width.
    let width = app.pages().wrap_width();
    let (rows, texts) = app.pages().page_text(0).expect("the resident page");
    let mut row = 0usize;
    let mut first = None;
    for ((line, _), text) in rows.iter().zip(&texts) {
        if first.is_none() && text.scopes.last() == Some(&call) {
            first = Some(row);
        }
        row += crate::view::rows(line.clone(), width);
    }
    let first = first.expect("opened rows");
    let _ = call;
    app.jump(first + 2);
    assert!(app.pinned_row().is_some(), "a pinned call");
    let (buf, _) = pointed(&mut app, W, H, None);
    let rows = pin_rows(&app);
    assert!(
        row_text(&buf, rows.y).contains("read"),
        "row 0 keeps the call row: {:?}",
        row_text(&buf, rows.y)
    );
    // The narrow call row wraps: the pin still takes one drawn row, so
    // row 1 is the window row after the covered top row, and the header's
    // wrapped rest never leaks onto it.
    let top = app.top().expect("a scrolled top");
    let (rows_page, _) = app.pages().page_text(0).expect("the resident page");
    let width = app.pages().wrap_width();
    let mut row = 0usize;
    let mut covered = None;
    for (line, _) in &rows_page {
        let height = crate::view::rows(line.clone(), width);
        if row + height > top + 1 && covered.is_none() {
            covered = Some(line.to_string());
        }
        row += height;
    }
    let covered = covered.expect("the row after the top");
    assert!(
        row_text(&buf, rows.y + 1).contains(covered.trim()),
        "row 1 is the row after the top: {:?}",
        row_text(&buf, rows.y + 1)
    );
}

#[test]
fn no_colour_pin_keeps_the_card_sides() {
    const W: u16 = 60;
    const H: u16 = 12;
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let (mut app, _, _, _, first) = pinned_call(30, true);
    app.jump(first + 2);
    assert!(app.pinned_row().is_some(), "a pinned call");
    let rows = pin_rows(&app);
    let (buf, _) = pointed(&mut app, W, H, None);
    // The pin's row takes the card's thin-box sides, as the other card
    // rows do.
    for y in [rows.y, rows.y + 1] {
        assert_eq!(buf[(rows.x, y)].symbol(), "│", "a left side on row {y}");
        assert_eq!(
            buf[(rows.right().saturating_sub(1), y)].symbol(),
            "│",
            "a right side on row {y}"
        );
    }
    crate::surface::init(&|_| None);
}

#[test]
fn every_card_tint_draws_the_thin_box_sides() {
    // The pin draws through `draw_row`, and the row loop shares its tint
    // check: a turn, surface or handoff background draws the thin-box
    // sides without colour, so flipping any arm of the check drops a side
    // here (`docs/tui.md`, "Look").
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    for (name, role) in [
        ("turn", Role::Turn),
        ("surface", Role::Surface),
        ("handoff", Role::Handoff),
    ] {
        let line = ratatui::text::Line::styled("tinted", Style::new().bg(role.color()));
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        draw_row(line, Rect::new(0, 0, 20, 1), &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "│", "{name} keeps its left side");
        assert_eq!(buf[(19, 0)].symbol(), "│", "{name} keeps its right side");
    }
    crate::surface::init(&|_| None);
}

#[test]
fn a_selected_pinned_row_keeps_the_cards_tint() {
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, _, _, _, first) = pinned_call(30, true);
    app.jump(first + 2);
    let rows = pin_rows(&app);
    let (plain, _) = pointed(&mut app, W, H, None);
    select(&mut app, W, H, (0, 0), (20, 2));
    // The drag copies, and " Copied " yields the pin: clear the flag so
    // the selection stays and the pin draws again.
    app.clear_copied();
    let (selected, _) = pointed(&mut app, W, H, None);
    // The pin draws after the selection marks: every cell of row 0 keeps
    // the card tint it had without the selection.
    for x in rows.x..rows.right() {
        assert_eq!(
            selected[(x, rows.y)].bg,
            plain[(x, rows.y)].bg,
            "the pin keeps its tint at column {x}"
        );
    }
}

#[test]
fn the_pin_yields_to_the_new_messages_pill_and_copied_label() {
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, _, call, _, first) = pinned_call(30, true);
    app.jump(first + 2);
    // New output while scrolled up raises the pill on the last row, and
    // the pin stays on the first.
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "more"));
    let (buf, _) = pointed(&mut app, W, H, None);
    let rows = pin_rows(&app);
    assert!(
        row_text(&buf, rows.y).contains("src/0.rs"),
        "the pin stays: {:?}",
        row_text(&buf, rows.y)
    );
    assert!(
        row_text(&buf, rows.y + rows.height - 1).contains("New messages below"),
        "the pill keeps its row: {:?}",
        row_text(&buf, rows.y + rows.height - 1)
    );
    // A copy puts " Copied " on the top row: the pin yields, and the
    // covered row shows.
    let copy_row = app
        .targets()
        .into_iter()
        .find_map(|(row, target)| matches!(target, crate::app::Target::Copy { .. }).then_some(row))
        .expect("a copy target in the tail");
    app.jump(copy_row);
    let (_, targets) = pointed(&mut app, W, H, None);
    let copy = targets
        .iter()
        .find(|target| {
            matches!(
                target.id,
                crate::mouse::TargetId::Line(crate::app::Target::Copy { .. })
            )
        })
        .expect("the drawn copy target");
    let (col, row) = (copy.rect.x, copy.rect.y);
    click(&mut app, W, H, col, row);
    assert!(app.copied(), "the copy lands");
    app.jump(first + 2);
    let (buf, targets) = pointed(&mut app, W, H, None);
    assert!(
        row_text(&buf, rows.y).contains("Copied"),
        "the copied label keeps its cells: {:?}",
        row_text(&buf, rows.y)
    );
    assert!(
        !row_text(&buf, rows.y).contains("src/0.rs"),
        "the pin yields to the label: {:?}",
        row_text(&buf, rows.y)
    );
    let drawn: Vec<(crate::app::Target, Rect)> = lines(&targets);
    assert!(
        drawn.iter().all(|(_, rect)| rect.y != rows.y),
        "no pin covers the copied row"
    );
    let _ = call;
}

/// The group summary's line, for the open-marker assertions.
fn summary_text(app: &App, group: crate::app::Target) -> String {
    let (rows, _) = app.pages().page_text(0).expect("the resident page");
    rows.into_iter()
        .find(|(_, target)| *target == Some(group))
        .map(|(line, _)| line.to_string())
        .expect("the summary line")
}

#[test]
fn clicking_the_pinned_call_closes_it_and_puts_its_row_on_the_top_row() {
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, group, call, header, first) = pinned_call(30, true);
    app.jump(first + 2);
    assert_eq!(
        app.pinned_row().map(|pin| pin.target),
        Some(call),
        "the call pins"
    );
    let rows = pin_rows(&app);
    click(&mut app, W, H, rows.x, rows.y);
    // The call closed and its row jumped to the top row, where the open
    // ledger's summary pins over it.
    assert!(
        app.targets()
            .into_iter()
            .all(|(_, target)| !matches!(target, crate::app::Target::CallView(_))),
        "the opened rows are gone"
    );
    assert_eq!(app.top(), Some(header), "its row is the top row");
    assert_eq!(
        app.pinned_row().map(|pin| pin.target),
        Some(group),
        "the summary pins the landed row"
    );
    let (buf, targets) = pointed(&mut app, W, H, None);
    assert!(
        row_text(&buf, rows.y).contains('▾'),
        "the pin shows the summary: {:?}",
        row_text(&buf, rows.y)
    );
    let drawn: Vec<(crate::app::Target, Rect)> = lines(&targets);
    assert!(
        drawn
            .iter()
            .filter(|(_, rect)| rect.y == rows.y)
            .all(|(target, _)| *target == group),
        "the covered row skips its hit"
    );
}

#[test]
fn clicking_the_pinned_group_closes_the_ledger_and_puts_the_group_line_on_the_top_row() {
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, group, call, header, _) = pinned_call(30, true);
    let _ = call;
    // The call closed, the top row on its row: the summary pins.
    app.open(call);
    app.jump(header);
    assert_eq!(
        app.pinned_row().map(|pin| pin.target),
        Some(group),
        "the summary pins"
    );
    let rows = pin_rows(&app);
    click(&mut app, W, H, rows.x, rows.y);
    // The ledger closed and its line jumped to the top row.
    let summary = summary_text(&app, group);
    assert!(summary.contains('▸'), "the ledger closed: {summary:?}");
    let (buf, _) = pointed(&mut app, W, H, None);
    assert!(
        row_text(&buf, rows.y).contains('▸'),
        "its line draws on row 0: {:?}",
        row_text(&buf, rows.y)
    );
}

#[test]
fn clicking_the_pinned_call_inside_a_ledger_closes_only_the_call() {
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, group, call, header, first) = pinned_call(30, true);
    app.jump(first + 2);
    let rows = pin_rows(&app);
    click(&mut app, W, H, rows.x, rows.y);
    assert_eq!(app.top(), Some(header), "its row is the top row");
    // The ledger stays open around the closed call.
    assert!(
        summary_text(&app, group).contains('▾'),
        "the ledger stays open"
    );
    assert!(
        app.targets().into_iter().any(|(_, target)| target == call),
        "the call row stays"
    );
    assert!(
        app.targets()
            .into_iter()
            .all(|(_, target)| !matches!(target, crate::app::Target::CallView(_))),
        "its opened rows are gone"
    );
}

#[test]
fn a_closed_row_near_the_bottom_clamps_like_settle() {
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, _, call, header, first) = pinned_call(30, false);
    app.jump(first + 2);
    assert_eq!(
        app.pinned_row().map(|pin| pin.target),
        Some(call),
        "the call pins"
    );
    let rows = pin_rows(&app);
    click(&mut app, W, H, rows.x, rows.y);
    // Closing leaves the header within the last screenful, so settle
    // clamps the top to the bottom: the header is not on row 0.
    let (top, total) = app.scroll();
    assert_eq!(top, total.saturating_sub(app.conversation_height()));
    assert!(
        header > top,
        "the header sits below the top: {header} > {top}"
    );
    let (buf, _) = pointed(&mut app, W, H, None);
    let at = (0..H)
        .find(|y| row_text(&buf, *y).contains("src/0.rs"))
        .expect("the header on screen");
    assert!(at > rows.y, "its row draws below row 0");
}

#[test]
fn an_ordinary_click_on_a_visible_call_row_still_toggles_without_scrolling() {
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, _, call, header, _) = pinned_call(30, true);
    // Scrolled so the call row shows below the summary's pin.
    app.jump(header - 1);
    let rows = pin_rows(&app);
    let (buf, _) = pointed(&mut app, W, H, None);
    let at = rows.y + 1;
    assert!(
        row_text(&buf, at).contains("src/0.rs"),
        "the call row shows: {:?}",
        row_text(&buf, at)
    );
    click(&mut app, W, H, rows.x, at);
    // The visible row toggles, and the scroll does not move.
    assert!(
        app.targets()
            .into_iter()
            .all(|(_, target)| !matches!(target, crate::app::Target::CallView(_))),
        "the call toggled"
    );
    assert_eq!(app.top(), Some(header - 1), "the scroll does not move");
    let _ = call;
}

#[test]
fn ctrl_o_keeps_its_meaning_while_pinned() {
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, _, call, _, first) = pinned_call(30, true);
    app.jump(first + 2);
    let rows = pin_rows(&app);
    let (buf, _) = pointed(&mut app, W, H, None);
    assert!(
        row_text(&buf, rows.y).contains("src/0.rs"),
        "the pin draws before the key"
    );
    let top = app.top().expect("a scrolled top");
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::CtrlO, now);
    // The ledgers close with any pinned call: the next frame pins
    // nothing, and the top only moves when settle clamps it.
    assert!(app.pinned_row().is_none(), "no pin with closed ledgers");
    assert!(
        app.top().is_none_or(|next| next <= top),
        "the top is unchanged or clamped"
    );
    app.on_key(Key::CtrlO, now);
    assert_eq!(
        app.pinned_row().map(|pin| pin.target),
        Some(call),
        "reopening pins the call again"
    );
}

#[test]
fn has_new_still_marks_while_scrolled_up_after_the_closing_click() {
    const W: u16 = 60;
    const H: u16 = 12;
    let (mut app, group, call, header, first) = pinned_call(30, true);
    app.jump(first + 2);
    let rows = pin_rows(&app);
    click(&mut app, W, H, rows.x, rows.y);
    assert_eq!(app.top(), Some(header), "its row is the top row");
    // New output while scrolled up raises the pill, and the group pins
    // the summary on the first row.
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "later"));
    assert_eq!(
        app.pinned_row().map(|pin| pin.target),
        Some(group),
        "the summary pins"
    );
    let (buf, _) = pointed(&mut app, W, H, None);
    assert!(
        row_text(&buf, rows.y).contains('▾'),
        "the pin shares the frame: {:?}",
        row_text(&buf, rows.y)
    );
    assert!(
        row_text(&buf, rows.y + rows.height - 1).contains("New messages below"),
        "the pill shares the frame: {:?}",
        row_text(&buf, rows.y + rows.height - 1)
    );
    let _ = call;
}

/// An app with a plain reply turn above a group turn holding one read
/// call with `lines` lines of output, the ledger and the call open.
/// Returns the app, the group, the call, the call row and the first
/// opened content row.
fn pinned_call_with_prior_turn(
    lines: usize,
) -> (App, crate::app::Target, crate::app::Target, usize, usize) {
    use crate::app::Target as AppTarget;
    let mut app = empty();
    attach(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "first"));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "text_completed",
        serde_json::json!({"text": "before"}),
        Some("a_p"),
    ));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "assistant_message_completed",
        serde_json::json!({"outcome": "completed"}),
        Some("a_p"),
    ));
    app.on_line(turn_completed("s_aaaaaaaaaaaaaaaa", "completed"));
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "go"));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "assistant_message_started",
        serde_json::json!({}),
        Some("a_m"),
    ));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "tool_call_requested",
        serde_json::json!({"name": "read", "arguments": {"path": "src/0.rs"}}),
        Some("a_c0"),
    ));
    let detail: Vec<String> = (1..=lines).map(|n| format!("L{n:04} of call")).collect();
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "tool_call_completed",
        serde_json::json!({"status": "completed",
            "content": [{"type": "text", "text": detail.join("\n")}]}),
        Some("a_c0"),
    ));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "assistant_message_completed",
        serde_json::json!({"outcome": "completed"}),
        Some("a_m"),
    ));
    app.on_line(turn_completed("s_aaaaaaaaaaaaaaaa", "completed"));
    let group = app
        .targets()
        .into_iter()
        .find_map(|(_, target)| matches!(target, AppTarget::Group(_)).then_some(target))
        .expect("a group target");
    app.open(group);
    let call = app
        .targets()
        .into_iter()
        .find_map(|(_, target)| matches!(target, AppTarget::Call(_)).then_some(target))
        .expect("a call target");
    app.open(call);
    let (rows, texts) = app.pages().page_text(0).expect("the resident page");
    let width = app.pages().wrap_width();
    let mut row = 0usize;
    let mut header = None;
    let mut first = None;
    for ((line, target), text) in rows.iter().zip(&texts) {
        if *target == Some(call) {
            header = Some(row);
        }
        if first.is_none() && text.scopes.last() == Some(&call) {
            first = Some(row);
        }
        row += crate::view::rows(line.clone(), width);
    }
    let header = header.expect("the call row");
    let first = first.expect("opened rows");
    assert_eq!(first, header + 1, "the opened content follows the call row");
    (app, group, call, header, first)
}

#[test]
fn a_turn_wholly_above_the_top_draws_no_turn_stop_while_pinned() {
    use crate::mouse::TargetId;
    const W: u16 = 60;
    const H: u16 = 12;
    // The earlier turn sits wholly above the top row, so its stop would
    // cover no rows: only a widened bound draws it (`docs/tui.md`,
    // "Pinned call").
    let (mut app, _, _, _, first) = pinned_call_with_prior_turn(30);
    app.jump(first + 2);
    assert!(app.pinned_row().is_some(), "a pinned call");
    let (_, targets) = pointed(&mut app, W, H, None);
    let turns: Vec<Rect> = targets
        .iter()
        .filter(|target| matches!(target.id, TargetId::Turn(_)))
        .map(|target| target.rect)
        .collect();
    assert!(!turns.is_empty(), "the turns loop drew a stop");
    assert!(
        turns.iter().all(|rect| rect.height > 0),
        "no zero-height turn stop: {turns:?}"
    );
}

#[test]
fn the_pinned_turn_keeps_its_stop_below_the_top_row() {
    use crate::mouse::TargetId;
    const W: u16 = 60;
    const H: u16 = 12;
    // The pin covers the top row, not the turn under it: the turn's stop
    // still spans the rows below, so only a flipped row check drops it
    // (`docs/tui.md`, "Pinned call").
    let (mut app, _, _, _, first) = pinned_call(30, true);
    app.jump(first + 2);
    assert!(app.pinned_row().is_some(), "a pinned call");
    let rows = pin_rows(&app);
    let (_, targets) = pointed(&mut app, W, H, None);
    let stop = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::Turn(_)))
        .expect("the pinned turn keeps a stop");
    assert_eq!(stop.rect.y, rows.y, "the stop starts on the top row");
    assert!(
        stop.rect.height > 1,
        "the stop spans below the top: {:?}",
        stop.rect
    );
}

/// An unshifted Home or End stroke.
fn home_end(code: crate::stroke::Code) -> crate::stroke::Stroke {
    crate::stroke::Stroke {
        code,
        mods: crate::stroke::Mods::NONE,
    }
}

/// Presses and releases with no movement through the full mouse path,
/// returning the release's effect: a click's effect wins, and a press
/// with no drag selects nothing.
fn click_at(app: &mut App, width: u16, height: u16, col: u16, row: u16) -> crate::app::Effect {
    use crate::keys::{Button, Mouse, MouseKind};
    use crate::mouse::Pointer;
    let now = fakes::clock::FakeClock::new().now();
    let mut pointer = Pointer::default();
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = render(app, area, &mut buf, None);
    let _ = app.on_mouse(
        &Mouse {
            kind: MouseKind::Press(Button::Left),
            col,
            row,
        },
        &targets,
        &mut pointer,
        true,
        now,
    );
    let mut buf = Buffer::empty(area);
    let targets = render(app, area, &mut buf, None);
    app.on_mouse(
        &Mouse {
            kind: MouseKind::Release,
            col,
            row,
        },
        &targets,
        &mut pointer,
        true,
        now,
    )
}

/// Drags from `from` to `to` through the full mouse path, returning the
/// release's effect: the selection's copy, and no click.
fn drag_at(
    app: &mut App,
    width: u16,
    height: u16,
    from: (u16, u16),
    to: (u16, u16),
) -> crate::app::Effect {
    use crate::keys::{Button, Mouse, MouseKind};
    use crate::mouse::Pointer;
    let now = fakes::clock::FakeClock::new().now();
    let mut pointer = Pointer::default();
    let area = Rect::new(0, 0, width, height);
    let mut report = |kind: MouseKind, (col, row): (u16, u16)| {
        let mut buf = Buffer::empty(area);
        let targets = render(app, area, &mut buf, None);
        app.on_mouse(&Mouse { kind, col, row }, &targets, &mut pointer, true, now)
    };
    let _ = report(MouseKind::Press(Button::Left), from);
    let _ = report(MouseKind::Drag(Button::Left), to);
    report(MouseKind::Release, to)
}

/// The screen cell where `needle` first shows on a `width` by `height`
/// screen.
fn cell_at(app: &mut App, width: u16, height: u16, needle: &str) -> (u16, u16) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    for y in 0..height {
        let row: String = (0..width)
            .map(|x| buf[(x, y)].symbol().to_owned())
            .collect();
        if let Some(byte) = row.find(needle) {
            let col = row.get(..byte).map_or(0, |before| before.chars().count());
            return (u16::try_from(col).expect("a column"), y);
        }
    }
    panic!("{needle:?} is not on screen");
}

#[test]
fn home_and_end_collapse_the_input_selection() {
    let mut app = asked();
    type_draft(&mut app, "zzqx");
    let at = cell_at(&mut app, WIDTH, HEIGHT, "zzqx");
    select(&mut app, WIDTH, HEIGHT, at, (at.0 + 4, at.1));
    assert_eq!(app.input().selected(), Some((0, 4)));
    app.on_press(
        home_end(crate::stroke::Code::Home),
        fakes::clock::FakeClock::new().now(),
    );
    assert_eq!(app.input().selected(), None);
    assert_eq!(app.input().position(), 0);
    select(&mut app, WIDTH, HEIGHT, at, (at.0 + 4, at.1));
    app.on_press(
        home_end(crate::stroke::Code::End),
        fakes::clock::FakeClock::new().now(),
    );
    assert_eq!(app.input().selected(), None);
    assert_eq!(app.input().position(), 4);
}

#[test]
fn left_after_an_input_drag_clears_at_the_start() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = asked();
    type_draft(&mut app, "zzqx");
    let at = cell_at(&mut app, WIDTH, HEIGHT, "zzqx");
    select(&mut app, WIDTH, HEIGHT, at, (at.0 + 4, at.1));
    assert_eq!(app.input().selected(), Some((0, 4)));
    app.on_edit(Edit::Left);
    assert_eq!(app.input().selected(), None);
    assert_eq!(app.input().position(), 0);
    let _ = now;
}

#[test]
fn a_click_in_the_input_box_places_the_cursor() {
    let mut app = asked();
    type_draft(&mut app, "zzqx");
    let at = cell_at(&mut app, WIDTH, HEIGHT, "zzqx");
    select(&mut app, WIDTH, HEIGHT, at, (at.0 + 4, at.1));
    assert_eq!(app.input().selected(), Some((0, 4)));
    // A press-and-release with no movement is a click: the selection
    // clears and the cursor lands on the clicked piece.
    let _ = click_at(&mut app, WIDTH, HEIGHT, at.0 + 1, at.1);
    assert_eq!(app.input().selected(), None);
    assert_eq!(app.input().position(), 1);
}

#[test]
fn a_click_without_movement_still_activates_its_target() {
    use crate::mouse::TargetId;
    let mut app = badged();
    let (_, targets) = pointed(&mut app, 40, 12, None);
    let badge = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::Badge))
        .expect("the badge draws a target");
    let (col, row) = (badge.rect.x, badge.rect.y);
    let _ = click_at(&mut app, 40, 12, col, row);
    assert!(app.panel().is_some(), "the click opens the approval panel");
    assert!(
        app.selection_cells(crate::app::SurfaceId::Conversation, app.conversation_area())
            .is_empty()
    );
    assert!(app.input().selected().is_none());
}

#[test]
fn a_drag_from_a_link_selects_and_clicks_nothing() {
    use crate::app::Effect;
    let mut app = replying(WIDTH, HEIGHT, "see [hi](http://example.com/x) now");
    let at = cell_at(&mut app, WIDTH, HEIGHT, "hi");
    // A click without movement follows the link: it copies the URL.
    let clicked = click_at(&mut app, WIDTH, HEIGHT, at.0, at.1);
    assert_eq!(clicked, Effect::Copy("http://example.com/x".to_owned()));
    // A drag from the same link selects instead: the copy holds the
    // selected text, never the link's URL, and the click never fires.
    let effect = drag_at(&mut app, WIDTH, HEIGHT, at, (at.0 + 1, at.1));
    assert!(
        matches!(&effect, Effect::Copy(text) if text.contains("hi") && !text.contains("example.com")),
        "{effect:?}"
    );
}

#[test]
fn an_input_drag_then_a_typed_char_replaces_the_selection() {
    use crate::app::Effect;
    let mut app = asked();
    type_draft(&mut app, "zzqx");
    let at = cell_at(&mut app, WIDTH, HEIGHT, "zzqx");
    // The release copies the dragged text and keeps the editor's
    // selection: typing replaces the selected pieces.
    let effect = drag_at(&mut app, WIDTH, HEIGHT, at, (at.0 + 2, at.1));
    assert_eq!(effect, Effect::Copy("zz".to_owned()));
    assert_eq!(app.input().selected(), Some((0, 2)));
    type_draft(&mut app, "q");
    assert_eq!(app.input().expand(), "qqx");
}

#[test]
fn a_drag_on_a_notice_copies_its_text() {
    use crate::app::Effect;
    let mut app = running(100, 24);
    raise_four(&mut app);
    let at = cell_at(&mut app, 100, 24, "Notice 1.");
    let effect = drag_at(&mut app, 100, 24, at, (at.0 + 9, at.1));
    // The copy holds the notice's text exactly: no closing pad and no
    // dismiss cross.
    assert_eq!(effect, Effect::Copy("Notice 1.".to_owned()));
}

#[test]
fn a_drag_on_the_notice_overlay_copies_its_text() {
    use crate::app::Effect;
    // A tall screen: a short frame drops its body rows, keeping the
    // title and footer, so the text needs room to show.
    let mut app = empty();
    attach(&mut app, S_A);
    app.set_size(80, 24);
    app.on_line(session_line(
        S_A,
        "notice",
        serde_json::json!({"code": "x", "message": "Saved."}),
        None,
    ));
    app.open_notice(0);
    let at = cell_at(&mut app, 80, 24, "Saved.");
    let effect = drag_at(&mut app, 80, 24, at, (at.0 + 4, at.1));
    assert_eq!(effect, Effect::Copy("Saved".to_owned()));
}

#[test]
fn a_drag_holds_a_fading_notice_until_after_the_release_copy() {
    use crate::app::Effect;
    use crate::keys::{Button, Mouse, MouseKind};
    use crate::mouse::Pointer;
    let t0 = fakes::clock::FakeClock::new().now();
    let mut app = empty();
    attach(&mut app, S_A);
    app.set_size(80, 24);
    app.on_line(session_line(
        S_A,
        "notice",
        serde_json::json!({"code": "io_failed", "message": "Saved."}),
        None,
    ));
    app.sweep_notices(t0);
    let area = Rect::new(0, 0, 80, 24);
    let at = cell_at(&mut app, 80, 24, "Saved.");
    let mut pointer = Pointer::default();
    let mut report =
        |app: &mut App, kind: MouseKind, col: u16, row: u16, now: std::time::Instant| {
            let mut buf = Buffer::empty(area);
            let targets = render(app, area, &mut buf, None);
            app.on_mouse(&Mouse { kind, col, row }, &targets, &mut pointer, true, now)
        };
    let _ = report(&mut app, MouseKind::Press(Button::Left), at.0, at.1, t0);
    let _ = report(&mut app, MouseKind::Drag(Button::Left), at.0 + 4, at.1, t0);
    // Past the fade, mid-drag, the row still shows: the fade waits
    // for the release (`docs/tui.md:1553`: on release the copy
    // happens first, then the waiting fade).
    app.sweep_notices(t0 + std::time::Duration::from_secs(30));
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    assert!(text(&buf).contains("Saved."));
    // The release copies first; the waiting fade runs on the next
    // sweep, after the copy.
    let effect = report(
        &mut app,
        MouseKind::Release,
        at.0 + 4,
        at.1,
        t0 + std::time::Duration::from_secs(30),
    );
    assert_eq!(effect, Effect::Copy("Saved".to_owned()));
    app.sweep_notices(t0 + std::time::Duration::from_secs(30));
    assert!(app.notices().is_empty());
}

#[test]
fn a_drag_on_the_approval_panel_copies_its_text() {
    use crate::app::Effect;
    let mut app = empty();
    attach(&mut app, S_A);
    app.on_line(session_line(
        S_A,
        "permission_requested",
        serde_json::json!({"request_id": "r_1", "effects": ["executes"],
            "reversible": true, "step": "review"}),
        Some("a_1"),
    ));
    assert!(app.panel().is_some());
    let at = cell_at(&mut app, WIDTH, HEIGHT, "runs a command");
    let effect = drag_at(&mut app, WIDTH, HEIGHT, at, (at.0 + 14, at.1));
    assert_eq!(effect, Effect::Copy("runs a command".to_owned()));
}

#[test]
fn a_drag_across_an_approval_wrap_reads_the_space_back() {
    use crate::app::Effect;
    let mut app = empty();
    attach(&mut app, S_A);
    app.set_size(60, 24);
    // Short words only, so every wrap lands on a space: joining with
    // nothing would drop them all, while the copy reads each back.
    let paths: Vec<String> = (0..12).map(|n| format!("p{n}")).collect();
    app.on_line(session_line(
        S_A,
        "permission_requested",
        serde_json::json!({"request_id": "r_1", "effects": ["executes"],
            "reversible": true, "step": "review", "paths": paths}),
        Some("a_1"),
    ));
    let facts = format!(
        "runs a command · {} · no rule allows this call",
        (0..12)
            .map(|n| format!("p{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    assert!(app.panel().is_some());
    assert!(
        app.panel()
            .expect("a panel")
            .lines
            .iter()
            .any(|line| line == &facts),
        "facts line shows"
    );
    // The whole facts line, first cell to last: the wrap's dropped
    // space reads back, and no newline splits the copy.
    let start = cell_at(&mut app, 60, 24, "runs a command");
    let end = cell_at(&mut app, 60, 24, "no rule allows this call");
    let effect = drag_at(
        &mut app,
        60,
        24,
        start,
        (
            end.0 + u16::try_from("no rule allows this call".len()).expect("a width"),
            end.1,
        ),
    );
    assert_eq!(effect, Effect::Copy(facts));
}

#[test]
fn a_drag_across_a_wrapped_conversation_row_joins_with_no_newline() {
    use crate::app::Effect;
    let text = format!("quixotic {}yodel", "zebras ".repeat(12));
    let mut app = replying(WIDTH, HEIGHT, &text);
    let start = cell_at(&mut app, WIDTH, HEIGHT, "quixotic");
    let end = cell_at(&mut app, WIDTH, HEIGHT, "yodel");
    let effect = drag_at(
        &mut app,
        WIDTH,
        HEIGHT,
        start,
        (
            end.0 + u16::try_from("yodel".len()).expect("a width"),
            end.1,
        ),
    );
    assert_eq!(effect, Effect::Copy(text));
}

/// A drag over the model picker copies the row's text: the picker
/// reads as its own surface over the conversation
/// (`docs/tui.md`, "Selection and copy").
#[test]
fn a_drag_over_the_model_picker_copies_its_row() {
    use crate::app::Effect;
    let mut app = empty();
    attach(&mut app, S_A);
    app.open_model_picker(crate::model_picker::Mode::Choose);
    let at = cell_at(&mut app, 80, 24, "Reading");
    let effect = drag_at(&mut app, 80, 24, at, (at.0 + 6, at.1));
    assert_eq!(effect, Effect::Copy("Reading".to_owned()));
}

/// A drag over the `/keys` screen copies the frame's rows: the title
/// and footer never copy (`docs/tui.md`, "Selection and copy").
#[test]
fn a_drag_over_the_keys_screen_copies_its_rows() {
    use crate::app::Effect;
    let mut app = empty();
    app.open_keys();
    let at = cell_at(&mut app, 80, 24, "Send a prompt");
    let effect = drag_at(&mut app, 80, 24, at, (at.0 + 3, at.1));
    assert_eq!(effect, Effect::Copy("Send".to_owned()));
}

/// A drag over the F1 key-map overlay copies its row's text: the
/// overlay notes its kept body rows through the shared overlay path,
/// the title and footer never copying
/// (`docs/tui.md`, "Selection and copy").
#[test]
fn a_drag_over_the_key_map_overlay_copies_its_row() {
    use crate::app::Effect;
    use crate::keys::Key;
    let mut app = empty();
    app.on_key(Key::F1, fakes::clock::FakeClock::new().now());
    assert!(app.keymap_top().is_some());
    let at = cell_at(&mut app, 80, 24, "Send a prompt");
    let effect = drag_at(&mut app, 80, 24, at, (at.0 + 3, at.1));
    assert_eq!(effect, Effect::Copy("Send".to_owned()));
}

/// A connected app attached to a session, holding a repository offer
/// of `items`.
fn offered(items: serde_json::Value, width: u16, height: u16) -> App {
    use crate::link::Line;
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(S_A.to_owned()));
    app.set_size(width, height);
    app.on_line(session_line(
        S_A,
        "repository_code_offered",
        serde_json::json!({"request_id": "r_1", "items": items}),
        None,
    ));
    assert!(app.offer_open());
    app
}

/// A drag over the repository offer copies the row's text: the
/// offer's body rows, the title never copying (`docs/tui.md`,
/// "Selection and copy").
#[test]
fn a_drag_over_the_offer_copies_its_body_text() {
    use crate::app::Effect;
    let items = serde_json::json!([{
        "kind": "mcp_server", "name": "db", "hash": "h", "required": true,
        "summary": "MCP server: db\ndeclared in: .fiber/config.json\nruns: /bin/echo",
    }]);
    let mut app = offered(items, 80, 24);
    let at = cell_at(&mut app, 80, 24, "declared in");
    let effect = drag_at(&mut app, 80, 24, at, (at.0 + 7, at.1));
    assert_eq!(effect, Effect::Copy("declared".to_owned()));
}

/// A drag over the login overlay copies its row's text: the login
/// overlay reads as its own surface (`docs/tui.md`, "Selection and
/// copy").
#[test]
fn a_drag_over_the_login_overlay_copies_its_row() {
    use crate::app::Effect;
    let mut app = empty();
    let seam = std::sync::Arc::new(crate::configure_fake::Fake::new(Vec::new()));
    app.set_configure(Some(seam));
    app.open_config_view(crate::app::ConfigView::Login);
    let at = cell_at(&mut app, 80, 24, "No provider");
    let effect = drag_at(&mut app, 80, 24, at, (at.0 + 10, at.1));
    assert_eq!(effect, Effect::Copy("No provider".to_owned()));
}

/// A drag over the input box paints the selection tint over the
/// covered cells and only those: the draft's range reads through its
/// visible spans (`docs/tui.md`, "Selection and copy").
#[test]
fn a_drag_over_the_input_box_paints_the_covered_cells() {
    use crate::keys::{Button, Mouse, MouseKind};
    use crate::mouse::Pointer;
    let mut app = empty();
    attach(&mut app, S_A);
    for ch in "hello".chars() {
        app.on_key(Key::Char(ch), fakes::clock::FakeClock::new().now());
    }
    let at = cell_at(&mut app, 80, 24, "hello");
    // The column counts cells, not bytes: the input box's `› ` mark is
    // multibyte.
    let area = Rect::new(0, 0, 80, 24);
    let mut probe = Buffer::empty(area);
    render(&app, area, &mut probe, None);
    let row_text: String = (0..80)
        .map(|col| {
            probe
                .cell((col, at.1))
                .map(|cell| cell.symbol().to_owned())
                .unwrap_or_default()
        })
        .collect();
    let prefix = row_text.split("hello").next().unwrap_or_default();
    let x = crate::text::to_u16(crate::text::width(prefix));
    let at = (x, at.1);
    let mut pointer = Pointer::default();
    let now = fakes::clock::FakeClock::new().now();
    let mut press = |app: &mut App, kind: MouseKind, col: u16, row: u16| {
        let mut buf = Buffer::empty(area);
        let targets = render(app, area, &mut buf, None);
        app.on_mouse(&Mouse { kind, col, row }, &targets, &mut pointer, true, now)
    };
    let _ = press(&mut app, MouseKind::Press(Button::Left), at.0 + 1, at.1);
    let _ = press(&mut app, MouseKind::Drag(Button::Left), at.0 + 3, at.1);
    let tint = crate::theme::Role::Selection.color();
    let buf = buffer(&app, 80, 24);
    // The covered cells carry the tint: the press and drag pieces, the
    // release piece's cell not yet, as the copy reads the same pieces.
    for col in [at.0 + 1, at.0 + 2] {
        assert_eq!(
            buf.cell((col, at.1)).map(|cell| cell.bg),
            Some(tint),
            "col {col} paints the selection"
        );
    }
    assert_ne!(
        buf.cell((at.0, at.1)).map(|cell| cell.bg),
        Some(tint),
        "before the drag stays plain"
    );
    assert_ne!(
        buf.cell((at.0 + 3, at.1)).map(|cell| cell.bg),
        Some(tint),
        "past the drag stays plain"
    );
}
