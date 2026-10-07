//! Tests for prompt recall, paging and the Ctrl+R panel.

use super::super::{App, Effect};
use crate::keys::{Edit, Key};
use crate::link::Line;
use contract::clock::Clock;
use serde_json::{Value, json};
use std::path::PathBuf;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";
const S_B: &str = "s_bbbbbbbbbbbbbbbb";
const KEY: &str = "-Users-me-work-fiber-.git";

/// An app in `/w` with the project key, 60 columns wide.
fn app() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_project(KEY.to_owned());
    app.set_size(60, 12);
    app
}

/// A hub line of `kind` with `payload`.
fn hub(kind: &str, payload: Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: kind.to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// An app connected to the hub.
fn connected() -> App {
    let mut app = app();
    assert!(app.on_line(hub("hub_hello", json!({}))).is_empty());
    app
}

/// Presses `key`.
fn press(app: &mut App, key: Key) -> Effect {
    app.on_key(key, fakes::clock::FakeClock::new().now())
}

/// Types `text` key by key.
fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        press(app, Key::Char(ch));
    }
}

/// The lines an effect sends, parsed; none for any other effect.
fn sent(effect: Effect) -> Vec<Value> {
    match effect {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_default())
            .collect(),
        Effect::None | Effect::Quit | Effect::ListFiles | Effect::Search { .. } => Vec::new(),
    }
}

/// The one `prompt_history` line in `lines`.
fn one(lines: Vec<Value>) -> Value {
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = lines.into_iter().next().unwrap_or_default();
    assert_eq!(line["command"], "prompt_history");
    line
}

/// Presses `key` and returns the one `prompt_history` it sends.
fn asks(app: &mut App, key: Key) -> Value {
    one(sent(press(app, key)))
}

/// A prompt history line.
fn line(session: &str, text: &str) -> Value {
    json!({"ts": 1, "session_id": session, "content": [{"type": "text", "text": text}]})
}

/// The hub's answer to `request` with `prompts`, and `before` when given;
/// the lines the app sends back.
fn answer(app: &mut App, request: &Value, prompts: &[Value], before: Option<u64>) -> Vec<Value> {
    let mut result = json!({ "prompts": prompts });
    if let Some(before) = before {
        result["before"] = json!(before);
    }
    let id = request["id"].clone();
    app.on_line(hub(
        "command_accepted",
        json!({"command_id": id, "result": result}),
    ))
    .iter()
    .map(|line| serde_json::from_str(line).unwrap_or_default())
    .collect()
}

