//! A rewind stops every running job before `rewound`
//! (`docs/events.md`, "Rewind"): the stops, the wait with no extra
//! deadline, and what the wait refuses or drops.

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
use contract::{CommandId, ErrorCode, JobId, Seq, SessionId};
use fakes::Deadline;
use fakes::{Scripted, ScriptedProvider};
use log::Log;
use r#loop::{Loop, Model, Permissions, PromptInputs, rewind_note_with_jobs};
use serde_json::json;

use support::{DEADLINE, MODEL, Session, TestTool, calls_reply, delivery, message, paris};

const A_ID: &str = "s_aaaaaaaaaaaaaaaa";
const B_ID2: &str = "s_bbbbbbbbbbbbbbbb";

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

/// Asserts two large strings equal by length and first differing offset,
/// without printing either whole on failure.
#[track_caller]
fn assert_large_str_eq(actual: &str, expected: &str) {
    assert_eq!(actual.len(), expected.len(), "large string length");
    let offset = actual
        .bytes()
        .zip(expected.bytes())
        .position(|(left, right)| left != right);
    assert_eq!(offset, None, "first differing byte offset");
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
        self.end_with_tail(inbox, id, status, error, None);
    }

    /// Ends `id` as `status` with `tail` as the end's output tail: its
    /// notice reaches `inbox`, then `running` drops it.
    fn end_with_tail(
        &self,
        inbox: &mpsc::Sender<Delivery>,
        id: &str,
        status: Outcome,
        error: Option<Failure>,
        tail: Option<String>,
    ) {
        let mut ids = self.ids.lock().unwrap();
        inbox
            .send(Delivery::Job(JobNotice {
                completed: JobCompleted {
                    job_id: JobId(id.into()),
                    status,
                    error,
                    process: None,
                    output_tail: tail,

                    notice: None,
                    changed_by: None,
                },
                claim: Claim(Box::new(|| Some(Vec::new()))),
                delegate: None,
            }))
            .unwrap();
        ids.retain(|listed| listed.0 != id);
    }

    /// Ends `id` with `delegate` riding its completion: its notice
    /// reaches `inbox`, then `running` drops it.
    fn end_delegate(&self, inbox: &mpsc::Sender<Delivery>, id: &str, delegate: DelegateFinished) {
        let mut ids = self.ids.lock().unwrap();
        inbox
            .send(Delivery::Job(JobNotice {
                completed: JobCompleted {
                    job_id: JobId(id.into()),
                    status: Outcome::Failed,
                    error: Some(Failure {
                        code: ErrorCode::NonzeroExit,
                        message: "Exit code 1.".into(),
                        retry_after_ms: None,
                        provider: None,
                    }),
                    process: None,
                    output_tail: None,

                    notice: None,
                    changed_by: None,
                },
                claim: Claim(Box::new(|| Some(Vec::new()))),
                delegate: Some(delegate),
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
            CommandId("c_rewind".into()),
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
    inbox.send(rw).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send((outcome, ())).unwrap();
    });
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
    inbox.send(rw).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
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
fn a_job_that_never_ends_keeps_the_wait_open_until_its_end_arrives() {
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
    inbox.send(rw).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    Deadline::after(DEADLINE)
        .recv(&stops)
        .expect("the job is stopped");
    // The wait parks with no deadline of its own.
    assert!(
        session
            .clock
            .await_parked_unbounded(fakes::Deadline::after(DEADLINE)),
        "the stop wait parked with no deadline"
    );
    assert!(
        log::read(&dir)
            .unwrap()
            .iter()
            .all(|line| line.kind != "rewound"),
        "no rewound is written while the job runs"
    );
    // Once its end arrives the wait returns and the rewind completes.
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
    inbox.send(rw).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    Deadline::after(DEADLINE)
        .recv(&stops)
        .expect("the job is stopped");
    // A prompt, a second rewind and a close are each refused closing; a
    // job notice whose claim does not hold writes no line. They arrive
    // during the stop wait, past the drain that took the rewind.
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

                notice: None,
                changed_by: None,
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
    inbox.send(rw).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    Deadline::after(DEADLINE)
        .recv(&stops)
        .expect("the job is stopped");
    // The batch arrives during the stop wait, past the drain that took
    // the rewind, so the wait drops it instead of writing it.
    inbox
        .send(Delivery::JobLine(contract::events::JobLine {
            job_id: JobId("j_1".into()),
            lines: "still running\n".into(),
            suppressed: None,

            changed_by: None,
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
    tx.send(rw).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
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
    let before = log::read(&dir).unwrap().len();
    let (rw, answered) = support::rewind(args(None, None, true));
    let inbox = take_inbox(&mut session);
    drop(drive_closed(looped, inbox, vec![rw]));
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::SummaryFailed);
    // The summary runs before the stop: its failed turn stays in the log,
    // with no `rewound` after it, and no stop was sent.
    let lines = log::read(&dir).unwrap();
    assert_eq!(
        lines[before..]
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>(),
        [
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed"
        ]
    );
    assert_eq!(lines.last().unwrap().payload["outcome"], "failed");
    assert!(lines.iter().all(|line| line.kind != "rewound"));
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
fn a_job_end_admitted_with_the_rewind_refuses_busy() {
    // Queued job news is refused busy: the rewind is refused
    // without stopping a job or writing `rewound`. The admitted end
    // still starts its own turn afterwards.
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    let (jobs, stops) = FakeJobs::new(&["j_1"]);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let inbox = take_inbox(&mut session);
    jobs.end(&inbox, "j_1", Outcome::Cancelled, None);
    let (rw, answered) = support::rewind(args(None, None, false));
    inbox.send(rw).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    drop(inbox);
    let outcome = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the wait ended in time");
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::Busy);
    assert_eq!(rejection.message, "A turn is running; rewind once it ends.");
    assert!(outcome.is_some(), "the admitted end still starts its turn");
    assert!(
        log::read(&dir)
            .unwrap()
            .iter()
            .all(|line| line.kind != "rewound"),
        "nothing is rewound"
    );
    assert!(stops.try_recv().is_err(), "no stop was sent");
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
    inbox.send(rw).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
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

// The rewind stop wait hooks a job's end like any other end: the
// output_file pass runs on its output file, then the delegate finish and
// completion dispatches, before `write_job_end` writes the lines.

use contract::events::DelegateFinished;
use contract::hook::{
    AfterToolAnswer, AfterToolCall, AfterToolOutcome, BeforeMessage, BeforeMessageAnswer,
    BeforeMessageOutcome, BeforeToolAnswer, BeforeToolCall, BeforeToolOutcome, CheckAnswer,
    CheckOutcome, Hooks, JobDelivery,
};
use contract::shapes::{Tokens, Usage};

/// Redacts `TOKEN-1234` from every delivery the stop wait hooks, or in
/// `withhold` mode replaces each with the withheld line. Records every
/// delivery it saw, with the content it saw.
struct StopHooks {
    seen: Mutex<Vec<(Option<JobDelivery>, String)>>,
    withhold: bool,
}

impl StopHooks {
    fn redacting() -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            withhold: false,
        })
    }

    fn withholding() -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            withhold: true,
        })
    }

    fn deliveries(&self) -> Vec<Option<JobDelivery>> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|(delivery, _)| *delivery)
            .collect()
    }
}

