//! Resume paths: the run boundary, the spawn, cancel around it, and the
//! forced interleaving. Launches run real blocked children, reaped through
//! the registry's stop under a bounded clock driver.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use fakes::MUST_SUCCEED_WITHIN;
use std::collections::VecDeque;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock as _;
use contract::inbox::Delivery;
use contract::jobs::{Jobs as _, Stop, Taken};
use contract::tool::{Answered, Ask, Asking, Output, Tool};
use contract::{ActionId, ErrorCode, JobId, SessionId};
use fakes::clock::FakeClock;
use fakes::{CancelToken, Recorder, TempDir, Watchdog};
use serde_json::{Map, Value, json};

use super::super::Watched;
use super::{DelegateMessage, LastSeq, Launch, Steer, Steered, Watch};
use crate::Start;
use crate::registry::Registry;
use crate::support::{delegate_finished, ended_ok, exited};

fn delegate_started(id: &str) -> contract::events::DelegateStarted {
    let mut started = crate::support::delegate_started(id);
    started.workspace = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    started
}
use crate::timing_tests::{Step, try_step};
use contract::Envelope;

/// The stop bound and closing wait the tests run with, on the fake clock.
const BOUND: Duration = Duration::from_secs(5);

/// The resuming call's action, from its `Ask`.
fn action() -> ActionId {
    ActionId("a_message00000002".into())
}

/// An `Ask` that answers nothing and records whether it was asked.
struct FakeAsk {
    asked: AtomicBool,
}

impl Ask for FakeAsk {
    fn action(&self) -> ActionId {
        action()
    }

    fn answerable(&self) -> bool {
        false
    }

    fn ask(&self, _: Asking) -> Answered {
        self.asked.store(true, Ordering::SeqCst);
        Answered::NoAnswer
    }
}

/// What a launch was asked, as the tool built it.
struct Launched {
    depth: u32,
    session_id: SessionId,
    job_id: JobId,
    parent: SessionId,
    prompt: String,
    workspace: std::path::PathBuf,
    worktree: Option<contract::shapes::Worktree>,
    resume: bool,
}

/// What a scripted steer answers.
#[derive(Clone, Copy)]
enum ResumeSteer {
    Gone,
    /// Gone before the commit ran: nothing reached any run.
    GoneUncommitted,
    Closing,
}

/// What a scripted launch does.
enum LaunchReply {
    /// A blocked `sh` child holding `marker` in its command line.
    Block(String),
    /// A blocked `sh` child held before its command returns, signalled
    /// on entry.
    Hold {
        marker: String,
        release: Mutex<mpsc::Receiver<()>>,
        reached: mpsc::Sender<()>,
    },
    /// A missing executable: the spawn fails.
    Missing,
}

/// The tool over scripted seams.
struct ResumeRig {
    _dir: TempDir,
    registry: Arc<Registry>,
    clock: Arc<FakeClock>,
    tool: Arc<DelegateMessage>,
    inbox: mpsc::Receiver<Delivery>,
    launched: Arc<Mutex<Vec<Launched>>>,
    watch_calls: WatchCalls,
    delivered: mpsc::Receiver<()>,
    last_seq_calls: Arc<Mutex<Vec<SessionId>>>,
    steer_calls: Arc<Mutex<usize>>,
}

/// Every watch call: the watched session and the boundary it passed.
type WatchCalls = Arc<Mutex<Vec<(SessionId, Option<u64>)>>>;

fn envelope(seq: u64, kind: &str, payload: serde_json::Value) -> Envelope {
    Envelope {
        kind: kind.into(),
        session_id: SessionId("s_child".into()),
        ts: 0,
        schema_version: 1,
        turn_id: None,
        action_id: None,
        seq: Some(contract::Seq(seq)),
        payload: payload.as_object().unwrap().clone(),
    }
}

fn exited_line(seq: u64, text: &str) -> Envelope {
    envelope(
        seq,
        "fiber_exited",
        serde_json::to_value(exited(text)).unwrap(),
    )
}

fn resume_rig(
    launches: Vec<LaunchReply>,
    watch: Vec<(Vec<Envelope>, Watched)>,
    last_seq: LastSeq,
) -> ResumeRig {
    resume_rig_with_steer(launches, watch, last_seq, vec![])
}

