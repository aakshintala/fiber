//! A rewind stops every running job before `rewound`
//! (`docs/events.md`, "Rewind"): the stops, the 5 s bound on the injected
//! clock, and what the wait refuses or drops.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

extern crate support as ext_support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock as _;
use contract::commands::RewindArgs;
use contract::events::{
    CommandResult, Event, JobCompleted, JobStarted, Outcome, Parent, SessionStarted, TurnOutcome,
    Variables, VariablesSource,
};
use contract::inbox::{Ack, Answer, Claim, Delivery, JobNotice, Rejection};
use contract::jobs::{Foreground, Jobs, OpenError, Opened, Opening};
use contract::rules::{Rule, RuleDecision, StandingRules};
use contract::shapes::{Failure, Point};
use contract::tool::Tool;
use contract::{ErrorCode, JobId, Seq, SessionId};
use fakes::Deadline;
use fakes::{Scripted, ScriptedProvider};
use log::Log;
use r#loop::{Loop, Model, Permissions, PromptInputs, rewind_note_with_jobs};
use serde_json::json;

use support::{DEADLINE, MODEL, Session, TestTool, calls_reply, delivery, message, paris};

const A_ID: &str = "s_aaaaaaaaaaaaaaaa";
const B_ID2: &str = "s_bbbbbbbbbbbbbbbb";

/// The stop wait's bound: the shutdown's per-process bound is 5 seconds, so
/// a parked stop wait names `origin + 5 s` as its instant.
const BOUND: Duration = Duration::from_secs(5);

/// The refusal a delivery refused after `rewound` carries: the session takes
/// no more commands.
const CLOSING: &str = "The session was rewound and takes no more commands.";

fn write_tool() -> Arc<TestTool> {
    Arc::new(TestTool::declaring(
        "write_file",
        "Wrote it.",
        vec![contract::shapes::Effect::Writes],
        Some(vec!["/w/note.txt".into()]),
    ))
}

fn exec_tool() -> Arc<TestTool> {
    Arc::new(TestTool::declaring(
        "run_cmd",
        "Ran it.",
        vec![contract::shapes::Effect::Executes],
        None,
    ))
}

fn allow_rule(tool: &str) -> Rule {
    // A standing allow, so the call runs without asking.
    Rule {
        decision: RuleDecision::Allow,
        tool: tool.into(),
        prefix: String::new(),
        added: None,
        session_id: None,
    }
}

/// Session A after two turns: turn 1 answers "one-done", turn 2 calls both
/// tools, then answers "two-done".
fn run_a_with_id(id: &str) -> Session {
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::clone(&write_tool()) as Arc<dyn Tool>,
        Arc::clone(&exec_tool()) as Arc<dyn Tool>,
    ];
    let mut session = Session::with_tools_and_id(
        vec![
            Scripted::text("one-done"),
            calls_reply("working", &[("write_file", paris()), ("run_cmd", paris())]),
            Scripted::text("two-done"),
        ],
        None,
        tools,
        SessionId(id.into()),
    );
    session.inbox.send(delivery("one")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.rules.set(StandingRules {
        global: vec![allow_rule("write_file"), allow_rule("run_cmd")],
        project: Vec::new(),
    });
    session.inbox.send(delivery("two")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session
}

fn args(from: Option<&str>, seq: Option<u64>, summarise: bool) -> RewindArgs {
    RewindArgs {
        from_session_id: from.map(|id| SessionId(id.into())),
        seq: seq.map(Seq),
        summarise,
    }
}

#[track_caller]
fn answer_of(rx: mpsc::Receiver<Answer>) -> Answer {
    Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the rewind is answered in time")
}

fn accepted(answer: Answer) -> SessionId {
    match answer {
        Ok(Some(CommandResult::Rewind { new_session_id })) => new_session_id,
        other => panic!("the rewind is accepted, got {other:?}"),
    }
}

fn rejected(answer: Answer) -> Rejection {
    match answer {
        Err(rejection) => rejection,
        other => panic!("the rewind is refused, got {other:?}"),
    }
}

#[track_caller]
fn log_bytes(dir: &std::path::Path) -> Vec<u8> {
    std::fs::read(dir.join("events.jsonl")).unwrap()
}

/// Sends `delivery` to an idle `looped` and runs its turn on a thread,
/// failing at one named deadline instead of hanging.
fn drive(
    mut looped: Loop,
    tx: &mpsc::Sender<Delivery>,
    delivery: Delivery,
) -> (Loop, Option<TurnOutcome>) {
    tx.send(delivery).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        done.send((looped, outcome)).unwrap();
    });
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the wait ended in time")
}

