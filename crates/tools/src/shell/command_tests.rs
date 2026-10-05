use std::io::{self, ErrorKind, Read};
use std::process::{Child, Command};
use std::sync::{Arc, TryLockError, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use contract::clock::{Clock, Wake};
use fakes::CancelToken;
use fakes::Recorder;
use fakes::clock::FakeClock;

use super::super::background::{Step, running_step, wait_deadline};
use super::super::output::{
    Inner, OUTPUT_CAP, Shared, bump, complete_prefix, lock, note_eof, read_output, stream_output,
};
use super::{
    MovePolicy, MoveReason, Moved, Phase, Run, StopKind, already_woken, exit_code_of, finish,
    group_alive, park, poll_while_occupied, refused_group, suppress_term,
};

#[test]
fn a_moved_sequence_wakes_without_a_cancel() {
    assert!(already_woken(1, 0, false, false));
}

#[test]
fn a_cancel_wakes_only_while_the_run_is_going() {
    assert!(already_woken(0, 0, true, true));
    assert!(!already_woken(0, 0, false, true));
    assert!(!already_woken(0, 0, true, false));
}

#[test]
fn the_drain_polls_only_while_the_group_may_be_occupied() {
    assert!(poll_while_occupied(false));
    assert!(!poll_while_occupied(true));
}

#[test]
fn a_second_signal_and_an_empty_group_are_not_signalled() {
    assert!(suppress_term(true, false));
    assert!(suppress_term(false, true));
    assert!(!suppress_term(false, false));
}

#[test]
fn bump_advances_the_sequence() {
    let mut inner = Inner::default();
    bump(&mut inner);
    assert_eq!(inner.seq, 1);
}

#[test]
fn an_open_pipe_is_discarded_once_the_run_returns() {
    let shared = Shared::default();
    finish(&shared, None, false, true, false, &Recorder::default(), 0);
    assert!(lock(&shared.inner).discard);
}

#[test]
fn finish_distinguishes_a_held_pipe_from_an_unfinished_stop() {
    let held = finish(
        &Shared::default(),
        None,
        false,
        true,
        false,
        &Recorder::default(),
        0,
    );
    assert!(held.held_open);
    assert!(!held.indeterminate);

    let closed = finish(
        &Shared::default(),
        None,
        false,
        true,
        true,
        &Recorder::default(),
        0,
    );
    assert!(!closed.held_open);
    assert!(!closed.indeterminate);

    let still_occupied = finish(
        &Shared::default(),
        None,
        false,
        false,
        false,
        &Recorder::default(),
        0,
    );
    assert!(!still_occupied.held_open);

    let stopped_open = finish(
        &Shared::default(),
        Some(StopKind::Cancel),
        true,
        true,
        false,
        &Recorder::default(),
        0,
    );
    assert!(stopped_open.indeterminate);
    assert!(!stopped_open.held_open);

    let stopped_occupied = finish(
        &Shared::default(),
        Some(StopKind::Timeout),
        true,
        false,
        true,
        &Recorder::default(),
        0,
    );
    assert!(stopped_occupied.indeterminate);
    assert!(!stopped_occupied.held_open);

    let stopped_clean = finish(
        &Shared::default(),
        Some(StopKind::Timeout),
        true,
        true,
        true,
        &Recorder::default(),
        0,
    );
    assert!(!stopped_clean.indeterminate);
    assert!(!stopped_clean.held_open);
}

#[test]
fn an_interrupted_read_is_retried() {
    let shared = Arc::new(Shared::default());
    let reader = Arc::clone(&shared);
    read_output(
        Scripted {
            steps: vec![
                Err(io::Error::new(ErrorKind::Interrupted, "again")),
                Ok(b"hi".to_vec()),
            ],
        },
        &reader,
    );
    let inner = lock(&shared.inner);
    assert_eq!(inner.output, b"hi");
    assert!(inner.eof);
}

#[test]
fn a_read_error_ends_the_output() {
    let shared = Shared::default();
    read_output(
        Scripted {
            steps: vec![Err(io::Error::other("broken")), Ok(b"later".to_vec())],
        },
        &shared,
    );
    let inner = lock(&shared.inner);
    assert!(
        inner.output.is_empty(),
        "bytes after a read error were kept"
    );
    assert!(inner.eof);
}

struct Scripted {
    steps: Vec<io::Result<Vec<u8>>>,
}

impl Read for Scripted {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.steps.is_empty() {
            return Ok(0);
        }
        match self.steps.remove(0) {
            Ok(bytes) => {
                let n = bytes.len().min(buf.len());
                if let Some(slot) = buf.get_mut(..n) {
                    slot.copy_from_slice(bytes.get(..n).unwrap_or(&[]));
                }
                Ok(n)
            }
            Err(err) => Err(err),
        }
    }
}

