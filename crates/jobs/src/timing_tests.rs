//! `jobs.start` scopes for new and resumed delegates: the stamp before
//! `Runner::new` and the guard after the runner thread's spawn, through
//! the registry-held timing. Time comes from the session's injected
//! clock, never the process clock; every wait below is a channel receive
//! or a parked-clock wait with a wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::SessionId;
use contract::clock::Clock as _;
use contract::diag::{Op, Timing};
use contract::events::JobCompleted;
use contract::inbox::Delivery;
use contract::jobs::{JobRecord, Jobs as _, Stop};
use contract::tool::Tool as _;
use fakes::clock::{FakeClock, Miss};
use fakes::{CancelToken, Deadline, MUST_SUCCEED_WITHIN, Recorder, TempDir, Watchdog};
use serde_json::{Map, Value, json};

use crate::delegate::message::Attempt;
use crate::delegate::run::{Launch, Launched};
use crate::delegate::{DelegateMessage, DelegateSpawn, LastSeq, Resolve, Watch};
use crate::registry::Registry;
use crate::support::{delegate_started, opening};

/// The stop bound and closing wait the tests run with, on the fake clock.
const BOUND: Duration = Duration::from_secs(5);

/// One closed `jobs.start` scope: the operation, the tool (always none),
/// the whole milliseconds its guard held, how many jobs were running when
/// it closed, and whether the lock probe passed inside the drop.
#[derive(Debug)]
struct Record {
    op: Op,
    tool: Option<String>,
    ms: u64,
    running: usize,
    probe_ok: bool,
}

/// A test-only `Timing`: each call counts itself, and each guard closes
/// one record from the stamp to the drop's `now`. Inside the drop a second
/// thread reaches the registry's listing (its inner lock) and a resume
/// (its resume gate, then its inner lock): holding either lock across the
/// drop fails that receive, so the record proves the emission holds no
/// lock.
struct Probe {
    calls: Arc<AtomicUsize>,
    clock: Arc<FakeClock>,
    registry: Arc<Registry>,
    records: mpsc::Sender<Record>,
}

impl Probe {
    fn recording(
        clock: Arc<FakeClock>,
        registry: Arc<Registry>,
        records: mpsc::Sender<Record>,
    ) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            clock,
            registry,
            records,
        }
    }

    fn timing(&self) -> Timing {
        let calls = Arc::clone(&self.calls);
        let clock = Arc::clone(&self.clock);
        let registry = Arc::clone(&self.registry);
        let records = self.records.clone();
        Arc::new(
            move |op: Op, tool: Option<&str>, started: Option<Instant>| {
                calls.fetch_add(1, Ordering::SeqCst);
                let start = started.unwrap_or_else(|| clock.now());
                let scope = Scope {
                    clock: Arc::clone(&clock),
                    registry: Arc::clone(&registry),
                    records: records.clone(),
                    op,
                    tool: tool.map(str::to_owned),
                    start,
                };
                Some(Box::new(scope) as Box<dyn Send>)
            },
        )
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

struct Scope {
    clock: Arc<FakeClock>,
    registry: Arc<Registry>,
    records: mpsc::Sender<Record>,
    op: Op,
    tool: Option<String>,
    start: Instant,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let end = self.clock.now();
        let ms = u64::try_from(end.saturating_duration_since(self.start).as_millis())
            .unwrap_or(u64::MAX);
        // The job is recorded running before its thread's spawn returns,
        // so the close already sees it.
        let running = contract::jobs::Jobs::running(self.registry.as_ref()).len();
        // The emission holds no registry, shell, steering-gate or
        // resume-gate lock: a second thread reaches the inner lock and
        // the resume gate from inside this drop, with a deadline, so a
        // bug fails the test instead of hanging it.
        let (tx, rx) = mpsc::channel();
        let registry = Arc::clone(&self.registry);
        thread::spawn(move || {
            let _ = registry.list_text();
            match registry.resume("j_unknown", None, no_start) {
                Ok(_) | Err(_) => {}
            }
            match tx.send(()) {
                Ok(()) | Err(_) => {}
            }
        });
        // A deadline, so a bug fails the test instead of hanging it.
        let probe_ok = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .is_ok();
        let record = Record {
            op: self.op,
            tool: self.tool.clone(),
            ms,
            running,
            probe_ok,
        };
        match self.records.send(record) {
            Ok(()) | Err(_) => {}
        }
    }
}