/// The rig with a scripted steer, for the closing and backoff resumes.
fn resume_rig_with_steer(
    launches: Vec<LaunchReply>,
    watch: Vec<(Vec<Envelope>, Watched)>,
    last_seq: LastSeq,
    steer: Vec<ResumeSteer>,
) -> ResumeRig {
    let dir = TempDir::new("fiber-delegate-resume");
    let clock = FakeClock::new();
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let registry = Registry::new(
        artifacts,
        Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        Arc::new(Recorder::default()),
    );
    let (inbox_tx, inbox_rx) = mpsc::channel();
    registry.deliver_to(inbox_tx);
    let launched: Arc<Mutex<Vec<Launched>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&launched);
    let queued = Arc::new(Mutex::new(VecDeque::from(launches)));
    let launch: Launch = Arc::new(move |launched| {
        recorded.lock().unwrap().push(Launched {
            depth: launched.depth,
            session_id: launched.session_id.clone(),
            job_id: launched.job_id.clone(),
            parent: launched.parent.clone(),
            prompt: launched.prompt.clone(),
            workspace: launched.workspace.clone(),
            worktree: launched.worktree.clone(),
            resume: matches!(launched.start, Start::Resume),
        });
        match queued.lock().unwrap().pop_front().expect("a launch reply") {
            LaunchReply::Block(marker) => {
                let mut command = Command::new("sh");
                command.args(["-c", &format!(": {marker}; read line")]);
                command
            }
            LaunchReply::Hold {
                marker,
                release,
                reached,
            } => {
                let _sent = reached.send(());
                fakes::Deadline::after(MUST_SUCCEED_WITHIN)
                    .recv(&release.lock().unwrap())
                    .expect("the test releases the launch");
                let mut command = Command::new("sh");
                command.args(["-c", &format!(": {marker}; read line")]);
                command
            }
            // Nothing to execute: the spawn fails closed.
            LaunchReply::Missing => Command::new("/nonexistent/fiber-delegate-launch"),
        }
    });
    let watch_calls: WatchCalls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&watch_calls);
    let pending = Arc::new(Mutex::new(VecDeque::from(watch)));
    let (delivered_tx, delivered_rx) = mpsc::channel();
    let signal = delivered_tx.clone();
    // As `doors::watch_after`: lines at or before the boundary belong to
    // an earlier run, so they are neither delivered nor end the watch.
    let watch: Watch = Arc::new(move |id, after, on_line| {
        seen.lock().unwrap().push((id.clone(), after));
        let (lines, watched) = pending
            .lock()
            .unwrap()
            .pop_front()
            .expect("a scripted watch reply");
        for line in &lines {
            let fresh = line
                .seq
                .map(|seq| after.is_none_or(|at| seq.0 > at))
                .unwrap_or(true);
            if fresh {
                on_line(line);
            }
        }
        let _sent = signal.send(());
        Ok(watched)
    });
    let last_seq_calls: Arc<Mutex<Vec<SessionId>>> = Arc::new(Mutex::new(Vec::new()));
    let read = Arc::clone(&last_seq_calls);
    let inner = last_seq;
    let last_seq: LastSeq = Arc::new(move |workspace, id| {
        read.lock().unwrap().push(id.clone());
        inner(workspace, id)
    });
    let steer_calls = Arc::new(Mutex::new(0usize));
    let counted = Arc::clone(&steer_calls);
    let scripted = Arc::new(Mutex::new(VecDeque::from(steer)));
    let steer: Steer = Arc::new(
        move |_, _, _, commit: &mut dyn FnMut() -> bool, _: &mut dyn FnMut()| {
            *counted.lock().unwrap() += 1;
            match scripted
                .lock()
                .unwrap()
                .pop_front()
                .expect("a scripted steer answer")
            {
                ResumeSteer::Gone => {
                    assert!(commit(), "the backoff steer commits");
                    Steered::Gone
                }
                ResumeSteer::GoneUncommitted => Steered::Gone,
                ResumeSteer::Closing => {
                    assert!(commit(), "the closing steer commits");
                    Steered::Rejected(contract::shapes::Failure {
                        code: ErrorCode::Closing,
                        message: "done".into(),
                        retry_after_ms: None,
                        provider: None,
                    })
                }
            }
        },
    );
    let tool = Arc::new(DelegateMessage {
        finish: crate::support::clean_finish(),
        depth: 0,
        registry: Arc::clone(&registry),
        parent: SessionId("s_parent".into()),
        clock: Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        bound: BOUND,
        cap: 1024,
        launch,
        watch,
        steer,
        worktree_record: Arc::new(|_, _| Ok(None)),
        last_seq,
    });
    ResumeRig {
        _dir: dir,
        registry,
        clock,
        tool,
        inbox: inbox_rx,
        launched,
        watch_calls,
        delivered: delivered_rx,
        last_seq_calls,
        steer_calls,
    }
}

