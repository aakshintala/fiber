//! Runs and resume: one entry per delegate, one run at a time, and every
//! run's news settled in event order.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use fakes::MUST_SUCCEED_WITHIN;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::ActionId;
use contract::clock::Clock as _;
use contract::inbox::Delivery;
use contract::jobs::{JobRecord, Jobs, Stop, Taken};
use contract::tool::Settle;
use fakes::Deadline;
use fakes::clock::FakeClock;
use fakes::{CancelToken, TempDir};

use super::{Mark, Resume, Resumed, Target, Waited};
use crate::registry::{Registry, Unreported};
use crate::support::{
    delegate_finished, delegate_started, ended_ok, opening, registry_with, world,
};

const OUTPUT: &str = "/sessions/s_x/events.jsonl";

#[test]
fn admissions_count_only_running_delegates_and_reuse_ended_slots() {
    for count in [9, 10, 11] {
        let (_dir, registry) = world();
        let mut finishes = Vec::new();
        for _ in 0..count.min(10) {
            finishes.push(open_delegate(&registry));
        }
        let mut shells = Vec::new();
        for _ in 0..5 {
            shells.push(registry.open(opening("shell")).unwrap());
        }
        let called = AtomicBool::new(false);
        let answer = registry.admit(false, || {
            called.store(true, Ordering::SeqCst);
            Ok::<_, ()>(open_delegate(&registry))
        });
        assert_eq!(answer.is_ok(), count < 10);
        assert_eq!(called.load(Ordering::SeqCst), count < 10);
        if count >= 10 {
            assert!(matches!(answer, Err(super::Admit::Full)));
            let (id, finish) = finishes.pop().unwrap();
            end_run(finish, &id, "done");
            assert!(
                registry
                    .admit(false, || Ok::<_, ()>(open_delegate(&registry)))
                    .is_ok()
            );
        }
    }
    let (_dir, registry) = world();
    let _running: Vec<_> = (0..9).map(|_| open_delegate(&registry)).collect();
    assert!(matches!(
        registry.admit(false, || Err::<(), _>("spawn failed")),
        Err(super::Admit::Start("spawn failed"))
    ));
    assert!(
        registry
            .admit(false, || Ok::<_, ()>(open_delegate(&registry)))
            .is_ok()
    );
}

#[test]
fn resume_counts_as_a_start_but_a_live_steer_does_not() {
    for count in [9, 10] {
        let (_dir, registry) = world();
        let (id, finish) = open_delegate(&registry);
        end_run(finish, &id, "done");
        let running: Vec<_> = (0..count).map(|_| open_delegate(&registry)).collect();
        let called = AtomicBool::new(false);
        let answer = registry.resume(&id.0, None, |_| {
            called.store(true, Ordering::SeqCst);
            Ok::<_, ()>((Stop(Box::new(|| {})), ()))
        });
        assert_eq!(called.load(Ordering::SeqCst), count == 9);
        assert_eq!(answer.is_ok(), count == 9);
        if count == 10 {
            assert!(matches!(answer, Err(Resume::Full)));
        }
        assert!(matches!(
            registry.resume(&running[0].0.0, None, |_| Err::<(Stop, ()), _>(
                "must not start"
            )),
            Err(Resume::Live(_, _))
        ));
    }
}

#[test]
fn concurrent_admissions_hold_the_gate_through_recording() {
    let (_dir, registry) = world();
    let _running: Vec<_> = (0..9).map(|_| open_delegate(&registry)).collect();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (a_tx, a_rx) = mpsc::channel();
    let a = Arc::clone(&registry);
    thread::spawn(move || {
        let answer = a.admit(false, || {
            entered_tx.send(()).unwrap();
            let _released = release_rx.recv();
            Ok::<_, ()>(open_delegate(&a))
        });
        a_tx.send(answer).unwrap();
    });
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    deadline
        .recv(&entered_rx)
        .expect("first start holds the gate");
    assert!(registry.start_gate.try_lock().is_err());
    let (b_tx, b_rx) = mpsc::channel();
    let b = Arc::clone(&registry);
    thread::spawn(move || {
        b_tx.send(b.admit(false, || Ok::<_, ()>(open_delegate(&b))))
            .unwrap();
    });
    assert!(
        Deadline::after(Duration::from_millis(100))
            .recv(&b_rx)
            .is_err()
    );
    release_tx.send(()).unwrap();
    let first = deadline.recv(&a_rx).expect("first start returned");
    assert!(first.is_ok());
    assert!(matches!(
        deadline.recv(&b_rx).expect("second start returned"),
        Err(super::Admit::Full)
    ));
}

fn action() -> ActionId {
    ActionId("a_resume00000001".into())
}

fn open_delegate(registry: &Arc<Registry>) -> (contract::JobId, Unreported) {
    let id = crate::registry::mint_job_id();
    let (_started, finish) = registry.open_started(
        id.clone(),
        "delegate_spawn".into(),
        "scan".into(),
        OUTPUT.into(),
        Stop(Box::new(|| {})),
        delegate_started(&id.0),
    );
    (id, finish)
}

fn end_run(finish: Unreported, id: &contract::JobId, text: &str) {
    finish.report(ended_ok(&id.0), Some(delegate_finished(&id.0, text)));
}

fn df(id: &contract::JobId, text: &str) -> Taken {
    Taken {
        record: JobRecord::DelegateFinished(delegate_finished(&id.0, text)),
        action: None,
    }
}

