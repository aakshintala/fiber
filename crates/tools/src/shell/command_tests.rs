use fakes::MUST_SUCCEED_WITHIN;
use std::io::{self, ErrorKind, Read};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::process::Command;
use std::sync::{Arc, TryLockError, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use contract::clock::{Clock, Wake};
use fakes::CancelToken;
use fakes::Deadline;
use fakes::Recorder;
use fakes::clock::FakeClock;

use super::super::background::{GroupMembers, PS_BOUND, Step, running_step, wait_deadline};
use super::super::drive::{LoopEnd, Phase, Run, exit_code_of, finish, park, pump, pump_inner};
use super::super::moved::Moved;
use super::super::output::{
    Inner, OUTPUT_CAP, Shared, bump, complete_prefix, lock, note_eof, read_output, stream_output,
};
use super::{MovePolicy, MoveReason, StopKind};
use fakes::children::{Ready, ignores_sigterm};
use support::group::alive;

#[test]
fn bump_advances_the_sequence() {
    let mut inner = Inner::default();
    bump(&mut inner);
    assert_eq!(inner.seq, 1);
}

#[test]
fn an_open_pipe_is_discarded_once_the_run_returns() {
    let shared = Shared::default();
    finish(
        &shared,
        None,
        false,
        true,
        false,
        &Recorder::default(),
        &mut Vec::new(),
    );
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
        &mut Vec::new(),
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
        &mut Vec::new(),
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
        &mut Vec::new(),
    );
    assert!(!still_occupied.held_open);

    let stopped_open = finish(
        &Shared::default(),
        Some(StopKind::Cancel),
        true,
        true,
        false,
        &Recorder::default(),
        &mut Vec::new(),
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
        &mut Vec::new(),
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
        &mut Vec::new(),
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

#[track_caller]
fn assert_park_returns(clock: BoundlessClock, shared: Arc<Shared>, seen: u64) {
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
        Deadline::after(MUST_SUCCEED_WITHIN).recv(&finished).is_ok(),
        "waited {MUST_SUCCEED_WITHIN:?} for park to return"
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

/// Truth table for `park`'s `guard.seq != seen || (wake_on_cancel &&
/// cancel.is_cancelled())`: each row flips one operand, so flipping either
/// operator changes a row. The moved-sequence row kills the `||`-to-`&&`
/// mutant; the two mixed cancel rows kill the `&&`-to-`||` mutant.
#[test]
fn park_wakes_on_sequence_or_on_cancel_while_running() {
    const STILL_PARKED: Duration = Duration::from_millis(200);
    // (sequence moved, wake_on_cancel, cancelled, park returns).
    for (moved, wake_on_cancel, cancelled, returns) in [
        (true, false, false, true),
        (false, false, false, false),
        (false, true, true, true),
        (false, true, false, false),
        (false, false, true, false),
    ] {
        let shared = Arc::new(Shared::default());
        if moved {
            shared.wake();
        }
        let cancel = CancelToken::new();
        if cancelled {
            cancel.cancel();
        }
        let origin = FakeClock::new().origin();
        let until = origin.checked_add(Duration::from_secs(2));
        let (done, parked) = mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        thread::spawn(move || {
            park(
                &BoundlessClock { origin, wake: None },
                &thread_shared,
                &cancel,
                until,
                false,
                wake_on_cancel,
                0,
            );
            done.send(()).unwrap();
        });
        if returns {
            assert!(
                Deadline::after(MUST_SUCCEED_WITHIN).recv(&parked).is_ok(),
                "park returned (moved={moved} wake_on_cancel={wake_on_cancel} cancelled={cancelled})"
            );
        } else {
            assert!(
                Deadline::after(STILL_PARKED).recv(&parked).is_err(),
                "park stayed (moved={moved} wake_on_cancel={wake_on_cancel} cancelled={cancelled})"
            );
            shared.wake();
            assert!(
                Deadline::after(MUST_SUCCEED_WITHIN).recv(&parked).is_ok(),
                "the stayed park was released (moved={moved} wake_on_cancel={wake_on_cancel} cancelled={cancelled})"
            );
        }
    }
}

/// How long a listing test waits on a command before it fails.
const LIST_DEADLINE: Duration = Duration::from_secs(10);

/// Runs `command` in the foreground with no move, on its own thread, and
/// returns how it finished.
fn run(dir: &std::path::Path, command: String) -> mpsc::Receiver<super::Finished> {
    let dir = dir.to_path_buf();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let clock = FakeClock::new();
        let cancel = CancelToken::new();
        let sandbox = crate::shell::sandbox::Settings {
            enabled: false,
            allow_write: Vec::new(),
            deny_read: Vec::new(),
            fiber_home: std::path::PathBuf::new(),
        };
        let ran = super::execute(
            std::path::Path::new("/bin/sh"),
            &command,
            &dir,
            Duration::from_secs(3600),
            clock.as_ref(),
            &cancel,
            &Recorder::default(),
            MovePolicy::Stay,
            None,
            None,
            None,
            &sandbox,
            &dir,
        )
        .unwrap();
        let super::Ran::Finished(finished) = ran else {
            panic!("a command that may not move moved");
        };
        let _sent = tx.send(finished);
    });
    rx
}

/// Whether `pgid` is listed within [`LIST_DEADLINE`]. The poll runs on its
/// own thread, so a stuck listing fails the test instead of hanging it.
fn wait_listed(pgid: u32) -> bool {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        while !support::group::live().contains(pgid) {
            thread::yield_now();
        }
        let _sent = tx.send(());
    });
    Deadline::after(LIST_DEADLINE).recv(&rx).is_ok()
}