#[test]
fn resume_workspace_recovery_reads_the_childs_record_and_never_sets_worktree() {
    for mode in [
        "kept",
        "removed",
        "absent",
        "missing-repository",
        "missing-workspace",
        "read-error",
    ] {
        let mut rig = resume_rig(
            vec![LaunchReply::Missing],
            vec![],
            Arc::new(|_, _| Ok(Some(7))),
        );
        let workspace = rig._dir.path().join("child");
        let repository = rig._dir.path().join("repository");
        if mode == "kept" || mode == "absent" {
            std::fs::create_dir(&workspace).unwrap();
        }
        if mode != "missing-repository" {
            std::fs::create_dir(&repository).unwrap();
        }
        let record = contract::shapes::Worktree {
            path: workspace.to_string_lossy().into_owned(),
            branch: "fiber/child".into(),
            repository: Some(repository.to_string_lossy().into_owned()),
        };
        let expected_workspace = workspace.clone();
        let returned = record.clone();
        Arc::get_mut(&mut rig.tool).unwrap().worktree_record = Arc::new(move |path, id| {
            assert_eq!(path, expected_workspace);
            assert_eq!(id.0, "s_d000000000000001");
            match mode {
                "absent" | "missing-workspace" => Ok(None),
                "read-error" => Err("record log unreadable".into()),
                _ => Ok(Some(returned.clone())),
            }
        });
        let expected_workspace = workspace.clone();
        Arc::get_mut(&mut rig.tool).unwrap().last_seq = Arc::new(move |path, _| {
            assert_eq!(path, expected_workspace);
            Ok(Some(7))
        });
        let id = crate::registry::mint_job_id();
        let mut delegate = delegate_started(&id.0);
        delegate.workspace = workspace.to_string_lossy().into_owned();
        let (_, finish) = rig.registry.open_started(
            id.clone(),
            "delegate_spawn".into(),
            "scan".into(),
            "output".into(),
            Stop(Box::new(|| {})),
            delegate,
        );
        end_resume_run(finish, &id, "done");
        let _notice = take_notice(&rig);
        let output = rig.tool.run(
            &resume_args(&id.0, "again"),
            &CancelToken::new(),
            &Recorder::default(),
        );
        match mode {
            "missing-repository" | "missing-workspace" => {
                assert_eq!(code_of(&output), ErrorCode::IoFailed, "{mode}");
                assert!(
                    content(&output).contains(workspace.to_str().unwrap()),
                    "{}",
                    content(&output)
                );
                assert!(rig.launched.lock().unwrap().is_empty());
            }
            "read-error" => {
                assert_eq!(code_of(&output), ErrorCode::ToolError);
                assert!(content(&output).contains("record log unreadable"));
                assert!(rig.launched.lock().unwrap().is_empty());
            }
            _ => {
                assert_eq!(
                    code_of(&output),
                    ErrorCode::ToolError,
                    "the scripted spawn fails"
                );
                let launched = rig.launched.lock().unwrap();
                assert_eq!(launched.len(), 1);
                assert_eq!(
                    launched[0].workspace,
                    if mode == "removed" {
                        &repository
                    } else {
                        &workspace
                    }
                    .to_path_buf()
                );
                assert!(launched[0].worktree.is_none());
            }
        }
    }
}

#[test]
fn a_resume_at_the_concurrency_cap_starts_nothing() {
    let rig = resume_rig(
        vec![],
        vec![],
        Arc::new(|_, _| panic!("cap precedes the boundary read")),
    );
    let (id, finish) = open_resume_delegate(&rig, "/unused");
    end_resume_run(finish, &id, "done");
    let _running: Vec<_> = (0..10)
        .map(|_| open_resume_delegate(&rig, "/unused"))
        .collect();
    let output = rig.tool.run(
        &resume_args(&id.0, "again"),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code_of(&output), ErrorCode::ToolError);
    assert_eq!(
        content(&output),
        "This session already runs 10 delegates, the most it can run at once, so nothing was started; wait for one with `jobs wait` or stop one with `jobs stop`, then try again.\n"
    );
    assert!(rig.launched.lock().unwrap().is_empty());
    assert!(rig.last_seq_calls.lock().unwrap().is_empty());
}

/// Opens a delegate whose log is `output`.
fn open_resume_delegate(
    rig: &ResumeRig,
    output: &str,
) -> (contract::JobId, crate::registry::Unreported) {
    let id = crate::registry::mint_job_id();
    let (_started, finish) = rig.registry.open_started(
        id.clone(),
        "delegate_spawn".into(),
        "scan".into(),
        output.into(),
        Stop(Box::new(|| {})),
        delegate_started(&id.0),
    );
    (id, finish)
}

fn end_resume_run(finish: crate::registry::Unreported, id: &contract::JobId, text: &str) {
    finish.report(ended_ok(&id.0), Some(delegate_finished(&id.0, text)));
}

fn resume_args(id: &str, message: &str) -> Map<String, Value> {
    let mut args = Map::new();
    args.insert("id".into(), json!(id));
    args.insert("message".into(), json!(message));
    args
}

fn content(output: &Output) -> &str {
    match output.content.first() {
        Some(contract::shapes::ContentPart::Text { text }) => text,
        other => panic!("expected text, got {other:?}"),
    }
}

fn code_of(output: &Output) -> ErrorCode {
    output.error.as_ref().expect("the call failed").code.clone()
}

fn call(settle: &contract::tool::Settle) -> Vec<Taken> {
    (settle.0)()
}

/// The runner's park when it lies ahead of `now()`, otherwise `None`.
/// Never blocks and never moves the clock. Panics naming `parked()` when
/// more than one thread is parked or a park has no deadline: the runner
/// is the only thread that parks on the clock while the driver runs.
fn park_ahead(clock: &FakeClock) -> Option<Instant> {
    let parked = clock.parked();
    if parked.len() > 1 {
        panic!("expected the runner's single park, saw parked={parked:?}");
    }
    match parked.into_iter().next() {
        None => None,
        Some(None) => panic!("the runner parked with no deadline; parked=[None]"),
        Some(Some(until)) => {
            let now = clock.now();
            (until > now).then_some(until)
        }
    }
}

