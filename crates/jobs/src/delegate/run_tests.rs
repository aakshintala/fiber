//! Tests first for the runner: spawning, the drain, the watch loop, the
//! stop timer and the wait, with fake children and a fake watch.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::Duration;

use crate::delegate::group::listed;
use contract::Envelope;
use contract::clock::Clock as _;
use contract::events::{FiberExited, FinalMessage, Outcome};
use contract::inbox::Delivery;
use contract::jobs::Jobs as _;
use contract::shapes::{Failure, Tokens, Usage};
use contract::{ActionId, ErrorCode, JobId, Seq, SessionId};
use fakes::clock::FakeClock;
use fakes::{Recorder, TempDir, Watchdog, group_empties, kill_pid, pids_exit, within};

use super::{
    Launch, Launched, Runner, Watch, Watched, kill_due, mint_session_id, note_seq, park_due,
};
use crate::delegate::group::serial_shared;
use crate::registry::Registry;

/// How long a test waits on the wall clock before it fails.
const DEADLINE: Duration = Duration::from_secs(3);

/// The stop bound the tests run with, on the fake clock.
const BOUND: Duration = Duration::from_secs(5);

/// A fake watch: replays queued replies and records when it ran.
struct Script {
    clock: Arc<FakeClock>,
    calls: Mutex<Vec<std::time::Instant>>,
    /// Notified on every recorded call.
    called: Condvar,
    replies: Mutex<VecDeque<WatchReply>>,
}

enum WatchReply {
    Closed(Vec<Envelope>),
    Exited(Vec<Envelope>),
    /// Blocks until the test releases the gate, at most the wall bound:
    /// the runner must supervise without it. Dropping the sender releases
    /// it too, so a failed test never leaves the watcher parked.
    Block(mpsc::Receiver<()>),
}

impl Script {
    fn scripted(clock: &Arc<FakeClock>, replies: Vec<WatchReply>) -> Arc<Self> {
        Arc::new(Self {
            clock: Arc::clone(clock),
            calls: Mutex::new(Vec::new()),
            called: Condvar::new(),
            replies: Mutex::new(replies.into()),
        })
    }

    fn watch(
        self: &Arc<Self>,
        _id: &SessionId,
        on_line: &mut dyn FnMut(&Envelope),
    ) -> io::Result<Watched> {
        self.calls.lock().unwrap().push(self.clock.now());
        self.called.notify_all();
        match self.replies.lock().unwrap().pop_front() {
            None => Err(io::Error::other("refused")),
            Some(WatchReply::Closed(lines)) => {
                for line in &lines {
                    on_line(line);
                }
                Ok(Watched::Closed)
            }
            Some(WatchReply::Exited(lines)) => {
                for line in &lines {
                    on_line(line);
                }
                Ok(Watched::Exited)
            }
            Some(WatchReply::Block(gate)) => {
                let _released = gate.recv_timeout(Duration::from_secs(5));
                Err(io::Error::other("released"))
            }
        }
    }

    fn calls(&self) -> Vec<std::time::Instant> {
        self.calls.lock().unwrap().clone()
    }

    /// Waits, at most `within` of real time, until the watch ran `n`
    /// times. True once it has; false at the deadline.
    fn await_calls(&self, n: usize, within: Duration) -> bool {
        let (calls, _) = self
            .called
            .wait_timeout_while(self.calls.lock().unwrap(), within, |calls| calls.len() < n)
            .unwrap();
        calls.len() >= n
    }
}

fn usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 5,
        },
        cost: Some(0.25),
        subscription_cost: 0.0,
    }
}

fn exited(text: &str) -> FiberExited {
    FiberExited {
        exit_code: 0,
        usage: usage(),
        final_message: Some(FinalMessage {
            final_action_id: ActionId("a_1".into()),
            text: text.to_owned(),
        }),
        error: None,
        suspended_on: None,
        questions: None,
    }
}