fn completed(id: &contract::JobId) -> Taken {
    Taken {
        record: JobRecord::Completed(ended_ok(&id.0)),
        action: None,
    }
}

fn started(id: &contract::JobId, action: Option<ActionId>) -> Taken {
    Taken {
        record: JobRecord::Started(contract::events::JobStarted {
            job_id: id.clone(),
            tool: Some("delegate_message".into()),
            extension: None,
            description: "scan".into(),
            output_path: OUTPUT.into(),
        }),
        action,
    }
}

fn delegate_started_taken(id: &contract::JobId, action: Option<ActionId>) -> Taken {
    Taken {
        record: JobRecord::DelegateStarted(delegate_started(&id.0)),
        action,
    }
}

fn call(settle: &Settle) -> Vec<Taken> {
    (settle.0)()
}

fn take_notice(rx: &mpsc::Receiver<Delivery>) -> contract::inbox::JobNotice {
    let Delivery::Job(notice) = rx.try_recv().expect("the end sent a notice") else {
        panic!("the end sent nothing job-like");
    };
    notice
}

#[track_caller]
fn resume_ok(
    registry: &Arc<Registry>,
    id: &contract::JobId,
    action: Option<ActionId>,
) -> Resumed<()> {
    match registry.resume(&id.0, action, |_| {
        Ok::<_, String>((Stop(Box::new(|| {})), ()))
    }) {
        Ok(resumed) => resumed,
        Err(_) => panic!("the resume committed"),
    }
}

#[test]
fn a_resume_queues_the_new_run_after_the_ended_run_news() {
    let (_dir, registry) = world();
    let (id, finish) = open_delegate(&registry);
    end_run(finish, &id, "Done.");
    let a = action();
    let resumed = resume_ok(&registry, &id, Some(a.clone()));
    let taken = call(&resumed.settle);
    assert_eq!(
        taken,
        vec![
            df(&id, "Done."),
            completed(&id),
            started(&id, Some(a.clone())),
            delegate_started_taken(&id, Some(a.clone())),
        ]
    );
    // A resumed run's `Started` names the resuming call and keeps the
    // job; its `DelegateStarted` copies the first run's.
    let [_, _, started, ds_taken] = &taken[..] else {
        panic!("the settle is spent");
    };
    assert!(matches!(
        &started.record,
        JobRecord::Started(line) if line.tool.as_deref() == Some("delegate_message")
            && line.description == "scan"
            && line.output_path == OUTPUT
    ));
    assert!(matches!(
        &ds_taken.record,
        JobRecord::DelegateStarted(line) if line == &delegate_started(&id.0)
    ));
    // One id for life, and the new run runs.
    assert_eq!(registry.list_text().matches(&id.0).count(), 1);
    assert_eq!(Jobs::running(registry.as_ref()), vec![id.clone()]);
    assert!(registry.running(&id.0, 2));
    assert!(!registry.running(&id.0, 1));
}

#[test]
fn a_resume_after_a_wait_takes_only_the_new_run_start() {
    let (_dir, registry) = world();
    let (id, finish) = open_delegate(&registry);
    end_run(finish, &id, "Done.");
    let waited = registry
        .wait(&id.0, 0, &CancelToken::new())
        .expect("run 1 ended");
    assert_eq!(
        call(&waited.settle.expect("an ended wait settles")),
        vec![df(&id, "Done."), completed(&id)]
    );
    let a = action();
    let resumed = resume_ok(&registry, &id, Some(a.clone()));
    assert_eq!(
        call(&resumed.settle),
        vec![
            started(&id, Some(a.clone())),
            delegate_started_taken(&id, Some(a.clone())),
        ]
    );
}

#[test]
fn a_resume_without_an_ask_queues_starts_with_no_action() {
    let (_dir, registry) = world();
    let (id, finish) = open_delegate(&registry);
    end_run(finish, &id, "Done.");
    let resumed = resume_ok(&registry, &id, None);
    assert_eq!(
        call(&resumed.settle),
        vec![
            df(&id, "Done."),
            completed(&id),
            started(&id, None),
            delegate_started_taken(&id, None),
        ]
    );
}

#[test]
fn a_failed_start_leaves_the_job_ended_with_its_news_queued() {
    let (_dir, registry) = world();
    let (id, finish) = open_delegate(&registry);
    end_run(finish, &id, "Done.");
    let Err(Resume::Start(message)) = registry.resume(&id.0, Some(action()), |_| {
        Err::<(Stop, ()), _>("boom".to_owned())
    }) else {
        panic!("the start failed");
    };
    assert_eq!(message, "boom");
    assert!(matches!(registry.target(&id.0), Target::Ended));
    let waited = registry
        .wait(&id.0, 0, &CancelToken::new())
        .expect("run 1 ended");
    assert_eq!(
        call(&waited.settle.expect("an ended wait settles")),
        vec![df(&id, "Done."), completed(&id)]
    );
    let _resumed = resume_ok(&registry, &id, Some(action()));
    assert!(registry.running(&id.0, 2));
}

#[test]
fn a_notice_claim_fails_once_the_news_left_and_holds_otherwise() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let (id, finish) = open_delegate(&registry);
    end_run(finish, &id, "Done.");
    let a = action();
    let resumed = resume_ok(&registry, &id, Some(a.clone()));
    // Nothing took run 1's news: the claim holds and removes it, leaving
    // run 2's start queued.
    let run1 = take_notice(&rx);
    assert_eq!((run1.claim.0)(), Some(Vec::new()));
    assert_eq!(
        call(&resumed.settle),
        vec![
            started(&id, Some(a.clone())),
            delegate_started_taken(&id, Some(a)),
        ]
    );
}

