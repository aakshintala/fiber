//! Tests for the completion panels, the built-in commands and the key map.

use crate::app::{App, Effect};
use crate::keys::{Edit, Key};
use crate::link::Line;
use contract::clock::Clock;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Instant;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// An app in `/w`, connected to the hub, with no session.
fn connected() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    assert!(app.on_line(Line::Hub(hello)).is_empty());
    app
}

/// A connected app attached to [`SESSION`].
fn attached() -> App {
    let mut app = connected();
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// Types `text`, checking each key does nothing else.
fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        assert_eq!(app.on_key(Key::Char(ch), now()), Effect::None);
    }
}

/// Types `text` and presses Enter.
fn enter(app: &mut App, text: &str) -> Effect {
    type_text(app, text);
    app.on_key(Key::Enter, now())
}

/// The command lines an effect sends, parsed.
fn sent(effect: Effect) -> Vec<Value> {
    match effect {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_default())
            .collect(),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_) => Vec::new(),
    }
}

/// One session envelope from [`SESSION`].
fn session_line(kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A `reloaded` line from `session`.
fn reloaded(session: &str) -> Line {
    let mut line = session_line(
        "reloaded",
        json!({"servers": {"kept": [], "restarted": [], "started": [], "stopped": []},
            "extensions": []}),
    );
    if let Line::Session(envelope) = &mut line {
        envelope.session_id = contract::SessionId(session.to_owned());
    }
    line
}

/// The session answers the `commands` a `reloaded` asks for with skills
/// named `names`.
fn answer_commands(app: &mut App, names: &[&str]) {
    let asked = app.on_line(reloaded(SESSION));
    assert_eq!(asked.len(), 1);
    let asked: Value = serde_json::from_str(&asked[0]).unwrap_or_default();
    assert_eq!(asked["command"], "commands");
    let rows: Vec<Value> = names
        .iter()
        .map(|name| json!({"name": name, "description": format!("Runs {name}."), "tag": "skill"}))
        .collect();
    let answer = session_line(
        "command_accepted",
        json!({"command_id": asked["id"], "result": {"commands": rows}}),
    );
    assert!(app.on_line(answer).is_empty());
}

/// A turn starts on [`SESSION`].
fn turn_starts(app: &mut App) {
    let line = session_line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "hi"}]}]}),
    );
    assert!(app.on_line(line).is_empty());
}

/// The completion panel's rows.
fn shown(app: &App) -> Vec<String> {
    app.completions()
        .map(|completions| completions.lines)
        .unwrap_or_default()
}

/// The selected completion row's text.
fn selected(app: &App) -> Option<String> {
    let completions = app.completions()?;
    completions.lines.get(completions.selected?).cloned()
}

#[test]
fn slash_alone_shows_every_row_from_the_top() {
    let mut app = connected();
    assert_eq!(app.completions(), None);
    type_text(&mut app, "/");
    let rows = shown(&app);
    assert_eq!(rows.len(), 8);
    assert_eq!(
        rows.first().map(String::as_str),
        Some("/home  Goes home.  command")
    );
    assert_eq!(selected(&app), rows.first().cloned());
}

#[test]
fn typing_filters_prefix_matches_first() {
    let mut app = attached();
    answer_commands(&mut app, &["tdd", "areview"]);
    type_text(&mut app, "/re");
    assert_eq!(
        shown(&app),
        [
            "/resume  Opens home at the session list.  command",
            "/reload  Reloads configuration, MCP servers and extensions.  command",
            "/areview  Runs areview.  skill",
        ]
    );
    type_text(&mut app, "zz");
    assert_eq!(app.completions(), None);
}

#[test]
fn a_skill_from_the_commands_answer_is_a_row_and_runs_as_a_prompt() {
    let mut app = attached();
    answer_commands(&mut app, &["tdd"]);
    type_text(&mut app, "/td");
    assert_eq!(shown(&app), ["/tdd  Runs tdd.  skill"]);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert_eq!(lines[0]["args"]["content"][0]["text"], "/tdd");
}

#[test]
fn another_sessions_reloaded_asks_nothing() {
    let mut app = attached();
    assert!(app.on_line(reloaded("s_bbbbbbbbbbbbbbbb")).is_empty());
}

#[test]
fn reloaded_with_the_link_down_asks_nothing() {
    let mut app = App::new(PathBuf::from("/w"));
    app.attach(contract::SessionId(SESSION.to_owned()));
    assert!(app.on_line(reloaded(SESSION)).is_empty());
}