fn envelope(seq: u64, kind: &str, payload: serde_json::Value) -> Envelope {
    Envelope {
        kind: kind.into(),
        session_id: SessionId("s_child".into()),
        ts: 0,
        schema_version: 1,
        turn_id: None,
        action_id: None,
        seq: Some(Seq(seq)),
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

fn json_line(seq: u64, text: &str) -> String {
    serde_json::to_string(&exited_line(seq, text)).unwrap()
}

struct Rig {
    _dir: TempDir,
    registry: Arc<Registry>,
    clock: Arc<FakeClock>,
    inbox: mpsc::Receiver<Delivery>,
    script: Arc<Script>,
    job: JobId,
    events: std::path::PathBuf,
}

fn rig(replies: Vec<WatchReply>) -> Rig {
    let dir = TempDir::new("fiber-delegate-run");
    let clock = FakeClock::new();
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let registry = Registry::new(
        artifacts,
        Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        Arc::new(Recorder::default()),
    );
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let sessions = dir.path().join("sessions");
    std::fs::create_dir_all(sessions.join("s_child")).unwrap();
    let script = Script::scripted(&clock, replies);
    Rig {
        _dir: dir,
        registry,
        clock,
        inbox: rx,
        script,
        job: crate::registry::mint_job_id(),
        events: sessions.join("s_child").join("events.jsonl"),
    }
}

impl Rig {
    /// Mirrors the tool's order: build the runner, spawn, record, drive.
    /// Returns the runner's stop, so tests stop through the registry as
    /// `jobs stop` does.
    fn start(&self, shell: &str, bound: Duration, cap: u64) -> mpsc::Receiver<()> {
        let shell = shell.to_owned();
        let script = Arc::clone(&self.script);
        let watch = Arc::new(move |id: &SessionId, on_line: &mut dyn FnMut(&Envelope)| {
            script.watch(id, on_line)
        });
        let events = self.events.clone();
        let launch: Launch = Arc::new(move |_: &Launched| {
            let mut command = Command::new("sh");
            command.args(["-c", shell.as_str()]);
            command
        });
        let (runner, stop) = Runner::new(
            self.job.clone(),
            SessionId("s_child".into()),
            events.clone(),
            Arc::clone(&self.clock) as Arc<dyn contract::clock::Clock>,
            bound,
            cap,
            watch,
        );
        let launched = Launched {
            session_id: SessionId("s_child".into()),
            job_id: self.job.clone(),
            parent: SessionId("s_parent".into()),
            model: "fake/m".into(),
            prompt: "hi".into(),
            workspace: self.events.parent().unwrap().to_path_buf(),
        };
        let child = runner.spawn(&launch, &launched).unwrap();
        let (_started, finish) = self.registry.open_started(
            self.job.clone(),
            "delegate_spawn".into(),
            "scan".into(),
            events.to_string_lossy().into_owned(),
            stop,
        );
        let (done, waited) = mpsc::channel();
        thread::spawn(move || {
            runner.drive(child, finish);
            let _sent = done.send(());
        });
        waited
    }
}

/// The runner's park when it lies ahead of `now()`, otherwise `None`.
/// Never blocks and never moves the clock: the caller advances only from
/// the returned `until` (docs/testing.md, "Waits and timeouts": a test
/// advances a fake clock only after a signal that the code under test is
/// waiting on that clock). Panics naming `parked()` when more than one
/// thread is parked or a park has no deadline: the runner is the only
/// thread that parks on the rig's clock in these tests.
fn park_ahead(clock: &FakeClock) -> Option<std::time::Instant> {
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

/// The first notice the runner sends. A scoped driver thread advances the
/// fake clock only from the runner's own park (docs/testing.md, "Waits
/// and timeouts"): a park at most one poll ahead moves time to its
/// `until`; a park further ahead is the drain wait and is never advanced,
/// because the drain's end wakes the runner itself; with no park ahead
/// the driver yields. The test thread's single `recv_timeout(DEADLINE)`
/// is the wall-clock deadline for the whole wait, driver included
/// (docs/testing.md, "Waits and timeouts": every wait has a deadline on
/// the wall clock). The stop flag is set on every outcome of that
/// receive, before the result is inspected, so the driver always ends
/// within `DEADLINE` and the scope never outlives the wait.
fn reported(rig: &Rig) -> contract::inbox::JobNotice {
    reported_before(rig, None)
}

/// `reported` with a horizon: fake time never reaches `horizon`. A park
/// at or past it wakes the runner with a zero advance, which moves no
/// time but wakes every subscriber, so a leader whose stdout EOF beat its
/// zombie state is still reaped on a later pass (docs/testing.md,
/// "Waits and timeouts": deadlines are hang guards, never timing
/// assertions; no timing is asserted here).
fn reported_before(rig: &Rig, horizon: Option<std::time::Instant>) -> contract::inbox::JobNotice {
    let stop = Arc::new(AtomicBool::new(false));
    let driver_stop = Arc::clone(&stop);
    let clock = Arc::clone(&rig.clock);
    thread::scope(|scope| {
        scope.spawn(move || {
            loop {
                if driver_stop.load(Ordering::Relaxed) {
                    return;
                }
                match park_ahead(&clock) {
                    Some(until) => {
                        if horizon.is_some_and(|horizon| until >= horizon) {
                            clock.advance(Duration::ZERO);
                        } else {
                            let now = clock.now();
                            match until.checked_duration_since(now) {
                                Some(gap) if gap <= super::POLL => {
                                    clock.advance(gap);
                                }
                                _ => thread::yield_now(),
                            }
                        }
                    }
                    None => thread::yield_now(),
                }
            }
        });
        let result = rig.inbox.recv_timeout(DEADLINE);
        stop.store(true, Ordering::Relaxed);
        match result {
            Ok(Delivery::Job(notice)) => notice,
            Ok(other) => panic!("expected the runner's report, got {other:?}"),
            Err(_) => panic!(
                "the runner did not report; now={:?} parked={:?}",
                rig.clock.now(),
                rig.clock.parked()
            ),
        }
    })
}

/// Wakes the runner by one poll interval at a time, each only once it is
/// parked on the clock with every deadline ahead of `now()`: it has finished
/// a pass and waits for the next wake. Stops when it parks `bound` ahead of
/// `now()`, which only the wait for the drain does: the clock has not moved
/// since the reap, and an ordinary poll parks at most one interval ahead.
/// One wall-clock deadline covers the whole loop, so a runner that never
/// reaches the drain fails the test instead of hanging it.
fn wake_until_draining(clock: &Arc<FakeClock>, bound: Duration) {
    within("the runner waits for the drain", DEADLINE, {
        let clock = Arc::clone(clock);
        move || {
            loop {
                let parked = clock.parked();
                let now = clock.now();
                if parked.iter().flatten().any(|until| *until >= now + bound) {
                    return;
                }
                if !parked.is_empty() && parked.iter().flatten().all(|until| *until > now) {
                    clock.advance(Duration::from_secs(1));
                } else {
                    thread::yield_now();
                }
            }
        }
    });
}

/// A FIFO `shell` blocks reading until the test writes. Short-lived and
/// stdio-null, so it holds no harness pipe even briefly.
fn fifo(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    Command::new("mkfifo")
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("mkfifo ran");
    path
}

/// Releases a child blocked reading `fifo`, without ever blocking: a
/// non-blocking open pairs with a waiting reader and fails fast with no
/// reader at all, so a write to a dead child's fifo fails the test
/// instead of hanging it. A starting child may not have opened yet; the
/// retries absorb that within a wall-clock bound.
fn release_fifo(fifo: &std::path::Path, bytes: &[u8]) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let fifo = fifo.to_path_buf();
    let bytes = bytes.to_vec();
    within("a reader opens the fifo", DEADLINE, move || {
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(rustix::fs::OFlags::NONBLOCK.bits().cast_signed())
                .open(&fifo)
            {
                Ok(mut fifo) => {
                    fifo.write_all(&bytes)
                        .expect("a few bytes always fit the pipe");
                    return;
                }
                Err(source)
                    if source.raw_os_error() == Some(rustix::io::Errno::NXIO.raw_os_error()) =>
                {
                    thread::yield_now();
                }
                Err(source) => panic!("opening {fifo:?} for writing failed: {source}"),
            }
        }
    });
}

/// A pid `shell` wrote to `pid`: the leader's, or a member's.
fn pid_in_file(pid: &std::path::Path) -> u32 {
    let pid = pid.to_path_buf();
    within("the leader writes its pid", DEADLINE, move || {
        loop {
            let Ok(text) = std::fs::read_to_string(&pid) else {
                thread::yield_now();
                continue;
            };
            let Ok(pid) = text.trim().parse() else {
                thread::yield_now();
                continue;
            };
            return pid;
        }
    })
}

/// Waits until the fake watch ran `n` times, under one wall-clock
/// deadline that only guards against a hang. It never moves the clock:
/// a caller that needs time to pass advances it itself.
fn wait_calls(rig: &Rig, n: usize) {
    assert!(
        rig.script.await_calls(n, DEADLINE),
        "the watch was not called {n} times"
    );
}

/// Wakes the runner through one refused-watch backoff: the runner parks
/// at `at`, the test advances `gap`, and the watch runs again
/// (docs/testing.md, "Waits and timeouts": a test advances a fake clock
/// only after a signal that the code under test is waiting on that
/// clock). The caller counts `at` cumulatively from the first call, with
/// backoff 50, 100, 200, 400, 800 ms, then 1 s.
fn wake_retry(rig: &Rig, at: std::time::Instant, gap: Duration, calls: usize) {
    assert!(
        rig.clock.await_parked(at, DEADLINE),
        "the runner parks until {at:?}"
    );
    rig.clock.advance(gap);
    wait_calls(rig, calls);
}

/// Waits until `shell` writes its readiness file: its traps are armed, so
/// a stop from here cannot land before them.
fn wait_ready(path: &std::path::Path) {
    let path = path.to_path_buf();
    within("the child arms its traps", DEADLINE, move || {
        loop {
            if std::fs::read_to_string(&path).is_ok_and(|text| !text.trim().is_empty()) {
                return;
            }
            thread::yield_now();
        }
    });
}

/// Waits, driving the fake clock, until `pgid` leaves the jobs list:
/// retirement runs on wakes. One wall-clock deadline covers the whole
/// wait (docs/testing.md, "Waits and timeouts"): the wait fails naming
/// the pgid instead of hanging. Each advance goes only to the retire
/// park's own `until` (docs/testing.md, "Waits and timeouts": a test
/// advances a fake clock only after a signal that the code under test is
/// waiting on that clock); with no park ahead the wait yields.
fn wait_retired(clock: &Arc<FakeClock>, pgid: u32) {
    within(&format!("pgid {pgid} retires"), DEADLINE, {
        let clock = Arc::clone(clock);
        move || loop {
            if !listed(pgid) {
                return;
            }
            match park_ahead(&clock) {
                Some(until) => match until.checked_duration_since(clock.now()) {
                    Some(gap) if gap <= super::POLL => clock.advance(gap),
                    _ => thread::yield_now(),
                },
                None => thread::yield_now(),
            }
        }
    });
}

/// How many SIGKILLs went to `pgid` so far.
fn kills(pgid: u32) -> usize {
    crate::delegate::group::sent_signals()
        .iter()
        .filter(|(signalled, signal)| {
            *signalled == pgid && *signal == rustix::process::Signal::KILL
        })
        .count()
}

#[test]
fn the_watch_flow_completes_with_the_delegate_text() {
    let _serial = serial_shared();
    // The child prints the same line the watch delivers: whether the reap
    // or the watch wins the race, the fold sees one `fiber_exited`.
    let line = json_line(2, "Done.");
    let rig = rig(vec![WatchReply::Exited(vec![
        envelope(1, "turn_completed", serde_json::json!({})),
        exited_line(2, "Done."),
    ])]);
    // Backstop armed before the shell starts: the child exits on
    // its own in milliseconds; this only fires if a mutant strands it.
    let watchdog = Watchdog::matching("fiber-delegate-watch-flow");
    rig.start(
        &format!(": fiber-delegate-watch-flow; printf '%s\\n' '{line}'; exit 0"),
        BOUND,
        1024,
    );
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("Done.".into())
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_pre_session_error_fails_with_that_error() {
    let _serial = serial_shared();
    let errored = FiberExited {
        exit_code: 1,
        usage: usage(),
        final_message: Some(FinalMessage {
            final_action_id: ActionId("a_1".into()),
            text: "almost".to_owned(),
        }),
        error: Some(Failure {
            code: ErrorCode::InvalidArguments,
            message: "Bad model.".to_owned(),
            retry_after_ms: None,
            provider: None,
        }),
        suspended_on: None,
        questions: None,
    };
    let line = serde_json::to_string(&envelope(
        1,
        "fiber_exited",
        serde_json::to_value(&errored).unwrap(),
    ))
    .unwrap();
    // Every watch is refused: the child failed before it could bind.
    let rig = rig(vec![]);
    // Backstop armed before the shell starts: the child exits on
    // its own in milliseconds; this only fires if a mutant strands it.
    let watchdog = Watchdog::matching("fiber-delegate-presession");
    rig.start(
        &format!(": fiber-delegate-presession; printf '%s\\n' '{line}'; exit 1"),
        BOUND,
        1024,
    );
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::InvalidArguments)
    );
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("almost".into())
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_child_that_exits_before_any_connect_uses_stdout() {
    let _serial = serial_shared();
    let line = json_line(1, "Quick.");
    let rig = rig(vec![]);
    // Backstop armed before the shell starts: the child exits on
    // its own in milliseconds; this only fires if a mutant strands it.
    let watchdog = Watchdog::matching("fiber-delegate-early-exit");
    rig.start(
        &format!(": fiber-delegate-early-exit; printf '%s\\n' '{line}'; exit 0"),
        BOUND,
        1024,
    );
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("Quick.".into())
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_delayed_drain_still_feeds_the_fold() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-delayed");
    let release = fifo(dir.path(), "release");
    let ready_fifo = fifo(dir.path(), "ready");
    // The member leaves the leader's group before it blocks: the reap's
    // SIGKILL cannot reach it, and the pipe stays open until it prints.
    // The leader leaves only after the member is set up, so the pipe is
    // never unheld. Its command line carries the fifo path, which the
    // watchdog below matches as a backstop.
    let line = json_line(7, "Late.");
    let shell = format!(
        "perl -MPOSIX -e 'POSIX::setsid(); open(my $W, \">\", $ARGV[2]) or die $!; print $W \"ok\\n\"; close $W; open(my $F, \"<\", $ARGV[0]) or die $!; my $x = <$F>; print STDOUT \"$ARGV[1]\\n\";' '{}' '{line}' '{}' & read _ < '{}'; exit 0",
        release.display(),
        ready_fifo.display(),
        ready_fifo.display()
    );
    let watchdog = Watchdog::matching(&release.to_string_lossy());
    // The watch never connects: only the drain can carry the line.
    let rig = rig(vec![]);
    rig.start(&shell, Duration::from_secs(30), 1024);
    // The runner is waiting for the drain once it is parked at the stop
    // bound. Each wake is given only after the runner has parked on the
    // clock, so the reap lands on a wake it is waiting for; the clock stays
    // still once the bound park is seen, so only the drain's end can wake it.
    wake_until_draining(&rig.clock, Duration::from_secs(30));
    assert!(
        rig.inbox.try_recv().is_err(),
        "the fold waits for the drain"
    );
    release_fifo(&release, b"go\n");
    let Ok(Delivery::Job(notice)) = rig.inbox.recv_timeout(DEADLINE) else {
        panic!("the drain's end did not wake the runner");
    };
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("Late.".into())
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_member_holding_stdout_past_the_reap_ends_indeterminate() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-held-stdout");
    let member_pid = dir.path().join("member");
    let ready = fifo(dir.path(), "ready");
    // Escaped past the leader's group, the sleeper holds the pipe past
    // the reap: the fold proceeds after the bound with no `fiber_exited`,
    // and does not hang. The leader leaves only after the member writes
    // past its own `setsid`, so the reap's SIGKILL cannot reach it: an
    // early leader exit would reap and signal the group before the
    // member escapes, closing the pipe and ending the drain early.
    let shell = format!(
        "perl -MPOSIX -e 'POSIX::setsid(); open(my $W, \">\", $ARGV[0]) or die $!; print $W \"ok\\n\"; close $W; sleep 60; # fiber-delegate-held-stdout' '{}' & echo $! > '{}'; read _ < '{}'; exit 0",
        ready.display(),
        member_pid.display(),
        ready.display()
    );
    let rig = rig(vec![]);
    // Backstop armed before the shell starts: on any early failure the
    // Drop kills the escaped sleeper, which would otherwise hold stdout
    // for 60 s.
    let watchdog = Watchdog::matching("fiber-delegate-held-stdout");
    rig.start(&shell, BOUND, 1024);
    let member = pid_in_file(&member_pid);
    // The runner waits for the drain past the poll horizon: the only
    // test that needs fake time past the drain wait. One advance to the
    // drain park's own `until`, then one bounded receive for the notice
    // (docs/testing.md, "Waits and timeouts": one deadline for the
    // whole wait).
    wake_until_draining(&rig.clock, BOUND);
    let drain_until = {
        let parked = rig.clock.parked();
        let now = rig.clock.now();
        let ahead: Vec<std::time::Instant> = parked
            .iter()
            .flatten()
            .filter(|until| **until > now + super::POLL)
            .copied()
            .collect();
        assert_eq!(
            ahead.len(),
            1,
            "the runner waits for the drain past the poll; parked={parked:?}"
        );
        ahead[0]
    };
    rig.clock.advance(drain_until - rig.clock.now());
    let Ok(Delivery::Job(notice)) = rig.inbox.recv_timeout(DEADLINE) else {
        panic!(
            "the drain's end did not report; now={:?} parked={:?}",
            rig.clock.now(),
            rig.clock.parked()
        );
    };
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::Indeterminate)
    );
    kill_pid(member, "KILL").unwrap();
    assert!(
        pids_exit(&[member], DEADLINE),
        "the escaped member is reaped"
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn exit_zero_with_no_line_anywhere_is_indeterminate() {
    let _serial = serial_shared();
    let rig = rig(vec![]);
    // Backstop armed before the shell starts: the child exits on its own
    // in milliseconds; this only fires if a mutant strands it.
    let watchdog = Watchdog::matching("fiber-delegate-exit-zero");
    rig.start(": fiber-delegate-exit-zero; exit 0", BOUND, 1024);
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::Indeterminate)
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_stalled_startup_backs_off_to_one_second_then_flows() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-backoff");
    let fifo = fifo(dir.path(), "release");
    let pidfile = dir.path().join("pid");
    let line = json_line(10, "Bound.");
    let shell = format!(
        "echo $$ > '{}'; read _ < '{}'; printf '%s\\n' '{line}'; exit 0",
        pidfile.display(),
        fifo.display()
    );
    // Each refused call is held until the test releases it, so the runner's
    // park on the poll horizon while it runs is told apart from its park
    // on the retry: once the backoff caps at the poll, both share a
    // deadline.
    let (gates, mut replies): (Vec<_>, Vec<_>) = (0..7)
        .map(|_| {
            let (gate, held) = mpsc::channel();
            (gate, WatchReply::Block(held))
        })
        .unzip();
    replies.push(WatchReply::Exited(vec![
        envelope(9, "turn_completed", serde_json::json!({})),
        exited_line(10, "Bound."),
    ]));
    let rig = rig(replies);
    let done = rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    wait_calls(&rig, 1);
    // 50, 100, 200, 400, 800 ms, then capped at 1 s, never more often.
    let mut at = rig.script.calls()[0];
    for (held, (gate, gap)) in gates
        .iter()
        .zip([50, 100, 200, 400, 800, 1000, 1000])
        .enumerate()
    {
        // The call is held: the runner parks on the poll horizon. Its
        // next park, once it has taken the refusal, is the retry.
        let running = rig
            .clock
            .mark_parked(at + super::POLL, DEADLINE)
            .expect("the runner parks while the watch runs");
        gate.send(()).expect("the held watch takes its release");
        at += Duration::from_millis(gap);
        assert!(
            rig.clock.await_parked_since(&running, Some(at), DEADLINE),
            "the runner parks until {at:?}"
        );
        rig.clock.advance(Duration::from_millis(gap));
        wait_calls(&rig, held + 2);
    }
    // The eighth call succeeds; the child binds and its lines flow.
    release_fifo(&fifo, b"go\n");
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(
        rig.script.calls().len(),
        8,
        "seven retries and the first call, then nothing more"
    );
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("Bound.".into())
    );
    // The report is sent from `retire`: waiting on the runner's return
    // proves the watch stays at rest (docs/testing.md, "Waits and
    // timeouts": every wait has a deadline on the wall clock).
    done.recv_timeout(DEADLINE)
        .expect("the runner returns after `fiber_exited`");
    assert_eq!(
        rig.script.calls().len(),
        8,
        "after `fiber_exited` the watch rests"
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_closed_watch_is_retried_and_each_seq_counts_once() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-closed");
    let fifo = fifo(dir.path(), "release");
    let shell = format!("read _ < '{}'; exit 0", fifo.display());
    let rig = rig(vec![
        WatchReply::Closed(vec![envelope(1, "turn_completed", serde_json::json!({}))]),
        WatchReply::Closed(vec![
            envelope(1, "turn_completed", serde_json::json!({})),
            envelope(2, "turn_completed", serde_json::json!({})),
        ]),
        WatchReply::Exited(vec![exited_line(3, "Replayed.")]),
    ]);
    // Backstop: kills the group if the test fails before the child exits.
    let watchdog = Watchdog::matching(&fifo.to_string_lossy());
    rig.start(&shell, BOUND, 1024);
    // The child waits for the third delivery before it may exit: the
    // replay above cannot win the race with the reap. Each close backs
    // the retry off 50 ms, then 100 ms, from the first call.
    wait_calls(&rig, 1);
    let mut at = rig.script.calls()[0];
    for (gap, calls) in [(50, 2), (100, 3)] {
        at += Duration::from_millis(gap);
        assert!(
            rig.clock.await_parked(at, DEADLINE),
            "the runner parks until {at:?}"
        );
        rig.clock.advance(Duration::from_millis(gap));
        wait_calls(&rig, calls);
    }
    release_fifo(&fifo, b"go\n");
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(rig.script.calls().len(), 3);
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("Replayed.".into())
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn the_watch_is_not_called_again_after_fiber_exited() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-rests");
    let fifo = fifo(dir.path(), "release");
    let shell = format!("read _ < '{}'; exit 0", fifo.display());
    let rig = rig(vec![WatchReply::Exited(vec![exited_line(1, "Once.")])]);
    // Backstop: kills the group if the test fails before the child exits.
    let watchdog = Watchdog::matching(&fifo.to_string_lossy());
    let done = rig.start(&shell, BOUND, 1024);
    wait_calls(&rig, 1);
    release_fifo(&fifo, b"go\n");
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    // The report is sent from `retire`: waiting on the runner's return
    // proves the watch stays at rest (docs/testing.md, "Waits and
    // timeouts": every wait has a deadline on the wall clock).
    done.recv_timeout(DEADLINE)
        .expect("the runner returns after `fiber_exited`");
    assert_eq!(rig.script.calls().len(), 1);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_stop_on_a_term_trap_cancels_without_sigkill() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-trap");
    let pidfile = dir.path().join("pid");
    let ready = dir.path().join("ready");
    let fifo = fifo(dir.path(), "block");
    // A builtin `read` blocks with no child to race: TERM interrupts it
    // and the trap exits the shell itself.
    let shell = format!(
        "echo $$ > '{}'; trap 'exit 143' TERM; echo ready > '{}'; read _ < '{}'",
        pidfile.display(),
        ready.display(),
        fifo.display()
    );
    let rig = rig(vec![]);
    let done = rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    wait_ready(&ready);
    // Fake time never reaches the stop bound, so the trap's own exit
    // cannot race the timer's SIGKILL (docs/testing.md, "Waits and
    // timeouts": deadlines are hang guards, never timing assertions).
    let kill_at = rig.clock.now() + BOUND;
    assert_eq!(rig.registry.stop_delegates(), 1);
    let notice = reported_before(&rig, Some(kill_at));
    assert_eq!(notice.completed.status, Outcome::Cancelled);
    assert_eq!(
        notice
            .completed
            .process
            .as_ref()
            .and_then(|process| process.exit_code),
        Some(143)
    );
    // The report is sent from `retire`: waiting on the runner's return
    // proves no timer can still fire (docs/testing.md, "Waits and
    // timeouts": every wait has a deadline on the wall clock).
    done.recv_timeout(DEADLINE)
        .expect("the runner returns after the trap's exit");
    assert!(
        !crate::delegate::group::sent_signals()
            .iter()
            .any(|(signalled, signal)| *signalled == pgid
                && *signal == rustix::process::Signal::KILL),
        "no SIGKILL went to the reaped group"
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_stop_on_a_term_ignorer_kills_past_the_bound() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-ignore");
    let pidfile = dir.path().join("pid");
    let ready = dir.path().join("ready");
    let fifo = fifo(dir.path(), "block");
    let shell = format!(
        "echo $$ > '{}'; trap '' TERM; echo ready > '{}'; read _ < '{}'",
        pidfile.display(),
        ready.display(),
        fifo.display()
    );
    let rig = rig(vec![]);
    rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    wait_ready(&ready);
    assert_eq!(rig.registry.stop_delegates(), 1);
    // No stepped advance: the driver inside `reported` walks the runner
    // through its retry parks to the park at the stop bound, whose
    // advance sends SIGKILL (docs/testing.md, "Waits and timeouts": a
    // test advances a fake clock only after a signal that the code under
    // test is waiting on that clock).
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Cancelled);
    assert_eq!(
        notice
            .completed
            .process
            .as_ref()
            .and_then(|process| process.signal.clone()),
        Some("SIGKILL".to_owned())
    );
    assert!(
        crate::delegate::group::sent_signals()
            .iter()
            .any(|(signalled, signal)| *signalled == pgid
                && *signal == rustix::process::Signal::KILL),
        "SIGKILL went out once the bound passed"
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn the_cap_at_an_exact_boundary_trips_only_past_it() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-cap");
    let fifo = fifo(dir.path(), "release");
    let shell = format!("read _ < '{}'; exit 0", fifo.display());
    let rig = rig(vec![
        WatchReply::Closed(vec![envelope(1, "turn_completed", serde_json::json!({}))]),
        WatchReply::Closed(vec![
            envelope(1, "turn_completed", serde_json::json!({})),
            envelope(2, "turn_completed", serde_json::json!({})),
        ]),
    ]);
    // Ten bytes: exactly at the cap. Neither the new line, its replay,
    // nor a backoff wake may stop the delegate for that. Each receive
    // below must time out: a report here means the cap tripped at the
    // boundary, and swallowing it would hang the fifo write below on a
    // dead child (docs/testing.md, "Waits and timeouts": a receive
    // with a bound, not a sleep).
    std::fs::write(&rig.events, "0123456789").unwrap();
    // Backstop: kills the group if the test fails before the child exits.
    let watchdog = Watchdog::matching(&fifo.to_string_lossy());
    rig.start(&shell, BOUND, 10);
    // Calls 1 and 2 are `Closed` with the new line and its replay;
    // calls 3 and 4 are refused backoff wakes.
    wait_calls(&rig, 1);
    let mut at = rig.script.calls()[0];
    for (gap, calls) in [(50, 2), (100, 3), (200, 4)] {
        at += Duration::from_millis(gap);
        wake_retry(&rig, at, Duration::from_millis(gap), calls);
        if rig.inbox.recv_timeout(Duration::from_millis(20)).is_ok() {
            panic!("exactly at the cap is not past it");
        }
    }
    // The fourth call is refused, so the backoff is 400 ms.
    at += Duration::from_millis(400);
    assert!(
        rig.clock.await_parked(at, DEADLINE),
        "the runner parks until {at:?}"
    );
    if rig.inbox.recv_timeout(Duration::from_millis(20)).is_ok() {
        panic!("exactly at the cap is not past it");
    }
    std::fs::write(&rig.events, "01234567890").unwrap();
    release_fifo(&fifo, b"go\n");
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::OutputCap)
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_disconnected_cap_trips_on_a_backoff_wake() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-blind");
    let fifo = fifo(dir.path(), "release");
    let shell = format!(
        "for i in 1 2 3 4; do echo filler-line >> '{}'; done; read _ < '{}'",
        "EVENTS",
        fifo.display()
    );
    let rig = rig(vec![]);
    let shell = shell.replace("EVENTS", &rig.events.to_string_lossy());
    // Backstop: kills the group if the test fails before the child exits.
    let watchdog = Watchdog::matching(&fifo.to_string_lossy());
    rig.start(&shell, BOUND, 32);
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::OutputCap)
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_stop_before_the_cap_keeps_cancelled() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-first-wins");
    let fifo = fifo(dir.path(), "release");
    let ready = dir.path().join("ready");
    let shell = format!(
        "printf '0123456789' >> '{}'; trap '' TERM; echo ready > '{}'; read _ < '{}'",
        "EVENTS",
        ready.display(),
        fifo.display()
    );
    let rig = rig(vec![]);
    let shell = shell.replace("EVENTS", &rig.events.to_string_lossy());
    // Backstop: kills the group if the test fails before the child exits.
    let watchdog = Watchdog::matching(&fifo.to_string_lossy());
    rig.start(&shell, BOUND, 10);
    wait_ready(&ready);
    assert_eq!(rig.registry.stop_delegates(), 1);
    // The watch is refused every time, so from call 1 the runner parks
    // at 50, 150 and 350 ms on (backoff 50, 100, 200 ms): the runner
    // passes after the stop with the log exactly at the cap.
    wait_calls(&rig, 1);
    let mut at = rig.script.calls()[0];
    for (gap, calls) in [(50, 2), (100, 3), (200, 4)] {
        at += Duration::from_millis(gap);
        wake_retry(&rig, at, Duration::from_millis(gap), calls);
    }
    // Past the cap, but the stop was first: the end stays `cancelled`.
    std::fs::write(&rig.events, "01234567890").unwrap();
    release_fifo(&fifo, b"go\n");
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Cancelled);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_child_that_exits_in_the_stop_timer_sends_no_kill() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-quick-stop");
    let pidfile = dir.path().join("pid");
    let ready = dir.path().join("ready");
    let fifo = fifo(dir.path(), "block");
    let shell = format!(
        "echo $$ > '{}'; trap 'exit 0' TERM; echo ready > '{}'; read _ < '{}'",
        pidfile.display(),
        ready.display(),
        fifo.display()
    );
    let rig = rig(vec![]);
    let done = rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    wait_ready(&ready);
    // Fake time never reaches the stop bound, so the quick exit cannot
    // race the timer's SIGKILL (docs/testing.md, "Waits and timeouts":
    // deadlines are hang guards, never timing assertions).
    let kill_at = rig.clock.now() + BOUND;
    assert_eq!(rig.registry.stop_delegates(), 1);
    let notice = reported_before(&rig, Some(kill_at));
    assert_eq!(notice.completed.status, Outcome::Cancelled);
    // The report is sent from `retire`: waiting on the runner's return
    // proves no timer can still fire (docs/testing.md, "Waits and
    // timeouts": every wait has a deadline on the wall clock).
    done.recv_timeout(DEADLINE)
        .expect("the runner returns after the quick exit");
    assert!(
        !crate::delegate::group::sent_signals()
            .iter()
            .any(|(signalled, signal)| *signalled == pgid
                && *signal == rustix::process::Signal::KILL),
        "the timer found the group retired"
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_member_outliving_a_stop_is_killed_and_the_job_still_cancels() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-stop-member");
    let leader = dir.path().join("leader");
    let member = dir.path().join("member");
    let shell = format!(
        "echo $$ > '{}'; trap '' TERM; sleep 60 & echo $! > '{}'; trap 'exit 0' TERM; read _ < /dev/stdin",
        leader.display(),
        member.display()
    );
    let rig = rig(vec![]);
    rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&leader);
    let watchdog = Watchdog::group(pgid);
    let survivor: u32 = within("the member writes its pid", DEADLINE, {
        let member = member.clone();
        move || loop {
            let Ok(text) = std::fs::read_to_string(&member) else {
                thread::yield_now();
                continue;
            };
            let Ok(pid) = text.trim().parse() else {
                thread::yield_now();
                continue;
            };
            return pid;
        }
    });
    assert_eq!(rig.registry.stop_delegates(), 1);
    let notice = reported(&rig);
    // The job ends as soon as the leader is reaped, not when the member
    // is gone.
    assert_eq!(notice.completed.status, Outcome::Cancelled);
    assert!(
        pids_exit(&[survivor], DEADLINE),
        "the surviving member got SIGKILL"
    );
    assert!(
        group_empties(pgid, DEADLINE),
        "the group retires once it is empty"
    );
    // Retired, not just empty, once the runner has run: without the
    // retire loop the pgid would stay listed after its members are gone.
    wait_retired(&rig.clock, pgid);
    assert!(!listed(pgid));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_member_listed_past_the_bound_gets_sigkill_from_retire() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-retire-timer");
    let clock = FakeClock::new();
    // A live group with no delegate attached: only `retire` supervises
    // it, so every SIGKILL below is the retire timer's.
    let mut member = Command::new("sleep");
    member
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut member = crate::delegate::group::spawn(&mut member).unwrap();
    let pgid = member.id();
    let watchdog = Watchdog::group(pgid);
    let watch: Watch = Arc::new(|_: &SessionId, _: &mut dyn FnMut(&Envelope)| {
        Err(std::io::Error::other("refused"))
    });
    let (runner, stop) = Runner::new(
        crate::registry::mint_job_id(),
        mint_session_id(),
        dir.path().to_path_buf(),
        Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        BOUND,
        1024,
        watch,
    );
    // The stop sets the bound; the member ignores nothing and no other
    // thread signals this group.
    stop.0();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        runner.retire(pgid);
        let _sent = done_tx.send(());
    });
    let first_poll = clock.origin() + Duration::from_secs(1);
    assert!(
        clock.await_parked(first_poll, DEADLINE),
        "retire waits on the fake clock"
    );
    let mark = clock.advance_marked(BOUND + Duration::from_secs(1));
    let after_bound = clock.origin() + BOUND + Duration::from_secs(1);
    assert!(
        clock.await_parked_since(&mark, Some(after_bound + Duration::from_secs(1)), DEADLINE),
        "retire checks the timer and parks again past the bound"
    );
    assert!(kills(pgid) >= 1, "SIGKILL went out past the bound");

    let (reaped_tx, reaped_rx) = mpsc::channel();
    thread::spawn(move || {
        let _status = member.wait();
        let _sent = reaped_tx.send(());
    });
    reaped_rx
        .recv_timeout(DEADLINE)
        .expect("the killed member was reaped");

    let _mark = clock.advance_marked(Duration::from_secs(1));
    done_rx
        .recv_timeout(DEADLINE)
        .expect("retire returned once the group was empty");
    assert!(!listed(pgid));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_new_seq_counts_and_a_replay_does_not() {
    let mut last = None;
    assert!(note_seq(&mut last, Some(1)));
    assert!(!note_seq(&mut last, Some(1)), "a replayed seq is not new");
    assert!(!note_seq(&mut last, Some(0)), "an older seq is not new");
    assert!(note_seq(&mut last, Some(2)));
    assert!(
        note_seq(&mut last, None),
        "a line without seq always counts"
    );
}