#[test]
fn a_resume_settle_takes_news_a_wait_already_took_as_nothing() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let (id, finish) = open_delegate(&registry);
    end_run(finish, &id, "Done.");
    let waited = registry
        .wait(&id.0, 0, &CancelToken::new())
        .expect("run 1 ended");
    assert_eq!(
        call(&waited.settle.expect("settles")),
        vec![df(&id, "Done."), completed(&id)]
    );
    let run1 = take_notice(&rx);
    assert_eq!((run1.claim.0)(), None);
}

#[test]
fn a_late_unreported_of_a_superseded_run_records_nothing() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let (id, finish1) = open_delegate(&registry);
    end_run(finish1, &id, "Done.");
    let a = action();
    let resumed = resume_ok(&registry, &id, Some(a.clone()));
    // Run 1's end reporter, dropped after the resume committed: it
    // records nothing on run 2, and sends no notice.
    drop(Unreported {
        job_id: id.clone(),
        run: 1,
        registry: Some(Arc::clone(&registry)),
        delegate: true,
    });
    assert!(registry.running(&id.0, 2), "run 2 still runs");
    assert_eq!(
        call(&resumed.settle),
        vec![
            df(&id, "Done."),
            completed(&id),
            started(&id, Some(a.clone())),
            delegate_started_taken(&id, Some(a)),
        ]
    );
    let run1 = take_notice(&rx);
    assert_eq!((run1.claim.0)(), None);
    assert!(rx.try_recv().is_err(), "a late report sends no notice");
}

#[test]
fn run_2_finish_queues_its_news_behind_the_taken_start() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let (id, finish1) = open_delegate(&registry);
    end_run(finish1, &id, "Done.");
    let a = action();
    let resumed = resume_ok(&registry, &id, Some(a.clone()));
    assert_eq!(
        call(&resumed.settle),
        vec![
            df(&id, "Done."),
            completed(&id),
            started(&id, Some(a.clone())),
            delegate_started_taken(&id, Some(a)),
        ]
    );
    end_run(resumed.unreported, &id, "Done again.");
    let _run1 = take_notice(&rx);
    let run2 = take_notice(&rx);
    assert_eq!((run2.claim.0)(), Some(Vec::new()));
    let waited = registry
        .wait(&id.0, 0, &CancelToken::new())
        .expect("run 2 ended");
    assert!(call(&waited.settle.expect("settles")).is_empty());
}

#[test]
fn target_names_what_the_id_names() {
    let (_dir, registry) = world();
    assert!(matches!(registry.target("j_missing"), Target::Unknown));
    let opened = registry.open(opening("npm test")).unwrap();
    assert!(matches!(
        registry.target(&opened.started.job_id.0),
        Target::NotDelegate
    ));
    drop(opened.end);
    let (id, finish) = open_delegate(&registry);
    let Target::Live(session, 1) = registry.target(&id.0) else {
        panic!("a running delegate is live");
    };
    assert_eq!(session.0, "s_d000000000000001");
    end_run(finish, &id, "Done.");
    assert!(matches!(registry.target(&id.0), Target::Ended));
}

#[test]
fn await_end_answers_when_the_run_stops_running() {
    let clock = FakeClock::new();
    let (_dir, registry) = registry_with(Arc::clone(&clock) as Arc<dyn contract::clock::Clock>);
    let (id, finish) = open_delegate(&registry);
    let until = clock.now() + Duration::from_secs(60);
    end_run(finish, &id, "Done.");
    assert!(matches!(
        registry.await_end(&id.0, 1, until, &CancelToken::new()),
        Waited::Ended
    ));
    // A resume already running counts as run 1 no longer running.
    let (id, finish) = open_delegate(&registry);
    end_run(finish, &id, "Done.");
    let _held = resume_ok(&registry, &id, Some(action()));
    assert!(matches!(
        registry.await_end(&id.0, 1, until, &CancelToken::new()),
        Waited::Ended
    ));
    // A running run times out and answers to a cancelled wait.
    let (id, _running) = open_delegate(&registry);
    assert!(matches!(
        registry.await_end(&id.0, 1, clock.now(), &CancelToken::new()),
        Waited::Timeout
    ));
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(matches!(
        registry.await_end(&id.0, 1, until, &cancel),
        Waited::Cancelled
    ));
}

#[test]
fn stop_delegates_stops_a_resumed_run() {
    let (_dir, registry) = world();
    let id = crate::registry::mint_job_id();
    let fired = Arc::new(AtomicBool::new(false));
    let stop = {
        let fired = Arc::clone(&fired);
        Stop(Box::new(move || fired.store(true, Ordering::SeqCst)))
    };
    let (_started, finish) = registry.open_started(
        id.clone(),
        "delegate_spawn".into(),
        "scan".into(),
        OUTPUT.into(),
        stop,
        delegate_started(&id.0),
    );
    end_run(finish, &id, "Done.");
    let fired2 = Arc::new(AtomicBool::new(false));
    let stop2 = {
        let fired2 = Arc::clone(&fired2);
        Stop(Box::new(move || fired2.store(true, Ordering::SeqCst)))
    };
    match registry.resume(&id.0, Some(action()), |_| Ok::<_, String>((stop2, ()))) {
        Ok(held) => {
            assert_eq!(registry.stop_delegates(), 1);
            assert!(fired2.load(Ordering::SeqCst), "the resumed run stops");
            assert!(!fired.load(Ordering::SeqCst), "the old stop stays");
            drop(held);
        }
        Err(_) => panic!("the resume committed"),
    }
}

