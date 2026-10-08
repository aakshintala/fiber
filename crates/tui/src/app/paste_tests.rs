//! Tests for Ctrl+V's gate and its result: one read at a time, results
//! landing only in their draft, and every context swallowing the key
//! (`docs/tui.md`, "The input box", "Bindings").

use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Map, Value, json};

use contract::clock::Clock;

use super::{Landed, Paste};
use crate::app::{App, Effect};
use crate::editor::Target;
use crate::home::{Launch, Spot};
use crate::keys::{Edit, Key};
use crate::link::Line;
use crate::stroke::Stroke;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// The stroke `name` names.
fn stroke(name: &str) -> Stroke {
    Stroke::parse(name).unwrap()
}

/// Presses the key `name` names, through the effective bindings.
fn press(app: &mut App, name: &str) -> Effect {
    app.on_press(stroke(name), now())
}

/// Types `text` through the effective bindings.
fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        press(app, &ch.to_string());
    }
}

/// An app connected to the hub and attached to [`S_A`].
fn attached() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    assert!(app.on_line(hello()).is_empty());
    app.attach(contract::SessionId(S_A.to_owned()));
    app
}

/// An app on home at 80x24.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
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

/// A session line of `kind` from `session`.
fn session_line(session: &str, kind: &str, payload: Value, action: Option<&str>) -> Line {
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

/// An approval request from [`S_A`], opening the approval panel.
fn approval() -> Line {
    session_line(
        S_A,
        "permission_requested",
        json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "standing_ask", "standing_rule": {"scope": "global", "prefix": "p"}}),
        Some("a_1"),
    )
}

/// An `ask_user` question from [`S_A`], opening the question form.
fn question() -> Line {
    session_line(
        S_A,
        "interaction_requested",
        json!({"request_id": "r_2", "kind": "form", "fields": [
            {"header": "Pick", "question": "Which?", "multiSelect": false,
             "options": [{"label": "a"}, {"label": "b"}]}]}),
        None,
    )
}

/// An offer of one MCP server from [`S_A`], opening the offer.
fn offer() -> Line {
    session_line(
        S_A,
        "repository_code_offered",
        json!({"request_id": "r_1", "items": [
            {"kind": "mcp_server", "name": "a", "hash": "h", "required": false,
             "summary": "MCP server: a"}]}),
        None,
    )
}

/// A steering queue of two selectable rows on [`S_A`].
fn steering() -> Line {
    let messages = ["one", "two"]
        .iter()
        .enumerate()
        .map(|(at, text)| {
            json!({"content": [{"type": "text", "text": text}],
                   "source": "driver", "command_id": format!("c_{}", at + 1)})
        })
        .collect::<Vec<_>>();
    session_line(S_A, "steering_queue", json!({"messages": messages}), None)
}

/// Selects the newest steering row.
fn select_newest(app: &mut App) {
    assert!(app.on_line(steering()).is_empty());
    assert_eq!(press(app, "alt+up"), Effect::None);
    assert_eq!(app.input().expand(), "two");
}

#[test]
fn press_while_running_does_nothing() {
    // G1: a second press reads nothing; after the running ticket lands, a
    // press reads again.
    let mut paste = Paste::default();
    assert_eq!(paste.press(7), Effect::ReadImage(0));
    assert_eq!(paste.press(7), Effect::None);
    assert!(matches!(
        paste.land(0, 7, Err("No image on the clipboard.".to_owned())),
        Landed::Notice(_)
    ));
    assert_eq!(paste.press(7), Effect::ReadImage(1));
}

#[test]
fn a_result_for_another_ticket_changes_nothing() {
    // G2: another ticket's result drops, and the read still runs.
    let mut paste = Paste::default();
    assert_eq!(paste.press(7), Effect::ReadImage(0));
    assert!(matches!(
        paste.land(1, 7, Ok("AAA".to_owned())),
        Landed::Dropped
    ));
    assert_eq!(paste.press(7), Effect::None);
}

