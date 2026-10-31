//! A session that moves to another folder (`docs/events.md`, "Change
//! workspace"): the refusals, the stop order before `moved`, the point at
//! the tip with no summary, and the terminal boundary, at crate level.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

extern crate support as ext_support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use contract::commands::{ChangeWorkspaceArgs, RewindArgs};
use contract::events::{CommandResult, JobCompleted, Outcome, Parent};
use contract::inbox::{Ack, Answer, Claim, Delivery, JobNotice, Rejection};
use contract::jobs::{Foreground, Jobs, OpenError, Opened, Opening};
use contract::shapes::Failure;
use contract::tool::Tool;
use contract::{CommandId, ErrorCode, JobId, SessionId};
use fakes::Deadline;
use fakes::{Scripted, ScriptedProvider};
use r#loop::{Loop, Model, Permissions, PromptInputs};
use serde_json::{Map, Value, json};

use support::{DEADLINE, MODEL, Session, delivery, ignore, read_until};

/// A driver's `change_workspace` on the loop's inbox, answering on the
/// returned channel: the answer arrives with one named deadline, never a
/// hang.
fn change_workspace(path: &str) -> (Delivery, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (
        Delivery::ChangeWorkspace(
            CommandId("c_cd".into()),
            ChangeWorkspaceArgs { path: path.into() },
            Ack(Box::new(move |answer| {
                let _sent = tx.send(answer);
            })),
        ),
        rx,
    )
}

fn move_args() -> RewindArgs {
    RewindArgs::Rewind {
        from_session_id: None,
        seq: None,
        summarise: false,
    }
}

#[track_caller]
fn answer_of(rx: mpsc::Receiver<Answer>) -> Answer {
    Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the move is answered in time")
}

fn accepted(answer: Answer) -> SessionId {
    match answer {
        Ok(Some(CommandResult::Rewind { new_session_id })) => new_session_id,
        other => panic!("the move is accepted, got {other:?}"),
    }
}

fn rejected(answer: Answer) -> Rejection {
    match answer {
        Err(rejection) => rejection,
        other => panic!("the move is refused, got {other:?}"),
    }
}

#[track_caller]
fn log_bytes(dir: &Path) -> Vec<u8> {
    std::fs::read(dir.join("events.jsonl")).unwrap()
}

