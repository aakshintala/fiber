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