#[test]
fn up_and_down_move_the_selection_clamped_and_scroll_the_window() {
    let mut app = connected();
    type_text(&mut app, "/");
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(
        selected(&app).as_deref(),
        Some("/home  Goes home.  command")
    );
    for _ in 0..7 {
        app.on_key(Key::Down, now());
    }
    // The eighth row, still in the first window.
    assert_eq!(
        selected(&app).as_deref(),
        Some("/reload  Reloads configuration, MCP servers and extensions.  command")
    );
    assert_eq!(app.completions().and_then(|c| c.selected), Some(7));
    app.on_key(Key::Down, now());
    // The ninth row: the window moves down by one.
    let completions = app.completions();
    assert_eq!(completions.as_ref().and_then(|c| c.selected), Some(7));
    assert_eq!(
        completions
            .and_then(|c| c.lines.first().cloned())
            .as_deref(),
        Some("/new  Goes home with the cursor in the input box.  command")
    );
    app.on_key(Key::Down, now());
    app.on_key(Key::Down, now());
    app.on_key(Key::Down, now());
    assert_eq!(
        selected(&app).as_deref(),
        Some("/?  Opens the key map.  command")
    );
    app.on_key(Key::Down, now());
    assert_eq!(
        selected(&app).as_deref(),
        Some("/help  Opens the key map.  command")
    );
    app.on_key(Key::Up, now());
    assert_eq!(
        selected(&app).as_deref(),
        Some("/?  Opens the key map.  command")
    );
    // The draft is untouched by moving.
    assert_eq!(app.draft(), "/");
}

#[test]
fn tab_completes_the_selected_name_and_closes_the_panel() {
    let mut app = connected();
    type_text(&mut app, "/han");
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "/handoff ");
    assert_eq!(app.completions(), None);
}

#[test]
fn tab_with_no_panel_and_no_match_does_nothing() {
    let mut app = connected();
    type_text(&mut app, "hi");
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    assert_eq!(app.draft(), "hi");
    let mut app = connected();
    type_text(&mut app, "/zz");
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "/zz");
}

#[test]
fn enter_runs_the_selected_row() {
    let mut app = attached();
    type_text(&mut app, "/rel");
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "reload");
    assert_eq!(app.draft(), "");
}

#[test]
fn handoff_sends_its_instructions() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/handoff  do x "));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "handoff");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0]["args"], json!({"instructions": "do x"}));
    assert_eq!(app.draft(), "");
    let lines = sent(enter(&mut app, "/handoff"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "handoff");
    assert_eq!(lines[0].get("args"), None);
}

#[test]
fn the_slash_list_shows_the_name_row() {
    let mut app = connected();
    type_text(&mut app, "/nam");
    assert_eq!(shown(&app), ["/name <text>  Names the session.  command"]);
}

#[test]
fn reload_sends_reload_without_args() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/reload"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "reload");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0].get("args"), None);
}

#[test]
fn a_rejected_handoff_returns_its_draft_with_the_notice() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/handoff do x"));
    let id = lines[0]["id"].clone();
    let rejected = session_line(
        "command_rejected",
        json!({"command_id": id, "code": "busy", "message": "Busy."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.draft(), "/handoff do x");
    assert_eq!(app.notice(), Some("Busy."));
}

#[test]
fn a_command_with_no_session_says_so() {
    for command in ["/handoff x", "/name x", "/reload", "/close"] {
        let mut app = connected();
        assert_eq!(enter(&mut app, command), Effect::None, "{command}");
        assert_eq!(app.notice(), Some("No session on screen."), "{command}");
        assert_eq!(app.draft(), "", "{command}");
    }
}

#[test]
fn a_command_with_the_link_down_keeps_its_draft() {
    for command in ["/handoff x", "/name x", "/reload", "/close"] {
        let mut app = attached();
        app.disconnected();
        assert_eq!(enter(&mut app, command), Effect::None, "{command}");
        assert_eq!(app.draft(), command, "{command}");
        assert!(app.session().is_some(), "{command}");
    }
}

#[test]
fn close_while_idle_sends_close_and_goes_home() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/close"));
    let commands: Vec<&Value> = lines.iter().map(|line| &line["command"]).collect();
    assert_eq!(commands, [&json!("close")]);
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0]["args"], json!({"now": true}));
    assert_eq!(app.session(), None);
}