/// The start a lock probe never runs: the probed resume names an unknown
/// job, so the registry answers before any start.
fn no_start(_: &crate::registry::runs::Resumable) -> Result<(Stop, ()), String> {
    panic!("an unknown job starts nothing")
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Stops `id` through the jobs seam: the inherent `stop` with the same
/// name waits instead.
fn halt(registry: &Registry, id: &contract::JobId) -> bool {
    contract::jobs::Jobs::stop(registry, id)
}

/// One fixture: its directory, its fake clock, a registry on that clock
/// with an inbox, and a recording probe the test installs when the case
/// needs timing.
struct Fixture {
    _dir: TempDir,
    clock: Arc<FakeClock>,
    registry: Arc<Registry>,
    inbox: mpsc::Receiver<Delivery>,
    sessions: std::path::PathBuf,
}

impl Fixture {
    fn new(prefix: &str) -> Self {
        let dir = TempDir::new(prefix);
        let clock = FakeClock::new();
        let artifacts = dir.path().join("artifacts");
        std::fs::create_dir(&artifacts).unwrap();
        let sessions = dir.path().join("sessions");
        std::fs::create_dir(&sessions).unwrap();
        let registry = Registry::new(
            artifacts,
            Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
            Arc::new(Recorder::default()),
        );
        let (inbox_tx, inbox_rx) = mpsc::channel();
        registry.deliver_to(inbox_tx);
        Self {
            _dir: dir,
            clock,
            registry,
            inbox: inbox_rx,
            sessions,
        }
    }

    fn probe(&self) -> (Probe, mpsc::Receiver<Record>) {
        let (tx, rx) = mpsc::channel();
        (
            Probe::recording(Arc::clone(&self.clock), Arc::clone(&self.registry), tx),
            rx,
        )
    }

    fn spawner(&self, launch: Launch, watch: Watch) -> DelegateSpawn {
        DelegateSpawn {
            resolve: resolve(),
            wiring: crate::DelegateWiring {
                create: Arc::new(
                    #[allow(
                        clippy::result_large_err,
                        reason = "the jobs boundary returns its typed error by value"
                    )]
                    |_, _| panic!("unexpected worktree creation"),
                ),
                finish: Arc::new(|end| contract::events::FinishedWorktree {
                    worktree: contract::shapes::Worktree {
                        path: end.path.to_string_lossy().into_owned(),
                        branch: end.branch,
                        repository: None,
                    },
                    dirty: false,
                }),
                depth: 0,
                registry: Arc::clone(&self.registry),
                parent: SessionId("s_parent".into()),
                workspace: self._dir.path().to_path_buf(),
                sessions: {
                    let sessions = self.sessions.clone();
                    Arc::new(move |_| sessions.clone())
                },
                clock: Arc::clone(&self.clock) as Arc<dyn contract::clock::Clock>,
                bound: BOUND,
                cap: 1024,
                launch,
                watch,
            },
        }
    }

    fn messenger(&self, launch: Launch, watch: Watch, last_seq: LastSeq) -> DelegateMessage {
        DelegateMessage {
            finish: Arc::new(|end| contract::events::FinishedWorktree {
                worktree: contract::shapes::Worktree {
                    path: end.path.to_string_lossy().into_owned(),
                    branch: end.branch,
                    repository: None,
                },
                dirty: false,
            }),
            depth: 0,
            registry: Arc::clone(&self.registry),
            parent: SessionId("s_parent".into()),
            clock: Arc::clone(&self.clock) as Arc<dyn contract::clock::Clock>,
            bound: BOUND,
            cap: 1024,
            launch,
            watch,
            steer: Arc::new(|_, _, _, _, _| panic!("no steer on this path")),
            last_seq,
        }
    }
}

fn resolve() -> Resolve {
    Resolve {
        lookup: Arc::new(|model: &str| match model {
            "fiber:fake/m" => Ok("fake/m".to_owned()),
            _ => Err(format!("Unknown delegate model {model:?}.")),
        }),
        catalogue: Arc::from("References:\nfiber:fake/m"),
        credential: Arc::new(|_| Ok(None)),
    }
}

