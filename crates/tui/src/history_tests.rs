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
        Effect::None
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. } => Vec::new(),
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

fn remote_history() -> App {
    let mut app = connected();
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Remote {
            hub: "box".to_owned(),
        },
        ..Default::default()
    });
    app
}

fn remote_history_row(app: &mut App, id: &str, project: &str, workspace: &str, since: u64) {
    app.on_line(Line::Session(contract::Envelope {
        kind: "session_status".to_owned(), session_id: contract::SessionId(id.to_owned()), ts:0,
        schema_version: contract::SCHEMA_VERSION, turn_id:None, action_id:None, seq:None,
        payload: json!({"name":"row", "project":project,"workspace":workspace,"since":since,"state":"idle",
            "started":0,"calls":0,"delegates":0,"jobs":0,"clients":0,"model":"test/model",
            "spend":{"tokens":{"input":0,"output":0,"cache_read":0,"cache_write":{}},"cost":0.0,"subscription_cost":0.0}}).as_object().unwrap().clone()
    }));
}

#[test]
fn remote_history_waits_for_a_known_project_then_uses_the_newest_row() {
    let mut app = remote_history();
    assert_eq!(app.recall_key(&Key::Up), Some(Effect::None));
    assert_eq!(app.draft(), "");
    assert_eq!(app.open_search(), Effect::None);
    app.close_search();
    remote_history_row(&mut app, S_A, "p", "/remote", 1);
    remote_history_row(&mut app, S_B, "q", "/remote", 2);
    app.home_click(crate::home::Spot::Pick(0));
    app.attach(contract::SessionId(S_A.to_owned()));
    let request = asks(&mut app, Key::Up);
    assert_eq!(request["args"], json!({"project":"q"}));
}

#[test]
fn remote_unknown_project_recalls_only_this_sessions_own_prompts() {
    let mut app = remote_history();
    with_own(&mut app, &["own prompt"]);
    assert_eq!(press(&mut app, Key::Up), Effect::None);
    assert_eq!(app.draft(), "own prompt");
    assert_eq!(press(&mut app, Key::Up), Effect::None);
    assert_eq!(app.draft(), "own prompt");
}

#[test]
fn remote_history_workspace_switch_drops_pages_and_restarts_paging() {
    let mut app = remote_history();
    remote_history_row(&mut app, S_A, "p", "/p", 1);
    remote_history_row(&mut app, S_B, "q", "/q", 2);
    app.home_click(crate::home::Spot::Pick(0));
    app.attach(contract::SessionId(S_A.to_owned()));
    let request = asks(&mut app, Key::Up);
    assert_eq!(request["args"], json!({"project":"p"}));
    answer(&mut app, &request, &[line(S_B, "p prompt")], Some(9));
    assert_eq!(app.draft(), "p prompt");
    app.go_home();
    app.home_click(crate::home::Spot::Workspace);
    app.home_click(crate::home::Spot::Pick(1));
    app.attach(contract::SessionId(S_B.to_owned()));
    app.draft.clear();
    let request = asks(&mut app, Key::Up);
    assert_eq!(request["args"], json!({"project":"q"}));
    assert_eq!(app.draft(), "");
    answer(&mut app, &request, &[line(S_A, "q prompt")], Some(8));
    assert_eq!(app.draft(), "q prompt");
    let next = asks(&mut app, Key::Up);
    assert_eq!(next["args"], json!({"project":"q", "before":8}));
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
        .map(|panel| (panel.lines(), panel.selected))
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

/// An app attached to `S_A` with `x` recalled, the oldest entry loaded,
/// and an ↑ waiting for the first page; that page's request.
fn waiting() -> (App, Value) {
    let mut app = connected();
    with_own(&mut app, &["x"]);
    let request = asks(&mut app, Key::Up);
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    assert_eq!(app.draft(), "x");
    (app, request)
}

#[test]
fn a_page_after_ctrl_r_opens_leaves_the_draft_and_joins_the_list() {
    let (mut app, request) = waiting();
    press(&mut app, Key::CtrlR);
    answer(&mut app, &request, &[line(S_B, "late")], None);
    assert_eq!(app.draft(), "x");
    press(&mut app, Key::Esc);
    assert_eq!(app.draft(), "x");
    // The page joined the list: the next ↑ shows it.
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "late");
}

