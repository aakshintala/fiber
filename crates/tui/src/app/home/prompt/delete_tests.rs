//! Tests for `/delete` on the session on screen: the discovery dry
//! run, the immutable question, cancellation, and the confirmed
//! stop/delete correlation (`docs/tui.md`, "Slash commands").

use super::super::super::{App, Effect};
use super::State;
use crate::app::testkit::{hello, home_with, sent};
use crate::home::{Launch, Spot};
use crate::keys::{Button, Key, Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::{Pointer, TargetId};
use contract::SessionId;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};
use std::path::PathBuf;

const A: &str = "s_aaaaaaaaaaaaaaaa";
const B: &str = "s_bbbbbbbbbbbbbbbb";
const C: &str = "s_cccccccccccccccc";
const D: &str = "s_dddddddddddddddd";

fn launch() -> Launch {
    Launch {
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
    }
}

/// Folds `hub_hello`: the feed and recent lines go out, both waiting
/// for their answers.
fn linked(app: &mut App) {
    let _lines: Vec<String> = app.on_line(hello());
}

/// A live `session_status` for `session` named `name`.
fn live(session: &str, name: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({
            "name": name,
            "workspace": "/w",
            "project": "-w",
            "state": "idle",
            "started": 0, "calls": 0, "since": 0,
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

/// A live status reporting active output.
fn working(session: &str, name: &str) -> Line {
    let mut status = live(session, name);
    if let Line::Session(envelope) = &mut status {
        envelope
            .payload
            .insert("state".to_owned(), Value::String("streaming".to_owned()));
    }
    status
}

/// A hub `session_left` for `session`, ending `how`.
fn left(session: &str, how: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"session_id": session, "how": how})
            .as_object()
            .cloned()
            .unwrap_or_default(),
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

/// A hub `command_rejected` for `id` with `code` and `message`.
fn rejected(id: &str, code: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": id, "code": code, "message": message})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// A delete refusal carrying its ordered busy ids.
fn rejected_with_held(id: &str, message: &str, held: &[&str]) -> Line {
    rejected_with_held_value(id, message, json!(held))
}

/// A delete refusal carrying an arbitrary structured `held` value.
fn rejected_with_held_value(id: &str, message: &str, held: Value) -> Line {
    let mut payload = json!({
        "command_id": id,
        "code": "session_held",
        "message": message
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    payload.insert("held".to_owned(), held);
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload,
    })
}

/// Types `text` and presses Enter, returning the effect.
fn slash(app: &mut App, text: &str) -> Effect {
    let now = fakes::clock::FakeClock::new().now();
    for ch in text.chars() {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    app.on_key(Key::Enter, now)
}

/// A linked home attached to `A`, with a live row naming it.
fn attached() -> App {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(A, "old work"));
    app.attach(SessionId(A.to_owned()));
    app
}

/// The dry-run command id `slash` sent, after pinning its shape.
fn discovery(app: &mut App) -> String {
    let lines = sent(slash(app, "/delete"));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["command"], "delete");
    assert_eq!(
        lines[0]["args"],
        json!({"session": A, "cascade": true, "dry_run": true})
    );
    assert_eq!(app.draft(), "");
    lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("discovery id"))
        .to_owned()
}

/// Answers the discovery with the accepted `sessions` and `held`, and
/// returns what folding it sent.
fn answer_discovery(app: &mut App, command: &str, sessions: &[&str], held: &[&str]) -> Vec<String> {
    app.on_line(accepted(
        command,
        json!({"sessions": sessions, "held": held}),
    ))
}

/// The command's stored question, while one shows.
fn question(app: &App) -> String {
    let State::Asked { question, .. } = &app
        .home
        .as_ref()
        .unwrap_or_else(|| panic!("home"))
        .questions
        .delete_command
        .state
    else {
        panic!("no open command question");
    };
    question.clone()
}

/// Whether the command's delete question draws.
fn open(app: &App) -> bool {
    app.home_question_open()
}

/// The frame at 80x24, with the targets it drew.
fn drawn(app: &App) -> (String, Vec<crate::mouse::Target>) {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    (crate::view::text(&buf), targets)
}

/// The frame's text at 80x24.
fn frame(app: &App) -> String {
    drawn(app).0
}

/// The `close` lines among the outgoing commands.
fn closes(lines: &[Value]) -> Vec<Value> {
    lines
        .iter()
        .filter(|line| line["command"] == "close")
        .cloned()
        .collect()
}

/// The `delete` lines among the outgoing commands.
fn deletes(lines: &[Value]) -> Vec<Value> {
    lines
        .iter()
        .filter(|line| line["command"] == "delete")
        .cloned()
        .collect()
}

/// Confirms the open question, returning the raw lines going out.
fn confirm(app: &mut App) -> Vec<String> {
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter confirms");
    };
    lines
}