#[test]
fn a_rejected_close_gives_only_its_notice_on_the_home_draft() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/close"));
    let rejected = contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": lines[0]["id"], "code": "busy", "message": "Busy."})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    };
    assert!(app.on_line(Line::Hub(rejected)).is_empty());
    assert_eq!(app.draft(), "");
    assert_eq!(app.notice(), Some("Busy."));
}

#[test]
fn close_during_a_turn_sends_only_close_with_now() {
    let mut app = attached();
    turn_starts(&mut app);
    let lines = sent(enter(&mut app, "/close"));
    let commands: Vec<&Value> = lines.iter().map(|line| &line["command"]).collect();
    assert_eq!(commands, [&json!("close")]);
    assert!(lines.iter().all(|line| line["session_id"] == SESSION));
    assert_eq!(lines[0]["args"], json!({"now": true}));
    assert_eq!(app.session(), None);
    assert!(app.lines().is_empty());
}

#[test]
fn quit_quits() {
    let mut app = connected();
    assert_eq!(enter(&mut app, "/quit"), Effect::Quit);
}

#[test]
fn home_and_new_return_to_the_screen_before_a_session() {
    for command in ["/home", "/new"] {
        let mut app = attached();
        answer_commands(&mut app, &["tdd"]);
        turn_starts(&mut app);
        app.set_size(80, 3);
        app.on_key(Key::PageUp, now());
        assert_eq!(enter(&mut app, command), Effect::None, "{command}");
        assert_eq!(app.session(), None, "{command}");
        assert!(app.lines().is_empty(), "{command}");
        assert_eq!(app.top(), None, "{command}");
        assert_eq!(app.draft(), "", "{command}");
        // The old session's skills are gone with it.
        type_text(&mut app, "/td");
        assert_eq!(app.completions(), None, "{command}");
        app.on_key(Key::Esc, now());
        app.on_key(Key::CtrlC, now());
        // The next Enter starts a new session.
        let lines = sent(enter(&mut app, "hi"));
        assert_eq!(lines.len(), 1, "{command}");
        assert_eq!(lines[0]["command"], "start", "{command}");
    }
}

#[test]
fn home_while_start_is_pending_only_clears_the_draft() {
    let mut app = connected();
    let lines = sent(enter(&mut app, "hi"));
    assert_eq!(lines[0]["command"], "start");
    assert_eq!(enter(&mut app, "/home"), Effect::None);
    assert_eq!(app.draft(), "");
    // Still waiting on that `start`: Enter sends nothing and keeps the
    // draft, where the screen before a session would send `start`.
    assert_eq!(enter(&mut app, "again"), Effect::None);
    assert_eq!(app.draft(), "again");
}

#[test]
fn an_unknown_command_is_sent_as_written() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/foo bar"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert_eq!(lines[0]["args"]["content"][0]["text"], "/foo bar");
    turn_starts(&mut app);
    let lines = sent(enter(&mut app, "/foo"));
    assert_eq!(lines[0]["command"], "steer");
    assert_eq!(lines[0]["args"]["content"][0]["text"], "/foo");
}

#[test]
fn esc_closes_the_slash_panel_and_keeps_the_draft_until_it_is_emptied() {
    let mut app = attached();
    turn_starts(&mut app);
    type_text(&mut app, "/re");
    // Esc closes the panel; it does not interrupt the turn.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(app.draft(), "/re");
    assert_eq!(app.completions(), None);
    // Typing again does not reopen it.
    type_text(&mut app, "l");
    assert_eq!(app.completions(), None);
    // Backspace back to `/` keeps it closed; to empty, then `/`, reopens.
    for _ in 0..3 {
        app.on_key(Key::Backspace, now());
    }
    assert_eq!(app.draft(), "/");
    assert_eq!(app.completions(), None);
    app.on_key(Key::Backspace, now());
    type_text(&mut app, "/");
    assert_eq!(shown(&app).len(), 8);
    // A draft that stops starting with `/` also reopens it after Esc.
    app.on_key(Key::Esc, now());
    app.on_key(Key::CtrlC, now());
    type_text(&mut app, "/");
    assert_eq!(shown(&app).len(), 8);
}

#[test]
fn a_space_closes_the_slash_panel() {
    let mut app = connected();
    type_text(&mut app, "/handoff ");
    assert_eq!(app.completions(), None);
    app.on_key(Key::Backspace, now());
    assert_eq!(shown(&app).len(), 1);
}

