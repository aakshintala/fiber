//! Normalisation of hub and session command answers.

use super::Answer;
use crate::app::outbox::{Outbox, Waiting};
use crate::link::Line;
use contract::{Envelope, HubLine, SessionId};
use serde_json::{Value, json};

fn line(session: bool, kind: &str, payload: Value) -> Line {
    let payload = payload.as_object().unwrap().clone();
    if session {
        Line::Session(Envelope {
            kind: kind.to_owned(),
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
            kind: kind.to_owned(),
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

#[test]
fn images_disconnected_forgets_fetch_tags_and_keeps_display_tags() {
    let mut app = crate::app::commands::commands_testkit::attached();
    app.outbox.wait("c_fetch", Waiting::ImageFetch);
    app.outbox.wait("c_display", Waiting::ImageDisplay);
    app.images_disconnected();
    assert_eq!(app.outbox.waiting("c_fetch"), None);
    assert_eq!(app.outbox.waiting("c_display"), Some(Waiting::ImageDisplay));
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
    for tag in [Waiting::ImageFetch, Waiting::ImageDisplay, Waiting::History] {
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