/// Answers the discovery for `sessions` held by `held`, and confirms.
fn asked(app: &mut App, sessions: &[&str], held: &[&str]) -> Vec<String> {
    let command = discovery(app);
    assert!(answer_discovery(app, &command, sessions, held).is_empty());
    assert!(open(app));
    confirm(app)
}

#[test]
fn delete_sends_only_the_cascading_dry_run() {
    let mut app = attached();
    let command = discovery(&mut app);
    // Nothing draws until the hub answers: the conversation stays.
    assert!(!open(&app));
    assert!(!frame(&app).contains("Delete old work"));
    let _ = command;
}

#[test]
fn delete_with_no_session_says_so_and_clears_the_draft() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    assert_eq!(slash(&mut app, "/delete"), Effect::None);
    assert_eq!(app.notice(), Some("No session on screen."));
    assert_eq!(app.draft(), "");
}

#[test]
fn delete_with_the_link_down_sends_nothing_and_keeps_the_draft() {
    let mut app = attached();
    app.disconnected();
    assert_eq!(slash(&mut app, "/delete"), Effect::None);
    assert_eq!(app.draft(), "/delete");
}

#[test]
fn accepted_discovery_opens_the_question_naming_every_cascade_id() {
    let mut app = attached();
    let command = discovery(&mut app);
    assert!(
        answer_discovery(&mut app, &command, &[A, B, C], &[A]).is_empty(),
        "the answer sends nothing"
    );
    assert!(open(&app));
    // The target keeps its row title; cascade ids appear without rows.
    assert_eq!(
        question(&app),
        format!(
            "Delete old work ({A}) and 2 sessions that continue it: {B}, {C}? \
             They will be stopped and permanently deleted. It cannot be undone · Enter delete all · Esc keep"
        )
    );
    let shown = frame(&app);
    for id in [A, B, C] {
        assert!(shown.contains(id), "every candidate draws");
    }
    assert!(shown.contains("permanently deleted"), "{shown}");
    assert!(shown.contains("It cannot be undone."), "{shown}");
    insta::assert_snapshot!("cascade", frame(&app));
}

#[test]
fn target_only_set_asks_the_single_form() {
    let mut app = attached();
    let command = discovery(&mut app);
    assert!(answer_discovery(&mut app, &command, &[A], &[]).is_empty());
    assert!(open(&app));
    assert_eq!(
        question(&app),
        format!(
            "Delete old work ({A})? It will be stopped and permanently deleted. \
             It cannot be undone · Enter delete · Esc keep"
        )
    );
    insta::assert_snapshot!("single", frame(&app));
}