#[test]
fn a_wait_parked_on_run_1_answers_run_1_after_a_resume() {
    let (_dir, registry) = world();
    let (id, finish1) = open_delegate(&registry);
    let a = action();
    let held: Arc<Mutex<Option<Unreported>>> = Arc::new(Mutex::new(None));
    let waiter = Arc::clone(&registry);
    let wid = id.clone();
    let hook_id = wid.clone();
    let hook_waiter = Arc::clone(&waiter);
    let (answer_tx, answer_rx) = mpsc::channel();
    let (settle_tx, settle_rx) = mpsc::channel::<Settle>();
    let moved_held = Arc::clone(&held);
    let handle = thread::spawn(move || {
        super::super::park::BEFORE_PARK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                // Between the waiter's check and its park, with no
                // registry lock held: run 1 ends and resumes to run 2,
                // as another call would.
                end_run(finish1, &hook_id, "Done.");
                let resumed = resume_ok(&hook_waiter, &hook_id, Some(action()));
                let _sent = settle_tx.send(resumed.settle);
                *moved_held.lock().unwrap() = Some(resumed.unreported);
            }));
        });
        let answer = waiter.wait(&wid.0, 60_000, &CancelToken::new()).unwrap();
        let _sent = answer_tx.send(answer);
    });
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    let answer = wait.recv(&answer_rx).expect("the wait answered run 1");
    assert!(
        answer.text.contains("Final message:\nDone.\n"),
        "{}",
        answer.text
    );
    assert_eq!(
        call(&answer.settle.expect("an ended wait settles")),
        vec![df(&id, "Done."), completed(&id)]
    );
    // The resume's settle, called after, takes only run 2's start.
    let resumed_settle = wait.recv(&settle_rx).expect("the resume settled");
    assert_eq!(
        call(&resumed_settle),
        vec![
            started(&id, Some(a.clone())),
            delegate_started_taken(&id, Some(a)),
        ]
    );
    assert!(registry.running(&id.0, 2), "run 2 still runs");
    handle.join().unwrap();
    assert!(held.lock().unwrap().is_some(), "run 2 is held");
}

#[test]
fn a_stop_on_run_1_stops_run_1_and_answers_it_after_a_resume() {
    let (_dir, registry) = world();
    let id = crate::registry::mint_job_id();
    let stop1 = Arc::new(AtomicBool::new(false));
    let stop = {
        let stop1 = Arc::clone(&stop1);
        Stop(Box::new(move || stop1.store(true, Ordering::SeqCst)))
    };
    let (_started, finish1) = registry.open_started(
        id.clone(),
        "delegate_spawn".into(),
        "scan".into(),
        OUTPUT.into(),
        stop,
        delegate_started(&id.0),
    );
    let a = action();
    let stop2 = Arc::new(AtomicBool::new(false));
    let (stop2_tx, stop2_rx) = mpsc::channel::<()>();
    let held: Arc<Mutex<Option<Unreported>>> = Arc::new(Mutex::new(None));
    let waiter = Arc::clone(&registry);
    let wid = id.clone();
    let hook_id = wid.clone();
    let hook_waiter = Arc::clone(&waiter);
    let (answer_tx, answer_rx) = mpsc::channel();
    let moved_held = Arc::clone(&held);
    let moved_stop2 = Arc::clone(&stop2);
    let handle = thread::spawn(move || {
        super::super::park::BEFORE_PARK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                end_run(finish1, &hook_id, "Done.");
                let stop = {
                    let moved_stop2 = Arc::clone(&moved_stop2);
                    let stop2_tx = stop2_tx.clone();
                    Stop(Box::new(move || {
                        moved_stop2.store(true, Ordering::SeqCst);
                        let _sent = stop2_tx.send(());
                    }))
                };
                match hook_waiter
                    .resume(&hook_id.0, Some(action()), |_| Ok::<_, String>((stop, ())))
                {
                    Ok(resumed) => {
                        *moved_held.lock().unwrap() = Some(resumed.unreported);
                    }
                    Err(_) => panic!("the resume committed"),
                }
            }));
        });
        let answer = waiter.stop(&wid.0, &CancelToken::new()).unwrap();
        let _sent = answer_tx.send(answer);
    });
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    let answer = wait.recv(&answer_rx).expect("the stop answered run 1");
    assert!(answer.text.contains("completed"), "{}", answer.text);
    assert!(stop1.load(Ordering::SeqCst), "run 1's stop ran");
    assert!(
        !stop2.load(Ordering::SeqCst),
        "run 2's stop never ran for run 1"
    );
    assert_eq!(
        call(&answer.settle.expect("an ended stop settles")),
        vec![df(&id, "Done."), completed(&id)]
    );
    handle.join().unwrap();
    // A later stop on run 2 runs run 2's stop once: the fired stop
    // proves it was sent while run 2 ran, so the stop parks instead of
    // racing the end.
    let stopper = Arc::clone(&registry);
    let stop_id = id.clone();
    let (run2_tx, run2_rx) = mpsc::channel();
    let stopping = thread::spawn(move || {
        let answer = stopper.stop(&stop_id.0, &CancelToken::new()).unwrap();
        let _sent = run2_tx.send(answer);
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&stop2_rx)
        .expect("run 2's stop was sent");
    end_run(
        held.lock().unwrap().take().expect("run 2 is held"),
        &id,
        "Done again.",
    );
    stopping.join().unwrap();
    let answer = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&run2_rx)
        .expect("the stop answered run 2");
    assert!(stop2.load(Ordering::SeqCst), "run 2's stop ran once");
    assert_eq!(
        call(&answer.settle.expect("settles")),
        vec![
            started(&id, Some(a.clone())),
            delegate_started_taken(&id, Some(a.clone())),
            df(&id, "Done again."),
            completed(&id),
        ]
    );
}