impl Hooks for StopHooks {
    fn after_tool(&self, call: &AfterToolCall<'_>) -> AfterToolAnswer {
        self.seen
            .lock()
            .unwrap()
            .push((call.delivery, call.content.to_owned()));
        if self.withhold {
            return AfterToolAnswer {
                outcome: AfterToolOutcome::Withheld {
                    extension: "redactor".into(),
                },
                changed_by: Vec::new(),
                notices: Vec::new(),
            };
        }
        AfterToolAnswer {
            outcome: AfterToolOutcome::Changed {
                content: Some(call.content.replace("TOKEN-1234", "[redacted]")),
                details: None,
                artifact: None,
            },
            changed_by: vec!["redactor".into()],
            notices: Vec::new(),
        }
    }

    fn before_tool(&self, _call: &BeforeToolCall<'_>) -> BeforeToolAnswer {
        BeforeToolAnswer {
            outcome: BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }

    fn check_tool(&self, _call: &BeforeToolCall<'_>) -> CheckAnswer {
        CheckAnswer {
            outcome: CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn before_message(&self, _message: &BeforeMessage<'_>) -> BeforeMessageAnswer {
        BeforeMessageAnswer {
            outcome: BeforeMessageOutcome::Passed {
                replaced: None,
                context: Vec::new(),
            },
            notices: Vec::new(),
        }
    }

    fn session_start(
        &self,
        _start: &contract::hook::SessionStart<'_>,
    ) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn turn_start(&self, _turn: &contract::hook::TurnStart<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_model_call(
        &self,
        _call: &contract::hook::BeforeModelCall<'_>,
    ) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn turn_end(&self, _end: &contract::hook::TurnEnd<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_handoff(
        &self,
        _handoff: &contract::hook::BeforeHandoff<'_>,
    ) -> contract::hook::BeforeHandoffAnswer {
        fakes::no_note()
    }

    fn deliver_to(&self, inbox: mpsc::Sender<Delivery>) {
        drop(inbox);
    }
}

/// A zeroed usage for a delegate finish a test builds by hand.
fn no_usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 0,
            cache_read: 0,
            cache_write: std::collections::BTreeMap::new(),
            output: 0,
        },
        cost: Some(0.0),
        subscription_cost: 0.0,
    }
}

#[test]
fn a_job_ending_during_the_rewind_stop_wait_is_redacted_in_its_end_and_its_file() {
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    // The output file lives outside the session, named by an absolute path.
    let outputs = fakes::TempDir::new("fiber-rewind-output");
    let output = outputs.path().join("out.log");
    std::fs::write(&output, "token TOKEN-1234 here\n").unwrap();
    let hooks = StopHooks::redacting();
    let mut looped = session.looped.take().unwrap();
    looped
        .write(
            &Event::JobStarted(JobStarted {
                job_id: JobId("j_1".into()),
                tool: Some("shell".into()),
                extension: None,
                description: "test job".into(),
                output_path: output.display().to_string(),
            }),
            None,
            None,
        )
        .unwrap();
    let (jobs, stops) = FakeJobs::new(&["j_1"]);
    let looped = looped
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>)
        .hooks(hooks.clone());
    let (rw, answered) = support::rewind(args(None, None, false));
    let inbox = session.inbox.clone();
    inbox.send(rw).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    Deadline::after(DEADLINE)
        .recv(&stops)
        .expect("the job is stopped");
    // The wait parks with no deadline of its own.
    assert!(
        session
            .clock
            .await_parked_unbounded(fakes::Deadline::after(DEADLINE)),
        "the stop wait parked with no deadline"
    );
    jobs.end_with_tail(
        &inbox,
        "j_1",
        Outcome::Failed,
        Some(Failure {
            code: ErrorCode::NonzeroExit,
            message: "Exit code 1.".into(),
            retry_after_ms: None,
            provider: None,
        }),
        Some("token TOKEN-1234 here\n".into()),
    );
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
    let end = &lines[lines.len() - 2];
    assert_eq!(end.payload["job_id"], json!("j_1"));
    let notice = end.payload["notice"]
        .as_str()
        .expect("the hook set a notice");
    assert!(notice.contains("[redacted]"), "{notice}");
    assert!(!notice.contains("TOKEN-1234"), "{notice}");
    assert_eq!(end.payload["changed_by"], json!(["redactor"]));
    assert!(end.payload.get("output_tail").is_none());
    assert_eq!(std::fs::read(&output).unwrap(), b"token [redacted] here\n");
    // Neither the session log nor the output file names the token.
    let raw = std::fs::read(dir.join("events.jsonl")).unwrap();
    assert!(
        !raw.windows(10).any(|window| window == b"TOKEN-1234"),
        "the raw log holds no token"
    );
    // The output_file pass ran before the completion hook.
    assert_eq!(
        hooks.deliveries(),
        [Some(JobDelivery::OutputFile), Some(JobDelivery::Completion)]
    );
}

#[test]
fn a_withheld_job_end_during_the_rewind_stop_wait_withholds_its_file_and_notice() {
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    let outputs = fakes::TempDir::new("fiber-rewind-output");
    let output = outputs.path().join("out.log");
    std::fs::write(&output, "token TOKEN-1234 here\n").unwrap();
    let hooks = StopHooks::withholding();
    let mut looped = session.looped.take().unwrap();
    looped
        .write(
            &Event::JobStarted(JobStarted {
                job_id: JobId("j_1".into()),
                tool: Some("shell".into()),
                extension: None,
                description: "test job".into(),
                output_path: output.display().to_string(),
            }),
            None,
            None,
        )
        .unwrap();
    let (jobs, stops) = FakeJobs::new(&["j_1"]);
    let looped = looped
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>)
        .hooks(hooks.clone());
    let (rw, answered) = support::rewind(args(None, None, false));
    let inbox = session.inbox.clone();
    inbox.send(rw).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    Deadline::after(DEADLINE)
        .recv(&stops)
        .expect("the job is stopped");
    assert!(
        session
            .clock
            .await_parked_unbounded(fakes::Deadline::after(DEADLINE)),
        "the stop wait parked with no deadline"
    );
    jobs.end_with_tail(
        &inbox,
        "j_1",
        Outcome::Failed,
        Some(Failure {
            code: ErrorCode::NonzeroExit,
            message: "Exit code 1.".into(),
            retry_after_ms: None,
            provider: None,
        }),
        Some("token TOKEN-1234 here\n".into()),
    );
    let outcome = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the rewind ended in time");
    assert_eq!(outcome, None);
    accepted(answer_of(answered));
    let withheld = "Output withheld: the `after_tool` hook of extension redactor failed.\n";
    assert_eq!(std::fs::read(&output).unwrap(), withheld.as_bytes());
    let lines = log::read(&dir).unwrap();
    let end = &lines[lines.len() - 2];
    assert_eq!(end.payload["job_id"], json!("j_1"));
    assert_eq!(end.payload["notice"], json!(withheld.trim_end()));
    assert!(end.payload.get("changed_by").is_none());
    assert!(end.payload.get("output_tail").is_none());
    assert_eq!(
        hooks.deliveries(),
        [Some(JobDelivery::OutputFile), Some(JobDelivery::Completion)]
    );
}

