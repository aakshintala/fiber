//! Normalisation of hub and session command answers.

use super::Answer;
use crate::app::commands::commands_testkit::{SESSION, attached, enter, sent, session_line};
use crate::app::outbox::{Outbox, Waiting};
use crate::app::testkit::{hello, home_with, now};
use crate::app::{App, Effect};
use crate::approvals::form::Spot;
use crate::ext::{
    Decision, Input as RendererInput, RawSpan, Renderer, SessionCtx, SessionVersion, TuiCommand,
};
use crate::keys::Key;
use crate::link::Line;
use crate::model_picker::Choice;
use crate::mouse::TargetId;
use contract::{Envelope, HubLine, SessionId};
use contract::{HubEventKind, events::EventKind};
use serde_json::{Value, json};
use std::path::PathBuf;

fn line(session: bool, kind: &str, payload: Value) -> Line {
    let payload = payload.as_object().unwrap().clone();
    if session {
        Line::Session(Envelope {
            kind: EventKind::from_name(kind),
            session_id: SessionId("s_answer".to_owned()),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: None,
            seq: None,
            payload,
        })
    } else {
        Line::Hub(HubLine {
            kind: HubEventKind::from(kind.to_owned()),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            payload,
        })
    }
}

#[test]
fn answer_of_a_hub_accept_reads_id_payload_and_tag() {
    let mut outbox = Outbox::default();
    let tag = Waiting::Dismiss(SessionId("s_target".to_owned()));
    outbox.wait("c_answer", tag.clone());
    for session in [false, true] {
        for (kind, accepted) in [("command_accepted", true), ("command_rejected", false)] {
            let line = line(
                session,
                kind,
                json!({"command_id":"c_answer", "code":"", "message":"kept"}),
            );
            let answer = Answer::of(&line, &outbox).unwrap();
            assert_eq!(answer.id, "c_answer");
            assert_eq!(answer.accepted, accepted);
            assert_eq!(answer.payload.get("code"), Some(&json!("")));
            assert_eq!(answer.payload.get("message"), Some(&json!("kept")));
            assert_eq!(answer.waiting, Some(tag.clone()));
            assert_eq!(outbox.waiting("c_answer"), Some(tag.clone()));
        }
    }
    let line = line(false, "command_accepted", json!({"command_id":"c_unknown"}));
    assert_eq!(Answer::of(&line, &outbox).unwrap().waiting, None);
}

#[test]
fn answer_of_a_line_without_a_string_command_id_is_none() {
    for session in [false, true] {
        for kind in ["command_accepted", "command_rejected"] {
            for payload in [
                json!({}),
                json!({"command_id":null}),
                json!({"command_id":1}),
                json!({"command_id":false}),
            ] {
                assert!(Answer::of(&line(session, kind, payload), &Outbox::default()).is_none());
            }
        }
    }
}

#[test]
fn answer_of_a_session_reject_names_the_session() {
    let line = line(true, "command_rejected", json!({"command_id":"c_answer"}));
    let answer = Answer::of(&line, &Outbox::default()).unwrap();
    assert!(matches!(answer.from, super::Source::Session(session) if session.0 == "s_answer"));
    assert!(!answer.accepted);
    let line = self::line(false, "command_accepted", json!({"command_id":"c_answer"}));
    assert!(matches!(
        Answer::of(&line, &Outbox::default()).unwrap().from,
        super::Source::Hub
    ));
}

#[test]
fn a_disconnect_forgets_abandoned_waits_only() {
    let mut app = crate::app::commands::commands_testkit::attached();
    app.abandon_history("c_abandoned".to_owned());
    app.outbox.wait("c_fork", Waiting::Fork);
    app.disconnected();
    assert_eq!(app.outbox.waiting("c_abandoned"), None);
    assert_eq!(app.outbox.waiting("c_fork"), Some(Waiting::Fork));
}

#[test]
fn attach_forgets_the_rewind_tags_and_keeps_fork() {
    let mut app = crate::app::commands::commands_testkit::attached();
    app.outbox.wait("c_history", Waiting::RewindHistory);
    app.outbox.wait("c_send", Waiting::RewindSend);
    app.outbox.wait("c_fork", Waiting::Fork);
    app.attach(SessionId("s_new".to_owned()));
    assert_eq!(app.outbox.waiting("c_history"), None);
    assert_eq!(app.outbox.waiting("c_send"), None);
    assert_eq!(app.outbox.waiting("c_fork"), Some(Waiting::Fork));
}