#[test]
fn a_result_for_a_stale_draft_drops_and_clears() {
    // G3: the running ticket with another serial drops, and the gate
    // reopens.
    let mut paste = Paste::default();
    assert_eq!(paste.press(7), Effect::ReadImage(0));
    assert!(matches!(
        paste.land(0, 8, Ok("AAA".to_owned())),
        Landed::Dropped
    ));
    assert_eq!(paste.press(8), Effect::ReadImage(1));
}

#[test]
fn a_matching_failure_is_a_notice() {
    // G4.
    let mut paste = Paste::default();
    assert_eq!(paste.press(7), Effect::ReadImage(0));
    match paste.land(0, 7, Err("No image on the clipboard.".to_owned())) {
        Landed::Notice(notice) => assert_eq!(notice, "No image on the clipboard."),
        Landed::Image(_) | Landed::Dropped => panic!("a matching failure is a notice"),
    }
}

#[test]
fn a_matching_image_lands() {
    // G5.
    let mut paste = Paste::default();
    assert_eq!(paste.press(7), Effect::ReadImage(0));
    match paste.land(0, 7, Ok("AAA".to_owned())) {
        Landed::Image(data) => assert_eq!(&*data, "AAA"),
        Landed::Notice(_) | Landed::Dropped => panic!("a matching image lands"),
    }
}

#[test]
fn a_second_ctrl_v_while_one_reads_does_nothing() {
    // A1.
    let mut app = attached();
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(0));
    assert_eq!(press(&mut app, "ctrl+v"), Effect::None);
}

#[test]
fn a_result_for_another_ticket_changes_nothing_on_the_app() {
    // A2: the draft stays, and the read still runs.
    let mut app = attached();
    type_text(&mut app, "look");
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(0));
    app.on_image(1, Ok("AAA".to_owned()));
    assert_eq!(app.input().expand(), "look");
    assert_eq!(press(&mut app, "ctrl+v"), Effect::None);
}

#[test]
fn a_result_after_enter_is_dropped() {
    // A3: Enter renews the serial, so the running ticket's image drops.
    let mut app = attached();
    type_text(&mut app, "look");
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(0));
    assert!(matches!(press(&mut app, "enter"), Effect::Send(_)));
    app.on_image(0, Ok("AAA".to_owned()));
    assert_eq!(app.input().expand(), "");
}

#[test]
fn a_result_after_ctrl_c_is_dropped() {
    // A3: Ctrl+C on a non-empty box clears it and renews the serial.
    let mut app = attached();
    type_text(&mut app, "look");
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(0));
    assert_eq!(press(&mut app, "ctrl+c"), Effect::None);
    app.on_image(0, Ok("AAA".to_owned()));
    assert_eq!(app.input().expand(), "");
}

#[test]
fn a_result_after_home_is_dropped() {
    // A3: /home clears the draft and renews the serial.
    let mut app = home();
    type_text(&mut app, "look");
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(0));
    type_text(&mut app, "/home");
    press(&mut app, "enter");
    app.on_image(0, Ok("AAA".to_owned()));
    assert_eq!(app.input().expand(), "");
}

#[test]
fn a_result_after_a_steering_selection_is_dropped() {
    // A3: selecting a steering row loads its text under a new serial.
    let mut app = attached();
    type_text(&mut app, "mine");
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(0));
    select_newest(&mut app);
    app.on_image(0, Ok("AAA".to_owned()));
    assert_eq!(app.input().expand(), "two");
}

#[test]
fn a_result_after_the_editor_returns_the_draft_is_dropped() {
    // A3: returning the whole draft renews the serial.
    let mut app = attached();
    type_text(&mut app, "look");
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(0));
    app.editor_returned(Target::Draft, Ok("changed".to_owned()));
    app.on_image(0, Ok("AAA".to_owned()));
    assert_eq!(app.input().expand(), "changed");
}

