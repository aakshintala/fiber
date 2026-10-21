//! The running-session client against real session sockets.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures and assertions"
)]
mod support;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{Event, SessionState, SessionStatus};
use contract::inbox::Delivery;
use contract::sessions::{Running, Sessions};
use contract::shapes::{Tokens, Usage};
use doors::{Session, SessionSockets, mint};
use fakes::CancelToken;
use fakes::Client;
use fakes::Deadline;
use fakes::clock::FakeClock;
use log::Log;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, mpsc};
use std::thread;

#[test]
fn real_sessions_answer_twice_with_their_latest_summary_and_parent() {
    let temp = support::Temp::new();
    let home = temp.0.join("h");
    let sessions = home.join("projects/p/sessions");
    let clock = FakeClock::new();
    let timed: Arc<dyn Clock> = clock.clone();
    let mut ids = [
        contract::SessionId(mint("s_")),
        contract::SessionId(mint("s_")),
    ];
    ids.sort_by(|a, b| a.0.cmp(&b.0));
    let mut opened = Vec::new();
    let mut expected = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let log = Arc::new(Log::create(&sessions, id.clone(), timed.clone()).unwrap());
        let session = Session::open(
            &home,
            &sessions.join(&id.0),
            &log,
            timed.clone(),
            vec![],
            Box::new(support::Shared::new()),
        )
        .unwrap();
        let status = SessionStatus {
            name: format!("session {index}"),
            workspace: format!("/w/{index}"),
            parent: (index == 1).then(|| ids[0].clone()),
            model: "m".into(),
            thinking: None,
            state: SessionState::Idle,
            since: 0,
            started: 0,
            calls: 0,
            turns: Some(0),
            step: None,
            git: None,
            context: None,
            spend: Usage {
                tokens: Tokens {
                    input: 0,
                    cache_read: 0,
                    cache_write: Default::default(),
                    output: 0,
                },
                cost: Some(0.0),
                subscription_cost: 0.0,
            },
            delegates: 0,
            jobs: 0,
            project: "p".into(),
            clients: 0,
            closing: false,
        };
        log.append(&Event::SessionStatus(status.clone()), None, None)
            .unwrap();
        expected.push(Running {
            id: id.clone(),
            status: Some(status),
        });
        let (release, held) = mpsc::channel::<()>();
        let (ready, started) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            session
                .run(vec![], Arc::new(|| false), move |_| {
                    ready.send(()).unwrap();
                    // The test owns release and drops it before waiting for cleanup.
                    if let Ok(()) = held.recv() {}
                    Ok(())
                })
                .unwrap();
            session.close(log);
            done.send(()).unwrap();
        });
        Deadline::after(fakes::deadline::WAITS).recv_or_fail(&started, "session serving");
        opened.push((release, finished));
    }
    let client = SessionSockets::new(&home, timed);
    assert_eq!(client.scope().as_os_str(), home.join("run/").as_os_str());
    for _ in 0..2 {
        let client = SessionSockets::new(&home, clock.clone());
        let (tx, rx) = mpsc::channel();
        thread::spawn(
            move || {
                if let Ok(()) = tx.send(client.list(&CancelToken::new())) {}
            },
        );
        assert_eq!(
            Deadline::after(fakes::deadline::WAITS)
                .recv_or_fail(&rx, "real summaries")
                .unwrap(),
            expected
        );
    }
    for (release, finished) in opened {
        drop(release);
        Deadline::after(fakes::deadline::WAITS).recv_or_fail(&finished, "session closed");
    }
}

fn served() -> support::Opened {
    support::Opened::open(vec![])
}

fn message_on(
    home: &std::path::Path,
    clock: &Arc<FakeClock>,
    to: &contract::SessionId,
    from: &contract::SessionId,
    text: &str,
) -> Receiver<Result<(), contract::sessions::MessageError>> {
    let client = SessionSockets::new(home, clock.clone());
    let (to, from, text) = (to.clone(), from.clone(), text.to_owned());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(client.message(&to, &from, None, &text, &CancelToken::new()))
            .unwrap();
    });
    rx
}

fn delivered(rx: &Receiver<Result<(), contract::sessions::MessageError>>) {
    Deadline::after(fakes::deadline::WAITS)
        .recv(rx)
        .expect("the message answered")
        .expect("the message delivered");
}

