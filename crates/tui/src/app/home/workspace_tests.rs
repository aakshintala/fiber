//! Tests for the workspace picker's typed path: the `directories`
//! lines it sends, the rows it focuses, and the workspace Enter opens.

use super::super::super::{App, Effect};
use crate::home::{Launch, Spot};
use crate::keys::{Edit, Key};
use crate::link::Line;
use contract::clock::Clock;
use serde_json::{Value, json};
use std::path::PathBuf;

/// An app on home at 80x24.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
        model: Some("test/model".to_owned()),
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// A `hub_hello` this terminal reads.
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// A hub `command_accepted` for `id` with `result`.
fn accepted(id: &str, result: Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("result".to_owned(), result),
        ]
        .into_iter()
        .collect(),
    })
}

/// A hub `command_rejected` for `id` with `message`.
fn refused(id: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            (
                "code".to_owned(),
                Value::String("invalid_arguments".to_owned()),
            ),
            ("message".to_owned(), Value::String(message.to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// A live `session_status` for `session`, named `name`, in `workspace`.
fn live(session: &str, name: &str, workspace: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({
            "name": name,
            "workspace": workspace,
            "project": "-w",
            "started": 0, "calls": 0, "since": 0,
            "state": "idle",
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// Links the app: the feed and recent ids, both waiting for their
/// answers.
fn linked(app: &mut App) -> (String, String) {
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 2);
    (
        lines[0]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("feed id"))
            .to_owned(),
        lines[1]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("recent id"))
            .to_owned(),
    )
}

/// Parses outgoing command lines.
fn commands(lines: Vec<String>) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// The `(id, path)` of every `directories` line an effect sends.
fn directories_of(effect: &Effect) -> Vec<(String, String)> {
    match effect {
        Effect::Send(lines) => sent_directories(lines),
        Effect::None
        | Effect::Quit
        | Effect::Exit(_)
        | Effect::ListFiles
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Copy(_)
        | Effect::FindPause { .. }
        | Effect::OpenLink(_)
        | Effect::ReadImage(_)
        | Effect::OpenFile(_) => Vec::new(),
    }
}

/// The `(id, path)` of every `directories` line in `lines`.
fn sent_directories(lines: &[String]) -> Vec<(String, String)> {
    lines
        .iter()
        .filter_map(|line| {
            let line: Value = serde_json::from_str(line).ok()?;
            if line["command"] != "directories" {
                return None;
            }
            Some((
                line["id"].as_str()?.to_owned(),
                line["args"]["path"].as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// Presses `key` on the picker.
fn key(app: &mut App, key: Key) -> Effect {
    app.on_key(key, fakes::clock::FakeClock::new().now())
}

/// Types `text` into the picker, collecting every `(id, path)` sent.
fn type_path(app: &mut App, text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for ch in text.chars() {
        out.extend(directories_of(&key(app, Key::Char(ch))));
    }
    out
}

/// Opens the picker on a linked app.
fn open(app: &mut App) {
    linked(app);
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
}

/// The picker's recents.
fn recents(app: &App) -> Vec<String> {
    app.home_screen()
        .and_then(|screen| screen.picker)
        .map(|picker| picker.recents)
        .unwrap_or_default()
}

/// The typed text, once its row is open.
fn typed_text(app: &App) -> Option<String> {
    app.home_screen()
        .and_then(|screen| screen.picker)?
        .typed
        .map(|typed| typed.text)
}

/// The current directory's completions.
fn completions(app: &App) -> Vec<String> {
    app.home_screen()
        .and_then(|screen| screen.picker)
        .and_then(|picker| picker.typed)
        .map(|typed| typed.completions)
        .unwrap_or_default()
}

/// The focused row's text: a completion, else a recent.
fn focused(app: &App) -> Option<String> {
    let picker = app.home_screen()?.picker?;
    let at = picker.focus?;
    match picker.typed {
        None => picker.recents.get(at).cloned(),
        Some(typed) => {
            if at < typed.completions.len() {
                typed.completions.get(at).cloned()
            } else {
                picker.recents.get(at - typed.completions.len()).cloned()
            }
        }
    }
}

/// Whether the picker shows "no such directory".
fn missing(app: &App) -> bool {
    app.home_screen()
        .and_then(|screen| screen.picker)
        .and_then(|picker| picker.typed)
        .is_some_and(|typed| typed.missing)
}

/// Whether the picker is open.
fn picker_open(app: &App) -> bool {
    app.home_screen()
        .is_some_and(|screen| screen.picker.is_some())
}

/// The chips home draws, as text left to right.
fn chips(app: &App) -> Vec<String> {
    app.home_screen()
        .map(|screen| screen.chips.into_iter().map(|(_, text)| text).collect())
        .unwrap_or_default()
}

/// Answers the `directories` request `id` with `names` at `path`.
fn answer(app: &mut App, id: &str, names: &[&str], path: &str) {
    app.on_line(accepted(id, json!({"directories": names, "path": path})));
}

fn remote() -> App {
    let mut app = home();
    app.set_home(Launch {
        hub: Some("box".to_owned()),
        workspace: PathBuf::from("/sentinel/launch-dir"),
        project: "sentinel-project".to_owned(),
        git: true,
        model: Some("test/model".to_owned()),
        ..Default::default()
    });
    app
}

#[test]
fn remote_starts_with_no_workspace_and_cannot_escape() {
    let mut app = remote();
    assert!(picker_open(&app));
    assert!(recents(&app).is_empty());
    assert_eq!(app.home_workspace(), PathBuf::new());
    assert_eq!(chips(&app)[0], "[no workspace]");
    assert!(app.home_screen().unwrap().no_workspace);
    assert!(!app.home_screen().unwrap().picker.unwrap().escapes);
    assert_eq!(key(&mut app, Key::Esc), Effect::None);
    assert!(picker_open(&app));
    assert!(!chips(&app).iter().any(|chip| chip.contains("worktree")));
    linked(&mut app);
    app.on_line(live("s_aaaaaaaaaaaaaaaa", "chosen", "/remote/w"));
    assert_eq!(recents(&app), ["/remote/w"]);
    key(&mut app, Key::Enter);
    assert_eq!(app.home_workspace(), PathBuf::from("/remote/w"));
    assert_eq!(chips(&app)[0], "[w]");
    assert!(!app.home_screen().unwrap().no_workspace);
    app.home_click(Spot::Workspace);
    assert!(app.home_screen().unwrap().picker.unwrap().escapes);
    key(&mut app, Key::Esc);
    assert!(!picker_open(&app));
}

#[test]
fn remote_picker_refills_feed_then_recent_distinct_absolute_rows() {
    let mut app = remote();
    let (_, recent) = linked(&mut app);
    for (i, ws) in ["/a", "/b", "/a", "", "relative"].iter().enumerate() {
        app.on_line(live(&format!("s_{i:016x}"), "row", ws));
    }
    app.on_line(accepted(
        &recent,
        json!({"sessions": [
            {"session_id":"s_1111111111111111", "workspace":"/c", "project":"p", "how":"exited"},
            {"session_id":"s_2222222222222222", "workspace":"/b", "project":"p", "how":"exited"}
        ]}),
    ));
    assert_eq!(recents(&app), ["/a", "/b", "/c"]);
    key(&mut app, Key::Down);
    assert_eq!(focused(&app).as_deref(), Some("/b"));
    app.on_line(live("s_0000000000000000", "changed", "/z"));
    assert_eq!(focused(&app).as_deref(), Some("/b"));
    app.on_line(live("s_0000000000000001", "changed", "/y"));
    // The recent row still keeps /b listed.
    assert_eq!(focused(&app).as_deref(), Some("/b"));
    for _ in 0..recents(&app).len() {
        key(&mut app, Key::Up);
    }
    assert_eq!(focused(&app).as_deref(), Some("/z"));
    app.on_line(live("s_0000000000000000", "changed", "/x"));
    assert_eq!(focused(&app).as_deref(), Some("/x"));
}

#[test]
fn remote_refill_focuses_first_when_the_selected_workspace_leaves() {
    let mut app = remote();
    linked(&mut app);
    app.on_line(live("s_aaaaaaaaaaaaaaaa", "first", "/a"));
    app.on_line(live("s_bbbbbbbbbbbbbbbb", "second", "/b"));
    key(&mut app, Key::Down);
    assert_eq!(focused(&app).as_deref(), Some("/b"));
    app.on_line(live("s_bbbbbbbbbbbbbbbb", "moved", "/c"));
    assert_eq!(recents(&app), ["/a", "/c"]);
    assert_eq!(focused(&app).as_deref(), Some("/a"));
}

#[test]
fn remote_picker_cap_and_typing_refill_boundary() {
    for count in [9, 10, 11] {
        let mut app = remote();
        linked(&mut app);
        for i in 0..count {
            app.on_line(live(&format!("s_{i:016x}"), "row", &format!("/w{i}")));
        }
        assert_eq!(recents(&app).len(), count.min(10));
        assert_eq!(recents(&app)[0], "/w0");
    }
    let mut app = remote();
    linked(&mut app);
    app.on_line(live("s_0000000000000000", "row", "/a"));
    type_path(&mut app, "x");
    app.on_line(live("s_1111111111111111", "row", "/b"));
    assert_eq!(recents(&app), ["/a"]);
    key(&mut app, Key::Backspace);
    assert_eq!(recents(&app), ["/a", "/b"]);
    assert_eq!(typed_text(&app), None);
}

#[test]
fn remote_sentinel_is_only_a_hub_row_and_uses_hub_git() {
    for git in [false, true] {
        let mut app = remote();
        linked(&mut app);
        assert!(recents(&app).is_empty());
        let mut row = live("s_aaaaaaaaaaaaaaaa", "same path", "/sentinel/launch-dir");
        if let Line::Session(envelope) = &mut row
            && git
        {
            envelope
                .payload
                .insert("git".to_owned(), json!({"branch":"main"}));
        }
        app.on_line(row);
        assert_eq!(recents(&app), ["/sentinel/launch-dir"]);
        key(&mut app, Key::Enter);
        assert_eq!(app.home_workspace(), PathBuf::from("/sentinel/launch-dir"));
        assert_eq!(
            chips(&app).iter().any(|chip| chip.contains("worktree")),
            git
        );
        assert_eq!(app.start_args()["workspace"], "/sentinel/launch-dir");
    }
}

#[test]
fn recents_alone_exclude_empty_and_relative_workspaces() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live("s_aaaaaaaaaaaaaaaa", "here", "/w"));
    app.on_line(live("s_bbbbbbbbbbbbbbbb", "empty", ""));
    app.on_line(live("s_cccccccccccccccc", "relative", "rel"));
    app.on_line(live("s_dddddddddddddddd", "away", "/other"));
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert_eq!(recents(&app), vec!["/w".to_owned(), "/other".to_owned()]);
}

#[test]
fn esc_closes_the_typed_picker_keeping_the_workspace() {
    let mut app = home();
    open(&mut app);
    type_path(&mut app, "/");
    assert_eq!(key(&mut app, Key::Esc), Effect::None);
    assert!(!picker_open(&app));
    assert_eq!(chips(&app)[0], "[w]");
}

#[test]
fn a_click_on_a_recent_while_typing_chooses_it() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live("s_aaaaaaaaaaaaaaaa", "away", "/other"));
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    type_path(&mut app, "/x");
    assert_eq!(app.home_click(Spot::Pick(1)), Effect::None);
    assert!(!picker_open(&app));
    assert_eq!(chips(&app)[0], "[other]");
}

#[test]
fn ctrl_c_passes_through_while_typing() {
    let mut app = home();
    open(&mut app);
    type_path(&mut app, "/");
    assert_eq!(app.home_key(&Key::CtrlC), None);
    assert!(picker_open(&app));
    assert_eq!(typed_text(&app), Some("/".to_owned()));
}

#[test]
fn paste_is_swallowed_while_typing() {
    let mut app = home();
    open(&mut app);
    type_path(&mut app, "/");
    assert_eq!(app.on_edit(Edit::Paste("/pasted".to_owned())), Effect::None);
    assert_eq!(typed_text(&app), Some("/".to_owned()));
    assert!(app.input().expand().is_empty());
}

#[test]
fn slash_sends_one_directories_and_enter_waits_for_its_answer() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/");
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["/"]
    );
    // Enter before the answer does nothing: the picker stays open, the
    // chip stays, and nothing goes out.
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(picker_open(&app));
    assert_eq!(chips(&app)[0], "[w]");
    assert_eq!(focused(&app), None);
    answer(&mut app, &sent[0].0, &["a1", "a2", ".git"], "/");
    assert_eq!(focused(&app), Some("a1".to_owned()));
    assert_eq!(completions(&app), vec!["a1".to_owned(), "a2".to_owned()]);
    // Typing `.` shows the hidden directory alone, asking nothing more.
    let sent = type_path(&mut app, ".");
    assert!(sent.is_empty());
    assert_eq!(completions(&app), vec![".git".to_owned()]);
    assert_eq!(focused(&app), Some(".git".to_owned()));
}

#[test]
fn arrows_walk_completions_then_recents_and_clamp() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/");
    answer(&mut app, &sent[0].0, &["a1", "a2"], "/");
    assert_eq!(focused(&app), Some("a1".to_owned()));
    assert_eq!(key(&mut app, Key::Down), Effect::None);
    assert_eq!(focused(&app), Some("a2".to_owned()));
    assert_eq!(key(&mut app, Key::Down), Effect::None);
    assert_eq!(focused(&app), Some("/w".to_owned()));
    assert_eq!(key(&mut app, Key::Down), Effect::None);
    assert_eq!(focused(&app), Some("/w".to_owned()));
    assert_eq!(key(&mut app, Key::Up), Effect::None);
    assert_eq!(focused(&app), Some("a2".to_owned()));
    assert_eq!(key(&mut app, Key::Up), Effect::None);
    assert_eq!(focused(&app), Some("a1".to_owned()));
    assert_eq!(key(&mut app, Key::Up), Effect::None);
    assert_eq!(focused(&app), Some("a1".to_owned()));
}

#[test]
fn enter_on_a_completion_chooses_it_for_the_next_start() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/");
    answer(&mut app, &sent[0].0, &["a1", "a2"], "/");
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(!picker_open(&app));
    assert_eq!(chips(&app)[0], "[a1]");
    assert!(!chips(&app).contains(&"[x] new worktree".to_owned()));
    for ch in "hi".chars() {
        key(&mut app, Key::Char(ch));
    }
    let Effect::Send(lines) = key(&mut app, Key::Enter) else {
        panic!("Enter sends the start");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    assert_eq!(line["command"], "start");
    assert_eq!(line["args"]["workspace"], "/a1");
}

#[test]
fn tab_completes_the_focused_completion_and_lists_it() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/");
    answer(&mut app, &sent[0].0, &["a1", "a2"], "/");
    let sent = directories_of(&key(&mut app, Key::Tab));
    assert_eq!(typed_text(&app), Some("/a1/".to_owned()));
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["/a1/"]
    );
}