#[test]
fn the_approvals_row_opens_the_waiting_queue() {
    let mut app = connected();
    assert_eq!(enter(&mut app, "/approvals"), Effect::None);
    assert_eq!(app.notice(), Some("No requests waiting."));
    assert_eq!(app.draft(), "");
}

#[test]
fn the_conversation_gives_up_the_panel_rows() {
    let mut app = connected();
    app.set_size(80, 24);
    assert_eq!(app.conversation_height(), 23);
    type_text(&mut app, "/");
    assert_eq!(app.conversation_height(), 15);
    type_text(&mut app, "han");
    assert_eq!(app.conversation_height(), 22);
}

#[test]
fn an_open_approval_panel_wins_over_the_slash_panel() {
    let mut app = attached();
    type_text(&mut app, "/");
    let request = session_line(
        "permission_requested",
        json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "review", "rule": {"subject": "npm test", "prefix": "npm test"}}),
    );
    assert!(app.on_line(request).is_empty());
    assert!(app.panel().is_some());
    assert_eq!(app.completions(), None);
    // Esc goes to the approval panel, which puts the request aside; the
    // slash panel shows again, still open.
    app.on_key(Key::Esc, now());
    assert!(app.panel().is_none());
    assert_eq!(shown(&app).len(), 8);
}

#[test]
fn slash_question_mark_slash_help_and_f1_open_the_key_map() {
    for open in ["/?", "/help", "F1"] {
        let mut app = connected();
        if open == "F1" {
            assert_eq!(app.on_key(Key::F1, now()), Effect::None);
        } else {
            assert_eq!(enter(&mut app, open), Effect::None, "{open}");
        }
        assert_eq!(app.keymap_top(), Some(0), "{open}");
        assert_eq!(app.draft(), "", "{open}");
        assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
        assert_eq!(app.keymap_top(), None, "{open}");
    }
}

#[test]
fn the_key_map_scrolls_and_takes_every_key_but_ctrl_c() {
    let mut app = attached();
    app.set_size(80, 10);
    turn_starts(&mut app);
    app.on_key(Key::F1, now());
    // A conversation of 9 rows: a page is 8.
    assert_eq!(app.conversation_height(), 9);
    app.on_key(Key::Up, now());
    assert_eq!(app.keymap_top(), Some(0));
    app.on_key(Key::Down, now());
    assert_eq!(app.keymap_top(), Some(1));
    app.on_key(Key::PageDown, now());
    assert_eq!(app.keymap_top(), Some(9));
    app.on_key(Key::PageUp, now());
    assert_eq!(app.keymap_top(), Some(1));
    app.on_key(Key::PageUp, now());
    assert_eq!(app.keymap_top(), Some(0));
    // Down and PageDown stop at the last screenful.
    for _ in 0..20 {
        app.on_key(Key::PageDown, now());
    }
    let last = app.keymap_top();
    let rows: usize = crate::keymap::lines(app.keys())
        .iter()
        .map(|line| crate::view::rows(ratatui::text::Line::raw(line.as_str()), 80))
        .sum();
    assert_eq!(last, Some(rows - 9));
    app.on_key(Key::Down, now());
    assert_eq!(app.keymap_top(), last);
    // Other keys do nothing: no typing, no interrupt, no send.
    for key in [Key::Char('x'), Key::Enter, Key::Tab, Key::End, Key::F1] {
        assert_eq!(app.on_key(key, now()), Effect::None);
    }
    assert_eq!(app.draft(), "");
    assert_eq!(app.keymap_top(), last);
    // Ctrl+C still arms the quit.
    app.on_key(Key::CtrlC, now());
    assert!(app.hint());
    assert_eq!(app.keymap_top(), last);
}

/// Types `text`, whatever each key does.
fn type_any(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.on_key(Key::Char(ch), now());
    }
}

/// Types one character, returning its effect.
fn key(app: &mut App, ch: char) -> Effect {
    app.on_key(Key::Char(ch), now())
}

fn paths(list: &[&str]) -> Result<Vec<String>, String> {
    Ok(list.iter().map(|path| (*path).to_owned()).collect())
}

