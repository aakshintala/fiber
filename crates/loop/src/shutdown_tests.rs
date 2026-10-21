//! The loop's part of a shutdown: every job is stopped and each end is
//! written before `run` returns; nothing else starts.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use contract::clock::Clock as _;
use contract::events::{
    DelegateFinished, DelegateStarted, Event, ExtensionLog, JobCompleted, JobLine, JobStarted,
    Outcome,
};
use contract::inbox::{Ack, Claim, Delivery, JobNotice, Message, Rejection};
use contract::jobs::{Foreground, Jobs, OpenError, Opened, Opening};
use contract::shapes::{ContentPart, Origin, Sender as From, Tokens, Usage};
use contract::{CommandId, Envelope, ErrorCode, JobId, SessionId};
use fakes::Deadline;
use fakes::NoRules;
use fakes::clock::FakeClock;
use log::Log;

use crate::{Loop, Model, TurnCancel};

/// How long a test waits on the loop before it fails.
const DEADLINE: Duration = Duration::from_secs(10);

/// The idle delay a test sets, on the fake clock.
const IDLE: Duration = Duration::from_secs(60);

/// Jobs a test lists by hand. Each stop sent is reported on `stops`. An
/// end is sent under the lock `running` reads, as the registry sends it.
/// `order` records the close and every `running` read, so a test fails if
/// `close_extension_starts` moves after a sweep's first read. `gate` and
/// `closed` model the registry's start gate and closed-starts flag: `start`
/// refuses with `closing` once closed, and `close_extension_starts` sets
/// the flag under the gate. `hold` parks one `start` at a pause point.
struct Listed {
    ids: Mutex<Vec<JobId>>,
    stops: Mutex<Sender<JobId>>,
    snapshot_pause: Mutex<Option<(Sender<()>, Receiver<()>)>>,
    order: Mutex<Vec<String>>,
    gate: Mutex<()>,
    closed: Mutex<bool>,
    hold: Mutex<Option<(Sender<()>, Receiver<()>)>>,
    bound: Mutex<Vec<JobId>>,
    launched: Mutex<usize>,
}

impl Listed {
    fn new(ids: &[&str]) -> (Arc<Self>, Receiver<JobId>) {
        let (stops, stopped) = mpsc::channel();
        let listed = Arc::new(Self {
            ids: Mutex::new(ids.iter().map(|id| JobId((*id).into())).collect()),
            stops: Mutex::new(stops),
            snapshot_pause: Mutex::new(None),
            order: Mutex::new(Vec::new()),
            gate: Mutex::new(()),
            closed: Mutex::new(false),
            hold: Mutex::new(None),
            bound: Mutex::new(Vec::new()),
            launched: Mutex::new(0),
        });
        (listed, stopped)
    }

    /// Lists `id` as running.
    fn add(&self, id: &str) {
        self.ids.lock().unwrap().push(JobId(id.into()));
    }

    /// Parks the next `start` at a pause point: it sends `reached`, then
    /// blocks until the test drops or sends on `release`.
    fn hold_start(&self, reached: Sender<()>, release: Receiver<()>) {
        *self.hold.lock().unwrap() = Some((reached, release));
    }

    /// An extension-owned delegate start, modelling the registry's rule: a
    /// held start signals before the gate is taken; under the gate a
    /// closed flag refuses with `closing` and records nothing, and an open
    /// one binds, lists and launches.
    fn start(&self, id: &str) -> Result<JobId, (ErrorCode, String)> {
        if let Some((reached, release)) = self.hold.lock().unwrap().take() {
            let _sent = reached.send(());
            let _released = release.recv();
        }
        let _gate = self.gate.lock().unwrap();
        if *self.closed.lock().unwrap() {
            return Err((
                ErrorCode::Closing,
                "The session is ending, so no delegate was started.".into(),
            ));
        }
        let job = JobId(id.into());
        self.bound.lock().unwrap().push(job.clone());
        self.ids.lock().unwrap().push(job.clone());
        *self.launched.lock().unwrap() += 1;
        Ok(job)
    }

    /// What `start` recorded, in order.
    fn bound(&self) -> Vec<JobId> {
        self.bound.lock().unwrap().clone()
    }

    /// How many starts reached the launch.
    fn launched(&self) -> usize {
        *self.launched.lock().unwrap()
    }

    /// The close and `running` reads, in order.
    fn order(&self) -> Vec<String> {
        self.order.lock().unwrap().clone()
    }

    /// Ends `id`: its notice reaches `inbox`, then `running` drops it.
    fn end(&self, id: &str, inbox: &Sender<Delivery>) {
        self.end_with(id, inbox, None);
    }

    /// [`Listed::end`], with a delegate's finish riding the notice.
    fn end_delegate(&self, id: &str, inbox: &Sender<Delivery>, text: &str) {
        self.end_with(
            id,
            inbox,
            Some(DelegateFinished {
                job_id: JobId(id.into()),
                text: text.into(),
                artifact: None,
                questions: None,
                usage: Usage {
                    tokens: Tokens {
                        input: 0,
                        cache_read: 0,
                        cache_write: std::collections::BTreeMap::new(),
                        output: 0,
                    },
                    cost: Some(0.0),
                    subscription_cost: 0.0,
                },
                worktree: None,

                changed_by: None,
            }),
        );
    }

    /// Ends `id`: its notice reaches `inbox`, then `running` drops it.
    fn end_with(&self, id: &str, inbox: &Sender<Delivery>, delegate: Option<DelegateFinished>) {
        let mut ids = self.ids.lock().unwrap();
        inbox
            .send(Delivery::Job(JobNotice {
                completed: JobCompleted {
                    job_id: JobId(id.into()),
                    status: Outcome::Cancelled,
                    error: None,
                    process: None,
                    output_tail: None,

                    notice: None,
                    changed_by: None,
                },
                claim: Claim(Box::new(|| Some(Vec::new()))),
                delegate,
            }))
            .unwrap();
        ids.retain(|listed| listed.0 != id);
    }

    /// Sends `notice` for `id`, then drops `id` from the running list,
    /// under the one `ids` lock: the settle either sees `id` running with
    /// the notice already queued, or sees it gone and drains the queued
    /// notice, so it never blocks on `recv` with nothing left to deliver.
    fn send_and_forget(&self, id: &str, inbox: &Sender<Delivery>, notice: JobNotice) {
        let mut ids = self.ids.lock().unwrap();
        inbox.send(Delivery::Job(notice)).unwrap();
        ids.retain(|listed| listed.0 != id);
    }
}

impl Jobs for Listed {
    fn open(&self, _: Opening) -> Result<Opened, OpenError> {
        Err(OpenError {
            path: "unused".into(),
            source: std::io::Error::other("a listed job is not opened"),
        })
    }

    fn stop(&self, job_id: &JobId) -> bool {
        let _sent = self.stops.lock().unwrap().send(job_id.clone());
        true
    }

    fn stop_delegates(&self) -> usize {
        0
    }

    fn background(&self) -> usize {
        0
    }

    fn foreground(&self, _: Foreground) {}

    fn running(&self) -> Vec<JobId> {
        self.order.lock().unwrap().push("running".into());
        let ids = self.ids.lock().unwrap().clone();
        if let Some((reached, release)) = self.snapshot_pause.lock().unwrap().take() {
            reached.send(()).unwrap();
            let _released = release.recv();
        }
        ids
    }

    fn close_extension_starts(&self) {
        let _gate = self.gate.lock().unwrap();
        *self.closed.lock().unwrap() = true;
        self.order.lock().unwrap().push("close".into());
    }

    fn deliver_to(&self, _: Sender<Delivery>) {}
}

/// `id`'s end, stopped, whose claim holds.
fn notice(id: &str) -> Delivery {
    Delivery::Job(JobNotice {
        completed: JobCompleted {
            job_id: JobId(id.into()),
            status: Outcome::Cancelled,
            error: None,
            process: None,
            output_tail: None,

            notice: None,
            changed_by: None,
        },
        claim: Claim(Box::new(|| Some(Vec::new()))),
        delegate: None,
    })
}

fn message(text: &str) -> Message {
    Message {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: From {
            origin: Origin::Driver,
            command_id: Some(CommandId("c_1".into())),
        },
    }
}

/// An ack that reports what the command was answered.
fn answered() -> (Ack, Receiver<Result<(), Rejection>>) {
    let (tx, rx) = mpsc::channel();
    let ack = Ack(Box::new(move |result| {
        let _sent = tx.send(result.map(|_| ()));
    }));
    (ack, rx)
}

/// A loop on a fresh log with `jobs`, which no model call reaches.
struct World {
    held: Held,
    clock: Arc<FakeClock>,
    inbox: Sender<Delivery>,
    cancel: Arc<TurnCancel>,
    looped: Loop,
}

/// What outlives the loop: its directory.
struct Held {
    _home: fakes::TempDir,
    dir: std::path::PathBuf,
}