/// A `turn_started` for `session` with one message from `source`.
fn turn(session: &str, text: &str, source: &str) -> Line {
    let mut message = json!({"type": "message", "content": [{"type": "text", "text": text}],
        "source": source});
    match source {
        "extension" => message["extension"] = json!("lint"),
        "session" => message["from_session_id"] = json!(S_B),
        _ => {}
    }
    Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({ "input": [message] })
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// Attaches to `S_A` and folds its prompts from a client, oldest first.
fn with_own(app: &mut App, prompts: &[&str]) {
    app.attach(contract::SessionId(S_A.to_owned()));
    for prompt in prompts {
        app.on_line(turn(S_A, prompt, "driver"));
    }
}

#[test]
fn the_first_up_asks_once_and_a_second_before_the_answer_sends_nothing() {
    let mut app = connected();
    let request = asks(&mut app, Key::Up);
    assert_eq!(request["args"], json!({ "project": KEY }));
    assert!(
        request["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("c_"))
    );
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    assert_eq!(app.draft(), "");
    // The answer fills the list and shows the newest entry.
    let more = answer(
        &mut app,
        &request,
        &[line(S_B, "newest"), line(S_B, "older")],
        None,
    );
    assert!(more.is_empty());
    assert_eq!(app.draft(), "newest");
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "older");
    // The oldest entry with no more pages: ↑ does nothing.
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    assert_eq!(app.draft(), "older");
}

#[test]
fn a_page_with_before_is_followed_only_at_the_end() {
    let mut app = connected();
    let first = asks(&mut app, Key::Up);
    let more = answer(
        &mut app,
        &first,
        &[line(S_B, "one"), line(S_B, "two")],
        Some(12345),
    );
    assert!(more.is_empty());
    // "one" shows; ↑ to "two" is within what is loaded: nothing goes out.
    assert_eq!(app.draft(), "one");
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    assert_eq!(app.draft(), "two");
    // At the end, the next page is asked for with `before`.
    let next = asks(&mut app, Key::Up);
    assert_eq!(next["args"], json!({"project": KEY, "before": 12345}));
    assert_ne!(next["id"], first["id"]);
    assert_eq!(app.draft(), "two");
    // One request at a time: ↑ again while it is out sends nothing.
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    // The answer shows the next older entry.
    answer(&mut app, &next, &[line(S_B, "three")], None);
    assert_eq!(app.draft(), "three");
    assert!(sent(press(&mut app, Key::Up)).is_empty());
}

#[test]
fn a_page_adding_nothing_new_asks_for_the_next() {
    let mut app = connected();
    with_own(&mut app, &["mine"]);
    let first = asks(&mut app, Key::Up);
    assert_eq!(app.draft(), "mine");
    assert!(answer(&mut app, &first, &[], Some(7)).is_empty());
    let next = asks(&mut app, Key::Up);
    assert_eq!(next["args"]["before"], 7);
    // Only a line this session took, which the list already holds: the
    // waiting recall asks for the page after.
    let last = one(answer(&mut app, &next, &[line(S_A, "mine")], Some(3)));
    assert_eq!(last["args"]["before"], 3);
    answer(&mut app, &last, &[line(S_B, "theirs")], None);
    assert_eq!(app.draft(), "theirs");
}

#[test]
fn this_sessions_prompts_come_first_and_are_not_repeated_from_the_hub() {
    let mut app = connected();
    with_own(&mut app, &["mine 1", "mine 2"]);
    let request = asks(&mut app, Key::Up);
    // The newest of this session's shows before any answer.
    assert_eq!(app.draft(), "mine 2");
    answer(
        &mut app,
        &request,
        &[
            line(S_A, "mine 2"),
            line(S_B, "theirs"),
            line(S_A, "only in the file"),
            line(S_B, "mine 1"),
        ],
        None,
    );
    let mut shown = vec![app.draft()];
    for _ in 0..3 {
        press(&mut app, Key::Up);
        shown.push(app.draft());
    }
    assert_eq!(shown, ["mine 2", "mine 1", "theirs", "theirs"]);
}

#[test]
fn only_a_clients_message_is_this_sessions_prompt() {
    let mut app = app();
    app.attach(contract::SessionId(S_A.to_owned()));
    for source in ["driver", "extension", "session", "fiber"] {
        app.on_line(turn(S_A, source, source));
    }
    // Another session's turn is not this session's.
    app.on_line(turn(S_B, "elsewhere", "driver"));
    // Link not up: recall uses this session's prompts alone.
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    assert_eq!(app.draft(), "driver");
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    assert_eq!(app.draft(), "driver");
}

#[test]
fn image_only_entries_and_duplicates_are_skipped() {
    let mut app = connected();
    let request = asks(&mut app, Key::Up);
    let image = json!({"type": "image", "path": "artifacts/a.png", "mime_type": "image/png",
        "width": 1, "height": 1});
    let image_only = json!({"ts": 1, "session_id": S_B, "content": [image]});
    let parts = json!({"ts": 1, "session_id": S_B, "content": [
        {"type": "text", "text": "a"}, image, {"type": "text", "text": "b"}]});
    answer(
        &mut app,
        &request,
        &[
            line(S_B, "same"),
            image_only,
            line(S_B, "same"),
            json!({"bad": true}),
            parts,
            line(S_B, "last"),
        ],
        None,
    );
    assert_eq!(app.draft(), "same");
    press(&mut app, Key::Up);
    // Text parts join with a line break.
    assert_eq!(app.draft(), "a\nb");
    press(&mut app, Key::Up);
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "last");
}