#[test]
fn a_wait_on_a_running_run_parks_then_answers_still_running() {
    let clock = FakeClock::new();
    let (_dir, registry) = registry_with(Arc::clone(&clock) as Arc<dyn contract::clock::Clock>);
    let (_id, _running) = open_delegate(&registry);
    let id = _id.clone();
    let (answer_tx, answer_rx) = mpsc::channel();
    thread::spawn(move || {
        let answer = registry.wait(&id.0, 1_000, &CancelToken::new()).unwrap();
        let _sent = answer_tx.send(answer);
    });
    let until = clock.now() + Duration::from_millis(1_000);
    clock.advance_when_parked(
        Some(until),
        None,
        Duration::from_millis(1_000),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    let answer = wait.recv(&answer_rx).expect("the wait timed out");
    assert!(answer.text.contains("still running"), "{}", answer.text);
    assert!(answer.settle.is_none());
}

#[test]
fn the_run_number_is_never_reused() {
    let (_dir, registry) = world();
    let (id, finish) = open_delegate(&registry);
    end_run(finish, &id, "Done.");
    {
        let mut inner = super::super::lock(&registry.inner);
        inner
            .jobs
            .iter_mut()
            .find(|job| job.started.job_id == id)
            .unwrap()
            .run = u64::MAX - 1;
    }
    let a = action();
    let resumed = resume_ok(&registry, &id, Some(a.clone()));
    assert_eq!(registry.run_of(&id.0), Some(u64::MAX));
    // Run `u64::MAX - 1` never ran, so its stale claim fails and takes
    // nothing.
    assert_eq!(registry.claim_through(&id.0, u64::MAX - 1), None);
    assert_eq!(
        call(&resumed.settle),
        vec![
            df(&id, "Done."),
            completed(&id),
            started(&id, Some(a.clone())),
            delegate_started_taken(&id, Some(a)),
        ]
    );
    // Run `u64::MAX` ends; the next resume is exhausted without a start.
    end_run(resumed.unreported, &id, "Done MAX.");
    let called = Arc::new(AtomicBool::new(false));
    let moved = Arc::clone(&called);
    match registry.resume(&id.0, Some(action()), |_| {
        moved.store(true, Ordering::SeqCst);
        Ok::<_, String>((Stop(Box::new(|| {})), ()))
    }) {
        Err(Resume::Exhausted) => {}
        _ => panic!("the run count is exhausted"),
    }
    assert!(!called.load(Ordering::SeqCst), "no start ran");
    assert!(matches!(registry.target(&id.0), Target::Ended));
    assert_eq!(
        registry.take_through(&id.0, Mark::end(u64::MAX)),
        vec![df(&id, "Done MAX."), completed(&id)]
    );
}

#[test]
fn two_resumes_race_through_the_gate_and_start_one_run() {
    let (_dir, registry) = world();
    let (id, finish) = open_delegate(&registry);
    end_run(finish, &id, "Done.");
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (attempting_tx, attempting_rx) = mpsc::channel::<()>();
    let (called_tx, called_rx) = mpsc::channel::<()>();
    let first = Arc::clone(&registry);
    let first_id = id.clone();
    let held: Arc<Mutex<Option<Unreported>>> = Arc::new(Mutex::new(None));
    let moved_held = Arc::clone(&held);
    let handle1 = thread::spawn(move || {
        first.resume(&first_id.0, Some(action()), |_| {
            let _sent = started_tx.send(());
            release_rx.recv().unwrap();
            Ok::<_, String>((Stop(Box::new(|| {})), ()))
        })
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&started_rx)
        .expect("the first resume holds the gate in start");
    let second = Arc::clone(&registry);
    let second_id = id.clone();
    let handle2 = thread::spawn(move || {
        let _sent = attempting_tx.send(());
        second.resume(&second_id.0, Some(action()), |_| {
            let _sent = called_tx.send(());
            Ok::<_, String>((Stop(Box::new(|| {})), ()))
        })
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&attempting_rx)
        .expect("the second resume started");
    // The first resume holds the gate in `start`: the gate is still
    // held here, so the second resume cannot have passed it. A state
    // check, not a wait: it holds however the threads are scheduled.
    assert!(
        registry.start_gate.try_lock().is_err(),
        "the first resume holds the gate in start"
    );
    assert!(called_rx.try_recv().is_err(), "the second start never ran");
    // Always released, so both joins below end whatever the assert says.
    release_tx.send(()).unwrap();
    match handle1.join().unwrap() {
        Ok(resumed) => {
            *moved_held.lock().unwrap() = Some(resumed.unreported);
        }
        Err(_) => panic!("the first resume committed"),
    }
    match handle2.join().unwrap() {
        Err(Resume::Live(_, 2)) => {}
        _ => panic!("the second resume saw run 2 live"),
    }
    assert!(called_rx.try_recv().is_err(), "no second start ran");
    assert!(registry.running(&id.0, 2));
}

#[test]
fn a_resume_whose_run_moved_during_start_answers_live_and_changes_nothing() {
    let (_dir, registry) = world();
    let (id, finish) = open_delegate(&registry);
    end_run(finish, &id, "Done.");
    let moved = Arc::clone(&registry);
    let moved_id = id.clone();
    match registry.resume(&id.0, Some(action()), |_| {
        // Another resume committed while `start` ran with the inner
        // lock released: the entry moved on without this resume.
        let mut inner = super::super::lock(&moved.inner);
        inner
            .jobs
            .iter_mut()
            .find(|job| job.started.job_id == moved_id)
            .expect("the job is still there")
            .run = 7;
        Ok::<_, String>((Stop(Box::new(|| {})), ()))
    }) {
        Err(Resume::Live(_, 7)) => {}
        _ => panic!("the stale resume answers live"),
    }
    assert_eq!(registry.run_of(&id.0), Some(7));
    assert!(matches!(registry.target(&id.0), Target::Ended));
}

#[test]
fn a_resume_whose_phase_left_ended_during_start_answers_live_and_changes_nothing() {
    let (_dir, registry) = world();
    let (id, finish) = open_delegate(&registry);
    end_run(finish, &id, "Done.");
    let moved = Arc::clone(&registry);
    let moved_id = id.clone();
    match registry.resume(&id.0, Some(action()), |_| {
        // The entry started running again while `start` ran with the
        // inner lock released: the recheck fails closed instead of
        // overwriting the running phase.
        let mut inner = super::super::lock(&moved.inner);
        inner
            .jobs
            .iter_mut()
            .find(|job| job.started.job_id == moved_id)
            .expect("the job is still there")
            .phase = super::super::Phase::Running;
        Ok::<_, String>((Stop(Box::new(|| {})), ()))
    }) {
        Err(Resume::Live(_, 1)) => {}
        _ => panic!("the racing resume answers live"),
    }
    assert!(registry.running(&id.0, 1));
}

/// Two runs in: run 1 ended, run 2 started by `a` and ended, run 3
/// started by `c`. Both settles held, both ends reported.
struct TwoRuns {
    _dir: TempDir,
    registry: Arc<Registry>,
    id: contract::JobId,
    a: ActionId,
    c: ActionId,
    a_settle: Settle,
    c_settle: Settle,
    inbox: mpsc::Receiver<Delivery>,
    /// Run 3's end reporter, held so run 3 stays running.
    _held: Unreported,
}

fn two_runs() -> TwoRuns {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let (id, finish1) = open_delegate(&registry);
    end_run(finish1, &id, "Done 1.");
    let a = action();
    let c = ActionId("a_resume00000002".into());
    let resumed_a = resume_ok(&registry, &id, Some(a.clone()));
    end_run(resumed_a.unreported, &id, "Done 2.");
    let resumed_c = resume_ok(&registry, &id, Some(c.clone()));
    TwoRuns {
        _dir,
        registry,
        id,
        a,
        c,
        a_settle: resumed_a.settle,
        c_settle: resumed_c.settle,
        inbox: rx,
        _held: resumed_c.unreported,
    }
}

#[test]
fn queue_takes_in_request_order_and_leaves_the_rest() {
    // (i) B answered run 1 before A's commit; A is written first.
    let (_dir, registry) = world();
    let (id, finish) = open_delegate(&registry);
    end_run(finish, &id, "Done.");
    let waited = registry
        .wait(&id.0, 0, &CancelToken::new())
        .expect("run 1 ended");
    let a = action();
    let resumed = resume_ok(&registry, &id, Some(a.clone()));
    assert_eq!(
        call(&resumed.settle),
        vec![
            df(&id, "Done."),
            completed(&id),
            started(&id, Some(a.clone())),
            delegate_started_taken(&id, Some(a)),
        ]
    );
    assert!(call(&waited.settle.expect("settles")).is_empty());
}

#[test]
fn queue_takes_a_later_run_after_an_earlier_settle() {
    // (ii) B answered run 2, written after A.
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let (id, finish1) = open_delegate(&registry);
    end_run(finish1, &id, "Done 1.");
    let a = action();
    let resumed_a = resume_ok(&registry, &id, Some(a.clone()));
    end_run(resumed_a.unreported, &id, "Done 2.");
    let waited = registry
        .wait(&id.0, 0, &CancelToken::new())
        .expect("run 2 ended");
    assert_eq!(
        call(&resumed_a.settle),
        vec![
            df(&id, "Done 1."),
            completed(&id),
            started(&id, Some(a.clone())),
            delegate_started_taken(&id, Some(a)),
        ]
    );
    assert_eq!(
        call(&waited.settle.expect("settles")),
        vec![df(&id, "Done 2."), completed(&id)]
    );
    assert_eq!((take_notice(&rx).claim.0)(), None);
}

#[test]
fn queue_takes_everything_before_a_later_mark_when_called_first() {
    // (iii) B's settle first takes all six; A's then takes nothing, and
    // run 2's notice claim takes nothing.
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let (id, finish1) = open_delegate(&registry);
    end_run(finish1, &id, "Done 1.");
    let a = action();
    let resumed_a = resume_ok(&registry, &id, Some(a.clone()));
    end_run(resumed_a.unreported, &id, "Done 2.");
    let waited = registry
        .wait(&id.0, 0, &CancelToken::new())
        .expect("run 2 ended");
    assert_eq!(
        call(&waited.settle.expect("settles")),
        vec![
            df(&id, "Done 1."),
            completed(&id),
            started(&id, Some(a.clone())),
            delegate_started_taken(&id, Some(a.clone())),
            df(&id, "Done 2."),
            completed(&id),
        ]
    );
    assert!(call(&resumed_a.settle).is_empty());
    assert_eq!((take_notice(&rx).claim.0)(), None);
}

#[test]
fn two_resumes_in_one_step_queue_both_starts() {
    // (iv) A resumes to run 2, run 2 ends, C resumes to run 3.
    let chain = two_runs();
    assert_eq!(
        call(&chain.a_settle),
        vec![
            df(&chain.id, "Done 1."),
            completed(&chain.id),
            started(&chain.id, Some(chain.a.clone())),
            delegate_started_taken(&chain.id, Some(chain.a.clone())),
        ]
    );
    assert_eq!(
        call(&chain.c_settle),
        vec![
            df(&chain.id, "Done 2."),
            completed(&chain.id),
            started(&chain.id, Some(chain.c.clone())),
            delegate_started_taken(&chain.id, Some(chain.c)),
        ]
    );
    // In the reverse order the later settle takes all eight.
    let chain = two_runs();
    assert_eq!(
        call(&chain.c_settle),
        vec![
            df(&chain.id, "Done 1."),
            completed(&chain.id),
            started(&chain.id, Some(chain.a.clone())),
            delegate_started_taken(&chain.id, Some(chain.a.clone())),
            df(&chain.id, "Done 2."),
            completed(&chain.id),
            started(&chain.id, Some(chain.c.clone())),
            delegate_started_taken(&chain.id, Some(chain.c)),
        ]
    );
    assert!(call(&chain.a_settle).is_empty());
    // With run 2 still live, a second resume is refused without a start.
    let (_dir, registry) = world();
    let (id, finish) = open_delegate(&registry);
    end_run(finish, &id, "Done.");
    let _held = resume_ok(&registry, &id, Some(action()));
    let called = Arc::new(AtomicBool::new(false));
    let moved = Arc::clone(&called);
    match registry.resume(&id.0, Some(action()), |_| {
        moved.store(true, Ordering::SeqCst);
        Ok::<_, String>((Stop(Box::new(|| {})), ()))
    }) {
        Err(Resume::Live(_, 2)) => {}
        _ => panic!("run 2 is live"),
    }
    assert!(!called.load(Ordering::SeqCst), "no start ran");
}

#[test]
fn a_notice_claim_after_a_later_settle_takes_nothing() {
    // (v) C's settle took run 2's news: run 2's claim takes nothing.
    let chain = two_runs();
    assert_eq!(call(&chain.c_settle).len(), 8);
    assert_eq!((take_notice(&chain.inbox).claim.0)(), None);
    assert_eq!((take_notice(&chain.inbox).claim.0)(), None);
    // C's settle took everything, run 3's start included.
    assert!(
        chain
            .registry
            .take_through(&chain.id.0, Mark::end(9))
            .is_empty()
    );
}

#[test]
fn a_notice_claim_before_the_later_settle_leaves_later_starts() {
    // (v) with C's settle uncalled: run 2's claim takes through run 2's
    // news, and run 3's start stays queued.
    let chain = two_runs();
    assert_eq!(call(&chain.a_settle).len(), 4);
    let _run1 = take_notice(&chain.inbox);
    assert_eq!((take_notice(&chain.inbox).claim.0)(), Some(Vec::new()));
    assert_eq!(
        chain.registry.take_through(&chain.id.0, Mark::end(3)),
        vec![
            started(&chain.id, Some(chain.c.clone())),
            delegate_started_taken(&chain.id, Some(chain.c)),
        ]
    );
}

#[test]
fn a_shell_job_queues_only_its_completion() {
    // (vi)
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(ended_ok(&id));
    let waited = registry
        .wait(&id, 0, &CancelToken::new())
        .expect("the shell job ended");
    assert_eq!(
        call(&waited.settle.expect("settles")),
        vec![completed(&contract::JobId(id.clone()))]
    );
    let again = registry
        .wait(&id, 0, &CancelToken::new())
        .expect("the shell job ended");
    assert!(call(&again.settle.expect("settles")).is_empty());
    assert_eq!((take_notice(&rx).claim.0)(), None);
}

#[test]
fn marks_order_start_before_end_at_each_boundary() {
    // (vii)
    let chain = two_runs();
    assert_eq!(
        chain.registry.take_through(&chain.id.0, Mark::end(1)),
        vec![df(&chain.id, "Done 1."), completed(&chain.id)]
    );
    assert_eq!(
        chain.registry.take_through(&chain.id.0, Mark::start(2)),
        vec![
            started(&chain.id, Some(chain.a.clone())),
            delegate_started_taken(&chain.id, Some(chain.a)),
        ]
    );
    assert_eq!(
        chain.registry.take_through(&chain.id.0, Mark::end(2)),
        vec![df(&chain.id, "Done 2."), completed(&chain.id)]
    );
    // Past the entry's run takes everything; an empty queue gives
    // nothing; a settle takes its records once.
    let chain = two_runs();
    assert_eq!(
        chain.registry.take_through(&chain.id.0, Mark::end(5)).len(),
        8
    );
    let (_dir, registry) = world();
    let (id, _running) = open_delegate(&registry);
    assert!(registry.take_through(&id.0, Mark::end(1)).is_empty());
    let (_dir, registry) = world();
    let (id, finish) = open_delegate(&registry);
    end_run(finish, &id, "Done.");
    let resumed = resume_ok(&registry, &id, Some(action()));
    assert_eq!(call(&resumed.settle).len(), 4);
    assert!(call(&resumed.settle).is_empty());
}

#[test]
fn a_dropped_resume_settle_is_taken_by_a_later_wait() {
    // (viii) A's settle never called; a wait on run 2 takes all six.
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let (id, finish1) = open_delegate(&registry);
    end_run(finish1, &id, "Done 1.");
    let a = action();
    let resumed_a = resume_ok(&registry, &id, Some(a.clone()));
    end_run(resumed_a.unreported, &id, "Done 2.");
    let waited = registry
        .wait(&id.0, 0, &CancelToken::new())
        .expect("run 2 ended");
    assert_eq!(
        call(&waited.settle.expect("settles")),
        vec![
            df(&id, "Done 1."),
            completed(&id),
            started(&id, Some(a.clone())),
            delegate_started_taken(&id, Some(a.clone())),
            df(&id, "Done 2."),
            completed(&id),
        ]
    );
    let _run1 = take_notice(&rx);
    assert_eq!((take_notice(&rx).claim.0)(), None);
    let _held = resume_ok(&registry, &id, Some(action()));
    assert!(registry.running(&id.0, 3));
}

#[test]
fn a_dropped_resume_settle_is_taken_by_the_run_notice() {
    // (ix) A's settle never called; run 2's notice takes everything
    // ahead of its own news.
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let (id, finish1) = open_delegate(&registry);
    end_run(finish1, &id, "Done 1.");
    let a = action();
    let resumed_a = resume_ok(&registry, &id, Some(a.clone()));
    end_run(resumed_a.unreported, &id, "Done 2.");
    let _run1 = take_notice(&rx);
    let run2 = take_notice(&rx);
    assert_eq!(
        (run2.claim.0)(),
        Some(vec![
            df(&id, "Done 1."),
            completed(&id),
            started(&id, Some(a.clone())),
            delegate_started_taken(&id, Some(a)),
        ])
    );
    assert!(registry.take_through(&id.0, Mark::end(9)).is_empty());
    let _held = resume_ok(&registry, &id, Some(action()));
    assert!(registry.running(&id.0, 3));
}

#[test]
fn a_dropped_resume_settle_is_taken_by_shutdown_after_a_stop() {
    // (x) A's settle never called; run 1's claim takes its news, then
    // run 2 ends and its claim takes run 2's start.
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let (id, finish1) = open_delegate(&registry);
    end_run(finish1, &id, "Done 1.");
    let a = action();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let resumed_a = match registry.resume(&id.0, Some(a.clone()), |_| {
        Ok::<_, String>((
            Stop(Box::new(move || {
                let _sent = stop_tx.send(());
            })),
            (),
        ))
    }) {
        Ok(resumed) => resumed,
        Err(_) => panic!("the resume committed"),
    };
    let run1 = take_notice(&rx);
    assert_eq!((run1.claim.0)(), Some(Vec::new()));
    // Run 2 is stopped while it runs: the fired stop proves the stop
    // was sent before run 2 ends, so the stop parks instead of racing
    // the end.
    let (stopped_tx, stopped_rx) = mpsc::channel();
    let stopper = Arc::clone(&registry);
    let stop_id = id.clone();
    let stopping = thread::spawn(move || {
        let answer = stopper.stop(&stop_id.0, &CancelToken::new()).unwrap();
        let _sent = stopped_tx.send(answer);
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&stop_rx)
        .expect("run 2's stop was sent");
    end_run(resumed_a.unreported, &id, "Done 2.");
    stopping.join().unwrap();
    let stopped = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&stopped_rx)
        .expect("the stop answered run 2");
    assert!(stopped.text.contains("completed"), "{}", stopped.text);
    let run2 = take_notice(&rx);
    assert_eq!(
        (run2.claim.0)(),
        Some(vec![
            started(&id, Some(a.clone())),
            delegate_started_taken(&id, Some(a)),
        ])
    );
    assert!(registry.take_through(&id.0, Mark::end(9)).is_empty());
}

#[test]
fn a_refused_claim_after_a_later_take_leaves_later_starts() {
    // (xi) Run 2's wait took through run 2's end; a third resume queued
    // run 3's start; run 2's claim takes nothing.
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let (id, finish1) = open_delegate(&registry);
    end_run(finish1, &id, "Done 1.");
    let a = action();
    let resumed_a = resume_ok(&registry, &id, Some(a.clone()));
    end_run(resumed_a.unreported, &id, "Done 2.");
    let waited = registry
        .wait(&id.0, 0, &CancelToken::new())
        .expect("run 2 ended");
    assert_eq!(call(&waited.settle.expect("settles")).len(), 6);
    let c = ActionId("a_resume00000002".into());
    let _held3 = resume_ok(&registry, &id, Some(c.clone()));
    let _run1 = take_notice(&rx);
    let run2 = take_notice(&rx);
    assert_eq!((run2.claim.0)(), None);
    assert_eq!(
        registry.take_through(&id.0, Mark::end(3)),
        vec![
            started(&id, Some(c.clone())),
            delegate_started_taken(&id, Some(c)),
        ]
    );
}