/// A named action on the app, and the draft expected after it.
type Case = (&'static str, fn(&mut App), &'static str);

#[test]
fn a_page_after_any_edit_ctrl_g_send_or_clear_leaves_the_draft() {
    let now = fakes::clock::FakeClock::new().now();
    let cases: [Case; 6] = [
        ("cursor move", |app| drop(app.on_edit(Edit::Left)), "x"),
        (
            "paste",
            |app| drop(app.on_edit(Edit::Paste("y".into()))),
            "xy",
        ),
        ("ctrl+g", |app| drop(press(app, Key::CtrlG)), "x"),
        ("send", |app| drop(press(app, Key::Enter)), ""),
        ("ctrl+c", |app| drop(press(app, Key::CtrlC)), ""),
        ("key map", |app| drop(press(app, Key::F1)), "x"),
    ];
    for (name, act, draft) in cases {
        let (mut app, request) = waiting();
        act(&mut app);
        answer(&mut app, &request, &[line(S_B, "late")], None);
        assert_eq!(app.draft(), draft, "{name}");
    }
    let (mut app, request) = waiting();
    app.on_click(crate::mouse::TargetId::NewBelow);
    answer(&mut app, &request, &[line(S_B, "late")], None);
    assert_eq!(app.draft(), "x", "click");
    // A key that moves the view keeps the wait.
    let (mut app, request) = waiting();
    app.on_key(Key::PageUp, now);
    answer(&mut app, &request, &[line(S_B, "late")], None);
    assert_eq!(app.draft(), "late", "page up");
}

/// A line of `kind` from `S_A` with `payload`.
fn from_a(kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(S_A.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A standing ask `r_1` from `S_A`, which opens the approval panel.
fn asked() -> Line {
    from_a(
        "permission_requested",
        json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "standing_ask", "standing_rule": {"scope": "global", "prefix": "echo hi"}}),
    )
}

#[test]
fn a_page_while_the_approval_panel_is_open_leaves_the_draft() {
    let (mut app, request) = waiting();
    app.on_line(asked());
    assert!(app.panel().is_some());
    answer(&mut app, &request, &[line(S_B, "late")], None);
    assert_eq!(app.draft(), "x");
}

#[test]
fn a_page_after_an_approval_panel_opened_and_closed_leaves_the_draft() {
    let (mut app, request) = waiting();
    app.on_line(asked());
    app.on_line(from_a(
        "permission_resolved",
        json!({"request_id": "r_1", "decision": "deny", "decided_by": "cancel"}),
    ));
    assert!(app.panel().is_none());
    answer(&mut app, &request, &[line(S_B, "late")], None);
    assert_eq!(app.draft(), "x");
    press(&mut app, Key::Up);
    assert_eq!(app.draft(), "late");
}

/// An app attached to `S_A` with `x` recalled and an ↑ waiting for the
/// first page, after a reply to `r_1` went out and closed the panel; the
/// page's request and the reply's line.
fn waiting_after_a_reply() -> (App, Value, String) {
    let mut app = connected();
    with_own(&mut app, &["x"]);
    app.on_line(asked());
    let Effect::Send(lines) = press(&mut app, Key::Enter) else {
        panic!("no reply sent");
    };
    let reply = lines.into_iter().next().unwrap_or_default();
    assert!(app.panel().is_none());
    let request = asks(&mut app, Key::Up);
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    assert_eq!(app.draft(), "x");
    (app, request, reply)
}

#[test]
fn a_page_after_a_restored_approval_leaves_the_draft() {
    // The reply is rejected: the panel reopens.
    let (mut app, request, reply) = waiting_after_a_reply();
    let id: Value = serde_json::from_str(&reply).unwrap_or_default();
    app.on_line(from_a(
        "command_rejected",
        json!({"command_id": id["id"], "code": "stale_request", "message": "gone"}),
    ));
    assert!(app.panel().is_some());
    answer(&mut app, &request, &[line(S_B, "late")], None);
    assert_eq!(app.draft(), "x");
    // Its write fails: the panel reopens too.
    let (mut app, request, reply) = waiting_after_a_reply();
    app.write_failed(&[reply]);
    assert!(app.panel().is_some());
    answer(&mut app, &request, &[line(S_B, "late")], None);
    assert_eq!(app.draft(), "x");
}

