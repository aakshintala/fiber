use std::sync::mpsc;
use std::time::Duration;

use contract::events::{DelegateStarted, JobStarted};
use contract::inbox::{Ack, Delivery};
use contract::{JobId, SessionId};

use super::Queue;

fn start(id: &str) -> Delivery {
    Delivery::ExtensionJob(
        JobStarted {
            job_id: JobId(id.into()),
            tool: None,
            extension: Some("test/scan".into()),
            description: "scan".into(),
            output_path: "/child/events.jsonl".into(),
        },
        DelegateStarted {
            job_id: JobId(id.into()),
            delegate_session_id: SessionId("s_child".into()),
            harness: "fiber".into(),
            model: "fake/model".into(),
            workspace: "/w".into(),
            worktree: None,
            forked_from: None,
        },
    )
}

fn ids(deliveries: Vec<Delivery>) -> Vec<String> {
    deliveries
        .into_iter()
        .map(|delivery| {
            let Delivery::ExtensionJob(started, _) = delivery else {
                panic!("expected an extension start");
            };
            started.job_id.0
        })
        .collect()
}

#[test]
fn extension_drain_keeps_other_deliveries_in_order() {
    let (tx, rx) = mpsc::channel();
    let mut queue = Queue::new(rx);
    tx.send(Delivery::Cancelled).unwrap();
    tx.send(start("first")).unwrap();
    tx.send(Delivery::Close(Ack(Box::new(|_| {})))).unwrap();
    assert_eq!(ids(queue.take_extension_jobs()), ["first"]);
    tx.send(start("second")).unwrap();
    tx.send(Delivery::Cancelled).unwrap();
    tx.send(start("third")).unwrap();
    assert_eq!(ids(queue.take_extension_jobs()), ["second", "third"]);
    // Repeating the selective drain never returns a held non-start.
    assert!(queue.take_extension_jobs().is_empty());
    let deliveries: Vec<_> = queue.try_iter().collect();
    assert_eq!(deliveries.len(), 3);
    assert!(matches!(deliveries[0], Delivery::Cancelled));
    assert!(matches!(deliveries[1], Delivery::Close(_)));
    assert!(matches!(deliveries[2], Delivery::Cancelled));
}

#[test]
fn message_drain_keeps_other_deliveries_in_order() {
    let (tx, rx) = mpsc::channel();
    let mut queue = Queue::new(rx);
    let first = || crate::inbox::session_message::for_test("s_from", None, None, "c_1", "first");
    let second = || crate::inbox::session_message::for_test("s_from", None, None, "c_2", "second");
    tx.send(Delivery::Cancelled).unwrap();
    tx.send(Delivery::Message(first(), Ack(Box::new(|_| {}))))
        .unwrap();
    tx.send(Delivery::Close(Ack(Box::new(|_| {})))).unwrap();
    tx.send(Delivery::Message(second(), Ack(Box::new(|_| {}))))
        .unwrap();
    let texts: Vec<String> = queue
        .take_messages()
        .into_iter()
        .map(|delivery| {
            let Delivery::Message(message, _) = delivery else {
                panic!("expected a session message");
            };
            message.text
        })
        .collect();
    assert_eq!(texts, ["first", "second"]);
    // Repeating the selective drain never returns a held non-message.
    assert!(queue.take_messages().is_empty());
    let deliveries: Vec<_> = queue.try_iter().collect();
    assert_eq!(deliveries.len(), 2);
    assert!(matches!(deliveries[0], Delivery::Cancelled));
    assert!(matches!(deliveries[1], Delivery::Close(_)));
}

#[test]
fn try_recv_takes_held_before_receiver() {
    let (tx, rx) = mpsc::channel();
    let mut queue = Queue::new(rx);
    tx.send(Delivery::Cancelled).unwrap();
    assert!(queue.take_extension_jobs().is_empty());
    tx.send(Delivery::Close(Ack(Box::new(|_| {})))).unwrap();
    assert!(matches!(queue.try_recv(), Ok(Delivery::Cancelled)));
    assert!(matches!(queue.try_recv(), Ok(Delivery::Close(_))));
    assert!(matches!(queue.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

#[test]
fn blocking_reads_return_held_even_after_disconnect() {
    let (tx, rx) = mpsc::channel();
    let mut queue = Queue::new(rx);
    tx.send(Delivery::Cancelled).unwrap();
    tx.send(Delivery::Close(Ack(Box::new(|_| {})))).unwrap();
    assert!(queue.take_extension_jobs().is_empty());
    drop(tx);
    // Disconnection would return an error if either read reached the receiver.
    assert!(matches!(
        queue.recv_timeout(Duration::ZERO),
        Ok(Delivery::Cancelled)
    ));
    assert!(matches!(queue.recv(), Ok(Delivery::Close(_))));
    assert!(matches!(queue.recv(), Err(mpsc::RecvError)));
}