impl World {
    fn new(jobs: Arc<dyn Jobs>) -> Self {
        let home = fakes::TempDir::new("fiber-shutdown");
        let workspace = home.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let fake = FakeClock::new();
        let clock: Arc<dyn contract::clock::Clock> = fake.clone();
        let log = Arc::new(
            Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap(),
        );
        let (inbox, rx) = mpsc::channel();
        let cancel = Arc::new(TurnCancel::default());
        let looped = Loop::start(
            crate::Session {
                log,
                provider: Arc::new(fakes::ScriptedProvider::new(Vec::new())),
                model: Model {
                    reference: "fake/model".into(),
                    cost: None,
                    subscription: false,
                },
                prompt: crate::prompt::PromptInputs::new(
                    home.path().to_path_buf(),
                    "/bin/sh".into(),
                    home.path()
                        .join("s_test/events.jsonl")
                        .display()
                        .to_string(),
                    clock,
                    fakes::CONTEXT_WINDOW,
                ),
                inbox: rx,
                tools: Vec::new(),
                permissions: crate::Permissions {
                    workspace: workspace.display().to_string(),
                    credentials: home.path().join("credentials"),
                    credential_files: Vec::new(),
                    rules: Arc::new(NoRules),
                },
            },
            None,
        )
        .unwrap()
        .cancelled_by(Arc::clone(&cancel))
        .jobs(jobs);
        Self {
            held: Held {
                dir: home.path().join("s_test"),
                _home: home,
            },
            clock: fake,
            inbox,
            cancel,
            looped,
        }
    }

    /// Runs the loop to its end on its own thread.
    fn spawn_run(self) -> (Receiver<Result<(), crate::Error>>, Sender<Delivery>, Held) {
        let (done, finished) = mpsc::channel();
        let looped = self.looped;
        thread::spawn(move || {
            let _sent = done.send(looped.run());
        });
        (finished, self.inbox, self.held)
    }
}

struct DroppedCommand(Arc<std::sync::atomic::AtomicBool>);
impl Drop for DroppedCommand {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn settling_drops_a_command_request_without_taking_answering_or_logging_it() {
    let (jobs, _stops) = Listed::new(&[]);
    let mut world = World::new(jobs);
    let watchdog = fakes::Watchdog::matching(&world.held._home.path().display().to_string());
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let guard = DroppedCommand(dropped.clone());
    let taken = Arc::new(AtomicUsize::new(0));
    let take = taken.clone();
    let answered = Arc::new(AtomicUsize::new(0));
    let answer = answered.clone();
    let request = contract::inbox::InnerCallRequest {
        extension: "ext".into(),
        command: CommandId("c_1".into()),
        name: "read".into(),
        arguments: serde_json::json!({}),
        cancel: Arc::new(fakes::CancelToken::new()),
        taken: Box::new(move || {
            take.fetch_add(1, Ordering::SeqCst);
        }),
        answer: Box::new(move |_| {
            answer.fetch_add(1, Ordering::SeqCst);
            drop(guard);
        }),
    };
    let world = fakes::within(
        "settle command request",
        Deadline::after(Duration::from_secs(3)),
        move || {
            world
                .looped
                .settle_one(Delivery::InnerCall(request))
                .unwrap();
            world
        },
    );
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(taken.load(Ordering::SeqCst), 0);
    assert_eq!(answered.load(Ordering::SeqCst), 0);
    assert_eq!(world.held.kinds(), ["session_started"]);
    watchdog.stand_down(fakes::Deadline::after(Duration::from_secs(3)));
}

impl Held {
    fn durable(&self) -> Vec<Envelope> {
        log::read(&self.dir)
            .unwrap()
            .into_iter()
            .filter(Envelope::is_durable)
            .collect()
    }