#[test]
fn a_result_after_a_token_edit_lands() {
    // A3: editing one paste token keeps the draft, so the image lands.
    let mut app = attached();
    app.on_edit(Edit::Paste(
        "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nl11\n".to_owned(),
    ));
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(0));
    app.editor_returned(Target::Token(1), Ok("short".to_owned()));
    app.on_image(0, Ok("AAA".to_owned()));
    assert!(app.input().has_image());
}

#[test]
fn a_failed_read_is_a_notice_and_the_draft_stays() {
    // A4.
    let mut app = attached();
    type_text(&mut app, "look");
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(0));
    app.on_image(0, Err("No image on the clipboard.".to_owned()));
    assert_eq!(app.input().expand(), "look");
    let firsts: Vec<String> = app
        .notices()
        .iter()
        .map(|notice| notice.rows[0].trim_end_matches(['✕', ' ']).to_owned())
        .collect();
    assert!(firsts.contains(&"No image on the clipboard.".to_owned()));
}

#[test]
fn the_image_lands_at_the_cursor() {
    // A5: "ab" + image + "cd".
    let mut app = attached();
    type_text(&mut app, "abcd");
    press(&mut app, "left");
    press(&mut app, "left");
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(0));
    app.on_image(0, Ok("AAA".to_owned()));
    assert_eq!(app.input().expand(), "ab[Image #1]cd");
}

/// Ctrl+V with `overlay` open does nothing and leaves the draft.
fn stays_shut(app: &mut App) {
    assert_eq!(app.input().expand(), "hi");
    assert_eq!(press(app, "ctrl+v"), Effect::None);
    assert_eq!(app.input().expand(), "hi");
}

#[test]
fn ctrl_v_does_nothing_with_the_key_map_open() {
    let mut app = attached();
    type_text(&mut app, "hi");
    assert_eq!(press(&mut app, "f1"), Effect::None);
    assert!(app.keymap_top().is_some());
    stays_shut(&mut app);
    assert!(app.keymap_top().is_some());
}

#[test]
fn ctrl_v_does_nothing_with_the_approval_panel_open() {
    let mut app = attached();
    type_text(&mut app, "hi");
    assert!(app.on_line(approval()).is_empty());
    assert!(app.panel().is_some());
    stays_shut(&mut app);
    assert!(app.panel().is_some());
}

#[test]
fn ctrl_v_does_nothing_with_the_question_form_open() {
    let mut app = attached();
    type_text(&mut app, "hi");
    assert!(app.on_line(question()).is_empty());
    assert!(app.panel().is_some());
    stays_shut(&mut app);
    assert!(app.panel().is_some());
}

#[test]
fn ctrl_v_does_nothing_with_the_offer_open() {
    let mut app = attached();
    type_text(&mut app, "hi");
    assert!(app.on_line(offer()).is_empty());
    assert!(app.offer_open());
    stays_shut(&mut app);
    assert!(app.offer_open());
}

#[test]
fn ctrl_v_does_nothing_with_the_ctrl_r_panel_open() {
    let mut app = attached();
    type_text(&mut app, "hi");
    press(&mut app, "ctrl+r");
    assert!(app.completions().is_some());
    stays_shut(&mut app);
    assert!(app.completions().is_some());
}

#[test]
fn ctrl_v_does_nothing_with_the_slash_panel_open() {
    let mut app = attached();
    type_text(&mut app, "/");
    assert!(app.completions().is_some());
    assert_eq!(press(&mut app, "ctrl+v"), Effect::None);
    assert_eq!(app.input().expand(), "/");
    assert!(app.completions().is_some());
}

#[test]
fn ctrl_v_does_nothing_with_the_at_panel_open() {
    let mut app = attached();
    // `@` opens the panel before its results arrive; results keep it open
    // in the overlay context.
    assert_eq!(press(&mut app, "@"), Effect::ListFiles);
    assert!(app.files_open());
    assert_eq!(press(&mut app, "ctrl+v"), Effect::None);
    assert_eq!(app.input().expand(), "@");
    assert!(app.files_open());
    app.on_files(app.generation(), Ok(vec!["a.txt".to_owned()]));
    assert!(app.completions().is_some());
    assert_eq!(press(&mut app, "ctrl+v"), Effect::None);
    assert_eq!(app.input().expand(), "@");
    assert!(app.completions().is_some());
}