#[test]
fn rewind_lost_forgets_the_rewind_history_tag() {
    let mut app = crate::app::commands::commands_testkit::attached();
    app.outbox.wait("c_history", Waiting::RewindHistory);
    app.outbox.wait("c_send", Waiting::RewindSend);
    app.rewind_lost();
    assert_eq!(app.outbox.waiting("c_history"), None);
    assert_eq!(app.outbox.waiting("c_send"), Some(Waiting::RewindSend));
    app.outbox.wait("c_reopened", Waiting::RewindHistory);
    app.rewind_reopened();
    assert_eq!(app.outbox.waiting("c_reopened"), None);
}

/// An image fetch tag for a viewer open.
fn image_fetch() -> Waiting {
    Waiting::ImageFetch {
        id: 1,
        session: "s_a".to_owned(),
        path: "a.png".to_owned(),
    }
}

/// An image tag for an inline display.
fn image_display() -> Waiting {
    Waiting::ImageDisplay {
        session: "s_a".to_owned(),
        path: "a.png".to_owned(),
    }
}

#[test]
fn images_disconnected_forgets_fetch_tags_and_keeps_display_tags() {
    let mut app = crate::app::commands::commands_testkit::attached();
    app.outbox.wait("c_fetch", image_fetch());
    app.outbox.wait("c_display", image_display());
    app.images_disconnected();
    assert_eq!(app.outbox.waiting("c_fetch"), None);
    assert_eq!(app.outbox.waiting("c_display"), Some(image_display()));
    app.forget_images();
    assert_eq!(app.outbox.waiting("c_display"), None);
}

#[test]
fn sessions_dropped_forgets_the_sessions_tag() {
    let mut app = crate::app::commands::commands_testkit::attached();
    app.outbox.wait("c_sessions", Waiting::Sessions);
    app.outbox.wait("c_fork", Waiting::Fork);
    app.sessions_dropped();
    assert_eq!(app.outbox.waiting("c_sessions"), None);
    assert_eq!(app.outbox.waiting("c_fork"), Some(Waiting::Fork));
}

#[test]
fn fork_answer_reaches_its_handler_and_takes_the_tag_once() {
    use crate::app::commands::commands_testkit::{attached, enter, sent, session_line};
    let mut app = attached();
    let request = sent(enter(&mut app, "/fork"));
    let id = request[0]["id"].as_str().unwrap();
    assert_eq!(app.outbox.waiting(id), Some(Waiting::Fork));
    let answer = session_line(
        "command_accepted",
        json!({"command_id":id, "result":{"new_session_id":"s_child"}}),
    );
    app.on_line(answer.clone());
    assert_eq!(app.session(), Some(&SessionId("s_child".to_owned())));
    assert_eq!(app.outbox.waiting(id), None);
    assert!(app.on_line(answer).is_empty());
}

#[test]
fn fork_refusal_takes_the_tag_and_allows_another_fork() {
    use crate::app::commands::commands_testkit::{attached, enter, sent, session_line};
    for hub in [false, true] {
        let mut app = attached();
        let request = sent(enter(&mut app, "/fork"));
        let id = request[0]["id"].as_str().unwrap();
        let payload = json!({"command_id":id, "code":"busy", "message":"fork refused"});
        let answer = if hub {
            line(false, "command_rejected", payload)
        } else {
            session_line("command_rejected", payload)
        };
        app.on_line(answer);
        assert_eq!(app.outbox.waiting(id), None);
        assert_eq!(app.notice(), Some("fork refused"));
        assert!(!sent(enter(&mut app, "/fork")).is_empty());
    }
}