#[test]
fn a_command_is_listed_while_it_runs_and_not_once_it_ends() {
    let dir = fakes::TempDir::new("fiber-groups-listed");
    let ready = Ready::new(dir.path());
    let done = run(dir.path(), ignores_sigterm(ready.path()));
    let pgid = ready.wait(fakes::Deadline::after(LIST_DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(LIST_DEADLINE));
    // The child's ready files do not prove the listing: `spawn` lists the
    // group only after `Command::spawn` returns.
    assert!(wait_listed(pgid), "the running group is listed");

    support::group::kill_every_group();
    // The run ends on its own pass: no cancel, no clock move.
    let finished = Deadline::after(LIST_DEADLINE)
        .recv(&done)
        .expect("the run ended");
    assert_eq!(finished.status.and_then(|status| status.signal()), Some(9));
    assert_eq!(finished.stop, None);
    assert!(!fakes::kill_group(pgid, "0").unwrap(), "the group is gone");
    assert!(
        !support::group::live().contains(pgid),
        "an empty group leaves the list"
    );
    watchdog.stand_down(fakes::Deadline::after(LIST_DEADLINE));
}

#[test]
fn a_finished_command_is_not_listed() {
    let dir = fakes::TempDir::new("fiber-groups-done");
    let ready = Ready::new(dir.path());
    let done = run(
        dir.path(),
        format!("echo $$ > '{}'", ready.path().display()),
    );
    let pgid = ready.wait(fakes::Deadline::after(LIST_DEADLINE))[0];
    let finished = Deadline::after(LIST_DEADLINE)
        .recv(&done)
        .expect("the run ended");
    assert_eq!(finished.status.and_then(|status| status.code()), Some(0));
    assert!(!support::group::live().contains(pgid));
}

/// The drain polls the group while it may still be occupied: with the
/// shell reaped and the pipe closed but the group alive, the drive notices
/// the group emptying with no further wake, long before the drain bound.
/// Without the poll (removed, or inverted to poll only once empty) the
/// drive sleeps until the bound instead.
#[test]
fn the_drain_polls_while_the_group_may_be_occupied() {
    let mut cmd = Command::new("sleep");
    cmd.arg("60").process_group(0);
    let mut child = cmd.spawn().expect("sleep spawned");
    let pgid = child.id();
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(alive(pgid), "the running group probes live");

    // The shell is reaped and the pipe closed, but the group still holds
    // the sleeper: the drain below starts occupied.
    let shared = Arc::new(Shared::default());
    note_eof(&shared);
    lock(&shared.inner).reaped = true;

    let clock = FakeClock::new();
    let until = clock
        .origin()
        .checked_add(support::group::DRAIN)
        .expect("the drain bound fits");
    let (done, finished) = mpsc::channel();
    let drive_shared = Arc::clone(&shared);
    let drive_clock = Arc::clone(&clock);
    thread::spawn(move || {
        let mut progress = Run {
            phase: Phase::Draining { until },
            stop: None,
            sent_signal: false,
            seen_empty: false,
            stream_carry: Vec::new(),
            timeout_at: None,
            move_at: None,
            members: None,
            pgid,
            listing: None,
            shared: drive_shared,
            ask: None,
            job: None,
            feed: None,
        };
        let cancel = CancelToken::new();
        let end = pump(
            &mut progress,
            MovePolicy::Stay,
            drive_clock.as_ref(),
            &cancel,
            &Recorder::default(),
        );
        let _sent = done.send((progress.seen_empty, end));
    });

    // The sleeper dies with no output and no wake: only the drain's poll
    // can notice it is gone.
    fakes::kill_group(pgid, "KILL").expect("the kill sent");
    let (reaped, waited) = mpsc::channel();
    thread::spawn(move || {
        let _sent = reaped.send(child.wait());
    });
    let status = Deadline::after(LIST_DEADLINE)
        .recv(&waited)
        .expect("the killed child was reaped")
        .unwrap();
    assert_eq!(status.signal(), Some(9));
    let (saw_empty, end) = Deadline::after(LIST_DEADLINE)
        .recv(&finished)
        .expect("the drain noticed the emptied group within the deadline");
    assert!(saw_empty, "the drain saw the emptied group");
    let super::super::drive::LoopEnd::Finished(_) = end else {
        panic!("an occupied drain that empties finishes");
    };
    watchdog.stand_down(fakes::Deadline::after(LIST_DEADLINE));
}

/// `send_term` signals a group only when it was neither signalled nor seen
/// empty: each of the four pairs against a live group. A suppressed send
/// leaves the group running; the one real send ends it with SIGTERM. The
/// two mixed pairs kill the `||`-to-`&&` mutant, which would signal them.
#[test]
fn send_term_signals_only_a_group_neither_signalled_nor_empty() {
    use super::super::process_group::send_term;

    fn reap(mut child: std::process::Child) -> std::process::ExitStatus {
        let (done, waited) = mpsc::channel();
        thread::spawn(move || {
            let _sent = done.send(child.wait());
        });
        Deadline::after(LIST_DEADLINE)
            .recv(&waited)
            .expect("the child answered")
            .unwrap()
    }

    // (already sent, seen empty, the send signals, what it returns).
    for (sent, seen_empty, signalled, want) in [
        (false, false, true, true),
        (true, false, false, true),
        (false, true, false, false),
        (true, true, false, true),
    ] {
        let mut cmd = Command::new("sleep");
        cmd.arg("60").process_group(0);
        let child = cmd.spawn().expect("sleep spawned");
        let pgid = child.id();
        let watchdog = fakes::Watchdog::group(pgid);
        assert!(alive(pgid), "the running group probes live");

        assert_eq!(
            send_term(pgid, sent, seen_empty),
            want,
            "sent={sent} seen_empty={seen_empty}"
        );
        if signalled {
            assert_eq!(
                reap(child).signal(),
                Some(15),
                "SIGTERM ended the signalled group"
            );
        } else {
            assert!(
                alive(pgid),
                "a suppressed send leaves the group running \
                 (sent={sent} seen_empty={seen_empty})"
            );
            fakes::kill_group(pgid, "KILL").expect("the cleanup kill sent");
            assert_eq!(reap(child).signal(), Some(9));
        }
        watchdog.stand_down(fakes::Deadline::after(LIST_DEADLINE));
    }
}

#[test]
fn kill_every_group_kills_a_listed_group_and_then_drops_it() {
    let mut cmd = Command::new("sleep");
    cmd.arg("60");
    support::group::detach(&mut cmd, false);
    let (mut child, listing) = support::group::spawn(&mut cmd).expect("sleep spawned");
    let pgid = child.id();
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(support::group::live().contains(pgid));

    support::group::kill_every_group();
    let (done, waited) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = Deadline::after(LIST_DEADLINE)
        .recv(&waited)
        .expect("the killed child was reaped")
        .unwrap();
    assert_eq!(status.signal(), Some(9));
    // Reaped, the group is empty: the next kill drops it unsignalled.
    assert!(support::group::live().contains(pgid));
    // The probe can lag the reap (see `support::group::alive`'s doc): poll it until the kill prunes.
    fakes::within(
        &format!("group {pgid} to read not alive"),
        Deadline::after(LIST_DEADLINE),
        move || {
            let (_tick, ticked) = mpsc::channel::<()>();
            while alive(pgid) {
                let _waited = Deadline::after(support::group::GROUP_POLL).recv(&ticked);
            }
        },
    );
    support::group::kill_every_group();
    assert!(!support::group::live().contains(pgid));
    support::group::live().unlist(listing);
    watchdog.stand_down(fakes::Deadline::after(LIST_DEADLINE));
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
    let mut carry = Vec::new();
    read_output(
        Scripted {
            steps: vec![Ok(b"a\xC3".to_vec())],
        },
        &shared,
    );
    stream_output(&shared, &recorder, &mut carry);
    assert_eq!(recorder.text(), "a");
    assert_eq!(carry, b"\xC3");
    read_output(
        Scripted {
            steps: vec![Ok(b"\xA9b".to_vec())],
        },
        &shared,
    );
    stream_output(&shared, &recorder, &mut carry);
    assert_eq!(recorder.text(), "a\u{e9}b");
    assert!(carry.is_empty());
}

#[test]
fn an_invalid_byte_streams_as_the_replacement_character() {
    let shared = Shared::default();
    let recorder = Recorder::default();
    read_output(
        Scripted {
            steps: vec![Ok(b"\xFFx".to_vec())],
        },
        &shared,
    );
    stream_output(&shared, &recorder, &mut Vec::new());
    assert_eq!(recorder.text(), "�x");
}

#[test]
fn finish_streams_the_queued_tail_lossily() {
    let shared = Shared::default();
    let recorder = Recorder::default();
    let mut carry = Vec::new();
    read_output(
        Scripted {
            steps: vec![Ok(b"a\xE2\x82".to_vec())],
        },
        &shared,
    );
    stream_output(&shared, &recorder, &mut carry);
    let finished = finish(&shared, None, false, true, true, &recorder, &mut carry);
    assert!(
        matches!(finished.output, super::Kept::Whole(bytes) if bytes == b"a\xE2\x82"),
        "the result keeps the whole output"
    );
    // The tail streams once: "a" is not repeated, and the concatenated
    // texts equal the lossy whole output.
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
        .args(["-c", "kill -USR1 $$"])
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
    moved.attach_output(std::fs::File::create(&path).unwrap(), &path);
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
    parked(Arc::clone(&shared)).attach_output(std::fs::File::create(&path).unwrap(), &path);
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
            stream_carry: Vec::new(),
            timeout_at: None,
            move_at: None,
            members: None,
            pgid: 2,
            listing: None,
            shared,
            ask: None,
            job: None,
            feed: None,
        },
        cap: OUTPUT_CAP,
        spill: None,
    }
}