/// Advances the fake clock only from the runner's own park: a park at
/// most one poll ahead moves time to its `until`; anything further ahead
/// is the drain wait and is never advanced. Each pass advances to the
/// park it just read, through `try_advance_when_parked`; a pass that
/// finds no fresh park loops, and a pass that finds a park at another
/// instant carries that instant into the next pass. Ends when `stop` is
/// set, which the single wall-clock wait below sets on every outcome,
/// and fails at the driver's own deadline naming the wait.
fn drive(clock: &FakeClock, stop: &AtomicBool, deadline: fakes::Deadline) {
    let mut carried: Option<Instant> = None;
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if deadline.left().is_zero() {
            panic!(
                "the runner never reported; now={:?} parked={:?}",
                clock.now(),
                clock.parked()
            );
        }
        let mut candidate = carried.take();
        if candidate.is_some_and(|watched| watched <= clock.now()) {
            candidate = None;
        }
        match candidate.or_else(|| park_ahead(clock)) {
            Some(until) => {
                let now = clock.now();
                match until.checked_duration_since(now) {
                    // The runner's poll horizon: a park further ahead is
                    // the drain wait, which its own end wakes.
                    Some(gap) if gap <= Duration::from_secs(1) => {
                        match try_step(clock, until, gap) {
                            Step::Moved | Step::NoPark => {}
                            Step::Next(watched) => {
                                carried = Some(watched);
                            }
                        }
                    }
                    _ => thread::yield_now(),
                }
            }
            None => thread::yield_now(),
        }
    }
}

/// Takes one notice the registry sent, without claiming it: the queue
/// stays as it was, so a later settle or claim still takes it.
fn take_notice(rig: &ResumeRig) -> contract::inbox::JobNotice {
    let Delivery::Job(notice) = rig.inbox.try_recv().expect("the end sent a notice") else {
        panic!("the end sent nothing job-like");
    };
    notice
}

/// The runner's report, driving the clock until it arrives, failing at
/// one wall-clock deadline for the whole wait.
#[track_caller]
fn reported(rig: &ResumeRig) -> contract::inbox::JobNotice {
    let stop = AtomicBool::new(false);
    let clock = Arc::clone(&rig.clock);
    let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
    thread::scope(|scope| {
        scope.spawn(|| drive(&clock, &stop, deadline));
        let result = deadline.recv(&rig.inbox);
        stop.store(true, Ordering::Relaxed);
        match result {
            Ok(Delivery::Job(notice)) => notice,
            Ok(other) => panic!("expected the runner's report, got {other:?}"),
            Err(_) => panic!("the runner never reported"),
        }
    })
}

/// Stops run `id`'s child, drives its end, and reaps it behind the
/// watchdog the test started before it launched the child, so a panic or
/// timeout between the launch and this helper still reaps the group.
/// Returns the run's notice, unclaimed.
fn stop_and_reap(rig: &ResumeRig, id: &JobId, watchdog: Watchdog) -> contract::inbox::JobNotice {
    assert!(
        contract::jobs::Jobs::stop(rig.registry.as_ref(), id),
        "run {id:?} stops"
    );
    let notice = reported(rig);
    assert_eq!(notice.completed.job_id, *id);
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    notice
}

/// Runs the call with `message` through `run_asking` on its own thread.
/// The `Ask` is never asked.
fn call_async(
    tool: &Arc<DelegateMessage>,
    id: &str,
    message: &str,
    cancel: CancelToken,
) -> (mpsc::Receiver<Output>, thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let tool = Arc::clone(tool);
    let args = resume_args(id, message);
    let ask = FakeAsk {
        asked: AtomicBool::new(false),
    };
    let handle = thread::spawn(move || {
        let output = tool.run_asking(&args, &cancel, &Recorder::default(), &ask);
        assert!(!ask.asked.load(Ordering::SeqCst), "the tool never asks");
        let _sent = tx.send(output);
    });
    (rx, handle)
}

fn answered(rx: &mpsc::Receiver<Output>) -> Output {
    fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(rx)
        .expect("the call answered")
}

/// The reply's receipt text for `id` logging at `log`.
fn resumed_text(id: &str, log: &str) -> String {
    format!("Resumed delegate {id}. Its log is {log}.")
}

/// One queue entry the tests compare whole.
fn taken_record(id: &JobId, text: &str, action: Option<ActionId>) -> Taken {
    Taken {
        record: contract::jobs::JobRecord::DelegateFinished(delegate_finished(&id.0, text)),
        action,
    }
}

fn taken_completed(id: &JobId) -> Taken {
    Taken {
        record: contract::jobs::JobRecord::Completed(ended_ok(&id.0)),
        action: None,
    }
}

fn taken_started(id: &JobId, output: &str, action: Option<ActionId>) -> Taken {
    Taken {
        record: contract::jobs::JobRecord::Started(contract::events::JobStarted {
            job_id: id.clone(),
            tool: Some("delegate_message".into()),
            extension: None,
            description: "scan".into(),
            output_path: output.into(),
        }),
        action,
    }
}

fn taken_delegate_started(id: &JobId, action: Option<ActionId>) -> Taken {
    Taken {
        record: contract::jobs::JobRecord::DelegateStarted(delegate_started(&id.0)),
        action,
    }
}

