//! `jobs.start` scopes for shell background starts and moves: the stamp in
//! [`crate::Shell::run`], the entry stamp in `take`, and the guard in
//! `hand_off` after the job thread's spawn. Time comes from the session's
//! injected clock, never the process clock; every wait below is a channel
//! receive, a watchdog, or a parked-clock wait with a wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock as _;
use contract::diag::{Op, Timing};
use contract::inbox::Delivery;
use contract::jobs::{Foreground, JobRecord, Jobs, Opening};
use contract::tool::Tool;
use fakes::children::Ready;
use fakes::clock::FakeClock;
use fakes::{CancelToken, Deadline, Recorder, TempDir, Watchdog};
use serde_json::{Map, Value, json};

use crate::Shell;

/// How long a test waits on the wall clock before it fails.
const STEP: Duration = Duration::from_secs(10);

/// The fake-clock step held inside adoption setup: every record below
/// proves its span from it.
const TICK: Duration = Duration::from_millis(5);

/// Thirty seconds of fake time: the foreground move point.
const THIRTY: Duration = Duration::from_secs(30);

/// A terminal's first-output wait (`crates/tools/src/shell/tty.rs`): the
/// test advances past it only once the drive parks there.
const FIRST_OUTPUT: Duration = Duration::from_millis(250);

/// One closed `jobs.start` scope: the operation, the tool (always none),
/// the whole milliseconds its guard held, and how many jobs were recorded
/// when it closed.
#[derive(Debug)]
struct Record {
    op: Op,
    tool: Option<String>,
    ms: u64,
    jobs: usize,
}

/// A test-only `Timing`: each call counts itself, and each guard closes
/// one record from the stamp (or the call's `now`) to the drop's `now`.
struct Probe {
    calls: Arc<AtomicUsize>,
    clock: Arc<FakeClock>,
    jobs: Arc<fakes::jobs::FakeJobs>,
    records: mpsc::Sender<Record>,
}

