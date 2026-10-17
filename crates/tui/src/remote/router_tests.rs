use super::*;
use std::io::{BufRead, BufReader};
use std::sync::mpsc;
use std::thread;

use fakes::Deadline;
use serde_json::json;

fn answer(id: &str, kind: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: kind.into(),
        ts: 1,
        schema_version: 1,
        payload: serde_json::from_value(json!({"command_id":id,"result":{"ok":true}})).unwrap(),
    })
}

fn read(stream: UnixStream) -> mpsc::Receiver<Value> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            let value = serde_json::from_str(&line).unwrap();
            if tx.send(value).is_err() {
                break;
            }
        }
    });
    rx
}

fn wait(pending: Pending) -> mpsc::Receiver<Result<contract::HubLine, AskError>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(pending.wait());
    });
    rx
}

#[test]
fn routes_each_answer_once_and_forgets_it() {
    let deadline = Deadline::start();
    let router = Router::new();
    let (client, hub) = UnixStream::pair().unwrap();
    router.attach(client);
    let lines = read(hub);
    for kind in ["command_accepted", "command_rejected"] {
        let pending = router
            .send("configure", json!({"operation":"settings"}))
            .unwrap();
        let line = deadline.recv_or_fail(&lines, "request");
        assert_eq!(line["command"], "configure");
        assert_eq!(line["args"], json!({"operation":"settings"}));
        assert_eq!(router.registered(), 1);
        let response = answer(line["id"].as_str().unwrap(), kind);
        assert!(router.route(&response));
        assert!(!router.route(&response));
        assert_eq!(router.registered(), 0);
        assert_eq!(
            deadline
                .recv_or_fail(&wait(pending), "answer")
                .unwrap()
                .kind,
            kind
        );
    }
    assert!(!router.route(&answer("unknown", "command_accepted")));
    router.detach();
}

#[test]
fn epochs_reject_a_result_on_another_connection() {
    let deadline = Deadline::start();
    let router = Router::new();
    let (client, hub) = UnixStream::pair().unwrap();
    router.attach(client);
    let lines = read(hub);
    let pending = router.send("device_login", json!({"name":"acme"})).unwrap();
    assert_eq!(pending.epoch(), 1);
    deadline.recv_or_fail(&lines, "start");
    let result = wait(pending);
    router.detach();
    assert_eq!(
        deadline.recv_or_fail(&result, "lost answer"),
        Err(AskError::Lost)
    );
    let (client, hub) = UnixStream::pair().unwrap();
    router.attach(client);
    let lines = read(hub);
    assert!(matches!(
        router.send_on(1, "device_login_result", json!({})),
        Err(AskError::OtherConnection)
    ));
    assert_eq!(router.registered(), 0);
    assert!(
        Deadline::after(std::time::Duration::from_millis(100))
            .recv(&lines)
            .is_err()
    );
    let pending = router.send_on(2, "device_login_result", json!({})).unwrap();
    assert_eq!(pending.epoch(), 2);
    let line = deadline.recv_or_fail(&lines, "new result");
    assert!(router.route(&answer(line["id"].as_str().unwrap(), "command_accepted")));
    assert!(deadline.recv_or_fail(&wait(pending), "new answer").is_ok());
    router.detach();
}

#[test]
fn detached_send_and_loop_ask_return_without_registering() {
    let deadline = Deadline::start();
    let router = Router::new();
    let (tx, rx) = mpsc::channel();
    let worker = Arc::clone(&router);
    thread::spawn(move || {
        let lock = Mutex::new(());
        let guard = support::lock(&lock);
        let sent = worker.send("configure", json!({})).err();
        drop(guard);
        let asked = worker.ask("configure", json!({}), Caller::Loop).err();
        tx.send((sent, asked)).unwrap();
    });
    assert_eq!(
        deadline.recv_or_fail(&rx, "detached calls"),
        (Some(AskError::Detached), Some(AskError::Detached))
    );
    assert_eq!(router.registered(), 0);
}

fn await_link(router: &Router, deadline: &Deadline) {
    while router.link_waiters() != 1 {
        assert!(
            !deadline.left().is_zero(),
            "worker did not park waiting for the link"
        );
        thread::yield_now();
    }
}