#[test]
fn run_and_run_asking_differ_only_in_the_starts_action() {
    let rig = resume_rig(
        vec![LaunchReply::Block("fiber-message-definition-1".into())],
        vec![(Vec::new(), Watched::Closed)],
        Arc::new(|_, _| Ok(None)),
    );
    let output_path = format!("{}/sessions/s_1/events.jsonl", rig._dir.path().display());
    let (id, finish) = open_resume_delegate(&rig, &output_path);
    end_resume_run(finish, &id, "Done.");
    // The watchdog starts before the child launches, so every path reaps.
    let watchdog = Watchdog::matching("fiber-message-definition-1");
    // Without an `Ask` the starts carry no action.
    let output = rig.tool.run(
        &resume_args(&id.0, "go again"),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(content(&output), resumed_text(&id.0, &output_path));
    assert!(output.jobs.is_empty());
    let taken = call(&output.settle.clone().expect("a resume settles"));
    assert_eq!(taken.len(), 4);
    assert!(taken[2].action.is_none() && taken[3].action.is_none());
    stop_and_reap(&rig, &id, watchdog);

    // With an `Ask` the starts carry the call's action; the receipt is
    // the same.
    let rig = resume_rig(
        vec![LaunchReply::Block("fiber-message-definition-2".into())],
        vec![(Vec::new(), Watched::Closed)],
        Arc::new(|_, _| Ok(None)),
    );
    let output_path = format!("{}/sessions/s_2/events.jsonl", rig._dir.path().display());
    let (id, finish) = open_resume_delegate(&rig, &output_path);
    end_resume_run(finish, &id, "Done.");
    let watchdog = Watchdog::matching("fiber-message-definition-2");
    let ask = FakeAsk {
        asked: AtomicBool::new(false),
    };
    let output = rig.tool.run_asking(
        &resume_args(&id.0, "go again"),
        &CancelToken::new(),
        &Recorder::default(),
        &ask,
    );
    assert!(output.error.is_none());
    assert_eq!(content(&output), resumed_text(&id.0, &output_path));
    assert!(!ask.asked.load(Ordering::SeqCst), "the tool never asks");
    let taken = call(&output.settle.clone().expect("a resume settles"));
    assert_eq!(taken.len(), 4);
    assert_eq!(taken[2].action, Some(action()));
    assert_eq!(taken[3].action, Some(action()));
    stop_and_reap(&rig, &id, watchdog);
}

#[test]
fn a_resume_reports_its_launch_and_queues_both_runs() {
    let rig = resume_rig(
        vec![LaunchReply::Block("fiber-message-resume".into())],
        vec![(
            vec![exited_line(7, "Old."), exited_line(9, "New.")],
            Watched::Exited,
        )],
        Arc::new(|_, _| Ok(Some(7))),
    );
    let output_path = format!("{}/sessions/s_9/events.jsonl", rig._dir.path().display());
    let (id, finish) = open_resume_delegate(&rig, &output_path);
    end_resume_run(finish, &id, "Done.");
    let _run1 = take_notice(&rig);
    let watchdog = Watchdog::matching("fiber-message-resume");
    let ask = FakeAsk {
        asked: AtomicBool::new(false),
    };
    let output = rig.tool.run_asking(
        &resume_args(&id.0, "tell me more"),
        &CancelToken::new(),
        &Recorder::default(),
        &ask,
    );
    assert!(output.error.is_none());
    assert_eq!(content(&output), resumed_text(&id.0, &output_path));
    assert!(output.jobs.is_empty());
    // The launch got the resume: the same session and the message as
    // prompt. A resume carries no model: the child continues the
    // recorded session.
    let launched = rig.launched.lock().unwrap();
    assert_eq!(launched.len(), 1);
    assert!(launched[0].resume);
    assert_eq!(launched[0].session_id.0, "s_d000000000000001");
    assert_eq!(launched[0].job_id, id);
    assert_eq!(launched[0].parent.0, "s_parent");
    assert_eq!(launched[0].depth, 1);
    assert_eq!(launched[0].prompt, "tell me more");
    assert_eq!(launched[0].workspace, std::env::current_dir().unwrap());
    drop(launched);
    // The boundary read ran on the delegate's session.
    assert_eq!(
        rig.last_seq_calls.lock().unwrap().as_slice(),
        [SessionId("s_d000000000000001".into())],
    );
    // The settle writes both runs: the news with no action, the starts
    // with the resuming call's.
    let taken = call(&output.settle.clone().expect("a resume settles"));
    assert_eq!(taken.len(), 4);
    assert!(taken[0].action.is_none() && taken[1].action.is_none());
    assert_eq!(taken[2].action, Some(action()));
    assert_eq!(taken[3].action, Some(action()));
    assert!(call(&output.settle.clone().expect("a resume settles")).is_empty());
    // The watch got the boundary the log read gave, and delivered the
    // live run's end past it.
    fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rig.delivered)
        .expect("the watch delivered");
    assert_eq!(
        rig.watch_calls.lock().unwrap().as_slice(),
        [(SessionId("s_d000000000000001".into()), Some(7))],
    );
    // The replayed earlier run's end never stands in for the new run's:
    // the run ends on the live exit once the watch delivered it.
    let run2 = stop_and_reap(&rig, &id, watchdog);
    assert_eq!(
        run2.delegate.map(|finished| finished.text),
        Some("New.".into())
    );
}