#[test]
fn a_multi_line_entry_moves_the_cursor_row_by_row_before_recalling_older() {
    let mut app = app();
    with_own(&mut app, &["old", "x\ny\nz"]);
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "x\ny\nz");
    assert_eq!(app.input().cursor(60).0, 2);
    press(&mut app, Key::Up);
    assert_eq!(app.input().cursor(60).0, 1);
    press(&mut app, Key::Up);
    assert_eq!(app.input().cursor(60).0, 0);
    assert_eq!(app.draft(), "x\ny\nz");
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "old");
    // ↓ back: the multi-line entry, cursor at its end on the last row.
    press(&mut app, Key::Down);
    assert_eq!(app.draft(), "x\ny\nz");
    assert_eq!(app.input().cursor(60).0, 2);
    // ↓ past the newest restores the empty draft; ↓ again does nothing.
    press(&mut app, Key::Down);
    assert_eq!(app.draft(), "");
    press(&mut app, Key::Down);
    assert_eq!(app.draft(), "");
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "x\ny\nz");
    // ↓ from the first row moves down row by row before going newer.
    press(&mut app, Key::Up);
    press(&mut app, Key::Up);
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "old");
    press(&mut app, Key::Down);
    press(&mut app, Key::Up);
    press(&mut app, Key::Up);
    assert_eq!(app.input().cursor(60).0, 0);
    press(&mut app, Key::Down);
    assert_eq!(app.input().cursor(60).0, 1);
    assert_eq!(app.draft(), "x\ny\nz");
}

#[test]
fn up_in_a_typed_draft_moves_the_cursor_and_never_recalls() {
    let mut app = app();
    with_own(&mut app, &["old"]);
    type_text(&mut app, "typed");
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "typed");
    press(&mut app, Key::Down);
    assert_eq!(app.draft(), "typed");
}

#[test]
fn an_edit_ends_browsing_and_a_cursor_move_does_not() {
    let mut app = app();
    with_own(&mut app, &["one", "two"]);
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "two");
    type_text(&mut app, "!");
    // ↑ and ↓ on the edited draft's only row recall nothing.
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "two!");
    press(&mut app, Key::Down);
    assert_eq!(app.draft(), "two!");
    // Back to the entry's text by hand is not browsing either.
    press(&mut app, Key::Backspace);
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "two");
    // A paste, a line break and a delete each end browsing.
    for edit in [Edit::Paste("x".to_owned()), Edit::ShiftEnter, Edit::Delete] {
        app.on_key(Key::CtrlC, fakes::clock::FakeClock::new().now());
        press(&mut app, Key::Up);
        assert_eq!(app.draft(), "two");
        if edit == Edit::Delete {
            app.on_edit(Edit::LineStart);
        }
        app.on_edit(edit);
        let edited = app.draft();
        assert_ne!(edited, "two");
        press(&mut app, Key::Up);
        press(&mut app, Key::Up);
        assert_eq!(app.draft(), edited);
    }
    // A cursor move alone keeps browsing.
    app.on_key(Key::CtrlC, fakes::clock::FakeClock::new().now());
    press(&mut app, Key::Up);
    app.on_edit(Edit::Left);
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "one");
}

#[test]
fn an_answer_after_typing_shows_nothing() {
    let mut app = connected();
    let request = asks(&mut app, Key::Up);
    type_text(&mut app, "a");
    press(&mut app, Key::Backspace);
    answer(&mut app, &request, &[line(S_B, "late")], None);
    assert_eq!(app.draft(), "");
    // The entry is there for the next ↑.
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    assert_eq!(app.draft(), "late");
}