/// Sends `delivery` to an idle `looped` and runs its turn on a thread,
/// failing at one named deadline instead of hanging.
fn drive(
    mut looped: Loop,
    tx: &mpsc::Sender<Delivery>,
    delivery: Delivery,
) -> (Loop, Option<contract::events::TurnOutcome>) {
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

/// Takes the session's inbox sender, leaving a dead one behind.
fn take_inbox(session: &mut Session) -> mpsc::Sender<Delivery> {
    std::mem::replace(&mut session.inbox, mpsc::channel().0)
}

/// Jobs a test lists by hand. Each stop is reported on `stops`. An end
/// drops its id from `running` and then sends its notice through the
/// installed inbox, under the one `running` lock, so a loop that reads an
/// empty `running` always finds the notice waiting, as the registry's
/// contract reads: once a job is gone from `running` its notice is already
/// waiting in the inbox.
struct FakeJobs {
    ids: Mutex<Vec<JobId>>,
    stops: Mutex<mpsc::Sender<JobId>>,
    inbox: Mutex<Option<mpsc::Sender<Delivery>>>,
    /// The session's log: a held end watches it for the written end.
    log: Arc<log::Log>,
    /// Every job a stop was sent for: an end for one of these waits until
    /// the loop writes it, forcing the interleaving where the loop parks
    /// with the job still listed.
    stopped: Mutex<Vec<JobId>>,
}

impl FakeJobs {
    fn new(ids: &[&str], log: &Arc<log::Log>) -> (Arc<Self>, mpsc::Receiver<JobId>) {
        let (tx, rx) = mpsc::channel();
        let jobs = Arc::new(Self {
            ids: Mutex::new(ids.iter().map(|id| JobId((*id).into())).collect()),
            stops: Mutex::new(tx),
            inbox: Mutex::new(None),
            log: Arc::clone(log),
            stopped: Mutex::new(Vec::new()),
        });
        (jobs, rx)
    }

    /// Ends `id` as `status`: the shared transition below, with no hold.
    fn end(&self, id: &str, status: Outcome, error: Option<Failure>) {
        self.transition(id, status, error);
    }

    /// Ends `id` held until the test releases `held`: the drop and the
    /// send land only after the test saw the stop, forcing the stop-then-end
    /// order on every run. Dropping the sender releases it too.
    fn end_hold(
        &self,
        id: &str,
        status: Outcome,
        error: Option<Failure>,
        held: mpsc::Receiver<()>,
    ) {
        let _released = held.recv();
        self.transition(id, status, error);
    }

    /// The shared end transition: drops `id` from `running`, then sends
    /// the notice through the installed inbox, under the one `running`
    /// lock, so the signal follows the state it announces, and a loop that
    /// reads an empty `running` always finds the notice waiting. When the
    /// loop already stopped the job, the return waits until the loop writes
    /// the end, so the test never drops the inbox under a stop wait that
    /// has not settled yet.
    fn transition(&self, id: &str, status: Outcome, error: Option<Failure>) {
        {
            let mut running = self.ids.lock().unwrap();
            running.retain(|listed| listed.0 != id);
            let inbox = self.inbox.lock().unwrap().clone().expect("deliver_to ran");
            inbox
                .send(Delivery::Job(JobNotice {
                    completed: JobCompleted {
                        job_id: JobId(id.into()),
                        status,
                        error,
                        process: None,
                        output_tail: None,
                        notice: None,
                        changed_by: None,
                        protected_paths: None,
                    },
                    claim: Claim(Box::new(|| Some(Vec::new()))),
                    delegate: None,
                }))
                .unwrap();
        }
        if self
            .stopped
            .lock()
            .unwrap()
            .iter()
            .any(|listed| listed.0 == id)
        {
            Self::wait_written(&self.log, id);
        }
    }

    /// Waits until the log holds the `job_completed` line for `id`: the
    /// loop's stop wait wrote the end this `end` sent.
    fn wait_written(log: &log::Log, id: &str) {
        fn is_end(line: &contract::Envelope, id: &str) -> bool {
            line.kind == "job_completed"
                && line.payload.get("job_id").and_then(|value| value.as_str()) == Some(id)
        }
        let id = id.to_owned();
        let _ = read_until(log.watch_all(), &format!("the end of {id}"), move |line| {
            is_end(line, &id)
        });
    }

    /// Drops the installed inbox sender, closing the inbox once every other
    /// sender is gone: the loop's wait ends `Disconnected` with these jobs
    /// still listed as running.
    fn disconnect(&self) {
        self.inbox.lock().unwrap().take();
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
        // Records the id a later `end` checks: an end for a stopped job
        // waits until the loop writes it.
        let mut stopped = self.stopped.lock().unwrap();
        if !stopped.iter().any(|listed| listed == job_id) {
            stopped.push(job_id.clone());
        }
        drop(stopped);
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

    fn deliver_to(&self, inbox: mpsc::Sender<Delivery>) {
        *self.inbox.lock().unwrap() = Some(inbox);
    }
}

#[test]
fn a_held_end_waits_for_the_tests_release() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    let (jobs, _stops) = FakeJobs::new(&["j_1"], &session.log);
    jobs.deliver_to(take_inbox(&mut session));
    let (release, held) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        jobs.end_hold("j_1", Outcome::Cancelled, None, held);
        done.send(()).unwrap();
    });
    assert!(
        Deadline::after(std::time::Duration::from_millis(100))
            .recv(&finished)
            .is_err(),
        "the held end waits for the release"
    );
    release.send(()).expect("the held end is released");
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the released end ends");
}

/// A tool that sends `change_workspace` to the inbox from inside a turn,
/// capturing its answer: the move lands while the turn runs.
struct SendMove {
    inbox: Mutex<Option<mpsc::Sender<Delivery>>>,
    seen: Mutex<Option<mpsc::Receiver<Answer>>>,
    close_first: bool,
    path: String,
}

impl SendMove {
    fn install(&self, inbox: mpsc::Sender<Delivery>) {
        *self.inbox.lock().unwrap() = Some(inbox);
    }

    fn answer(&self) -> Answer {
        let rx = self.seen.lock().unwrap().take().unwrap();
        answer_of(rx)
    }
}