#[test]
fn tab_with_a_recent_or_nothing_focused_changes_nothing() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/x");
    answer(&mut app, &sent[0].0, &[], "/");
    assert_eq!(focused(&app), None);
    assert_eq!(key(&mut app, Key::Tab), Effect::None);
    assert_eq!(typed_text(&app), Some("/x".to_owned()));
    assert_eq!(key(&mut app, Key::Down), Effect::None);
    assert_eq!(focused(&app), Some("/w".to_owned()));
    assert_eq!(key(&mut app, Key::Tab), Effect::None);
    assert_eq!(typed_text(&app), Some("/x".to_owned()));
}

#[test]
fn enter_on_a_trailing_slash_reads_existence_from_its_parent() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/a1/");
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["/", "/a1/"]
    );
    answer(&mut app, &sent[1].0, &[], "/a1/");
    answer(&mut app, &sent[0].0, &["a1"], "/");
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(!picker_open(&app));
    assert_eq!(chips(&app)[0], "[a1]");
}

#[test]
fn enter_on_an_unlisted_trailing_slash_shows_no_such_directory() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/zz/");
    answer(&mut app, &sent[1].0, &[], "/zz/");
    answer(&mut app, &sent[0].0, &["a1"], "/");
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(picker_open(&app));
    assert!(missing(&app));
    assert_eq!(chips(&app)[0], "[w]");
    // The next keystroke clears it.
    type_path(&mut app, "z");
    assert!(!missing(&app));
}