#[test]
fn an_answer_while_browsing_at_the_end_shows_the_next_unless_edited() {
    let mut app = connected();
    with_own(&mut app, &["mine"]);
    let first = asks(&mut app, Key::Up);
    // ↑ again at the end while the first page is out: it waits.
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    answer(&mut app, &first, &[line(S_B, "theirs")], Some(9));
    assert_eq!(app.draft(), "theirs");
    let next = asks(&mut app, Key::Up);
    type_text(&mut app, "?");
    answer(&mut app, &next, &[line(S_B, "older")], None);
    assert_eq!(app.draft(), "theirs?");
}

#[test]
fn an_answer_for_another_id_changes_nothing() {
    let mut app = connected();
    let request = asks(&mut app, Key::Up);
    let stale = json!({"id": "c_0000000000000000"});
    assert!(answer(&mut app, &stale, &[line(S_B, "stale")], Some(1)).is_empty());
    assert_eq!(app.draft(), "");
    // Still waiting on the first: no second request.
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    answer(&mut app, &request, &[line(S_B, "fresh")], None);
    assert_eq!(app.draft(), "fresh");
    // Answered once: a repeat of the same id is stale too.
    answer(&mut app, &request, &[line(S_B, "again")], None);
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "fresh");
}

#[test]
fn with_the_link_not_up_nothing_is_sent() {
    let mut app = app();
    with_own(&mut app, &["mine"]);
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    assert_eq!(app.draft(), "mine");
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    assert!(sent(press(&mut app, Key::CtrlR)).is_empty());
    // Once the link is up, the end of what is loaded asks.
    press(&mut app, Key::Esc);
    app.on_line(hub("hub_hello", json!({})));
    let request = asks(&mut app, Key::Up);
    assert_eq!(request["args"], json!({ "project": KEY }));
}

#[test]
fn a_rejection_shows_its_message_and_ends_paging() {
    let mut app = connected();
    let request = asks(&mut app, Key::Up);
    let id = request["id"].clone();
    let lines = app.on_line(hub(
        "command_rejected",
        json!({"command_id": id, "code": "usage", "message": "no history here"}),
    ));
    assert!(lines.is_empty());
    assert_eq!(app.notice(), Some("no history here"));
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    assert!(sent(press(&mut app, Key::CtrlR)).is_empty());
    // A rejection for another id is not recall's.
    app.on_line(hub(
        "command_rejected",
        json!({"command_id": "c_1", "code": "usage", "message": "other"}),
    ));
    assert_eq!(app.notice(), Some("no history here"));
}

#[test]
fn attaching_moves_that_sessions_hub_lines_out() {
    let mut app = connected();
    let request = asks(&mut app, Key::Up);
    answer(
        &mut app,
        &request,
        &[line(S_A, "from a"), line(S_B, "from b")],
        None,
    );
    assert_eq!(app.draft(), "from a");
    press(&mut app, Key::Down);
    app.attach(contract::SessionId(S_A.to_owned()));
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "from b");
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "from b");
}

/// The search panel's rows and selected row.
fn panel(app: &App) -> (Vec<String>, Option<usize>) {
    app.completions()
        .map(|panel| (panel.lines, panel.selected))
        .unwrap_or_default()
}

#[test]
fn ctrl_r_searches_ignoring_case_and_enter_puts_the_match_in_the_draft() {
    let mut app = connected();
    type_text(&mut app, "draft");
    let request = asks(&mut app, Key::CtrlR);
    assert_eq!(request["args"], json!({ "project": KEY }));
    assert_eq!(
        panel(&app),
        (
            vec![
                "search prompts: ".to_owned(),
                "no matching prompts".to_owned()
            ],
            None
        )
    );
    answer(
        &mut app,
        &request,
        &[
            line(S_B, "Fix the Build"),
            line(S_B, "run tests"),
            line(S_B, "build\ndocs"),
        ],
        None,
    );
    type_text(&mut app, "BUILD");
    assert_eq!(app.draft(), "draft");
    assert_eq!(
        panel(&app),
        (
            vec![
                "search prompts: BUILD".to_owned(),
                "Fix the Build".to_owned(),
                "build docs".to_owned(),
            ],
            Some(1)
        )
    );
    // Ctrl+R and ↓ move to the next match and stop at the last; ↑ back.
    press(&mut app, Key::CtrlR);
    assert_eq!(panel(&app).1, Some(2));
    press(&mut app, Key::Down);
    assert_eq!(panel(&app).1, Some(2));
    press(&mut app, Key::Up);
    assert_eq!(panel(&app).1, Some(1));
    press(&mut app, Key::CtrlR);
    // Enter replaces the draft, sends nothing and closes.
    assert_eq!(press(&mut app, Key::Enter), Effect::None);
    assert_eq!(app.draft(), "build\ndocs");
    assert_eq!(app.input().position(), 10);
    assert!(app.completions().is_none());
}