/// A launch that signals `reached`, blocks on `release`, then runs a
/// `read line` child holding `marker` in its command line: the child
/// blocks on the stdin lifeline the runner holds. When the test is gone
/// the spawn fails closed, so no child is left behind.
fn hold_launch(marker: &str) -> (Launch, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (reached_tx, reached_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(release_rx);
    let marker = marker.to_owned();
    let launch: Launch = Arc::new(move |_: &Launched| {
        match reached_tx.send(()) {
            Ok(()) | Err(_) => {}
        }
        if lock(&release).recv().is_err() {
            return Command::new("/nonexistent/fiber-p10-launch");
        }
        let mut command = Command::new("sh");
        command.args(["-c", &format!(": {marker}; read line")]);
        command
    });
    (launch, reached_rx, release_tx)
}

/// A launch that runs its child at once.
fn block_launch(body: &str) -> Launch {
    let body = body.to_owned();
    Arc::new(move |_: &Launched| {
        let mut command = Command::new("sh");
        command.args(["-c", &body]);
        command
    })
}

/// A watch that signals `entered`, blocks on `release`, then answers
/// `Err(refused)`. The drive thread stays inside the watch until the test
/// releases it; when the test is gone the watch refuses at once.
fn holding_watch() -> (Watch, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(release_rx);
    let watch: Watch = Arc::new(move |_, _, _| {
        match entered_tx.send(()) {
            Ok(()) | Err(_) => {}
        }
        match lock(&release).recv() {
            Ok(()) | Err(_) => {}
        }
        Err(std::io::Error::other("refused"))
    });
    (watch, entered_rx, release_tx)
}

/// A watch that refuses at once.
fn refused_watch() -> Watch {
    Arc::new(|_, _, _| Err(std::io::Error::other("refused")))
}

/// The spawn's arguments.
fn spawn_args() -> Map<String, Value> {
    match json!({"description": "scan", "prompt": "say hello", "model": "fiber:fake/m"}) {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            panic!("arguments are an object")
        }
    }
}

/// Runs `spawn` on its own thread: the drive's launch hold blocks there,
/// and the result arrives on the channel. The thread is never joined: the
/// watchdog owns the child, and the deadline fails a stuck call.
fn spawn_async(
    spawn: DelegateSpawn,
    arguments: Map<String, Value>,
    cancel: CancelToken,
) -> mpsc::Receiver<contract::tool::Output> {
    let (out_tx, out_rx) = mpsc::channel();
    thread::spawn(move || {
        let output = spawn.run(&arguments, &cancel, &Recorder::default());
        match out_tx.send(output) {
            Ok(()) | Err(_) => {}
        }
    });
    out_rx
}

/// Resumes `id` on its own thread, for a held launch.
fn resume_async(
    tool: Arc<DelegateMessage>,
    id: String,
    cancel: CancelToken,
) -> mpsc::Receiver<Attempt> {
    let (out_tx, out_rx) = mpsc::channel();
    thread::spawn(move || {
        let attempt = tool.resume_now(&id, "again", &None, &cancel);
        match out_tx.send(attempt) {
            Ok(()) | Err(_) => {}
        }
    });
    out_rx
}

/// The call's answer when the resume committed: panics otherwise.
fn done(attempt: Attempt) -> contract::tool::Output {
    match attempt {
        Attempt::Done(output) => output,
        Attempt::Again(_) => panic!("expected the receipt, not a retry"),
        Attempt::Found(_, _) => panic!("expected the receipt, not a live run"),
    }
}

/// One record, failing the test when the scope never closed.
fn closed(records: &mpsc::Receiver<Record>, deadline: Deadline) -> Record {
    deadline.recv_or_fail(records, "the closed scope")
}

/// No record is waiting.
fn assert_none(records: &mpsc::Receiver<Record>, what: &str) {
    assert!(records.try_recv().is_err(), "{what}: no scope closed");
}

/// One wait inside `try_advance_when_parked` for a fresh park: a poll
/// pause, not a deadline. At most 100 ms, so a driver whose runner left
/// its park without a clock move sees its end condition within one pause.
/// Shared by every poll driver in `crates/jobs` tests: each driver moves
/// time only through `try_step` below.
pub(crate) const PAUSE: Duration = Duration::from_millis(100);

/// One `try_advance_when_parked` call's outcome: the move landed, no fresh
/// park arrived within the pause, or a park at another instant was
/// watched and is carried into the next pass.
pub(crate) enum Step {
    Moved,
    NoPark,
    Next(Instant),
}

