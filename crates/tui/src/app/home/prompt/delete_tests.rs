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
    live_since(session, name, 0)
}

/// A live `session_status` at an explicit watermark.
fn live_since(session: &str, name: &str, since: u64) -> Line {
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
            "started": 0, "calls": 0, "since": since,
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

/// Adds an exited row only to `recent`, not to the live feed.
fn recent_only(app: &mut App, session: &str, name: &str) {
    let Line::Session(envelope) = live(session, name) else {
        panic!("status envelope");
    };
    let mut row = crate::home::from_status(&envelope);
    row.left = Some(crate::home::Left::Exited);
    app.home
        .as_mut()
        .unwrap_or_else(|| panic!("home"))
        .sessions
        .recent(vec![row], true);
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
    let prior_notice = app.notice().map(str::to_owned);
    assert_eq!(slash(&mut app, "/delete"), Effect::None);
    assert_eq!(app.draft(), "/delete");
    assert_eq!(app.notice(), prior_notice.as_deref());
}

#[test]
fn offline_repeat_during_lost_discovery_is_silent_and_keeps_the_first_state() {
    let mut app = attached();
    let _first = discovery(&mut app);
    app.disconnected();
    let prior_notice = app.notice().map(str::to_owned);
    assert_eq!(slash(&mut app, "/delete"), Effect::None);
    assert_eq!(app.draft(), "/delete");
    assert_eq!(app.notice(), prior_notice.as_deref());
    assert!(matches!(
        app.home.as_ref().unwrap().questions.delete_command.state,
        State::Discovering { .. }
    ));
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
fn held_sessions_follow_the_original_preview_order() {
    let target = SessionId(A.to_owned());
    let result = json!({"sessions": [A, B], "held": [B, A]});
    let (sessions, held) = super::validated_sets(&result, &target).unwrap();
    assert_eq!(
        sessions,
        vec![SessionId(A.to_owned()), SessionId(B.to_owned())]
    );
    assert_eq!(held, vec![SessionId(A.to_owned()), SessionId(B.to_owned())]);
}

#[test]
fn malformed_answers_show_a_notice_and_open_no_question() {
    for (sessions, held) in [
        (vec![A, A], vec![]),
        (vec![B], vec![]),
        (vec![], vec![]),
        (vec![A], vec![B]),
        (vec![A], vec![A, A]),
        (vec![A, "bogus"], vec![]),
        (vec![A, "s_123"], vec![]),
        (vec![A, "s_gggggggggggggggg"], vec![]),
        (vec![A], vec!["bogus"]),
    ] {
        let mut app = attached();
        let command = discovery(&mut app);
        let result = json!({"sessions": sessions, "held": held});
        assert!(app.on_line(accepted(&command, result)).is_empty());
        let notice = app.notice().unwrap_or_else(|| panic!("a notice"));
        assert_eq!(
            notice,
            "Not deleted: the hub's preview was invalid. Nothing was deleted."
        );
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
        Some("Delete failed: Session `s_aaaaaaaaaaaaaaaa` is held by another process.")
    );
    assert!(!open(&app));
    assert!(matches!(
        app.home.as_ref().unwrap().questions.delete_command.state,
        State::Idle
    ));
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
fn a_repeat_during_discovery_keeps_the_first_command() {
    let mut app = attached();
    let first = discovery(&mut app);
    for ch in "/delete".chars() {
        assert_eq!(
            app.on_key(Key::Char(ch), fakes::clock::FakeClock::new().now()),
            Effect::None
        );
    }
    let second = sent(app.on_key(Key::Enter, fakes::clock::FakeClock::new().now()));
    assert!(second.is_empty(), "no second dry run is sent");
    assert_eq!(app.draft(), "/delete");
    assert_eq!(app.notice(), Some("`/delete` is already in progress."));
    assert!(answer_discovery(&mut app, &first, &[A, B], &[A]).is_empty());
    assert!(open(&app), "the first discovery still opens its question");
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
    assert!(app.on_home(), "confirmation returns Home before the stops");
    // The unrelated live session is never stopped.
    assert!(
        lines.iter().all(|line| line["session_id"] != D),
        "{lines:?}"
    );
}

#[test]
fn a_repeat_while_stopping_leaves_the_first_sequence_intact() {
    let mut app = attached();
    let _raw = asked(&mut app, &[A], &[A]);
    app.draft.paste("/delete");
    assert_eq!(app.delete_on_screen(), Effect::None);
    assert_eq!(app.draft(), "/delete");
    assert_eq!(app.notice(), Some("`/delete` is already in progress."));
    assert!(matches!(
        app.home.as_ref().unwrap().questions.delete_command.state,
        State::Stopping { .. }
    ));
    let lines = fold_lines(&mut app, left(A, "exited"));
    assert_eq!(deletes(&lines).len(), 1);
}

#[test]
fn no_feed_delegate_is_not_closed_and_does_not_gate_delete() {
    let mut app = attached();
    // B represents a live delegate that is absent from the session feed.
    let raw = asked(&mut app, &[A, B], &[A, B]);
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    let stopped = closes(&lines);
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0]["session_id"], A);
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
fn a_feed_row_already_left_at_confirmation_needs_no_close_or_wait() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    app.on_line(left(B, "exited"));
    let raw = asked(&mut app, &[A, B], &[A, B]);
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    let stopped = closes(&lines);
    assert_eq!(stopped.len(), 1, "the already-left feed row is not closed");
    assert_eq!(stopped[0]["session_id"], A);
    assert!(deletes(&lines).is_empty());

    let deletion = deletes(&fold_lines(&mut app, left(A, "exited")));
    assert_eq!(
        deletion.len(),
        1,
        "A's exit completes without waiting for B"
    );
    assert_eq!(
        deletion[0]["args"],
        json!({"session": A, "cascade": true, "expect": [A, B]})
    );
}

#[test]
fn recent_only_held_id_gets_no_close_or_wait() {
    let mut app = attached();
    recent_only(&mut app, B, "forked from recent");
    let raw = asked(&mut app, &[A, B], &[A, B]);
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
    assert!(deletes(&fold_lines(&mut app, left(B, "exited"))).is_empty());
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
    // The fork resumes before the person confirms, with a newer status
    // timestamp that makes the marked exit stale.
    assert!(fold_lines(&mut app, live_since(B, "forked", 1)).is_empty());

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
    assert!(app.on_home(), "confirmation returns Home before deleting");
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
fn crashed_exit_counts_as_stopped_and_permits_the_exact_delete() {
    let mut app = attached();
    let _raw = asked(&mut app, &[A], &[A]);
    let lines: Vec<Value> = app
        .on_line(left(A, "crashed"))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    let deletion = deletes(&lines);
    assert_eq!(deletion.len(), 1);
    assert_eq!(
        deletion[0]["args"],
        json!({"session": A, "cascade": true, "expect": [A]})
    );
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
        app.disconnected();
        let notice = app.notice().unwrap_or_else(|| panic!("a notice"));
        assert!(
            notice.contains(&format!(
                "Not deleted: the hub link dropped before {names} stopped. Nothing was deleted."
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
    // A failed stop write is treated as link loss: no delete follows.
    assert!(app.notices.lines().iter().any(|line| matches!(
        line,
        crate::app::NoticeLine::Notice { text, .. }
            if text.contains("Not deleted: the hub link dropped")
    )));
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
    // Confirmation already moved Home; no retry or re-question.
    assert!(app.on_home());
    assert_eq!(app.session(), None);
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
    assert!(empty.on_home());
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
    assert!(app.on_home());
    assert!(!open(&app));
}

#[test]
fn post_stop_held_array_is_validated_in_full_before_using_it() {
    for held in [
        json!([B, "not-a-session-id"]),
        json!([B, B]),
        json!([C]),
        json!([B, 17]),
        json!([]),
        json!("not-an-array"),
    ] {
        let mut app = attached();
        let raw = asked(&mut app, &[A, B], &[]);
        let deletion: Vec<Value> = raw
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(deletes(&deletion).len(), 1);
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
fn a_valid_held_id_without_any_row_uses_the_captured_target_name() {
    let mut app = attached();
    let _raw = asked(&mut app, &[A, B], &[A, B]);
    let out = fold_lines(&mut app, left(A, "exited"));
    let gone = deletes(&out);
    assert_eq!(gone.len(), 1);
    let delete = gone[0]["id"].as_str().unwrap();
    assert!(
        app.on_line(rejected_with_held(delete, "held", &[B]))
            .is_empty()
    );
    assert_eq!(
        app.notice(),
        Some("Not deleted: old work is running again. Nothing was deleted.")
    );
    assert!(!app.notice().unwrap().contains(B));
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
    let notice = format!("{refused} The sessions remain stopped.");
    assert_eq!(app.notice(), Some(notice.as_str()));
    assert!(lines.iter().any(|line| line["command"] == "recent"));
    insta::assert_snapshot!("stale_set", frame(&app));
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
    assert_eq!(
        app.notice(),
        Some(format!("Delete failed: {refused}").as_str())
    );
    assert!(lines.iter().any(|line| line["command"] == "recent"));
    insta::assert_snapshot!("dependent_set", frame(&app));
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
    assert_eq!(
        app.notice(),
        Some(format!("Delete failed: {failed}").as_str())
    );
    assert!(lines.iter().any(|line| line["command"] == "recent"));
    assert!(!frame(&app).contains("Nothing was deleted"));
}

#[test]
fn unanswered_delete_is_uncertain_and_is_not_retried() {
    let mut app = attached();
    let _raw = asked(&mut app, &[A], &[A]);
    let out = fold_lines(&mut app, left(A, "exited"));
    let gone = deletes(&out);
    assert_eq!(gone.len(), 1);
    let delete_id = gone[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    // The connection drops with the delete in flight: its answer never
    // arrives, so nothing confirms the removal.
    app.disconnected();
    app.home_reset();
    assert_eq!(app.notice(), Some("Delete status unknown"));
    assert!(app.on_home());
    assert_eq!(app.session(), None);
    // The lost command no longer owns the state, and reconnect does not
    // resend or retry the destructive line.
    assert!(!app.outbox.is_kept(&delete_id));
    assert!(
        !app.home
            .as_ref()
            .unwrap()
            .questions
            .asks
            .contains_key(&delete_id)
    );
    assert!(matches!(
        app.home.as_ref().unwrap().questions.delete_command.state,
        State::Idle
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
fn discovery_does_not_replace_another_sessions_screen_or_question() {
    for switch_session in [true, false] {
        let mut app = attached();
        let command = discovery(&mut app);
        if switch_session {
            app.on_line(live(B, "other work"));
            app.attach(SessionId(B.to_owned()));
        } else {
            app.on_line(working(A, "old work"));
            assert_eq!(app.quit(), Effect::None);
            assert!(app.quit_open());
        }
        assert!(answer_discovery(&mut app, &command, &[A], &[]).is_empty());
        assert!(!frame(&app).contains("Delete session"));
        if switch_session {
            assert_eq!(app.session(), Some(&SessionId(B.to_owned())));
        } else {
            assert!(app.quit_open());
        }
    }
}

#[test]
fn command_question_draws_only_over_its_attached_session() {
    let mut app = attached();
    let command = discovery(&mut app);
    answer_discovery(&mut app, &command, &[A], &[]);
    assert!(frame(&app).contains("Delete session"));
    app.on_line(live(B, "other work"));
    app.attach(SessionId(B.to_owned()));
    assert!(!frame(&app).contains("Delete session"));
    app.go_home();
    assert!(frame(&app).contains("Delete session"));
    assert!(app.home_question_open());
    let sent = sent(app.on_key(Key::Enter, fakes::clock::FakeClock::new().now()));
    assert!(
        closes(&sent).is_empty(),
        "home Enter must not send an attached-session key"
    );
    assert_eq!(
        deletes(&sent).len(),
        1,
        "home Enter confirms the captured exact set"
    );
}

#[test]
fn an_attached_target_without_a_feed_row_still_shows_the_captured_question() {
    let mut app = attached();
    app.home
        .as_mut()
        .unwrap()
        .sessions
        .remove(&SessionId(A.to_owned()));
    app.name = Some("old work".to_owned());
    let command = discovery(&mut app);
    answer_discovery(&mut app, &command, &[A], &[]);
    assert!(frame(&app).contains("Delete old work"));
    assert!(open(&app));
    let lines = confirm(&mut app);
    assert_eq!(
        deletes(
            &lines
                .iter()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect::<Vec<_>>()
        )
        .len(),
        1
    );
    assert!(app.on_home());
}

#[test]
fn cancelling_does_not_turn_a_later_row_delete_into_a_stop() {
    let mut app = attached();
    let command = discovery(&mut app);
    answer_discovery(&mut app, &command, &[A, B], &[A, B]);
    app.on_key(Key::Esc, fakes::clock::FakeClock::new().now());
    app.on_line(left(A, "exited"));
    app.go_home();
    let key = app
        .home
        .as_ref()
        .unwrap()
        .sessions
        .row(&SessionId(A.to_owned()))
        .unwrap()
        .key;
    assert_eq!(app.home_click(Spot::Stop(key)), Effect::None);
    assert!(frame(&app).contains("Delete session"));
    let lines = sent(app.on_key(Key::Enter, fakes::clock::FakeClock::new().now()));
    // The session-list delete sends one delete and no stops
    // (docs/tui.md, "The session list").
    assert!(closes(&lines).is_empty(), "{lines:?}");
    assert_eq!(deletes(&lines).len(), 1);
    assert_eq!(deletes(&lines)[0]["args"], json!({"session": A}));
}

#[test]
fn stopped_sessions_drop_their_resume_suppression_but_pending_stops_keep_it() {
    let mut app = attached();
    app.on_line(live(B, "forked"));
    app.on_line(live(D, "unrelated"));
    let raw = asked(&mut app, &[A, B], &[A, B]);
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let close_b = closes(&lines)
        .into_iter()
        .find(|line| line["session_id"] == B)
        .unwrap();
    assert!(fold_lines(&mut app, left(A, "exited")).is_empty());
    // A can resume while B's stop still waits. The status is strictly
    // newer than the exit evidence, so A needs a resume line on exit.
    app.on_line(live_since(A, "old work", 1));
    let resumes = app.exit_lines();
    assert!(resumes.iter().any(|line| line.contains(A)), "{resumes:?}");
    assert!(resumes.iter().any(|line| line.contains(D)), "{resumes:?}");
    assert!(!resumes.iter().any(|line| line.contains(B)), "{resumes:?}");
    app.on_line(rejected(
        close_b["id"].as_str().unwrap(),
        "io_failed",
        "Stop refused.",
    ));
    let resumes = app.exit_lines();
    assert!(resumes.iter().any(|line| line.contains(B)), "{resumes:?}");
    assert!(resumes.iter().any(|line| line.contains(D)), "{resumes:?}");
}

#[test]
fn actual_disconnect_reports_first_still_waiting_name_and_count_without_deleting() {
    let mut app = attached();
    app.on_line(live(B, "beta"));
    app.on_line(live(C, "gamma"));
    // The preview's held order is deliberately different from lexical ID order.
    let raw = asked(&mut app, &[A, C, B], &[A, C, B]);
    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert_eq!(closes(&lines).len(), 3);
    assert!(deletes(&lines).is_empty());

    // A retires first, leaving B then C in the confirmed-set order.
    assert!(deletes(&fold_lines(&mut app, left(A, "exited"))).is_empty());
    app.disconnected();
    assert_eq!(
        app.notice(),
        Some(
            "Not deleted: the hub link dropped before gamma and 1 more stopped. Nothing was deleted."
        )
    );
    assert!(app.on_home());
    assert!(!open(&app));
    assert!(deletes(&fold_lines(&mut app, left(B, "exited"))).is_empty());
}

#[test]
fn disconnect_uses_single_waiting_session_wording() {
    let mut app = attached();
    let _raw = asked(&mut app, &[A], &[A]);
    app.disconnected();
    assert_eq!(
        app.notice(),
        Some("Not deleted: the hub link dropped before old work stopped. Nothing was deleted.")
    );
}

#[test]
fn a_stale_row_delete_reprompt_replaces_and_cancels_the_slash_question() {
    let mut app = attached();
    let command = discovery(&mut app);
    answer_discovery(&mut app, &command, &[A], &[A]);
    app.go_home();
    assert!(open(&app));
    app.home.as_mut().unwrap().questions.asks.insert(
        "stale-row-delete".to_owned(),
        super::super::Ask::Delete(SessionId(A.to_owned())),
    );

    app.on_line(rejected(
        "stale-row-delete",
        "stale_request",
        "The sessions this delete would remove are now `s_aaaaaaaaaaaaaaaa`, `s_bbbbbbbbbbbbbbbb`.",
    ));
    assert!(matches!(
        app.home.as_ref().unwrap().questions.delete_command.state,
        State::Idle
    ));
    assert!(matches!(
        app.home.as_ref().unwrap().questions.prompt(),
        Some(super::super::Prompt::Delete {
            expect: Some(expect),
            ..
        }) if expect == &[SessionId(A.to_owned()), SessionId(B.to_owned())]
    ));
    assert!(app.home_question_open());
}

#[test]
fn another_rows_delete_answer_does_not_finish_the_command() {
    let mut app = attached();
    app.on_line(live(B, "other work"));
    app.on_line(left(B, "exited"));
    app.go_home();
    let key = app
        .home
        .as_ref()
        .unwrap()
        .sessions
        .row(&SessionId(B.to_owned()))
        .unwrap()
        .key;
    app.home_click(Spot::Stop(key));
    let old = confirm(&mut app);
    let old: Value = serde_json::from_str(&old[0]).unwrap();
    app.on_key(Key::Esc, fakes::clock::FakeClock::new().now());
    app.attach(SessionId(A.to_owned()));
    let raw = asked(&mut app, &[A], &[]);
    let deletion: Value = serde_json::from_str(&raw[0]).unwrap();
    app.on_line(accepted(old["id"].as_str().unwrap(), json!({})));
    assert!(app.on_home());
    assert!(matches!(
        app.home.as_ref().unwrap().questions.delete_command.state,
        State::Deleting { .. }
    ));
    let refresh = fold_lines(
        &mut app,
        accepted(deletion["id"].as_str().unwrap(), json!({})),
    );
    assert!(app.on_home());
    assert!(refresh.iter().any(|line| line["command"] == "recent"));
}

#[test]
fn same_target_row_delete_answer_does_not_settle_slash_delete() {
    let mut app = attached();
    let raw = asked(&mut app, &[A], &[]);
    let slash_delete: Value = serde_json::from_str(&raw[0]).unwrap();
    let slash_id = slash_delete["id"].as_str().unwrap();
    app.home.as_mut().unwrap().questions.asks.insert(
        "row-delete".to_owned(),
        super::super::Ask::Delete(SessionId(A.to_owned())),
    );

    app.on_line(accepted("row-delete", json!({})));
    assert!(matches!(
        app.home.as_ref().unwrap().questions.delete_command.state,
        State::Deleting { .. }
    ));
    assert!(app.on_home());
    assert!(matches!(
        app.home.as_ref().unwrap().questions.delete_command.state,
        State::Deleting { .. }
    ));

    app.on_line(accepted(slash_id, json!({})));
    assert!(app.on_home());
    assert!(matches!(
        app.home.as_ref().unwrap().questions.delete_command.state,
        State::Idle
    ));
}

#[test]
fn quit_replacing_the_unconfirmed_question_cancels_the_delete() {
    let mut app = attached();
    app.on_line(working(A, "old work"));
    let command = discovery(&mut app);
    answer_discovery(&mut app, &command, &[A], &[A]);
    assert!(open(&app));

    assert_eq!(app.quit(), Effect::None);
    assert!(app.quit_open());
    assert!(matches!(
        app.home.as_ref().unwrap().questions.delete_command.state,
        State::Idle
    ));
    assert_eq!(
        app.on_key(Key::Esc, fakes::clock::FakeClock::new().now()),
        Effect::None
    );
    assert!(!app.quit_open());
}

#[test]
fn replacing_the_slash_question_with_close_all_stop_or_clear_cancels_it() {
    for replacement in ["close all", "stop", "clear"] {
        let mut app = attached();
        let command = discovery(&mut app);
        answer_discovery(&mut app, &command, &[A], &[A]);
        app.go_home();
        assert!(open(&app));

        match replacement {
            "close all" => assert_eq!(app.ask_close_all(), Effect::None),
            "stop" => {
                let key = app
                    .home
                    .as_ref()
                    .unwrap()
                    .sessions
                    .row(&SessionId(A.to_owned()))
                    .unwrap()
                    .key;
                assert_eq!(app.ask_stop(key), Effect::None);
            }
            "clear" => assert_eq!(
                app.open_clear(
                    SessionId(A.to_owned()),
                    "old work".to_owned(),
                    "approval".to_owned()
                ),
                Effect::None
            ),
            _ => unreachable!(),
        }

        assert!(matches!(
            app.home.as_ref().unwrap().questions.delete_command.state,
            State::Idle
        ));
        let prompt = app.home.as_ref().unwrap().questions.prompt();
        let replaced_with_expected_prompt = match replacement {
            "close all" => matches!(prompt, Some(super::super::Prompt::CloseAll { .. })),
            "stop" => matches!(prompt, Some(super::super::Prompt::Stop { .. })),
            "clear" => matches!(prompt, Some(super::super::Prompt::Clear { .. })),
            _ => unreachable!(),
        };
        assert!(replaced_with_expected_prompt);
    }
}

#[test]
fn a_stale_same_target_row_delete_answer_cancels_the_open_slash_question() {
    let mut app = attached();
    let command = discovery(&mut app);
    answer_discovery(&mut app, &command, &[A], &[A]);
    assert!(open(&app));
    app.home.as_mut().unwrap().questions.asks.insert(
        "stale-row-delete".to_owned(),
        super::super::Ask::Delete(SessionId(A.to_owned())),
    );

    app.on_line(accepted("stale-row-delete", json!({})));
    assert!(!open(&app));
    assert!(matches!(
        app.home.as_ref().unwrap().questions.delete_command.state,
        State::Idle
    ));
}

#[test]
fn the_pinned_warning_can_be_selected_and_copied() {
    let mut app = attached();
    let command = discovery(&mut app);
    answer_discovery(&mut app, &command, &[A, B, C], &[]);
    let (shown, targets) = drawn(&app);
    let warning = "It cannot be undone.";
    let (row, text) = shown
        .lines()
        .enumerate()
        .find(|(_, text)| text.contains(warning))
        .unwrap();
    let col = text[..text.find(warning).unwrap()].chars().count();
    let col = u16::try_from(col).unwrap();
    let row = u16::try_from(row).unwrap();
    let end = col + u16::try_from(warning.len()).unwrap();
    for (kind, x) in [
        (MouseKind::Press(Button::Left), col),
        (MouseKind::Drag(Button::Left), end),
    ] {
        assert_eq!(
            app.on_select(&Mouse { kind, col: x, row }, &targets),
            Effect::None
        );
    }
    assert_eq!(
        app.on_select(
            &Mouse {
                kind: MouseKind::Release,
                col: end,
                row
            },
            &targets
        ),
        Effect::Copy(warning.to_owned())
    );
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