#[test]
fn the_cap_stops_a_running_job_like_a_cancel() {
    let shared = Shared::default();
    let cancel = CancelToken::new();
    assert!(!super::super::drive::view(&shared, &cancel).cancelled);
    lock(&shared.inner).cap_fired = true;
    assert!(super::super::drive::view(&shared, &cancel).cancelled);
}

#[test]
fn finish_reports_a_fired_cap() {
    let shared = Shared::default();
    let open = |shared: &Shared| {
        finish(
            shared,
            None,
            false,
            true,
            true,
            &Recorder::default(),
            &mut Vec::new(),
        )
    };
    assert!(!open(&shared).capped);
    lock(&shared.inner).cap_fired = true;
    assert!(open(&shared).capped);
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
        let sandbox = crate::shell::sandbox::Settings {
            enabled: false,
            allow_write: Vec::new(),
            deny_read: Vec::new(),
            fiber_home: std::path::PathBuf::new(),
        };
        let workspace = dir.path().to_path_buf();
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
            None,
            None,
            &sandbox,
            &workspace,
        )
        .unwrap();
        let super::Ran::Moved(mut moved) = ran else {
            panic!("the command did not move");
        };
        let pgid = moved.pgid();
        let watchdog = fakes::Watchdog::group(pgid);
        moved.cap = cap;
        let out = dir.path().join("job.log");
        moved.attach_output(std::fs::File::create(&out).unwrap(), &out);
        moved.arm(&cancel);
        moved.detach_call_cancel();
        let deltas = Arc::new(fakes::jobs::JobDeltas::default());
        let stream = super::super::output::JobStream::new(
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
        assert_eq!(ready.wait(fakes::Deadline::after(JOB_DEADLINE))[0], pgid);
        let _own = ready.wait(fakes::Deadline::after(JOB_DEADLINE));
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
    #[track_caller]
    fn end(self) -> (super::Finished, fakes::TempDir, Arc<fakes::jobs::JobDeltas>) {
        let finished = Deadline::after(JOB_DEADLINE)
            .recv(&self.done)
            .expect("the job's drive did not return within the deadline");
        assert!(!alive(self.pgid), "the group was left running");
        self.watchdog
            .stand_down(fakes::Deadline::after(JOB_DEADLINE));
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
        job.deltas
            .wait_for_text("one", fakes::Deadline::after(JOB_DEADLINE)),
        "waited {JOB_DEADLINE:?} for the first delta"
    );
    job.go();
    // Parked at the interval: the second write is held, not emitted.
    assert!(
        job.clock
            .await_parked(due, fakes::Deadline::after(JOB_DEADLINE)),
        "waited {JOB_DEADLINE:?} for the job to park for the held delta"
    );
    assert_eq!(job.deltas.text(), "one");
    job.clock.advance_when_parked(
        Some(due),
        None,
        Duration::from_millis(100),
        &Deadline::after(JOB_DEADLINE),
    );
    assert!(
        job.deltas
            .wait_for_text("onetwo", fakes::Deadline::after(JOB_DEADLINE)),
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

use super::super::spill::Config as SpillConfig;

fn spill_config(dir: &std::path::Path) -> SpillConfig {
    SpillConfig {
        dir: dir.to_path_buf(),
        bound: contract::tool::Bound {
            start: 8192,
            end: 8192,
        },
        wrap: None,
        name: None,
    }
}

#[test]
fn a_foreground_command_past_a_mebibyte_spills() {
    let dir = fakes::TempDir::new("fiber-shell-foreground-spill");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    // The blocking call runs on its own thread: a stalled child fails
    // the test at the deadline instead of hanging it (`docs/testing.md`,
    // "Waits and timeouts"). The command ends on its own; the fake clock
    // is never advanced.
    let work = dir.path().to_path_buf();
    let ran = fakes::within(
        "a foreground command past a mebibyte",
        Deadline::after(JOB_DEADLINE),
        move || {
            let sandbox = crate::shell::sandbox::Settings {
                enabled: false,
                allow_write: Vec::new(),
                deny_read: Vec::new(),
                fiber_home: std::path::PathBuf::new(),
            };
            let workspace = work.clone();
            super::execute(
                std::path::Path::new("/bin/sh"),
                "head -c 1048577 /dev/zero | tr '\\0' a",
                &work,
                Duration::from_secs(60),
                FakeClock::new().as_ref(),
                &CancelToken::new(),
                &Recorder::default(),
                MovePolicy::Foreground,
                None,
                Some(spill_config(&artifacts)),
                None,
                &sandbox,
                &workspace,
            )
            .unwrap()
        },
    );
    let super::Ran::Finished(finished) = ran else {
        panic!("a quick command moved");
    };
    let super::Kept::Spilled(spill) = finished.output else {
        panic!("a command past a mebibyte spills");
    };
    let spilled = spill.close(None, "Exit code 0.");
    assert!(spilled.file.is_ok());
    assert_eq!(spilled.removed, 1048577 + 14 - 16_384);
}

#[test]
fn a_command_moved_at_once_never_spills() {
    let dir = fakes::TempDir::new("fiber-shell-background-no-spill");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let release = dir.path().join("release");
    // The command holds until the test observes the move, so the move
    // always wins the race with the command's end. The loop is bounded,
    // so a failure before the release still ends the child.
    let held = format!(
        "i=0\nwhile test \"$i\" -lt 600; do\ntest -e '{}' && exit 0\ni=$((i + 1))\nsleep 0.1\ndone\n",
        release.display()
    );
    let clock = FakeClock::new();
    let cancel = CancelToken::new();
    let work = dir.path().to_path_buf();
    // `execute` and `resume` both block: each runs on its own thread
    // under the deadline (`docs/testing.md`, "Waits and timeouts").
    let moved = fakes::within(
        "a background command moves",
        Deadline::after(JOB_DEADLINE),
        {
            let clock = Arc::clone(&clock);
            let cancel = cancel.clone();
            let artifacts = artifacts.clone();
            move || {
                let sandbox = crate::shell::sandbox::Settings {
                    enabled: false,
                    allow_write: Vec::new(),
                    deny_read: Vec::new(),
                    fiber_home: std::path::PathBuf::new(),
                };
                let workspace = work.clone();
                let ran = super::execute(
                    std::path::Path::new("/bin/sh"),
                    &held,
                    &work,
                    Duration::from_secs(60),
                    clock.as_ref(),
                    &cancel,
                    &Recorder::default(),
                    MovePolicy::Background,
                    None,
                    Some(spill_config(&artifacts)),
                    None,
                    &sandbox,
                    &workspace,
                )
                .unwrap();
                let super::Ran::Moved(moved) = ran else {
                    panic!("a background command did not move");
                };
                moved
            }
        },
    );
    // The move is observed: releasing the command lets it end.
    std::fs::write(&release, b"go\n").unwrap();
    {
        let inner = super::super::output::lock(&moved.progress.shared.inner);
        assert!(inner.spill_config.is_none());
        assert!(inner.spill.is_none());
    }
    // The move's command ends cleanly back in the foreground.
    let finished = fakes::within("a moved command resumes", Deadline::after(JOB_DEADLINE), {
        let clock = Arc::clone(&clock);
        let cancel = cancel.clone();
        move || moved.resume(clock.as_ref(), &cancel, &Recorder::default())
    });
    assert!(matches!(finished.output, super::Kept::Whole(_)));
    assert!(std::fs::read_dir(&artifacts).unwrap().next().is_none());
}

#[test]
fn a_resumed_command_with_a_file_does_not_arm_the_spill() {
    let dir = fakes::TempDir::new("fiber-shell-resume-filed");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let release = dir.path().join("release");
    // The command holds until the test observes the move, so the move
    // always wins the race with the command's end. The loop is bounded,
    // so a failure before the release still ends the child.
    let held = format!(
        "i=0\nwhile test \"$i\" -lt 600; do\ntest -e '{}' && exit 0\ni=$((i + 1))\nsleep 0.1\ndone\n",
        release.display()
    );
    let clock = FakeClock::new();
    let cancel = CancelToken::new();
    let work = dir.path().to_path_buf();
    // `execute` and `resume` both block: each runs on its own thread
    // under the deadline (`docs/testing.md`, "Waits and timeouts").
    let moved = fakes::within(
        "a background command moves",
        Deadline::after(JOB_DEADLINE),
        {
            let clock = Arc::clone(&clock);
            let cancel = cancel.clone();
            move || {
                let sandbox = crate::shell::sandbox::Settings {
                    enabled: false,
                    allow_write: Vec::new(),
                    deny_read: Vec::new(),
                    fiber_home: std::path::PathBuf::new(),
                };
                let workspace = work.clone();
                let ran = super::execute(
                    std::path::Path::new("/bin/sh"),
                    &held,
                    &work,
                    Duration::from_secs(60),
                    clock.as_ref(),
                    &cancel,
                    &Recorder::default(),
                    MovePolicy::Background,
                    None,
                    Some(spill_config(&artifacts)),
                    None,
                    &sandbox,
                    &workspace,
                )
                .unwrap();
                let super::Ran::Moved(moved) = ran else {
                    panic!("a background command did not move");
                };
                moved
            }
        },
    );
    // The job's file arrived first: the resume must not arm the spill
    // over it, so the buffered output stays whole. Injected while the
    // command is still held, so its end cannot land before this state.
    {
        let mut inner = super::super::output::lock(&moved.progress.shared.inner);
        inner.output = vec![b'y'; super::super::spill::SPILL_AT + 1];
        inner.file = Some(std::fs::File::create(dir.path().join("j_1.log")).unwrap());
    }
    // The move is observed: releasing the command lets it end.
    std::fs::write(&release, b"go\n").unwrap();
    let finished = fakes::within("a filed command resumes", Deadline::after(JOB_DEADLINE), {
        let clock = Arc::clone(&clock);
        let cancel = cancel.clone();
        move || moved.resume(clock.as_ref(), &cancel, &Recorder::default())
    });
    assert!(
        matches!(finished.output, super::Kept::Whole(_)),
        "a resumed command with a file stays whole"
    );
}

#[test]
fn a_resumed_command_arms_the_spill() {
    let dir = fakes::TempDir::new("fiber-shell-resume-spill");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let release = dir.path().join("release");
    // The command holds until the test observes the move, so the move
    // always wins the race with the command's end. The loop is bounded,
    // so a failure before the release still ends the child.
    let held = format!(
        "i=0\nwhile test \"$i\" -lt 600; do\ntest -e '{}' && break\ni=$((i + 1))\nsleep 0.1\ndone\nhead -c 2097152 /dev/zero | tr '\\0' a\n",
        release.display()
    );
    let clock = FakeClock::new();
    let cancel = CancelToken::new();
    let work = dir.path().to_path_buf();
    // `execute` and `resume` both block: each runs on its own thread
    // under the deadline (`docs/testing.md`, "Waits and timeouts").
    let moved = fakes::within(
        "a background command moves",
        Deadline::after(JOB_DEADLINE),
        {
            let clock = Arc::clone(&clock);
            let cancel = cancel.clone();
            move || {
                let sandbox = crate::shell::sandbox::Settings {
                    enabled: false,
                    allow_write: Vec::new(),
                    deny_read: Vec::new(),
                    fiber_home: std::path::PathBuf::new(),
                };
                let workspace = work.clone();
                let ran = super::execute(
                    std::path::Path::new("/bin/sh"),
                    &held,
                    &work,
                    Duration::from_secs(60),
                    clock.as_ref(),
                    &cancel,
                    &Recorder::default(),
                    MovePolicy::Background,
                    None,
                    Some(spill_config(&artifacts)),
                    None,
                    &sandbox,
                    &workspace,
                )
                .unwrap();
                let super::Ran::Moved(moved) = ran else {
                    panic!("a background command did not move");
                };
                moved
            }
        },
    );
    // The move is observed: releasing the command lets it run its payload.
    std::fs::write(&release, b"go\n").unwrap();
    // A move on the first pass never spills; the resume arms it, and the
    // command's output past 1 MiB spills from there.
    let finished = fakes::within("a moved command resumes", Deadline::after(JOB_DEADLINE), {
        let clock = Arc::clone(&clock);
        let cancel = cancel.clone();
        move || moved.resume(clock.as_ref(), &cancel, &Recorder::default())
    });
    let super::Kept::Spilled(spill) = finished.output else {
        panic!("a resumed command past a mebibyte spills");
    };
    let spilled = spill.close(None, "Exit code 0.");
    assert!(spilled.file.is_ok());
}

/// The reap window: the group probe can lag the reap while the group is
/// already empty. A successful listing that finds no members finishes the
/// call in the foreground with the shell's output and exit code, however
/// the probe lags.
#[test]
fn a_successful_empty_listing_finishes_foreground_when_alive_probe_lags() {
    fn always_alive(_pgid: u32) -> bool {
        true
    }
    let dir = fakes::TempDir::new("fiber-shell-empty-listing");
    // A `ps` that lists nothing and succeeds: the group has no members.
    let ps = fakes::script(dir.path(), "ps", "exit 0");
    let work = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel();
    // The blocking call runs on its own thread: a stuck drive fails the
    // test at the deadline instead of hanging it.
    thread::spawn(move || {
        let clock = FakeClock::new();
        let cancel = CancelToken::new();
        let sandbox = crate::shell::sandbox::Settings {
            enabled: false,
            allow_write: Vec::new(),
            deny_read: Vec::new(),
            fiber_home: std::path::PathBuf::new(),
        };
        let ran = super::execute_inner(
            std::path::Path::new("/bin/sh"),
            "true",
            &work,
            Duration::from_secs(60),
            clock.as_ref(),
            &cancel,
            &Recorder::default(),
            MovePolicy::Foreground,
            None,
            None,
            None,
            &sandbox,
            &work,
            always_alive,
            &ps,
            super::super::sandbox::install,
        )
        .unwrap();
        let _sent = tx.send(ran);
    });
    let ran = Deadline::after(JOB_DEADLINE)
        .recv(&rx)
        .expect("the foreground call ended within the deadline");
    let super::Ran::Finished(finished) = ran else {
        panic!("a shell with no members left finishes in the foreground");
    };
    assert_eq!(exit_code_of(finished.status), 0);
}

/// Drives one reaped shell with the probe answering alive, so only the
/// member listing can move it. The shell exited with code 3; `ps` is the
/// member-query program under test.
fn drive_shell_exit(ps: &std::path::Path) -> (LoopEnd, Run) {
    fn always_alive(_pgid: u32) -> bool {
        true
    }
    let shared = Arc::new(Shared::default());
    note_eof(&shared);
    {
        let mut inner = lock(&shared.inner);
        inner.reaped = true;
        inner.status = Some(
            Command::new("sh")
                .args(["-c", "exit 3"])
                .status()
                .expect("the exit status"),
        );
    }
    let clock = FakeClock::new();
    let mut progress = Run {
        phase: Phase::Running,
        stop: None,
        sent_signal: false,
        seen_empty: false,
        stream_carry: Vec::new(),
        timeout_at: None,
        move_at: None,
        members: None,
        pgid: 99,
        listing: None,
        shared,
        ask: None,
        job: None,
        feed: None,
    };
    let cancel = CancelToken::new();
    let end = pump_inner(
        &mut progress,
        MovePolicy::Foreground,
        clock.as_ref(),
        &cancel,
        &Recorder::default(),
        always_alive,
        ps,
    );
    (end, progress)
}

/// A listing that names members moves the command at once and saves the
/// names on the run for the receipt, even when the alive probe lags.
#[test]
fn a_members_listing_moves_at_once_and_is_saved_for_the_receipt() {
    let dir = fakes::TempDir::new("fiber-shell-members-save");
    let ps = fakes::script(
        dir.path(),
        "ps",
        "printf '  12  99 sleep\\n  34  99 my cmd\\n'",
    );
    let (end, progress) = drive_shell_exit(&ps);
    let LoopEnd::Move(MoveReason::ShellExited { code }) = end else {
        panic!("a shell that left members moves");
    };
    assert_eq!(code, 3);
    // `docs/tools.md` promises the names, not their order: `ps` prints in
    // its own order, so the test compares the listing as a set.
    let Some(GroupMembers::Members(names)) = &progress.members else {
        panic!(
            "a shell that left members saves them, got {:?}",
            progress.members
        );
    };
    let mut got: Vec<&str> = names.split(", ").collect();
    got.sort_unstable();
    assert_eq!(got, ["my cmd (34)", "sleep (12)"]);
}

/// A listing that never answers moves the command and saves the failure
/// on the run, so no real child is left untracked.
#[test]
fn an_unavailable_listing_moves_and_is_saved_for_the_receipt() {
    let dir = fakes::TempDir::new("fiber-shell-unavailable-save");
    let (end, progress) = drive_shell_exit(&dir.path().join("missing"));
    let LoopEnd::Move(MoveReason::ShellExited { code }) = end else {
        panic!("an unreadable listing still moves");
    };
    assert_eq!(code, 3);
    assert_eq!(progress.members, Some(GroupMembers::Unavailable));
}

/// Only a shell exit lists the group: any other move returns at once
/// without running the member query.
#[test]
fn only_a_shell_exit_lists_the_group_before_moving() {
    let dir = fakes::TempDir::new("fiber-shell-no-list");
    let probe = dir.path().join("probe");
    let ps = fakes::script(
        dir.path(),
        "ps",
        &format!("echo started > '{}'; exit 0", probe.display()),
    );
    let shared = Arc::new(Shared::default());
    let clock = FakeClock::new();
    let mut progress = Run {
        phase: Phase::Running,
        stop: None,
        sent_signal: false,
        seen_empty: false,
        stream_carry: Vec::new(),
        timeout_at: None,
        move_at: Some(clock.origin()),
        members: None,
        pgid: 99,
        listing: None,
        shared,
        ask: None,
        job: None,
        feed: None,
    };
    let cancel = CancelToken::new();
    let end = pump_inner(
        &mut progress,
        MovePolicy::Foreground,
        clock.as_ref(),
        &cancel,
        &Recorder::default(),
        |_pgid| true,
        &ps,
    );
    let LoopEnd::Move(MoveReason::AfterThirtySeconds) = end else {
        panic!("thirty seconds still moves without listing the group");
    };
    assert!(
        !probe.exists(),
        "a move for any other reason runs no listing"
    );
}

/// A stop that becomes due while the member listing is parked wins before
/// any handoff: the listing's answer cannot move a command its timeout or
/// cancellation already claimed. The fixture `ps` reports its start, then
/// holds until the test releases it; the stop lands while it is parked.
#[test]
fn cancel_or_timeout_during_member_listing_wins_before_handoff() {
    fn always_alive(_pgid: u32) -> bool {
        true
    }
    // `true` cancels the call while the listing waits; `false` lets its
    // timeout lapse while the listing waits.
    for cancelled in [true, false] {
        let dir = fakes::TempDir::new("fiber-shell-listing-stop");
        let ready = Ready::new(dir.path());
        let release_path = dir.path().join("release");
        fakes::fifo(&release_path);
        // Open read-write so releasing never waits for the fixture to open it.
        let mut release = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&release_path)
            .unwrap();
        let ps = fakes::script(
            dir.path(),
            "ps",
            &format!(
                "echo $$ > '{}'\nread -r _ < '{}'\nexit 0",
                ready.path().display(),
                release_path.display(),
            ),
        );
        let work = dir.path().to_path_buf();
        let clock = FakeClock::new();
        let bound = clock.origin() + PS_BOUND;
        let cancel = CancelToken::new();
        let (tx, rx) = mpsc::channel();
        // The blocking call runs on its own thread: a stuck drive fails
        // the test at the deadline instead of hanging it.
        thread::spawn({
            let clock = Arc::clone(&clock);
            let cancel = cancel.clone();
            // The timeout lapses mid-listing but before the listing's own
            // bound, so the listing still answers successfully.
            let timeout = match cancelled {
                true => Duration::from_secs(3600),
                false => Duration::from_secs(1),
            };
            move || {
                let sandbox = crate::shell::sandbox::Settings {
                    enabled: false,
                    allow_write: Vec::new(),
                    deny_read: Vec::new(),
                    fiber_home: std::path::PathBuf::new(),
                };
                let ran = super::execute_inner(
                    std::path::Path::new("/bin/sh"),
                    "true",
                    &work,
                    timeout,
                    clock.as_ref(),
                    &cancel,
                    &Recorder::default(),
                    MovePolicy::Foreground,
                    None,
                    None,
                    None,
                    &sandbox,
                    &work,
                    always_alive,
                    &ps,
                    super::super::sandbox::install,
                )
                .unwrap();
                let _sent = tx.send(ran);
            }
        });
        // The fixture `ps` started: the drive selected the shell-exit move
        // and is inside the listing.
        ready.wait(fakes::Deadline::after(JOB_DEADLINE));
        assert!(
            clock.await_parked(bound, fakes::Deadline::after(JOB_DEADLINE)),
            "the listing did not park at its bound (cancelled={cancelled})"
        );
        // The stop becomes due before the listing is released.
        if cancelled {
            cancel.cancel();
        } else {
            clock.advance_when_parked(
                Some(bound),
                None,
                Duration::from_millis(1500),
                &fakes::Deadline::after(JOB_DEADLINE),
            );
        }
        std::io::Write::write_all(&mut release, b"go\n").unwrap();
        let ran = Deadline::after(JOB_DEADLINE)
            .recv(&rx)
            .expect("the foreground call ended within the deadline");
        let super::Ran::Finished(finished) = ran else {
            panic!(
                "a stop during the listing still finishes in the foreground (cancelled={cancelled})"
            );
        };
        assert_eq!(
            finished.stop,
            Some(if cancelled {
                StopKind::Cancel
            } else {
                StopKind::Timeout
            }),
            "the stop wins over the listing (cancelled={cancelled})"
        );
    }
}

#[test]
fn nl_numbered_file_piped_through_print_only_sed_reads() {
    // #3597 acceptance: `nl -ba` over a file, piped through a
    // print-only `sed`, declares `reads`. `docs/tools.md`, "Shell",
    // "Effects" promises the paths each stage names, not their order,
    // so the two paths are asserted as a set (`docs/testing.md`,
    // "Event streams").
    let effects = super::super::classify::classify(
        "nl -ba crates/main/src/switch.rs | sed -n '1,5p'",
        std::path::Path::new("/work"),
        &[],
    );
    assert_eq!(
        effects.declared.effects,
        vec![contract::shapes::Effect::Reads],
        "nl -ba crates/main/src/switch.rs | sed -n '1,5p'"
    );
    assert!(
        effects.declared.reversible,
        "nl -ba crates/main/src/switch.rs | sed -n '1,5p'"
    );
    let declared = effects.declared.paths.unwrap_or_default();
    let actual: std::collections::BTreeSet<&str> = declared.iter().map(String::as_str).collect();
    assert_eq!(
        actual,
        std::collections::BTreeSet::from(["/work/crates/main/src/switch.rs", "/work"]),
        "nl -ba crates/main/src/switch.rs | sed -n '1,5p'"
    );
}

#[test]
fn cat_with_discard_redirect_or_true_reads() {
    // #3597 acceptance: a read-only `cat` beside `true` declares
    // `reads` with only the earlier part's path.
    let effects = super::super::classify::classify(
        "cat a 2>/dev/null || true",
        std::path::Path::new("/work"),
        &[],
    );
    assert_eq!(
        effects.declared.effects,
        vec![contract::shapes::Effect::Reads],
        "cat a 2>/dev/null || true"
    );
    assert!(effects.declared.reversible, "cat a 2>/dev/null || true");
    assert_eq!(
        effects.declared.paths,
        Some(vec!["/work/a".to_owned()]),
        "cat a 2>/dev/null || true"
    );
}

fn sandbox_settings_for_test(
    _workspace: &std::path::Path,
    name: &str,
) -> crate::shell::sandbox::Settings {
    let home = std::env::var_os("HOME").map_or_else(
        || std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    );
    let fiber_home = home.join(format!("fiber-sandbox-cmd-{name}-{}", std::process::id()));
    crate::shell::sandbox::Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home,
    }
}

