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

fn reconnect(server: &str, name: &'static str, seen: mpsc::Sender<&'static str>) -> Delivery {
    Delivery::McpReconnect(
        contract::commands::McpServerArgs {
            server: server.into(),
        },
        Ack(Box::new(move |outcome| {
            assert!(outcome.unwrap().is_none());
            seen.send(name).unwrap();
        })),
    )
}

#[test]
fn reconnect_drain_stops_at_close_rewind_and_a_move_to_another_folder() {
    let root = fakes::TempDir::new("reconnect-boundary");
    let other = root.path().join("other");
    std::fs::create_dir(&other).unwrap();
    for transition in [
        Delivery::Close(Ack(Box::new(|_| {}))),
        Delivery::Rewind(
            contract::CommandId("rewind".into()),
            contract::commands::RewindArgs::AtEnd,
            Ack(Box::new(|_| {})),
        ),
        Delivery::ChangeWorkspace(
            contract::CommandId("move".into()),
            contract::commands::ChangeWorkspaceArgs {
                path: other.display().to_string(),
            },
            Ack(Box::new(|_| {})),
        ),
        Delivery::ChangeWorkspace(
            contract::CommandId("missing".into()),
            contract::commands::ChangeWorkspaceArgs {
                path: "missing".into(),
            },
            Ack(Box::new(|_| {})),
        ),
    ] {
        let (tx, rx) = mpsc::channel();
        let mut queue = Queue::new(rx);
        let (seen, answers) = mpsc::channel();
        tx.send(Delivery::Cancelled).unwrap();
        tx.send(reconnect("db", "first", seen.clone())).unwrap();
        tx.send(reconnect("other", "different", seen.clone()))
            .unwrap();
        tx.send(reconnect("db", "second", seen.clone())).unwrap();
        tx.send(transition).unwrap();
        tx.send(reconnect("db", "after", seen)).unwrap();
        for ack in queue.take_reconnects("db", root.path(), None) {
            (ack.0)(Ok(None));
        }
        // Only pre-transition duplicates join; everything else retains
        // arrival order (`docs/invocation.md`, "Lifecycle").
        assert_eq!(answers.try_iter().collect::<Vec<_>>(), ["first", "second"]);
        let held: Vec<_> = queue.try_iter().collect();
        assert_eq!(held.len(), 4);
        assert!(matches!(&held[0], Delivery::Cancelled));
        assert!(matches!(&held[1], Delivery::McpReconnect(args, _) if args.server == "other"));
        assert!(matches!(
            &held[2],
            Delivery::Close(..) | Delivery::Rewind(..) | Delivery::ChangeWorkspace(..)
        ));
        assert!(matches!(&held[3], Delivery::McpReconnect(args, _) if args.server == "db"));
    }
}

#[test]
fn reconnect_drain_joins_past_a_same_folder_move_without_taking_it() {
    let root = fakes::TempDir::new("reconnect-same-folder");
    for path in [".", "~/"] {
        let (tx, rx) = mpsc::channel();
        let mut queue = Queue::new(rx);
        let (seen, answers) = mpsc::channel();
        tx.send(reconnect("db", "before", seen.clone())).unwrap();
        tx.send(Delivery::ChangeWorkspace(
            contract::CommandId("same".into()),
            contract::commands::ChangeWorkspaceArgs { path: path.into() },
            Ack(Box::new(|_| {})),
        ))
        .unwrap();
        tx.send(reconnect("db", "after", seen)).unwrap();
        for ack in queue.take_reconnects("db", root.path(), Some(root.path())) {
            (ack.0)(Ok(None));
        }
        // Same-folder moves reject without ending the session
        // (`docs/invocation.md`, "Driver commands").
        assert_eq!(answers.try_iter().collect::<Vec<_>>(), ["before", "after"]);
        assert!(matches!(
            queue.try_recv(),
            Ok(Delivery::ChangeWorkspace(..))
        ));
        assert!(queue.try_recv().is_err());
    }
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