#[test]
fn clear_start_declined_still_reaches_started() {
    let mut app = crate::app::commands::commands_testkit::attached();
    app.outbox.wait("c_stale", Waiting::ClearStart);
    app.outbox.insert(
        "c_stale".to_owned(),
        crate::app::Kind::Start,
        crate::input::Draft::default(),
    );
    let out = app.on_line(line(
        false,
        "command_accepted",
        json!({"command_id":"c_stale","result":{"session_id":"s_started"}}),
    ));
    assert_eq!(app.session(), Some(&SessionId("s_started".to_owned())));
    assert!(
        out.iter()
            .any(|line| serde_json::from_str::<Value>(line).unwrap()["command"] == "prompt")
    );
    assert!(!app.outbox.pending.contains_key("c_stale"));
}

#[test]
fn image_and_history_declined_still_reach_generic_settlement() {
    for tag in [image_fetch(), image_display(), Waiting::History] {
        let mut app = crate::app::commands::commands_testkit::attached();
        app.outbox.wait("c_stale", tag);
        app.outbox.insert(
            "c_stale".to_owned(),
            crate::app::Kind::Command,
            crate::input::Draft::default(),
        );
        app.on_line(line(
            false,
            "command_accepted",
            json!({"command_id":"c_stale"}),
        ));
        assert!(!app.outbox.pending.contains_key("c_stale"));
        assert!(app.session().is_some());
    }
}

#[test]
fn an_unknown_id_falls_through_to_the_generic_path() {
    use crate::app::commands::commands_testkit::{attached, session_line};
    let mut app = attached();
    app.outbox.insert(
        "c_generic".to_owned(),
        crate::app::Kind::Command,
        crate::input::Draft::default(),
    );
    app.on_line(session_line(
        "command_accepted",
        json!({"command_id":"c_generic"}),
    ));
    assert!(!app.outbox.pending.contains_key("c_generic"));
    assert!(app.session().is_some());
}

#[test]
fn answer_of_a_non_answer_kind_is_none() {
    for session in [false, true] {
        for kind in ["notice", "session_status", "command_accepted_extra", ""] {
            assert!(
                Answer::of(
                    &line(session, kind, json!({"command_id":"c_answer"})),
                    &Outbox::default()
                )
                .is_none()
            );
        }
    }
}

/// One session envelope from the attached session, with no action id and
/// the given sequence number.
fn envelope(kind: &str, payload: Value, seq: Option<u64>, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: contract::events::EventKind::from_name(kind),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: seq.map(contract::Seq),
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Folds one session line, answering the `commands` ask a fold may send
/// with no rows, so setups leave no pending ask behind.
fn fold(app: &mut App, line: Line) {
    let mut out = app.on_line(line);
    assert!(out.len() <= 1, "{out:?}");
    let Some(first) = out.pop() else {
        return;
    };
    let asked: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(asked["command"], "commands", "{asked}");
    assert!(
        app.on_line(session_line(
            "command_accepted",
            json!({"command_id": asked["id"], "result": {"commands": []}})
        ))
        .is_empty()
    );
}

/// Folds one numbered turn line for the find scan, recording every envelope
/// in the log the fetch answer later replays.
fn numbered(
    app: &mut App,
    log: &mut Vec<contract::Envelope>,
    seq: &mut u64,
    kind: &str,
    payload: Value,
    action: Option<&str>,
) {
    *seq += 1;
    let envelope = contract::Envelope {
        kind: contract::events::EventKind::from_name(kind),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: Some(contract::Seq(*seq)),
        payload: payload.as_object().cloned().unwrap_or_default(),
    };
    log.push(envelope.clone());
    fold(app, Line::Session(envelope));
}

/// A turn of `replies` text replies, seqs from `seq`, recorded in `log`.
fn text_turn(app: &mut App, log: &mut Vec<contract::Envelope>, seq: &mut u64, replies: &[&str]) {
    numbered(
        app,
        log,
        seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    );
    for reply in replies {
        numbered(
            app,
            log,
            seq,
            "text_completed",
            json!({"text": reply}),
            Some("a_m"),
        );
    }
    numbered(
        app,
        log,
        seq,
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    );
}

/// The `history` fetch `line` asks for: its id and seq range.
fn fetch_command(line: &str) -> (String, u64, u64) {
    let value: Value = serde_json::from_str(line).expect("a command line");
    assert_eq!(value["command"], "history");
    (
        value["id"].as_str().unwrap_or_default().to_owned(),
        value["args"]["from_seq"].as_u64().unwrap_or(0),
        value["args"]["to_seq"].as_u64().unwrap_or(0),
    )
}

/// The hub's `history` answer to `id`, replaying the logged lines it holds.
fn history_answer(id: &str, log: &[contract::Envelope], from: u64, to: u64) -> Line {
    let held: Vec<contract::Envelope> = log
        .iter()
        .filter(|line| line.seq.is_some_and(|seq| from <= seq.0 && seq.0 <= to))
        .cloned()
        .collect();
    envelope(
        "command_accepted",
        json!({"command_id": id, "result": {"lines": held}}),
        None,
        None,
    )
}

/// An app with a find fetch on the wire: enough turns to drop the first
/// page, then a query for the needle the first turn holds.
fn fetching_app() -> (App, Vec<contract::Envelope>, String) {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(40, 10);
    app.on_line(hello());
    app.attach(contract::SessionId(SESSION.to_owned()));
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut app, &mut log, &mut seq, &["a needle here"]);
    for turn in 0..16 {
        let filler: Vec<String> = (0..4).map(|line| format!("filler {turn} {line}")).collect();
        let refs: Vec<&str> = filler.iter().map(String::as_str).collect();
        text_turn(&mut app, &mut log, &mut seq, &refs);
    }
    assert!(app.pages().page_count() > 1);
    assert!(app.pages().part(0).is_none(), "the first page dropped");
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    let mut generation = 0u64;
    for ch in "needle".chars() {
        generation += 1;
        assert_eq!(
            app.on_key(Key::Char(ch), now()),
            Effect::FindPause {
                generation,
                after: crate::app::find::FIND_PAUSE,
            }
        );
    }
    let mut first = app.find_due(generation);
    first.extend(app.find_outgoing());
    assert_eq!(first.len(), 1, "one fetch on the wire");
    let id = fetch_command(&first[0]).0;
    assert_eq!(app.outbox.waiting(&id), Some(Waiting::Find));
    (app, log, first.into_iter().next().unwrap_or_default())
}