#[test]
fn enter_on_a_full_name_with_no_match_shows_no_such_directory() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/a1/x");
    answer(&mut app, &sent[1].0, &[], "/a1/");
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(missing(&app));
}

#[test]
fn enter_on_root_with_an_empty_listing_opens_root() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/");
    answer(&mut app, &sent[0].0, &[], "/");
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(!picker_open(&app));
    assert_eq!(chips(&app)[0], "[/]");
}

#[test]
fn enter_on_relative_text_shows_no_such_directory_with_nothing_sent() {
    for text in ["foo", "~x/"] {
        let mut app = home();
        open(&mut app);
        let sent = type_path(&mut app, text);
        assert!(sent.is_empty());
        assert_eq!(key(&mut app, Key::Enter), Effect::None);
        assert!(missing(&app));
        assert!(picker_open(&app));
    }
}

#[test]
fn bare_tilde_asks_once_then_opens_home() {
    let mut app = home();
    open(&mut app);
    type_path(&mut app, "~");
    // The first Enter asks for the home listing, and nothing else.
    let Effect::Send(lines) = key(&mut app, Key::Enter) else {
        panic!("the first Enter asks for ~");
    };
    let sent = sent_directories(&lines);
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["~/"]
    );
    // A second Enter before the answer sends nothing.
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(picker_open(&app));
    answer(&mut app, &sent[0].0, &[], "/Users/me/");
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(!picker_open(&app));
    assert_eq!(chips(&app)[0], "[me]");
}