#[test]
fn a_blocked_watch_still_lets_a_stop_through() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-blocked-stop");
    let pidfile = dir.path().join("pid");
    let ready = dir.path().join("ready");
    let block = fifo(dir.path(), "block");
    let shell = format!(
        "echo $$ > '{}'; trap '' TERM; echo ready > '{}'; read _ < '{}'",
        pidfile.display(),
        ready.display(),
        block.display()
    );
    // The watch blocks until the test releases it; the runner must still
    // supervise the child: one watcher runs, and the stop timer fires on
    // the clock without it.
    let (release, gate) = mpsc::channel();
    let rig = rig(vec![WatchReply::Block(gate)]);
    let done = rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    wait_ready(&ready);
    within("the first watch call", DEADLINE, {
        let script = Arc::clone(&rig.script);
        move || {
            while script.calls().is_empty() {
                thread::yield_now();
            }
        }
    });
    assert_eq!(rig.registry.stop_delegates(), 1);
    // The watch is blocked, so `outstanding` keeps the retry out of the
    // deadline: the runner parks at each poll horizon, then at the stop
    // bound (docs/testing.md, "Waits and timeouts": a test advances a
    // fake clock only after a signal that the code under test is waiting
    // on that clock). Nothing moves the clock before the stop, so the
    // bound is five polls on from the origin.
    let t0 = rig.clock.origin();
    for k in 1..=5 {
        let at = t0 + Duration::from_secs(k);
        assert!(
            rig.clock.await_parked(at, DEADLINE),
            "the runner parks until {at:?}"
        );
        rig.clock.advance(super::POLL);
    }
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Cancelled);
    assert_eq!(
        notice
            .completed
            .process
            .as_ref()
            .and_then(|process| process.signal.clone()),
        Some("SIGKILL".to_owned())
    );
    assert!(
        crate::delegate::group::sent_signals()
            .iter()
            .any(|(signalled, signal)| *signalled == pgid
                && *signal == rustix::process::Signal::KILL),
        "SIGKILL went out once the bound passed"
    );
    // The report is sent from `retire`: waiting on the runner's return
    // proves no timer can still fire (docs/testing.md, "Waits and
    // timeouts": every wait has a deadline on the wall clock).
    done.recv_timeout(DEADLINE)
        .expect("the runner returns after the stop");
    assert_eq!(
        rig.script.calls().len(),
        1,
        "one watcher runs while it is blocked, never one per wake"
    );
    drop(release);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_blocked_watch_does_not_block_the_cap() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-blocked-cap");
    let block = fifo(dir.path(), "block");
    let shell = format!(
        "for i in 1 2 3 4; do echo filler-line >> '{}'; done; read _ < '{}'",
        "EVENTS",
        block.display()
    );
    let (release, gate) = mpsc::channel();
    let rig = rig(vec![WatchReply::Block(gate)]);
    let shell = shell.replace("EVENTS", &rig.events.to_string_lossy());
    // Backstop: kills the group if the test fails before the child exits.
    let watchdog = Watchdog::matching(&block.to_string_lossy());
    rig.start(&shell, BOUND, 32);
    // No line is ever delivered: the backoff wakes alone trip the cap.
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::OutputCap)
    );
    drop(release);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn park_due_takes_the_earliest_live_bound() {
    let clock = FakeClock::new();
    let t0 = clock.now();
    let poll = t0 + Duration::from_secs(1);
    // A due retry pulls the deadline in.
    assert_eq!(
        park_due(poll, false, false, t0 + Duration::from_millis(50), None),
        t0 + Duration::from_millis(50)
    );
    // A future retry leaves the horizon alone.
    assert_eq!(
        park_due(poll, false, false, t0 + Duration::from_secs(2), None),
        poll
    );
    // Once exited, a stale retry never pulls the deadline back: that
    // would spin instead of parking.
    assert_eq!(park_due(poll, true, false, t0, None), poll);
    // While a watcher runs, its stale retry never pulls it back either.
    assert_eq!(park_due(poll, false, true, t0, None), poll);
    // The stop timer clamps everything, exited or not.
    assert_eq!(
        park_due(poll, true, false, t0, Some(t0 + Duration::from_millis(500))),
        t0 + Duration::from_millis(500)
    );
    assert_eq!(
        park_due(
            poll,
            false,
            false,
            t0 + Duration::from_millis(50),
            Some(t0 + Duration::from_millis(500))
        ),
        t0 + Duration::from_millis(50)
    );
}

#[test]
fn kill_due_matches_the_exact_boundaries() {
    let clock = FakeClock::new();
    let kill_at = clock.now() + Duration::from_secs(1);
    let tick = Duration::from_nanos(1);
    let before = kill_at.checked_sub(tick).unwrap();
    let after = kill_at.checked_add(tick).unwrap();

    for (case, kill_at, now, expected) in [
        ("no bound", None, kill_at, false),
        ("one tick before", Some(kill_at), before, false),
        ("at the bound", Some(kill_at), kill_at, true),
        ("one tick after", Some(kill_at), after, true),
    ] {
        assert_eq!(kill_due(kill_at, now), expected, "{case}");
    }
}