#[test]
fn long_cascade_keeps_the_permanence_warning_on_the_first_frame() {
    let mut app = attached();
    let command = discovery(&mut app);
    let ids: Vec<String> = (0..20u8).map(|n| format!("s_{n:016x}")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let mut sessions = vec![A];
    sessions.extend(refs.iter().take(20).copied());
    assert!(answer_discovery(&mut app, &command, &sessions, &[A]).is_empty());
    assert!(open(&app));
    // Twenty cascade ids wrap past the frame: the warning still shows
    // before any key, outside the scrolled list.
    let shown = frame(&app);
    assert!(shown.contains("It cannot be undone."), "{shown}");
    insta::assert_snapshot!("clipped", frame(&app));
}

#[test]
fn malformed_answers_show_a_notice_and_open_no_question() {
    for (sessions, held) in [
        (vec![A, A], vec![]),
        (vec![B], vec![]),
        (vec![], vec![]),
        (vec![A], vec![B]),
        (vec![A], vec![A, A]),
        (vec!["bogus"], vec![]),
        (vec![A], vec!["bogus"]),
    ] {
        let mut app = attached();
        let command = discovery(&mut app);
        let result = json!({"sessions": sessions, "held": held});
        assert!(app.on_line(accepted(&command, result)).is_empty());
        let notice = app.notice().unwrap_or_else(|| panic!("a notice"));
        assert!(notice.contains("Nothing was deleted"), "{notice}");
        assert!(!open(&app));
        assert!(!frame(&app).contains("Delete "));
    }
    // A missing `held` array is malformed the same way.
    let mut app = attached();
    let command = discovery(&mut app);
    assert!(
        app.on_line(accepted(&command, json!({"sessions": [A]})))
            .is_empty()
    );
    assert!(
        app.notice()
            .is_some_and(|notice| notice.contains("Nothing was deleted"))
    );
    assert!(!open(&app));
}

#[test]
fn rejected_discovery_shows_the_refusal_not_a_question() {
    let mut app = attached();
    let command = discovery(&mut app);
    assert!(
        app.on_line(rejected(
            &command,
            "session_held",
            "Session `s_aaaaaaaaaaaaaaaa` is held by another process."
        ))
        .is_empty()
    );
    assert_eq!(
        app.notice(),
        Some("Session `s_aaaaaaaaaaaaaaaa` is held by another process.")
    );
    assert!(!open(&app));
    assert!(!frame(&app).contains("Delete "));
}

#[test]
fn esc_cancels_and_keeps_the_attached_session() {
    let mut app = attached();
    let command = discovery(&mut app);
    assert!(answer_discovery(&mut app, &command, &[A, B], &[A]).is_empty());
    assert!(open(&app));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert!(!open(&app));
    // The same session stays attached: no close, delete or subscribe.
    assert_eq!(app.session(), Some(&SessionId(A.to_owned())));
    assert!(!frame(&app).contains("Delete old work"));
    // The command was consumed: the box takes new text.
    for ch in "/pwd".chars() {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    assert_eq!(app.draft(), "/pwd");
}

#[test]
fn the_cross_cancels_through_esc() {
    let mut app = attached();
    let command = discovery(&mut app);
    assert!(answer_discovery(&mut app, &command, &[A], &[]).is_empty());
    assert!(open(&app));
    let (_, targets) = drawn(&app);
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::Home(Spot::Keep)),
        "the ✕ draws"
    );
    assert_eq!(app.on_click(TargetId::Home(Spot::Keep)), Effect::None);
    assert!(!open(&app));
    assert_eq!(app.session(), Some(&SessionId(A.to_owned())));
}

#[test]
fn a_press_outside_cancels_through_esc() {
    let mut app = attached();
    let command = discovery(&mut app);
    assert!(answer_discovery(&mut app, &command, &[A], &[]).is_empty());
    assert!(open(&app));
    let (before, targets) = drawn(&app);
    assert!(before.contains("Delete old work"));
    let slab = crate::view::frame_of(&app, crate::app::Surface::DeleteQuestion);
    assert!(!slab.is_empty(), "the question draws its frame");
    assert!(!slab.contains(ratatui::layout::Position::new(0, 0)));
    let press = Mouse {
        kind: MouseKind::Press(Button::Left),
        col: 0,
        row: 0,
    };
    let mut pointer = Pointer::default();
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(
        app.on_mouse(&press, &targets, &mut pointer, true, now),
        Effect::None
    );
    assert!(!open(&app));
    assert_eq!(app.session(), Some(&SessionId(A.to_owned())));
}