#[test]
fn sandbox_enabled_runs_a_confined_echo() {
    let dir = fakes::TempDir::new("fiber-sandbox-cmd-echo");
    let work = dir.path().to_path_buf();
    let sandbox = sandbox_settings_for_test(&work, "echo");
    let ran = fakes::within(
        "a confined echo",
        Deadline::after(JOB_DEADLINE),
        move || {
            super::execute(
                std::path::Path::new("/bin/sh"),
                "echo hi",
                &work,
                Duration::from_secs(60),
                FakeClock::new().as_ref(),
                &CancelToken::new(),
                &Recorder::default(),
                MovePolicy::Stay,
                None,
                None,
                None,
                &sandbox,
                &work,
            )
            .unwrap()
        },
    );
    let super::Ran::Finished(finished) = ran else {
        panic!("a confined echo finishes");
    };
    assert_eq!(exit_code_of(finished.status), 0);
}

/// An enabled sandbox whose policy cannot apply fails the call instead of
/// running it unconfined: the deny sits inside the granted workspace, so
/// the check reports overlap and spawn never happens.
#[cfg(target_os = "linux")]
#[test]
fn sandbox_unavailable_fails_closed_without_running() {
    let dir = fakes::TempDir::new("fiber-sandbox-cmd-unavailable");
    let work = dir.path().to_path_buf();
    let home = std::env::var_os("HOME").map_or_else(
        || std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    );
    let fiber_home = home.join(format!(
        "fiber-sandbox-cmd-unavailable-{}",
        std::process::id()
    ));
    let sandbox = crate::shell::sandbox::Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: vec![work.join("secret")],
        fiber_home,
    };
    let marker = work.join("marker");
    let command = format!("touch {} && echo hi", marker.display());
    let result = super::execute(
        std::path::Path::new("/bin/sh"),
        &command,
        &work,
        Duration::from_secs(60),
        FakeClock::new().as_ref(),
        &CancelToken::new(),
        &Recorder::default(),
        MovePolicy::Stay,
        None,
        None,
        None,
        &sandbox,
        &work,
    );
    let Err(error) = result else {
        panic!("an unavailable sandbox fails the call");
    };
    let apply = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<super::SandboxApply>())
        .expect("an apply failure, not a spawn failure");
    assert!(
        apply.cause.contains("secret"),
        "the cause names the deny: {}",
        apply.cause
    );
    assert!(!marker.is_file(), "no command runs after an apply failure");
}