    fn kinds(&self) -> Vec<String> {
        self.durable().into_iter().map(|line| line.kind).collect()
    }
}

#[track_caller]
fn ran(finished: &Receiver<Result<(), crate::Error>>) {
    let result = Deadline::after(DEADLINE)
        .recv(finished)
        .expect("run returned");
    assert!(result.is_ok(), "{result:?}");
}

#[track_caller]
fn stopped(stops: &Receiver<JobId>, wait: &Deadline) -> String {
    wait.recv(stops).expect("a stop was sent").0
}

/// The ack's answer, failing at [`DEADLINE`].
#[track_caller]
fn answer(rx: &Receiver<Result<(), Rejection>>) -> Result<(), Rejection> {
    Deadline::after(DEADLINE).recv(rx).unwrap()
}

#[test]
fn a_normal_end_stops_only_extension_jobs_and_waits_for_their_ends() {
    let (jobs, stops) = Listed::new(&["j_extension", "j_model"]);
    let mut world = World::new(jobs.clone());
    world
        .looped
        .write(
            &contract::events::Event::JobStarted(contract::events::JobStarted {
                job_id: JobId("j_extension".into()),
                tool: None,
                extension: Some("test/scan".into()),
                description: "scan".into(),
                output_path: "/child/events.jsonl".into(),
            }),
            None,
            None,
        )
        .unwrap();
    let (done, finished) = mpsc::channel();
    let inbox = world.inbox.clone();
    let held = world.held;
    std::thread::spawn(move || {
        done.send(world.looped.settled(Ok(()))).unwrap();
    });
    let wait = Deadline::after(DEADLINE);
    assert_eq!(stopped(&stops, &wait), "j_extension");
    jobs.end_delegate("j_extension", &inbox, "PRIVATE SENTINEL");
    ran(&finished);
    assert!(
        stops.try_recv().is_err(),
        "the model-owned job is not stopped"
    );
    let lines = log::read(&held.dir).unwrap();
    let ends: Vec<_> = lines
        .iter()
        .filter(|line| matches!(line.kind.as_str(), "delegate_finished" | "job_completed"))
        .collect();
    assert_eq!(ends.len(), 2);
    assert_eq!(ends[1].payload["status"], "cancelled");
    assert!(
        ends.iter()
            .all(|line| line.turn_id.is_none() && line.action_id.is_none())
    );
    assert!(
        !lines
            .iter()
            .any(|line| matches!(line.kind.as_str(), "turn_started" | "jobs_pending_notified"))
    );
}

#[test]
fn a_normal_end_does_not_wait_again_for_an_end_drained_after_its_running_snapshot() {
    for remaining in [false, true] {
        let ids = if remaining {
            vec!["j_finished", "j_running"]
        } else {
            vec!["j_finished"]
        };
        let (jobs, stops) = Listed::new(&ids);
        let mut world = World::new(jobs.clone());
        for id in &ids {
            world
                .looped
                .write(
                    &contract::events::Event::JobStarted(contract::events::JobStarted {
                        job_id: JobId((*id).into()),
                        tool: None,
                        extension: Some("test/scan".into()),
                        description: "scan".into(),
                        output_path: "/child/events.jsonl".into(),
                    }),
                    None,
                    None,
                )
                .unwrap();
        }
        let (reached, snapshot) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        *jobs.snapshot_pause.lock().unwrap() = Some((reached, resume));
        let (done, finished) = mpsc::channel();
        let inbox = world.inbox.clone();
        let held = world.held;
        thread::spawn(move || {
            let _sent = done.send(world.looped.settled(Ok(())));
        });
        Deadline::after(DEADLINE).recv_or_fail(&snapshot, "the running snapshot was read");
        let Delivery::Job(mut end) = notice("j_finished") else {
            unreachable!()
        };
        end.completed.status = Outcome::Completed;
        jobs.send_and_forget("j_finished", &inbox, end);
        release.send(()).unwrap();
        if remaining {
            assert_eq!(stopped(&stops, &Deadline::after(DEADLINE)), "j_running");
            jobs.end("j_running", &inbox);
        }
        ran(&finished);
        assert!(stops.try_recv().is_err(), "no ended job is stopped");
        let expected = if remaining {
            vec![
                "session_started",
                "job_started",
                "job_started",
                "job_completed",
                "job_completed",
            ]
        } else {
            vec!["session_started", "job_started", "job_completed"]
        };
        assert_eq!(held.kinds(), expected);
        let lines = held.durable();
        let end = &lines[ids.len() + 1];
        assert_eq!(end.payload["job_id"], "j_finished");
        assert_eq!(end.payload["status"], "completed");
        assert!(end.turn_id.is_none() && end.action_id.is_none());
        if remaining {
            assert_eq!(lines.last().unwrap().payload["job_id"], "j_running");
            assert_eq!(lines.last().unwrap().payload["status"], "cancelled");
        }
    }
}

#[test]
fn a_normal_end_releases_running_extension_jobs_before_its_first_drain() {
    let (jobs, stops) = Listed::new(&["j_1", "j_2"]);
    let hooks = Arc::new(CountingHooks::default());
    let mut world = World::new(jobs.clone()).hooked(Arc::clone(&hooks));
    // `j_0` ended before the end: owned but not running, so never released.
    for id in ["j_0", "j_1", "j_2"] {
        world
            .looped
            .write(
                &Event::JobStarted(JobStarted {
                    job_id: JobId(id.into()),
                    tool: None,
                    extension: Some("test/scan".into()),
                    description: "scan".into(),
                    output_path: "/child/events.jsonl".into(),
                }),
                None,
                None,
            )
            .unwrap();
    }
    // The running jobs' outputs are other sessions' logs: the end path
    // skips its `output_file` pass instead of opening `/child`.
    for id in ["j_1", "j_2"] {
        world
            .looped
            .write(
                &Event::DelegateStarted(DelegateStarted {
                    job_id: JobId(id.into()),
                    delegate_session_id: SessionId("s_child".into()),
                    harness: "fiber".into(),
                    model: "fake/model".into(),
                    workspace: "/w".into(),
                    worktree: None,
                    forked_from: None,
                }),
                None,
                None,
            )
            .unwrap();
    }
    let (released_tx, released_rx) = mpsc::channel();
    let (proceed_tx, proceed_rx) = mpsc::channel();
    let proceed_rx = Mutex::new(proceed_rx);
    // The release parks the settled thread: while parked, no drain delivery
    // can have run and no stop can have been sent, so the release's place
    // at the top of the branch is structural, not raced.
    world.looped = world.looped.released(Arc::new(move |ids: &[JobId]| {
        let _sent = released_tx.send(ids.to_vec());
        let _go = proceed_rx.lock().unwrap().recv();
    }));
    // The drain waits on the running snapshot: the release has not fired.
    let (reached, snapshot) = mpsc::channel();
    let (unblock, unblocked) = mpsc::channel();
    *jobs.snapshot_pause.lock().unwrap() = Some((reached, unblocked));
    let (done, finished) = mpsc::channel();
    let inbox = world.inbox.clone();
    let held = world.held;
    thread::spawn(move || {
        let _sent = done.send(world.looped.settled(Ok(())));
    });
    let wait = Deadline::after(DEADLINE);
    wait.recv_or_fail(&snapshot, "the running snapshot was read");
    // Queued while the snapshot is withheld: the running read already took
    // its list, so both ids are still released. The steer is processed by
    // the first drain, the `j_1` end is dropped by it.
    let (steer, steered) = answered();
    inbox
        .send(Delivery::Steer(message("hello"), steer))
        .unwrap();
    jobs.end_delegate("j_1", &inbox, "QUEUED SENTINEL");
    assert!(
        released_rx.try_recv().is_err(),
        "the release waits on the drain"
    );
    unblock.send(()).unwrap();
    // The release fired first, with exactly the still-running owned ids:
    // installed, and no ended id passed.
    assert_eq!(
        wait.recv(&released_rx).expect("the release fired"),
        vec![JobId("j_1".into()), JobId("j_2".into())]
    );
    // While the release parks the settled thread, nothing after it ran.
    assert!(stops.try_recv().is_err(), "no stop preceded the release");
    assert!(
        steered.try_recv().is_err(),
        "no drain delivery ran before the release"
    );
    proceed_tx.send(()).unwrap();
    // The first drain then processes the steer and drops the `j_1` end.
    let refusal = wait.recv(&steered).expect("the steer was refused");
    assert_eq!(refusal.unwrap_err().code, ErrorCode::Closing);
    // `j_1` already ended: only `j_2` is stopped, once.
    assert_eq!(stopped(&stops, &wait), "j_2");
    jobs.end("j_1", &inbox);
    jobs.end("j_2", &inbox);
    ran(&finished);
    assert!(stops.try_recv().is_err(), "no ended job is stopped");
    // Both real ends are written cancelled; the queued `j_1` end never was.
    let lines = held.durable();
    let ends: Vec<_> = lines
        .iter()
        .filter(|line| line.kind.as_str() == "job_completed")
        .collect();
    assert_eq!(ends.len(), 2);
    for (end, id) in ends.iter().zip(["j_1", "j_2"]) {
        assert_eq!(end.payload["job_id"], id);
        assert_eq!(end.payload["status"], "cancelled");
        assert!(end.turn_id.is_none() && end.action_id.is_none());
    }
    assert!(
        !lines
            .iter()
            .any(|line| line.kind.as_str() == "delegate_finished"
                && line.payload["text"] == "QUEUED SENTINEL"),
        "the dropped end was never written"
    );
    // The dropped end ran no delivery hook; both real ends still ran theirs.
    assert!(
        !hooks
            .contents()
            .iter()
            .any(|content| content.contains("QUEUED SENTINEL")),
        "a dropped end runs no delivery hook"
    );
    assert_eq!(
        hooks.calls(),
        2,
        "both real ends still run their delivery hooks"
    );
}

#[test]
fn a_shutdown_writes_a_bound_end_raw_with_no_release() {
    let (jobs, stops) = Listed::new(&["j_1"]);
    let mut world = World::new(jobs.clone());
    world
        .looped
        .write(
            &Event::JobStarted(JobStarted {
                job_id: JobId("j_1".into()),
                tool: None,
                extension: Some("test/scan".into()),
                description: "scan".into(),
                output_path: "/child/events.jsonl".into(),
            }),
            None,
            None,
        )
        .unwrap();
    let (released_tx, released_rx) = mpsc::channel();
    world.looped = world.looped.released(Arc::new(move |ids: &[JobId]| {
        let _sent = released_tx.send(ids.to_vec());
    }));
    world.cancel.shutdown(143);
    let (done, finished) = mpsc::channel();
    let inbox = world.inbox.clone();
    let held = world.held;
    thread::spawn(move || {
        let _sent = done.send(world.looped.settled(Ok(())));
    });
    let wait = Deadline::after(DEADLINE);
    assert_eq!(stopped(&stops, &wait), "j_1");
    jobs.end_delegate("j_1", &inbox, "BOUND SENTINEL");
    ran(&finished);
    assert!(
        released_rx.try_recv().is_err(),
        "a shutdown releases nothing"
    );
    let lines = held.durable();
    assert_eq!(
        held.kinds(),
        [
            "session_started",
            "job_started",
            "delegate_finished",
            "job_completed"
        ]
    );
    assert_eq!(lines[2].payload["text"], "BOUND SENTINEL");
    assert_eq!(lines[3].payload["job_id"], "j_1");
    assert_eq!(lines[3].payload["status"], "cancelled");
    assert!(
        lines[2..]
            .iter()
            .all(|line| line.turn_id.is_none() && line.action_id.is_none())
    );
}

#[test]
fn a_normal_end_hooks_a_model_jobs_end() {
    let (jobs, stops) = Listed::new(&["j_extension", "j_model"]);
    let hooks = Arc::new(CountingHooks {
        redact: true,
        ..Default::default()
    });
    let mut world = World::new(jobs.clone()).hooked(hooks.clone());
    world
        .looped
        .write(
            &contract::events::Event::JobStarted(contract::events::JobStarted {
                job_id: JobId("j_extension".into()),
                tool: None,
                extension: Some("test/scan".into()),
                description: "scan".into(),
                output_path: "/child/events.jsonl".into(),
            }),
            None,
            None,
        )
        .unwrap();
    world
        .looped
        .write(
            &contract::events::Event::DelegateStarted(contract::events::DelegateStarted {
                job_id: JobId("j_extension".into()),
                delegate_session_id: SessionId("s_child".into()),
                harness: "fiber".into(),
                model: "fake/model".into(),
                workspace: "/w".into(),
                worktree: None,
                forked_from: None,
            }),
            None,
            None,
        )
        .unwrap();
    let (done, finished) = mpsc::channel();
    let inbox = world.inbox.clone();
    let held = world.held;
    std::thread::spawn(move || {
        done.send(world.looped.settled(Err(crate::Error::NoSessionStarted)))
            .unwrap();
    });
    let wait = Deadline::after(DEADLINE);
    assert_eq!(stopped(&stops, &wait), "j_extension");
    jobs.end_delegate("j_model", &inbox, "PRIVATE MODEL OUTPUT");
    jobs.end_delegate("j_extension", &inbox, "PRIVATE EXTENSION OUTPUT");
    assert!(matches!(
        wait.recv(&finished).unwrap(),
        Err(crate::Error::NoSessionStarted)
    ));
    let lines = held.durable();
    let end = lines
        .iter()
        .find(|line| line.kind == "delegate_finished" && line.payload["job_id"] == "j_model")
        .unwrap();
    assert_eq!(end.payload["text"], "redacted");
    assert_eq!(end.payload["changed_by"], serde_json::json!(["redactor"]));
    assert_eq!(hooks.calls(), 4);
}

#[test]
fn abnormal_settle_redacts_only_the_extension_owned_end() {
    let (jobs, _stops) = Listed::new(&["j_extension", "j_model"]);
    let hooks = Arc::new(CountingHooks {
        redact: true,
        ..Default::default()
    });
    let mut world = World::new(jobs.clone()).hooked(hooks);
    world
        .looped
        .write(
            &contract::events::Event::JobStarted(contract::events::JobStarted {
                job_id: JobId("j_extension".into()),
                tool: None,
                extension: Some("test/scan".into()),
                description: "scan".into(),
                output_path: "/child/events.jsonl".into(),
            }),
            None,
            None,
        )
        .unwrap();
    world
        .looped
        .write(
            &contract::events::Event::DelegateStarted(contract::events::DelegateStarted {
                job_id: JobId("j_extension".into()),
                delegate_session_id: SessionId("s_child".into()),
                harness: "fiber".into(),
                model: "fake/model".into(),
                workspace: "/w".into(),
                worktree: None,
                forked_from: None,
            }),
            None,
            None,
        )
        .unwrap();
    jobs.end_delegate("j_extension", &world.inbox, "PRIVATE EXTENSION OUTPUT");
    jobs.end_delegate("j_model", &world.inbox, "MODEL OUTPUT");
    let world = fakes::within(
        "settle both job ends",
        Deadline::after(DEADLINE),
        move || {
            for delivery in world.looped.inbox.try_iter() {
                world.looped.settle_one(delivery).unwrap();
            }
            world
        },
    );
    assert_eq!(
        world.held.kinds(),
        [
            "session_started",
            "job_started",
            "delegate_started",
            "delegate_finished",
            "job_completed",
            "delegate_finished",
            "job_completed",
        ]
    );
    let lines = world.held.durable();
    assert_eq!(lines[3].payload["job_id"], "j_extension");
    assert_eq!(lines[3].payload["text"], "redacted");
    assert_eq!(
        lines[3].payload["changed_by"],
        serde_json::json!(["redactor"])
    );
    assert_eq!(lines[4].payload["job_id"], "j_extension");
    assert_eq!(lines[4].payload["status"], "cancelled");
    assert_eq!(lines[5].payload["job_id"], "j_model");
    assert_eq!(lines[5].payload["text"], "MODEL OUTPUT");
    assert_eq!(lines[5].payload.get("changed_by"), None);
    assert_eq!(lines[6].payload["job_id"], "j_model");
    assert_eq!(lines[6].payload["status"], "cancelled");
}

#[test]
fn a_normal_end_with_no_jobs_drains_nothing() {
    let (jobs, _) = Listed::new(&[]);
    let mut world = World::new(jobs);
    let (ack, answer) = answered();
    world
        .inbox
        .send(Delivery::Shell(
            contract::events::ShellCommand {
                command: "late".into(),
                output: "late".into(),
                artifact: None,
                process: contract::shapes::Process {
                    exit_code: Some(0),
                    signal: None,
                    timed_out: false,
                },
            },
            ack,
        ))
        .unwrap();
    world.looped.settled(Ok(())).unwrap();
    assert!(answer.try_recv().is_err());
    assert!(
        !log::read(&world.held.dir)
            .unwrap()
            .iter()
            .any(|line| line.kind == "shell_command")
    );
}

#[test]
fn a_shutdown_stops_a_running_job_and_writes_its_end_before_run_returns() {
    let (jobs, stops) = Listed::new(&["j_1"]);
    let world = World::new(Arc::clone(&jobs) as Arc<dyn Jobs>);
    world.cancel.shutdown(143);
    let (finished, inbox, held) = world.spawn_run();

    let wait = Deadline::after(DEADLINE);
    assert_eq!(stopped(&stops, &wait), "j_1");
    jobs.end("j_1", &inbox);
    ran(&finished);

    // No turn, no ending notice: only the job's end.
    assert_eq!(held.kinds(), ["session_started", "job_completed"]);
    let end = &held.durable()[1];
    assert_eq!(end.payload["job_id"], "j_1");
    assert_eq!(end.payload["status"], "cancelled");
    assert_eq!(end.turn_id, None);
}

#[test]
fn settle_writes_a_queued_delegate_finish_before_its_end() {
    let (jobs, _stops) = Listed::new(&[]);
    let mut world = World::new(Arc::clone(&jobs) as Arc<dyn Jobs>);
    world.looped.queued.push_back(crate::jobs::Queued::Job(
        Vec::new(),
        JobCompleted {
            job_id: JobId("j_1".into()),
            status: Outcome::Cancelled,
            error: None,
            process: None,
            output_tail: None,

            notice: None,
            changed_by: None,
        },
        Some(DelegateFinished {
            job_id: JobId("j_1".into()),
            text: "Done.".into(),
            artifact: None,
            questions: None,
            usage: Usage {
                tokens: Tokens {
                    input: 0,
                    cache_read: 0,
                    cache_write: std::collections::BTreeMap::new(),
                    output: 0,
                },
                cost: Some(0.0),
                subscription_cost: 0.0,
            },
            worktree: None,

            changed_by: None,
        }),
    ));
    world.cancel.shutdown(143);
    let (finished, _inbox, held) = world.spawn_run();
    ran(&finished);
    assert_eq!(
        held.kinds(),
        ["session_started", "delegate_finished", "job_completed"]
    );
    let lines = held.durable();
    assert_eq!(lines[1].payload["job_id"], "j_1");
    assert_eq!(lines[1].payload["text"], "Done.");
    assert_eq!(lines[1].turn_id, None);
    assert_eq!(lines[2].payload["job_id"], "j_1");
    assert_eq!(lines[2].turn_id, None);
}

#[test]
fn settle_writes_a_delegate_finish_delivered_while_settling() {
    let (jobs, stops) = Listed::new(&["j_1"]);
    let world = World::new(Arc::clone(&jobs) as Arc<dyn Jobs>);
    world.cancel.shutdown(143);
    let (finished, inbox, held) = world.spawn_run();
    let wait = Deadline::after(DEADLINE);
    assert_eq!(stopped(&stops, &wait), "j_1");
    jobs.end_delegate("j_1", &inbox, "Done.");
    ran(&finished);
    assert_eq!(
        held.kinds(),
        ["session_started", "delegate_finished", "job_completed"]
    );
    let lines = held.durable();
    assert_eq!(lines[1].payload["text"], "Done.");
    assert_eq!(lines[2].payload["job_id"], "j_1");
    assert_eq!(lines[2].payload["status"], "cancelled");
}

#[test]
fn the_stop_is_sent_again_to_a_job_listed_after_the_first() {
    let (jobs, stops) = Listed::new(&["j_1"]);
    let world = World::new(Arc::clone(&jobs) as Arc<dyn Jobs>);
    world.cancel.shutdown(130);
    let (finished, inbox, held) = world.spawn_run();

    let wait = Deadline::after(DEADLINE);
    assert_eq!(stopped(&stops, &wait), "j_1");
    // A call moves to the background as the first job ends.
    jobs.add("j_2");
    jobs.end("j_1", &inbox);
    let wait = Deadline::after(DEADLINE);
    while stopped(&stops, &wait) != "j_2" {}
    jobs.end("j_2", &inbox);
    ran(&finished);

    assert_eq!(
        held.kinds(),
        ["session_started", "job_completed", "job_completed"]
    );
}

#[test]
fn a_shutdown_answers_what_arrives_and_starts_nothing() {
    let (jobs, stops) = Listed::new(&["j_1"]);
    let world = World::new(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let (prompt, prompt_answer) = answered();
    let (steer, steer_answer) = answered();
    let (drop_steer, drop_answer) = answered();
    let (reply, reply_answer) = answered();
    let (model, model_answer) = answered();
    let (credential, credential_answer) = answered();
    let (name, name_answer) = answered();
    let (close, close_answer) = answered();
    world.cancel.shutdown(129);
    let (finished, inbox, held) = world.spawn_run();
    let wait = Deadline::after(DEADLINE);
    assert_eq!(stopped(&stops, &wait), "j_1");

    inbox
        .send(Delivery::Prompt(message("hello"), prompt))
        .unwrap();
    inbox.send(Delivery::Steer(message("more"), steer)).unwrap();
    inbox
        .send(Delivery::SteerDrop(CommandId("c_1".into()), drop_steer))
        .unwrap();
    inbox
        .send(Delivery::Reply(
            contract::commands::Reply {
                request_id: contract::RequestId("r_1".into()),
                answer: contract::commands::ReplyAnswer::Confirmed { confirmed: true },
            },
            reply,
        ))
        .unwrap();
    inbox.send(Delivery::Close(close)).unwrap();
    inbox
        .send(Delivery::Model(
            contract::commands::ModelArgs {
                model: "fake/model-2".into(),
                thinking: None,
            },
            model,
        ))
        .unwrap();
    inbox
        .send(Delivery::Credential(
            contract::commands::CredentialArgs {
                label: "home".into(),
            },
            credential,
        ))
        .unwrap();
    inbox
        .send(Delivery::Name(
            contract::commands::Name {
                text: "Mine".into(),
            },
            name,
        ))
        .unwrap();
    inbox
        .send(Delivery::JobLine(JobLine {
            job_id: JobId("j_1".into()),
            lines: "tick\n".into(),
            suppressed: None,

            changed_by: None,
        }))
        .unwrap();
    jobs.end("j_1", &inbox);
    ran(&finished);

    assert_eq!(answer(&prompt_answer).unwrap_err().code, ErrorCode::Closing);
    assert_eq!(answer(&steer_answer).unwrap_err().code, ErrorCode::Closing);
    assert_eq!(
        answer(&drop_answer).unwrap_err().code,
        ErrorCode::StaleRequest
    );
    assert_eq!(
        answer(&reply_answer).unwrap_err().code,
        ErrorCode::StaleRequest
    );
    assert_eq!(answer(&model_answer).unwrap_err().code, ErrorCode::Closing);
    assert_eq!(
        answer(&credential_answer).unwrap_err().code,
        ErrorCode::Closing
    );
    assert_eq!(answer(&name_answer).unwrap_err().code, ErrorCode::Closing);
    assert!(answer(&close_answer).is_ok());
    // The monitor's batch is dropped; the job's end is written.
    assert_eq!(held.kinds(), ["session_started", "job_completed"]);
}

#[test]
fn a_prompt_waiting_when_the_shutdown_lands_starts_no_turn() {
    let (jobs, _stops) = Listed::new(&[]);
    let world = World::new(jobs);
    let (prompt, prompt_answer) = answered();
    world
        .inbox
        .send(Delivery::Prompt(message("hello"), prompt))
        .unwrap();
    world.cancel.shutdown(143);
    let (finished, _inbox, held) = world.spawn_run();
    ran(&finished);

    let rejected = Deadline::after(DEADLINE)
        .recv(&prompt_answer)
        .unwrap()
        .unwrap_err();
    assert_eq!(rejected.code, ErrorCode::Closing);
    assert_eq!(held.kinds(), ["session_started"]);
}

#[test]
fn a_steer_whose_ack_starts_the_shutdown_writes_no_turn_started() {
    let (jobs, _stops) = Listed::new(&[]);
    let world = World::new(jobs);
    let cancel = Arc::clone(&world.cancel);
    // The steer is accepted as the idle wait admits it: after `turn`
    // checked for a shutdown, before `turn_started` is armed.
    let ack = Ack(Box::new(move |_| cancel.shutdown(143)));
    world
        .inbox
        .send(Delivery::Steer(message("hello"), ack))
        .unwrap();
    let (finished, _inbox, held) = world.spawn_run();
    ran(&finished);

    assert!(!held.kinds().contains(&"turn_started".to_owned()));
}

#[test]
fn without_a_shutdown_a_closed_inbox_stops_no_job() {
    let (jobs, stops) = Listed::new(&["j_1"]);
    let world = World::new(jobs);
    let (finished, inbox, held) = world.spawn_run();
    // Every sender gone: the loop returns with the job still running.
    drop(inbox);
    ran(&finished);

    assert!(stops.try_recv().is_err(), "no stop was sent");
    assert_eq!(held.kinds(), ["session_started"]);
}

#[test]
fn a_shutdown_ends_an_idle_wait_at_its_next_wake() {
    let (jobs, _stops) = Listed::new(&[]);
    let mut world = World::new(jobs);
    world.looped = world.looped.idle_exit(Some(IDLE));
    let until = world.clock.now() + IDLE;
    let clock = Arc::clone(&world.clock);
    let cancel = Arc::clone(&world.cancel);
    let (finished, inbox, held) = world.spawn_run();
    assert!(
        clock.await_parked(until, fakes::Deadline::after(DEADLINE)),
        "the loop waits on the idle deadline"
    );
    // The door's stopper wakes the wait with a delivery.
    cancel.shutdown(143);
    inbox.send(Delivery::Cancelled).unwrap();
    ran(&finished);

    assert_eq!(held.kinds(), ["session_started"]);
}

#[test]
fn a_job_end_taken_into_a_refused_turn_is_written_by_the_settle() {
    let (jobs, _stops) = Listed::new(&[]);
    let world = World::new(jobs);
    let cancel = Arc::clone(&world.cancel);
    world.inbox.send(notice("j_1")).unwrap();
    let ack = Ack(Box::new(move |_| cancel.shutdown(143)));
    world
        .inbox
        .send(Delivery::Steer(message("hello"), ack))
        .unwrap();
    let (finished, _inbox, held) = world.spawn_run();
    ran(&finished);

    let kinds = held.kinds();
    assert!(!kinds.contains(&"turn_started".to_owned()));
    assert_eq!(kinds.last().map(String::as_str), Some("job_completed"));
    assert_eq!(held.durable().last().unwrap().payload["job_id"], "j_1");
}

#[test]
fn a_reply_taken_as_the_shutdown_lands_leaves_the_request_pending() {
    let (jobs, _stops) = Listed::new(&[]);
    let mut world = World::new(jobs);
    world.cancel.shutdown(143);
    let pending = contract::RequestId("r_1".into());
    let turn = contract::TurnId("t_1".into());
    let (ack, answer) = answered();
    let reply = contract::commands::Reply {
        request_id: pending.clone(),
        answer: contract::commands::ReplyAnswer::Approval {
            decision: contract::events::Decision::Allow,
            feedback: None,
            remember: None,
        },
    };
    let waited = world
        .looped
        .take_while_waiting(&pending, Delivery::Reply(reply, ack), &turn)
        .unwrap();
    assert!(matches!(waited, crate::inbox::Waited::Again));
    let rejected = Deadline::after(DEADLINE)
        .recv(&answer)
        .unwrap()
        .unwrap_err();
    assert_eq!(rejected.code, ErrorCode::StaleRequest);

    let (ack, answer) = answered();
    let waited = world
        .looped
        .take_while_waiting(&pending, Delivery::Close(ack), &turn)
        .unwrap();
    assert!(matches!(waited, crate::inbox::Waited::Again));
    assert!(Deadline::after(DEADLINE).recv(&answer).unwrap().is_ok());
}

/// An `extension_log` taken while the jobs settle is written live and to
/// the diagnostic log, never saved.
#[test]
fn an_extension_log_taken_while_settling_is_written_and_never_saved() {
    let home = fakes::TempDir::new("fiber-shutdown-log");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    let (_inbox, rx) = mpsc::channel::<Delivery>();
    let mut looped = Loop::start(
        crate::Session {
            log: Arc::clone(&log),
            provider: Arc::new(fakes::ScriptedProvider::new(Vec::new())),
            model: Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: crate::prompt::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                clock,
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials: home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(NoRules),
            },
        },
        None,
    )
    .unwrap();
    let mut watched = log.watch();
    looped
        .settle_one(Delivery::ExtensionLog(ExtensionLog {
            extension: "fiber.test/notes".into(),
            message: "hello".into(),
        }))
        .unwrap();
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("extension_log arrives in time")
        .expect("the log outlives the settle")
        .expect("the log ended before extension_log");
    assert_eq!(line.kind, "extension_log");
    assert!(line.seq.is_none(), "extension_log is ephemeral");
    let saved: Vec<String> = log::read(&home.path().join("s_test"))
        .unwrap_or_default()
        .into_iter()
        .map(|line| line.kind)
        .collect();
    assert!(!saved.contains(&"extension_log".to_owned()));
    let diag =
        std::fs::read_to_string(home.path().join("logs").join("session-s_test.log")).unwrap();
    assert!(diag.ends_with("\"message\":\"fiber.test/notes: hello\"}\n"));
}

/// An `extension_log` delivered after `run` has returned is dropped:
/// nothing reads the inbox past the settle, so it reaches neither the
/// watchers nor the diagnostic log. (`run` returning is where the
/// process writes `fiber_exited`, the last line, after which no status
/// follows.)
#[test]
fn an_extension_log_delivered_after_run_returns_is_dropped() {
    let (jobs, _stops) = Listed::new(&[]);
    let world = World::new(jobs);
    world.cancel.shutdown(143);
    let (finished, inbox, held) = world.spawn_run();
    ran(&finished);
    // The inbox has no reader left: the delivery goes nowhere, whether
    // the channel still accepts it or is already disconnected.
    let _dropped = inbox.send(Delivery::ExtensionLog(ExtensionLog {
        extension: "fiber.test/notes".into(),
        message: "late".into(),
    }));
    assert!(
        !held.kinds().contains(&"extension_log".to_owned()),
        "extension_log after run returns is never saved"
    );
    let diag = held
        .dir
        .parent()
        .unwrap()
        .join("logs")
        .join("session-s_test.log");
    let text = std::fs::read_to_string(&diag).unwrap_or_default();
    assert!(
        !text.contains("extension_log"),
        "extension_log after run returns writes no diagnostic line"
    );
}

/// An allow of `pending`, answered on the returned receiver.
fn allow(pending: &contract::RequestId) -> (Delivery, Receiver<Result<(), Rejection>>) {
    let (ack, answer) = answered();
    let reply = contract::commands::Reply {
        request_id: pending.clone(),
        answer: contract::commands::ReplyAnswer::Approval {
            decision: contract::events::Decision::Allow,
            feedback: None,
            remember: None,
        },
    };
    (Delivery::Reply(reply, ack), answer)
}

#[test]
fn a_reply_taken_with_the_signal_live_is_returned_unanswered() {
    let (jobs, _stops) = Listed::new(&[]);
    let mut world = World::new(jobs);
    assert!(world.cancel.arm());
    let pending = contract::RequestId("r_1".into());
    let turn = contract::TurnId("t_1".into());
    let (reply, answer) = allow(&pending);
    let waited = world
        .looped
        .take_while_waiting(&pending, reply, &turn)
        .unwrap();
    assert!(matches!(waited, crate::inbox::Waited::Reply(..)));
    assert!(answer.try_recv().is_err(), "the ack is not yet answered");
}

#[test]
fn a_reply_taken_after_a_cancel_alone_ends_the_wait_cancelled() {
    let (jobs, _stops) = Listed::new(&[]);
    let mut world = World::new(jobs);
    assert!(world.cancel.arm());
    assert!(world.cancel.cancel());
    let pending = contract::RequestId("r_1".into());
    let turn = contract::TurnId("t_1".into());
    let (reply, answer) = allow(&pending);
    let waited = world
        .looped
        .take_while_waiting(&pending, reply, &turn)
        .unwrap();
    assert!(matches!(waited, crate::inbox::Waited::Cancelled));
    let rejected = Deadline::after(DEADLINE)
        .recv(&answer)
        .unwrap()
        .unwrap_err();
    assert_eq!(rejected.code, ErrorCode::StaleRequest);
}

fn asked(id: &str) -> contract::events::InteractionRequested {
    contract::events::InteractionRequested {
        request_id: contract::RequestId(id.into()),
        interaction: contract::events::Interaction::Confirm {
            prompt: "Overwrite?".into(),
        },
        action_ids: None,
        extension: Some("fiber.test/notes".into()),
        resumes: false,
    }
}

fn resolved(id: &str) -> contract::events::InteractionResolved {
    contract::events::InteractionResolved {
        request_id: contract::RequestId(id.into()),
        by: contract::events::ResolvedBy::Person,
        answer: contract::events::Answer::Confirmed { confirmed: true },
    }
}

#[test]
fn settle_writes_an_interaction_and_its_resolution_then_acks() {
    let (jobs, _stops) = Listed::new(&[]);
    let mut world = World::new(jobs);
    world
        .looped
        .settle_one(Delivery::Interaction(asked("r_1")))
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let dir = world.held.dir.clone();
    let ack = Ack(Box::new(move |answer| {
        let seen = log::read(&dir)
            .unwrap_or_default()
            .into_iter()
            .map(|line| line.kind)
            .collect::<Vec<_>>();
        let _sent = tx.send((answer.is_ok(), seen));
    }));
    world
        .looped
        .settle_one(Delivery::Resolved(resolved("r_1"), ack))
        .unwrap();
    let (accepted, seen) = Deadline::after(DEADLINE).recv(&rx).expect("the ack runs");
    assert!(accepted, "resolved accepts its ack");
    assert_eq!(
        seen.last().map(String::as_str),
        Some("interaction_resolved"),
        "settle calls the ack only after its line is written"
    );
    let kinds = world.held.kinds();
    assert_eq!(
        kinds[kinds.len().saturating_sub(2)..],
        ["interaction_requested", "interaction_resolved"],
        "both lines are written in order"
    );
}

fn exec(program: &str) -> contract::events::ExtensionExec {
    contract::events::ExtensionExec {
        extension: "fiber.test/notes".into(),
        program: program.into(),
        args: vec!["status".into()],
        cwd: "/w".into(),
        process: contract::shapes::Process {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
        },
    }
}

#[test]
fn after_rewound_a_shutdown_writes_nothing_and_refuses_what_waits_closing() {
    let (jobs, _stops) = Listed::new(&[]);
    let mut world = World::new(Arc::clone(&jobs) as Arc<dyn Jobs>);
    world
        .looped
        .log
        .append(
            &contract::events::Event::Rewound(contract::events::Rewound {
                new_session_id: SessionId("s_9e2b0000000000b2".into()),
                seq: contract::Seq(3),
                from_session_id: None,
                summary: None,
            }),
            None,
            None,
        )
        .unwrap();
    world.looped.rewound = true;
    world.cancel.shutdown(143);
    // Sent before the run starts, so the settling drain takes each one:
    // nothing is written for any of them.
    let (tx, refused) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer);
    }));
    world
        .inbox
        .send(Delivery::ExtensionExec(exec("git")))
        .unwrap();
    world
        .inbox
        .send(Delivery::Interaction(asked("r_1")))
        .unwrap();
    world
        .inbox
        .send(Delivery::Prompt(message("hello"), ack))
        .unwrap();
    let (finished, _inbox, held) = world.spawn_run();
    let answer = Deadline::after(DEADLINE)
        .recv(&refused)
        .expect("the prompt is refused");
    match answer {
        Err(rejection) => {
            assert_eq!(rejection.code, ErrorCode::Closing);
            assert_eq!(
                rejection.message,
                "The session was rewound and takes no more commands."
            );
        }
        Ok(_) => panic!("the prompt is refused, got {answer:?}"),
    }
    ran(&finished);
    assert_eq!(held.kinds(), ["session_started", "rewound"]);
}