#[test]
fn a_late_discovery_reply_after_cancel_opens_nothing() {
    let mut app = attached();
    let command = discovery(&mut app);
    assert!(answer_discovery(&mut app, &command, &[A], &[]).is_empty());
    assert!(open(&app));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert!(!open(&app));
    // The hub's late duplicate answers nothing new: no revived question,
    // no notice, nothing sent.
    assert!(
        app.on_line(accepted(&command, json!({"sessions": [A], "held": []})))
            .is_empty()
    );
    assert!(!open(&app));
    assert!(app.notice().is_none());
    assert!(!frame(&app).contains("Delete "));
}

#[test]
fn a_newer_command_invalidates_the_older_reply() {
    let mut app = attached();
    let first = discovery(&mut app);
    let second = discovery(&mut app);
    assert_ne!(first, second);
    // The older answer cannot revive or replace the newer question.
    assert!(
        app.on_line(accepted(&first, json!({"sessions": [A], "held": []})))
            .is_empty()
    );
    assert!(!open(&app));
    assert!(
        app.on_line(accepted(&second, json!({"sessions": [A, B], "held": [A]})))
            .is_empty()
    );
    assert!(open(&app));
    assert!(question(&app).contains(B));
}

#[test]
fn confirm_stops_every_held_id_with_now_and_nothing_else() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    app.on_line(live(D, "elsewhere"));
    let raw = asked(&mut app, &[A, B], &[A, B]);
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    // One `close` with `now` per held id, and a summary subscribe where
    // this connection held nothing.
    let died = closes(&lines);
    assert_eq!(died.len(), 2);
    for (line, id) in died.iter().zip([A, B]) {
        assert_eq!(line["command"], "close");
        assert_eq!(line["args"], json!({"now": true}));
        assert_eq!(line["session_id"], id);
    }
    let mut subscribed: Vec<&str> = lines
        .iter()
        .filter(|line| line["command"] == "subscribe")
        .filter_map(|line| line["session_id"].as_str())
        .collect();
    subscribed.sort();
    assert_eq!(subscribed, [A, B]);
    // No delete goes out before every held id reports its exit.
    assert!(deletes(&lines).is_empty());
    // The unrelated live session is never stopped.
    assert!(
        lines.iter().all(|line| line["session_id"] != D),
        "{lines:?}"
    );
}

#[test]
fn no_feed_delegate_is_closed_but_does_not_gate_delete() {
    let mut app = attached();
    // B represents a live delegate that is absent from the session feed.
    let raw = asked(&mut app, &[A, B], &[A, B]);
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    let stopped = closes(&lines);
    assert_eq!(stopped.len(), 2);
    assert_eq!(
        stopped
            .iter()
            .map(|line| line["session_id"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        [A, B]
    );
    assert!(deletes(&lines).is_empty());

    // A hypothetical delegate exit is not feed evidence and cannot gate
    // or trigger the exact-set delete.
    assert!(deletes(&fold_lines(&mut app, left(B, "exited"))).is_empty());
    let deletion = deletes(&fold_lines(&mut app, left(A, "exited")));
    assert_eq!(deletion.len(), 1);
    assert_eq!(
        deletion[0]["args"],
        json!({"session": A, "cascade": true, "expect": [A, B]})
    );
    assert!(deletes(&fold_lines(&mut app, left(A, "exited"))).is_empty());
}

#[test]
fn an_old_exited_row_does_not_count_as_fresh_stop_evidence() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    app.on_line(left(B, "exited"));
    let raw = asked(&mut app, &[A, B], &[A, B]);
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    let stopped = closes(&lines);
    assert_eq!(stopped.len(), 2, "the old Left row still gets a close-now");
    assert!(stopped.iter().any(|line| line["session_id"] == B));
    assert!(deletes(&lines).is_empty());

    assert!(deletes(&fold_lines(&mut app, left(A, "exited"))).is_empty());
    let deletion = deletes(&fold_lines(&mut app, left(B, "exited")));
    assert_eq!(
        deletion.len(),
        1,
        "a fresh post-confirm exit completes once"
    );
    assert_eq!(
        deletion[0]["args"],
        json!({"session": A, "cascade": true, "expect": [A, B]})
    );
}

#[test]
fn an_exit_after_discovery_and_before_confirm_needs_no_close() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    let command = discovery(&mut app);
    assert!(answer_discovery(&mut app, &command, &[A, B], &[A, B]).is_empty());
    assert!(fold_lines(&mut app, left(B, "exited")).is_empty());

    let raw = confirm(&mut app);
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    let stopped = closes(&lines);
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0]["session_id"], A);
    assert!(deletes(&lines).is_empty());
    let deletion = deletes(&fold_lines(&mut app, left(A, "exited")));
    assert_eq!(deletion.len(), 1);
    assert_eq!(
        deletion[0]["args"],
        json!({"session": A, "cascade": true, "expect": [A, B]})
    );
}