impl Tool for SendMove {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "send".into(),
            description: "Sends the move.".into(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(
        &self,
        _: &Map<String, Value>,
    ) -> Result<contract::tool::Effects, contract::tool::EffectsError> {
        Ok(contract::tool::Effects {
            declared: contract::shapes::DeclaredEffects {
                effects: vec![contract::shapes::Effect::Reads],
                reversible: true,
                paths: None,
                confined: false,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            review_when_confined: false,
            hosts: None,
        })
    }

    fn run(
        &self,
        _: &Map<String, Value>,
        _: &dyn contract::tool::Cancel,
        _: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        let inbox = self
            .inbox
            .lock()
            .unwrap()
            .clone()
            .expect("the inbox sender is installed");
        if self.close_first {
            inbox.send(Delivery::Close(ignore())).unwrap();
        }
        let (mv, answered) = change_workspace(&self.path);
        inbox.send(mv).unwrap();
        *self.seen.lock().unwrap() = Some(answered);
        contract::tool::Output {
            content: vec![contract::shapes::ContentPart::Text {
                text: "sent".into(),
            }],
            ..contract::tool::Output::default()
        }
    }

    fn bound(&self) -> contract::tool::Bound {
        contract::tool::Bound::DEFAULT
    }
}

fn target_dir(session: &Session) -> PathBuf {
    let target = session
        .dir
        .parent()
        .expect("the session dir has a home")
        .join("other");
    std::fs::create_dir_all(&target).unwrap();
    target
}

#[test]
fn a_move_writes_moved_at_the_tip_with_no_summary() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    let dir = session.dir.clone();
    let target = target_dir(&session);
    let before = log_bytes(&dir);
    let moved_seq = log::read(&dir).unwrap().len() as u64;
    let (mv, answered) = change_workspace("../other");
    let (_looped, outcome) = drive(session.looped.take().unwrap(), &session.inbox, mv);
    assert_eq!(outcome, None, "the moved session starts no turn");
    let child = accepted(answer_of(answered));
    let lines = log::read(&dir).unwrap();
    assert_eq!(lines.len() as u64, moved_seq + 1);
    assert_eq!(&log_bytes(&dir)[..before.len()], &before[..]);
    let last = lines.last().unwrap();
    assert_eq!(last.kind, "moved");
    assert_eq!(last.payload["new_session_id"], child.0.as_str());
    assert_eq!(last.payload["seq"], moved_seq - 1);
    assert_eq!(
        last.payload["workspace"],
        std::fs::canonicalize(&target)
            .unwrap()
            .display()
            .to_string()
    );
    let mut keys: Vec<&str> = last.payload.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["new_session_id", "seq", "workspace"]);
    assert!(
        log::history(&dir).unwrap().len() == 1,
        "the old log still reads as one chain"
    );
}

#[test]
fn the_same_folder_is_refused_without_writing() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    let dir = session.dir.clone();
    let before = log_bytes(&dir);
    let (mv, answered) = change_workspace(&session.workspace.display().to_string());
    let looped = session.looped.take().unwrap();
    let inbox = take_inbox(&mut session);
    inbox.send(mv).unwrap();
    drop(inbox);
    let mut looped = looped;
    let outcome = looped.turn().unwrap();
    assert_eq!(outcome, None);
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(rejection.message, "already in that folder");
    assert_eq!(log_bytes(&dir), before, "nothing is written");
}

#[test]
fn a_move_during_a_turn_is_busy() {
    let target = fakes::TempDir::new("fiber-move-turn-target");
    let tool = Arc::new(SendMove {
        inbox: Mutex::new(None),
        seen: Mutex::new(None),
        close_first: false,
        path: target.path().display().to_string(),
    });
    let mut session = Session::with_tools(
        vec![
            support::calls_reply("", &[("send", json!({}))]),
            Scripted::text("Done."),
        ],
        None,
        vec![Arc::clone(&tool) as Arc<dyn Tool>],
    );
    tool.install(session.inbox.clone());
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    let rejection = rejected(tool.answer());
    assert_eq!(rejection.code, ErrorCode::Busy);
    assert_eq!(
        rejection.message,
        "A turn is running; change workspace once it ends."
    );
    assert!(
        log::read(&session.dir)
            .unwrap()
            .iter()
            .all(|line| line.kind != "moved"),
        "the refused move closes nothing"
    );
}

#[test]
fn a_move_during_a_closing_turn_is_closing() {
    let target = fakes::TempDir::new("fiber-move-closing-target");
    let tool = Arc::new(SendMove {
        inbox: Mutex::new(None),
        seen: Mutex::new(None),
        close_first: true,
        path: target.path().display().to_string(),
    });
    let mut session = Session::with_tools(
        vec![
            support::calls_reply("", &[("send", json!({}))]),
            Scripted::text("Done."),
        ],
        None,
        vec![Arc::clone(&tool) as Arc<dyn Tool>],
    );
    tool.install(session.inbox.clone());
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    let rejection = rejected(tool.answer());
    assert_eq!(rejection.code, ErrorCode::Closing);
    assert_eq!(
        rejection.message,
        "The session is closing and takes no new turn."
    );
}