#[test]
fn a_delegate_end_during_the_rewind_stop_wait_is_hooked_in_full_and_artifacted() {
    let mut session = run_a_with_id(A_ID);
    let dir = session.dir.clone();
    let outputs = fakes::TempDir::new("fiber-rewind-output");
    let output = outputs.path().join("out.log");
    std::fs::write(&output, "shell output\n").unwrap();
    let hooks = StopHooks::redacting();
    let mut looped = session.looped.take().unwrap();
    looped
        .write(
            &Event::JobStarted(JobStarted {
                job_id: JobId("j_1".into()),
                tool: Some("codex".into()),
                extension: None,
                description: "test delegate".into(),
                output_path: output.display().to_string(),
            }),
            None,
            None,
        )
        .unwrap();
    let (jobs, stops) = FakeJobs::new(&["j_1"]);
    let looped = looped
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>)
        .hooks(hooks.clone());
    let (rw, answered) = support::rewind(args(None, None, false));
    let inbox = session.inbox.clone();
    inbox.send(rw).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    Deadline::after(DEADLINE)
        .recv(&stops)
        .expect("the job is stopped");
    assert!(
        session
            .clock
            .await_parked_unbounded(fakes::Deadline::after(DEADLINE)),
        "the stop wait parked with no deadline"
    );
    let long = format!("token TOKEN-1234 {}\n", "x".repeat(20 * 1024));
    jobs.end_delegate(
        &inbox,
        "j_1",
        DelegateFinished {
            job_id: JobId("j_1".into()),
            text: long.clone(),
            artifact: None,
            questions: None,
            usage: no_usage(),
            worktree: None,

            changed_by: None,
        },
    );
    let outcome = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the rewind ended in time");
    assert_eq!(outcome, None);
    accepted(answer_of(answered));
    // The delegate hook saw the whole text, not the cut.
    let seen = hooks.seen.lock().unwrap().clone();
    let delegate = seen
        .iter()
        .find(|(delivery, _)| *delivery == Some(JobDelivery::Delegate))
        .expect("the delegate dispatch ran");
    assert_large_str_eq(&delegate.1, &long);
    // The finish is cut with the hook's text in the artifact.
    let lines = log::read(&dir).unwrap();
    let finish = lines
        .iter()
        .rev()
        .find(|line| line.kind == "delegate_finished")
        .expect("the delegate finish is written");
    assert_eq!(finish.payload["changed_by"], json!(["redactor"]));
    let artifact = finish.payload["artifact"]
        .as_str()
        .expect("a long finish writes an artifact");
    let held = std::fs::read(dir.join(artifact)).unwrap();
    let held = String::from_utf8_lossy(&held);
    assert!(held.contains("[redacted]"));
    assert!(!held.contains("TOKEN-1234"), "the artifact holds no token");
}