/// A `session_status` after the post-discovery exit resumes the feed
/// row, so confirmation no longer trusts the stale exit and closes the
/// session again before the exact-set delete.
#[test]
fn a_resume_after_the_fresh_exit_needs_a_close_again() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    let command = discovery(&mut app);
    assert!(answer_discovery(&mut app, &command, &[A, B], &[A, B]).is_empty());
    assert!(fold_lines(&mut app, left(B, "exited")).is_empty());
    // The fork resumes before the person confirms: its row is live
    // again, so the marked exit is stale.
    assert!(fold_lines(&mut app, live(B, "forked")).is_empty());

    let raw = confirm(&mut app);
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    let stopped = closes(&lines);
    assert_eq!(stopped.len(), 2, "the resumed fork gets a close-now again");
    assert!(stopped.iter().any(|line| line["session_id"] == B));
    assert!(deletes(&lines).is_empty());
    let deletion = deletes(&fold_lines(&mut app, left(B, "exited")));
    assert!(deletion.is_empty(), "the parent is still live");
    let deletion = deletes(&fold_lines(&mut app, left(A, "exited")));
    assert_eq!(deletion.len(), 1);
    assert_eq!(
        deletion[0]["args"],
        json!({"session": A, "cascade": true, "expect": [A, B]})
    );
}

#[test]
fn unheld_ids_get_no_close() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    app.on_line(live(C, "gone"));
    app.on_line(left(C, "exited"));
    let raw = asked(&mut app, &[A, B, C], &[A]);
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    // The live-but-unheld session and the exited one need no stop.
    let died = closes(&lines);
    assert_eq!(died.len(), 1);
    assert_eq!(died[0]["session_id"], A);
    assert_eq!(died[0]["args"], json!({"now": true}));
}

#[test]
fn held_empty_confirms_straight_to_the_exact_delete() {
    let mut app = attached();
    let raw = asked(&mut app, &[A], &[]);
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert!(closes(&lines).is_empty(), "{lines:?}");
    let gone = deletes(&lines);
    assert_eq!(gone.len(), 1);
    assert_eq!(gone[0]["command"], "delete");
    assert_eq!(
        gone[0]["args"],
        json!({"session": A, "cascade": true, "expect": [A]})
    );
}

#[test]
fn finishing_stops_preserves_a_quit_prompt_opened_in_the_meantime() {
    let mut app = attached();
    let _lines: Vec<String> = app.on_line(working(A, "a"));
    let _raw = asked(&mut app, &[A], &[A]);
    assert_eq!(app.quit(), Effect::None);
    assert!(app.quit_open());

    let deletion = deletes(&fold_lines(&mut app, left(A, "exited")));
    assert_eq!(deletion.len(), 1);
    assert_eq!(
        deletion[0]["args"],
        json!({"session": A, "cascade": true, "expect": [A]})
    );
    assert!(
        app.quit_open(),
        "sending delete must not replace the quit prompt"
    );
}