#[test]
fn backspace_and_paste_edit_the_query_and_esc_keeps_the_draft() {
    let mut app = app();
    with_own(&mut app, &["alpha", "beta"]);
    type_text(&mut app, "draft");
    assert!(sent(press(&mut app, Key::CtrlR)).is_empty());
    type_text(&mut app, "bx");
    assert_eq!(
        panel(&app).0.get(1).cloned(),
        Some("no matching prompts".to_owned())
    );
    press(&mut app, Key::Backspace);
    assert_eq!(panel(&app).0, ["search prompts: b", "beta"]);
    app.on_edit(Edit::Paste("e\nt".to_owned()));
    assert_eq!(
        panel(&app).0,
        ["search prompts: be t", "no matching prompts"]
    );
    press(&mut app, Key::Esc);
    assert!(app.completions().is_none());
    assert_eq!(app.draft(), "draft");
}

#[test]
fn an_open_search_asks_page_after_page_until_the_history_ends() {
    let mut app = connected();
    let first = asks(&mut app, Key::CtrlR);
    // A second Ctrl+R while one is out sends nothing.
    assert!(sent(press(&mut app, Key::CtrlR)).is_empty());
    let next = one(answer(&mut app, &first, &[line(S_B, "one")], Some(40)));
    assert_eq!(next["args"]["before"], 40);
    assert!(answer(&mut app, &next, &[line(S_B, "two")], None).is_empty());
    assert_eq!(panel(&app).0, ["search prompts: ", "one", "two"]);
    // Closed and opened again: the history has ended, nothing is sent.
    press(&mut app, Key::Esc);
    assert!(sent(press(&mut app, Key::CtrlR)).is_empty());
}

#[test]
fn the_search_window_keeps_the_selection_in_view() {
    let mut app = app();
    let prompts: Vec<String> = (0..12).map(|at| format!("p{at}")).collect();
    let prompts: Vec<&str> = prompts.iter().map(String::as_str).collect();
    with_own(&mut app, &prompts);
    press(&mut app, Key::CtrlR);
    for _ in 0..9 {
        press(&mut app, Key::Down);
    }
    let (lines, selected) = panel(&app);
    assert_eq!(lines.len(), 9);
    assert_eq!(lines.get(1).map(String::as_str), Some("p9"));
    assert_eq!(selected, Some(8));
    assert_eq!(lines.get(8).map(String::as_str), Some("p2"));
}

#[test]
fn another_sessions_prompts_seen_earlier_are_not_this_sessions() {
    let mut app = app();
    with_own(&mut app, &["in a"]);
    app.attach(contract::SessionId(S_B.to_owned()));
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "");
}

#[test]
fn emptying_a_recalled_draft_by_hand_ends_the_wait() {
    let mut app = connected();
    with_own(&mut app, &["x"]);
    let request = asks(&mut app, Key::Up);
    assert_eq!(app.draft(), "x");
    // At the end while the first page is out: the recall waits.
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    press(&mut app, Key::Backspace);
    answer(&mut app, &request, &[line(S_B, "late")], None);
    assert_eq!(app.draft(), "");
}