/// A queued notice whose claim answers `holds`, counting its asks in
/// `asked`, with a delegate's finish riding it.
fn queued_notice(holds: bool, asked: &Arc<AtomicUsize>) -> crate::jobs::Queued {
    let notice = JobNotice {
        completed: JobCompleted {
            job_id: JobId("j_1".into()),
            status: Outcome::Cancelled,
            error: None,
            process: None,
            output_tail: None,

            notice: None,
            changed_by: None,
        },
        claim: Claim(Box::new({
            let asked = Arc::clone(asked);
            move || {
                asked.fetch_add(1, Ordering::SeqCst);
                holds.then(Vec::new)
            }
        })),
        delegate: Some(DelegateFinished {
            job_id: JobId("j_1".into()),
            text: "Done.".into(),
            artifact: None,
            questions: None,
            usage: Usage {
                tokens: Tokens {
                    input: 0,
                    cache_read: 0,
                    cache_write: std::collections::BTreeMap::new(),
                    output: 0,
                },
                cost: Some(0.0),
                subscription_cost: 0.0,
            },
            worktree: None,

            changed_by: None,
        }),
    };
    crate::jobs::Queued::Notice(notice)
}

#[test]
fn settle_claims_a_queued_notice_before_writing_it() {
    let (jobs, _stops) = Listed::new(&[]);
    let mut world = World::new(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let asked = Arc::new(AtomicUsize::new(0));
    world.looped.queued.push_back(queued_notice(true, &asked));
    assert_eq!(asked.load(Ordering::SeqCst), 0, "queueing asks nothing");
    world.cancel.shutdown(143);
    let (finished, _inbox, held) = world.spawn_run();
    ran(&finished);
    assert_eq!(
        held.kinds(),
        ["session_started", "delegate_finished", "job_completed"]
    );
    assert_eq!(asked.load(Ordering::SeqCst), 1, "the settle claims once");
    let lines = held.durable();
    assert_eq!(lines[1].payload["job_id"], "j_1");
    assert_eq!(lines[1].payload["text"], "Done.");
    assert_eq!(lines[2].payload["job_id"], "j_1");
}

#[test]
fn settle_writes_nothing_for_a_refused_queued_notice() {
    let (jobs, _stops) = Listed::new(&[]);
    let mut world = World::new(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let asked = Arc::new(AtomicUsize::new(0));
    world.looped.queued.push_back(queued_notice(false, &asked));
    world.cancel.shutdown(143);
    let (finished, _inbox, held) = world.spawn_run();
    ran(&finished);
    assert_eq!(held.kinds(), ["session_started"]);
    assert_eq!(asked.load(Ordering::SeqCst), 1, "the settle claims once");
}

/// A resumed run's start records for `j_1`, queued under `action`: the
/// action of a call the loop never started, so shutdown's settle, which
/// writes with no turn, applies the fallback.
fn resumed_ahead(action: &contract::ActionId) -> Vec<contract::jobs::Taken> {
    use contract::events::{DelegateStarted, JobStarted};
    vec![
        contract::jobs::Taken {
            record: contract::jobs::JobRecord::Started(JobStarted {
                job_id: JobId("j_1".into()),
                tool: Some("delegate_message".into()),
                extension: None,
                description: "resumed".into(),
                output_path: "artifacts/j_1.log".into(),
            }),
            action: Some(action.clone()),
        },
        contract::jobs::Taken {
            record: contract::jobs::JobRecord::DelegateStarted(DelegateStarted {
                job_id: JobId("j_1".into()),
                delegate_session_id: SessionId("s_d000000000000001".into()),
                harness: "fiber".into(),
                model: "fiber:fake/m".into(),
                workspace: "/w".into(),
                worktree: None,
                forked_from: None,
            }),
            action: Some(action.clone()),
        },
    ]
}

/// A queued notice whose claim took `ahead` before the notice's own
/// finish and completion.
fn queued_ahead_notice(
    ahead: Vec<contract::jobs::Taken>,
    asked: &Arc<AtomicUsize>,
) -> crate::jobs::Queued {
    let notice = JobNotice {
        completed: JobCompleted {
            job_id: JobId("j_1".into()),
            status: Outcome::Cancelled,
            error: None,
            process: None,
            output_tail: None,

            notice: None,
            changed_by: None,
        },
        claim: Claim(Box::new({
            let asked = Arc::clone(asked);
            move || {
                asked.fetch_add(1, Ordering::SeqCst);
                Some(ahead.clone())
            }
        })),
        delegate: Some(DelegateFinished {
            job_id: JobId("j_1".into()),
            text: "Done.".into(),
            artifact: None,
            questions: None,
            usage: Usage {
                tokens: Tokens {
                    input: 0,
                    cache_read: 0,
                    cache_write: std::collections::BTreeMap::new(),
                    output: 0,
                },
                cost: Some(0.0),
                subscription_cost: 0.0,
            },
            worktree: None,

            changed_by: None,
        }),
    };
    crate::jobs::Queued::Notice(notice)
}

/// The four lines a settle writes for a notice whose claim took a resumed
/// run's start: the two starts under `action`, then the finish and the
/// completion with no action, all with no turn, as shutdown writes them.
/// `fiber_exited` follows these in production; `run` does not write it.
fn assert_settled_ahead(lines: &[Envelope], action: &contract::ActionId) {
    assert_eq!(lines[1].kind, "job_started");
    assert_eq!(lines[1].action_id.as_ref(), Some(action));
    assert_eq!(lines[1].turn_id, None);
    assert_eq!(lines[2].kind, "delegate_started");
    assert_eq!(lines[2].action_id.as_ref(), Some(action));
    assert_eq!(lines[2].turn_id, None);
    assert_eq!(lines[3].kind, "delegate_finished");
    assert_eq!(lines[3].action_id, None);
    assert_eq!(lines[3].turn_id, None);
    assert_eq!(lines[3].payload["job_id"], "j_1");
    assert_eq!(lines[4].kind, "job_completed");
    assert_eq!(lines[4].action_id, None);
    assert_eq!(lines[4].turn_id, None);
    assert_eq!(lines[4].payload["job_id"], "j_1");
}

#[test]
fn settle_writes_ahead_before_a_queued_notice() {
    let (jobs, _stops) = Listed::new(&[]);
    let mut world = World::new(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let asked = Arc::new(AtomicUsize::new(0));
    let action = contract::ActionId("a_aaaaaaaaaaaaaaaa".into());
    world
        .looped
        .queued
        .push_back(queued_ahead_notice(resumed_ahead(&action), &asked));
    world.cancel.shutdown(143);
    let (finished, _inbox, held) = world.spawn_run();
    ran(&finished);
    assert_eq!(
        held.kinds(),
        [
            "session_started",
            "job_started",
            "delegate_started",
            "delegate_finished",
            "job_completed"
        ]
    );
    assert_eq!(asked.load(Ordering::SeqCst), 1, "the settle claims once");
    assert_settled_ahead(&held.durable(), &action);
}

#[test]
fn settle_writes_ahead_before_a_notice_delivered_while_settling() {
    let (jobs, stops) = Listed::new(&["j_1"]);
    let world = World::new(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let asked = Arc::new(AtomicUsize::new(0));
    let action = contract::ActionId("a_aaaaaaaaaaaaaaaa".into());
    world.cancel.shutdown(143);
    let (finished, inbox, held) = world.spawn_run();
    let wait = Deadline::after(DEADLINE);
    assert_eq!(stopped(&stops, &wait), "j_1");
    // Ends `j_1` with a notice whose claim took a resumed run's start.
    let ahead = resumed_ahead(&action);
    jobs.send_and_forget(
        "j_1",
        &inbox,
        JobNotice {
            completed: JobCompleted {
                job_id: JobId("j_1".into()),
                status: Outcome::Cancelled,
                error: None,
                process: None,
                output_tail: None,

                notice: None,
                changed_by: None,
            },
            claim: Claim(Box::new({
                let asked = Arc::clone(&asked);
                move || {
                    asked.fetch_add(1, Ordering::SeqCst);
                    Some(ahead.clone())
                }
            })),
            delegate: Some(DelegateFinished {
                job_id: JobId("j_1".into()),
                text: "Done.".into(),
                artifact: None,
                questions: None,
                usage: Usage {
                    tokens: Tokens {
                        input: 0,
                        cache_read: 0,
                        cache_write: std::collections::BTreeMap::new(),
                        output: 0,
                    },
                    cost: Some(0.0),
                    subscription_cost: 0.0,
                },
                worktree: None,

                changed_by: None,
            }),
        },
    );
    ran(&finished);
    assert_eq!(
        held.kinds(),
        [
            "session_started",
            "job_started",
            "delegate_started",
            "delegate_finished",
            "job_completed"
        ]
    );
    assert_eq!(asked.load(Ordering::SeqCst), 1, "the settle claims once");
    assert_settled_ahead(&held.durable(), &action);
}

// Shutdowns: no after_tool hook runs while jobs settle or orphans land.

/// Counts the `after_tool` dispatches a shutdown would run: none.
#[derive(Default)]
struct CountingHooks {
    calls: AtomicUsize,
    redact: bool,
    waits: AtomicUsize,
    barrier: Mutex<Option<RunBarrier>>,
    /// Every `after_tool` call's content, in order: a dropped end runs no
    /// delivery hook, so its content never appears here.
    contents: Mutex<Vec<String>>,
}

struct RunBarrier {
    arrived: Sender<()>,
    release: Receiver<()>,
    inbox: Sender<Delivery>,
}

impl CountingHooks {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn contents(&self) -> Vec<String> {
        self.contents.lock().unwrap().clone()
    }
}

impl contract::hook::Hooks for CountingHooks {
    fn wait_for_runs(&self) {
        self.waits.fetch_add(1, Ordering::SeqCst);
        if let Some(barrier) = self.barrier.lock().unwrap().take() {
            barrier.arrived.send(()).unwrap();
            let _released = barrier.release.recv();
            barrier
                .inbox
                .send(Delivery::ExtensionExec(contract::events::ExtensionExec {
                    extension: "test/barrier".into(),
                    program: "sh".into(),
                    args: vec!["-c".into(), "exit 0".into()],
                    cwd: "/tmp".into(),
                    process: contract::shapes::Process {
                        exit_code: Some(0),
                        signal: None,
                        timed_out: false,
                    },
                }))
                .unwrap();
        }
    }

    fn after_tool(
        &self,
        call: &contract::hook::AfterToolCall<'_>,
    ) -> contract::hook::AfterToolAnswer {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.contents.lock().unwrap().push(call.content.to_owned());
        contract::hook::AfterToolAnswer {
            outcome: if self.redact {
                contract::hook::AfterToolOutcome::Changed {
                    content: Some("redacted".into()),
                    details: None,
                    artifact: None,
                }
            } else {
                contract::hook::AfterToolOutcome::Unchanged
            },
            changed_by: if self.redact {
                vec!["redactor".into()]
            } else {
                Vec::new()
            },
            notices: Vec::new(),
        }
    }

    fn deliver_to(&self, _: Sender<Delivery>) {}

    fn before_tool(
        &self,
        _: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::BeforeToolAnswer {
        // Never consulted: a shutdown runs no hook.
        contract::hook::BeforeToolAnswer {
            outcome: contract::hook::BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }

    fn check_tool(&self, _: &contract::hook::BeforeToolCall<'_>) -> contract::hook::CheckAnswer {
        // Never consulted: a shutdown runs no hook.
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn before_message(
        &self,
        _: &contract::hook::BeforeMessage<'_>,
    ) -> contract::hook::BeforeMessageAnswer {
        // Never consulted: a shutdown runs no hook.
        contract::hook::BeforeMessageAnswer {
            outcome: contract::hook::BeforeMessageOutcome::Passed {
                replaced: None,
                context: Vec::new(),
            },
            notices: Vec::new(),
        }
    }

    fn session_start(&self, _: &contract::hook::SessionStart<'_>) -> contract::hook::ContextAnswer {
        // Never consulted: a shutdown runs no hook.
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn turn_start(&self, _: &contract::hook::TurnStart<'_>) -> contract::hook::ContextAnswer {
        // Never consulted: a shutdown runs no hook.
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_model_call(
        &self,
        _: &contract::hook::BeforeModelCall<'_>,
    ) -> contract::hook::CheckAnswer {
        // Never consulted: a shutdown runs no hook.
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn turn_end(&self, _: &contract::hook::TurnEnd<'_>) -> contract::hook::ContextAnswer {
        // Never consulted: a shutdown runs no hook.
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_handoff(
        &self,
        _: &contract::hook::BeforeHandoff<'_>,
    ) -> contract::hook::BeforeHandoffAnswer {
        // Never consulted: a shutdown runs no hook.
        contract::hook::BeforeHandoffAnswer {
            outcome: contract::hook::BeforeHandoffOutcome::NoNote,
            notices: Vec::new(),
        }
    }
}

impl World {
    /// Installs `hooks` on the loop; the test reads their count after.
    fn hooked(mut self, hooks: Arc<CountingHooks>) -> Self {
        let dyned: Arc<dyn contract::hook::Hooks> = hooks;
        self.looped = self.looped.hooks(dyned);
        self
    }
}

#[test]
fn shutdown_settle_waits_for_runs_before_its_final_drain() {
    let (jobs, _stops) = Listed::new(&[]);
    let hooks = Arc::new(CountingHooks::default());
    let mut world = World::new(jobs).hooked(hooks.clone());
    let (arrived, arrival) = mpsc::channel();
    let (release, released) = mpsc::channel();
    *hooks.barrier.lock().unwrap() = Some(RunBarrier {
        arrived,
        release: released,
        inbox: world.inbox.clone(),
    });
    world.cancel.shutdown(143);
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let settled = world.looped.settled(Ok(()));
        let _sent = done.send((settled, world.held));
    });
    Deadline::after(DEADLINE)
        .recv(&arrival)
        .expect("the loop entered the run barrier");
    release.send(()).unwrap();
    let (settled, held) = Deadline::after(DEADLINE)
        .recv(&result)
        .expect("settle returned");
    settled.unwrap();
    assert_eq!(hooks.waits.load(Ordering::SeqCst), 1);
    assert_eq!(held.kinds(), ["session_started", "extension_exec"]);
    let lines = held.durable();
    assert_eq!(lines[1].payload["program"], "sh");
    assert_eq!(lines[1].payload["process"]["exit_code"], 0);
}

#[test]
fn normal_settle_does_not_wait_for_runs() {
    let (jobs, _stops) = Listed::new(&[]);
    let hooks = Arc::new(CountingHooks::default());
    let mut world = World::new(jobs).hooked(hooks.clone());
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let settled = world.looped.settled(Ok(()));
        let _sent = done.send((settled, world.held));
    });
    let (settled, held) = Deadline::after(DEADLINE)
        .recv(&result)
        .expect("normal settle returned");
    settled.unwrap();
    assert_eq!(hooks.waits.load(Ordering::SeqCst), 0);
    assert_eq!(held.kinds(), ["session_started"]);
}

#[test]
fn shutdown_settle_without_hooks_returns() {
    let (jobs, _stops) = Listed::new(&[]);
    let mut world = World::new(jobs);
    world.cancel.shutdown(143);
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let settled = world.looped.settled(Ok(()));
        let _sent = done.send((settled, world.held));
    });
    let (settled, held) = Deadline::after(DEADLINE)
        .recv(&result)
        .expect("unhooked settle returned");
    settled.unwrap();
    assert_eq!(held.kinds(), ["session_started"]);
}

#[test]
fn settle_under_shutdown_runs_no_hook_and_writes_its_end_raw() {
    let (jobs, _stops) = Listed::new(&[]);
    let hooks = Arc::new(CountingHooks::default());
    let mut world = World::new(Arc::clone(&jobs) as Arc<dyn Jobs>).hooked(Arc::clone(&hooks));
    world.looped.queued.push_back(crate::jobs::Queued::Job(
        Vec::new(),
        JobCompleted {
            job_id: JobId("j_1".into()),
            status: Outcome::Cancelled,
            error: None,
            process: None,
            output_tail: None,

            notice: None,
            changed_by: None,
        },
        Some(DelegateFinished {
            job_id: JobId("j_1".into()),
            text: "Done.".into(),
            artifact: None,
            questions: None,
            usage: Usage {
                tokens: Tokens {
                    input: 0,
                    cache_read: 0,
                    cache_write: std::collections::BTreeMap::new(),
                    output: 0,
                },
                cost: Some(0.0),
                subscription_cost: 0.0,
            },
            worktree: None,

            changed_by: None,
        }),
    ));
    world.cancel.shutdown(143);
    let (finished, _inbox, held) = world.spawn_run();
    ran(&finished);
    assert_eq!(hooks.calls(), 0);
    assert_eq!(
        held.kinds(),
        ["session_started", "delegate_finished", "job_completed"]
    );
    let lines = held.durable();
    assert_eq!(lines[1].payload["text"], "Done.");
    assert_eq!(lines[2].payload["status"], "cancelled");
}

#[test]
fn mark_orphans_runs_no_hook_and_writes_the_end_raw() {
    let (jobs, _stops) = Listed::new(&[]);
    let hooks = Arc::new(CountingHooks::default());
    let mut world = World::new(Arc::clone(&jobs) as Arc<dyn Jobs>).hooked(Arc::clone(&hooks));
    world
        .looped
        .mark_orphans(vec![JobCompleted {
            job_id: JobId("j_1".into()),
            status: Outcome::Failed,
            error: None,
            process: None,
            output_tail: Some("1 failing\n".into()),

            notice: None,
            changed_by: None,
        }])
        .unwrap();
    assert_eq!(hooks.calls(), 0);
    let lines = held_of(&world);
    assert_eq!(lines[lines.len() - 1].kind, "job_completed");
    assert_eq!(lines[lines.len() - 1].payload["output_tail"], "1 failing\n");
    assert!(lines[lines.len() - 1].payload.get("notice").is_none());
}

/// The session file's lines for a loop that never ran.
fn held_of(world: &World) -> Vec<Envelope> {
    log::read(&world.held.dir).unwrap()
}

#[test]
fn a_normal_end_closes_extension_starts_before_its_first_running_read() {
    let (jobs, stops) = Listed::new(&["j_extension"]);
    let mut world = World::new(jobs.clone());
    world
        .looped
        .write(
            &contract::events::Event::JobStarted(contract::events::JobStarted {
                job_id: JobId("j_extension".into()),
                tool: None,
                extension: Some("test/scan".into()),
                description: "scan".into(),
                output_path: "/child/events.jsonl".into(),
            }),
            None,
            None,
        )
        .unwrap();
    let (done, finished) = mpsc::channel();
    let inbox = world.inbox.clone();
    std::thread::spawn(move || {
        done.send(world.looped.settled(Ok(()))).unwrap();
    });
    let wait = Deadline::after(DEADLINE);
    assert_eq!(stopped(&stops, &wait), "j_extension");
    jobs.end_delegate("j_extension", &inbox, "PRIVATE SENTINEL");
    ran(&finished);
    // The close runs before the sweep's first `running()` read, not only
    // before the stop: the log starts with the close, then the read.
    let order = jobs.order();
    assert!(
        order.len() >= 2 && order[0] == "close" && order[1] == "running",
        "{order:?}"
    );
}

#[test]
fn a_start_held_past_the_normal_end_sweep_is_refused_closed() {
    let (jobs, _stops) = Listed::new(&[]);
    let mut world = World::new(jobs.clone());
    let (reached_tx, reached_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    jobs.hold_start(reached_tx, release_rx);
    let (result_tx, result_rx) = mpsc::channel();
    let starter = Arc::clone(&jobs);
    std::thread::spawn(move || {
        let _sent = result_tx.send(starter.start("j_new"));
    });
    let wait = Deadline::after(DEADLINE);
    wait.recv(&reached_rx)
        .expect("the start holds at the pause point");
    // No job runs, so the sweep returns at once, after the close.
    world.looped.settled(Ok(())).unwrap();
    drop(release_tx);
    let Err((code, message)) = wait.recv(&result_rx).expect("the start answered") else {
        panic!("the held start is refused");
    };
    assert_eq!(code, ErrorCode::Closing);
    assert_eq!(
        message,
        "The session is ending, so no delegate was started."
    );
    let order = jobs.order();
    assert!(
        order.len() >= 2 && order[0] == "close" && order[1] == "running",
        "{order:?}"
    );
    assert!(jobs.bound().is_empty(), "no bind was recorded");
    assert_eq!(jobs.launched(), 0, "nothing launched");
    assert!(jobs.running().is_empty(), "no job was listed");
}

#[test]
fn a_start_held_past_the_shutdown_sweep_is_refused_closed() {
    let (jobs, _stops) = Listed::new(&[]);
    let mut world = World::new(jobs.clone());
    world.cancel.shutdown(143);
    let (reached_tx, reached_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    jobs.hold_start(reached_tx, release_rx);
    let (result_tx, result_rx) = mpsc::channel();
    let starter = Arc::clone(&jobs);
    std::thread::spawn(move || {
        let _sent = result_tx.send(starter.start("j_new"));
    });
    let wait = Deadline::after(DEADLINE);
    wait.recv(&reached_rx)
        .expect("the start holds at the pause point");
    // The shutdown path closes before `settle`'s first `running()` read.
    world.looped.settled(Ok(())).unwrap();
    drop(release_tx);
    let Err((code, message)) = wait.recv(&result_rx).expect("the start answered") else {
        panic!("the held start is refused");
    };
    assert_eq!(code, ErrorCode::Closing);
    assert_eq!(
        message,
        "The session is ending, so no delegate was started."
    );
    let order = jobs.order();
    assert!(
        order.len() >= 2 && order[0] == "close" && order[1] == "running",
        "{order:?}"
    );
    assert!(jobs.bound().is_empty(), "no bind was recorded");
    assert_eq!(jobs.launched(), 0, "nothing launched");
    assert!(jobs.running().is_empty(), "no job was listed");
}