/// The handler state an answer must leave alone: the slash list, the model
/// picker's session mark, the find scan, and the offer's open flag.
struct Markers {
    extensions: Vec<String>,
    session_only: Option<(SessionId, String)>,
    current: Option<usize>,
    offer_open: bool,
}

fn markers(app: &App) -> Markers {
    Markers {
        extensions: app.completion.extensions.clone(),
        session_only: app.model_picker.session_only.clone(),
        current: app.find.current_index(),
        offer_open: app.offer_open(),
    }
}

/// The isolation ids, and one tag per variant in turn order.
const ISOLATION_IDS: [&str; 7] = [
    "c_iso_find",
    "c_iso_commands",
    "c_iso_model",
    "c_iso_reply",
    "c_iso_offer",
    "c_iso_steer",
    "c_iso_shell",
];

const ISOLATION_TAGS: [Waiting; 7] = [
    Waiting::Find,
    Waiting::Commands,
    Waiting::Model,
    Waiting::Reply,
    Waiting::OfferFile,
    Waiting::Steer,
    Waiting::Shell,
];

/// Registers every non-owner tag, then snapshots the markers: an answer
/// for the owner must settle only its own tag and move only its handler.
fn isolate(app: &mut App, owner: usize) -> Markers {
    for (index, tag) in ISOLATION_TAGS.iter().enumerate() {
        if index != owner {
            app.outbox.wait(ISOLATION_IDS[index], tag.clone());
        }
    }
    markers(app)
}

/// Every non-owner tag is still registered and no other handler moved.
fn assert_isolated(app: &App, owner: usize, before: &Markers) {
    for (index, tag) in ISOLATION_TAGS.iter().enumerate() {
        if index != owner {
            assert_eq!(
                app.outbox.waiting(ISOLATION_IDS[index]),
                Some(tag.clone()),
                "the {tag:?} tag survives another variant's answer"
            );
        }
    }
    let after = markers(app);
    // The owner's own marker moves by design; every other handler's
    // state must read exactly as before the answer.
    if owner != 1 {
        assert_eq!(after.extensions, before.extensions, "slash list unchanged");
    }
    if owner != 2 {
        assert_eq!(
            after.session_only, before.session_only,
            "session-only mark unchanged"
        );
    }
    if owner != 0 {
        assert_eq!(after.current, before.current, "find scan unchanged");
    }
    assert_eq!(after.offer_open, before.offer_open, "offer unchanged");
}