/// One `try_advance_when_parked` call at the instant the driver read or
/// carried this pass, moving `advance` and no further. Never checks a
/// deadline and never moves time itself: the driver's own pass top fails
/// at its deadline. A watched park with no deadline panics naming it:
/// the runner never parks without one.
pub(crate) fn try_step(clock: &FakeClock, until: Instant, advance: Duration) -> Step {
    match clock.try_advance_when_parked(Some(until), None, advance, PAUSE) {
        Ok(_) => Step::Moved,
        Err(Miss::NoPark) => Step::NoPark,
        Err(Miss::Mismatch {
            watched: (_, Some(watched)),
            ..
        }) => Step::Next(watched),
        Err(
            miss @ Miss::Mismatch {
                watched: (_, None), ..
            },
        ) => {
            panic!("{miss}; now={:?} parked={:?}", clock.now(), clock.parked());
        }
    }
}

/// Takes the run's end, re-running the runner's reap pass while it holds
/// its retry park at `park`: the drain's end of file can beat the zombie,
/// and only a clock wake re-runs the reap. The driver advances zero, which
/// moves no time and changes no deadline; it only wakes the parked pass.
/// `park` is the drive start plus the watch retry backoff. Modelled on
/// `reported_before` in `delegate/run_tests.rs`. The driver names `park`
/// every pass: a mismatch at any other instant is dropped, so there is no
/// carried instant to keep. The driver runs in the
/// scope, so no wait outlives the call: a panic in the driver fails the
/// test at the scope.
fn end_after_reap(
    fixture: &Fixture,
    park: Instant,
    deadline: Deadline,
    what: &str,
) -> JobCompleted {
    let stop = AtomicBool::new(false);
    let clock = Arc::clone(&fixture.clock);
    thread::scope(|scope| {
        scope.spawn(|| {
            loop {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                if deadline.left().is_zero() {
                    panic!("{what}; now={:?} parked={:?}", clock.now(), clock.parked());
                }
                // Only the park this call waits for is named: a mismatch
                // at any other instant is dropped, and the next pass names
                // `park` again.
                match try_step(&clock, park, Duration::ZERO) {
                    Step::Moved | Step::NoPark | Step::Next(_) => {}
                }
            }
        });
        let ended = expect_end(&fixture.inbox, deadline, what);
        stop.store(true, Ordering::SeqCst);
        ended
    })
}

/// The run's end from the inbox.
fn expect_end(inbox: &mpsc::Receiver<Delivery>, deadline: Deadline, what: &str) -> JobCompleted {
    match deadline.recv_or_fail(inbox, what) {
        Delivery::Job(notice) => notice.completed,
        other @ (Delivery::Prompt(..)
        | Delivery::Steer(..)
        | Delivery::SteerDrop(..)
        | Delivery::Message(..)
        | Delivery::Model(..)
        | Delivery::Reload(..)
        | Delivery::McpReconnect(..)
        | Delivery::Credential(..)
        | Delivery::Name(..)
        | Delivery::Rewind(..)
        | Delivery::Handoff(..)
        | Delivery::Reply(..)
        | Delivery::Close(_)
        | Delivery::JobLine(_)
        | Delivery::ExtensionExec(_)
        | Delivery::Interaction(_)
        | Delivery::Resolved(..)
        | Delivery::InnerCall(_)
        | Delivery::ExtensionLog(_)
        | Delivery::ProposeNotes(..)
        | Delivery::ExtensionJob(..)
        | Delivery::ExtensionUsage(_)
        | Delivery::DelegateUsage { .. }
        | Delivery::Shell(..)
        | Delivery::Cancelled) => panic!("expected a job end, got {other:?}"),
    }
}

/// The two records a started delegate carries.
fn started_records(
    output: &contract::tool::Output,
) -> (
    contract::events::JobStarted,
    contract::events::DelegateStarted,
) {
    match output.jobs.as_slice() {
        [
            JobRecord::Started(started),
            JobRecord::DelegateStarted(delegate),
        ] => (started.clone(), delegate.clone()),
        other => panic!("expected a start and a delegate start, got {other:?}"),
    }
}

fn text(output: &contract::tool::Output) -> String {
    match output.content.first() {
        Some(contract::shapes::ContentPart::Text { text }) => text.clone(),
        _ => String::new(),
    }
}

/// Holds the launch for one new delegate, advances the fake clock by
/// `step` inside the start work, and releases it.
fn through_launch(
    clock: &FakeClock,
    step: Duration,
    reached: &mpsc::Receiver<()>,
    release: &mpsc::Sender<()>,
) {
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    deadline.recv_or_fail(reached, "the spawn inside its start work");
    // No code under test waits on the clock here: the launch closure is held on a channel and the runner thread does not exist yet; the advance stamps the elapsed milliseconds the record reports (ruling 6179406086).
    clock.advance(step);
    release.send(()).unwrap();
}