#[test]
fn at_opens_the_file_panel_and_each_keystroke_searches_anew() {
    let mut app = connected();
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    assert!(app.files_open());
    let opened = app.generation();
    assert_eq!(
        key(&mut app, 's'),
        Effect::Search {
            generation: opened + 1,
            query: "s".to_owned()
        }
    );
    assert_eq!(
        key(&mut app, 'r'),
        Effect::Search {
            generation: opened + 2,
            query: "sr".to_owned()
        }
    );
    assert_eq!(
        app.on_key(Key::Backspace, now()),
        Effect::Search {
            generation: opened + 3,
            query: "s".to_owned()
        }
    );
    // A superseded result changes nothing; the current one shows.
    app.on_files(opened + 2, paths(&["old.rs"]));
    assert_eq!(app.completions(), None);
    app.on_files(opened + 3, paths(&["src/a.rs", "src/b.rs"]));
    assert_eq!(shown(&app), ["src/a.rs", "src/b.rs"]);
    app.on_key(Key::Down, now());
    assert_eq!(selected(&app).as_deref(), Some("src/b.rs"));
    // Tab puts the path in place of `@s`, with a space, and closes.
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "src/b.rs ");
    assert!(!app.files_open());
}

#[test]
fn enter_chooses_a_file_mid_draft_without_sending() {
    let mut app = attached();
    type_text(&mut app, "look at ");
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    key(&mut app, 'm');
    app.on_files(app.generation(), paths(&["main.rs"]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(app.draft(), "look at main.rs ");
    // The next Enter sends the draft.
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines[0]["args"]["content"][0]["text"], "look at main.rs ");
}

#[test]
fn only_an_at_at_the_start_or_after_whitespace_opens_the_panel() {
    let mut app = connected();
    type_text(&mut app, "me@x");
    assert!(!app.files_open());
    // An `@` inside an open panel's query is part of the query.
    app.on_key(Key::CtrlC, now());
    key(&mut app, '@');
    assert_eq!(
        key(&mut app, '@'),
        Effect::Search {
            generation: app.generation(),
            query: "@".to_owned()
        }
    );
    // A second panel opens only after whitespace closes the first.
    assert_eq!(key(&mut app, ' '), Effect::None);
    assert!(!app.files_open());
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "@@ @");
}

#[test]
fn esc_and_backspace_past_the_at_close_the_panel() {
    let mut app = connected();
    type_any(&mut app, "x @ab");
    app.on_files(app.generation(), paths(&["ab.rs"]));
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(app.draft(), "x @ab");
    assert!(!app.files_open());
    // Typing on does not reopen it.
    assert_eq!(key(&mut app, 'c'), Effect::None);
    assert!(!app.files_open());
    let mut app = connected();
    type_any(&mut app, "x @a");
    app.on_key(Key::Backspace, now());
    assert!(app.files_open());
    assert_eq!(app.on_key(Key::Backspace, now()), Effect::None);
    assert_eq!(app.draft(), "x ");
    assert!(!app.files_open());
}

#[test]
fn a_result_after_close_or_reopen_is_dropped() {
    let mut app = connected();
    key(&mut app, '@');
    let first = app.generation();
    app.on_key(Key::Esc, now());
    app.on_files(first, paths(&["late.rs"]));
    assert_eq!(app.completions(), None);
    // Reopened: the generation moved on, never back.
    app.on_key(Key::CtrlC, now());
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    assert!(app.generation() > first);
    app.on_files(first, paths(&["late.rs"]));
    assert_eq!(app.completions(), None);
    app.on_files(app.generation(), paths(&["new.rs"]));
    assert_eq!(shown(&app), ["new.rs"]);
}

#[test]
fn a_failed_listing_is_one_row_that_cannot_be_chosen() {
    let mut app = connected();
    key(&mut app, '@');
    app.on_files(
        app.generation(),
        Err("fatal: not a git repository".to_owned()),
    );
    let completions = app.completions();
    assert_eq!(
        completions.as_ref().map(|c| c.lines.clone()),
        Some(vec!["No files: fatal: not a git repository".to_owned()])
    );
    assert_eq!(completions.and_then(|c| c.selected), None);
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "@");
}

#[test]
fn a_shorter_result_clamps_the_selection() {
    let mut app = connected();
    key(&mut app, '@');
    app.on_files(app.generation(), paths(&["a", "b", "c"]));
    app.on_key(Key::Down, now());
    app.on_key(Key::Down, now());
    assert_eq!(selected(&app).as_deref(), Some("c"));
    app.on_files(app.generation(), paths(&["a", "b"]));
    assert_eq!(selected(&app).as_deref(), Some("b"));
}