impl Probe {
    fn recording(
        clock: Arc<FakeClock>,
        jobs: Arc<fakes::jobs::FakeJobs>,
        records: mpsc::Sender<Record>,
    ) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            clock,
            jobs,
            records,
        }
    }

    fn timing(&self) -> Timing {
        let calls = Arc::clone(&self.calls);
        let clock = Arc::clone(&self.clock);
        let jobs = Arc::clone(&self.jobs);
        let records = self.records.clone();
        Arc::new(
            move |op: Op, tool: Option<&str>, started: Option<Instant>| {
                calls.fetch_add(1, Ordering::SeqCst);
                let start = started.unwrap_or_else(|| clock.now());
                let scope = Scope {
                    clock: Arc::clone(&clock),
                    jobs: Arc::clone(&jobs),
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
    jobs: Arc<fakes::jobs::FakeJobs>,
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
        // The job is recorded before its thread's spawn returns, so the
        // record's close already sees it.
        let jobs = self.jobs.started().len();
        let record = Record {
            op: self.op,
            tool: self.tool.clone(),
            ms,
            jobs,
        };
        match self.records.send(record) {
            Ok(()) | Err(_) => {}
        }
    }
}

/// A `Jobs` that forwards to a [`fakes::jobs::FakeJobs`] and, when armed,
/// signals `reached` from `open` and blocks on `release`, so the test
/// advances the clock by a known step only after the call is inside
/// adoption setup.
struct Holding {
    inner: Arc<fakes::jobs::FakeJobs>,
    hold: AtomicBool,
    reached: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl Holding {
    fn wrap(
        inner: Arc<fakes::jobs::FakeJobs>,
    ) -> (Arc<Self>, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let holding = Arc::new(Self {
            inner: Arc::clone(&inner),
            hold: AtomicBool::new(false),
            reached: reached_tx,
            release: Mutex::new(release_rx),
        });
        (holding, reached_rx, release_tx)
    }

    fn arm(&self) {
        self.hold.store(true, Ordering::SeqCst);
    }
}

impl Jobs for Holding {
    fn open(&self, opening: Opening) -> Result<contract::jobs::Opened, contract::jobs::OpenError> {
        if self.hold.load(Ordering::SeqCst) {
            match self.reached.send(()) {
                Ok(()) | Err(_) => {}
            }
            let release = lock(&self.release);
            match release.recv() {
                Ok(()) | Err(_) => {}
            }
        }
        self.inner.open(opening)
    }

    fn write(
        &self,
        job_id: &contract::JobId,
        text: &str,
    ) -> Result<(), contract::jobs::WriteError> {
        self.inner.write(job_id, text)
    }

    fn stop(&self, job_id: &contract::JobId) -> bool {
        self.inner.stop(job_id)
    }

    fn stop_delegates(&self) -> usize {
        self.inner.stop_delegates()
    }

    fn background(&self) -> usize {
        self.inner.background()
    }

    fn foreground(&self, call: Foreground) {
        self.inner.foreground(call);
    }

    fn running(&self) -> Vec<contract::JobId> {
        self.inner.running()
    }

    fn deliver_to(&self, inbox: mpsc::Sender<Delivery>) {
        self.inner.deliver_to(inbox);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One fixture: its directory, its fake clock at its origin, a recording
/// probe over `jobs`, and the records channel the test owns.
struct Fixture {
    dir: TempDir,
    clock: Arc<FakeClock>,
    origin: Instant,
    probe: Probe,
    records: mpsc::Receiver<Record>,
    jobs: Arc<fakes::jobs::FakeJobs>,
}

impl Fixture {
    fn new(prefix: &str) -> Self {
        let dir = TempDir::new(prefix);
        let clock = FakeClock::new();
        let origin = clock.origin();
        let jobs = fakes::jobs::FakeJobs::new(dir.path());
        let (tx, rx) = mpsc::channel();
        let probe = Probe::recording(Arc::clone(&clock), Arc::clone(&jobs), tx);
        Self {
            dir,
            clock,
            origin,
            probe,
            records: rx,
            jobs,
        }
    }

    /// The model's shell: timed, moving into `holding`.
    fn shell(&self, holding: Arc<Holding>) -> Arc<Shell> {
        Arc::new(
            Shell::new(self.dir.path().to_path_buf(), Arc::clone(&self.clock) as _)
                .with_jobs(holding as _)
                .with_timing(self.probe.timing()),
        )
    }
}

/// The call's arguments for `command` with `flag` set.
fn args(command: &str, flag: &str) -> Map<String, Value> {
    let mut arguments = Map::new();
    arguments.insert("command".into(), json!(command));
    arguments.insert(flag.into(), json!(true));
    arguments
}

/// Runs `shell` on its own thread: the first send names the drive thread,
/// the second carries the result. The thread is never joined: the
/// watchdog owns the child, and the deadline fails a stuck call.
fn run_async(
    shell: Arc<Shell>,
    arguments: Map<String, Value>,
    cancel: CancelToken,
) -> (
    mpsc::Receiver<thread::ThreadId>,
    mpsc::Receiver<contract::tool::Output>,
) {
    let (id_tx, id_rx) = mpsc::channel();
    let (out_tx, out_rx) = mpsc::channel();
    thread::spawn(move || {
        match id_tx.send(thread::current().id()) {
            Ok(()) | Err(_) => {}
        }
        let output = shell.run(&arguments, &cancel, &Recorder::default());
        match out_tx.send(output) {
            Ok(()) | Err(_) => {}
        }
    });
    (id_rx, out_rx)
}

// P2-2: a second fifo the child blocks on after its ready line, until the
// test releases it, so a group watchdog armed in between covers every
// path. Created beside `ready`; see `Ready::new`.
fn release_fifo(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("release.fifo");
    fakes::fifo(&path);
    path
}

/// Releases a child held at `release_fifo`: one line unblocks its read.
/// Opened read-write, so the write never blocks on the child's arrival.
fn release_child(path: &std::path::Path) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("the release fifo opens");
    file.write_all(b"go\n").expect("the release line");
}

/// Holds `path` open read-write, so a line written before the child opens
/// the fifo for reading waits in the buffer instead of being lost when
/// the last descriptor closes.
fn hold_release_fifo(path: &std::path::Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("the release fifo opens")
}

/// One record, failing the test when the scope never closed.
fn closed(records: &mpsc::Receiver<Record>, deadline: Deadline) -> Record {
    deadline.recv_or_fail(records, "the closed scope")
}

/// No record is waiting.
fn assert_none(records: &mpsc::Receiver<Record>, what: &str) {
    assert!(records.try_recv().is_err(), "{what}: no scope closed");
}

/// Stops `id` and receives its end, panicking with the registry and process
/// state when the end never arrives.
#[track_caller]
fn stop_and_end(
    jobs: &Arc<fakes::jobs::FakeJobs>,
    id: &contract::JobId,
    marker: &str,
) -> contract::events::JobCompleted {
    assert!(jobs.stop(id), "the stop sent");
    match jobs.ended(fakes::Deadline::after(STEP)) {
        Some(ended) => ended,
        None => {
            let live = fakes::matching(marker).unwrap_or_default();
            panic!(
                "the stopped job to end: running={:?} started={} live={:?}",
                jobs.running(),
                jobs.started().len(),
                live,
            );
        }
    }
}

/// The one started job a background receipt names.
fn started(output: &contract::tool::Output) -> contract::events::JobStarted {
    match output.jobs.as_slice() {
        [JobRecord::Started(started)] => started.clone(),
        other => panic!("expected one started job, got {other:?}"),
    }
}

/// Holds `open` for one background start, advances the fake clock by
/// `TICK` inside adoption setup, and releases it.
fn through_open(fixture: &Fixture, reached: &mpsc::Receiver<()>, release: &mpsc::Sender<()>) {
    let deadline = Deadline::after(STEP);
    deadline.recv_or_fail(reached, "the call inside adoption setup");
    // No code under test waits on the clock here; the call is held inside jobs.open and the job's drive thread starts after it returns, so this move is the time the jobs.start scope measures (ruling 6179406086).
    fixture.clock.advance(TICK);
    release.send(()).unwrap();
}

#[test]
fn run_in_background_times_adoption_setup_only() {
    let fixture = Fixture::new("fiber-p10-bg");
    let marker = "fiber-p10-bg";
    let _watchdog = Watchdog::matching(marker);
    // The child's own acknowledgement: it writes its pid to this fifo
    // once started, so the test waits for the child's signal, never the
    // process table.
    let ready = Ready::new(fixture.dir.path());
    let release_path = release_fifo(fixture.dir.path());
    let (holding, reached, release) = Holding::wrap(Arc::clone(&fixture.jobs));
    holding.arm();
    let shell = fixture.shell(holding);
    // The fifo lives in the workspace, the call's own workdir, so the
    // relative name resolves there and the description keeps its shape.
    let command =
        format!(": {marker}; echo $$ > ready.fifo; read -r _ < release.fifo; exec sleep 60");
    let (_ids, output_rx) = run_async(
        shell,
        args(&command, "run_in_background"),
        CancelToken::new(),
    );
    through_open(&fixture, &reached, &release);
    let deadline = Deadline::after(STEP);
    let output = deadline.recv_or_fail(&output_rx, "the receipt");

    assert!(output.error.is_none(), "{}", fakes::text_of(&output));
    assert!(
        fakes::text_of(&output).starts_with("Started in the background.\n"),
        "{}",
        fakes::text_of(&output)
    );
    let job = started(&output);
    assert_eq!(
        fixture.jobs.started(),
        vec![job.clone()],
        "one job, recorded"
    );
    assert_eq!(job.description, command);
    assert_eq!(job.tool.as_deref(), Some("shell"));
    // The acknowledged child is held at the release fifo until its
    // group is watched, so every path reaps it; releasing lets it exec.
    let child = ready.wait(fakes::Deadline::after(STEP));
    assert!(!child.is_empty(), "the job's shell acknowledged its start");
    // The marker cannot match an exec'd child on Linux, so the group is
    // watched by id: the pid is the group id after `setsid`.
    let _group = Watchdog::group(child[0]);
    release_child(&release_path);
    assert_eq!(fixture.probe.calls(), 1);
    let record = closed(&fixture.records, deadline);
    assert_eq!((record.op, record.ms), (Op::JobsStart, 5));
    assert_eq!(record.tool, None);
    assert_eq!(record.jobs, 1, "recorded before the guard closed");
    assert_none(&fixture.records, "the start");
    assert!(
        !format!("{record:?}").contains(marker),
        "no command text in the record"
    );
    // The job's later life adds nothing.
    let ended = stop_and_end(&fixture.jobs, &job.job_id, marker);
    assert_eq!(ended.status, contract::events::Outcome::Cancelled);
    assert_eq!(fixture.probe.calls(), 1, "the end writes nothing");
    assert_none(&fixture.records, "the end");
}

#[test]
fn tty_times_the_start_before_the_first_output_wait() {
    let fixture = Fixture::new("fiber-p10-tty");
    let marker = "fiber-p10-tty";
    let _watchdog = Watchdog::matching(marker);
    // The child's own acknowledgement: it writes its pid to this fifo
    // once started, so the test waits for the child's signal, never the
    // process table.
    let ready = Ready::new(fixture.dir.path());
    let release_path = release_fifo(fixture.dir.path());
    let (holding, reached, release) = Holding::wrap(Arc::clone(&fixture.jobs));
    holding.arm();
    let shell = fixture.shell(holding);
    let command = format!(
        ": {marker}; echo out; echo $$ > ready.fifo; read -r _ < release.fifo; exec sleep 60"
    );
    let (id_rx, output_rx) = run_async(shell, args(&command, "tty"), CancelToken::new());
    through_open(&fixture, &reached, &release);
    let deadline = Deadline::after(STEP);
    let call_thread = deadline.recv_or_fail(&id_rx, "the drive thread");
    // The record arrives while the call is still waiting for its first
    // output: the result has not answered.
    let record = closed(&fixture.records, deadline);
    assert_eq!((record.op, record.ms), (Op::JobsStart, 5));
    assert_eq!(record.tool, None);
    assert!(
        output_rx.try_recv().is_err(),
        "the start closed before the receipt wait"
    );
    // The frozen clock holds the first-output wait: advancing past it,
    // once the drive parks there, lets the receipt finish. The thread is
    // named, because the job's own drive thread parks on this clock too.
    fixture.clock.advance_when_parked(
        Some(fixture.origin + TICK + FIRST_OUTPUT),
        Some(call_thread),
        FIRST_OUTPUT,
        &deadline,
    );
    let output = deadline.recv_or_fail(&output_rx, "the receipt");
    let job = started(&output);
    assert!(output.error.is_none(), "{}", fakes::text_of(&output));
    assert_eq!(fixture.probe.calls(), 1);
    assert_none(&fixture.records, "the start");
    let child = ready.wait(fakes::Deadline::after(STEP));
    assert!(!child.is_empty(), "the job acknowledged its start");
    let _group = Watchdog::group(child[0]);
    release_child(&release_path);
    stop_and_end(&fixture.jobs, &job.job_id, marker);
}

#[test]
fn monitor_times_its_start() {
    let fixture = Fixture::new("fiber-p10-monitor");
    let marker = "fiber-p10-monitor";
    let _watchdog = Watchdog::matching(marker);
    // The child's own acknowledgement: it writes its pid to this fifo
    // once started, so the test waits for the child's signal, never the
    // process table.
    let ready = Ready::new(fixture.dir.path());
    let release_path = release_fifo(fixture.dir.path());
    let (holding, reached, release) = Holding::wrap(Arc::clone(&fixture.jobs));
    holding.arm();
    let shell = fixture.shell(holding);
    let command =
        format!(": {marker}; echo $$ > ready.fifo; read -r _ < release.fifo; exec sleep 60");
    let (_ids, output_rx) = run_async(shell, args(&command, "monitor"), CancelToken::new());
    through_open(&fixture, &reached, &release);
    let deadline = Deadline::after(STEP);
    let output = deadline.recv_or_fail(&output_rx, "the receipt");
    let job = started(&output);
    assert!(output.error.is_none(), "{}", fakes::text_of(&output));
    assert_eq!(fixture.probe.calls(), 1);
    let record = closed(&fixture.records, deadline);
    assert_eq!((record.op, record.ms), (Op::JobsStart, 5));
    assert_eq!(record.tool, None);
    assert_eq!(record.jobs, 1);
    let child = ready.wait(fakes::Deadline::after(STEP));
    assert!(!child.is_empty(), "the job acknowledged its start");
    let _group = Watchdog::group(child[0]);
    release_child(&release_path);
    stop_and_end(&fixture.jobs, &job.job_id, marker);
    assert_none(&fixture.records, "the end");
}

#[test]
fn a_foreground_call_moved_after_thirty_seconds_times_the_move_only() {
    let fixture = Fixture::new("fiber-p10-thirty");
    let marker = "fiber-p10-thirty";
    let _watchdog = Watchdog::matching(marker);
    // The child's own acknowledgement: it writes its pid to this fifo
    // once started, so the test waits for the child's signal, never the
    // process table.
    let ready = Ready::new(fixture.dir.path());
    let release_path = release_fifo(fixture.dir.path());
    let (holding, reached, release) = Holding::wrap(Arc::clone(&fixture.jobs));
    holding.arm();
    let shell = fixture.shell(Arc::clone(&holding));
    let command =
        format!(": {marker}; echo $$ > ready.fifo; read -r _ < release.fifo; exec sleep 60");
    let mut arguments = Map::new();
    arguments.insert("command".into(), json!(command));
    let (id_rx, output_rx) = run_async(shell, arguments, CancelToken::new());
    let deadline = Deadline::after(STEP);
    let thread = deadline.recv_or_fail(&id_rx, "the drive thread");
    // The drive thread's park at start + 30 seconds: the move waits there.
    fixture.clock.advance_when_parked(
        Some(fixture.origin + THIRTY),
        Some(thread),
        THIRTY,
        &deadline,
    );
    through_open(&fixture, &reached, &release);
    let output = deadline.recv_or_fail(&output_rx, "the receipt");
    let job = started(&output);
    assert!(
        fakes::text_of(&output)
            .starts_with("Still running after 30 seconds, so it moved to the background.\n"),
        "{}",
        fakes::text_of(&output)
    );
    assert_eq!(fixture.probe.calls(), 1);
    let record = closed(&fixture.records, deadline);
    assert_eq!(
        (record.op, record.ms),
        (Op::JobsStart, 5),
        "the foreground's 30 seconds are never counted"
    );
    let child = ready.wait(fakes::Deadline::after(STEP));
    assert!(!child.is_empty(), "the job acknowledged its start");
    let _group = Watchdog::group(child[0]);
    release_child(&release_path);
    assert!(holding.stop(&job.job_id));
    stop_and_end(&fixture.jobs, &job.job_id, marker);
}

#[test]
fn the_background_command_moves_a_call_with_one_start() {
    let fixture = Fixture::new("fiber-p10-commanded");
    let marker = "fiber-p10-commanded";
    let _watchdog = Watchdog::matching(marker);
    // The child's own acknowledgement: it writes its pid to this fifo
    // once started, so the test waits for the child's signal, never the
    // process table.
    let ready = Ready::new(fixture.dir.path());
    let release_path = release_fifo(fixture.dir.path());
    let (holding, reached, release) = Holding::wrap(Arc::clone(&fixture.jobs));
    holding.arm();
    let shell = fixture.shell(Arc::clone(&holding));
    let command =
        format!(": {marker}; echo $$ > ready.fifo; read -r _ < release.fifo; exec sleep 60");
    let mut arguments = Map::new();
    arguments.insert("command".into(), json!(command));
    let (_ids, output_rx) = run_async(shell, arguments, CancelToken::new());
    let deadline = Deadline::after(STEP);
    assert!(
        fixture
            .clock
            .await_parked(fixture.origin + THIRTY, fakes::Deadline::after(STEP)),
        "the run did not park at 30 seconds"
    );
    assert_eq!(holding.background(), 1);
    through_open(&fixture, &reached, &release);
    let output = deadline.recv_or_fail(&output_rx, "the receipt");
    assert!(
        fakes::text_of(&output)
            .starts_with("Moved to the background by the `background` command.\n"),
        "{}",
        fakes::text_of(&output)
    );
    assert_eq!(fixture.probe.calls(), 1);
    let record = closed(&fixture.records, deadline);
    assert_eq!((record.op, record.ms), (Op::JobsStart, 5));
    let job = started(&output);
    let child = ready.wait(fakes::Deadline::after(STEP));
    assert!(!child.is_empty(), "the job acknowledged its start");
    let _group = Watchdog::group(child[0]);
    release_child(&release_path);
    assert!(holding.stop(&job.job_id));
    stop_and_end(&fixture.jobs, &job.job_id, marker);
}

#[test]
fn a_shell_exit_leaving_a_member_times_the_move() {
    let fixture = Fixture::new("fiber-p10-exited");
    let marker = "fiber-p10-exited";
    let _watchdog = Watchdog::matching(marker);
    let (holding, reached, release) = Holding::wrap(Arc::clone(&fixture.jobs));
    holding.arm();
    let shell = fixture.shell(holding);
    let command = format!(": {marker}; sleep 60 & exit 0");
    let mut arguments = Map::new();
    arguments.insert("command".into(), json!(command));
    let (_ids, output_rx) = run_async(shell, arguments, CancelToken::new());
    through_open(&fixture, &reached, &release);
    let deadline = Deadline::after(STEP);
    let output = deadline.recv_or_fail(&output_rx, "the receipt");
    assert!(
        fakes::text_of(&output).contains("so it moved to the background."),
        "{}",
        fakes::text_of(&output)
    );
    assert_eq!(fixture.probe.calls(), 1);
    let record = closed(&fixture.records, deadline);
    assert_eq!((record.op, record.ms), (Op::JobsStart, 5));
    let job = started(&output);
    stop_and_end(&fixture.jobs, &job.job_id, marker);
}

#[test]
fn failures_and_foreground_ends_write_nothing() {
    // A refused argument never runs.
    {
        let fixture = Fixture::new("fiber-p10-refused");
        let _watchdog = Watchdog::matching("fiber-p10-refused");
        let shell = Arc::new(
            Shell::new(
                fixture.dir.path().to_path_buf(),
                Arc::clone(&fixture.clock) as _,
            )
            .with_jobs(fixture.jobs.clone() as _)
            .with_timing(fixture.probe.timing()),
        );
        let output = shell.run(&Map::new(), &CancelToken::new(), &Recorder::default());
        assert!(output.error.is_some());
        assert_eq!(fixture.probe.calls(), 0);
        assert_none(&fixture.records, "a refused argument");
    }
    // No jobs: the background call is refused before any stamp.
    {
        let fixture = Fixture::new("fiber-p10-nojobs");
        let _watchdog = Watchdog::matching("fiber-p10-nojobs");
        let shell = Arc::new(
            Shell::new(
                fixture.dir.path().to_path_buf(),
                Arc::clone(&fixture.clock) as _,
            )
            .with_timing(fixture.probe.timing()),
        );
        let output = shell.run(
            &args(": fiber-p10-nojobs; sleep 60", "run_in_background"),
            &CancelToken::new(),
            &Recorder::default(),
        );
        assert!(fakes::text_of(&output).contains("Background jobs are not available"));
        assert_eq!(fixture.probe.calls(), 0);
        assert_none(&fixture.records, "no jobs");
    }
    // A spawn failure never moves.
    {
        let fixture = Fixture::new("fiber-p10-spawnfail");
        let _watchdog = Watchdog::matching("fiber-p10-spawnfail");
        let jail = fixture.dir.path().join("jail");
        std::fs::create_dir(&jail).unwrap();
        let mut permissions = std::fs::metadata(&jail).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o000);
        std::fs::set_permissions(&jail, permissions).unwrap();
        let shell = Arc::new(
            Shell::new(
                fixture.dir.path().to_path_buf(),
                Arc::clone(&fixture.clock) as _,
            )
            .with_jobs(fixture.jobs.clone() as _)
            .with_timing(fixture.probe.timing()),
        );
        let mut arguments = args(": fiber-p10-spawnfail; sleep 60", "run_in_background");
        arguments.insert("workdir".into(), json!(jail.display().to_string()));
        let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
        // Restored before the directory drops, whatever the result held.
        let mut permissions = std::fs::metadata(&jail).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&jail, permissions).unwrap();
        assert!(
            fakes::text_of(&output).contains("could not be started"),
            "{}",
            fakes::text_of(&output)
        );
        assert_eq!(fixture.probe.calls(), 0);
        assert_none(&fixture.records, "a spawn failure");
    }
    // A refused open keeps the foreground result and writes nothing.
    {
        let dir = TempDir::new("fiber-p10-openfail");
        let clock = FakeClock::new();
        let jobs = fakes::jobs::FakeJobs::failing();
        let (tx, rx) = mpsc::channel();
        let probe = Probe::recording(Arc::clone(&clock), Arc::clone(&jobs), tx);
        let _watchdog = Watchdog::matching("fiber-p10-openfail");
        let release_path = release_fifo(dir.path());
        // Opened before the shell runs and kept until the write below,
        // so the buffered line survives until the child reads it.
        let mut release_file = hold_release_fifo(&release_path);
        let (holding, reached, release) = Holding::wrap(Arc::clone(&jobs));
        holding.arm();
        let shell = Arc::new(
            Shell::new(dir.path().to_path_buf(), Arc::clone(&clock) as _)
                .with_jobs(holding as _)
                .with_timing(probe.timing()),
        );
        // The child blocks on the release fifo from spawn, so its group
        // is live whenever the first drive pass checks it.
        let command = ": fiber-p10-openfail; read -r _ < release.fifo; echo done".to_owned();
        let (_, output_rx) = run_async(
            shell,
            args(&command, "run_in_background"),
            CancelToken::new(),
        );
        Deadline::after(STEP).recv_or_fail(&reached, "the call inside the refused open");
        {
            use std::io::Write as _;
            release_file.write_all(b"go\n").expect("the release line");
        }
        match release.send(()) {
            Ok(()) | Err(_) => {}
        }
        let output = Deadline::after(STEP).recv_or_fail(&output_rx, "the foreground result");
        assert!(
            fakes::text_of(&output).contains("It could not move to the background"),
            "{}",
            fakes::text_of(&output)
        );
        assert_eq!(probe.calls(), 0);
        assert!(rx.try_recv().is_err(), "a refused open: no scope closed");
    }
    // A foreground call that finishes before 30 seconds never moves.
    {
        let fixture = Fixture::new("fiber-p10-quick");
        let _watchdog = Watchdog::matching("fiber-p10-quick");
        let shell = Arc::new(
            Shell::new(
                fixture.dir.path().to_path_buf(),
                Arc::clone(&fixture.clock) as _,
            )
            .with_jobs(fixture.jobs.clone() as _)
            .with_timing(fixture.probe.timing()),
        );
        let mut arguments = Map::new();
        arguments.insert("command".into(), json!("echo hi"));
        let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
        assert!(output.error.is_none(), "{}", fakes::text_of(&output));
        assert!(output.jobs.is_empty());
        assert_eq!(fixture.probe.calls(), 0);
        assert_none(&fixture.records, "a finished foreground call");
    }
    // A stamp read but never moved writes nothing: the timeout stops the
    // call on its first pass.
    {
        let fixture = Fixture::new("fiber-p10-nomove");
        let _watchdog = Watchdog::matching("fiber-p10-nomove");
        let shell = Arc::new(
            Shell::new(
                fixture.dir.path().to_path_buf(),
                Arc::clone(&fixture.clock) as _,
            )
            .with_jobs(fixture.jobs.clone() as _)
            .with_timing(fixture.probe.timing()),
        );
        let mut arguments = args(": fiber-p10-nomove; exec sleep 30", "run_in_background");
        arguments.insert("timeout_ms".into(), json!(0));
        let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
        assert!(output.jobs.is_empty());
        assert_eq!(fixture.probe.calls(), 0);
        assert_none(&fixture.records, "a call stopped before it moved");
    }
}

#[test]
fn a_pre_cancelled_call_writes_nothing() {
    let fixture = Fixture::new("fiber-p10-precancel");
    let _watchdog = Watchdog::matching("fiber-p10-precancel");
    let shell = Arc::new(
        Shell::new(
            fixture.dir.path().to_path_buf(),
            Arc::clone(&fixture.clock) as _,
        )
        .with_jobs(fixture.jobs.clone() as _)
        .with_timing(fixture.probe.timing()),
    );
    let cancel = CancelToken::new();
    cancel.cancel();
    let output = shell.run(
        &args(": fiber-p10-precancel; sleep 60", "run_in_background"),
        &cancel,
        &Recorder::default(),
    );
    assert!(
        fakes::text_of(&output).contains("Cancelled"),
        "{}",
        fakes::text_of(&output)
    );
    assert_eq!(fixture.probe.calls(), 0);
    assert_none(&fixture.records, "a pre-cancelled call");
}

#[test]
fn a_foreground_call_cancelled_before_it_moves_writes_nothing() {
    let fixture = Fixture::new("fiber-p10-fgcancel");
    let marker = "fiber-p10-fgcancel";
    let _watchdog = Watchdog::matching(marker);
    // The child's own acknowledgement: it writes its pid to this fifo
    // once started, so the test waits for the child's signal, never the
    // process table.
    let ready = Ready::new(fixture.dir.path());
    let release_path = release_fifo(fixture.dir.path());
    let jobs = fakes::jobs::FakeJobs::new(fixture.dir.path());
    let shell = Arc::new(
        Shell::new(
            fixture.dir.path().to_path_buf(),
            Arc::clone(&fixture.clock) as _,
        )
        .with_jobs(jobs.clone() as _)
        .with_timing(fixture.probe.timing()),
    );
    // The fifo lives in the workspace, the call's own workdir, so the
    // relative name resolves there and the description keeps its shape.
    // `exec` turns the shell into its last command under the same pid,
    // so the ready line proves the group's only process exists and no
    // process is created after it.
    let command =
        format!(": {marker}; echo $$ > ready.fifo; read -r _ < release.fifo; exec sleep 60");
    let mut arguments = Map::new();
    arguments.insert("command".into(), json!(command));
    let cancel = CancelToken::new();
    let (id_rx, out_rx) = run_async(Arc::clone(&shell), arguments, cancel.clone());
    // The child acknowledged its start: the call reached its drive. The
    // drive's park proves it waits in `Running`, past any fork, so
    // cancelling now stops it before any move. Each wait below keeps its
    // own wall-clock budget, so a slow spawn does not eat the result wait.
    let child = ready.wait(fakes::Deadline::after(STEP));
    assert!(!child.is_empty(), "the child acknowledged its start");
    // `exec` drops the marker from the command line, so the group is
    // watched by id: the pid is the group id after `setsid`.
    let _group = Watchdog::group(child[0]);
    release_child(&release_path);
    let call_thread = Deadline::after(STEP).recv_or_fail(&id_rx, "the drive thread");
    assert!(
        fixture.clock.await_thread_parked(
            call_thread,
            Some(fixture.origin + THIRTY),
            fakes::Deadline::after(STEP)
        ),
        "the drive parked in Running"
    );
    cancel.cancel();
    // Its own wall-clock budget: the deadline above was shared with the
    // start acknowledgement, so a slow spawn must not eat this wait.
    let output = Deadline::after(STEP).recv_or_fail(&out_rx, "the cancelled result");
    assert!(output.jobs.is_empty(), "cancelled before any move");
    assert!(jobs.started().is_empty());
    assert_eq!(fixture.probe.calls(), 0);
    assert_none(&fixture.records, "a cancelled foreground call");
}

#[test]
fn untimed_and_timed_background_receipts_agree() {
    let run = |prefix: &str, timed: bool| {
        let dir = TempDir::new(prefix);
        let washed = dir.path().display().to_string();
        let clock = FakeClock::new();
        let jobs = fakes::jobs::FakeJobs::new(dir.path());
        let (tx, _rx) = mpsc::channel();
        let probe = Probe::recording(Arc::clone(&clock), Arc::clone(&jobs), tx);
        let mut shell = Shell::new(dir.path().to_path_buf(), Arc::clone(&clock) as _)
            .with_jobs(jobs.clone() as _);
        if timed {
            shell = shell.with_timing(probe.timing());
        }
        let shell = Arc::new(shell);
        // One marker for both runs, so the compared descriptions agree.
        // The runs are sequential, and each watchdog drops with its run.
        let marker = "fiber-p10-agree-marker";
        let _watchdog = Watchdog::matching(marker);
        let ready = Ready::new(dir.path());
        let release_path = release_fifo(dir.path());
        let command =
            format!(": {marker}; echo $$ > ready.fifo; read -r _ < release.fifo; exec sleep 60");
        let (_ids, output_rx) = run_async(
            shell,
            args(&command, "run_in_background"),
            CancelToken::new(),
        );
        let output = Deadline::after(STEP).recv_or_fail(&output_rx, "the receipt");
        let job = started(&output);
        let child = ready.wait(fakes::Deadline::after(STEP));
        assert!(!child.is_empty(), "the job acknowledged its start");
        let _group = Watchdog::group(child[0]);
        release_child(&release_path);
        assert!(jobs.stop(&job.job_id));
        let ended = jobs
            .ended(fakes::Deadline::after(STEP))
            .expect("the stopped job to end");
        // The receipt names the run's own directory: washed before compare.
        let washed_text = fakes::text_of(&output).replace(&washed, "DIR");
        (washed_text, job, ended.status, probe.calls())
    };
    let (untimed_text, untimed_job, untimed_status, untimed_calls) =
        run("fiber-p10-agree-a", false);
    let (timed_text, timed_job, timed_status, timed_calls) = run("fiber-p10-agree-b", true);
    assert_eq!(untimed_calls, 0);
    assert_eq!(timed_calls, 1);
    assert_eq!(
        untimed_text.replace(&untimed_job.job_id.0, "ID"),
        timed_text.replace(&timed_job.job_id.0, "ID"),
        "the same receipt text"
    );
    assert_eq!(untimed_job.description, timed_job.description);
    assert_eq!(untimed_job.tool, timed_job.tool);
    assert_eq!(untimed_status, timed_status, "the same end status");
}