#[test]
fn no_answer_path_probes_features_in_turn() {
    // Find: the fetch's answer scans the dropped page, and only the scan
    // moves: every other handler's state stays as it was.
    {
        let (mut app, log, fetch) = fetching_app();
        let (id, from, to) = fetch_command(&fetch);
        let before = isolate(&mut app, 0);
        assert!(before.current.is_none(), "no match before the answer");
        app.on_line(history_answer(&id, &log, from, to));
        assert_eq!(app.outbox.waiting(&id), None);
        assert!(
            app.find.current_index().is_some(),
            "the fetch's answer scans"
        );
        assert_isolated(&app, 0, &before);
    }
    // Commands: the answer fills the `/` list from the result rows.
    {
        let mut app = attached();
        let send: Value =
            serde_json::from_str(&app.ask_commands(&SessionId(SESSION.to_owned()))).unwrap();
        let id = send["id"].as_str().unwrap_or_default().to_owned();
        assert_eq!(app.outbox.waiting(&id), Some(Waiting::Commands));
        let before = isolate(&mut app, 1);
        app.on_line(session_line(
            "command_accepted",
            json!({"command_id": id, "result": {"commands": [{
                "name": "tdd", "description": "Runs tdd.",
                "tag": "skill", "extension": true
            }]}}),
        ));
        assert_eq!(app.outbox.waiting(&id), None);
        assert_eq!(app.completion.extensions, vec!["tdd".to_owned()]);
        assert!(app.completion.commands_id.is_none());
        assert_isolated(&app, 1, &before);
    }
    // Model: the accept marks the row for this session only.
    {
        let mut app = attached();
        app.on_line(hello());
        let out = sent(app.choose(Choice {
            reference: "acme/m1".to_owned(),
            level: None,
            level_chosen: false,
            save_model: false,
            session_only: true,
        }));
        assert_eq!(out.len(), 1);
        let id = out[0]["id"].as_str().unwrap_or_default().to_owned();
        assert_eq!(app.outbox.waiting(&id), Some(Waiting::Model));
        let before = isolate(&mut app, 2);
        app.on_line(session_line("command_accepted", json!({"command_id": id})));
        assert_eq!(app.outbox.waiting(&id), None);
        assert_eq!(
            app.model_picker.session_only,
            Some((SessionId(SESSION.to_owned()), "acme/m1".to_owned()))
        );
        assert_isolated(&app, 2, &before);
    }
    // Reply: declining answers with a `reply` the accept then cancels.
    {
        let mut app = attached();
        fold(
            &mut app,
            envelope(
                "interaction_requested",
                json!({"request_id": "r_c", "kind": "confirm",
                    "prompt": "Continue?", "action_ids": ["a_1"]}),
                None,
                None,
            ),
        );
        let Effect::Send(lines) = app.on_click(TargetId::Form(Spot::Chat)) else {
            panic!("the chat button declines");
        };
        assert_eq!(lines.len(), 1);
        let reply: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(reply["command"], "reply");
        let id = reply["id"].as_str().unwrap_or_default().to_owned();
        assert_eq!(app.outbox.waiting(&id), Some(Waiting::Reply));
        let before = isolate(&mut app, 3);
        let out = app.on_line(session_line("command_accepted", json!({"command_id": id})));
        assert_eq!(app.outbox.waiting(&id), None);
        assert_eq!(out.len(), 1, "the accept ends the turn with a cancel");
        let cancel: Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(cancel["command"], "cancel");
        assert_isolated(&app, 3, &before);
    }
    // OfferFile: the answer folds into the source view, showing its text.
    {
        let mut app = attached();
        app.set_size(80, 24);
        fold(
            &mut app,
            envelope(
                "repository_code_offered",
                json!({"request_id": "r_1", "items": [{"kind": "hook",
                    "name": "db", "hash": "h", "required": false,
                    "summary": "hook",
                    "files": ["tools/db.sh", "tools/b.sh"]}]}),
                None,
                None,
            ),
        );
        assert!(app.offer_open());
        let Effect::Send(lines) = app.on_key(Key::Enter, now()) else {
            panic!("Enter reads the offered source");
        };
        assert_eq!(lines.len(), 1);
        let first: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(first["command"], "offered_file");
        let id = first["id"].as_str().unwrap_or_default().to_owned();
        assert_eq!(app.outbox.waiting(&id), Some(Waiting::OfferFile));
        let before = isolate(&mut app, 4);
        app.on_line(session_line(
            "command_accepted",
            json!({"command_id": id, "result": {"text": "first source"}}),
        ));
        assert_eq!(app.outbox.waiting(&id), None);
        let text = app
            .offer_rows(app.column_width())
            .expect("the source stays open")
            .0
            .iter()
            .map(|row| row.line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("first source"), "{text}");
        assert_isolated(&app, 4, &before);
    }
    // Steer: the arm settles the tag; no handler owns state to move.
    {
        let mut app = attached();
        fold(
            &mut app,
            envelope(
                "steering_queue",
                json!({"messages": [
                    {"content": [{"type": "text", "text": "one"}],
                        "source": "driver", "command_id": "c_1"},
                    {"content": [{"type": "text", "text": "two"}],
                        "source": "driver", "command_id": "c_2"}]}),
                None,
                None,
            ),
        );
        app.select_steering(0);
        let Effect::Send(lines) = app.on_key(Key::Enter, now()) else {
            panic!("Enter amends the selected steering");
        };
        assert_eq!(lines.len(), 2);
        let steer: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(steer["command"], "steer");
        let id = steer["id"].as_str().unwrap_or_default().to_owned();
        assert_eq!(app.outbox.waiting(&id), Some(Waiting::Steer));
        let before = isolate(&mut app, 5);
        app.on_line(session_line("command_accepted", json!({"command_id": id})));
        assert_eq!(app.outbox.waiting(&id), None);
        assert!(!app.outbox.pending.contains_key(&id));
        assert_isolated(&app, 5, &before);
    }
    // Shell: the accept folds the run's output into the conversation
    // (`!!` shows it; `!` sends it, arriving on `shell_command`).
    {
        let mut app = attached();
        let out = sent(enter(&mut app, "!!git status"));
        assert_eq!(out.len(), 1);
        let id = out[0]["id"].as_str().unwrap_or_default().to_owned();
        assert_eq!(app.outbox.waiting(&id), Some(Waiting::Shell));
        let before = isolate(&mut app, 6);
        app.on_line(session_line(
            "command_accepted",
            json!({"command_id": id, "result": {
                "output": "clean\n",
                "process": {"exit_code": 0, "timed_out": false}
            }}),
        ));
        assert_eq!(app.outbox.waiting(&id), None);
        let texts: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
        assert!(texts.iter().any(|text| text.contains("clean")), "{texts:?}");
        assert_isolated(&app, 6, &before);
    }
}

