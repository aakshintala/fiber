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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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
/// answers. A remote picker's home listing goes out beside them.
fn linked(app: &mut App) -> (String, String) {
    let lines: Vec<Value> = commands(app.on_line(hello()))
        .into_iter()
        .filter(|line| line["command"] != "directories")
        .collect();
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
        | Effect::Feedback { .. }
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

/// Opens the picker on a linked app, cleared for a path typed from
/// scratch.
fn open(app: &mut App) {
    linked(app);
    clear_open(app);
}

/// Clicks the workspace chip, then gives the picker a clean empty typed
/// row, as Backspace past the root leaves it, for a path typed from
/// scratch: the root's listing is forgotten, so no answer reaches it.
fn clear_open(app: &mut App) {
    app.home_click(Spot::Workspace);
    let picker = app
        .home
        .as_mut()
        .and_then(|home| home.chooser.picker.as_mut())
        .unwrap_or_else(|| panic!("the picker is open"));
    picker.typed = super::Typed::new();
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
        .map(|screen| screen.chips.into_iter().map(|chip| chip.text).collect())
        .unwrap_or_default()
}

/// Answers the `directories` request `id` with `names` at `path`.
fn answer(app: &mut App, id: &str, names: &[&str], path: &str) {
    app.on_line(accepted(id, json!({"directories": names, "path": path})));
}

fn remote() -> App {
    let mut app = home();
    app.set_home(Launch {
        location: crate::LaunchLocation::Remote {
            hub: "box".to_owned(),
        },
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
    assert_eq!(app.home_workspace(), None);
    assert_eq!(chips(&app)[0], "[no workspace]");
    assert!(app.home_screen().unwrap().chips[0].dim);
    assert!(!app.home_screen().unwrap().picker.unwrap().escapes);
    assert_eq!(key(&mut app, Key::Esc), Effect::None);
    assert!(picker_open(&app));
    assert!(!chips(&app).iter().any(|chip| chip.contains("worktree")));
    linked(&mut app);
    app.on_line(live("s_aaaaaaaaaaaaaaaa", "chosen", "/remote/w"));
    assert_eq!(recents(&app), ["/remote/w"]);
    key(&mut app, Key::Down);
    key(&mut app, Key::Enter);
    assert_eq!(app.home_workspace(), Some(PathBuf::from("/remote/w")));
    assert_eq!(chips(&app)[0], "[w]");
    assert!(!app.home_screen().unwrap().chips[0].dim);
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
    assert_eq!(typed_text(&app), Some("~/".to_owned()));
    // A key ends the refill: the recents stay as they were.
    key(&mut app, Key::Down);
    key(&mut app, Key::Down);
    assert_eq!(focused(&app).as_deref(), Some("/b"));
    app.on_line(live("s_0000000000000000", "changed", "/z"));
    assert_eq!(recents(&app), ["/a", "/b", "/c"]);
    assert_eq!(focused(&app).as_deref(), Some("/b"));
}

#[test]
fn remote_refill_focuses_first_when_the_selected_workspace_leaves() {
    let mut app = remote();
    linked(&mut app);
    app.on_line(live("s_aaaaaaaaaaaaaaaa", "first", "/a"));
    app.on_line(live("s_bbbbbbbbbbbbbbbb", "second", "/b"));
    // Backspace past `~/` leaves the recents alone, refilling again.
    key(&mut app, Key::Backspace);
    key(&mut app, Key::Backspace);
    assert_eq!(typed_text(&app), None);
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
    for _ in "~/x".chars() {
        key(&mut app, Key::Backspace);
    }
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
        key(&mut app, Key::Down);
        key(&mut app, Key::Enter);
        assert_eq!(
            app.home_workspace(),
            Some(PathBuf::from("/sentinel/launch-dir"))
        );
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
    clear_open(&mut app);
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
    clear_open(&mut app);
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
    assert_eq!(app.home_picker_key(&Key::CtrlC), None);
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
    clear_open(&mut app);
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
    clear_open(&mut app);
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
    // An answer under the old id changes nothing; the hello already
    // ended the drop's cause, so its notice is gone with the reconnect.
    answer(&mut app, &sent[0].0, &["stale"], "/");
    assert_eq!(app.notice(), None);
    assert_eq!(focused(&app), None);
    answer(&mut app, &resent[0].0, &["a1"], "/");
    assert_eq!(focused(&app), Some("a1".to_owned()));
}

#[test]
fn a_disconnect_requeues_two_directories_in_send_order() {
    let mut app = home();
    clear_open(&mut app);
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
    clear_open(&mut app);
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
    clear_open(&mut app);
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
    clear_open(&mut app);
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
    clear_open(&mut app);
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
    clear_open(&mut app);
    assert_eq!(key(&mut app, Key::Down), Effect::None);
    assert_eq!(focused(&app), Some("/other".to_owned()));
    key(&mut app, Key::Char('x'));
    assert_eq!(key(&mut app, Key::Backspace), Effect::None);
    assert_eq!(focused(&app), Some("/w".to_owned()));
}

#[test]
fn launch_location_exposes_only_its_own_machine() {
    let local = crate::LaunchLocation::Local {
        workspace: PathBuf::from("/launch"),
        project: "project-key".to_owned(),
        git: true,
    };
    assert_eq!(local.workspace(), Some(std::path::Path::new("/launch")));
    assert_eq!(local.project(), Some("project-key"));
    assert_eq!(local.hub(), None);
    assert_eq!(
        crate::LaunchLocation::default(),
        crate::LaunchLocation::Local {
            workspace: PathBuf::new(),
            project: String::new(),
            git: false,
        }
    );
    let remote = crate::LaunchLocation::Remote {
        hub: "box".to_owned(),
    };
    assert_eq!(remote.workspace(), None);
    assert_eq!(remote.project(), None);
    assert_eq!(remote.hub(), Some("box"));
}

#[test]
fn the_picker_refills_its_recents_until_a_key_touches_it() {
    let mut app = home();
    linked(&mut app);
    app.home_click(Spot::Workspace);
    assert_eq!(recents(&app), ["/w"]);
    app.on_line(live("s_aaaaaaaaaaaaaaaa", "new", "/arrived"));
    assert_eq!(recents(&app), ["/w", "/arrived"]);
    key(&mut app, Key::Down);
    app.on_line(live("s_bbbbbbbbbbbbbbbb", "later", "/later"));
    assert_eq!(recents(&app), ["/w", "/arrived"]);
}

#[test]
fn local_and_remote_pickers_share_one_layout() {
    let mut local = home();
    let mut remote = remote();
    let mut screens = Vec::new();
    for (app, root) in [(&mut local, "/w/"), (&mut remote, "~/")] {
        let sent = sent_directories(&app.on_line(hello()));
        app.on_line(live("s_aaaaaaaaaaaaaaaa", "row", "/a"));
        if !picker_open(app) {
            let more = directories_of(&app.home_click(Spot::Workspace));
            assert!(sent.is_empty());
            answer(app, &more[0].0, &["x", "y"], "/listed");
            assert_eq!(more[0].1, root);
        } else {
            answer(app, &sent[0].0, &["x", "y"], "/listed");
            assert_eq!(sent[0].1, root);
        }
        let picker = app.home_screen().unwrap().picker.unwrap();
        let typed = picker.typed.clone().unwrap();
        assert_eq!(typed.text, root);
        screens.push((picker, typed));
    }
    let (local, local_typed) = &screens[0];
    let (remote, remote_typed) = &screens[1];
    // The same rows: the typed row, its completions with the first
    // focused, then the recents; only the root and the launch row differ.
    assert_eq!(local_typed.completions, remote_typed.completions);
    assert_eq!(local_typed.completions, ["x", "y"]);
    assert_eq!(local.focus, Some(0));
    assert_eq!(remote.focus, Some(0));
    assert_eq!(local.recents, ["/w", "/a"]);
    assert_eq!(remote.recents, ["/a"]);
    // Esc closes only once a workspace is chosen: the remote has none.
    assert!(local.escapes);
    assert!(!remote.escapes);
}

/// Links a remote app, returning the id of its one home listing.
fn remote_linked() -> (App, String) {
    let mut app = remote();
    let sent = sent_directories(&app.on_line(hello()));
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["~/"]
    );
    let id = sent[0].0.clone();
    (app, id)
}

#[test]
fn remote_with_no_recents_opens_on_the_home_row_and_waits_for_it() {
    let unlinked = remote();
    assert_eq!(typed_text(&unlinked), Some("~/".to_owned()));
    assert!(completions(&unlinked).is_empty());
    let (mut app, id) = remote_linked();
    assert_eq!(typed_text(&app), Some("~/".to_owned()));
    assert!(completions(&app).is_empty());
    assert_eq!(focused(&app), None);
    // Enter before the answer does nothing.
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(picker_open(&app));
    assert_eq!(chips(&app)[0], "[no workspace]");
    answer(&mut app, &id, &["notes", "work", ".cache"], "/home/me");
    assert_eq!(
        completions(&app),
        vec!["notes".to_owned(), "work".to_owned()]
    );
    assert_eq!(focused(&app), Some("notes".to_owned()));
    assert!(!app.home_screen().unwrap().picker.unwrap().escapes);
    key(&mut app, Key::Down);
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(!picker_open(&app));
    assert_eq!(app.home_workspace(), Some(PathBuf::from("/home/me/work")));
}

#[test]
fn remote_home_row_tab_descends_into_the_focused_completion() {
    let (mut app, id) = remote_linked();
    answer(&mut app, &id, &["work"], "/home/me");
    let sent = directories_of(&key(&mut app, Key::Tab));
    assert_eq!(typed_text(&app), Some("~/work/".to_owned()));
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["~/work/"]
    );
    answer(&mut app, &sent[0].0, &["fiber"], "/home/me/work");
    assert_eq!(completions(&app), vec!["fiber".to_owned()]);
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert_eq!(
        app.home_workspace(),
        Some(PathBuf::from("/home/me/work/fiber"))
    );
}

#[test]
fn a_failed_home_listing_keeps_the_row_with_no_completions() {
    let (mut app, id) = remote_linked();
    app.on_line(refused(&id, "no home"));
    assert!(app.notice().is_none());
    assert!(picker_open(&app));
    assert_eq!(typed_text(&app), Some("~/".to_owned()));
    assert!(completions(&app).is_empty());
    assert_eq!(focused(&app), None);
}

#[test]
fn arriving_recents_list_below_the_untouched_home_row() {
    let (mut app, id) = remote_linked();
    app.on_line(live("s_aaaaaaaaaaaaaaaa", "here", "/remote/w"));
    assert_eq!(typed_text(&app), Some("~/".to_owned()));
    assert_eq!(recents(&app), ["/remote/w"]);
    // Enter waits for the home listing even with a recent below.
    assert_eq!(focused(&app), None);
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(picker_open(&app));
    answer(&mut app, &id, &["work"], "/home/me");
    assert_eq!(focused(&app).as_deref(), Some("work"));
    // Down walks past the completions into the recents.
    key(&mut app, Key::Down);
    assert_eq!(focused(&app).as_deref(), Some("/remote/w"));
    key(&mut app, Key::Enter);
    assert_eq!(app.home_workspace(), Some(PathBuf::from("/remote/w")));
}

#[test]
fn a_key_on_the_home_row_keeps_it_when_recents_arrive() {
    let (mut app, _) = remote_linked();
    type_path(&mut app, "w");
    app.on_line(live("s_aaaaaaaaaaaaaaaa", "here", "/remote/w"));
    assert_eq!(typed_text(&app), Some("~/w".to_owned()));
    assert!(recents(&app).is_empty());
}

#[test]
fn a_remote_reopening_lists_home_again_with_its_recents_below() {
    let (mut app, _) = remote_linked();
    app.on_line(live("s_aaaaaaaaaaaaaaaa", "here", "/remote/w"));
    key(&mut app, Key::Down);
    key(&mut app, Key::Enter);
    assert!(!picker_open(&app));
    // The root is now the last pick.
    let sent = directories_of(&app.home_click(Spot::Workspace));
    assert_eq!(
        sent.iter().map(|(_, dir)| dir.as_str()).collect::<Vec<_>>(),
        vec!["/remote/w/"]
    );
    assert_eq!(typed_text(&app), Some("/remote/w/".to_owned()));
    assert_eq!(recents(&app), ["/remote/w"]);
    assert!(app.home_screen().unwrap().picker.unwrap().escapes);
}

#[test]
fn the_local_picker_opens_on_its_recents_alone() {
    let mut app = home();
    open(&mut app);
    assert_eq!(typed_text(&app), None);
    assert_eq!(recents(&app), ["/w"]);
}

#[test]
fn empty_values_carry_no_workspace() {
    // An empty construction path is none, like the remote-home launch.
    assert_eq!(App::new(None).workspace(), None);
    assert_eq!(App::new(Some(PathBuf::from(""))).workspace(), None);
    assert_eq!(
        App::new(Some(PathBuf::from("/w"))).workspace(),
        Some(PathBuf::from("/w"))
    );
    // A default launch names an empty local directory: also none, with
    // no workspace key in the start arguments.
    let mut app = App::new(None);
    app.set_home(Launch::default());
    assert_eq!(app.workspace(), None);
    assert_eq!(app.home_workspace(), None);
    assert!(
        !app.start_args()
            .as_object()
            .unwrap_or_else(|| panic!("start args are an object"))
            .contains_key("workspace")
    );
    // An unreadable status row carries an empty workspace: it yields
    // none, so it never becomes a recent or the workspace in use.
    let mut app = remote();
    let Line::Session(mut envelope) = live("s_aaaaaaaaaaaaaaaa", "newer", "/remote/new") else {
        unreachable!("a live row is a session line");
    };
    envelope.schema_version = contract::SCHEMA_VERSION + 1;
    app.on_line(Line::Session(envelope));
    assert_eq!(app.home_workspace(), None);
    assert!(recents(&app).is_empty());
    // An empty choice clears the pick: no workspace is chosen.
    app.choose_workspace("/remote/w".to_owned());
    assert_eq!(app.home_workspace(), Some(PathBuf::from("/remote/w")));
    app.choose_workspace(String::new());
    assert_eq!(app.home_workspace(), None);
}

#[test]
fn an_empty_choice_clears_the_pick_and_enter_reopens_the_picker() {
    let mut app = remote();
    app.choose_workspace("/remote/w".to_owned());
    assert_eq!(app.home_workspace(), Some(PathBuf::from("/remote/w")));
    app.choose_workspace(String::new());
    assert_eq!(app.home_workspace(), None);
    // Enter on the prompt with no workspace reopens the picker
    // (`docs/tui.md`, "The workspace picker") instead of starting.
    key(&mut app, Key::Enter);
    assert!(picker_open(&app));
}

/// With no local workspace nothing reads the working directory: the
/// same remote-home app built with none reports none and draws the
/// same home frame from two different directories.
#[test]
fn no_workspace_never_reads_the_working_directory() {
    let first = fakes::TempDir::new("fiber-no-workspace-first");
    let second = fakes::TempDir::new("fiber-no-workspace-second");
    assert_ne!(first.path(), second.path());
    let previous = std::env::current_dir().unwrap_or_else(|err| panic!("cwd: {err}"));
    let mut frames = Vec::new();
    for dir in [&first, &second] {
        std::env::set_current_dir(dir.path()).unwrap_or_else(|err| panic!("chdir: {err}"));
        let mut app = App::new(None);
        app.set_home(Launch {
            location: crate::LaunchLocation::Remote {
                hub: "box".to_owned(),
            },
            version: "0.0.1".to_owned(),
            logo_glyph: "⌇".to_owned(),
            ..Default::default()
        });
        app.set_size(80, 24);
        // White-box on purpose: the readers below prefer home once it
        // is set, so only the field catches a construction that
        // smuggles the working directory in. (`workspace()`,
        // `home_workspace()` and the start args stay none by the same
        // empty-means-none mapping `empty_values_carry_no_workspace`
        // pins, so they assert nothing new here.)
        assert_eq!(app.workspace, None);
        let area = ratatui::layout::Rect::new(0, 0, 80, 24);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        crate::view::render(&app, area, &mut buf, None);
        frames.push(crate::view::text(&buf));
    }
    std::env::set_current_dir(&previous).unwrap_or_else(|err| panic!("restore cwd: {err}"));
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0], frames[1], "the home frame moves with the cwd");
    assert!(
        frames[0].contains("[no workspace]"),
        "the frame is the no-workspace home"
    );
}
