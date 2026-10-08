//! Tests for the first prompt's wait: unarmed, armed while the waiter
//! checks, and the entry its bound removes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use contract::CommandId;
use serde_json::json;

use super::*;
use crate::diag::Diag;
use crate::fake::{FakeStarter, Handshake};
use crate::start::Outcome;

/// One named deadline per wait.
const DEADLINE: Duration = Duration::from_secs(10);

struct Setup {
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
    dir: PathBuf,
    clock: Arc<fakes::clock::FakeClock>,
    starter: FakeStarter,
    hub: Arc<Hub>,
    relays: Arc<Mutex<Relays>>,
}

fn setup() -> Setup {
    let held = fakes::TempDir::new("hf");
    let dir = held.path().join("h");
    fs::create_dir_all(dir.join("w")).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let starter = FakeStarter::with_handshake(
        &dir,
        Handshake {
            accept: true,
            code: String::new(),
            message: String::new(),
        },
    );
    let timed: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
    let hub = Arc::new(Hub::new(
        &dir,
        "0.0.0",
        Arc::new(starter.clone()),
        Arc::clone(&timed),
        Diag::open(&dir, timed),
    ));
    Setup {
        held,
        dir,
        clock,
        starter,
        hub,
        relays: Arc::default(),
    }
}

/// Runs `start` with content on a thread and returns its session id and
/// held prompt.
fn held(setup: &Setup) -> (String, Held) {
    let hub = Arc::clone(&setup.hub);
    let workspace = setup.dir.join("w").to_string_lossy().into_owned();
    let (done_tx, done) = mpsc::channel();
    thread::spawn(move || {
        let content = json!([{"type": "text", "text": "hi"}]);
        let outcome = start::run(
            &hub,
            &CommandId("c_start".to_owned()),
            &workspace,
            None,
            false,
            Some(&content),
        );
        done_tx.send(outcome).unwrap_or(());
    });
    let Outcome::Accepted {
        session_id,
        first: Some(held),
    } = done.recv_timeout(DEADLINE).expect("start answered")
    else {
        panic!("the start is accepted with its prompt held");
    };
    (session_id.0, *held)
}

fn prompts(starter: &FakeStarter) -> usize {
    starter
        .received()
        .iter()
        .filter(|line| line.contains("\"prompt\""))
        .count()
}

#[test]
fn an_unarmed_wait_never_reaches_a_deadline() {
    let setup = setup();
    let (_, held) = held(&setup);
    let first = First::new(Arc::clone(&setup.hub.tick));
    later(&setup.hub, &setup.relays, held, Arc::clone(&first)).unwrap();
    assert!(setup.clock.await_parked_unbounded(DEADLINE));
    let mark = setup.clock.advance_marked(Duration::from_secs(3_600));
    assert!(
        setup.clock.await_parked_since(&mark, None, DEADLINE),
        "it re-checked and still has no deadline"
    );
    assert_eq!(prompts(&setup.starter), 0);
    let due = setup.clock.now() + FIRST_PROMPT_WAIT;
    first.arm(due);
    assert!(setup.clock.await_parked(due, DEADLINE));
    setup.clock.advance(FIRST_PROMPT_WAIT);
    assert!(setup.starter.await_received(2, DEADLINE));
    assert_eq!(prompts(&setup.starter), 1);
}

#[test]
fn arm_while_the_waiter_checks_does_not_deadlock() {
    let setup = setup();
    let (_, held) = held(&setup);
    let first = First::new(Arc::clone(&setup.hub.tick));
    let (checking_tx, checking) = mpsc::channel();
    let (armed_tx, armed) = mpsc::channel::<()>();
    *lock(&first.before_check) = Some(Box::new(move || {
        checking_tx.send(()).unwrap_or(());
        armed
            .recv_timeout(DEADLINE)
            .expect("arm dropped the deadline guard");
    }));
    *lock(&first.armed) = Some(Box::new(move || armed_tx.send(()).unwrap_or(())));
    later(&setup.hub, &setup.relays, held, Arc::clone(&first)).unwrap();
    checking
        .recv_timeout(DEADLINE)
        .expect("the waiter is in its check");
    let due = setup.clock.now() + FIRST_PROMPT_WAIT;
    let (done_tx, done) = mpsc::channel();
    let arming = Arc::clone(&first);
    thread::spawn(move || {
        arming.arm(due);
        done_tx.send(()).unwrap_or(());
    });
    assert!(
        setup.clock.await_parked(due, DEADLINE),
        "the waiter read the deadline"
    );
    done.recv_timeout(DEADLINE).expect("arm returned");
    setup.clock.advance(FIRST_PROMPT_WAIT);
    assert!(setup.starter.await_received(2, DEADLINE));
}

#[test]
fn the_bound_removes_its_awaiting_entry() {
    let setup = setup();
    for n in 1..=2 {
        let (session, held) = held(&setup);
        let first = First::new(Arc::clone(&setup.hub.tick));
        lock(&setup.relays)
            .awaiting
            .push((session, Arc::clone(&first)));
        later(&setup.hub, &setup.relays, held, Arc::clone(&first)).unwrap();
        let due = setup.clock.now() + FIRST_PROMPT_WAIT;
        first.arm(due);
        assert!(setup.clock.await_parked(due, DEADLINE));
        setup.clock.advance(FIRST_PROMPT_WAIT);
        assert!(setup.starter.await_received(2 * n, DEADLINE));
        assert_eq!(prompts(&setup.starter), n);
        assert!(lock(&setup.relays).awaiting.is_empty(), "start {n}");
    }
}