#[test]
fn a_resume_after_a_wait_takes_only_the_new_run_start() {
    let rig = resume_rig(
        vec![LaunchReply::Block("fiber-message-waited".into())],
        vec![(Vec::new(), Watched::Closed)],
        Arc::new(|_, _| Ok(None)),
    );
    let output_path = format!("{}/sessions/s_w/events.jsonl", rig._dir.path().display());
    let (id, finish) = open_resume_delegate(&rig, &output_path);
    end_resume_run(finish, &id, "Done.");
    let waited = rig
        .registry
        .wait(&id.0, 0, &CancelToken::new())
        .expect("run 1 ended");
    assert_eq!(
        call(&waited.settle.expect("an ended wait settles")).len(),
        2
    );
    let watchdog = Watchdog::matching("fiber-message-waited");
    let output = rig.tool.run(
        &resume_args(&id.0, "go again"),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert!(output.jobs.is_empty());
    assert_eq!(call(&output.settle.expect("a resume settles")).len(), 2);
    stop_and_reap(&rig, &id, watchdog);
}

#[test]
fn a_dropped_resume_output_loses_no_start() {
    let rig = resume_rig(
        vec![LaunchReply::Block("fiber-message-dropped".into())],
        vec![(Vec::new(), Watched::Closed)],
        Arc::new(|_, _| Ok(None)),
    );
    let output_path = format!("{}/sessions/s_d/events.jsonl", rig._dir.path().display());
    let (id, finish) = open_resume_delegate(&rig, &output_path);
    end_resume_run(finish, &id, "Done.");
    let _run1 = take_notice(&rig);
    let watchdog = Watchdog::matching("fiber-message-dropped");
    let output = rig.tool.run(
        &resume_args(&id.0, "go again"),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    // The settle is never called: run 2 ends, and its notice takes
    // everything ahead of its own news.
    let run2 = stop_and_reap(&rig, &id, watchdog);
    assert!(run2.delegate.is_some(), "run 2 finished");
    assert_eq!(
        (run2.claim.0)(),
        Some(vec![
            taken_record(&id, "Done.", None),
            taken_completed(&id),
            taken_started(&id, &output_path, None),
            taken_delegate_started(&id, None),
        ])
    );
}

#[test]
fn two_calls_resume_in_turn() {
    let rig = resume_rig(
        vec![
            LaunchReply::Block("fiber-message-two-1".into()),
            LaunchReply::Block("fiber-message-two-2".into()),
        ],
        vec![(Vec::new(), Watched::Closed), (Vec::new(), Watched::Closed)],
        Arc::new(|_, _| Ok(None)),
    );
    let output_path = format!("{}/sessions/s_2/events.jsonl", rig._dir.path().display());
    let (id, finish) = open_resume_delegate(&rig, &output_path);
    end_resume_run(finish, &id, "Done 1.");
    let _run1 = take_notice(&rig);
    // Call 1 resumes to run 2; its settle is not yet called.
    let watchdog = Watchdog::matching("fiber-message-two-1");
    let first = rig.tool.run(
        &resume_args(&id.0, "first"),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(first.error.is_none());
    let run2 = stop_and_reap(&rig, &id, watchdog);
    // Call 2 resumes to run 3 with one more launch. No refusal.
    let watchdog = Watchdog::matching("fiber-message-two-2");
    let second = rig.tool.run(
        &resume_args(&id.0, "second"),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(second.error.is_none());
    assert_eq!(rig.launched.lock().unwrap().len(), 2);
    assert_eq!(
        call(&first.settle.expect("call 1 settles")),
        vec![
            taken_record(&id, "Done 1.", None),
            taken_completed(&id),
            taken_started(&id, &output_path, None),
            taken_delegate_started(&id, None),
        ]
    );
    let run2_finished = run2.delegate.clone().expect("run 2 finished");
    assert_eq!(
        call(&second.settle.expect("call 2 settles")),
        vec![
            Taken {
                record: contract::jobs::JobRecord::DelegateFinished(run2_finished),
                action: None,
            },
            Taken {
                record: contract::jobs::JobRecord::Completed(run2.completed.clone()),
                action: None,
            },
            taken_started(&id, &output_path, None),
            taken_delegate_started(&id, None),
        ]
    );
    // Run 2's notice holds nothing new: its news left with the settles.
    assert_eq!((run2.claim.0)(), None);
    stop_and_reap(&rig, &id, watchdog);
}

#[test]
fn a_spawn_failure_starts_nothing_and_a_second_call_resumes() {
    let rig = resume_rig(
        vec![
            LaunchReply::Missing,
            LaunchReply::Block("fiber-message-respawn".into()),
        ],
        vec![(Vec::new(), Watched::Closed)],
        Arc::new(|_, _| Ok(None)),
    );
    let output_path = format!("{}/sessions/s_f/events.jsonl", rig._dir.path().display());
    let (id, finish) = open_resume_delegate(&rig, &output_path);
    end_resume_run(finish, &id, "Done.");
    let output = rig.tool.run(
        &resume_args(&id.0, "go again"),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code_of(&output), ErrorCode::ToolError);
    assert_eq!(
        content(&output),
        "Resuming the delegate failed: No such file or directory (os error 2).\n"
    );
    // No start is queued: run 1's news is still queued.
    let waited = rig
        .registry
        .wait(&id.0, 0, &CancelToken::new())
        .expect("run 1 ended");
    assert_eq!(
        call(&waited.settle.expect("an ended wait settles")).len(),
        2
    );
    // A second call then resumes.
    let watchdog = Watchdog::matching("fiber-message-respawn");
    let output = rig.tool.run(
        &resume_args(&id.0, "go again"),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(content(&output), resumed_text(&id.0, &output_path));
    stop_and_reap(&rig, &id, watchdog);
}

#[test]
fn a_boundary_read_that_failed_starts_nothing_though_the_log_is_back_before_the_spawn() {
    // A committed seam the test reaches from outside: the read answers
    // from a flag, signals the read, and blocks until released.
    let denied = Arc::new(AtomicBool::new(true));
    let reading = Arc::clone(&denied);
    let (read_tx, read_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let output_path = "/sessions/s_b/events.jsonl".to_owned();
    let log = output_path.clone();
    let last_seq: LastSeq = Arc::new(move |_: &std::path::Path, id: &SessionId| {
        let _ = id;
        if reading.load(Ordering::SeqCst) {
            let _sent = read_tx.send(());
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(&release_rx.lock().unwrap())
                .expect("the test releases the read");
            return Err(format!("{log}: Permission denied (os error 13)"));
        }
        Ok(Some(7))
    });
    let rig = resume_rig(
        vec![LaunchReply::Block("fiber-message-deny".into())],
        vec![(
            vec![exited_line(7, "Old."), exited_line(9, "New.")],
            Watched::Exited,
        )],
        last_seq,
    );
    let (id, finish) = open_resume_delegate(&rig, &output_path);
    end_resume_run(finish, &id, "Done.");
    let _run1 = take_notice(&rig);
    let tool = Arc::clone(&rig.tool);
    let args = resume_args(&id.0, "go again");
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let output = tool.run(&args, &CancelToken::new(), &Recorder::default());
        let _sent = tx.send(output);
    });
    fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&read_rx)
        .expect("the boundary read ran");
    // Access is restored before the spawn.
    denied.store(false, Ordering::SeqCst);
    release_tx.send(()).unwrap();
    let output = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the call answered");
    handle.join().unwrap();
    assert_eq!(code_of(&output), ErrorCode::ToolError);
    assert_eq!(
        content(&output),
        format!(
            "Resuming delegate {} failed: its log could not be read: {}: Permission denied (os error 13). Nothing was started.\n",
            id.0, output_path
        )
    );
    assert!(rig.launched.lock().unwrap().is_empty(), "nothing launched");
    assert!(matches!(
        rig.registry.target(&id.0),
        crate::registry::runs::Target::Ended
    ));
    let waited = rig
        .registry
        .wait(&id.0, 0, &CancelToken::new())
        .expect("run 1 ended");
    assert_eq!(
        call(&waited.settle.expect("an ended wait settles")).len(),
        2
    );

    // A second call reads past the restored log and launches on it: the
    // stale replay never stands in for the new run's end.
    let watchdog = Watchdog::matching("fiber-message-deny");
    let output = rig.tool.run(
        &resume_args(&id.0, "go again"),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(rig.launched.lock().unwrap().len(), 1);
    fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rig.delivered)
        .expect("the watch delivered");
    assert_eq!(
        rig.watch_calls.lock().unwrap().as_slice(),
        [(delegate_started(&id.0).delegate_session_id, Some(7))],
    );
    let run2 = stop_and_reap(&rig, &id, watchdog);
    assert_eq!(
        run2.delegate.map(|finished| finished.text),
        Some("New.".into()),
        "run 1's replayed exit never stands in for run 2's end"
    );
}

#[test]
fn closing_resumes_once_the_run_ends_before_the_bound() {
    let rig = resume_rig_with_steer(
        vec![LaunchReply::Block("fiber-message-closing".into())],
        vec![(Vec::new(), Watched::Closed)],
        Arc::new(|_, _| Ok(None)),
        vec![ResumeSteer::Closing],
    );
    let output_path = format!("{}/sessions/s_c/events.jsonl", rig._dir.path().display());
    let (id, finish) = open_resume_delegate(&rig, &output_path);
    let watchdog = Watchdog::matching("fiber-message-closing");
    let (rx, handle) = call_async(&rig.tool, &id.0, "hi there", CancelToken::new());
    let at = rig.clock.now() + BOUND;
    assert!(
        rig.clock
            .await_parked(at, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the call parks its closing wait"
    );
    end_resume_run(finish, &id, "Done.");
    let _run1 = take_notice(&rig);
    let output = answered(&rx);
    handle.join().unwrap();
    assert!(output.error.is_none());
    assert_eq!(content(&output), resumed_text(&id.0, &output_path));
    assert_eq!(rig.launched.lock().unwrap().len(), 1);
    stop_and_reap(&rig, &id, watchdog);
}

#[test]
fn gone_resumes_once_the_run_ends_during_the_backoff() {
    let rig = resume_rig_with_steer(
        vec![LaunchReply::Block("fiber-message-gone".into())],
        vec![(Vec::new(), Watched::Closed)],
        Arc::new(|_, _| Ok(None)),
        vec![ResumeSteer::GoneUncommitted],
    );
    let output_path = format!("{}/sessions/s_g/events.jsonl", rig._dir.path().display());
    let (id, finish) = open_resume_delegate(&rig, &output_path);
    let watchdog = Watchdog::matching("fiber-message-gone");
    let (rx, handle) = call_async(&rig.tool, &id.0, "hi", CancelToken::new());
    assert!(
        rig.clock.await_parked(
            rig.clock.now() + Duration::from_millis(50),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "the call parks its backoff"
    );
    end_resume_run(finish, &id, "Done.");
    let _run1 = take_notice(&rig);
    let output = answered(&rx);
    handle.join().unwrap();
    assert!(output.error.is_none());
    assert_eq!(content(&output), resumed_text(&id.0, &output_path));
    assert_eq!(rig.launched.lock().unwrap().len(), 1);
    assert_eq!(*rig.steer_calls.lock().unwrap(), 1);
    stop_and_reap(&rig, &id, watchdog);
}

#[test]
fn closing_after_a_committed_gone_resumes_once_the_run_ends() {
    let rig = resume_rig_with_steer(
        vec![LaunchReply::Block("fiber-message-closing-gone".into())],
        vec![(Vec::new(), Watched::Closed)],
        Arc::new(|_, _| Ok(None)),
        vec![ResumeSteer::Gone, ResumeSteer::Closing],
    );
    let output_path = format!("{}/sessions/s_cg/events.jsonl", rig._dir.path().display());
    let (id, finish) = open_resume_delegate(&rig, &output_path);
    let watchdog = Watchdog::matching("fiber-message-closing-gone");
    let (rx, handle) = call_async(&rig.tool, &id.0, "hi", CancelToken::new());
    let backoff = rig.clock.now() + Duration::from_millis(50);
    assert!(
        rig.clock
            .await_parked(backoff, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the call parks its backoff"
    );
    rig.clock.advance_when_parked(
        Some(backoff),
        None,
        Duration::from_millis(50),
        &fakes::Deadline::after(MUST_SUCCEED_WITHIN),
    );
    // The closing rejection clears the committed mark; the closing wait
    // follows on the session clock.
    let at = rig.clock.now() + BOUND;
    assert!(
        rig.clock
            .await_parked(at, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the call parks its closing wait"
    );
    end_resume_run(finish, &id, "Done.");
    let _run1 = take_notice(&rig);
    let output = answered(&rx);
    handle.join().unwrap();
    assert!(output.error.is_none());
    assert_eq!(content(&output), resumed_text(&id.0, &output_path));
    assert_eq!(rig.launched.lock().unwrap().len(), 1);
    stop_and_reap(&rig, &id, watchdog);
}

#[test]
fn a_cancel_before_the_resume_starts_nothing() {
    let rig = resume_rig(
        vec![LaunchReply::Block("fiber-message-cancelled".into())],
        vec![(Vec::new(), Watched::Closed)],
        Arc::new(|_, _| Ok(None)),
    );
    let output_path = format!("{}/sessions/s_q/events.jsonl", rig._dir.path().display());
    let (id, finish) = open_resume_delegate(&rig, &output_path);
    end_resume_run(finish, &id, "Done.");
    // The run ended and the call is about to resume; the cancel is set
    // before the call reaches the resume.
    let cancel = CancelToken::new();
    cancel.cancel();
    let output = rig.tool.run(
        &resume_args(&id.0, "go again"),
        &cancel,
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(
        content(&output),
        format!(
            "The call was cancelled before the message reached delegate {}; nothing was delivered.",
            id.0
        )
    );
    assert!(rig.launched.lock().unwrap().is_empty(), "nothing launched");
    assert!(output.settle.is_none());
}

#[test]
fn a_cancel_inside_the_start_still_commits_the_resume() {
    let (hold_tx, hold_rx) = mpsc::channel();
    let (reached_tx, reached_rx) = mpsc::channel();
    let rig = resume_rig(
        vec![LaunchReply::Hold {
            marker: "fiber-message-cancel-start".into(),
            release: Mutex::new(hold_rx),
            reached: reached_tx,
        }],
        vec![(Vec::new(), Watched::Closed)],
        Arc::new(|_, _| Ok(None)),
    );
    let output_path = format!("{}/sessions/s_cs/events.jsonl", rig._dir.path().display());
    let (id, finish) = open_resume_delegate(&rig, &output_path);
    end_resume_run(finish, &id, "Done.");
    let _run1 = take_notice(&rig);
    // The call is inside `resume`, held in its `start` closure, when
    // the cancel fires.
    let watchdog = Watchdog::matching("fiber-message-cancel-start");
    let cancel = CancelToken::new();
    let (rx, handle) = call_async(&rig.tool, &id.0, "go again", cancel.clone());
    fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&reached_rx)
        .expect("the start held");
    cancel.cancel();
    hold_tx.send(()).unwrap();
    let output = answered(&rx);
    handle.join().unwrap();
    // The resume committed: the receipt with its settle, though the
    // call was cancelled. A run that exists is always logged.
    assert!(output.error.is_none());
    assert_eq!(content(&output), resumed_text(&id.0, &output_path));
    assert!(output.jobs.is_empty());
    let taken = call(&output.settle.expect("a committed resume settles"));
    assert_eq!(
        taken,
        vec![
            taken_record(&id, "Done.", None),
            taken_completed(&id),
            taken_started(&id, &output_path, Some(action())),
            taken_delegate_started(&id, Some(action())),
        ]
    );
    assert_eq!(rig.launched.lock().unwrap().len(), 1);
    stop_and_reap(&rig, &id, watchdog);
}