#[test]
fn a_new_delegate_times_spawn_to_running() {
    let fixture = Fixture::new("fiber-p10-new");
    let marker = "fiber-p10-new";
    let _watchdog = Watchdog::matching(marker);
    let (probe, records) = fixture.probe();
    fixture.registry.install_timing(probe.timing());
    let (launch, reached, release) = hold_launch(marker);
    let (watch, entered, watch_release) = holding_watch();
    let spawn = fixture.spawner(launch, watch);
    let output_rx = spawn_async(spawn, spawn_args(), CancelToken::new());
    through_launch(&fixture.clock, Duration::from_millis(7), &reached, &release);
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    let output = deadline.recv_or_fail(&output_rx, "the receipt");
    let (started, delegate) = started_records(&output);
    assert_eq!(started.description, "scan");
    assert_eq!(started.tool.as_deref(), Some("delegate_spawn"));
    assert_eq!(delegate.harness, "fiber");
    // The drive thread reached its watch and stays there: the record
    // below arrives while the watch is still held.
    deadline.recv_or_fail(&entered, "the drive inside its watch");
    assert!(
        fixture.inbox.try_recv().is_err(),
        "no end while the watch is held"
    );
    assert_eq!(probe.calls(), 1);
    let record = closed(&records, deadline);
    assert_eq!((record.op, record.ms), (Op::JobsStart, 7));
    assert_eq!(record.tool, None, "no tool name on a generic start");
    assert_eq!(record.running, 1, "recorded before the guard closed");
    assert!(record.probe_ok, "the emission holds no lock");
    assert_none(&records, "the start");
    // Releasing the watch, stopping the job and receiving its end add no
    // record and change nothing.
    watch_release.send(()).unwrap();
    halt(&fixture.registry, &started.job_id);
    let ended = end_after_reap(
        &fixture,
        fixture.clock.origin() + Duration::from_millis(57),
        deadline,
        "the run end",
    );
    assert_eq!(ended.job_id, started.job_id);
    assert_eq!(probe.calls(), 1, "the end writes nothing");
    assert_none(&records, "the end");
}

#[test]
fn a_resumed_delegate_times_the_new_run() {
    let fixture = Fixture::new("fiber-p10-resume");
    let first = "fiber-p10-resume-first";
    let _first_watchdog = Watchdog::matching(first);
    let (probe, records) = fixture.probe();
    fixture.registry.install_timing(probe.timing());
    // A first spawn whose child exits at once, its end received.
    let spawn = fixture.spawner(block_launch(&format!(": {first}; exit 0")), refused_watch());
    let output = spawn.run(&spawn_args(), &CancelToken::new(), &Recorder::default());
    let (started, _) = started_records(&output);
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    end_after_reap(
        &fixture,
        fixture.clock.origin() + Duration::from_millis(50),
        deadline,
        "the run end",
    );
    // Then a resume with a held launch and a 9 ms step inside its start.
    // The marker carries a unique suffix: the bare `fiber-p10-resume`
    // prefix-matches other fixtures' markers (`resumeunknown`,
    // `resumefail`, `resumeboundary`, `resumecancel`), and a watchdog
    // dropping on one would kill another test's processes.
    let marker = "fiber-p10-resumerun";
    let _watchdog = Watchdog::matching(marker);
    let (launch, reached, release) = hold_launch(marker);
    let tool = Arc::new(fixture.messenger(launch, refused_watch(), Arc::new(|_| Ok(None))));
    let attempt_rx = resume_async(tool, started.job_id.0.clone(), CancelToken::new());
    through_launch(&fixture.clock, Duration::from_millis(9), &reached, &release);
    let attempt = deadline.recv_or_fail(&attempt_rx, "the resumed receipt");
    let output = done(attempt);
    assert!(
        text(&output).starts_with(&format!("Resumed delegate {}", started.job_id.0)),
        "{}",
        text(&output)
    );
    // The records are the first spawn's and one more with 9 ms.
    assert_eq!(probe.calls(), 2);
    let first_record = closed(&records, deadline);
    assert_eq!((first_record.op, first_record.ms), (Op::JobsStart, 0));
    assert!(first_record.probe_ok);
    let second = closed(&records, deadline);
    assert_eq!((second.op, second.ms), (Op::JobsStart, 9));
    assert_eq!(second.tool, None);
    assert_eq!(second.running, 1);
    assert!(second.probe_ok, "the resume emission holds no lock");
    assert_none(&records, "the resume");
    halt(&fixture.registry, &started.job_id);
    end_after_reap(
        &fixture,
        fixture.clock.origin() + Duration::from_millis(59),
        deadline,
        "the run end",
    );
    assert_eq!(probe.calls(), 2, "the resumed end writes nothing");
    assert_none(&records, "the resumed end");
}