#[test]
fn close_accepted_alone_never_deletes() {
    let mut app = attached();
    let raw = asked(&mut app, &[A], &[A]);
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    let close = closes(&lines).into_iter().next().expect("the close");
    let id = close["id"].as_str().unwrap_or_else(|| panic!("close id"));
    assert!(app.on_line(accepted(id, json!({}))).is_empty());
    // Only the feed's exit evidence permits the delete.
    assert!(!open(&app));
}

#[test]
fn session_left_before_the_close_answer_still_deletes_once() {
    let mut app = attached();
    let raw = asked(&mut app, &[A], &[A]);
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    let close = closes(&lines).into_iter().next().expect("the close");
    let close_id = close["id"].as_str().unwrap_or_else(|| panic!("close id"));
    // The exit lands first: the delete goes out with the confirmed set.
    let lines: Vec<Value> = app
        .on_line(left(A, "exited"))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    let gone = deletes(&lines);
    assert_eq!(gone.len(), 1);
    assert_eq!(
        gone[0]["args"],
        json!({"session": A, "cascade": true, "expect": [A]})
    );
    let delete = gone[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"));
    // The late close answer is no second delete.
    assert!(app.on_line(accepted(close_id, json!({}))).is_empty());
    let _ = delete;
}

#[test]
fn crashed_exit_never_permits_the_delete() {
    let mut app = attached();
    let _raw = asked(&mut app, &[A], &[A]);
    let lines: Vec<Value> = app
        .on_line(left(A, "crashed"))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert!(deletes(&lines).is_empty());
}

#[test]
fn stop_refused_deletes_nothing_and_names_the_session() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    let raw = asked(&mut app, &[A, B], &[A, B]);
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    let closes = closes(&lines);
    assert_eq!(closes.len(), 2);
    let refused = closes
        .iter()
        .find(|line| line["session_id"] == B)
        .unwrap_or_else(|| panic!("the fork's close"));
    let id = refused["id"].as_str().unwrap_or_else(|| panic!("close id"));
    assert!(
        app.on_line(rejected(
            id,
            "session_not_found",
            "Session `s_bbbbbbbbbbbbbbbb` does not exist."
        ))
        .is_empty()
    );
    let notice = app.notice().unwrap_or_else(|| panic!("a notice"));
    assert!(notice.contains(B), "{notice}");
    assert!(notice.contains("Nothing was deleted"), "{notice}");
    assert!(!open(&app));
    // The later exit of the other session deletes nothing either.
    let lines: Vec<Value> = app
        .on_line(left(A, "exited"))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert!(deletes(&lines).is_empty());
}

/// The lines a fold sends, parsed, with only the command lines kept.
fn fold_lines(app: &mut App, line: Line) -> Vec<Value> {
    app.on_line(line)
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect()
}

#[test]
fn unanswered_close_is_uncertain_and_sends_no_delete() {
    for (sessions, held, names) in [
        (vec![A], vec![A], "old work"),
        (vec![A, B], vec![A, B], "old work and 1 more"),
    ] {
        let mut app = attached();
        if held.contains(&B) {
            app.on_line(live(B, "forked"));
        }
        let _raw = asked(&mut app, &sessions, &held);
        app.home_reset();
        let notice = app.notice().unwrap_or_else(|| panic!("a notice"));
        assert!(
            notice.contains(&format!(
                "Stop status unknown for {names}. Nothing was deleted."
            )),
            "{notice}"
        );
        // The stops may or may not have landed: no delete follows.
        assert!(deletes(&fold_lines(&mut app, left(A, "exited"))).is_empty());
    }
}

#[test]
fn write_failure_is_uncertain_and_sends_no_delete() {
    let mut app = attached();
    let raw = asked(&mut app, &[A], &[A]);
    app.write_failed(&raw);
    // The write failure drops the link too, so its notice lands after
    // the uncertainty: the frame shows both.
    let shown = frame(&app);
    assert!(
        shown.contains("Stop status unknown for old work. Nothing was deleted."),
        "{shown}"
    );
    assert!(deletes(&fold_lines(&mut app, left(A, "exited"))).is_empty());
}

