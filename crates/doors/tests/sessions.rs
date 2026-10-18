//! The running-session client against real session sockets.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures and assertions"
)]
mod support;

use contract::clock::Clock;
use contract::events::{Event, SessionState, SessionStatus};
use contract::sessions::{Running, Sessions};
use contract::shapes::{Tokens, Usage};
use doors::{Session, SessionSockets, mint};
use fakes::CancelToken;
use fakes::Deadline;
use fakes::clock::FakeClock;
use log::Log;
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