#[test]
fn ctrl_v_does_nothing_with_the_search_bar_open() {
    let mut app = attached();
    type_text(&mut app, "hi");
    assert_eq!(app.find_key(&Key::CtrlF), Some(Effect::None));
    assert!(app.find_open());
    stays_shut(&mut app);
    assert!(app.find_open());
}

#[test]
fn ctrl_v_does_nothing_with_the_results_view_open() {
    let mut app = attached();
    type_text(&mut app, "hi");
    assert_eq!(app.find_key(&Key::CtrlF), Some(Effect::None));
    assert_eq!(app.find_key(&Key::CtrlF), Some(Effect::None));
    assert!(app.results_open());
    stays_shut(&mut app);
    assert!(app.results_open());
}

#[test]
fn ctrl_v_does_nothing_while_navigating() {
    let mut app = attached();
    type_text(&mut app, "hi");
    app.drawn(&[crate::mouse::Target {
        id: crate::mouse::TargetId::Badge,
        rect: ratatui::layout::Rect::new(0, 0, 1, 1),
    }]);
    assert_eq!(press(&mut app, "shift+tab"), Effect::None);
    assert!(app.focused().is_some());
    stays_shut(&mut app);
    assert!(app.focused().is_some());
}

/// Links the app: the feed and recent ids, both waiting for answers.
fn linked(app: &mut App) -> (String, String) {
    let lines: Vec<Value> = app
        .on_line(hello())
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
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

/// A live `session_status` for `session` in `state`.
fn live(session: &str, name: &str, workspace: &str, project: &str, state: Value) -> Line {
    let mut payload = json!({
        "name": name,
        "workspace": workspace,
        "project": project,
        "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
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

/// An exited session row for the recent answer.
fn exited(session: &str, name: &str) -> Value {
    json!({
        "session_id": session,
        "ts": 0,
        "project": "-w",
        "workspace": "/w",
        "name": name,
        "how": "exited",
    })
}

/// The home rows' keys, in order.
fn row_keys(app: &App) -> Vec<u64> {
    app.home_screen()
        .map(|screen| screen.rows.into_iter().map(|(key, _, _)| key).collect())
        .unwrap_or_default()
}

#[test]
fn ctrl_v_does_nothing_with_the_quit_question_open() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(S_A, "here", "/w", "-w", json!({"state": "streaming"})));
    type_text(&mut app, "hi");
    app.quit();
    assert!(app.quit_open());
    stays_shut(&mut app);
    assert!(app.quit_open());
}

#[test]
fn ctrl_v_does_nothing_with_the_delete_question_open() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [exited(S_A, "old work")]}),
    ));
    type_text(&mut app, "hi");
    let key = row_keys(&app)[0];
    app.home_click(Spot::Stop(key));
    assert!(
        app.home_screen()
            .and_then(|screen| screen.question)
            .is_some()
    );
    stays_shut(&mut app);
    assert!(
        app.home_screen()
            .and_then(|screen| screen.question)
            .is_some()
    );
}

#[test]
fn ctrl_v_does_nothing_with_the_picker_open() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(S_A, "here", "/w", "-w", json!({"state": "idle"})));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "away",
        "/other",
        "-w",
        json!({"state": "idle"}),
    ));
    type_text(&mut app, "hi");
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert!(app.home_screen().and_then(|screen| screen.picker).is_some());
    stays_shut(&mut app);
    assert!(app.home_screen().and_then(|screen| screen.picker).is_some());
}