/// Sends `batch` to an idle `looped` in order and ends the wait: every
/// refusal answers while the wait goes on, and `sender` (the only live
/// inbox sender) is dropped to end it with no turn.
fn drive_closed(mut looped: Loop, sender: mpsc::Sender<Delivery>, batch: Vec<Delivery>) -> Loop {
    for delivery in batch {
        sender.send(delivery).unwrap();
    }
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        done.send((looped, outcome)).unwrap();
    });
    drop(sender);
    let (looped, outcome) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the wait ended in time");
    assert_eq!(outcome, None, "no turn starts");
    looped
}

/// Takes the session's inbox sender, leaving a dead one behind: dropping
/// the taken sender ends a driven wait, while the session stays alive for
/// its log and home.
fn take_inbox(session: &mut Session) -> mpsc::Sender<Delivery> {
    std::mem::replace(&mut session.inbox, mpsc::channel().0)
}

fn resume_b(
    session: &Session,
    id: &str,
    from: Option<Point>,
    parent: Option<Parent>,
) -> (Loop, mpsc::Sender<Delivery>, PathBuf) {
    let home = session.dir.parent().unwrap().to_path_buf();
    let clock = session.clock.clone();
    let log = Arc::new(Log::create(&home, SessionId(id.into()), clock.clone()).unwrap());
    let dir = log.dir().to_path_buf();
    log.append(
        &contract::events::Event::SessionStarted(SessionStarted {
            workspace: session.workspace.display().to_string(),
            variables: Variables {
                path: "/usr/bin:/bin".to_owned(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent,
            forked_from: from,
            rewind: None,
            worktree: None,
        }),
        None,
        None,
    )
    .unwrap();
    r#loop::fiber_started(&log, "0.0.1", true).unwrap();
    let folded = r#loop::resumed(&dir).unwrap();
    let (tx, rx) = mpsc::channel();
    let clock: Arc<dyn contract::clock::Clock> = clock;
    let mut prompt = PromptInputs::new(
        home,
        "/bin/sh".into(),
        dir.join("events.jsonl").display().to_string(),
        clock,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let rules: Arc<dyn contract::rules::Rules> = session.rules.clone();
    let looped = Loop::resume(
        r#loop::Session {
            log,
            provider: Arc::new(ScriptedProvider::new(Vec::new())),
            model: Model {
                reference: MODEL.into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: Vec::new(),
            permissions: Permissions {
                workspace: session.workspace.display().to_string(),
                credentials: session.credentials.clone(),
                credential_files: Vec::new(),
                rules,
            },
        },
        folded,
    )
    .unwrap();
    (looped, tx, dir)
}

fn run_chained_a() -> (Session, u64, u64, u64) {
    let session = run_a_with_id(A_ID);
    let lines = log::read(&session.dir).unwrap();
    let mut starts = lines
        .iter()
        .filter(|line| line.kind == "turn_started")
        .map(|line| line.seq.unwrap().0);
    let first = starts.next().unwrap();
    let second = starts.next().unwrap();
    (session, first, second, second - 1)
}

/// Jobs a test lists by hand. Each stop sent is reported on `stops`. An
/// end is sent under the lock `running` reads, as the registry sends it.
struct FakeJobs {
    ids: Mutex<Vec<JobId>>,
    stops: Mutex<mpsc::Sender<JobId>>,
}

impl FakeJobs {
    fn new(ids: &[&str]) -> (Arc<Self>, mpsc::Receiver<JobId>) {
        let (tx, rx) = mpsc::channel();
        let jobs = Arc::new(Self {
            ids: Mutex::new(ids.iter().map(|id| JobId((*id).into())).collect()),
            stops: Mutex::new(tx),
        });
        (jobs, rx)
    }

    /// Ends `id` as `status`: its notice reaches `inbox`, then `running`
    /// drops it.
    fn end(
        &self,
        inbox: &mpsc::Sender<Delivery>,
        id: &str,
        status: Outcome,
        error: Option<Failure>,
    ) {
        let mut ids = self.ids.lock().unwrap();
        inbox
            .send(Delivery::Job(JobNotice {
                completed: JobCompleted {
                    job_id: JobId(id.into()),
                    status,
                    error,
                    process: None,
                    output_tail: None,
                },
                claim: Claim(Box::new(|| Some(Vec::new()))),
                delegate: None,
            }))
            .unwrap();
        ids.retain(|listed| listed.0 != id);
    }
}

impl Jobs for FakeJobs {
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
        self.ids.lock().unwrap().clone()
    }

    fn deliver_to(&self, _: mpsc::Sender<Delivery>) {}
}

/// An acknowledgement answering on the returned channel: the answer
/// arrives with one named deadline, never a hang.
fn asked() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (
        Ack(Box::new(move |answer| {
            let _sent = tx.send(answer);
        })),
        rx,
    )
}

/// A `rewind` on the loop's inbox whose answer first checks the log's last
/// line is `rewound`: the answer is not sent until `rewound` is written.
fn rewind_checked(dir: &std::path::Path) -> (Delivery, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    let dir = dir.to_path_buf();
    (
        Delivery::Rewind(
            args(None, None, false),
            Ack(Box::new(move |answer| {
                let last = log::read(&dir).unwrap().pop().unwrap();
                assert_eq!(last.kind, "rewound");
                let _sent = tx.send(answer);
            })),
        ),
        rx,
    )
}

#[test]
fn a_rewind_with_no_job_running_writes_only_rewound() {
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    let (jobs, _stops) = FakeJobs::new(&[]);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let (rw, answered) = support::rewind(args(None, None, false));
    let (_looped, outcome) = drive(looped, &session.inbox, rw);
    accepted(answer_of(answered));
    assert_eq!(outcome, None);
    let lines = log::read(&dir).unwrap();
    let tail: Vec<&str> = lines
        .iter()
        .rev()
        .take(2)
        .map(|line| line.kind.as_str())
        .collect();
    assert_eq!(tail, ["rewound", "turn_completed"]);
    assert!(
        lines.iter().all(|line| line.kind != "job_completed"),
        "no job end is written"
    );
}

#[test]
fn a_rewind_stops_two_running_jobs_before_rewound() {
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    let (jobs, stops) = FakeJobs::new(&["j_1", "j_2"]);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let (rw, answered) = rewind_checked(&dir);
    let inbox = session.inbox.clone();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send((outcome, ())).unwrap();
    });
    inbox.send(rw).unwrap();
    // Both jobs receive a stop.
    let mut stopped: Vec<String> = Vec::new();
    for _ in 0..2 {
        let id = Deadline::after(DEADLINE)
            .recv(&stops)
            .expect("the job is stopped");
        stopped.push(id.0);
    }
    stopped.sort();
    assert_eq!(stopped, ["j_1", "j_2"]);
    jobs.end(&inbox, "j_1", Outcome::Cancelled, None);
    jobs.end(&inbox, "j_2", Outcome::Cancelled, None);
    let (outcome, ()) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the rewind ended in time");
    assert_eq!(outcome, None);
    accepted(answer_of(answered));
    let lines = log::read(&dir).unwrap();
    let tail: Vec<&str> = lines
        .iter()
        .rev()
        .take(3)
        .map(|line| line.kind.as_str())
        .collect();
    assert_eq!(tail, ["rewound", "job_completed", "job_completed"]);
    assert_eq!(lines[lines.len() - 3].payload["job_id"], json!("j_1"));
    assert_eq!(lines[lines.len() - 2].payload["job_id"], json!("j_2"));
}