#[test]
fn a_recalled_slash_entry_keeps_its_panel_and_its_wait() {
    let mut app = connected();
    with_own(&mut app, &["/ne"]);
    let request = asks(&mut app, Key::Up);
    assert!(app.completions().is_some());
    assert!(sent(press(&mut app, Key::Up)).is_empty());
    answer(&mut app, &request, &[line(S_B, "late")], None);
    assert_eq!(app.draft(), "late");
}

#[test]
fn a_page_after_a_steering_row_is_selected_leaves_the_draft() {
    let (mut app, request) = waiting();
    app.on_line(from_a(
        "steering_queue",
        json!({"messages": [{"content": [{"type": "text", "text": "use the parser"}],
            "source": "driver", "command_id": "c_1"}]}),
    ));
    // Selected without a key, as the click layer does: what stands in for
    // the draft changed, so the page shows nothing.
    app.select_steering(0);
    let draft = app.draft();
    answer(&mut app, &request, &[line(S_B, "late")], None);
    assert_eq!(app.draft(), draft);
    // ⌥↑ is a key: it ends the wait too.
    let (mut app, request) = waiting();
    press(&mut app, Key::AltUp);
    answer(&mut app, &request, &[line(S_B, "late")], None);
    assert_eq!(app.draft(), "x");
}

#[test]
fn a_page_while_the_notice_overlay_is_open_leaves_the_draft() {
    let (mut app, request) = waiting();
    app.on_line(from_a(
        "notice",
        json!({"code": "extension_failed", "message": "Notice."}),
    ));
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    answer(&mut app, &request, &[line(S_B, "late")], None);
    assert_eq!(app.draft(), "x");
}

#[test]
fn the_steering_keys_do_nothing_while_the_search_panel_is_open() {
    let mut app = connected();
    with_own(&mut app, &["x"]);
    app.on_line(from_a(
        "steering_queue",
        json!({"messages": [{"content": [{"type": "text", "text": "use the parser"}],
            "source": "driver", "command_id": "c_1"}]}),
    ));
    press(&mut app, Key::CtrlR);
    for key in [Key::AltUp, Key::AltDown, Key::AltX] {
        assert_eq!(press(&mut app, key), Effect::None);
    }
    assert_eq!(app.draft(), "");
    assert!(app.completions().is_some());
}

#[test]
fn up_moves_within_a_recalled_entry_wrapped_at_the_inset() {
    // Attached at 60 the draft wraps at the inset width of 58, past the
    // box's stripe and gap: 56 characters take two rows, 55 one
    // (`docs/tui.md`, "Look", "The input box").
    for (len, stays) in [(56, true), (55, false)] {
        let mut app = app();
        let long: String = std::iter::repeat_n('d', len).collect();
        with_own(&mut app, &["old", &long]);
        press(&mut app, Key::Up);
        assert_eq!(app.draft(), long, "len {len}");
        let width = app.draft_width();
        assert_eq!(width, 58, "len {len}");
        press(&mut app, Key::Up);
        if stays {
            assert_eq!(app.draft(), long, "len {len}");
            assert_eq!(app.input().cursor(width).0, 0, "len {len}");
        } else {
            assert_eq!(app.draft(), "old", "len {len}");
        }
    }
}

/// The search panel's structured rows: the Ctrl+R kind with its query,
/// window and match count, reserving no rows below the conversation.
#[test]
fn the_search_panel_is_search_rows_and_reserves_nothing() {
    let mut app = connected();
    type_text(&mut app, "draft");
    let closed = app.below_rows();
    let request = asks(&mut app, Key::CtrlR);
    answer(
        &mut app,
        &request,
        &[line(S_B, "Fix the Build"), line(S_B, "run tests")],
        None,
    );
    type_text(&mut app, "build");
    let completions = app.completions().expect("the panel shows");
    assert_eq!(completions.query, "build");
    assert_eq!(completions.total, 1);
    assert_eq!(completions.start, 0);
    assert_eq!(completions.selected, Some(1));
    assert!(matches!(
        completions.rows,
        super::super::commands::Rows::Search(_)
    ));
    assert_eq!(
        completions.lines(),
        ["search prompts: build", "Fix the Build"]
    );
    assert_eq!(app.below_rows(), closed);
}