#[test]
fn bare_tilde_opens_root_for_a_home_path_of_slash() {
    let mut app = home();
    open(&mut app);
    type_path(&mut app, "~");
    let Effect::Send(lines) = key(&mut app, Key::Enter) else {
        panic!("the first Enter asks for ~");
    };
    let sent = sent_directories(&lines);
    answer(&mut app, &sent[0].0, &[], "/");
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert_eq!(chips(&app)[0], "[/]");
}

#[test]
fn bare_tilde_with_a_rejected_home_listing_shows_no_such_directory() {
    let mut app = home();
    open(&mut app);
    type_path(&mut app, "~");
    let Effect::Send(lines) = key(&mut app, Key::Enter) else {
        panic!("the first Enter asks for ~");
    };
    let sent = sent_directories(&lines);
    assert_eq!(sent.len(), 1);
    app.on_line(refused(&sent[0].0, "no home"));
    assert!(app.notice().is_none());
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(missing(&app));
}

#[test]
fn a_home_completion_opens_through_the_listed_path() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "~/t");
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["~/"]
    );
    answer(&mut app, &sent[0].0, &["tree"], "/root/");
    assert_eq!(focused(&app), Some("tree".to_owned()));
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(!picker_open(&app));
    assert_eq!(chips(&app)[0], "[tree]");
}