#[test]
fn a_job_ending_indeterminate_is_written_verbatim_before_rewound() {
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    let (jobs, stops) = FakeJobs::new(&["j_1", "j_2"]);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let (rw, answered) = support::rewind(args(None, None, false));
    let inbox = session.inbox.clone();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    inbox.send(rw).unwrap();
    for _ in 0..2 {
        Deadline::after(DEADLINE)
            .recv(&stops)
            .expect("the job is stopped");
    }
    jobs.end(
        &inbox,
        "j_1",
        Outcome::Failed,
        Some(Failure {
            code: ErrorCode::Indeterminate,
            message: "the job did not stop in time".into(),
            retry_after_ms: None,
            provider: None,
        }),
    );
    jobs.end(&inbox, "j_2", Outcome::Cancelled, None);
    let outcome = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the rewind ended in time");
    assert_eq!(outcome, None);
    accepted(answer_of(answered));
    let lines = log::read(&dir).unwrap();
    let tail: Vec<&str> = lines
        .iter()
        .rev()
        .take(3)
        .map(|line| line.kind.as_str())
        .collect();
    assert_eq!(tail, ["rewound", "job_completed", "job_completed"]);
    assert_eq!(lines[lines.len() - 3].payload["job_id"], json!("j_1"));
    assert_eq!(lines[lines.len() - 3].payload["status"], json!("failed"));
    assert_eq!(
        lines[lines.len() - 3].payload["error"]["code"],
        json!("indeterminate")
    );
    assert_eq!(lines[lines.len() - 2].payload["job_id"], json!("j_2"));
}