#[test]
fn refused_and_failed_starts_write_nothing() {
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    // A missing model, a refused model and missing arguments: no spawn.
    for (name, arguments) in [
        (
            "a missing model",
            json!({"description": "scan", "prompt": "say hello"}),
        ),
        (
            "a refused model",
            json!({"description": "scan", "prompt": "say hello", "model": "fiber:fake/unknown"}),
        ),
        (
            "missing arguments",
            json!({"prompt": "say hello", "model": "fiber:fake/m"}),
        ),
    ] {
        let fixture = Fixture::new("fiber-p10-refused");
        let _watchdog = Watchdog::matching("fiber-p10-refused");
        let (probe, records) = fixture.probe();
        fixture.registry.install_timing(probe.timing());
        let spawn = fixture.spawner(
            block_launch(": fiber-p10-refused; read line"),
            refused_watch(),
        );
        let arguments = match arguments {
            Value::Object(map) => map,
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_) => panic!("arguments are an object"),
        };
        let output = spawn.run(&arguments, &CancelToken::new(), &Recorder::default());
        assert!(output.error.is_some(), "{name}: refused");
        assert!(output.jobs.is_empty(), "{name}: no job");
        assert_eq!(probe.calls(), 0, "{name}");
        assert_none(&records, name);
    }
    // A spawn failure, which also covers the group-listing refusal arm.
    {
        let fixture = Fixture::new("fiber-p10-spawnfail");
        let (probe, records) = fixture.probe();
        fixture.registry.install_timing(probe.timing());
        let launch: Launch = Arc::new(|_: &Launched| Command::new("/nonexistent/fiber-p10-launch"));
        let spawn = fixture.spawner(launch, refused_watch());
        let output = spawn.run(&spawn_args(), &CancelToken::new(), &Recorder::default());
        assert!(output.error.is_some(), "the spawn failed");
        assert!(
            text(&output).contains("Starting the delegate failed"),
            "{}",
            text(&output)
        );
        assert_eq!(probe.calls(), 0);
        assert_none(&records, "a spawn failure");
    }
    // A pre-cancelled resume starts nothing.
    {
        let fixture = Fixture::new("fiber-p10-resumecancel");
        let (probe, records) = fixture.probe();
        fixture.registry.install_timing(probe.timing());
        let tool = fixture.messenger(
            block_launch(": fiber-p10-resumecancel; read line"),
            refused_watch(),
            Arc::new(|_| Ok(None)),
        );
        let cancel = CancelToken::new();
        cancel.cancel();
        let attempt = tool.resume_now("j_unknown", "again", &None, &cancel);
        let output = done(attempt);
        assert!(
            text(&output).contains("nothing was delivered"),
            "{}",
            text(&output)
        );
        assert_eq!(probe.calls(), 0);
        assert_none(&records, "a pre-cancelled resume");
    }
    // A resume of an unknown job, a non-delegate shell job and a live
    // delegate: the start closure never runs.
    {
        let fixture = Fixture::new("fiber-p10-resumeunknown");
        let marker = "fiber-p10-resumeunknown";
        let _watchdog = Watchdog::matching(marker);
        // The live delegate is spawned untimed, then timing is installed:
        // its start wrote nothing countable, and the resume must write
        // nothing either.
        let live = fixture.spawner(
            block_launch(&format!(": {marker}; read line")),
            refused_watch(),
        );
        let live_output = live.run(&spawn_args(), &CancelToken::new(), &Recorder::default());
        let (live_started, _) = started_records(&live_output);
        let (probe, records) = fixture.probe();
        fixture.registry.install_timing(probe.timing());
        let tool = fixture.messenger(
            block_launch(": fiber-p10-resumeunknown; read line"),
            refused_watch(),
            Arc::new(|_| Ok(None)),
        );
        let attempt = tool.resume_now("j_unknown", "again", &None, &CancelToken::new());
        let output = done(attempt);
        assert!(output.error.is_some(), "unknown: {}", text(&output));
        let shell = fixture.registry.open(opening("a shell job")).unwrap();
        let shell_id = shell.started.job_id.0.clone();
        let attempt = tool.resume_now(&shell_id, "again", &None, &CancelToken::new());
        let output = done(attempt);
        assert!(
            text(&output).contains("is not a delegate"),
            "{}",
            text(&output)
        );
        let live_id = live_started.job_id.0.clone();
        let attempt = tool.resume_now(&live_id, "again", &None, &CancelToken::new());
        assert!(
            matches!(attempt, Attempt::Found(_, _)),
            "a live delegate steers instead of resuming"
        );
        assert_eq!(probe.calls(), 0);
        assert_none(&records, "unknown, shell and live resumes");
        (shell.end.0)(crate::support::ended_ok(&shell_id));
        let shell_end = expect_end(&fixture.inbox, deadline, "the shell end");
        assert_eq!(shell_end.job_id.0, shell_id);
        halt(&fixture.registry, &live_started.job_id);
        let live_end = end_after_reap(
            &fixture,
            fixture.clock.origin() + Duration::from_millis(50),
            deadline,
            "the live end",
        );
        assert_eq!(live_end.job_id, live_started.job_id);
    }
    // A failed boundary read and a resume spawn failure start nothing.
    for (name, last_seq, launch) in [
        (
            "a failed boundary read",
            Arc::new(|_: &SessionId| Err("its log could not be read: /gone".to_owned())) as LastSeq,
            block_launch(": fiber-p10-resumeboundary; read line"),
        ),
        (
            "a resume spawn failure",
            Arc::new(|_: &SessionId| Ok(None)) as LastSeq,
            Arc::new(|_: &Launched| Command::new("/nonexistent/fiber-p10-launch")) as Launch,
        ),
    ] {
        let fixture = Fixture::new("fiber-p10-resumefail");
        let (probe, records) = fixture.probe();
        // The first run is untimed; the failing resume is timed.
        let first = fixture.spawner(
            block_launch(": fiber-p10-resumefail; exit 0"),
            refused_watch(),
        );
        let output = first.run(&spawn_args(), &CancelToken::new(), &Recorder::default());
        let (started, _) = started_records(&output);
        end_after_reap(
            &fixture,
            fixture.clock.origin() + Duration::from_millis(50),
            deadline,
            "the first run end",
        );
        fixture.registry.install_timing(probe.timing());
        let tool = fixture.messenger(launch, refused_watch(), last_seq);
        let attempt = tool.resume_now(&started.job_id.0, "again", &None, &CancelToken::new());
        let output = done(attempt);
        assert!(output.error.is_some(), "{name}: {}", text(&output));
        assert_eq!(probe.calls(), 0, "{name}");
        assert_none(&records, name);
    }
}