/// An app with the person's `keys` set.
fn with_keys(entries: &[(&str, Value)]) -> App {
    let mut app = attached();
    let mut user = Map::new();
    for (id, value) in entries {
        user.insert((*id).to_owned(), (*value).clone());
    }
    app.set_keys(crate::KeysSetup { user });
    app
}

#[test]
fn a_rebound_paste_image_reads_only_in_input_and_steering() {
    // B1: with paste_image on Alt+V, Alt+V reads in the input box and with
    // a steering row selected, but not behind the `/` panel; Ctrl+V reads
    // nowhere there.
    let mut app = with_keys(&[("paste_image", json!("alt+v"))]);
    assert_eq!(press(&mut app, "alt+v"), Effect::ReadImage(0));
    let mut app = with_keys(&[("paste_image", json!("alt+v"))]);
    select_newest(&mut app);
    assert_eq!(press(&mut app, "alt+v"), Effect::ReadImage(0));
    let mut app = with_keys(&[("paste_image", json!("alt+v"))]);
    app.on_edit(Edit::Paste("/".to_owned()));
    assert!(app.completions().is_some());
    assert_eq!(press(&mut app, "alt+v"), Effect::None);
    assert!(app.completions().is_some());
    assert_eq!(press(&mut app, "ctrl+v"), Effect::None);
    assert!(app.completions().is_some());
    let mut app = with_keys(&[("paste_image", json!("alt+v"))]);
    type_text(&mut app, "hi");
    assert_eq!(press(&mut app, "ctrl+v"), Effect::None);
    assert_eq!(app.input().expand(), "hi");
}

/// The id of the single line a send carries.
fn sent_id(effect: Effect) -> String {
    match effect {
        Effect::Send(lines) => {
            assert_eq!(lines.len(), 1);
            let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
            line["id"]
                .as_str()
                .unwrap_or_else(|| panic!("no id"))
                .to_owned()
        }
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_) => panic!("nothing sent"),
    }
}

/// Rejects the prompt `id` on the session: its draft comes back.
fn reject(app: &mut App, id: &str) {
    let rejected = session_line(
        S_A,
        "command_rejected",
        json!({"command_id": id, "code": "busy", "message": "Busy."}),
        None,
    );
    assert!(app.on_line(rejected).is_empty());
}

#[test]
fn a_result_after_enter_and_rejection_is_dropped() {
    // A6: the rejected prompt's draft comes back with a fresh serial, so
    // the running ticket's result drops and the gate stays shut until it
    // does; the next Ctrl+V reads.
    let mut app = attached();
    type_text(&mut app, "look");
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(0));
    let id = sent_id(press(&mut app, "enter"));
    reject(&mut app, &id);
    assert_eq!(app.input().expand(), "look");
    assert_eq!(press(&mut app, "ctrl+v"), Effect::None);
    app.on_image(0, Ok("AAA".to_owned()));
    assert_eq!(app.input().expand(), "look");
    assert!(!app.input().has_image());
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(1));
}

#[test]
fn a_result_after_a_steering_selection_and_esc_is_dropped() {
    // A7: the stash comes back through put_back, with a fresh serial.
    let mut app = attached();
    type_text(&mut app, "look");
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(0));
    select_newest(&mut app);
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert_eq!(app.input().expand(), "look");
    app.on_image(0, Ok("AAA".to_owned()));
    assert_eq!(app.input().expand(), "look");
    assert!(!app.input().has_image());
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(1));
}

#[test]
fn a_result_after_an_amend_is_dropped() {
    // A7b: the same through an amend.
    let mut app = attached();
    type_text(&mut app, "look");
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(0));
    select_newest(&mut app);
    assert!(matches!(press(&mut app, "enter"), Effect::Send(_)));
    assert_eq!(app.input().expand(), "look");
    app.on_image(0, Ok("AAA".to_owned()));
    assert_eq!(app.input().expand(), "look");
    assert!(!app.input().has_image());
    assert_eq!(press(&mut app, "ctrl+v"), Effect::ReadImage(1));
}
