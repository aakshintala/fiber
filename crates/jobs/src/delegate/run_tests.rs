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
use std::sync::{Arc, Mutex, mpsc};
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

use super::{Launch, Launched, Runner, Watch, Watched, mint_session_id, note_seq, park_due};
use crate::delegate::group::serial_shared;
use crate::registry::Registry;

/// How long a test waits on the wall clock before it fails.
const DEADLINE: Duration = Duration::from_secs(10);

/// The stop bound the tests run with, on the fake clock.
const BOUND: Duration = Duration::from_secs(5);

/// A fake watch: replays queued replies and records when it ran.
struct Script {
    clock: Arc<FakeClock>,
    calls: Mutex<Vec<std::time::Instant>>,
    replies: Mutex<VecDeque<WatchReply>>,
}

enum WatchReply {
    Refused,
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
            replies: Mutex::new(replies.into()),
        })
    }

    fn watch(
        self: &Arc<Self>,
        _id: &SessionId,
        on_line: &mut dyn FnMut(&Envelope),
    ) -> io::Result<Watched> {
        self.calls.lock().unwrap().push(self.clock.now());
        match self.replies.lock().unwrap().pop_front() {
            None | Some(WatchReply::Refused) => Err(io::Error::other("refused")),
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
                let _released = gate.recv_timeout(Duration::from_secs(30));
                Err(io::Error::other("released"))
            }
        }
    }

    fn calls(&self) -> Vec<std::time::Instant> {
        self.calls.lock().unwrap().clone()
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

/// The first notice the runner sends, driving the fake clock in small
/// steps. Panics after a bounded number of steps, so a runner that never
/// reports fails the test instead of hanging it.
fn reported(rig: &Rig) -> contract::inbox::JobNotice {
    for _ in 0..400 {
        if let Ok(Delivery::Job(notice)) = rig.inbox.try_recv() {
            return notice;
        }
        rig.clock.advance(Duration::from_millis(50));
        if let Ok(Delivery::Job(notice)) = rig.inbox.recv_timeout(Duration::from_millis(20)) {
            return notice;
        }
    }
    panic!("the runner did not report");
}

/// A FIFO `shell` blocks reading until the test writes.
fn fifo(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    Command::new("mkfifo")
        .arg(&path)
        .status()
        .expect("mkfifo ran");
    path
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

/// Waits until the fake watch ran `n` times, driving the clock. Bounds the
/// wait, so a runner that stops calling fails instead of hanging. Each
/// step gives the runner wall time: advances alone cost it none. A report
/// arriving here fails fast: swallowing it would hang the fifo write that
/// follows on an exited child.
fn wait_calls(rig: &Rig, n: usize) {
    for _ in 0..400 {
        if rig.script.calls().len() >= n {
            return;
        }
        rig.clock.advance(Duration::from_millis(50));
        if rig.inbox.recv_timeout(Duration::from_millis(5)).is_ok() {
            panic!("the runner reported before the child could exit");
        }
        if rig.script.calls().len() >= n {
            return;
        }
    }
    panic!("the watch was not called {n} times");
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

/// Waits, with a wall-clock bound, until the fake clock shows a parked
/// waiter: the thread under test reached its clock wait. A parked entry
/// proves it passed every check before the wait, so work observed after
/// this cannot have skipped supervision.
fn wait_parked(clock: &Arc<FakeClock>) {
    within("a waiter parks on the clock", DEADLINE, {
        let clock = Arc::clone(clock);
        move || loop {
            if !clock.parked().is_empty() {
                return;
            }
            thread::yield_now();
        }
    });
}

/// Waits, driving the fake clock, until `pgid` leaves the jobs list:
/// retirement runs on wakes. Panics boundedly instead of asserting on a
/// list the runner has not reached yet.
fn wait_retired(clock: &FakeClock, pgid: u32) {
    for _ in 0..400 {
        if !listed(pgid) {
            return;
        }
        clock.advance(Duration::from_millis(50));
        thread::yield_now();
    }
    panic!("pgid {pgid} was not retired");
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
    rig.start(&format!("printf '%s\\n' '{line}'; exit 0"), BOUND, 1024);
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("Done.".into())
    );
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
    rig.start(&format!("printf '%s\\n' '{line}'; exit 1"), BOUND, 1024);
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
}

#[test]
fn a_child_that_exits_before_any_connect_uses_stdout() {
    let _serial = serial_shared();
    let line = json_line(1, "Quick.");
    let rig = rig(vec![]);
    rig.start(&format!("printf '%s\\n' '{line}'; exit 0"), BOUND, 1024);
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("Quick.".into())
    );
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
    // Three wakes give the runner its reap; nothing is reported yet,
    // because the fold waits for the drain.
    for _ in 0..3 {
        rig.clock.advance(Duration::from_millis(50));
        rig.inbox
            .recv_timeout(Duration::from_millis(20))
            .expect_err("the fold waits for the drain");
    }
    std::fs::write(&release, "go\n").unwrap();
    let notice = reported(&rig);
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
    // Escaped past the leader's group, the sleeper holds the pipe past
    // the reap: the fold proceeds after the bound with no `fiber_exited`,
    // and does not hang.
    let shell = format!(
        "perl -MPOSIX -e 'POSIX::setsid(); sleep 60;' & echo $! > '{}'",
        member_pid.display()
    );
    let rig = rig(vec![]);
    rig.start(&shell, BOUND, 1024);
    let member = pid_in_file(&member_pid);
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
    kill_pid(member, "KILL").unwrap();
    assert!(
        pids_exit(&[member], DEADLINE),
        "the escaped member is reaped"
    );
}

#[test]
fn exit_zero_with_no_line_anywhere_is_indeterminate() {
    let _serial = serial_shared();
    let rig = rig(vec![]);
    rig.start("exit 0", BOUND, 1024);
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
    let rig = rig(vec![
        WatchReply::Refused,
        WatchReply::Refused,
        WatchReply::Refused,
        WatchReply::Refused,
        WatchReply::Refused,
        WatchReply::Refused,
        WatchReply::Refused,
        WatchReply::Exited(vec![
            envelope(9, "turn_completed", serde_json::json!({})),
            exited_line(10, "Bound."),
        ]),
    ]);
    rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    let first = within("the first watch call", DEADLINE, {
        let script = Arc::clone(&rig.script);
        move || loop {
            if let Some(first) = script.calls().first() {
                return *first;
            }
            thread::yield_now();
        }
    });
    // 50, 100, 200, 400, 800 ms, then capped at 1 s, never more often.
    let mut at = first;
    for gap in [50, 100, 200, 400, 800, 1000, 1000] {
        at += Duration::from_millis(gap);
        assert!(
            rig.clock.await_parked(at, DEADLINE),
            "the runner parks until {at:?}"
        );
        rig.clock.advance(Duration::from_millis(gap));
    }
    // The eighth call succeeds; the child binds and its lines flow. It
    // lands a wake after the seventh retry is consumed, so wait for it.
    wait_calls(&rig, 8);
    std::fs::write(&fifo, "go\n").unwrap();
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
    rig.clock.advance(Duration::from_secs(2));
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
    rig.start(&shell, BOUND, 1024);
    // The child waits for the third delivery before it may exit: the
    // replay above cannot win the race with the reap.
    wait_calls(&rig, 3);
    std::fs::write(&fifo, "go\n").unwrap();
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(rig.script.calls().len(), 3);
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("Replayed.".into())
    );
}