#[test]
fn a_move_with_admitted_job_news_is_busy() {
    // Queued job news is refused busy: the move is refused without
    // stopping a job or writing `moved`.
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    let dir = session.dir.clone();
    let (jobs, stops) = FakeJobs::new(&["j_1"], &session.log);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let inbox = take_inbox(&mut session);
    jobs.deliver_to(inbox.clone());
    jobs.end("j_1", Outcome::Cancelled, None);
    target_dir(&session);
    let (mv, answered) = change_workspace("../other");
    inbox.send(mv).unwrap();
    drop(inbox);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    let outcome = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the wait ended in time");
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::Busy);
    assert_eq!(
        rejection.message,
        "A turn is running; change workspace once it ends."
    );
    assert!(outcome.is_some(), "the admitted end still starts its turn");
    assert!(
        log::read(&dir)
            .unwrap()
            .iter()
            .all(|line| line.kind != "moved"),
        "nothing is moved"
    );
    assert!(stops.try_recv().is_err(), "no stop was sent");
}

#[test]
fn a_second_transition_during_the_stop_is_busy_then_the_move_is_terminal() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    let dir = session.dir.clone();
    target_dir(&session);
    let (jobs, stops) = FakeJobs::new(&["j_1"], &session.log);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let (mv, answered) = change_workspace("../other");
    let (rw, rw_answered) = support::rewind(move_args());
    let (mv2, mv2_answered) = change_workspace("../other");
    let inbox = take_inbox(&mut session);
    jobs.deliver_to(inbox.clone());
    inbox.send(mv).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send((looped, outcome)).unwrap();
    });
    // The held end lands only after the test saw the stop, forcing the
    // stop-then-end order on every run.
    thread::scope(|s| {
        let (release, held) = mpsc::channel();
        let ended = s.spawn(|| jobs.end_hold("j_1", Outcome::Cancelled, None, held));
        // The stop barrier: the move waits for the job's end, so nothing is
        // durable yet.
        assert_eq!(
            Deadline::after(DEADLINE)
                .recv(&stops)
                .expect("a stop arrives")
                .0,
            "j_1"
        );
        assert!(
            log::read(&dir)
                .unwrap()
                .iter()
                .all(|line| line.kind != "moved"),
            "no moved while the job runs"
        );
        inbox.send(rw).unwrap();
        inbox.send(mv2).unwrap();
        for answered in [rw_answered, mv2_answered] {
            let rejection = rejected(answer_of(answered));
            assert_eq!(rejection.code, ErrorCode::Busy);
            assert_eq!(
                rejection.message,
                "A workspace move is in progress; send the command again once it ends."
            );
        }
        assert!(
            log::read(&dir)
                .unwrap()
                .iter()
                .all(|line| line.kind != "moved"),
            "competitors write nothing"
        );
        release.send(()).expect("the held end is released");
        ended.join().expect("the held end ends");
    });
    drop(inbox);
    let (_looped, outcome) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the move ended in time");
    assert_eq!(outcome, None, "the moved session starts no turn");
    accepted(answer_of(answered));
    let lines = log::read(&dir).unwrap();
    let kinds: Vec<&str> = lines.iter().map(|line| line.kind.as_str()).collect();
    assert_eq!(*kinds.last().unwrap(), "moved");
    assert!(
        !kinds[..kinds.len() - 1].contains(&"moved"),
        "moved is the last line"
    );
}

#[test]
fn job_ends_are_written_before_moved_in_order() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    let dir = session.dir.clone();
    target_dir(&session);
    let (jobs, stops) = FakeJobs::new(&["j_1", "j_2"], &session.log);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    let (mv, answered) = change_workspace("../other");
    let inbox = take_inbox(&mut session);
    jobs.deliver_to(inbox.clone());
    inbox.send(mv).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn().unwrap();
        done.send(outcome).unwrap();
    });
    // The held ends land only after the test saw both stops, forcing the
    // stop-then-end order on every run, one written end at a time.
    thread::scope(|s| {
        let (release_first, held_first) = mpsc::channel();
        let (release_second, held_second) = mpsc::channel();
        let ended_first = s.spawn(|| jobs.end_hold("j_1", Outcome::Cancelled, None, held_first));
        let ended_second = s.spawn(|| {
            jobs.end_hold(
                "j_2",
                Outcome::Failed,
                Some(Failure {
                    code: ErrorCode::NoModel,
                    message: "boom".into(),
                    retry_after_ms: None,
                    provider: None,
                }),
                held_second,
            )
        });
        // The stop barrier: both stops arrive before the ends, so the move
        // waits for them instead of draining them early.
        let mut stopped = 0;
        while stopped < 2 {
            Deadline::after(DEADLINE)
                .recv(&stops)
                .expect("a stop arrives");
            stopped += 1;
        }
        release_first.send(()).expect("the held end is released");
        ended_first.join().expect("the held end ends");
        release_second.send(()).expect("the held end is released");
        ended_second.join().expect("the held end ends");
    });
    drop(inbox);
    let outcome = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the move ended in time");
    assert_eq!(outcome, None);
    accepted(answer_of(answered));
    let lines = log::read(&dir).unwrap();
    let tail: Vec<&str> = lines
        .iter()
        .rev()
        .take(3)
        .map(|line| line.kind.as_str())
        .collect();
    // docs/events.md:1606: each running job ends before `moved`.
    assert_eq!(tail, ["moved", "job_completed", "job_completed"]);
    let moved = lines.last().unwrap();
    assert_eq!(moved.payload["seq"], (lines.len() - 2) as u64);
}