#[test]
fn post_stop_session_held_absent_or_empty_uses_the_single_notice() {
    let mut app = attached();
    // There is no `held` field. The notice falls back to target A instead
    // of parsing B from the message.
    let _raw = asked(&mut app, &[A, B], &[A, B]);
    let mut out = fold_lines(&mut app, left(A, "exited"));
    out.extend(fold_lines(&mut app, left(B, "exited")));
    let gone = deletes(&out);
    assert_eq!(gone.len(), 1);
    let delete = gone[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"));
    // A resume sneaks in between: the delete is refused, never retried.
    assert!(
        app.on_line(rejected(
            delete,
            "session_held",
            "Session `s_bbbbbbbbbbbbbbbb` is held by another process."
        ))
        .is_empty()
    );
    assert_eq!(
        app.notice(),
        Some("Not deleted: old work is running again. Nothing was deleted.")
    );
    // The attached screen stays, the question stays closed, no retry.
    assert!(!app.on_home());
    assert_eq!(app.session(), Some(&SessionId(A.to_owned())));
    assert!(!open(&app));

    let mut empty = attached();
    let _raw = asked(&mut empty, &[A, B], &[A, B]);
    let mut out = fold_lines(&mut empty, left(A, "exited"));
    out.extend(fold_lines(&mut empty, left(B, "exited")));
    let gone = deletes(&out);
    assert_eq!(gone.len(), 1);
    let delete = gone[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"));
    assert!(
        empty
            .on_line(rejected_with_held(
                delete,
                "Session `s_bbbbbbbbbbbbbbbb` is held by another process.",
                &[],
            ))
            .is_empty()
    );
    assert_eq!(
        empty.notice(),
        Some("Not deleted: old work is running again. Nothing was deleted.")
    );
    assert!(!empty.on_home());
    assert!(!open(&empty));
}

#[test]
fn post_stop_session_held_uses_structured_ids_for_name_and_count() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    let _raw = asked(&mut app, &[A, B], &[A, B]);
    let mut out = fold_lines(&mut app, left(A, "exited"));
    out.extend(fold_lines(&mut app, left(B, "exited")));
    let gone = deletes(&out);
    assert_eq!(gone.len(), 1);
    let delete = gone[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"));
    assert!(
        app.on_line(rejected_with_held(
            delete,
            "Session `s_aaaaaaaaaaaaaaaa` is held by another process.",
            &[A, B],
        ))
        .is_empty()
    );
    assert_eq!(
        app.notice(),
        Some("Not deleted: old work is running again and 1 more. Nothing was deleted.")
    );
    assert!(!app.on_home());
    assert!(!open(&app));
}

#[test]
fn post_stop_held_array_is_validated_in_full_before_using_it() {
    for held in [
        json!([B, "not-a-session-id"]),
        json!([B, B]),
        json!([B, 17]),
        json!([]),
        json!("not-an-array"),
    ] {
        let mut app = attached();
        let _raw = asked(&mut app, &[A], &[A]);
        let deletion = deletes(&fold_lines(&mut app, left(A, "exited")));
        assert_eq!(deletion.len(), 1);
        let delete = deletion[0]["id"].as_str().unwrap();
        assert!(
            app.on_line(rejected_with_held_value(
                delete,
                "Session `s_bbbbbbbbbbbbbbbb` and 99 more are held.",
                held,
            ))
            .is_empty()
        );
        assert_eq!(
            app.notice(),
            Some("Not deleted: old work is running again. Nothing was deleted.")
        );
    }
}

#[test]
fn stale_set_after_stop_returns_home_with_stopped_rows() {
    let mut app = attached();
    let _raw = asked(&mut app, &[A], &[A]);
    let out = fold_lines(&mut app, left(A, "exited"));
    let gone = deletes(&out);
    assert_eq!(gone.len(), 1);
    let delete = gone[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"));
    // A fork made while the question was open rejects the delete: home
    // with stopped rows, no re-question, the refusal explained.
    let refused = "The sessions this delete would remove are now `s_aaaaaaaaaaaaaaaa`, `s_bbbbbbbbbbbbbbbb`. Nothing was deleted.";
    let lines = fold_lines(&mut app, rejected(delete, "stale_request", refused));
    assert!(app.on_home());
    assert!(!open(&app));
    assert_eq!(app.notice(), Some(refused));
    assert!(lines.iter().any(|line| line["command"] == "recent"));
    assert!(
        app.home
            .as_ref()
            .and_then(|home| home.sessions.row(&SessionId(A.to_owned())))
            .is_some(),
        "the stopped row stays for a retry"
    );
}

#[test]
fn dependent_set_after_stop_explains_without_requestion() {
    let mut app = attached();
    let _raw = asked(&mut app, &[A], &[A]);
    let out = fold_lines(&mut app, left(A, "exited"));
    let gone = deletes(&out);
    assert_eq!(gone.len(), 1);
    let delete = gone[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"));
    let refused = "Session `s_aaaaaaaaaaaaaaaa` has sessions that continue it: `s_bbbbbbbbbbbbbbbb`. `--cascade` deletes them too.";
    let lines = fold_lines(
        &mut app,
        rejected(delete, "session_has_dependents", refused),
    );
    assert!(app.on_home());
    assert!(!open(&app));
    assert_eq!(app.notice(), Some(refused));
    assert!(lines.iter().any(|line| line["command"] == "recent"));
}

#[test]
fn io_failed_after_stop_notes_and_returns_home() {
    let mut app = attached();
    let _raw = asked(&mut app, &[A], &[A]);
    let out = fold_lines(&mut app, left(A, "exited"));
    let gone = deletes(&out);
    assert_eq!(gone.len(), 1);
    let delete = gone[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"));
    // A partial removal never claims nothing was deleted.
    let failed = "Session `s_aaaaaaaaaaaaaaaa` could not be deleted: broken pipe.";
    let lines = fold_lines(&mut app, rejected(delete, "io_failed", failed));
    assert!(app.on_home());
    assert_eq!(app.notice(), Some(failed));
    assert!(lines.iter().any(|line| line["command"] == "recent"));
    assert!(!frame(&app).contains("Nothing was deleted"));
}

#[test]
fn unanswered_delete_is_uncertain_and_stays_attached() {
    let mut app = attached();
    let _raw = asked(&mut app, &[A], &[A]);
    let out = fold_lines(&mut app, left(A, "exited"));
    let gone = deletes(&out);
    assert_eq!(gone.len(), 1);
    // The connection drops with the delete in flight: its answer never
    // arrives, so nothing confirms the removal.
    app.home_reset();
    assert_eq!(
        app.notice(),
        Some("Delete status unknown for old work. Nothing was confirmed deleted.")
    );
    assert!(!app.on_home());
    assert_eq!(app.session(), Some(&SessionId(A.to_owned())));
    // A command whose answer was lost no longer owns the state: a new
    // slash command on the same attached session can start.
    let _next = discovery(&mut app);
    assert!(matches!(
        app.home.as_ref().unwrap().questions.delete_command.state,
        State::Discovering { .. }
    ));
}

#[test]
fn delete_accepted_goes_home_with_a_fresh_page() {
    let mut app = attached();
    let _raw = asked(&mut app, &[A], &[A]);
    let out = fold_lines(&mut app, left(A, "exited"));
    let gone = deletes(&out);
    assert_eq!(gone.len(), 1);
    let delete = gone[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"));
    let lines: Vec<Value> = app
        .on_line(accepted(delete, json!({})))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert!(app.on_home());
    assert!(lines.iter().any(|line| line["command"] == "recent"));
    // The connection is neither released nor replaced.
    assert!(app.outbox.is_up());
}

#[test]
fn exit_lines_exclude_sessions_whose_close_was_sent() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    let _raw = asked(&mut app, &[A, B], &[A, B]);
    let gone: Vec<String> = app
        .exit_lines()
        .iter()
        .filter(|line| line.contains(A) || line.contains(B))
        .cloned()
        .collect();
    assert!(gone.is_empty(), "{gone:?}");
}