#[test]
fn a_job_that_never_ends_waits_out_the_bound_then_rewinds() {
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    let origin = session.clock.now();
    let (jobs, stops) = FakeJobs::new(&["j_1"]);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let (rw, answered) = support::rewind(args(None, None, false));
    let inbox = session.inbox.clone();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    inbox.send(rw).unwrap();
    Deadline::after(DEADLINE)
        .recv(&stops)
        .expect("the job is stopped");
    // The loop parks on the bound: advancing to it is the only way the
    // wait ends without a job end.
    session.clock.advance_when_parked(
        Some(origin + BOUND),
        None,
        BOUND,
        &fakes::Deadline::after(DEADLINE),
    );
    assert!(
        log::read(&dir)
            .unwrap()
            .iter()
            .all(|line| line.kind != "rewound"),
        "no rewound is written before the bound"
    );
    // A clock move alone never wakes the inbox wait: a wake makes it look
    // again, find the bound passed, and end the wait.
    inbox.send(Delivery::Cancelled).unwrap();
    let outcome = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the rewind ended in time");
    assert_eq!(outcome, None);
    accepted(answer_of(answered));
    let lines = log::read(&dir).unwrap();
    assert_eq!(lines.last().unwrap().kind, "rewound");
    assert!(
        lines.iter().all(|line| line
            .payload
            .get("job_id")
            .map(|id| id != "j_1")
            .unwrap_or(true)
            || line.kind != "job_completed"),
        "no job end is written for the job that never ended"
    );
}

#[test]
fn a_job_ending_while_parked_on_the_bound_is_written_then_rewound() {
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    let origin = session.clock.now();
    let (jobs, stops) = FakeJobs::new(&["j_1"]);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let (rw, answered) = support::rewind(args(None, None, false));
    let inbox = session.inbox.clone();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    inbox.send(rw).unwrap();
    Deadline::after(DEADLINE)
        .recv(&stops)
        .expect("the job is stopped");
    assert!(
        session.clock.await_parked(origin + BOUND, DEADLINE),
        "the stop wait parked on the bound"
    );
    // The end arrives while parked, before any advance: it is still read.
    jobs.end(&inbox, "j_1", Outcome::Cancelled, None);
    let outcome = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the rewind ended in time");
    assert_eq!(outcome, None);
    accepted(answer_of(answered));
    let lines = log::read(&dir).unwrap();
    let tail: Vec<&str> = lines
        .iter()
        .rev()
        .take(2)
        .map(|line| line.kind.as_str())
        .collect();
    assert_eq!(tail, ["rewound", "job_completed"]);
    assert_eq!(lines[lines.len() - 2].payload["job_id"], json!("j_1"));
}