/// The prompt recall lists built since the last count.
fn recall_builds() -> usize {
    crate::work::take().recall_builds
}

/// Presses `keys` and returns the draft each leaves.
fn drafts(app: &mut App, keys: &[Key]) -> Vec<String> {
    keys.iter()
        .map(|key| {
            press(app, key.clone());
            app.draft()
        })
        .collect()
}

#[test]
fn recall_builds_its_list_once_for_a_run_of_ups_and_downs() {
    let mut app = connected();
    with_own(&mut app, &["one", "two", "three"]);
    recall_builds();
    let shown = drafts(&mut app, &[Key::Up, Key::Up, Key::Up, Key::Down, Key::Down]);
    assert_eq!(shown, ["three", "two", "one", "two", "three"]);
    assert_eq!(recall_builds(), 1);
}

#[test]
fn a_prompt_seen_after_a_recall_rebuilds_the_list_once() {
    let mut app = connected();
    with_own(&mut app, &["one", "two", "three"]);
    drafts(&mut app, &[Key::Up, Key::Up, Key::Up, Key::Down, Key::Down]);
    recall_builds();
    app.on_line(turn(S_A, "four", "driver"));
    assert_eq!(drafts(&mut app, &[Key::Down, Key::Up]), ["", "four"]);
    assert_eq!(recall_builds(), 1);
    assert_eq!(drafts(&mut app, &[Key::Up]), ["three"]);
    assert_eq!(recall_builds(), 0);
}

#[test]
fn an_answered_page_rebuilds_the_list_once_and_the_next_recall_reuses_it() {
    let mut app = connected();
    let request = asks(&mut app, Key::Up);
    recall_builds();
    answer(&mut app, &request, &[line(S_B, "from b")], None);
    assert_eq!(app.draft(), "from b");
    assert_eq!(recall_builds(), 1);
    press(&mut app, Key::Up);
    assert_eq!(recall_builds(), 0);
}

#[test]
fn typing_in_the_search_panel_builds_the_list_once() {
    let mut app = connected();
    let request = asks(&mut app, Key::CtrlR);
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
    recall_builds();
    for ch in "build".chars() {
        press(&mut app, Key::Char(ch));
        assert!(app.search_panel().is_some());
    }
    assert_eq!(recall_builds(), 1);
    assert_eq!(
        panel(&app).0,
        ["search prompts: build", "Fix the Build", "build docs"]
    );
}

#[test]
fn attaching_another_session_rebuilds_the_list_for_it() {
    let mut app = connected();
    app.attach(contract::SessionId(S_B.to_owned()));
    app.on_line(turn(S_B, "in b", "driver"));
    with_own(&mut app, &["in a"]);
    assert_eq!(drafts(&mut app, &[Key::Up]), ["in a"]);
    assert_eq!(recall_builds(), 1);
    app.attach(contract::SessionId(S_B.to_owned()));
    assert_eq!(drafts(&mut app, &[Key::Down, Key::Up]), ["", "in b"]);
    assert_eq!(recall_builds(), 1);
}

#[test]
fn a_project_change_rebuilds_the_list_without_the_old_projects_lines() {
    let mut app = connected();
    let request = asks(&mut app, Key::Up);
    answer(&mut app, &request, &[line(S_B, "old project")], None);
    assert_eq!(app.draft(), "old project");
    press(&mut app, Key::Down);
    recall_builds();
    app.set_project("-Users-me-other".to_owned());
    assert_eq!(drafts(&mut app, &[Key::Up]), [""]);
    assert_eq!(recall_builds(), 1);
}

#[cfg(debug_assertions)]
#[test]
fn settle_rejects_reentrant_calls() {
    let mut app = app();
    let outer = super::SettleDepth::enter();
    let nested = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.settle()));
    drop(outer);
    assert!(nested.is_err());
}
