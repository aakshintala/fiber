//! The loop's part of a shutdown: every job is stopped and each end is
//! written before `run` returns; nothing else starts.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use contract::clock::Clock as _;
use contract::events::{ExtensionLog, JobCompleted, JobLine, Outcome};
use contract::inbox::{Ack, Claim, Delivery, JobNotice, Message, Rejection};
use contract::jobs::{Foreground, Jobs, OpenError, Opened, Opening};
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, Origin, Sender as From};
use contract::{CommandId, Envelope, ErrorCode, JobId, SessionId};
use fakes::clock::FakeClock;
use log::Log;

use crate::{Loop, Model, TurnCancel};

/// How long a test waits on the loop before it fails.
const DEADLINE: Duration = Duration::from_secs(10);

/// The idle delay a test sets, on the fake clock.
const IDLE: Duration = Duration::from_secs(60);

/// Rules that hold nothing.
struct NoRules;

impl Rules for NoRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules::default())
    }

    fn remember(&self, _: &str, _: &str, _: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

/// Jobs a test lists by hand. Each stop sent is reported on `stops`. An
/// end is sent under the lock `running` reads, as the registry sends it.
struct Listed {
    ids: Mutex<Vec<JobId>>,
    stops: Mutex<Sender<JobId>>,
}

impl Listed {
    fn new(ids: &[&str]) -> (Arc<Self>, Receiver<JobId>) {
        let (stops, stopped) = mpsc::channel();
        let listed = Arc::new(Self {
            ids: Mutex::new(ids.iter().map(|id| JobId((*id).into())).collect()),
            stops: Mutex::new(stops),
        });
        (listed, stopped)
    }

    /// Lists `id` as running.
    fn add(&self, id: &str) {
        self.ids.lock().unwrap().push(JobId(id.into()));
    }

    /// Ends `id`: its notice reaches `inbox`, then `running` drops it.
    fn end(&self, id: &str, inbox: &Sender<Delivery>) {
        let mut ids = self.ids.lock().unwrap();
        inbox.send(notice(id)).unwrap();
        ids.retain(|listed| listed.0 != id);
    }
}

impl Jobs for Listed {
    fn open(&self, _: Opening) -> Result<Opened, OpenError> {
        Err(OpenError::Io {
            path: "unused".into(),
            source: std::io::Error::other("a listed job is not opened"),
        })
    }

    fn stop(&self, job_id: &JobId) -> bool {
        let _sent = self.stops.lock().unwrap().send(job_id.clone());
        true
    }

    fn background(&self) -> usize {
        0
    }

    fn foreground(&self, _: Foreground) {}

    fn running(&self) -> Vec<JobId> {
        self.ids.lock().unwrap().clone()
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
        },
        claim: Claim(Box::new(|| true)),
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
            log,
            Arc::new(fakes::ScriptedProvider::new(Vec::new())),
            Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            crate::prompt::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                clock,
            ),
            rx,
            Vec::new(),
            crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials: home.path().join("credentials"),
                rules: Arc::new(NoRules),
            },
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

fn ran(finished: &Receiver<Result<(), crate::Error>>) {
    let result = finished.recv_timeout(DEADLINE).expect("run returned");
    assert!(result.is_ok(), "{result:?}");
}

fn stopped(stops: &Receiver<JobId>) -> String {
    stops.recv_timeout(DEADLINE).expect("a stop was sent").0
}

#[test]
fn a_shutdown_stops_a_running_job_and_writes_its_end_before_run_returns() {
    let (jobs, stops) = Listed::new(&["j_1"]);
    let world = World::new(Arc::clone(&jobs) as Arc<dyn Jobs>);
    world.cancel.shutdown(143);
    let (finished, inbox, held) = world.spawn_run();

    assert_eq!(stopped(&stops), "j_1");
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
fn the_stop_is_sent_again_to_a_job_listed_after_the_first() {
    let (jobs, stops) = Listed::new(&["j_1"]);
    let world = World::new(Arc::clone(&jobs) as Arc<dyn Jobs>);
    world.cancel.shutdown(130);
    let (finished, inbox, held) = world.spawn_run();

    assert_eq!(stopped(&stops), "j_1");
    // A call moves to the background as the first job ends.
    jobs.add("j_2");
    jobs.end("j_1", &inbox);
    while stopped(&stops) != "j_2" {}
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
    let (close, close_answer) = answered();
    world.cancel.shutdown(129);
    let (finished, inbox, held) = world.spawn_run();
    assert_eq!(stopped(&stops), "j_1");

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
        .send(Delivery::JobLine(JobLine {
            job_id: JobId("j_1".into()),
            lines: "tick\n".into(),
            suppressed: None,
        }))
        .unwrap();
    jobs.end("j_1", &inbox);
    ran(&finished);

    let answer = |rx: &Receiver<Result<(), Rejection>>| rx.recv_timeout(DEADLINE).unwrap();
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

    let rejected = prompt_answer.recv_timeout(DEADLINE).unwrap().unwrap_err();
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
        clock.await_parked(until, DEADLINE),
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
    let rejected = answer.recv_timeout(DEADLINE).unwrap().unwrap_err();
    assert_eq!(rejected.code, ErrorCode::StaleRequest);

    let (ack, answer) = answered();
    let waited = world
        .looped
        .take_while_waiting(&pending, Delivery::Close(ack), &turn)
        .unwrap();
    assert!(matches!(waited, crate::inbox::Waited::Again));
    assert!(answer.recv_timeout(DEADLINE).unwrap().is_ok());
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
        Arc::clone(&log),
        Arc::new(fakes::ScriptedProvider::new(Vec::new())),
        Model {
            reference: "fake/model".into(),
            cost: None,
            subscription: false,
        },
        crate::prompt::PromptInputs::new(
            home.path().to_path_buf(),
            "/bin/sh".into(),
            home.path()
                .join("s_test/events.jsonl")
                .display()
                .to_string(),
            clock,
        ),
        rx,
        Vec::new(),
        crate::Permissions {
            workspace: workspace.display().to_string(),
            credentials: home.path().join("credentials"),
            rules: Arc::new(NoRules),
        },
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
    let rejected = answer.recv_timeout(DEADLINE).unwrap().unwrap_err();
    assert_eq!(rejected.code, ErrorCode::StaleRequest);
}