/// A home app with one opening session: its lines fold into the row and
/// are dropped until the raise's acknowledgement.
fn opening() -> (App, SessionId) {
    fn launch() -> crate::home::Launch {
        crate::home::Launch {
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

    let mut app = home_with(80, 24, launch());
    let session = SessionId("s_gated_commands".to_owned());
    app.open_session(session.clone(), None, None, false);
    assert_eq!(app.opening_session(), Some(session.clone()));
    (app, session)
}

/// The gated opening's answer, carrying the `commands` result rows.
fn gated_answer(session: &SessionId, id: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: EventKind::CommandAccepted,
        session_id: session.clone(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"command_id": id, "result": {"commands": [{
            "name": "tdd", "description": "Runs tdd.",
            "tag": "skill", "extension": true
        }]}})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

#[test]
fn a_gated_opening_commands_answer_fills_the_slash_list_by_tag() {
    let (mut app, session) = opening();
    let send: Value = serde_json::from_str(&app.ask_commands(&session)).unwrap();
    let id = send["id"].as_str().unwrap_or_default().to_owned();
    assert_eq!(app.outbox.waiting(&id), Some(Waiting::Commands));
    assert!(app.on_line(gated_answer(&session, &id)).is_empty());
    // The tag routes the answer to the slash list while the gate holds.
    assert_eq!(app.completion.extensions, vec!["tdd".to_owned()]);
    assert!(app.completion.commands_id.is_none());
    assert_eq!(app.opening_session(), Some(session));
}

#[test]
fn a_gated_non_commands_answer_still_gates() {
    let (mut app, session) = opening();
    let send: Value = serde_json::from_str(&app.ask_commands(&session)).unwrap();
    let id = send["id"].as_str().unwrap_or_default().to_owned();
    app.outbox.wait(&id, Waiting::Steer);
    assert_eq!(app.outbox.waiting(&id), Some(Waiting::Steer));
    assert!(app.on_line(gated_answer(&session, &id)).is_empty());
    // A non-`Commands` tag never reaches the list: the gate still holds
    // and the tag stays registered for its own answer.
    assert!(app.completion.extensions.is_empty());
    assert_eq!(app.opening_session(), Some(session));
    assert_eq!(app.outbox.waiting(&id), Some(Waiting::Steer));
}

/// A renderer serving the TUI command only while the halves name acme
/// 1.0.0: the adapter's eligibility gate as the terminal sees it.
#[derive(Debug)]
struct Halved;

impl Renderer for Halved {
    fn name(&self) -> &str {
        "halved"
    }

    fn fills(&self, _slot: &str) -> bool {
        false
    }

    fn render(
        &self,
        _ctx: &SessionCtx,
        _slot: &str,
        _input: &RendererInput<'_>,
        _width: u16,
        _builtin: &dyn Fn(&RendererInput<'_>, u16) -> Vec<Vec<RawSpan>>,
    ) -> Decision {
        Decision::NotForSession {
            extension: String::new(),
            tui_version: String::new(),
            half: SessionVersion::Unseen,
        }
    }

    fn tui_commands(&self, ctx: &SessionCtx) -> Vec<TuiCommand> {
        let eligible = ctx
            .halves
            .as_ref()
            .is_some_and(|halves| halves.get("acme").is_some_and(|half| half == "1.0.0"));
        if eligible {
            vec![TuiCommand {
                name: "ledger-diff".to_owned(),
                description: "Diffs.".to_owned(),
                extension: "acme".to_owned(),
            }]
        } else {
            Vec::new()
        }
    }
}

/// An `extensions_loaded` line naming `version` for acme.
fn halves_loaded(version: &str) -> Line {
    session_line(
        "extensions_loaded",
        json!({"extensions": [{"name": "acme", "version": version}]}),
    )
}

#[test]
fn command_extensions_loaded_refreshes_tui_eligibility_before_requery() {
    let mut app = attached();
    app.install_renderer(std::rc::Rc::new(Halved));
    // Newly compatible halves list the TUI row before the commands answer arrives.
    let asked = app.on_line(halves_loaded("1.0.0"));
    assert_eq!(asked.len(), 1);
    let asked: Value = serde_json::from_str(&asked[0]).unwrap_or_default();
    assert_eq!(asked["command"], "commands");
    assert!(
        app.completion
            .slash_rows
            .iter()
            .any(|row| row.name == "ledger-diff"),
        "TUI eligibility refreshes against the recorded halves before re-query"
    );
    let answer = session_line(
        "command_accepted",
        json!({"command_id": asked["id"], "result": {"commands": []}}),
    );
    assert!(app.on_line(answer).is_empty());
    assert!(
        app.completion
            .slash_rows
            .iter()
            .any(|row| row.name == "ledger-diff"),
        "the session answer keeps the TUI rows"
    );
    // Newly incompatible halves drop the TUI row without waiting for the answer, and re-query still goes out.
    let asked = app.on_line(halves_loaded("9.9.9"));
    assert_eq!(asked.len(), 1);
    let asked: Value = serde_json::from_str(&asked[0]).unwrap_or_default();
    assert_eq!(asked["command"], "commands");
    assert!(
        app.completion
            .slash_rows
            .iter()
            .all(|row| row.name != "ledger-diff"),
        "a newly incompatible TUI row drops before the commands answer"
    );
}