#[test]
fn a_disconnected_inbox_fails_the_move_without_moved_or_fabrication() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    let dir = session.dir.clone();
    target_dir(&session);
    let tx = take_inbox(&mut session);
    let (jobs, stops) = FakeJobs::new(&["j_1"], &session.log);
    let looped = session
        .looped
        .take()
        .unwrap()
        .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    jobs.deliver_to(tx.clone());
    let (mv, answered) = change_workspace("../other");
    tx.send(mv).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut looped = looped;
        done.send(looped.turn()).unwrap();
    });
    // The stop barrier: the stop was sent before the disconnection.
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&stops)
            .expect("a stop arrives")
            .0,
        "j_1"
    );
    drop(tx);
    jobs.disconnect();
    let outcome = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the move ended in time");
    assert!(
        outcome.is_err(),
        "the move fails while a job is still running"
    );
    assert!(
        answered.try_recv().is_err(),
        "the acknowledgement is never answered"
    );
    let lines = log::read(&dir).unwrap();
    assert!(
        lines.iter().all(|line| line.kind != "moved"),
        "no moved hides the unfinished job"
    );
    assert!(
        lines.iter().all(|line| line.kind != "job_completed"),
        "no end is invented for the job that never ended"
    );
}

#[test]
fn a_failed_append_ends_without_moved_and_without_an_answer() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    let dir = session.dir.clone();
    target_dir(&session);
    session.log.end_watchers();
    let (mv, answered) = change_workspace("../other");
    let inbox = take_inbox(&mut session);
    inbox.send(mv).unwrap();
    let mut looped = session.looped.take().unwrap();
    let failed = looped.turn().expect_err("the failed write ends the turn");
    assert!(
        log::read(&dir)
            .unwrap()
            .iter()
            .all(|line| line.kind != "moved"),
        "no moved is written"
    );
    assert!(
        answered.try_recv().is_err(),
        "the acknowledgement is never answered"
    );
    let _ = failed;
}

#[test]
fn a_move_on_a_delegate_is_refused() {
    // A delegate's loop: its `session_started` names its parent, so the
    // move is refused before anything starts.
    let session = Session::new(vec![Scripted::text("Done.")], None);
    let home = session.dir.parent().unwrap().to_path_buf();
    let clock: Arc<dyn contract::clock::Clock> = session.clock.clone();
    let log =
        Arc::new(log::Log::create(&home, SessionId("s_delegate".into()), clock.clone()).unwrap());
    let dir = log.dir().to_path_buf();
    let (tx, rx) = mpsc::channel();
    let mut prompt = PromptInputs::new(
        home.clone(),
        "/bin/sh".into(),
        dir.join("events.jsonl").display().to_string(),
        clock,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let mut looped = Loop::delegate(
        r#loop::Session {
            log,
            provider: Arc::new(ScriptedProvider::new(vec![Scripted::text("Done.")]))
                as Arc<dyn contract::provider::Provider>,
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
                rules: session.rules.clone(),
            },
        },
        Parent {
            session_id: SessionId("s_parent".into()),
            delegate_id: contract::JobId("d_1".into()),
        },
        None,
    )
    .unwrap();
    let before = log_bytes(&dir);
    let (mv, answered) = change_workspace("../other");
    tx.send(mv).unwrap();
    drop(tx);
    assert_eq!(looped.turn().unwrap(), None);
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::DelegateSession);
    assert_eq!(rejection.message, "A delegate cannot change workspace.");
    assert_eq!(log_bytes(&dir), before, "nothing is written");
}