#[test]
fn the_file_panel_follows_the_cursor_in_a_multi_line_draft() {
    // Whitespace after the query closes the panel, and moving back into
    // the query does not reopen it.
    let mut app = connected();
    type_any(&mut app, "first");
    assert_eq!(app.on_edit(Edit::ShiftEnter), Effect::None);
    type_any(&mut app, "@ma tail");
    assert!(!app.files_open());
    for _ in 0.." tail".len() {
        assert_eq!(app.on_edit(Edit::Left), Effect::None);
    }
    assert!(!app.files_open());
    let mut app = connected();
    type_any(&mut app, "first");
    app.on_edit(Edit::CtrlJ);
    key(&mut app, '@');
    key(&mut app, 'a');
    // A paste at the cursor searches anew.
    let generation = app.generation();
    assert_eq!(
        app.on_edit(Edit::Paste("in".to_owned())),
        Effect::Search {
            generation: generation + 1,
            query: "ain".to_owned()
        }
    );
    // Moving inside the query keeps the panel; choosing replaces the whole
    // query, not only the part before the cursor.
    app.on_edit(Edit::Left);
    assert!(app.files_open());
    app.on_files(app.generation(), paths(&["src/main.rs"]));
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "first\nsrc/main.rs ");
    // Left past the `@` closes the panel.
    let mut app = connected();
    type_any(&mut app, "x @ab");
    app.on_edit(Edit::LineStart);
    assert!(!app.files_open());
}