/// Passes `None` to the closure, the bound a fake clock gives when `until`
/// was still ahead at registration. With `wake` set, it first asserts that
/// the caller holds the waiter lock across `wait_until`, so no wake can land
/// between this point and the condvar wait, then delivers a wake from
/// another thread. That thread blocks on the lock until the wait releases
/// it, whenever it runs.
struct BoundlessClock {
    origin: Instant,
    wake: Option<Arc<Shared>>,
}

impl Clock for BoundlessClock {
    fn now(&self) -> Instant {
        self.origin
    }

    fn wall(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    fn sleep(&self, _duration: Duration) {}

    fn wait_until(&self, _until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        if let Some(shared) = &self.wake {
            assert!(
                matches!(shared.inner.try_lock(), Err(TryLockError::WouldBlock)),
                "park does not hold the waiter lock across wait_until"
            );
            let shared = Arc::clone(shared);
            thread::spawn(move || shared.wake());
        }
        wait(None);
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

fn assert_park_returns(clock: BoundlessClock, shared: Arc<Shared>, seen: u64) {
    const DEADLINE: Duration = Duration::from_secs(5);
    let cancel = CancelToken::new();
    let (done, finished) = mpsc::channel();
    let origin = clock.origin;
    thread::spawn(move || {
        park(
            &clock,
            &shared,
            &cancel,
            origin.checked_add(Duration::from_secs(2)),
            false,
            false,
            seen,
        );
        done.send(()).unwrap();
    });
    assert!(
        finished.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for park to return"
    );
}

#[test]
fn a_wake_that_landed_before_park_is_not_lost() {
    let shared = Arc::new(Shared::default());
    let origin = FakeClock::new().origin();
    shared.wake();
    assert_park_returns(
        BoundlessClock { origin, wake: None },
        Arc::clone(&shared),
        0,
    );
    assert_ne!(lock(&shared.inner).seq, 0);
}

#[test]
fn a_wake_inside_wait_until_is_not_lost() {
    let shared = Arc::new(Shared::default());
    let origin = FakeClock::new().origin();
    assert_park_returns(
        BoundlessClock {
            origin,
            wake: Some(Arc::clone(&shared)),
        },
        Arc::clone(&shared),
        0,
    );
}

/// A process in the test's own group that a stray group signal would kill.
fn sentinel() -> Child {
    Command::new("sleep").arg("30").spawn().unwrap()
}

/// Whether the sentinel is still running; ends it either way.
fn survived(mut sentinel: Child) -> bool {
    let alive = sentinel.try_wait().unwrap().is_none();
    sentinel.kill().unwrap();
    sentinel.wait().unwrap();
    alive
}

#[test]
fn group_zero_and_one_are_refused() {
    assert!(refused_group(0));
    assert!(refused_group(1));
    assert!(!refused_group(2));
}

/// Only the probe runs here. A test never hands 0 or 1 to `signal_group`:
/// a mutant of its guard would then send that signal to every process the
/// user owns.
#[test]
fn group_zero_and_one_are_never_occupied() {
    for group in [0, 1] {
        let sentinel = sentinel();
        assert!(!group_alive(group), "group {group} looked occupied");
        assert!(
            survived(sentinel),
            "group {group} probe touched the sentinel"
        );
    }
}

#[test]
fn note_eof_is_idempotent() {
    let shared = Shared::default();
    note_eof(&shared);
    let seq = lock(&shared.inner).seq;
    note_eof(&shared);
    assert_eq!(lock(&shared.inner).seq, seq);
    assert!(lock(&shared.inner).eof);
}

#[test]
fn a_complete_prefix_ends_on_a_character_boundary() {
    assert_eq!(complete_prefix(b""), 0);
    assert_eq!(complete_prefix(b"hi"), 2);
    // U+00E9, two bytes: a split lead waits.
    assert_eq!(complete_prefix(b"a\xC3"), 1);
    assert_eq!(complete_prefix(b"\xC3\xA9"), 2);
    // U+20AC, three bytes: one and two leads wait.
    assert_eq!(complete_prefix(b"\xE2"), 0);
    assert_eq!(complete_prefix(b"\xE2\x82"), 0);
    assert_eq!(complete_prefix(b"\xE2\x82\xAC"), 3);
    // U+1F600, four bytes: one, two and three leads wait.
    assert_eq!(complete_prefix(b"a\xF0"), 1);
    assert_eq!(complete_prefix(b"a\xF0\x9F"), 1);
    assert_eq!(complete_prefix(b"a\xF0\x9F\x98"), 1);
    assert_eq!(complete_prefix(b"a\xF0\x9F\x98\x80"), 5);
    // An invalid byte is consumed as U+FFFD at once, not held.
    assert_eq!(complete_prefix(b"\xFF"), 1);
    assert_eq!(complete_prefix(b"\xFFx"), 2);
    // An invalid byte in the middle is consumed; the length counts it.
    assert_eq!(complete_prefix(b"a\xFFb"), 3);
    // An invalid byte followed by an incomplete tail holds only the tail.
    assert_eq!(complete_prefix(b"\xFF\xE2"), 1);
}

#[test]
fn an_incomplete_tail_waits_for_the_next_chunk_and_streams_once_whole() {
    let shared = Shared::default();
    let recorder = Recorder::default();
    lock(&shared.inner).output.extend_from_slice(b"a\xC3");
    let streamed = stream_output(&shared, &recorder, 0);
    assert_eq!(recorder.text(), "a");
    assert_eq!(streamed, 1);
    lock(&shared.inner).output.extend_from_slice(b"\xA9b");
    let streamed = stream_output(&shared, &recorder, streamed);
    assert_eq!(recorder.text(), "a\u{e9}b");
    assert_eq!(streamed, 4);
}

#[test]
fn an_invalid_byte_streams_as_the_replacement_character() {
    let shared = Shared::default();
    let recorder = Recorder::default();
    lock(&shared.inner).output.extend_from_slice(b"\xFFx");
    let streamed = stream_output(&shared, &recorder, 0);
    assert_eq!(recorder.text(), "�x");
    assert_eq!(streamed, 2);
}

#[test]
fn finish_streams_the_tail_from_the_taken_snapshot() {
    let shared = Shared::default();
    let recorder = Recorder::default();
    lock(&shared.inner).output.extend_from_slice(b"a\xE2\x82");
    let streamed = stream_output(&shared, &recorder, 0);
    assert_eq!(streamed, 1);
    let finished = finish(&shared, None, false, true, true, &recorder, streamed);
    assert_eq!(finished.output, b"a\xE2\x82");
    // The tail streams once, from the snapshot the result took: "a" is not
    // repeated, and the concatenated texts equal the lossy whole output.
    assert_eq!(
        recorder.text(),
        String::from_utf8_lossy(b"a\xE2\x82").into_owned()
    );
}

fn at(seconds: u64) -> Instant {
    FakeClock::new()
        .origin()
        .checked_add(Duration::from_secs(seconds))
        .unwrap()
}

#[test]
fn a_timeout_wins_over_every_move() {
    for policy in [
        MovePolicy::Stay,
        MovePolicy::Foreground,
        MovePolicy::Background,
    ] {
        assert_eq!(
            running_step(policy, true, true, true, Some(3), true, false),
            Step::Stop(StopKind::Timeout),
            "{policy:?}"
        );
    }
}

#[test]
fn a_cancel_wins_over_a_move_and_an_empty_group() {
    assert_eq!(
        running_step(
            MovePolicy::Background,
            false,
            true,
            true,
            Some(3),
            true,
            false
        ),
        Step::Stop(StopKind::Cancel)
    );
}

#[test]
fn an_empty_group_finishes_in_the_foreground() {
    assert_eq!(
        running_step(
            MovePolicy::Background,
            false,
            false,
            true,
            Some(3),
            true,
            false
        ),
        Step::Drain
    );
    assert_eq!(
        running_step(
            MovePolicy::Foreground,
            false,
            false,
            true,
            None,
            true,
            false
        ),
        Step::Drain
    );
}

#[test]
fn a_reaped_shell_with_members_moves_before_the_other_triggers() {
    assert_eq!(
        running_step(
            MovePolicy::Background,
            false,
            false,
            false,
            Some(3),
            true,
            false
        ),
        Step::Move(MoveReason::ShellExited { code: 3 })
    );
    assert_eq!(
        running_step(
            MovePolicy::Foreground,
            false,
            false,
            false,
            Some(3),
            false,
            false
        ),
        Step::Move(MoveReason::ShellExited { code: 3 })
    );
}

#[test]
fn run_in_background_moves_on_the_first_pass() {
    assert_eq!(
        running_step(
            MovePolicy::Background,
            false,
            false,
            false,
            None,
            false,
            false
        ),
        Step::Move(MoveReason::StartedInBackground)
    );
    assert_eq!(
        running_step(
            MovePolicy::Background,
            false,
            false,
            false,
            None,
            true,
            false
        ),
        Step::Move(MoveReason::StartedInBackground)
    );
}

#[test]
fn thirty_seconds_moves_a_foreground_command_that_is_still_running() {
    assert_eq!(
        running_step(
            MovePolicy::Foreground,
            false,
            false,
            false,
            None,
            true,
            false
        ),
        Step::Move(MoveReason::AfterThirtySeconds)
    );
    assert_eq!(
        running_step(
            MovePolicy::Foreground,
            false,
            false,
            false,
            None,
            false,
            false
        ),
        Step::Park
    );
}

#[test]
fn the_background_command_moves_a_foreground_command_before_thirty_seconds() {
    assert_eq!(
        running_step(
            MovePolicy::Foreground,
            false,
            false,
            false,
            None,
            false,
            true
        ),
        Step::Move(MoveReason::BackgroundCommand)
    );
    assert_eq!(
        running_step(
            MovePolicy::Foreground,
            false,
            false,
            false,
            None,
            true,
            true
        ),
        Step::Move(MoveReason::BackgroundCommand)
    );
}

#[test]
fn the_background_command_yields_to_a_stop_a_drain_and_a_shell_exit() {
    assert_eq!(
        running_step(
            MovePolicy::Foreground,
            true,
            false,
            false,
            None,
            false,
            true
        ),
        Step::Stop(StopKind::Timeout)
    );
    assert_eq!(
        running_step(
            MovePolicy::Foreground,
            false,
            true,
            false,
            None,
            false,
            true
        ),
        Step::Stop(StopKind::Cancel)
    );
    assert_eq!(
        running_step(
            MovePolicy::Foreground,
            false,
            false,
            true,
            None,
            false,
            true
        ),
        Step::Drain
    );
    assert_eq!(
        running_step(
            MovePolicy::Foreground,
            false,
            false,
            false,
            Some(3),
            false,
            true
        ),
        Step::Move(MoveReason::ShellExited { code: 3 })
    );
}

#[test]
fn the_background_command_does_not_move_a_command_that_never_moves() {
    assert_eq!(
        running_step(MovePolicy::Stay, false, false, false, None, false, true),
        Step::Park
    );
    assert_eq!(
        running_step(
            MovePolicy::Background,
            false,
            false,
            false,
            None,
            false,
            true
        ),
        Step::Move(MoveReason::StartedInBackground)
    );
}

#[test]
fn a_shell_without_jobs_never_moves() {
    assert_eq!(
        running_step(MovePolicy::Stay, false, false, false, Some(3), true, false),
        Step::Park
    );
    assert_eq!(
        running_step(MovePolicy::Stay, false, false, false, None, true, false),
        Step::Park
    );
}

#[test]
fn the_park_deadline_is_the_earlier_of_the_timeout_and_thirty_seconds() {
    let thirty = at(30);
    let ten_minutes = at(600);
    assert_eq!(
        wait_deadline(MovePolicy::Foreground, Some(ten_minutes), Some(thirty)),
        Some(thirty)
    );
    assert_eq!(
        wait_deadline(MovePolicy::Foreground, Some(thirty), Some(ten_minutes)),
        Some(thirty)
    );
    assert_eq!(
        wait_deadline(MovePolicy::Foreground, Some(thirty), Some(thirty)),
        Some(thirty)
    );
    assert_eq!(
        wait_deadline(MovePolicy::Stay, Some(ten_minutes), Some(thirty)),
        Some(ten_minutes)
    );
    assert_eq!(
        wait_deadline(MovePolicy::Background, Some(ten_minutes), Some(thirty)),
        Some(ten_minutes)
    );
    assert_eq!(
        wait_deadline(MovePolicy::Foreground, None, Some(thirty)),
        Some(thirty)
    );
    assert_eq!(
        wait_deadline(MovePolicy::Foreground, Some(thirty), None),
        Some(thirty)
    );
    assert_eq!(wait_deadline(MovePolicy::Foreground, None, None), None);
}

#[test]
fn a_missing_exit_code_is_reported_as_zero() {
    assert_eq!(exit_code_of(None), 0);
    let killed = Command::new("sh")
        .args(["-c", "kill -ABRT $$"])
        .status()
        .unwrap();
    assert!(killed.code().is_none());
    assert_eq!(exit_code_of(Some(killed)), 0);
    let exited = Command::new("sh").args(["-c", "exit 3"]).status().unwrap();
    assert_eq!(exit_code_of(Some(exited)), 3);
}

#[test]
fn attaching_a_file_keeps_each_byte_once_in_order() {
    let dir = fakes::TempDir::new("fiber-shell-attach");
    let path = dir.path().join("out.log");
    let shared = Arc::new(Shared::default());
    lock(&shared.inner).output.extend_from_slice(b"ab");
    let moved = parked(Arc::clone(&shared));
    moved.attach_output(std::fs::File::create(&path).unwrap());
    assert!(lock(&shared.inner).output.is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), b"ab");
    read_output(
        Scripted {
            steps: vec![Ok(b"cd".to_vec())],
        },
        &shared,
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"abcd");
    assert!(lock(&shared.inner).output.is_empty());
}

#[test]
fn bytes_after_discard_are_not_written_to_the_file() {
    let dir = fakes::TempDir::new("fiber-shell-discard");
    let path = dir.path().join("out.log");
    let shared = Arc::new(Shared::default());
    lock(&shared.inner).output.extend_from_slice(b"ab");
    parked(Arc::clone(&shared)).attach_output(std::fs::File::create(&path).unwrap());
    lock(&shared.inner).discard = true;
    read_output(
        Scripted {
            steps: vec![Ok(b"no".to_vec())],
        },
        &shared,
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"ab");
}

fn parked(shared: Arc<Shared>) -> Moved {
    Moved {
        reason: MoveReason::StartedInBackground,
        bridge: None,
        progress: Run {
            phase: Phase::Running,
            stop: None,
            sent_signal: false,
            seen_empty: false,
            streamed: 0,
            timeout_at: None,
            move_at: None,
            pgid: 2,
            shared,
            ask: None,
            job: None,
            feed: None,
        },
        cap: OUTPUT_CAP,
    }
}

#[test]
fn the_cap_stops_a_running_job_like_a_cancel() {
    let shared = Shared::default();
    let cancel = CancelToken::new();
    assert!(!super::view(&shared, &cancel).cancelled);
    lock(&shared.inner).cap_fired = true;
    assert!(super::view(&shared, &cancel).cancelled);
}

#[test]
fn finish_reports_a_fired_cap() {
    let shared = Shared::default();
    let open = |shared: &Shared| finish(shared, None, false, true, true, &Recorder::default(), 0);
    assert!(!open(&shared).capped);
    lock(&shared.inner).cap_fired = true;
    assert!(open(&shared).capped);
}

#[test]
fn the_sooner_of_two_instants() {
    let early = at(1);
    let late = at(2);
    assert_eq!(super::sooner(Some(late), Some(early)), Some(early));
    assert_eq!(super::sooner(Some(early), Some(late)), Some(early));
    assert_eq!(super::sooner(Some(late), None), Some(late));
    assert_eq!(super::sooner(None, Some(early)), Some(early));
    assert_eq!(super::sooner(None, None), None);
}

const JOB_DEADLINE: Duration = Duration::from_secs(10);

/// A command moved to a job and driven on its own thread. The command
/// writes its group id, then holds `block` open read-write on fd 3, and each
/// `go` lets it past its next `read -r _ <&3`. The one descriptor, held by
/// both sides, never rendezvouses on an open and never reads end-of-file.
struct Job {
    dir: fakes::TempDir,
    clock: Arc<FakeClock>,
    deltas: Arc<fakes::jobs::JobDeltas>,
    block: std::fs::File,
    done: mpsc::Receiver<super::Finished>,
    out: std::path::PathBuf,
    pgid: u32,
    watchdog: fakes::Watchdog,
    _ready: fakes::children::Ready,
}

impl Job {
    fn start(name: &str, cap: u64, script: &str) -> Self {
        let dir = fakes::TempDir::new(name);
        let ready = fakes::children::Ready::new(dir.path());
        let block = dir.path().join("block");
        let command = format!(
            "echo $$ > '{ready}'\nmkfifo '{block}'\nexec 3<>'{block}'\necho $$ >> '{ready}'\n{script}",
            ready = ready.path().display(),
            block = block.display(),
        );
        let clock = FakeClock::new();
        let cancel = CancelToken::new();
        let ran = super::execute(
            std::path::Path::new("/bin/sh"),
            &command,
            dir.path(),
            Duration::from_secs(3600),
            clock.as_ref(),
            &cancel,
            &Recorder::default(),
            MovePolicy::Background,
            None,
        )
        .unwrap();
        let super::Ran::Moved(mut moved) = ran else {
            panic!("the command did not move");
        };
        let pgid = moved.pgid();
        let watchdog = fakes::Watchdog::group(pgid);
        moved.cap = cap;
        let out = dir.path().join("job.log");
        moved.attach_output(std::fs::File::create(&out).unwrap());
        moved.arm(&cancel);
        moved.detach_call_cancel();
        let deltas = Arc::new(fakes::jobs::JobDeltas::default());
        let stream = super::JobStream::new(
            contract::JobId("j_t".to_owned()),
            Arc::clone(&deltas) as Arc<dyn contract::emit::Emit>,
        );
        let job_clock = Arc::clone(&clock);
        let (tx, done) = mpsc::channel();
        thread::spawn(move || {
            let finished = moved.drive_job(job_clock.as_ref(), &cancel, stream, None);
            let _sent = tx.send(finished);
        });
        // Both lines: the fifo exists and fd 3 is open once the second arrives.
        assert_eq!(ready.wait(JOB_DEADLINE)[0], pgid);
        let _own = ready.wait(JOB_DEADLINE);
        // Read-write, so the open never waits for the shell.
        let block = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&block)
            .unwrap();
        Self {
            dir,
            clock,
            deltas,
            block,
            done,
            out,
            pgid,
            watchdog,
            _ready: ready,
        }
    }

    /// Lets the command past its next read.
    fn go(&self) {
        std::io::Write::write_all(&mut &self.block, b"go\n").unwrap();
    }

    /// The job's end, waited for at most [`JOB_DEADLINE`].
    fn end(self) -> (super::Finished, fakes::TempDir, Arc<fakes::jobs::JobDeltas>) {
        let finished = self
            .done
            .recv_timeout(JOB_DEADLINE)
            .expect("the job's drive did not return within the deadline");
        assert!(!group_alive(self.pgid), "the group was left running");
        self.watchdog.stand_down(JOB_DEADLINE);
        (finished, self.dir, self.deltas)
    }
}

#[test]
fn output_past_the_cap_stops_the_job_and_the_file_keeps_the_cap() {
    let job = Job::start(
        "fiber-job-cap",
        1000,
        "read -r _ <&3\nhead -c 2000 /dev/zero | tr '\\0' x\nread -r _ <&3\n",
    );
    job.go();
    let out = job.out.clone();
    let (finished, _dir, _deltas) = job.end();
    assert!(finished.capped);
    assert_eq!(finished.stop, Some(StopKind::Cancel));
    assert_eq!(std::fs::read(out).unwrap().len(), 1000);
}

#[test]
fn a_command_that_passes_the_cap_and_exits_at_once_still_ends_capped() {
    let job = Job::start(
        "fiber-job-cap-exit",
        1000,
        "read -r _ <&3\nhead -c 2000 /dev/zero | tr '\\0' x\n",
    );
    job.go();
    let out = job.out.clone();
    let (finished, _dir, _deltas) = job.end();
    assert!(finished.capped);
    assert_eq!(std::fs::read(out).unwrap().len(), 1000);
}

#[test]
fn a_command_under_the_cap_is_unaffected() {
    let job = Job::start(
        "fiber-job-under-cap",
        1000,
        "read -r _ <&3\nhead -c 500 /dev/zero | tr '\\0' x\n",
    );
    job.go();
    let out = job.out.clone();
    let (finished, _dir, _deltas) = job.end();
    assert!(!finished.capped);
    assert_eq!(finished.stop, None);
    assert_eq!(exit_code_of(finished.status), 0);
    assert_eq!(std::fs::read(out).unwrap().len(), 500);
}

#[test]
fn a_job_streams_paced_deltas_and_flushes_the_rest_at_its_end() {
    let job = Job::start(
        "fiber-job-delta",
        super::OUTPUT_CAP,
        "read -r _ <&3\nprintf one\nread -r _ <&3\nprintf two\nread -r _ <&3\nprintf three\n",
    );
    let due = job.clock.origin() + Duration::from_millis(100);
    job.go();
    assert!(
        job.deltas.wait_for_text("one", JOB_DEADLINE),
        "waited {JOB_DEADLINE:?} for the first delta"
    );
    job.go();
    // Parked at the interval: the second write is held, not emitted.
    assert!(
        job.clock.await_parked(due, JOB_DEADLINE),
        "waited {JOB_DEADLINE:?} for the job to park for the held delta"
    );
    assert_eq!(job.deltas.text(), "one");
    job.clock.advance(Duration::from_millis(100));
    assert!(
        job.deltas.wait_for_text("onetwo", JOB_DEADLINE),
        "waited {JOB_DEADLINE:?} for the held delta after the interval"
    );
    job.go();
    let (finished, _dir, deltas) = job.end();
    assert!(!finished.capped);
    let texts: Vec<_> = deltas.deltas().into_iter().map(|(_, text)| text).collect();
    assert_eq!(texts, ["one", "two", "three"]);
}

/// A monitor moves as `run_in_background` does: a timeout (its deadline)
/// and an empty group come first, then it moves on the first pass.
#[test]
fn a_monitor_moves_on_the_first_pass_after_its_deadline_and_an_empty_group() {
    let step = |timed_out, empty| {
        running_step(
            MovePolicy::Monitor,
            timed_out,
            false,
            empty,
            None,
            false,
            false,
        )
    };
    assert_eq!(step(true, true), Step::Stop(StopKind::Timeout));
    assert_eq!(step(false, true), Step::Drain);
    assert_eq!(
        step(false, false),
        Step::Move(MoveReason::StartedInBackground)
    );
    let clock = FakeClock::new();
    let deadline = clock.now() + Duration::from_secs(300);
    assert_eq!(
        wait_deadline(
            MovePolicy::Monitor,
            Some(deadline),
            Some(clock.now() + Duration::from_secs(30))
        ),
        Some(deadline)
    );
}