#[test]
fn the_watch_is_not_called_again_after_fiber_exited() {
    let _serial = serial_shared();
    let dir = TempDir::new("fiber-delegate-rests");
    let fifo = fifo(dir.path(), "release");
    let shell = format!("read _ < '{}'; exit 0", fifo.display());
    let rig = rig(vec![WatchReply::Exited(vec![exited_line(1, "Once.")])]);
    rig.start(&shell, BOUND, 1024);
    wait_calls(&rig, 1);
    std::fs::write(&fifo, "go\n").unwrap();
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    rig.clock.advance(Duration::from_secs(3));
    assert_eq!(rig.script.calls().len(), 1);
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
    rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    wait_ready(&ready);
    assert_eq!(rig.registry.stop_delegates(), 1);
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Cancelled);
    assert_eq!(
        notice
            .completed
            .process
            .as_ref()
            .and_then(|process| process.exit_code),
        Some(143)
    );
    rig.clock.advance(BOUND + Duration::from_secs(1));
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
    for _ in 0..12 {
        rig.clock.advance(Duration::from_millis(500));
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
    // nor a backoff wake may stop the delegate for that. Either receive
    // failing fast: a report here means the cap tripped at the boundary,
    // and swallowing it would hang the fifo write below on a dead child.
    std::fs::write(&rig.events, "0123456789").unwrap();
    rig.start(&shell, BOUND, 10);
    for _ in 0..6 {
        rig.clock.advance(Duration::from_millis(100));
        if rig.inbox.recv_timeout(Duration::from_millis(20)).is_ok() {
            panic!("exactly at the cap is not past it");
        }
        assert!(
            rig.inbox.try_recv().is_err(),
            "exactly at the cap is not past it"
        );
    }
    std::fs::write(&rig.events, "01234567890").unwrap();
    std::fs::write(&fifo, "go\n").unwrap();
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
    rig.start(&shell, BOUND, 10);
    wait_ready(&ready);
    assert_eq!(rig.registry.stop_delegates(), 1);
    for _ in 0..5 {
        rig.clock.advance(Duration::from_millis(100));
    }
    // Past the cap, but the stop was first: the end stays `cancelled`.
    std::fs::write(&rig.events, "01234567890").unwrap();
    std::fs::write(&fifo, "go\n").unwrap();
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Cancelled);
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
    rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    wait_ready(&ready);
    assert_eq!(rig.registry.stop_delegates(), 1);
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Cancelled);
    rig.clock.advance(BOUND + Duration::from_secs(1));
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
    for _ in 0..5 {
        rig.clock.advance(Duration::from_secs(1));
    }
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
    let (reaped_tx, reaped_rx) = mpsc::channel();
    thread::spawn(move || {
        let _status = member.wait();
        let _sent = reaped_tx.send(());
    });
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
    let done = thread::spawn(move || runner.retire(pgid));
    // Synchronise: retire must be parked on the clock, having passed
    // every check before its wait, before time moves at all.
    wait_parked(&clock);
    // Pre-bound wakes stay silent: only the reap-time signal, which never
    // ran here, could have gone out. An inverted timer sprays here and
    // fails this line. The thread proved itself parked above, so this
    // silence was supervised, not starved.
    for _ in 0..20 {
        clock.advance(Duration::from_millis(100));
        thread::yield_now();
    }
    assert_eq!(kills(pgid), 0, "no SIGKILL before the bound");
    // Past the bound the timer fires on every wake until the group is
    // empty; the reaper thread below reaps the member for retirement. A
    // timer that stays silent past the bound fails the wait below.
    for _ in 0..100 {
        clock.advance(Duration::from_millis(100));
        thread::yield_now();
        if kills(pgid) >= 1 {
            break;
        }
    }
    assert!(kills(pgid) >= 1, "SIGKILL went out once the bound passed");
    reaped_rx
        .recv_timeout(DEADLINE)
        .expect("the timer killed the member");
    for _ in 0..200 {
        if done.is_finished() {
            break;
        }
        clock.advance(Duration::from_millis(50));
        thread::yield_now();
    }
    assert!(done.is_finished(), "retire returned once empty");
    done.join().unwrap();
    wait_retired(&clock, pgid);
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
    rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    wait_ready(&ready);
    assert_eq!(rig.registry.stop_delegates(), 1);
    for _ in 0..12 {
        rig.clock.advance(Duration::from_millis(500));
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
    rig.clock.advance(Duration::from_secs(1));
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