#[test]
fn one_listing_per_directory_typed_through_two_path_segments() {
    let mut app = home();
    open(&mut app);
    let mut sent = Vec::new();
    for ch in "/tmp/fi".chars() {
        let asked = directories_of(&key(&mut app, Key::Char(ch)));
        for (id, dir) in &asked {
            match dir.as_str() {
                "/" => answer(&mut app, id, &["tmp"], "/"),
                "/tmp/" => answer(&mut app, id, &["fi"], "/tmp/"),
                dir => panic!("one listing per directory, asked {dir}"),
            }
        }
        sent.extend(asked);
    }
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["/", "/tmp/"]
    );
}

#[test]
fn retyping_a_held_directory_sends_nothing_until_tab() {
    let mut app = home();
    open(&mut app);
    let mut sent = Vec::new();
    for ch in "/tmp/fi".chars() {
        let asked = directories_of(&key(&mut app, Key::Char(ch)));
        for (id, dir) in &asked {
            match dir.as_str() {
                "/" => answer(&mut app, id, &["tmp"], "/"),
                "/tmp/" => answer(&mut app, id, &["fi", "fix"], "/tmp/"),
                dir => panic!("one listing per directory, asked {dir}"),
            }
        }
        sent.extend(asked);
    }
    assert_eq!(sent.len(), 2);
    // Backspace to `/` and retype `tmp/`: every directory is held.
    for _ in 0..6 {
        assert_eq!(key(&mut app, Key::Backspace), Effect::None);
    }
    assert_eq!(typed_text(&app), Some("/".to_owned()));
    let sent = type_path(&mut app, "tmp/");
    assert!(sent.is_empty());
    assert_eq!(typed_text(&app), Some("/tmp/".to_owned()));
    // Backspace to the recents alone and retype `/`: still held.
    for _ in 0..5 {
        assert_eq!(key(&mut app, Key::Backspace), Effect::None);
    }
    assert_eq!(typed_text(&app), None);
    let sent = type_path(&mut app, "/");
    assert!(sent.is_empty());
    // Back to `/tmp/` and Tab on its completion: exactly one more.
    let sent = type_path(&mut app, "tmp/");
    assert!(sent.is_empty());
    assert_eq!(focused(&app), Some("fi".to_owned()));
    let sent = directories_of(&key(&mut app, Key::Tab));
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["/tmp/fi/"]
    );
}

