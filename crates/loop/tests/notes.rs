//! Reviewer-note requests through the loop's public inbox.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::sync::mpsc;

use contract::events::CommandResult;
use contract::inbox::{Ack, Delivery};
use contract::notes::{Evidence, NotesAsk, ProposeNotesArgs, Topic};
use fakes::{Deadline, Scripted};
use support::{DEADLINE, Session, spawn_run};

#[test]
fn notes_on_the_inbox_record_usage_before_acknowledging() {
    let mut session = Session::new(vec![Scripted::text("1 | This is ours.")], None);
    let dir = session.dir.clone();
    let (answer, answered) = mpsc::channel();
    session
        .inbox
        .send(Delivery::ProposeNotes(
            NotesAsk {
                args: ProposeNotesArgs {
                    topic: Topic::SourceControl,
                    answer: None,
                    earlier: vec![],
                },
                evidence: vec![Evidence::Files {
                    text: "our remote".into(),
                }],
            },
            Ack(Box::new(move |result| {
                // Read inside the ack, not after it: a later append cannot satisfy this assertion.
                answer.send((result, log::read(&dir).unwrap())).unwrap();
            })),
        ))
        .unwrap();
    let finished = spawn_run(&mut session);
    let (result, at_ack) =
        Deadline::after(DEADLINE).recv_or_fail(&answered, "notes request acknowledged");
    let Ok(Some(CommandResult::ProposeNotes {
        evidence,
        proposed,
        error,
    })) = result
    else {
        panic!("notes returned the wrong result: {result:?}");
    };
    assert!(error.is_none());
    assert_eq!(
        evidence,
        [Evidence::Files {
            text: "our remote".into()
        }]
    );
    assert_eq!(proposed.len(), 1);
    assert_eq!(proposed[0].text(), "This is ours.");
    assert_eq!(proposed[0].cites(), [0]);
    assert_eq!(
        at_ack
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>(),
        ["session_started", "usage_recorded"]
    );
    let usage = &at_ack[1];
    assert!(usage.turn_id.is_none());
    assert!(usage.action_id.is_none());
    assert!(usage.payload.get("reviewer").is_none());
    assert_eq!(session.requests().len(), 1);
    session
        .inbox
        .send(Delivery::Close(support::ignore()))
        .unwrap();
    let ran = Deadline::after(DEADLINE).recv_or_fail(&finished, "notes session closed");
    assert!(ran.is_ok(), "{ran:?}");
}