#[test]
fn a_served_session_takes_the_message_into_its_inbox() {
    let opened = served();
    let from = contract::SessionId(mint("s_"));
    let answered = message_on(
        &opened.home,
        &opened.clock,
        &opened.id,
        &from,
        "the parser is fixed",
    );
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |inbox| {
            let Delivery::Message(message, ack) = Deadline::after(fakes::deadline::WAITS)
                .recv(&inbox)
                .expect("the message is delivered")
            else {
                panic!("a session message is delivered");
            };
            assert_eq!(message.from_session_id, from);
            assert_eq!(message.text, "the parser is fixed");
            // The sender's answer waits for the take's ack.
            ack.0(Ok(None));
            delivered(&answered);
            Ok(())
        })
        .unwrap();
    support::close_within(opened.session, opened.log);
}

#[test]
fn a_send_leaves_clients_and_the_status_count_unchanged() {
    let opened = served();
    let socket = opened.socket.clone();
    let from = contract::SessionId(mint("s_"));
    let target = opened.id.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |inbox| {
            let full = Client::connect(&socket).unwrap();
            support::subscribe(&full, "c_full", "full");
            // One full connection: the first `clients` line says 1.
            let attached = support::until(&full, |line| support::kind(line) == "clients");
            assert!(
                attached.iter().any(|line| line["payload"]["count"] == 1),
                "one full connection counts: {attached:?}"
            );
            let answered = message_on(
                &opened.home,
                &opened.clock,
                &target,
                &from,
                "the parser is fixed",
            );
            // A probe command's answer bounds the wait: nothing between the
            // send and the answer is a `clients` line, so the send is never
            // counted in `clients`, and the last status still says 1.
            support::send(&full, r#"{"id":"c_probe","command":"nope"}"#);
            let probed = support::until(&full, |line| support::command_id(line) == Some("c_probe"));
            assert!(
                probed.iter().all(|line| support::kind(line) != "clients"),
                "the send emits no clients line: {probed:?}"
            );
            // The inbox holds the message; the loop is this closure, so it
            // is taken and answered here, and only then is the sender
            // answered.
            let Delivery::Message(_, ack) = Deadline::after(fakes::deadline::WAITS)
                .recv(&inbox)
                .expect("the message is delivered")
            else {
                panic!("a session message is delivered");
            };
            ack.0(Ok(None));
            delivered(&answered);
            Ok(())
        })
        .unwrap();
    support::close_within(opened.session, opened.log);
}

#[test]
fn a_send_after_close_is_rejected_closing() {
    let opened = served();
    let socket = opened.socket.clone();
    let from = contract::SessionId(mint("s_"));
    let target = opened.id.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |inbox| {
            let closer = Client::connect(&socket).unwrap();
            support::subscribe(&closer, "c_sum", "summary");
            support::send(&closer, r#"{"id":"c_close","command":"close"}"#);
            // The close is answered only once the inbox takes it, so the
            // take proves `mark_closed` ran before the send below.
            let Delivery::Close(_) = Deadline::after(fakes::deadline::WAITS)
                .recv(&inbox)
                .expect("the close is delivered")
            else {
                panic!("a close is delivered");
            };
            let answered = message_on(&opened.home, &opened.clock, &target, &from, "too late");
            match Deadline::after(fakes::deadline::WAITS)
                .recv(&answered)
                .expect("the send answered")
            {
                Err(contract::sessions::MessageError::Rejected(failure)) => {
                    assert_eq!(failure.code, ErrorCode::Closing);
                    assert_eq!(
                        failure.message,
                        "The session is closing and takes no session message."
                    );
                }
                other => panic!("a send after close is rejected closing: {other:?}"),
            }
            Ok(())
        })
        .unwrap();
    support::close_within(opened.session, opened.log);
}

#[test]
fn a_fiber_ask_session_is_closing_from_its_start() {
    let opened = served();
    let from = contract::SessionId(mint("s_"));
    let target = opened.id.clone();
    opened
        .session
        .ask("hi".to_owned(), Arc::new(|| false), |_inbox| {
            let answered = message_on(&opened.home, &opened.clock, &target, &from, "too late");
            match Deadline::after(fakes::deadline::WAITS)
                .recv(&answered)
                .expect("the send answered")
            {
                Err(contract::sessions::MessageError::Rejected(failure)) => {
                    assert_eq!(failure.code, ErrorCode::Closing);
                    assert_eq!(
                        failure.message,
                        "The session is closing and takes no session message."
                    );
                }
                other => panic!("a send to a fiber ask target is rejected closing: {other:?}"),
            }
            Ok(())
        })
        .unwrap();
    support::close_within(opened.session, opened.log);
}