#[test]
fn a_rejected_listing_shows_no_notice_and_is_never_asked_again() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/");
    app.on_line(refused(&sent[0].0, "unreadable"));
    assert!(app.notice().is_none());
    assert_eq!(recents(&app), vec!["/w".to_owned()]);
    assert_eq!(completions(&app), Vec::<String>::new());
    // Retyping asks nothing more for the failed directory.
    assert_eq!(key(&mut app, Key::Backspace), Effect::None);
    let sent = type_path(&mut app, "/");
    assert!(sent.is_empty());
}

#[test]
fn malformed_answers_keep_the_recents_with_no_notice() {
    let malformed = [
        json!({"path": "/x/"}),
        json!({"directories": ["a", 3], "path": "/x/"}),
        json!({"directories": ["a"]}),
        json!({"directories": ["a"], "path": "x/"}),
    ];
    for result in malformed {
        let mut app = home();
        open(&mut app);
        let sent = type_path(&mut app, "/");
        app.on_line(accepted(&sent[0].0, result.clone()));
        assert!(app.notice().is_none(), "result {result}");
        assert_eq!(recents(&app), vec!["/w".to_owned()]);
        assert_eq!(completions(&app), Vec::<String>::new());
    }
}

#[test]
fn foreign_ids_pass_the_picker_untouched() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/");
    app.on_line(accepted(
        "c_foreign",
        json!({"directories": ["x"], "path": "/"}),
    ));
    app.on_line(refused("c_other", "rejected"));
    assert!(app.notice().is_none());
    assert_eq!(typed_text(&app), Some("/".to_owned()));
    assert_eq!(focused(&app), None);
    // The picker's own answer still lands.
    answer(&mut app, &sent[0].0, &["a1"], "/");
    assert_eq!(focused(&app), Some("a1".to_owned()));
}