#[test]
fn deliveries_during_the_stop_wait_are_refused_closing_or_dropped() {
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    let before = log::read(&dir).unwrap().len();
    let (jobs, stops) = FakeJobs::new(&["j_1"]);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let (rw, answered) = support::rewind(args(None, None, false));
    let inbox = session.inbox.clone();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    inbox.send(rw).unwrap();
    Deadline::after(DEADLINE)
        .recv(&stops)
        .expect("the job is stopped");
    // A prompt, a second rewind and a close are each refused closing; a
    // job notice whose claim does not hold writes no line.
    let (prompt_ack, prompt_answered) = asked();
    inbox
        .send(Delivery::Prompt(message("later"), prompt_ack))
        .unwrap();
    let (rw2, rw2_answered) = support::rewind(args(None, None, false));
    inbox.send(rw2).unwrap();
    let (close_ack, close_answered) = asked();
    inbox.send(Delivery::Close(close_ack)).unwrap();
    inbox
        .send(Delivery::Job(JobNotice {
            completed: JobCompleted {
                job_id: JobId("j_ghost".into()),
                status: Outcome::Cancelled,
                error: None,
                process: None,
                output_tail: None,
            },
            claim: Claim(Box::new(|| None)),
            delegate: None,
        }))
        .unwrap();
    for answered in [prompt_answered, rw2_answered, close_answered] {
        let rejection = rejected(answer_of(answered));
        assert_eq!(rejection.code, ErrorCode::Closing);
        assert_eq!(rejection.message, CLOSING);
    }
    jobs.end(&inbox, "j_1", Outcome::Cancelled, None);
    let outcome = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the rewind ended in time");
    assert_eq!(outcome, None);
    accepted(answer_of(answered));
    // Only the job's end and `rewound` were written.
    let after = log::read(&dir).unwrap();
    assert_eq!(after.len(), before + 2);
    assert_eq!(after[after.len() - 2].kind, "job_completed");
    assert_eq!(after.last().unwrap().kind, "rewound");
}

#[test]
fn a_monitor_batch_during_the_stop_wait_is_dropped() {
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    let (jobs, stops) = FakeJobs::new(&["j_1"]);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let (rw, answered) = support::rewind(args(None, None, false));
    let inbox = session.inbox.clone();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    inbox.send(rw).unwrap();
    Deadline::after(DEADLINE)
        .recv(&stops)
        .expect("the job is stopped");
    inbox
        .send(Delivery::JobLine(contract::events::JobLine {
            job_id: JobId("j_1".into()),
            lines: "still running\n".into(),
            suppressed: None,
        }))
        .unwrap();
    jobs.end(&inbox, "j_1", Outcome::Cancelled, None);
    let outcome = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the rewind ended in time");
    assert_eq!(outcome, None);
    accepted(answer_of(answered));
    let lines = log::read(&dir).unwrap();
    assert!(
        lines.iter().all(|line| line.kind != "job_line"),
        "no monitor batch is written"
    );
    assert_eq!(lines.last().unwrap().kind, "rewound");
}

#[test]
fn a_rewind_to_an_ancestor_point_stops_the_jobs_in_the_old_log() {
    let (session, first, _, at) = run_chained_a();
    let (looped, tx, dir) = resume_b(
        &session,
        B_ID2,
        Some(Point {
            session_id: SessionId(A_ID.into()),
            seq: Seq(at),
        }),
        None,
    );
    let (jobs, stops) = FakeJobs::new(&["j_1"]);
    let looped = looped.jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let (rw, answered) = support::rewind(args(Some(A_ID), Some(first - 1), false));
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    tx.send(rw).unwrap();
    Deadline::after(DEADLINE)
        .recv(&stops)
        .expect("the job is stopped");
    jobs.end(&tx, "j_1", Outcome::Cancelled, None);
    let outcome = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the rewind ended in time");
    assert_eq!(outcome, None);
    accepted(answer_of(answered));
    // The stop lines land in the rewound session's own log, before
    // `rewound`, even though the point counts in the ancestor's.
    let lines = log::read(&dir).unwrap();
    let tail: Vec<&str> = lines
        .iter()
        .rev()
        .take(2)
        .map(|line| line.kind.as_str())
        .collect();
    assert_eq!(tail, ["rewound", "job_completed"]);
    assert_eq!(lines[lines.len() - 2].payload["job_id"], json!("j_1"));
    assert_eq!(lines.last().unwrap().payload["seq"], json!(first - 1));
}