#[cfg(target_os = "linux")]
#[test]
fn sandbox_setup_failure_aborts_spawn_without_running() {
    let dir = fakes::TempDir::new("fiber-sandbox-cmd-fail");
    let work = dir.path().to_path_buf();
    let marker = work.join("marker");
    let home = std::env::var_os("HOME").map_or_else(
        || std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    );
    let fiber_home = home.join(format!("fiber-sandbox-cmd-fail-{}", std::process::id()));
    let sandbox = crate::shell::sandbox::Settings {
        enabled: true,
        allow_write: vec![std::path::PathBuf::from("/nonexistent-fiber-sandbox-allow")],
        deny_read: Vec::new(),
        fiber_home,
    };
    let command = format!("touch {} && echo hi", marker.display());
    let result = super::execute(
        std::path::Path::new("/bin/sh"),
        &command,
        &work,
        Duration::from_secs(60),
        FakeClock::new().as_ref(),
        &CancelToken::new(),
        &Recorder::default(),
        MovePolicy::Stay,
        None,
        None,
        None,
        &sandbox,
        &work,
    );
    let Err(error) = result else {
        panic!("a setup failure aborts spawn");
    };
    let apply = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<super::SandboxApply>())
        .expect("an apply failure, not a spawn failure");
    assert!(
        apply.cause.contains("nonexistent-fiber-sandbox-allow"),
        "the cause names the unresolvable root: {}",
        apply.cause
    );
    assert!(!marker.is_file(), "no command runs after an apply failure");
}

