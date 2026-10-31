//! Reconnect joins at delivery, without crossing admission or transitions.

use std::sync::{Arc, mpsc};

use contract::CommandId;
use contract::commands::{ChangeWorkspaceArgs, McpServerArgs, RewindArgs};
use contract::inbox::{Ack, Answer, Delivery};
use fakes::{Deadline, MUST_SUCCEED_WITHIN};

#[allow(
    clippy::duplicate_mod,
    reason = "unit-test files share the session fixture"
)]
#[path = "../tests/support/mod.rs"]
mod support;

fn reconnect(server: &str) -> (Delivery, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (
        Delivery::McpReconnect(
            McpServerArgs {
                server: server.into(),
            },
            Ack(Box::new(move |answer| {
                tx.send(answer).unwrap();
            })),
        ),
        rx,
    )
}

#[test]
fn an_executing_reconnect_joins_without_an_inbox_delivery() {
    let opened = support::Opened::open(Vec::new());
    let gate = opened.session.gate_for_test();
    let (tx, inbox) = mpsc::channel();
    *super::lock(&gate.inbox) = Some(tx);
    let flights = opened.session.reconnect_flights();
    super::lock(&flights).insert("db".into());
    // Answering is outside the flight and admission locks: a caller may
    // inspect the outcome or close admission from its acknowledgement.
    let observed = Arc::clone(&flights);
    let admission = Arc::clone(&gate.admission);
    let (tx, answer) = mpsc::channel();
    let deliver = Arc::clone(gate);
    fakes::within(
        "delivery with a reentrant acknowledgement",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            deliver.deliver(Delivery::McpReconnect(
                McpServerArgs {
                    server: "db".into(),
                },
                Ack(Box::new(move |outcome| {
                    assert!(super::lock(&observed).contains("db"));
                    assert_eq!(admission.admit(|| true), Some(true));
                    tx.send(outcome).unwrap();
                })),
            ))
        },
    );
    assert!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&answer, "the joined acknowledgement")
            .unwrap()
            .is_none()
    );
    assert!(matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)));
    opened.close();
}

#[test]
fn absent_different_and_ended_flights_deliver_normally() {
    let opened = support::Opened::open(Vec::new());
    let gate = opened.session.gate_for_test();
    let (tx, inbox) = mpsc::channel();
    *super::lock(&gate.inbox) = Some(tx);
    let flights = opened.session.reconnect_flights();
    for present in [None, Some("other"), Some("db")] {
        super::lock(&flights).clear();
        if let Some(server) = present {
            super::lock(&flights).insert(server.into());
        }
        // Removal re-arms retries even before the first ack is sent.
        super::lock(&flights).remove("db");
        let (delivery, answer) = reconnect("db");
        gate.deliver(delivery);
        assert!(
            matches!(inbox.try_recv(), Ok(Delivery::McpReconnect(args, _)) if args.server == "db")
        );
        assert!(matches!(
            answer.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }
    opened.close();
}

#[test]
fn transitions_and_closed_admission_keep_reconnects_on_the_inbox_path() {
    for transition in [
        Delivery::Rewind(
            CommandId("rewind".into()),
            RewindArgs::AtEnd,
            Ack(Box::new(|_| {})),
        ),
        Delivery::ChangeWorkspace(
            CommandId("move".into()),
            ChangeWorkspaceArgs { path: ".".into() },
            Ack(Box::new(|_| {})),
        ),
    ] {
        let opened = support::Opened::open(Vec::new());
        let gate = opened.session.gate_for_test();
        let (tx, inbox) = mpsc::channel();
        *super::lock(&gate.inbox) = Some(tx);
        super::lock(&opened.session.reconnect_flights()).insert("db".into());
        gate.deliver(transition);
        assert!(matches!(
            inbox.try_recv(),
            Ok(Delivery::Rewind(..) | Delivery::ChangeWorkspace(..))
        ));
        let (delivery, answer) = reconnect("db");
        gate.deliver(delivery);
        assert!(matches!(inbox.try_recv(), Ok(Delivery::McpReconnect(..))));
        assert!(matches!(
            answer.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        opened.close();
    }
    let opened = support::Opened::open(Vec::new());
    let gate = opened.session.gate_for_test();
    let (tx, inbox) = mpsc::channel();
    *super::lock(&gate.inbox) = Some(tx);
    super::lock(&opened.session.reconnect_flights()).insert("db".into());
    gate.mark_closed();
    let (delivery, answer) = reconnect("db");
    gate.deliver(delivery);
    assert!(matches!(inbox.try_recv(), Ok(Delivery::McpReconnect(..))));
    assert!(matches!(
        answer.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
    opened.close();
}