#[test]
fn a_pre_cancelled_spawn_still_starts_and_times_once() {
    let fixture = Fixture::new("fiber-p10-precancel");
    let marker = "fiber-p10-precancel";
    let _watchdog = Watchdog::matching(marker);
    let (probe, records) = fixture.probe();
    fixture.registry.install_timing(probe.timing());
    let spawn = fixture.spawner(
        block_launch(&format!(": {marker}; read line")),
        refused_watch(),
    );
    // Unchanged behaviour: no cancel check before the start.
    let cancel = CancelToken::new();
    cancel.cancel();
    let output = spawn.run(&spawn_args(), &cancel, &Recorder::default());
    let (started, _) = started_records(&output);
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(probe.calls(), 1);
    let record = closed(&records, deadline);
    assert_eq!((record.op, record.ms), (Op::JobsStart, 0));
    assert_eq!(record.tool, None);
    assert!(record.probe_ok);
    assert_none(&records, "the start");
    halt(&fixture.registry, &started.job_id);
    end_after_reap(
        &fixture,
        fixture.clock.origin() + Duration::from_millis(50),
        deadline,
        "the run end",
    );
    assert_eq!(probe.calls(), 1, "the end writes nothing");
}

#[test]
fn registration_is_not_timed() {
    let fixture = Fixture::new("fiber-p10-registration");
    let (probe, records) = fixture.probe();
    fixture.registry.install_timing(probe.timing());
    let opened = fixture.registry.open(opening("a shell job")).unwrap();
    let id = crate::registry::mint_job_id();
    let (_started, finish) = fixture.registry.open_started(
        id,
        "delegate_spawn".into(),
        "scan".into(),
        "sessions/s_child/events.jsonl".into(),
        Stop(Box::new(|| {})),
        delegate_started("j_child"),
    );
    drop(opened.end);
    drop(finish);
    assert_eq!(probe.calls(), 0, "neither open is a start");
    assert_none(&records, "registration");
}