#[test]
fn detached_worker_waits_for_attach_and_can_be_cancelled() {
    let deadline = Deadline::start();
    let router = Router::new();
    let cancel = Cancel::default();
    let (tx, rx) = mpsc::channel();
    let worker = Arc::clone(&router);
    let token = cancel.clone();
    thread::spawn(move || {
        tx.send(worker.ask("configure", json!({}), Caller::Worker(&token)))
            .unwrap();
    });
    await_link(&router, &deadline);
    cancel.set();
    router.wake();
    assert_eq!(
        deadline.recv_or_fail(&rx, "cancelled link wait"),
        Err(AskError::Cancelled)
    );
    let (client, hub) = UnixStream::pair().unwrap();
    router.attach(client);
    let lines = read(hub);
    let (tx, rx) = mpsc::channel();
    let worker = Arc::clone(&router);
    thread::spawn(move || {
        tx.send(worker.ask("configure", json!({}), Caller::Worker(&Cancel::default())))
            .unwrap();
    });
    let line = deadline.recv_or_fail(&lines, "worker request");
    router.route(&answer(line["id"].as_str().unwrap(), "command_accepted"));
    assert!(deadline.recv_or_fail(&rx, "worker answer").is_ok());
    router.detach();
}

#[test]
fn failed_write_removes_registration_and_detach_ends_all_waits() {
    let router = Router::new();
    let (client, hub) = UnixStream::pair().unwrap();
    drop(hub);
    router.attach(client);
    assert!(matches!(
        router.send("configure", json!({})),
        Err(AskError::Lost)
    ));
    assert_eq!(router.registered(), 0);
    router.detach();
    let (client, hub) = UnixStream::pair().unwrap();
    router.attach(client);
    let lines = read(hub);
    let deadline = Deadline::start();
    let first = wait(router.send("one", json!({})).unwrap());
    let second = wait(router.send("two", json!({})).unwrap());
    deadline.recv_or_fail(&lines, "first request");
    deadline.recv_or_fail(&lines, "second request");
    router.detach();
    assert_eq!(
        deadline.recv_or_fail(&first, "first lost wait"),
        Err(AskError::Lost)
    );
    assert_eq!(
        deadline.recv_or_fail(&second, "second lost wait"),
        Err(AskError::Lost)
    );
    assert_eq!(router.registered(), 0);
}

#[test]
fn worker_rechecks_after_a_link_wait_and_detach() {
    let deadline = Deadline::start();
    let router = Router::new();
    let (arrived, at_pause) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    *support::lock(&router.pause_before_send) = Some((arrived, resume));
    let (tx, rx) = mpsc::channel();
    let worker = Arc::clone(&router);
    thread::spawn(move || {
        tx.send(worker.ask("configure", json!({}), Caller::Worker(&Cancel::default())))
            .unwrap();
    });
    await_link(&router, &deadline);
    let (client, hub) = UnixStream::pair().unwrap();
    let old_lines = read(hub);
    router.attach(client);
    deadline.recv_or_fail(&at_pause, "worker after link wait");
    router.detach();
    release.send(()).unwrap();
    await_link(&router, &deadline);
    assert!(old_lines.try_recv().is_err());
    let (client, hub) = UnixStream::pair().unwrap();
    let lines = read(hub);
    router.attach(client);
    let line = deadline.recv_or_fail(&lines, "request on replacement link");
    router.route(&answer(line["id"].as_str().unwrap(), "command_accepted"));
    assert!(deadline.recv_or_fail(&rx, "replacement answer").is_ok());
    router.detach();
}

#[test]
fn mapped_answers_use_the_feed_without_credits() {
    let deadline = Deadline::start();
    let router = Router::new();
    let (tx, rx) = mpsc::channel();
    router.set_feed(tx, crate::link::Credits::new(0));
    let (client, hub) = UnixStream::pair().unwrap();
    router.attach(client);
    let lines = read(hub);
    router
        .ask_into(
            "search_files",
            json!({}),
            Box::new(|_| {
                Some(Input::Files {
                    generation: 7,
                    result: Ok(vec!["src/".into()]),
                })
            }),
        )
        .unwrap();
    let line = deadline.recv_or_fail(&lines, "mapped request");
    router.route(&answer(line["id"].as_str().unwrap(), "command_accepted"));
    assert!(
        matches!(deadline.recv_or_fail(&rx, "mapped input"), Input::Files { generation: 7, result: Ok(rows) } if rows == ["src/"])
    );
    router
        .ask_into("search_files", json!({}), Box::new(|_| None))
        .unwrap();
    let line = deadline.recv_or_fail(&lines, "ignored request");
    router.route(&answer(line["id"].as_str().unwrap(), "command_rejected"));
    assert!(rx.try_recv().is_err());
    router.detach();
    assert_eq!(
        router.ask_into("search_files", json!({}), Box::new(|_| None)),
        Err(AskError::Detached)
    );
}