#[test]
fn a_file_chosen_mid_line_keeps_the_text_after_it() {
    let mut app = connected();
    type_any(&mut app, "see  now");
    for _ in 0.." now".len() {
        app.on_edit(Edit::Left);
    }
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    key(&mut app, 'a');
    app.on_files(app.generation(), paths(&["a.rs"]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(app.draft(), "see a.rs  now");
    // The cursor is after the inserted path and its space.
    key(&mut app, '!');
    assert_eq!(app.draft(), "see a.rs ! now");
}

#[test]
fn a_line_break_closes_the_slash_panel() {
    let mut app = connected();
    type_any(&mut app, "/re");
    assert!(app.completions().is_some());
    app.on_edit(Edit::ShiftEnter);
    assert_eq!(app.completions(), None);
    app.on_key(Key::Backspace, now());
    assert!(app.completions().is_some());
}

#[test]
fn editing_keys_do_nothing_while_the_key_map_is_open() {
    let mut app = connected();
    type_any(&mut app, "ab");
    assert_eq!(app.on_key(Key::F1, now()), Effect::None);
    assert!(app.keymap_top().is_some());
    assert_eq!(app.on_edit(Edit::Paste("x".to_owned())), Effect::None);
    assert_eq!(app.on_edit(Edit::Left), Effect::None);
    app.on_key(Key::Esc, now());
    key(&mut app, '!');
    assert_eq!(app.draft(), "ab!");
}

/// A hub line of `kind` with `payload`.
fn hub_line(kind: &str, payload: Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: kind.to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Sends `hi` with no session and accepts the `start` for [`SESSION`],
/// returning the lines the acceptance sends, parsed.
fn start_accepted(app: &mut App) -> Vec<Value> {
    let lines = sent(enter(app, "hi"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    let accepted = hub_line(
        "command_accepted",
        json!({"command_id": lines[0]["id"], "result": {"session_id": SESSION}}),
    );
    app.on_line(accepted)
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect()
}

#[test]
fn start_carries_no_content() {
    let mut app = App::new(PathBuf::from("/w"));
    // Held before `hub_hello`, sent after it.
    assert_eq!(enter(&mut app, "hi"), Effect::None);
    let hello = hub_line("hub_hello", json!({}));
    let lines: Vec<Value> = app
        .on_line(hello)
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    assert_eq!(lines[0]["args"], json!({"workspace": "/w"}));
}

#[test]
fn an_accepted_start_subscribes_then_prompts() {
    let mut app = connected();
    let lines = start_accepted(&mut app);
    let commands: Vec<&Value> = lines.iter().map(|line| &line["command"]).collect();
    assert_eq!(
        commands,
        [&json!("subscribe"), &json!("commands"), &json!("prompt")]
    );
    assert_eq!(lines[0]["args"]["level"], "full");
    assert_eq!(lines[2]["session_id"], SESSION);
    assert_eq!(
        lines[2]["args"],
        json!({"content": [{"type": "text", "text": "hi"}]})
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

#[test]
fn a_rejected_first_prompt_returns_its_text() {
    let mut app = connected();
    let lines = start_accepted(&mut app);
    let rejected = session_line(
        "command_rejected",
        json!({"command_id": lines[2]["id"], "code": "busy", "message": "Busy."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.notice(), Some("Busy."));
    assert_eq!(app.draft(), "hi");
}

#[test]
fn a_rejected_start_still_returns_its_text() {
    let mut app = connected();
    let lines = sent(enter(&mut app, "hi"));
    let rejected = hub_line(
        "command_rejected",
        json!({"command_id": lines[0]["id"], "code": "invalid_arguments", "message": "No."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.notice(), Some("No."));
    assert_eq!(app.draft(), "hi");
    assert_eq!(app.session(), None);
}

/// Pastes an image into the draft: Ctrl+V reads, and its result lands.
fn paste_image(app: &mut App) {
    assert_eq!(app.on_key(Key::CtrlV, now()), Effect::ReadImage(0));
    app.on_image(0, Ok("AAA".to_owned()));
}

/// The image part the tests paste.
fn image_part() -> Value {
    json!({"type": "image", "data": "AAA", "mime_type": "image/png"})
}

#[test]
fn a_slash_draft_with_an_image_is_a_prompt() {
    // E1: "/name x" would name the session, but an image makes it a prompt.
    let mut app = attached();
    type_text(&mut app, "/name x");
    paste_image(&mut app);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert_eq!(
        lines[0]["args"],
        json!({"content": [{"type": "text", "text": "/name x"}, image_part()]})
    );
}

#[test]
fn a_bang_draft_with_an_image_is_a_prompt() {
    // E2: "!echo hi" would run a shell command, but an image makes it a
    // prompt.
    let mut app = attached();
    type_text(&mut app, "!echo hi");
    paste_image(&mut app);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert_eq!(
        lines[0]["args"],
        json!({"content": [{"type": "text", "text": "!echo hi"}, image_part()]})
    );
}

#[test]
fn an_image_only_draft_sends_one_image_part() {
    // E3.
    let mut app = attached();
    paste_image(&mut app);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert_eq!(lines[0]["args"], json!({"content": [image_part()]}));
}

#[test]
fn a_first_prompt_carries_the_image() {
    // E4: the draft sent before a session starts rides the first prompt.
    let mut app = connected();
    type_text(&mut app, "look");
    paste_image(&mut app);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    let accepted = hub_line(
        "command_accepted",
        json!({"command_id": lines[0]["id"], "result": {"session_id": SESSION}}),
    );
    let out: Vec<Value> = app
        .on_line(accepted)
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert_eq!(out.len(), 3);
    assert_eq!(out[2]["command"], "prompt");
    assert_eq!(
        out[2]["args"],
        json!({"content": [{"type": "text", "text": "look"}, image_part()]})
    );
}

#[test]
fn a_busy_session_gets_the_image_as_steer() {
    // E5.
    let mut app = attached();
    turn_starts(&mut app);
    type_text(&mut app, "look");
    paste_image(&mut app);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "steer");
    assert_eq!(
        lines[0]["args"],
        json!({"content": [{"type": "text", "text": "look"}, image_part()]})
    );
}

#[test]
fn a_rejected_prompt_returns_its_image() {
    // E6: the draft comes back with its image, the cursor at its end.
    let mut app = attached();
    type_text(&mut app, "look");
    paste_image(&mut app);
    let lines = sent(app.on_key(Key::Enter, now()));
    let rejected = session_line(
        "command_rejected",
        json!({"command_id": lines[0]["id"], "code": "busy", "message": "Busy."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.draft(), "look[Image #1]");
    assert_eq!(app.on_key(Key::Char('!'), now()), Effect::None);
    assert_eq!(app.draft(), "look[Image #1]!");
}

#[test]
fn an_unwritten_prompt_returns_its_image() {
    // E7: a prompt never written returns its image too.
    let mut app = attached();
    type_text(&mut app, "look");
    paste_image(&mut app);
    let Effect::Send(lines) = app.on_key(Key::Enter, now()) else {
        panic!("Enter sends the prompt");
    };
    app.write_failed(&lines);
    assert_eq!(app.draft(), "look[Image #1]");
}