#[test]
fn an_answer_after_esc_matches_nothing_with_no_notice() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/");
    assert_eq!(key(&mut app, Key::Esc), Effect::None);
    assert!(
        app.on_line(accepted(
            &sent[0].0,
            json!({"directories": ["a1"], "path": "/"})
        ))
        .is_empty()
    );
    assert!(app.notice().is_none());
}

#[test]
fn an_answer_for_a_typed_past_directory_waits_until_typed_back() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/a/");
    answer(&mut app, &sent[1].0, &["x"], "/a/");
    type_path(&mut app, "b");
    assert_eq!(completions(&app), Vec::<String>::new());
    // The parent listing arrives late: stored, not drawn.
    answer(&mut app, &sent[0].0, &["a"], "/");
    assert_eq!(completions(&app), Vec::<String>::new());
    assert_eq!(focused(&app), None);
    // Typing back draws it with no new request.
    assert_eq!(key(&mut app, Key::Backspace), Effect::None);
    assert_eq!(completions(&app), vec!["x".to_owned()]);
    assert_eq!(focused(&app), Some("x".to_owned()));
}

#[test]
fn a_late_listing_keeps_its_focused_recent() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live("s_aaaaaaaaaaaaaaaa", "away", "/other"));
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    let sent = type_path(&mut app, "/a");
    assert_eq!(sent.len(), 1);
    assert_eq!(key(&mut app, Key::Down), Effect::None);
    assert_eq!(focused(&app), Some("/w".to_owned()));
    answer(&mut app, &sent[0].0, &["a1", "a2"], "/");
    assert_eq!(focused(&app), Some("/w".to_owned()));
    assert_eq!(completions(&app), vec!["a1".to_owned(), "a2".to_owned()]);
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert_eq!(chips(&app)[0], "[w]");
}

#[test]
fn a_late_listing_focuses_its_first_completion_without_a_recent() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/a");
    answer(&mut app, &sent[0].0, &["a1", "a2"], "/");
    assert_eq!(focused(&app), Some("a1".to_owned()));
}

#[test]
fn the_picker_queues_while_down_and_asks_at_hello() {
    let mut app = home();
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert_eq!(type_path(&mut app, "/"), Vec::new());
    let lines = commands(app.on_line(hello()));
    let sent = sent_directories(&lines.iter().map(ToString::to_string).collect::<Vec<_>>());
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["/"]
    );
    // Typing inside the name queues nothing more.
    assert!(type_path(&mut app, "a").is_empty());
}

#[test]
fn hello_with_no_picker_sends_no_directories() {
    let mut app = home();
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 2);
    assert!(
        sent_directories(&lines.iter().map(ToString::to_string).collect::<Vec<_>>()).is_empty()
    );
}

#[test]
fn a_disconnect_asks_in_flight_directories_once_more() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/");
    app.disconnected();
    assert_eq!(app.notice(), Some("Connection lost."));
    let lines = commands(app.on_line(hello()));
    let resent = sent_directories(&lines.iter().map(ToString::to_string).collect::<Vec<_>>());
    assert_eq!(resent.len(), 1);
    assert_eq!(resent[0].1, "/");
    assert_ne!(resent[0].0, sent[0].0);
    // An answer under the old id changes nothing.
    answer(&mut app, &sent[0].0, &["stale"], "/");
    assert_eq!(app.notice(), Some("Connection lost."));
    assert_eq!(focused(&app), None);
    answer(&mut app, &resent[0].0, &["a1"], "/");
    assert_eq!(focused(&app), Some("a1".to_owned()));
}

#[test]
fn a_disconnect_requeues_two_directories_in_send_order() {
    let mut app = home();
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert!(type_path(&mut app, "/").is_empty());
    assert!(type_path(&mut app, "a/").is_empty());
    app.disconnected();
    let lines = commands(app.on_line(hello()));
    let sent = sent_directories(&lines.iter().map(ToString::to_string).collect::<Vec<_>>());
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["/", "/a/"]
    );
}