#[test]
fn refusals_send_no_stop_with_jobs_running() {
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    let (jobs, stops) = FakeJobs::new(&["j_1"]);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let before = log_bytes(&dir);
    let (rw, answered) = support::rewind(args(None, None, true));
    let inbox = take_inbox(&mut session);
    drop(drive_closed(looped, inbox, vec![rw]));
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::SummaryFailed);
    assert_eq!(log_bytes(&dir), before);
    assert!(stops.try_recv().is_err(), "no stop was sent");
    // A bad point is refused the same way, with the jobs still running.
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    let (jobs, stops) = FakeJobs::new(&["j_1"]);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let before = log_bytes(&dir);
    let (rw, answered) = support::rewind(args(None, Some(u64::MAX), false));
    let inbox = take_inbox(&mut session);
    drop(drive_closed(looped, inbox, vec![rw]));
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::NotStepBoundary);
    assert_eq!(log_bytes(&dir), before);
    assert!(stops.try_recv().is_err(), "no stop was sent");
}

#[test]
fn a_job_that_ended_just_before_the_rewind_is_written_first() {
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    let (jobs, _stops) = FakeJobs::new(&["j_1"]);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    // The end is already waiting in the inbox while `running` is empty.
    jobs.end(&session.inbox, "j_1", Outcome::Cancelled, None);
    let (rw, answered) = support::rewind(args(None, None, false));
    let (_looped, outcome) = drive(looped, &session.inbox, rw);
    accepted(answer_of(answered));
    assert_eq!(outcome, None);
    let lines = log::read(&dir).unwrap();
    let tail: Vec<&str> = lines
        .iter()
        .rev()
        .take(2)
        .map(|line| line.kind.as_str())
        .collect();
    assert_eq!(tail, ["rewound", "job_completed"]);
    assert_eq!(lines[lines.len() - 2].payload["job_id"], json!("j_1"));
}

#[test]
fn a_closed_inbox_ends_the_wait_with_the_job_still_running() {
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    // The inbox's only sender leaves with the rewind: the wait ends at
    // once instead of spinning, with no clock move.
    let tx = std::mem::replace(&mut session.inbox, mpsc::channel().0);
    let (jobs, _stops) = FakeJobs::new(&["j_1"]);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let (rw, answered) = support::rewind(args(None, None, false));
    tx.send(rw).unwrap();
    drop(tx);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    let outcome = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the rewind ended in time");
    assert_eq!(outcome, None);
    accepted(answer_of(answered));
    let lines = log::read(&dir).unwrap();
    assert_eq!(lines.last().unwrap().kind, "rewound");
    assert!(
        lines.iter().all(|line| line.kind != "job_completed"),
        "no end is invented for the job that never ended"
    );
}

#[test]
fn the_note_after_a_stop_names_the_jobs_with_their_descriptions() {
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    // The old log holds the jobs' starts: the note joins the stops to
    // them for their descriptions. They are history the idle loop has
    // already written past, so they are appended to the file directly.
    for (id, description) in [("j_1", "npm run dev"), ("j_2", "lint")] {
        let event = Event::JobStarted(JobStarted {
            job_id: JobId(id.into()),
            tool: None,
            extension: None,
            description: description.into(),
            output_path: "/tmp/out".into(),
        });
        let line = contract::Envelope {
            kind: event.kind().into(),
            session_id: SessionId(A_ID.into()),
            ts: 1_759_150_000_000,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: None,
            seq: None,
            payload: event.payload().unwrap(),
        };
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("events.jsonl"))
            .unwrap();
        writeln!(file, "{}", serde_json::to_string(&line).unwrap()).unwrap();
    }
    let (jobs, stops) = FakeJobs::new(&["j_1", "j_2"]);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let (rw, answered) = support::rewind(args(None, None, false));
    let inbox = session.inbox.clone();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    inbox.send(rw).unwrap();
    for _ in 0..2 {
        Deadline::after(DEADLINE)
            .recv(&stops)
            .expect("the job is stopped");
    }
    jobs.end(&inbox, "j_1", Outcome::Cancelled, None);
    jobs.end(&inbox, "j_2", Outcome::Cancelled, None);
    let outcome = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the rewind ended in time");
    assert_eq!(outcome, None);
    accepted(answer_of(answered));
    let note = rewind_note_with_jobs(&dir, &dir, Seq(0)).unwrap();
    assert!(note.contains("- j_1: npm run dev"), "{note}");
    assert!(note.contains("- j_2: lint"), "{note}");
}