#[cfg(target_os = "linux")]
#[test]
fn set_cloexec_sets_the_close_on_exec_flag() {
    use std::os::fd::AsFd as _;
    use std::os::unix::net::UnixStream;

    let (stream, _peer) = UnixStream::pair().expect("create a socket pair");
    let fd = stream.as_fd();
    rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::empty()).expect("clear descriptor flags");
    assert!(
        !rustix::io::fcntl_getfd(fd)
            .expect("read descriptor flags")
            .contains(rustix::io::FdFlags::CLOEXEC)
    );

    super::set_cloexec(&stream).expect("set close-on-exec");

    assert!(
        rustix::io::fcntl_getfd(fd)
            .expect("read descriptor flags")
            .contains(rustix::io::FdFlags::CLOEXEC)
    );
}

/// An install failure in the child aborts spawn through the sentinel: the
/// parent reads the raw errno the child wrote and maps it to the sandbox
/// cause, and the command never runs.
#[cfg(target_os = "linux")]
#[test]
fn sandbox_install_failure_aborts_spawn_without_running() {
    fn always_alive(_pgid: u32) -> bool {
        true
    }
    let dir = fakes::TempDir::new("fiber-sandbox-cmd-install-fail");
    let work = dir.path().to_path_buf();
    let ps = fakes::script(dir.path(), "ps", "exit 0");
    let marker = work.join("marker");
    let sandbox = sandbox_settings_for_test(&work, "install-fail");
    let command = format!("touch {} && echo hi", marker.display());
    let result = super::execute_inner(
        std::path::Path::new("/bin/sh"),
        &command,
        &work,
        Duration::from_secs(60),
        FakeClock::new().as_ref(),
        &CancelToken::new(),
        &Recorder::default(),
        MovePolicy::Stay,
        None,
        None,
        None,
        &sandbox,
        &work,
        always_alive,
        &ps,
        |_| Err(libc::EPERM),
    );
    let Err(error) = result else {
        panic!("an install failure aborts spawn");
    };
    let apply = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<super::SandboxApply>())
        .expect("an apply failure, not a spawn failure");
    assert!(
        apply.cause.contains("Operation not permitted"),
        "the cause names the errno: {}",
        apply.cause
    );
    assert!(!marker.is_file(), "no command runs after an apply failure");
}