#[test]
fn backspace_to_recents_while_down_sends_nothing_at_hello() {
    let mut app = home();
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert!(type_path(&mut app, "/a").is_empty());
    assert_eq!(key(&mut app, Key::Backspace), Effect::None);
    assert_eq!(key(&mut app, Key::Backspace), Effect::None);
    assert_eq!(typed_text(&app), None);
    let lines = commands(app.on_line(hello()));
    assert!(
        sent_directories(&lines.iter().map(ToString::to_string).collect::<Vec<_>>()).is_empty()
    );
}

#[test]
fn enter_with_a_typed_text_is_inert_while_down() {
    // A focused completion with its listing held: Enter does nothing
    // while down, and chooses once back up.
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/");
    answer(&mut app, &sent[0].0, &["a1"], "/");
    assert_eq!(focused(&app), Some("a1".to_owned()));
    app.disconnected();
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(picker_open(&app));
    assert_eq!(chips(&app)[0], "[w]");
    // Nothing is in flight, so hello asks nothing more...
    let lines = commands(app.on_line(hello()));
    assert!(
        sent_directories(&lines.iter().map(ToString::to_string).collect::<Vec<_>>()).is_empty()
    );
    // ...and the same Enter chooses.
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert_eq!(chips(&app)[0], "[a1]");
}

#[test]
fn enter_on_a_held_or_unheld_text_is_inert_while_down() {
    // A resolvable text with its listings held: Enter does nothing
    // while down, and chooses once back up.
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/a1/");
    answer(&mut app, &sent[1].0, &[], "/a1/");
    answer(&mut app, &sent[0].0, &["a1"], "/");
    app.disconnected();
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(picker_open(&app));
    app.on_line(hello());
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert_eq!(chips(&app)[0], "[a1]");
    // An unheld text: Enter does nothing while down, and waits for
    // the re-asked listing once back up.
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/");
    app.disconnected();
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(picker_open(&app));
    let lines = commands(app.on_line(hello()));
    let resent = sent_directories(&lines.iter().map(ToString::to_string).collect::<Vec<_>>());
    assert_eq!(resent.len(), 1);
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(picker_open(&app));
    answer(&mut app, &resent[0].0, &[], "/");
    assert_ne!(resent[0].0, sent[0].0);
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert_eq!(chips(&app)[0], "[/]");
}

#[test]
fn enter_on_a_focused_recent_is_inert_while_down() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live("s_aaaaaaaaaaaaaaaa", "away", "/other"));
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    type_path(&mut app, "/a");
    assert_eq!(key(&mut app, Key::Down), Effect::None);
    assert_eq!(focused(&app), Some("/w".to_owned()));
    app.disconnected();
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(picker_open(&app));
    assert_eq!(chips(&app)[0], "[w]");
    app.on_line(hello());
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert_eq!(chips(&app)[0], "[w]");
}

#[test]
fn enter_with_the_recents_alone_chooses_while_down() {
    let mut app = home();
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert_eq!(key(&mut app, Key::Down), Effect::None);
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(!picker_open(&app));
}

#[test]
fn a_new_opening_starts_empty_and_asks_again() {
    let mut app = home();
    open(&mut app);
    let sent = type_path(&mut app, "/");
    answer(&mut app, &sent[0].0, &["a1"], "/");
    assert_eq!(key(&mut app, Key::Esc), Effect::None);
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert_eq!(typed_text(&app), None);
    let sent = type_path(&mut app, "/");
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["/"]
    );
}

#[test]
fn backspace_past_the_text_returns_the_selection_to_the_first_recent() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live("s_aaaaaaaaaaaaaaaa", "here", "/w"));
    app.on_line(live("s_bbbbbbbbbbbbbbbb", "away", "/other"));
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert_eq!(key(&mut app, Key::Down), Effect::None);
    assert_eq!(focused(&app), Some("/other".to_owned()));
    key(&mut app, Key::Char('x'));
    assert_eq!(key(&mut app, Key::Backspace), Effect::None);
    assert_eq!(focused(&app), Some("/w".to_owned()));
}