#[test]
fn without_timing_nothing_is_called() {
    let fixture = Fixture::new("fiber-p10-untimed");
    let marker = "fiber-p10-untimed";
    let _watchdog = Watchdog::matching(marker);
    // Nothing installed: the registry answers no timing.
    assert!(fixture.registry.timing().is_none());
    let (probe, records) = fixture.probe();
    let spawn = fixture.spawner(
        block_launch(&format!(": {marker}; read line")),
        refused_watch(),
    );
    let output = spawn.run(&spawn_args(), &CancelToken::new(), &Recorder::default());
    let (started, _) = started_records(&output);
    assert_eq!(probe.calls(), 0, "the probe was never installed");
    assert_none(&records, "untimed");
    halt(&fixture.registry, &started.job_id);
    end_after_reap(
        &fixture,
        fixture.clock.origin() + Duration::from_millis(50),
        Deadline::after(MUST_SUCCEED_WITHIN),
        "the run end",
    );
}

#[test]
fn a_second_install_is_ignored() {
    let fixture = Fixture::new("fiber-p10-second");
    let marker = "fiber-p10-second";
    let _watchdog = Watchdog::matching(marker);
    let (first, first_records) = fixture.probe();
    let (second, second_records) = fixture.probe();
    fixture.registry.install_timing(first.timing());
    fixture.registry.install_timing(second.timing());
    let spawn = fixture.spawner(
        block_launch(&format!(": {marker}; read line")),
        refused_watch(),
    );
    let output = spawn.run(&spawn_args(), &CancelToken::new(), &Recorder::default());
    let (started, _) = started_records(&output);
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(first.calls(), 1);
    assert_eq!(second.calls(), 0, "the second installation never runs");
    closed(&first_records, deadline);
    assert_none(&first_records, "the start");
    assert_none(&second_records, "the second timing hears nothing");
    halt(&fixture.registry, &started.job_id);
    end_after_reap(
        &fixture,
        fixture.clock.origin() + Duration::from_millis(50),
        deadline,
        "the run end",
    );
}

#[test]
fn untimed_and_timed_spawns_agree() {
    let run = |prefix: &str, timed: bool| {
        let fixture = Fixture::new(prefix);
        let washed_dir = fixture._dir.path().display().to_string();
        let marker = "fiber-p10-agree-marker";
        let _watchdog = Watchdog::matching(marker);
        let (probe, records) = fixture.probe();
        if timed {
            fixture.registry.install_timing(probe.timing());
        }
        let spawn = fixture.spawner(
            block_launch(&format!(": {marker}; read line")),
            refused_watch(),
        );
        let output = spawn.run(&spawn_args(), &CancelToken::new(), &Recorder::default());
        let (started, delegate) = started_records(&output);
        halt(&fixture.registry, &started.job_id);
        let ended = end_after_reap(
            &fixture,
            fixture.clock.origin() + Duration::from_millis(50),
            Deadline::after(MUST_SUCCEED_WITHIN),
            "the run end",
        );
        // The receipt names this run's own ids and directory: washed
        // before compare.
        let washed = text(&output)
            .replace(&washed_dir, "DIR")
            .replace(&started.job_id.0, "ID")
            .replace(&delegate.delegate_session_id.0, "SID");
        (
            washed,
            started.description,
            started.tool,
            delegate.harness,
            delegate.model,
            ended.status,
            probe.calls(),
            records.try_recv().is_ok(),
        )
    };
    let untimed = run("fiber-p10-agree-a", false);
    let timed = run("fiber-p10-agree-b", true);
    assert_eq!(untimed.6, 0, "untimed installs nothing");
    assert_eq!(timed.6, 1, "timed starts once");
    assert!(!untimed.7, "untimed hears nothing");
    assert!(timed.7, "timed closes one scope");
    assert_eq!(untimed.0, timed.0, "the same receipt text");
    assert_eq!(untimed.1, timed.1, "the same description");
    assert_eq!(untimed.2, timed.2, "the same tool");
    assert_eq!(untimed.3, timed.3, "the same harness");
    assert_eq!(untimed.4, timed.4, "the same model");
    assert_eq!(untimed.5, timed.5, "the same end status");
}