/// Entry point for the confined socket probe: the sandbox test re-executes
/// this binary under confinement, where real `AF_INET` and Unix socket
/// calls must fail with the sandbox's `EPERM`.
#[test]
#[ignore = "started only by the confined socket probe"]
#[allow(clippy::print_stdout, reason = "the probe parent reads these lines")]
fn confined_socket_probe() {
    let tcp = std::net::TcpStream::connect("127.0.0.1:9").map(|_| ());
    println!("tcp={:?}", tcp.map_err(|error| error.kind()));
    let unix =
        std::os::unix::net::UnixStream::connect("/nonexistent-fiber-sandbox.sock").map(|_| ());
    println!("unix={:?}", unix.map_err(|error| error.kind()));
}

/// Real socket calls under confinement fail with the sandbox's `EPERM`,
/// not with a refused port or a shell syntax the shell never supports: the
/// re-executed helper attempts `connect` over TCP and over a Unix socket
/// through safe standard-library calls, with no `unsafe`.
#[cfg(target_os = "linux")]
#[test]
fn sandbox_denies_sockets_with_abi2_rules_on_this_host() {
    let dir = fakes::TempDir::new("fiber-sandbox-cmd-socket");
    let work = dir.path().to_path_buf();
    let sandbox = sandbox_settings_for_test(&work, "socket");
    let executable = std::env::current_exe().expect("the test executable");
    let quoted = executable.display().to_string().replace('\'', "'\\''");
    let command = format!(
        "'{quoted}' --ignored --exact shell::command::tests::confined_socket_probe --nocapture"
    );
    let ran = fakes::within(
        "a confined socket attempt",
        Deadline::after(JOB_DEADLINE),
        move || {
            super::execute(
                std::path::Path::new("/bin/sh"),
                &command,
                &work,
                Duration::from_secs(60),
                FakeClock::new().as_ref(),
                &CancelToken::new(),
                &Recorder::default(),
                MovePolicy::Stay,
                None,
                None,
                None,
                &sandbox,
                &work,
            )
            .unwrap()
        },
    );
    let super::Ran::Finished(finished) = ran else {
        panic!("a denied socket still finishes the call");
    };
    assert_eq!(
        exit_code_of(finished.status),
        0,
        "the probe itself finishes"
    );
    let super::Kept::Whole(bytes) = finished.output else {
        panic!("a small probe output stays whole");
    };
    let text = String::from_utf8(bytes).expect("the probe output is text");
    assert!(
        text.contains("tcp=Err(PermissionDenied)"),
        "the sandbox denies the TCP connect: {text}"
    );
    assert!(
        text.contains("unix=Err(PermissionDenied)"),
        "the sandbox denies the Unix connect: {text}"
    );
}
