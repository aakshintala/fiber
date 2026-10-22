//! `Reads`: a read on its own thread, a cancel that ends the wait, and the
//! credential commands it lists and kills.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use contract::ErrorCode;
use contract::clock::Clock;

use super::{CancelOnDrop, Reads, deadline_due};
use fakes::Deadline;
use fakes::children::Ready;
use fakes::clock::{FakeClock, Mark, Miss, SystemClock};
use rustix::io::Errno;
use support::clock::Parker;
use support::group::GROUP_POLL;

/// The deadline of each wait in these tests.
const DEADLINE: Duration = Duration::from_secs(10);

/// `sh -c <script>` with stdin and stderr null and stdout piped, as a
/// credential source's built command.
fn sh(script: &str) -> Command {
    let mut command = Command::new("sh");
    command
        .args(["-c", script])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    command
}

/// The groups `reads` lists now.
fn listed(reads: &Reads) -> Vec<u32> {
    reads.lock().groups.clone()
}

/// Waits under [`DEADLINE`] for the wake's generation to reach `target`.
/// The park ends at once on a bump past `seen`; the wall-clock deadline
/// bounds a wake that never comes. Always the process clock: a park on
/// the test's `FakeClock` would corrupt the parked counts these tests
/// assert.
#[track_caller]
fn generation_reaches(wake: &Arc<Parker>, target: u64, what: &str) {
    fakes::within(what, Deadline::after(DEADLINE), {
        let wake = Arc::clone(wake);
        move || loop {
            let seen = wake.generation();
            if seen >= target {
                return;
            }
            wake.park(&SystemClock, None, seen);
        }
    });
}

/// A `Ready` channel of its own: `root/name/ready.fifo`, so tests with
/// more than one line never share a FIFO.
fn ready_in(root: &std::path::Path, name: &str) -> Ready {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    Ready::new(&dir)
}

/// One poll step of a driver loop that is the clock's only mover: waits up
/// to what is left of the drive, capped at one poll pause, for the parked
/// poll instant, then advances past it. No fresh park means the settle
/// answered instead of re-parking, so the caller re-checks its answer
/// channel and the drive; a park at another instant fails the test. A zero
/// pause never reaches the clock: the caller re-checks instead.
#[track_caller]
fn poll_step(
    clock: &FakeClock,
    parked_at: Instant,
    advance: Duration,
    drive: &Deadline,
) -> Option<Mark> {
    let pause = drive.left().min(PARK_PAUSE);
    if pause.is_zero() {
        return None;
    }
    match clock.try_advance_when_parked(Some(parked_at), None, advance, pause) {
        Ok(mark) => Some(mark),
        Err(Miss::NoPark) => None,
        Err(miss @ Miss::Mismatch { .. }) => {
            panic!("{miss} at {:?}; parked: {:?}", clock.now(), clock.parked())
        }
    }
}

#[test]
fn run_returns_the_reads_value() {
    let reads = Reads::default();
    let value = fakes::within("the read", Deadline::after(DEADLINE), move || {
        reads.run("credential read", || 7)
    });
    assert_eq!(value.unwrap(), 7);
}

#[test]
fn cancel_ends_a_blocked_run_with_closing_and_a_late_value_changes_nothing() {
    let reads = Arc::new(Reads::default());
    let (open, gate) = mpsc::channel::<()>();
    let (started, began) = mpsc::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let waiter = {
        let (reads, finished) = (Arc::clone(&reads), Arc::clone(&finished));
        std::thread::spawn(move || {
            reads.run("credential read", move || {
                started.send(()).unwrap();
                gate.recv().unwrap_or(());
                finished.store(true, Ordering::SeqCst);
                9
            })
        })
    };
    Deadline::after(DEADLINE)
        .recv(&began)
        .expect("the read started");
    reads.cancel();
    let rejected = fakes::within("the cancelled run", Deadline::after(DEADLINE), move || {
        waiter.join().unwrap()
    });
    let failure = rejected.expect_err("a cancelled read is no value");
    assert_eq!(failure.code, ErrorCode::Closing);
    assert_eq!(failure.message, "The session is shutting down.");
    // The read finishes after the cancel; nobody receives its value.
    open.send(()).unwrap();
    assert!(reads.lock().waiting.is_empty());
}

#[test]
fn cancel_ends_a_blocked_run_started_before_a_quick_one_with_closing() {
    let reads = Arc::new(Reads::default());
    let (open, gate) = mpsc::channel::<()>();
    let (started, began) = mpsc::channel();
    let waiter = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || {
            reads.run("credential read", move || {
                started.send(()).unwrap();
                gate.recv().unwrap_or(());
                9
            })
        })
    };
    Deadline::after(DEADLINE)
        .recv(&began)
        .expect("the read started");
    // A second run takes the next id; under the `*=` mutant it reuses id 0,
    // replaces A's wake, and its finish removes A's entry, so the cancel
    // below never wakes A.
    let probe = Arc::clone(&reads);
    let quick = fakes::within("the quick run", Deadline::after(DEADLINE), move || {
        probe.run("credential read", || 1)
    });
    assert_eq!(quick.unwrap(), 1);
    reads.cancel();
    let rejected = fakes::within("the cancelled run", Deadline::after(DEADLINE), move || {
        waiter.join().unwrap()
    });
    let failure = rejected.expect_err("a cancelled read is no value");
    assert_eq!(failure.code, ErrorCode::Closing);
    assert_eq!(failure.message, "The session is shutting down.");
    // The read finishes after the cancel; nobody receives its value.
    open.send(()).unwrap();
    assert!(reads.lock().waiting.is_empty());
}

#[test]
fn a_value_answered_after_cancel_is_closing() {
    let reads = Arc::new(Reads::default());
    let (entered, pausing) = mpsc::channel();
    let (open, release) = mpsc::channel::<()>();
    reads.pause_at_answered(entered, release);
    let waiter = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.run("credential read", || 9))
    };
    // The value arrived and the run holds it; the cancel lands between
    // the answer and the run's check on every run.
    Deadline::after(DEADLINE)
        .recv(&pausing)
        .expect("the run answered");
    reads.cancel();
    open.send(()).unwrap();
    let failure = fakes::within("the cancelled run", Deadline::after(DEADLINE), move || {
        waiter.join().unwrap()
    })
    .expect_err("a read answered after cancel is no value");
    assert_eq!(failure.code, ErrorCode::Closing);
    assert_eq!(failure.message, "The session is shutting down.");
    assert!(reads.lock().waiting.is_empty());
}

#[test]
fn run_after_cancel_is_closing_without_running_the_read() {
    let reads = Reads::default();
    reads.cancel();
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let failure = fakes::within("the refused read", Deadline::after(DEADLINE), move || {
        reads.run("credential read", move || {
            flag.store(true, Ordering::SeqCst)
        })
    })
    .expect_err("a cancelled read");
    assert_eq!(failure.code, ErrorCode::Closing);
    assert!(!ran.load(Ordering::SeqCst), "the read never ran");
}

#[test]
fn a_command_leads_its_own_group_and_is_listed_until_the_group_empties() {
    let root = fakes::TempDir::new("fiber-read-group");
    let ready = Ready::new(root.path());
    let go = root.path().join("go");
    let reads = Arc::new(Reads::default());
    let script = format!(
        "echo \"$$ $(ps -o pgid= -p $$ | tr -d ' ')\" > '{}'; while [ ! -e '{}' ]; do sleep 0.01; done; echo key",
        ready.path().display(),
        go.display()
    );
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&go.display().to_string());
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let ids = ready.wait(fakes::Deadline::after(DEADLINE));
    let (pid, pgid) = (ids[0], ids[1]);
    let watchdog = fakes::Watchdog::group(pid);
    assert_eq!(pid, pgid, "the command leads its own group");
    assert_eq!(listed(&reads), vec![pid], "listed while it runs");
    std::fs::write(&go, "").unwrap();
    let output = fakes::within("the command", Deadline::after(DEADLINE), move || {
        running.join().unwrap()
    })
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "key\n");
    assert!(fakes::group_empties(pid, fakes::Deadline::after(DEADLINE)));
    // The next command prunes the empty group.
    let probe = Arc::clone(&reads);
    let pruned = fakes::within(
        "the pruning command",
        Deadline::after(DEADLINE),
        move || probe.command(&mut sh("true")),
    );
    pruned.unwrap();
    assert!(!listed(&reads).contains(&pid), "{:?}", listed(&reads));
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn cancel_kills_a_running_command_and_it_errs() {
    let root = fakes::TempDir::new("fiber-read-cancel");
    let ready = Ready::new(root.path());
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 30", ready.path().display());
    // Before the spawn, on the ready path the command line carries until
    // it execs; the group watchdog below takes over once the pid is known.
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pid: u32 = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pid);
    reads.cancel();
    let error = fakes::within("the killed command", Deadline::after(DEADLINE), move || {
        running.join().unwrap()
    })
    .expect_err("a killed command");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(fakes::group_empties(pid, fakes::Deadline::after(DEADLINE)));
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_descendant_keeps_its_group_listed_and_cancel_kills_it() {
    let root = fakes::TempDir::new("fiber-read-descendant");
    let ready = Ready::new(root.path());
    let reads = Arc::new(Reads::default());
    let script = format!(
        "sleep 30 >/dev/null 2>&1 & echo $$ > '{}'; echo key",
        ready.path().display()
    );
    // Before the spawn: the shell exits fast and leaves `sleep` behind,
    // so only a command-line watchdog covers the spawn itself.
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let first = Arc::clone(&reads);
    let output = fakes::within("the first command", Deadline::after(DEADLINE), move || {
        first.command(&mut sh(&script)).unwrap()
    });
    assert_eq!(String::from_utf8_lossy(&output.stdout), "key\n");
    let group: u32 = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(group);
    assert!(fakes::kill_group(group, "0").unwrap(), "sleep lives"); // A later command prunes only empty groups: `sleep` keeps this one.
    let probe = Arc::clone(&reads);
    fakes::within(
        "the pruning command",
        Deadline::after(DEADLINE),
        move || probe.command(&mut sh("true")).unwrap(),
    );
    assert!(listed(&reads).contains(&group), "{:?}", listed(&reads));
    reads.cancel();
    assert!(
        fakes::group_empties(group, fakes::Deadline::after(DEADLINE)),
        "cancel killed sleep"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_cancel_between_a_read_starting_and_its_spawn_starts_no_command() {
    let root = fakes::TempDir::new("fiber-read-interleave");
    let marker = root.path().join("ran");
    let reads = Arc::new(Reads::default());
    // No command starts here; the watchdog only fires if one ever does.
    let pre = fakes::Watchdog::matching(&marker.display().to_string());
    let (open, gate) = mpsc::channel::<()>();
    let (started, began) = mpsc::channel();
    let job = {
        let reads = Arc::clone(&reads);
        let script = format!("echo x > '{}'; echo key", marker.display());
        std::thread::spawn(move || {
            started.send(()).unwrap();
            gate.recv().unwrap();
            reads.command(&mut sh(&script))
        })
    };
    Deadline::after(DEADLINE)
        .recv(&began)
        .expect("the job started");
    reads.cancel();
    open.send(()).unwrap();
    let error = fakes::within(
        "the refused command",
        Deadline::after(DEADLINE),
        move || job.join().unwrap(),
    )
    .expect_err("a cancelled command");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(!marker.exists(), "the command never started");
    assert!(listed(&reads).is_empty());
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

/// Runs `reads.credential` for `command` on a thread of its own, returning
/// the channel its result arrives on. The test receives through
/// [`Deadline`], never a raw `recv_timeout`.
fn credential_on_thread(
    reads: &Arc<Reads>,
    command: Command,
    timeout: Duration,
    clock: &Arc<FakeClock>,
) -> mpsc::Receiver<std::io::Result<std::process::Output>> {
    let (done, finished) = mpsc::channel();
    let (reads, clock) = (Arc::clone(reads), Arc::clone(clock));
    std::thread::spawn(move || {
        let mut command = command;
        done.send(reads.credential(&mut command, timeout, &*clock))
            .unwrap_or(());
    });
    finished
}

#[test]
fn a_credential_command_still_running_at_its_timeout_is_killed_and_times_out() {
    let root = fakes::TempDir::new("fiber-read-credential-timeout");
    let ready = Ready::new(root.path());
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let deadline = clock.origin() + timeout;
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    // Before the spawn the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let mark = clock.advance_when_parked(
        Some(deadline),
        None,
        timeout.checked_sub(Duration::from_millis(1)).unwrap(),
        &Deadline::after(DEADLINE),
    );
    assert!(
        clock.await_parked_since(&mark, Some(deadline), fakes::Deadline::after(DEADLINE)),
        "the wait parked again after the advance"
    );
    // Armed while the wait is parked: the bump drives one more pass
    // through its checks, which pauses at the checkpoint still short of
    // the deadline. The acknowledgement proves the checks ran with
    // nothing to answer, so the immediate check proves no early answer
    // arrived without reading the real clock.
    let (checked, is_checked) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_checks(checked, freed);
    reads.wake.bump();
    Deadline::after(DEADLINE).recv_or_fail(&is_checked, "the wait's checks before its timeout");
    assert!(
        matches!(finished.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the wait answered before its timeout"
    );
    release.send(()).unwrap();
    assert!(
        clock.await_parked(deadline, fakes::Deadline::after(DEADLINE)),
        "the wait parked at its deadline again"
    );
    assert!(
        fakes::kill_group(pgid, "0").unwrap(),
        "the group died before its timeout"
    );
    // Scripted alive past the first kill, gone on the third probe: the
    // settlement's first pass sends once and parks at its poll, the
    // second sends again, and only SRCH retires the group — so one
    // advance past the deadline can never answer it, on any load. A pause
    // after a real SIGKILL cannot hold the group itself alive, and the
    // scripted sends never touch the real group, so the test kills it for
    // real after proving SRCH retired the scripted group. Armed after the
    // liveness probe above, so the script covers only the settlement's passes.
    reads.script_group(
        pgid,
        vec![Ok(()), Ok(()), Err(Errno::SRCH)],
        vec![Ok(()), Ok(())],
    );
    // Pause after each send to acknowledge that the previous named park
    // has been left before asking the helper to observe the next one.
    let (killed, is_killed) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_settle(killed, freed);
    let (settled, is_settled) = mpsc::channel::<()>();
    let (release_settled, resume_reaper) = mpsc::channel::<()>();
    reads.pause_at_settle_complete(settled, resume_reaper);
    clock.advance_when_parked(
        Some(deadline),
        None,
        Duration::from_millis(1),
        &Deadline::after(DEADLINE),
    );
    Deadline::after(DEADLINE).recv_or_fail(&is_killed, "the settlement's first kill");
    let (killed_again, is_killed_again) = mpsc::channel::<()>();
    let (release_again, freed_again) = mpsc::channel::<()>();
    reads.pause_at_settle(killed_again, freed_again);
    release.send(()).unwrap();

    // Each named advance drives exactly one of the two scripted surviving
    // passes. The pause after the second send proves the first park was
    // left before the second helper observes its exact poll instant.
    let drive = Deadline::after(DEADLINE);
    let poll = clock
        .now()
        .checked_add(GROUP_POLL)
        .expect("the poll is countable");
    clock.advance_when_parked(Some(poll), None, GROUP_POLL, &Deadline::after(DEADLINE));
    drive.recv_or_fail(&is_killed_again, "the settlement's second kill");
    release_again.send(()).unwrap();
    let repoll = clock
        .now()
        .checked_add(GROUP_POLL)
        .expect("the poll is countable");
    clock.advance_when_parked(Some(repoll), None, GROUP_POLL, &Deadline::after(DEADLINE));
    drive.recv_or_fail(&is_settled, "the timeout settlement");

    assert_eq!(reads.scripted_sends(), 2, "one send per pass before SRCH");
    assert!(
        !listed(&reads).contains(&pgid),
        "SRCH retires the group: {:?}",
        listed(&reads)
    );
    // The scripted sends never touched the real group: kill it for real so
    // the reaper answers and the timed-out read can leave its reap loop.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    release_settled.send(()).unwrap();
    let error = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv_or_fail(&finished, "the timed-out read")
        .expect_err("a command still running at its timeout");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the real group is gone"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_leader_that_exits_while_its_descendant_holds_stdout_times_out_and_the_group_dies() {
    let root = fakes::TempDir::new("fiber-read-credential-stdout");
    let ready = Ready::new(root.path());
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let deadline = clock.origin() + timeout;
    let reads = Arc::new(Reads::default());
    // The `sleep` inherits stdout, so the leader's exit is not the run's end.
    let script = format!("echo $$ > '{}'; sleep 600 & exit 0", ready.path().display());
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let mark = clock.advance_when_parked(
        Some(deadline),
        None,
        timeout.checked_sub(Duration::from_millis(1)).unwrap(),
        &Deadline::after(DEADLINE),
    );
    assert!(
        clock.await_parked_since(&mark, Some(deadline), fakes::Deadline::after(DEADLINE)),
        "the wait parked again after the advance"
    );
    // Armed while the wait is parked: the bump drives one more pass
    // through its checks, which pauses at the checkpoint still short of
    // the deadline. The acknowledgement proves the checks ran with
    // nothing to answer, so the immediate check proves no early answer
    // arrived without reading the real clock.
    let (checked, is_checked) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_checks(checked, freed);
    reads.wake.bump();
    Deadline::after(DEADLINE).recv_or_fail(&is_checked, "the wait's checks before its timeout");
    assert!(
        matches!(finished.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the wait answered before its timeout"
    );
    release.send(()).unwrap();
    clock.advance_when_parked(
        Some(deadline),
        None,
        Duration::from_millis(1),
        &Deadline::after(DEADLINE),
    );
    // The timeout re-kills on the poll cadence and parks between passes:
    // drive each confirmed poll park, as the re-kill test does. The advance
    // above wakes only one pass, whose probe can still see a
    // reaped-but-not-yet-reaped zombie alive under load; without further
    // advances nothing ever bumps again and the wait parks forever. Each
    // advance follows a confirmed park, and each park waits only a poll
    // pause of the drive, so an already-retired group still ends the loop
    // on its answer instead of stalling on a park that never comes.
    let drive = Deadline::after(DEADLINE);
    let mut answer: Option<std::io::Result<std::process::Output>> = None;
    while answer.is_none() {
        match finished.try_recv() {
            Ok(got) => {
                answer = Some(got);
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        if drive.left().is_zero() {
            break;
        }
        let poll = clock
            .now()
            .checked_add(GROUP_POLL)
            .expect("the poll is countable");
        let _stepped = poll_step(&clock, poll, GROUP_POLL, &drive);
    }
    let error = match answer {
        Some(got) => got.expect_err("a command holding stdout past its timeout"),
        None => Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv_or_fail(&finished, "the timed-out read")
            .expect_err("a command holding stdout past its timeout"),
    };
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the timeout killed the group"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

/// The timeout kill re-kills a group that survives the first SIGKILL until
/// no process is left in it: the script answers the first two probes alive
/// — a fork landing inside the kernel kill walk — and SRCH only after the
/// second send, so the wait must deliver twice and retire the group on
/// SRCH. An accepted send never retires the group, only SRCH does.
#[test]
fn timeout_rekills_a_group_that_survives_its_first_kill() {
    let root = fakes::TempDir::new("fiber-read-timeout-rekill");
    let ready = Ready::new(root.path());
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let deadline = clock.origin() + timeout;
    let reads = Arc::new(Reads::default());
    // Both wakes before the timeout must land before the poll cadence
    // below: the spawn's listing and the reaper's answer. Captured before
    // the spawn, so both are still to come.
    let sent = reads.wake.generation();
    // The `sleep` inherits stdout, so the leader's exit is not the run's
    // end: the wait parks at its deadline with the leader already gone,
    // and the timeout below needs no reap wait, only the group's kills.
    let script = format!("echo $$ > '{}'; sleep 600 & exit 0", ready.path().display());
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, fakes::Deadline::after(DEADLINE)),
        "the wait parked at its deadline"
    );
    // The reaper's answer bump can otherwise still be in flight here:
    // landing after the first poll park below, it would drive an extra
    // settlement pass with `now` still at the deadline, spending a probe
    // early so the second poll park never comes. Both wakes before the
    // timeout — the spawn's listing and the reaper's answer — land first,
    // so each advance below drives exactly one pass. The stdout reader
    // never answers while `sleep` holds the pipe, nothing is cancelled,
    // and the clock has not moved, so these are the only two.
    generation_reaches(&reads.wake, sent + 2, "the leader's wake");
    assert_eq!(
        reads.wake.generation(),
        sent + 2,
        "the spawn's listing and the reaper's answer are the only wakes before the timeout"
    );
    // Alive past the first kill, gone after the second.
    reads.script_group(
        pgid,
        vec![Ok(()), Ok(()), Err(Errno::SRCH)],
        vec![Ok(()), Ok(())],
    );
    // Forced late wake (repro for #3068): the settlement holds after
    // its first kill, so one bump of the wait's own wake lands inside
    // the generation-to-park window, exactly as the stdout reader's
    // late answer does when the `sleep` holding the pipe goes away —
    // the reader answers, bumps, and the first poll park ends at once.
    // The released pass then parks on a stale generation and returns
    // at once, so the next pass spends the second probe with `now`
    // still at the deadline; the poll advance below retires the group
    // via SRCH and the repoll park never comes.
    let (killed, is_killed) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_settle(killed, freed);
    clock.advance_when_parked(Some(deadline), None, timeout, &Deadline::after(DEADLINE));
    Deadline::after(DEADLINE).recv_or_fail(&is_killed, "the settlement's first kill");
    reads.wake.bump();
    release.send(()).unwrap();
    // The timeout re-kills on the poll cadence: each advance follows a
    // confirmed park, so the first two parks each earn one more send and
    // the third observation retires the group.
    let poll = deadline + GROUP_POLL;
    clock.advance_when_parked(Some(poll), None, GROUP_POLL, &Deadline::after(DEADLINE));
    let repoll = poll + GROUP_POLL;
    clock.advance_when_parked(Some(repoll), None, GROUP_POLL, &Deadline::after(DEADLINE));
    let error = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the timed-out read")
        .expect_err("a command still running at its timeout");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(reads.scripted_sends(), 2, "one send per pass before SRCH");
    assert!(
        !listed(&reads).contains(&pgid),
        "SRCH retires the group: {:?}",
        listed(&reads)
    );
    // The scripted sends never touched the real group: kill it for real
    // and reap it on every path.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the real group is gone"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

/// The timeout gives up re-killing at its bound: one nanosecond short it
/// is still parked with no answer, at the bound it answers `TimedOut` with
/// the group still listed for later settlement. Pins the boundary both
/// sides.
#[test]
fn timeout_leaves_a_stubborn_group_listed_at_its_bound() {
    let root = fakes::TempDir::new("fiber-read-timeout-bound");
    let ready = Ready::new(root.path());
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let deadline = clock.origin() + timeout;
    let end = deadline + doors::SHUTDOWN_BOUND;
    let reads = Arc::new(Reads::default());
    // As in the re-kill test above: the leader's exit is not the run's
    // end, so the timeout needs no reap wait, only the group's kills.
    let script = format!("echo $$ > '{}'; sleep 600 & exit 0", ready.path().display());
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, fakes::Deadline::after(DEADLINE)),
        "the wait parked at its deadline"
    );
    // Alive past every probe within the bound: 600 scripted sends at a
    // 10 ms cadence outlast the 5 s bound, as in the settle retry test.
    reads.script_group(
        pgid,
        (0..600).map(|_| Ok(())).collect(),
        (0..600).map(|_| Ok(())).collect(),
    );
    clock.advance_when_parked(Some(deadline), None, timeout, &Deadline::after(DEADLINE));
    assert!(
        clock.await_parked(deadline + GROUP_POLL, fakes::Deadline::after(DEADLINE)),
        "the timeout parked at its first poll"
    );
    // Jump to one nanosecond short of the end: every pass between still
    // found the scripted group alive and sent again.
    // Crosses only poll pauses to the shutdown end; the run is proven parked at its poll (ruling 6260728504).
    let mark = clock.advance_marked(
        doors::SHUTDOWN_BOUND
            .checked_sub(Duration::from_nanos(1))
            .expect("the bound holds a nanosecond"),
    );
    assert!(
        clock.await_parked_since(&mark, Some(end), fakes::Deadline::after(DEADLINE)),
        "the timeout parked again short of its bound"
    );
    assert!(
        matches!(finished.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "timeout answered short of its bound"
    );
    clock.advance_when_parked(
        Some(end),
        None,
        Duration::from_nanos(1),
        &Deadline::after(DEADLINE),
    );
    let error = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the timed-out read")
        .expect_err("a command still running at its timeout");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        listed(&reads).contains(&pgid),
        "the bound leaves the group listed: {:?}",
        listed(&reads)
    );
    let sends = reads.scripted_sends();
    assert!(
        sends >= 2,
        "the timeout retried within the bound, not once: {sends}"
    );
    // The scripted sends never touched the real group: kill it for real
    // and reap it on every path.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the real group is gone"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

/// The timeout shares one absolute end for settlement and the reap: the
/// leader below never exits, so an unbounded reap would hang past the
/// end. One nanosecond short of the end the wait is still parked with no
/// answer; at the end it answers `TimedOut` with the group still listed
/// for later settlement. Pins the boundary both sides.
#[test]
fn timeout_with_a_live_leader_answers_at_the_bound_without_the_reap() {
    let root = fakes::TempDir::new("fiber-read-timeout-live-leader");
    let ready = Ready::new(root.path());
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let deadline = clock.origin() + timeout;
    let end = deadline + doors::SHUTDOWN_BOUND;
    let reads = Arc::new(Reads::default());
    // The leader never exits: `sleep` holds both the group and stdout, so
    // neither the exit nor the output ever arrives.
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, fakes::Deadline::after(DEADLINE)),
        "the wait parked at its deadline"
    );
    // Alive past every probe within the bound: 600 scripted sends at a
    // 10 ms cadence outlast the 5 s bound, as in the settle retry test.
    reads.script_group(
        pgid,
        (0..600).map(|_| Ok(())).collect(),
        (0..600).map(|_| Ok(())).collect(),
    );
    clock.advance_when_parked(Some(deadline), None, timeout, &Deadline::after(DEADLINE));
    assert!(
        clock.await_parked(deadline + GROUP_POLL, fakes::Deadline::after(DEADLINE)),
        "the timeout parked at its first poll"
    );
    // Jump to one nanosecond short of the end: still parked, no answer.
    // Crosses only poll pauses to the shutdown end; the run is proven parked at its poll (ruling 6260728504).
    let mark = clock.advance_marked(
        doors::SHUTDOWN_BOUND
            .checked_sub(Duration::from_nanos(1))
            .expect("the bound holds a nanosecond"),
    );
    assert!(
        clock.await_parked_since(&mark, Some(end), fakes::Deadline::after(DEADLINE)),
        "the timeout parked again short of its bound"
    );
    assert!(
        matches!(finished.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "timeout answered short of its bound"
    );
    clock.advance_when_parked(
        Some(end),
        None,
        Duration::from_nanos(1),
        &Deadline::after(DEADLINE),
    );
    let error = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the timed-out read")
        .expect_err("a live leader still running at its timeout");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        listed(&reads).contains(&pgid),
        "the bound leaves the group listed: {:?}",
        listed(&reads)
    );
    let sends = reads.scripted_sends();
    assert!(
        sends >= 2,
        "the timeout retried within the bound, not once: {sends}"
    );
    // The scripted sends never touched the real group: kill it for real
    // and reap it on every path.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the real group is gone"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

/// A cancel that lands while the timeout settles its group answers
/// `Interrupted`, never `TimedOut`: the cancel bumps the parked
/// settlement pass, which observes it at once. The settlement below is
/// parked at its first poll when the cancel lands, on every run.
#[test]
fn cancel_during_settlement_answers_interrupted() {
    let root = fakes::TempDir::new("fiber-read-cancel-during-settlement");
    let ready = Ready::new(root.path());
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let deadline = clock.origin() + timeout;
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, fakes::Deadline::after(DEADLINE)),
        "the wait parked at its deadline"
    );
    // Scripted alive through the controlled poll: a real SIGKILL and the
    // reaper wake would otherwise complete the timeout before the cancel
    // below, so settlement stays pending until the cancel ends it. The
    // scripted sends never touch the real group; the test kills it for
    // real after the answer.
    reads.script_group(
        pgid,
        (0..10).map(|_| Ok(())).collect(),
        (0..10).map(|_| Ok(())).collect(),
    );
    clock.advance_when_parked(Some(deadline), None, timeout, &Deadline::after(DEADLINE));
    // The settlement re-kills on the poll cadence: parked at its first
    // poll, the wait can only leave through the cancel below.
    assert!(
        clock.await_parked(deadline + GROUP_POLL, fakes::Deadline::after(DEADLINE)),
        "the timeout parked at its first poll"
    );
    reads.cancel();
    let error = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the cancelled read")
        .expect_err("a read cancelled while timing out");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    // The scripted sends never touched the real group: kill it for real
    // and reap it on every path.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the real group is gone"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_quick_command_answers_without_the_clock_moving() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let mut printf = Command::new("printf");
    printf
        .arg("key")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    let guard = fakes::Watchdog::matching("printf key");
    let finished = credential_on_thread(&reads, printf, Duration::from_secs(30), &clock);
    let output = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the quick command")
        .unwrap();
    assert_eq!(output.stdout, b"key");
    assert_eq!(
        reads.lock().spawning,
        0,
        "the successful spawn dropped the count"
    );
    guard.stand_down(fakes::Deadline::after(DEADLINE));
}

/// A finish that lands between the wait's checks and its park is not
/// missed: the checkpoint holds the wait past its checks, so the helper's
/// finish lands there on every run. The generation captured before the
/// checks already holds those wakes, so the park ends at once; captured
/// after the checks, the park would miss them and hang on the frozen
/// clock until this test's deadline.
#[test]
fn a_completion_between_the_checks_and_the_park_is_not_missed() {
    let root = fakes::TempDir::new("fiber-read-credential-interleave");
    let ready = Ready::new(root.path());
    let go = root.path().join("go");
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!(
        "echo $$ > '{}'; while [ ! -e '{}' ]; do sleep 0.01; done; printf key",
        ready.path().display(),
        go.display()
    );
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    // Armed before the wait starts, so its first pass already pauses: the
    // acknowledgement below proves its checks ran with nothing to read,
    // since the helper cannot finish before `go` exists.
    let (checked, is_checked) = mpsc::channel();
    let (release, freed) = mpsc::channel();
    reads.pause_at_checks(checked, freed);
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    Deadline::after(DEADLINE).recv_or_fail(&is_checked, "the wait's checks");
    // Both wakes land while the wait is held past its checks: the park
    // below already holds them in an earlier generation, so it must
    // return at once. Each thread bumps after its send, so two bumps mean
    // both answers are in.
    let seen = reads.wake.generation();
    std::fs::write(&go, "").unwrap();
    generation_reaches(&reads.wake, seen + 2, "the two wakes");
    release.send(()).unwrap();
    let output = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the interleaved read")
        .unwrap();
    assert_eq!(output.stdout, b"key");
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the group reaped its leader"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

/// A key finished after its timeout is refused: the state lock holds the
/// wait between its checks while the helper finishes and the clock moves
/// past the deadline, so the wait observes both done past its end. The
/// late key is never accepted; the group is killed and the read times out.
#[test]
fn a_key_finished_after_its_timeout_is_refused() {
    let root = fakes::TempDir::new("fiber-read-credential-late-key");
    let ready = Ready::new(root.path());
    let go = root.path().join("go");
    let timeout = Duration::from_secs(30);
    let clock = FakeClock::new();
    let deadline = clock.origin() + timeout;
    let reads = Arc::new(Reads::default());
    let script = format!(
        "echo $$ > '{}'; while [ ! -e '{}' ]; do sleep 0.01; done; printf key",
        ready.path().display(),
        go.display()
    );
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), timeout, &clock);
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, fakes::Deadline::after(DEADLINE)),
        "the wait parked at its deadline"
    );
    // The helper finishes while the wait is held here, then the clock
    // moves past the deadline: the wait's next checks see a finish past
    // its end, which times out instead of answering the late key.
    let seen = reads.wake.generation();
    {
        let _held = reads.lock();
        std::fs::write(&go, "").unwrap();
        generation_reaches(&reads.wake, seen + 2, "the two wakes");
        // No code under test waits on the clock here; the wait is held on the state lock between its checks (ruling 6179406086).
        clock.advance(timeout + Duration::from_secs(1));
    }
    let error = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the late read")
        .expect_err("a key finished after its timeout");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the timeout killed the group"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

/// A helper that prints its key, closes stdout, then finishes cleanup
/// answers without the clock moving: the stdout reader's wake lands while
/// the leader still runs, and only the leader's own wake ends the wait.
/// `go` releases the print and `out` releases the exit, so stdout closes
/// strictly before the leader exits on every run; the second checkpoint
/// proves the wait consumed stdout with the exit still missing, since
/// `out` does not exist yet.
#[test]
fn a_leader_that_closes_stdout_before_it_exits_answers_without_the_clock_moving() {
    let root = fakes::TempDir::new("fiber-read-credential-cleanup");
    let ready = Ready::new(root.path());
    let go = root.path().join("go");
    let out = root.path().join("out");
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!(
        "echo $$ > '{}'; while [ ! -e '{}' ]; do sleep 0.01; done; printf key; exec >&-; while [ ! -e '{}' ]; do sleep 0.01; done; exit 0",
        ready.path().display(),
        go.display(),
        out.display()
    );
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    // Armed before the wait starts, so its first pass already pauses: the
    // acknowledgement proves its checks ran with nothing to read, since
    // the helper cannot print before `go` exists.
    let (checked_one, is_checked_one) = mpsc::channel::<()>();
    let (release_one, freed_one) = mpsc::channel::<()>();
    reads.pause_at_checks(checked_one, freed_one);
    let finished = credential_on_thread(&reads, sh(&script), Duration::from_secs(30), &clock);
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let wake = Arc::clone(&reads.wake);
    let sent = wake.generation();
    Deadline::after(DEADLINE).recv_or_fail(&is_checked_one, "the wait's first checks");
    // Armed again while the wait is still held, so its next pass pauses
    // too.
    let (checked_two, is_checked_two) = mpsc::channel::<()>();
    let (release_two, freed_two) = mpsc::channel::<()>();
    reads.pause_at_checks(checked_two, freed_two);
    release_one.send(()).unwrap();
    std::fs::write(&go, "").unwrap();
    Deadline::after(DEADLINE).recv_or_fail(&is_checked_two, "the wait's second checks");
    // Only the print's bump could have woken the wait, so the second
    // acknowledgement proves stdout is consumed with the leader still
    // running. Only now may the leader exit: its own bump lands while
    // the wait is still held, which is what ends the wait. Each thread
    // bumps after its send, so two bumps mean both answers are in.
    std::fs::write(&out, "").unwrap();
    generation_reaches(&wake, sent + 2, "the leader's wake");
    release_two.send(()).unwrap();
    let output = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the cleaning-up leader")
        .unwrap();
    assert_eq!(output.stdout, b"key");
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the group reaped its leader"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn cancel_ends_a_credential_wait_with_interrupted_and_kills_the_group() {
    let root = fakes::TempDir::new("fiber-read-credential-cancel");
    let ready = Ready::new(root.path());
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), Duration::from_secs(30), &clock);
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    reads.cancel();
    let error = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the cancelled read")
        .expect_err("a cancelled read");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the cancel killed the group"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_timeout_past_what_the_clock_can_count_starts_nothing() {
    let root = fakes::TempDir::new("fiber-read-credential-overflow");
    let marker = root.path().join("ran");
    let clock = FakeClock::new();
    let reads = Reads::default();
    // The watchdog only fires if a mutant spawns the command below; the
    // script exits at once, so a spawn still ends the test with a failure.
    let guard = fakes::Watchdog::matching(&marker.display().to_string());
    let mut command = sh(&format!("echo x > '{}'", marker.display()));
    let error = reads
        .credential(&mut command, Duration::MAX, &*clock)
        .expect_err("a timeout past what the clock can count");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        error
            .to_string()
            .contains("its timeout is longer than this system's clock can count"),
        "{error}"
    );
    assert!(!marker.exists(), "the command never started");
    assert!(listed(&reads).is_empty());
    guard.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn deadline_due_is_true_from_the_deadline_on() {
    let clock = FakeClock::new();
    let deadline = clock.origin() + Duration::from_secs(1);
    for (now, due) in [
        (
            deadline.checked_sub(Duration::from_nanos(1)).unwrap(),
            false,
        ),
        (deadline, true),
        (deadline + Duration::from_nanos(1), true),
    ] {
        assert_eq!(deadline_due(now, deadline), due, "{now:?}");
    }
}

#[test]
fn dropping_cancel_on_drop_kills_what_a_finished_command_left() {
    let root = fakes::TempDir::new("fiber-read-cancel-on-drop");
    let ready = Ready::new(root.path());
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!(
        "sleep 600 >/dev/null 2>&1 & echo $$ > '{}'; printf key",
        ready.path().display()
    );
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    // On a thread under a wall-clock deadline: a missed wake fails here,
    // never hangs the test binary.
    let finished = credential_on_thread(&reads, sh(&script), Duration::from_secs(30), &clock);
    let output = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the finished read")
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "key");
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(fakes::kill_group(pgid, "0").unwrap(), "sleep lives");
    drop(CancelOnDrop(Arc::clone(&reads)));
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "dropping the guard killed sleep"
    );
    let error = reads
        .credential(&mut sh("true"), Duration::from_secs(30), &*clock)
        .expect_err("a read after the drop");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

/// A cancel lands while a spawn is blocked before it spawns: the cancel
/// returns at once and kills every listed group, and the late spawn lists
/// its own group for settlement, kills it and is `Interrupted`. Before the
/// fix the spawn holds the state lock across the spawn, so the cancel
/// blocks behind it and the deadline below fails the test.
#[test]
fn cancel_passes_a_blocked_spawn_and_the_late_child_is_killed() {
    let root = fakes::TempDir::new("fiber-read-blocked-spawn");
    let ready_a = ready_in(root.path(), "ready-a");
    let marker = format!("fiber-read-late-{}", root.path().display());
    let reads = Arc::new(Reads::default());
    let script_a = format!("echo $$ > '{}'; exec sleep 30", ready_a.path().display());
    let pre_a = fakes::Watchdog::matching(&ready_a.path().display().to_string());
    // Group A pauses after it lists its group, so the assertion below
    // sees it listed on every run: the ready file alone does not prove
    // the spawn listed it yet.
    let (listed_tx, listed_rx) = mpsc::channel::<()>();
    let (listed_release_tx, listed_release_rx) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_tx, listed_release_rx);
    let running_a = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script_a)))
    };
    let pid_a: u32 = ready_a.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog_a = fakes::Watchdog::group(pid_a);
    Deadline::after(DEADLINE)
        .recv(&listed_rx)
        .expect("group A listed its group");
    assert_eq!(listed(&reads), vec![pid_a], "group A listed while it runs");
    listed_release_tx.send(()).unwrap();
    // The next spawn blocks before it spawns; the marker stays in the
    // late shell's command line, so a leaked late child still matches.
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered_tx, release_rx);
    let late = fakes::Watchdog::matching(&marker);
    let script_b = format!("sleep 30 # {marker}");
    let running_b = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script_b)))
    };
    Deadline::after(DEADLINE)
        .recv(&entered_rx)
        .expect("the spawn reached its pause");
    let (cancel_done_tx, cancel_done_rx) = mpsc::channel::<()>();
    {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || {
            reads.cancel();
            match cancel_done_tx.send(()) {
                Ok(()) | Err(_) => {}
            }
        });
    }
    Deadline::after(DEADLINE)
        .recv(&cancel_done_rx)
        .expect("the cancel returns while the spawn is blocked");
    assert!(
        fakes::group_empties(pid_a, fakes::Deadline::after(DEADLINE)),
        "the cancel killed group A"
    );
    let late_a = fakes::within("the killed command", Deadline::after(DEADLINE), move || {
        running_a.join().unwrap()
    })
    .expect_err("a killed command");
    match release_tx.send(()) {
        Ok(()) | Err(_) => {}
    }
    let error = fakes::within("the late command", Deadline::after(DEADLINE), move || {
        running_b.join().unwrap()
    })
    .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    // A listed group is retired once its probe says SRCH: A's join above
    // reaped its leader before the release, so the late spawn's prune
    // retires A and lists only the late group. The leader's reap never
    // retires surviving descendants; settlement does, once the probe
    // says SRCH.
    assert_eq!(
        listed(&reads).len(),
        1,
        "only the late group is listed: {:?}",
        listed(&reads)
    );
    assert!(
        !listed(&reads).contains(&pid_a),
        "A retired once empty: {:?}",
        listed(&reads)
    );
    assert!(
        fakes::matching_exits(&marker, fakes::Deadline::after(DEADLINE)),
        "the late group is gone"
    );
    // Settlement retires the reaped late group: nothing stays listed.
    let clock = FakeClock::new();
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3)).recv_or_fail(&settled, "the settle"),
        "the settle retires the late group"
    );
    assert!(
        listed(&reads).is_empty(),
        "settlement retires the late group: {:?}",
        listed(&reads)
    );
    assert_eq!(late_a.kind(), std::io::ErrorKind::Interrupted);
    watchdog_a.stand_down(fakes::Deadline::after(DEADLINE));
    pre_a.stand_down(fakes::Deadline::after(DEADLINE));
    late.stand_down(fakes::Deadline::after(DEADLINE));
}

/// A spawn prunes without signalling: only SRCH retires a listed group,
/// so a probe that says PERM keeps it and a later probe that says SRCH
/// retires it. The two following spawns each prune once, consuming one
/// scripted probe for A in order.
#[test]
fn a_spawn_prune_keeps_a_group_whose_probe_says_perm_and_retires_it_on_srch() {
    let root = fakes::TempDir::new("fiber-read-prune-perm-srch");
    let ready_a = ready_in(root.path(), "ready-a");
    let ready_b = ready_in(root.path(), "ready-b");
    let ready_c = ready_in(root.path(), "ready-c");
    let reads = Arc::new(Reads::default());
    let pre_a = fakes::Watchdog::matching(&ready_a.path().display().to_string());
    let pre_b = fakes::Watchdog::matching(&ready_b.path().display().to_string());
    let pre_c = fakes::Watchdog::matching(&ready_c.path().display().to_string());
    // Group A pauses after it lists its group, so the script below arms
    // only after A is really listed: the ready file alone does not prove
    // the spawn listed it yet.
    let (listed_tx, listed_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_tx, release_rx);
    let running_a = {
        let reads = Arc::clone(&reads);
        let script = format!("echo $$ > '{}'; exec sleep 600", ready_a.path().display());
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pid_a: u32 = ready_a.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog_a = fakes::Watchdog::group(pid_a);
    Deadline::after(DEADLINE)
        .recv(&listed_rx)
        .expect("group A listed its group");
    assert_eq!(listed(&reads), vec![pid_a], "group A listed while it runs");
    release_tx.send(()).unwrap();
    // The late spawns' prunes read only A's scripted probes; every other
    // id falls through to the real syscalls after the same validation.
    reads.script_group(pid_a, vec![Err(Errno::PERM), Err(Errno::SRCH)], vec![]);
    let spawn_listed = |reads: &Arc<Reads>, ready: &Ready| {
        let (listed_tx, listed_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        reads.pause_at_listed(listed_tx, release_rx);
        let running = {
            let reads = Arc::clone(reads);
            let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
            std::thread::spawn(move || reads.command(&mut sh(&script)))
        };
        let pid: u32 = ready.wait(fakes::Deadline::after(DEADLINE))[0];
        Deadline::after(DEADLINE)
            .recv(&listed_rx)
            .expect("the group listed its group");
        release_tx.send(()).unwrap();
        (running, pid)
    };
    let (running_b, pid_b) = spawn_listed(&reads, &ready_b);
    let watchdog_b = fakes::Watchdog::group(pid_b);
    // Only SRCH retires a listed group: PERM keeps A pending through the
    // next spawn's prune, so both groups stay listed.
    assert!(
        listed(&reads).contains(&pid_a),
        "PERM keeps the group listed: {:?}",
        listed(&reads)
    );
    assert!(
        listed(&reads).contains(&pid_b),
        "the new group is listed: {:?}",
        listed(&reads)
    );
    assert_eq!(
        listed(&reads).len(),
        2,
        "PERM retires nothing: {:?}",
        listed(&reads)
    );
    let (running_c, pid_c) = spawn_listed(&reads, &ready_c);
    let watchdog_c = fakes::Watchdog::group(pid_c);
    // SRCH means no member is left, so the prune retires A while the live
    // groups stay listed.
    assert!(
        !listed(&reads).contains(&pid_a),
        "SRCH retires the group: {:?}",
        listed(&reads)
    );
    assert!(
        listed(&reads).contains(&pid_b),
        "the live group stays listed: {:?}",
        listed(&reads)
    );
    assert!(
        listed(&reads).contains(&pid_c),
        "the new group is listed: {:?}",
        listed(&reads)
    );
    assert_eq!(
        listed(&reads).len(),
        2,
        "SRCH retires only the empty group: {:?}",
        listed(&reads)
    );
    // The scripted probes never touched the real groups: kill each for
    // real and reap every command thread on every path. Each fixture is
    // a single `exec sleep`, so the joins reap each leader with no
    // descendants left behind. The joins already prove the groups are
    // gone; no real-clock `group_empties` wait here.
    for pid in [pid_a, pid_b, pid_c] {
        match fakes::kill_group(pid, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
    for (what, running) in [
        ("the first command", running_a),
        ("the second command", running_b),
        ("the third command", running_c),
    ] {
        fakes::within(what, Deadline::after(DEADLINE), move || {
            running.join().unwrap()
        })
        .unwrap();
    }
    watchdog_a.stand_down(fakes::Deadline::after(DEADLINE));
    watchdog_b.stand_down(fakes::Deadline::after(DEADLINE));
    watchdog_c.stand_down(fakes::Deadline::after(DEADLINE));
    pre_a.stand_down(fakes::Deadline::after(DEADLINE));
    pre_b.stand_down(fakes::Deadline::after(DEADLINE));
    pre_c.stand_down(fakes::Deadline::after(DEADLINE));
}

/// Drives a settle to completion on the fake clock and returns its
/// answer, through a controlled interleaving that shows both halves
/// while the reap stays held. The blind-probe phase keeps every hold
/// and runs fifty poll steps, proving it spends budget with zero
/// progress: the clock moves fifty poll advances while the held leader
/// stays unreaped, so the
/// settle still lists the real group. The listing is read from the
/// settle itself: a killed-but-unreaped leader probes `EPERM` on some
/// platforms, which keeps the group pending in the settle (only `SRCH`
/// retires) while reading empty through the raw probe. The gated phase
/// then proves the fixed drive holds its budget while the group is
/// held: a few iterations still list the group, and an await for a park
/// past the current one must time out — without a clock advance the
/// settle never leaves its park (its kill path bumps nothing), so no
/// re-park arrives. Only then are the release senders dropped and the
/// settle driven to completion: past the release zero-move wakes let
/// the settle observe it without spending budget, and every budgeted
/// advance needs both a confirmed re-park and the settle no longer
/// listing `real`, so a slow real death parks the settle without
/// spending the bound. Completion and every acknowledgement share the
/// drive's one deadline: each acknowledgement waits only a poll pause
/// of it. Fails the test when the drive's deadline runs out first.
#[track_caller]
fn drive_settle_to_completion(
    clock: &Arc<FakeClock>,
    reads: &Arc<Reads>,
    finished: &mpsc::Receiver<bool>,
    drive: &Deadline,
    real: u32,
    releases: Vec<mpsc::Sender<()>>,
) -> bool {
    let start = clock.origin();
    let poll = start + GROUP_POLL;
    assert!(
        clock.await_parked(poll, *drive),
        "the settle parked at its poll"
    );
    // Blind-probe phase, holds maintained: fifty poll steps. Each
    // confirmed park is the held path; once the settle answers early
    // instead (its group died unheld) the loop ends, so the still-listed
    // assert below names the failure, not these lines.
    let mut answered = false;
    let mut mark: Option<Mark> = None;
    for _ in 0..50 {
        let parked_at = clock
            .now()
            .checked_add(GROUP_POLL)
            .expect("the poll is countable");
        match poll_step(clock, parked_at, GROUP_POLL, drive) {
            Some(stepped) => mark = Some(stepped),
            None => {
                answered = finished.try_recv().is_ok();
                assert!(answered, "the settle answered when it did not re-park");
                break;
            }
        }
    }
    let mark = mark.expect("the blind probe stepped at least once");
    let reparked =
        clock.await_any_parked_since(&mark, fakes::Deadline::after(drive.left().min(PARK_PAUSE)));
    if !reparked && !answered {
        answered = finished.try_recv().is_ok();
    }
    assert!(
        reparked || answered,
        "the settle parked again after the last probe advance"
    );
    assert_eq!(
        clock.now(),
        start + Duration::from_millis(500),
        "the blind probe spent fifty poll steps"
    );
    assert!(
        listed(reads).contains(&real),
        "the held group is still listed after the blind probe: {:?}",
        listed(reads)
    );
    // Retention phase, holds maintained: the fixed drive spends nothing
    // while the group is held. Four iterations still list the group,
    // then an await for a park past the current one must time out:
    // without a clock advance the settle never leaves its park (its
    // kill path bumps nothing), so no re-park arrives. The mark below
    // is taken without waking the settle, so the await is genuine.
    for _ in 0..4 {
        assert!(
            listed(reads).contains(&real),
            "the held group stays listed while frozen: {:?}",
            listed(reads)
        );
    }
    let frozen = clock
        .mark_parked(clock.now() + GROUP_POLL, *drive)
        .expect("the settle parked at its poll");
    assert!(
        !clock.await_any_parked_since(&frozen, fakes::Deadline::after(PARK_PAUSE)),
        "the settle re-parked without a clock advance"
    );
    assert_eq!(
        clock.now(),
        start + Duration::from_millis(500),
        "the retention spent no budget"
    );
    assert!(
        listed(reads).contains(&real),
        "the held group is still listed before release: {:?}",
        listed(reads)
    );
    assert!(
        matches!(finished.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the settle answered while its group was held"
    );
    for release in releases {
        drop(release);
    }
    // Release observation: the settle learns of the release only on a
    // wake (its kill path bumps nothing), so each pass below wakes one
    // without moving the clock — the park the mark names proves the
    // settle waits on it first. The loop ends once the settle retires
    // the reaped group (and answers in the same pass) or answers
    // outright; the clock still sits at the blind-probe mark after.
    let mut answer = finished.try_recv().ok();
    while answer.is_none() && listed(reads).contains(&real) {
        if drive.left().is_zero() {
            break;
        }
        let parked_at = clock.now() + GROUP_POLL;
        match poll_step(clock, parked_at, Duration::ZERO, drive) {
            Some(_) => {}
            None => {
                // Never parked again: the pass answered instead.
                answer = finished.try_recv().ok();
                break;
            }
        }
        answer = finished.try_recv().ok();
    }
    assert_eq!(
        clock.now(),
        start + Duration::from_millis(500),
        "the release observation spent no budget"
    );
    // Gated tail: every budgeted advance needs both a confirmed re-park
    // and the settle no longer listing `real`, so a slow real death
    // parks the settle without spending the bound. The marks below are
    // taken without waking the settle, so a hold iteration's
    // `PARK_PAUSE`-bounded await genuinely times out while frozen.
    // Usually skipped: the observation above already holds the answer.
    let answer = match answer {
        Some(answer) => answer,
        None => loop {
            if let Ok(answer) = finished.try_recv() {
                break answer;
            }
            if drive.left().is_zero() {
                break drive.recv(finished).expect("the settle");
            }
            let parked_at = clock.now() + GROUP_POLL;
            match clock.mark_parked(
                parked_at,
                fakes::Deadline::after(drive.left().min(PARK_PAUSE)),
            ) {
                // No park again: the settle answered instead; take it
                // under the drive.
                None => break drive.recv(finished).expect("the settle"),
                Some(since) => {
                    if !listed(reads).contains(&real)
                        && clock.await_any_parked_since(
                            &since,
                            fakes::Deadline::after(drive.left().min(PARK_PAUSE)),
                        )
                    {
                        let _stepped = poll_step(clock, parked_at, GROUP_POLL, drive);
                    }
                }
            }
        },
    };
    if answer {
        assert!(
            clock.now() < start + doors::SHUTDOWN_BOUND,
            "the gated drive kept budget: {:?} past {start:?}",
            clock.now() - start
        );
    }
    answer
}

/// A cancelled group with a live member is settled by killing it, not by
/// counting spawns: the command below blocks in `sleep` with nothing in
/// flight, so a count-only settle answers `true` while the group lives.
/// The `true`-means-empty assertion fails at Base on every platform; the
/// fix kills the pending group and answers `true` only once it is empty.
#[test]
fn settle_rekills_a_cancelled_survivor_before_answering_true() {
    let root = fakes::TempDir::new("fiber-read-settle-survivor");
    let ready = Ready::new(root.path());
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    // Paused after listing: the acknowledgement proves the group is listed
    // with nothing in flight before the survivor state is seeded below.
    let (listed_tx, listed_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_tx, release_rx);
    // Held before the reap: the acknowledgement proves the command thread
    // reached its leader's reap, so the leader stays a zombie and the
    // group stays listed until the drive below releases it. The pause is
    // one-shot, so it is armed before the only command thread starts.
    let (held_tx, is_held) = mpsc::channel::<()>();
    let (free_tx, freed_rx) = mpsc::channel::<()>();
    reads.pause_at_command_reap(held_tx, freed_rx);
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pgid: u32 = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    Deadline::after(DEADLINE)
        .recv(&listed_rx)
        .expect("the group listed");
    assert_eq!(listed(&reads), vec![pgid], "listed while it runs");
    assert_eq!(reads.lock().spawning, 0, "nothing in flight");
    release_tx.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&is_held)
        .expect("the command held before its reap");
    assert!(
        fakes::kill_group(pgid, "0").unwrap(),
        "the member lives before the settle"
    );
    // The probe's post-cancel survivor: cancelled with a live listed group
    // and no kill sent, as a fork during the kernel kill walk leaves one.
    // Set directly: `cancel` would kill the group and hide the survivor.
    reads.lock().cancelled = true;
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    let drive = Deadline::after(DEADLINE);
    let answer = drive_settle_to_completion(&clock, &reads, &settled, &drive, pgid, vec![free_tx]);
    let lives = fakes::kill_group(pgid, "0").unwrap_or(false);
    // Guarded cleanup before the assertions: the survivor must not
    // outlive the test either way.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    let _finished = fakes::within("the killed command", Deadline::after(DEADLINE), move || {
        running.join().unwrap()
    });
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
    assert!(answer, "settle answers `true` once the group is empty");
    assert!(
        !lives,
        "settle answered `true` while group {pgid} still lives"
    );
}

/// The superseded blind drive spends the whole bound while a held group
/// stays listed: with the reap held through the entire drive, every
/// 50 ms advance only re-parks the settle, so every derived cycle runs,
/// the settle answers `false` at the bound, and the group is still
/// listed. This pins the ticket's core scenario — bound spent while the
/// group stays listed — which the claim tests above now drive through
/// the gated helper instead. Deterministic with no load: every cycle is
/// confirmed by a re-park, the group cannot unlist while its leader
/// stays an unreaped zombie, and the clock moves only through this
/// test's advances.
#[test]
fn settle_withheld_reap_spends_the_bound_with_the_group_listed() {
    let root = fakes::TempDir::new("fiber-read-settle-withheld");
    let ready = Ready::new(root.path());
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    // Paused after listing: the acknowledgement proves the group is listed
    // with nothing in flight before the survivor state is seeded below.
    let (listed_tx, listed_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_tx, release_rx);
    // Held before the reap: the acknowledgement proves the command thread
    // reached its leader's reap, so the leader stays a zombie and the
    // group stays listed. Unlike the claim tests, the hold stays armed
    // through the whole drive below and is dropped only for cleanup.
    let (held_tx, is_held) = mpsc::channel::<()>();
    let (free_tx, freed_rx) = mpsc::channel::<()>();
    reads.pause_at_command_reap(held_tx, freed_rx);
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pgid: u32 = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    Deadline::after(DEADLINE)
        .recv(&listed_rx)
        .expect("the group listed");
    assert_eq!(listed(&reads), vec![pgid], "listed while it runs");
    assert_eq!(reads.lock().spawning, 0, "nothing in flight");
    release_tx.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&is_held)
        .expect("the command held before its reap");
    assert!(
        fakes::kill_group(pgid, "0").unwrap(),
        "the member lives before the settle"
    );
    // The probe's post-cancel survivor: cancelled with a live listed group
    // and no kill sent, as a fork during the kernel kill walk leaves one.
    // Set directly: `cancel` would kill the group and hide the survivor.
    reads.lock().cancelled = true;
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    // The superseded blind pattern, pinned: advance to each confirmed
    // re-park for the whole bound at the 50 ms cadence — the count is
    // derived from the bound, never hardcoded — checking the answer each
    // iteration, since the final advance's pass reaches the bound and
    // answers instead of re-parking.
    let drive = Deadline::after(DEADLINE);
    let poll = clock.origin() + GROUP_POLL;
    assert!(
        clock.await_parked(poll, drive),
        "the settle parked at its poll"
    );
    let cycles = (doors::SHUTDOWN_BOUND.as_millis() / 50) as usize;
    // Crosses only poll pauses to the shutdown end; the run is proven parked at its poll (ruling 6260728504).
    let mut mark = clock.advance_marked(Duration::from_millis(50));
    let mut answer = None;
    for _ in 1..cycles {
        if let Ok(got) = settled.try_recv() {
            answer = Some(got);
            break;
        }
        // A missing re-park means the settle answered instead of
        // parking again; the drive takes it below. While the reap stays
        // held that never happens before the bound.
        if !clock.await_parked_since(
            &mark,
            Some(clock.now() + GROUP_POLL),
            fakes::Deadline::after(drive.left().min(PARK_PAUSE)),
        ) {
            answer = Some(drive.recv(&settled).expect("the settle"));
            break;
        }
        // Crosses only poll pauses to the shutdown end; the run is proven parked at its poll (ruling 6260728504).
        mark = clock.advance_marked(Duration::from_millis(50));
    }
    // The final advance's pass answers at the bound instead of re-parking.
    let answer = match answer {
        Some(answer) => answer,
        None => drive.recv(&settled).expect("the settle"),
    };
    assert!(
        !answer,
        "the withheld drive spends the bound and settles false"
    );
    assert_eq!(
        clock.now(),
        clock.origin() + doors::SHUTDOWN_BOUND,
        "the blind drive spent exactly the bound"
    );
    assert!(
        listed(&reads).contains(&pgid),
        "the bound leaves the group listed: {:?}",
        listed(&reads)
    );
    // Cleanup: the release drops before anything joins the holder, then
    // the group is killed for real and the command thread reaped.
    drop(free_tx);
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    let _finished = fakes::within("the killed command", Deadline::after(DEADLINE), move || {
        running.join().unwrap()
    });
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

/// A cancelled group that survives the first kill gets SIGKILL again, and
/// none once SRCH retires it: the script answers the cancel's probe and
/// send, the settle's first probe and send, then SRCH. The send count pins
/// both deliveries and the absence of any after retirement.
#[test]
fn settle_sends_again_until_srch_then_never_again() {
    let root = fakes::TempDir::new("fiber-read-settle-retry");
    let ready = Ready::new(root.path());
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let (listed_tx, listed_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_tx, release_rx);
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pgid: u32 = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    Deadline::after(DEADLINE)
        .recv(&listed_rx)
        .expect("the group listed");
    release_tx.send(()).unwrap();
    // Alive through the cancel and the settle's first pass, gone on the
    // second: an accepted send never retires the group, only SRCH does.
    reads.script_group(
        pgid,
        vec![Ok(()), Ok(()), Err(Errno::SRCH)],
        vec![Ok(()), Ok(())],
    );
    let finished = cancel_and_settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    // The cancel's kill and the settle's first pass each deliver once;
    // the settle then parks at its first poll, an exact deadline on the
    // unmoved clock.
    let poll = clock.origin() + GROUP_POLL;
    clock.advance_when_parked(Some(poll), None, GROUP_POLL, &Deadline::after(DEADLINE));
    assert!(
        Deadline::after(DEADLINE)
            .recv(&finished)
            .expect("the settle"),
        "the settle retires the group on SRCH"
    );
    assert_eq!(reads.scripted_sends(), 2, "one send per pass before SRCH");
    // A retired group earns no more sends: another settle finds nothing.
    let again = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3)).recv_or_fail(&again, "the second settle"),
        "a retired group settles at once"
    );
    assert_eq!(
        reads.scripted_sends(),
        2,
        "no send after SRCH retired the group"
    );
    // The scripted sends never touched the real group: kill it for real
    // and reap the command thread on every path.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    let _finished = fakes::within("the killed command", Deadline::after(DEADLINE), move || {
        running.join().unwrap()
    });
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the real group is gone"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

/// Two cancelled groups drain together within the one bound, though their
/// empty observations arrive on different passes: the scripted group is
/// observed SRCH on the settle's second pass, while the real group is
/// pruned whenever its death lands. The scripted group is also killed for
/// real up front, so the script never disagrees with the kernel past its
/// schedule and no exhaustion fallback can keep it pending.
#[test]
fn settle_drains_two_groups_together_within_one_bound() {
    let root = fakes::TempDir::new("fiber-read-settle-two-groups");
    let ready_one = ready_in(root.path(), "ready-one");
    let ready_two = ready_in(root.path(), "ready-two");
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let pre_one = fakes::Watchdog::matching(&ready_one.path().display().to_string());
    let pre_two = fakes::Watchdog::matching(&ready_two.path().display().to_string());
    let start = |reads: &Arc<Reads>, ready: &Ready| {
        let reads = Arc::clone(reads);
        let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    // Each spawn pauses after it lists its group: the acknowledgements
    // prove both groups are listed before the assertion below. A ready
    // line alone does not, since the child writes it before the spawn
    // relocks and lists. The pauses are one-shot, so each spawn's pause
    // is armed before it starts, once the earlier spawn proved it listed.
    let (listed_one, is_listed_one) = mpsc::channel::<()>();
    let (free_one, freed_one) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_one, freed_one);
    // Each command thread also holds before it reaps its leader, so the
    // second group's leader stays a zombie and the group stays listed
    // until the drive below releases it. The pauses are one-shot: the
    // first hold is armed before the first thread starts and acknowledged
    // before the second is armed, so each thread consumes its own.
    let (held_one, is_held_one) = mpsc::channel::<()>();
    let (release_one, freed_reap_one) = mpsc::channel::<()>();
    reads.pause_at_command_reap(held_one, freed_reap_one);
    let running_one = start(&reads, &ready_one);
    Deadline::after(DEADLINE)
        .recv(&is_listed_one)
        .expect("the first group listed");
    free_one.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&is_held_one)
        .expect("the first command held before its reap");
    let (listed_two, is_listed_two) = mpsc::channel::<()>();
    let (free_two, freed_two) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_two, freed_two);
    let (held_two, is_held_two) = mpsc::channel::<()>();
    let (release_two, freed_reap_two) = mpsc::channel::<()>();
    reads.pause_at_command_reap(held_two, freed_reap_two);
    let running_two = start(&reads, &ready_two);
    Deadline::after(DEADLINE)
        .recv(&is_listed_two)
        .expect("the second group listed");
    free_two.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&is_held_two)
        .expect("the second command held before its reap");
    let pgid_one: u32 = ready_one.wait(fakes::Deadline::after(DEADLINE))[0];
    let pgid_two: u32 = ready_two.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog_one = fakes::Watchdog::group(pgid_one);
    let watchdog_two = fakes::Watchdog::group(pgid_two);
    assert_eq!(listed(&reads).len(), 2, "both groups listed");
    // The first group follows the script; the second stays real, so every
    // send really kills it. The first is also killed for real up front:
    // its command thread reaps the leader while the script schedules its
    // SRCH observation on the second pass, whatever wall time that takes.
    reads.script_group(
        pgid_one,
        vec![Ok(()), Ok(()), Err(Errno::SRCH)],
        vec![Ok(()), Ok(())],
    );
    match fakes::kill_group(pgid_one, "KILL") {
        Ok(_) | Err(_) => {}
    }
    let finished = cancel_and_settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    // The cancel's kill and the settle's first pass each deliver to the
    // scripted group; the shared drive below advances past each confirmed
    // park, so the script schedules its SRCH observation on the second
    // pass, whatever wall time the real group's death takes.
    let drive = Deadline::after(DEADLINE);
    assert!(
        drive_settle_to_completion(
            &clock,
            &reads,
            &finished,
            &drive,
            pgid_two,
            vec![release_one, release_two]
        ),
        "both groups drain within the one bound"
    );
    assert_eq!(reads.scripted_sends(), 2, "one send per pass before SRCH");
    assert!(
        fakes::group_empties(pgid_two, fakes::Deadline::after(DEADLINE)),
        "the real group died through the shared settlement"
    );
    // Reap both command threads on every path: the first was killed up
    // front, the second through the shared settlement.
    let _one = fakes::within(
        "the first killed command",
        Deadline::after(DEADLINE),
        move || running_one.join().unwrap(),
    );
    let _two = fakes::within(
        "the second killed command",
        Deadline::after(DEADLINE),
        move || running_two.join().unwrap(),
    );
    assert!(
        fakes::group_empties(pgid_one, fakes::Deadline::after(DEADLINE)),
        "the scripted group is gone"
    );
    watchdog_one.stand_down(fakes::Deadline::after(DEADLINE));
    watchdog_two.stand_down(fakes::Deadline::after(DEADLINE));
    pre_one.stand_down(fakes::Deadline::after(DEADLINE));
    pre_two.stand_down(fakes::Deadline::after(DEADLINE));
}

/// PERM and other probe errors keep a cancelled group pending: they never
/// retire it, so the settle retries within the bound and answers `false`
/// at it. The script outlasts the 5 s bound at a 10 ms cadence.
#[test]
fn settle_retries_a_perm_group_until_the_bound_then_fails() {
    let root = fakes::TempDir::new("fiber-read-settle-perm");
    let ready = Ready::new(root.path());
    let clock = FakeClock::new();
    let end = clock.origin() + doors::SHUTDOWN_BOUND;
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let (listed_tx, listed_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_tx, release_rx);
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pgid: u32 = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    Deadline::after(DEADLINE)
        .recv(&listed_rx)
        .expect("the group listed");
    release_tx.send(()).unwrap();
    let probes: Vec<rustix::io::Result<()>> = (0..300)
        .flat_map(|_| [Err(Errno::PERM), Err(Errno::INTR)])
        .collect();
    let sends: Vec<rustix::io::Result<()>> = vec![Ok(()); 600];
    reads.script_group(pgid, probes, sends);
    let finished = cancel_and_settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    // Drive to the bound, one acknowledged park per advance. The settle
    // cannot answer before the bound: every probe keeps it pending.
    let poll = clock.origin() + GROUP_POLL;
    assert!(
        clock.await_parked(poll, fakes::Deadline::after(DEADLINE)),
        "the settle parked at its poll"
    );
    // Crosses only poll pauses to the shutdown end; the run is proven parked at its poll (ruling 6260728504).
    let mut mark = clock.advance_marked(Duration::from_millis(50));
    loop {
        if let Ok(answer) = finished.try_recv() {
            assert!(
                !answer,
                "a group pending past every probe never settles true"
            );
            break;
        }
        if clock.now() >= end {
            let answer = Deadline::after(DEADLINE)
                .recv(&finished)
                .expect("the settle");
            assert!(!answer, "a group still pending at the bound settles false");
            break;
        }
        assert!(
            clock.await_parked_since(
                &mark,
                Some(clock.now() + GROUP_POLL),
                fakes::Deadline::after(DEADLINE)
            ),
            "the settle parked again"
        );
        if let Ok(answer) = finished.try_recv() {
            assert!(
                !answer,
                "a group pending past every probe never settles true"
            );
            break;
        }
        // Crosses only poll pauses to the shutdown end; the run is proven parked at its poll (ruling 6260728504).
        mark = clock.advance_marked(Duration::from_millis(50));
    }
    let sends = reads.scripted_sends();
    assert!(
        sends >= 2,
        "the settle retried within the bound, not once: {sends}"
    );
    // The scripted sends never touched the real group: kill it for real
    // and reap the command thread on every path.
    match fakes::kill_group(pgid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    let _finished = fakes::within("the killed command", Deadline::after(DEADLINE), move || {
        running.join().unwrap()
    });
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the real group is gone"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

/// A refusal id stays not alive without consulting its script: 0 and 1
/// are unsignallable while `u32::MAX` is unrepresentable, so the guarded
/// validation returns before any probe. Each script answers `Ok` (alive),
/// so the `&&` mutant, which proceeds on either half alone, answers true
/// and fails here.
#[test]
fn refusal_ids_stay_not_alive_without_probing() {
    let reads = Reads::default();
    reads.script_group(0, vec![Ok(())], vec![]);
    assert!(
        !reads.lock().group_alive(0),
        "group 0 is refused before any probe"
    );
    reads.script_group(1, vec![Ok(())], vec![]);
    assert!(
        !reads.lock().group_alive(1),
        "group 1 is refused before any probe"
    );
    reads.script_group(u32::MAX, vec![Ok(())], vec![]);
    assert!(
        !reads.lock().group_alive(u32::MAX),
        "an unrepresentable id is refused before any probe"
    );
}

/// A refusal id is never sent to: 0 and 1 are unsignallable while
/// `u32::MAX` is unrepresentable, so the guarded validation returns
/// before any send. The `&&` mutant, which proceeds on either half
/// alone, records a scripted send and fails the count below.
#[test]
fn refusal_ids_are_never_sent() {
    let reads = Reads::default();
    reads.script_group(0, vec![], vec![Ok(())]);
    reads.lock().kill_listed(0);
    assert_eq!(
        reads.scripted_sends(),
        0,
        "group 0 is refused before any send"
    );
    reads.script_group(1, vec![], vec![Ok(())]);
    reads.lock().kill_listed(1);
    assert_eq!(
        reads.scripted_sends(),
        0,
        "group 1 is refused before any send"
    );
    reads.script_group(u32::MAX, vec![], vec![Ok(())]);
    reads.lock().kill_listed(u32::MAX);
    assert_eq!(
        reads.scripted_sends(),
        0,
        "an unrepresentable id is refused before any send"
    );
}

/// A cancelled settle with nothing listed answers `true` at once: no
/// group means no poll park, on any clock.
#[test]
fn settle_after_cancel_answers_true_at_once_with_nothing_listed() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    reads.cancel();
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3)).recv_or_fail(&settled, "the settle"),
        "nothing listed settles at once"
    );
}

/// Repeat shutdowns share the first end: a second `cancel_and_settle` and
/// a later `settle` earn no fresh bound. The held spawn keeps both waits
/// parked; past the first end both answer `false`.
#[test]
fn repeat_shutdowns_reuse_the_first_end() {
    let clock = FakeClock::new();
    let bound = doors::SHUTDOWN_BOUND;
    let end = clock.origin() + bound;
    let reads = Arc::new(Reads::default());
    // The marker stays in the held shell's command line, so a leaked late
    // child still matches: the watchdog reaps the group on every path,
    // including a failed assertion before the release below.
    let marker = "fiber-shutdown-end-reuse";
    let watching = fakes::Watchdog::matching(marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let held = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&format!("sleep 30 # {marker}"))))
    };
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    let first = cancel_and_settle_on_thread(&reads, &clock, bound);
    assert!(
        clock.await_parked(end, fakes::Deadline::after(DEADLINE)),
        "the first shutdown parked at its end"
    );
    assert_eq!(
        reads.lock().shutdown_end,
        Some(end),
        "the first call stored the end"
    );
    // A repeat one nanosecond before the end must not move it.
    let mark = clock.advance_when_parked(
        Some(end),
        None,
        bound
            .checked_sub(Duration::from_nanos(1))
            .expect("the bound holds a nanosecond"),
        &Deadline::after(DEADLINE),
    );
    assert!(
        clock.await_parked_since(&mark, Some(end), fakes::Deadline::after(DEADLINE)),
        "the first shutdown parked again at its end"
    );
    let second = cancel_and_settle_on_thread(&reads, &clock, bound);
    assert_eq!(
        reads.lock().shutdown_end,
        Some(end),
        "the repeat stored no fresh end"
    );
    // Past the first end both answer `false`: the repeat earned nothing.
    clock.advance_when_parked_threads(
        Some(end),
        2,
        Duration::from_nanos(1),
        &Deadline::after(DEADLINE),
    );
    assert!(
        !Deadline::after(DEADLINE)
            .recv(&first)
            .expect("the first shutdown"),
        "the held spawn outlasts the shared end"
    );
    assert!(
        !Deadline::after(DEADLINE)
            .recv(&second)
            .expect("the second shutdown"),
        "the repeat shares the spent end"
    );
    // A later settle reads the same spent end and answers `false` at
    // once, without parking.
    let settled = settle_on_thread(&reads, &clock, bound);
    assert!(
        !Deadline::after(Duration::from_secs(3)).recv_or_fail(&settled, "the spent settle"),
        "a settle past the shared end answers false at once"
    );
    release.send(()).expect("the spawn's release");
    let error = fakes::within("the late command", Deadline::after(DEADLINE), move || {
        held.join().unwrap()
    })
    .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(marker, fakes::Deadline::after(DEADLINE)),
        "the late group is gone"
    );
    watching.stand_down(fakes::Deadline::after(DEADLINE));
}

/// An end past what the clock can count expires at once: a held spawn
/// still answers `false` without parking, and the stored end is now.
#[test]
fn an_overflowing_shutdown_end_expires_at_once() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let marker = "fiber-shutdown-end-overflow";
    let watching = fakes::Watchdog::matching(marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let held = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&format!("sleep 30 # {marker}"))))
    };
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    let now = clock.now();
    let finished = cancel_and_settle_on_thread(&reads, &clock, Duration::MAX);
    assert!(
        !Deadline::after(Duration::from_secs(3))
            .recv(&finished)
            .expect("the shutdown"),
        "an overflowing bound expires at once"
    );
    assert_eq!(
        reads.lock().shutdown_end,
        Some(now),
        "overflow stores now, not None"
    );
    release.send(()).expect("the spawn's release");
    let error = fakes::within("the late command", Deadline::after(DEADLINE), move || {
        held.join().unwrap()
    })
    .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(marker, fakes::Deadline::after(DEADLINE)),
        "the late group is gone"
    );
    watching.stand_down(fakes::Deadline::after(DEADLINE));
}

/// An empty shutdown answers `true` even past its stored end: expiry only
/// fails a shutdown with anything still pending.
#[test]
fn an_empty_shutdown_answers_true_past_its_end() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let first = cancel_and_settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3))
            .recv(&first)
            .expect("the first shutdown"),
        "an empty shutdown settles at once"
    );
    // No code under test waits on the clock here; the first shutdown has answered without parking (ruling 6179406086).
    clock.advance(doors::SHUTDOWN_BOUND + Duration::from_secs(1));
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3)).recv_or_fail(&settled, "the spent settle"),
        "an empty shutdown stays true past its end"
    );
}

/// Runs `reads.cancel_and_settle` for `bound` on `clock` on a thread of
/// its own, returning the channel its answer arrives on. The test receives
/// through [`Deadline`], never a raw `recv_timeout`.
fn cancel_and_settle_on_thread(
    reads: &Arc<Reads>,
    clock: &Arc<FakeClock>,
    bound: Duration,
) -> mpsc::Receiver<bool> {
    let (done, finished) = mpsc::channel();
    let (reads, clock) = (Arc::clone(reads), Arc::clone(clock));
    std::thread::spawn(move || {
        done.send(reads.cancel_and_settle(&*clock, bound))
            .unwrap_or(());
    });
    finished
}

/// Runs `reads.settle` for `bound` on `clock` on a thread of its own,
/// returning the channel its answer arrives on. The test receives through
/// [`Deadline`], never a raw `recv_timeout`.
fn settle_on_thread(
    reads: &Arc<Reads>,
    clock: &Arc<FakeClock>,
    bound: Duration,
) -> mpsc::Receiver<bool> {
    let (done, finished) = mpsc::channel();
    let (reads, clock) = (Arc::clone(reads), Arc::clone(clock));
    std::thread::spawn(move || {
        done.send(reads.settle(&*clock, bound)).unwrap_or(());
    });
    finished
}

/// Proves the settle is still waiting: it parks on the `FakeClock` at
/// `until`, so it ran its checks and found work left (`done` false, the
/// clock short of its end) and cannot answer without a wake, and its
/// answer channel is still empty. A settle that answered early never
/// parks, so the park wait fails naming `what` instead of hanging.
#[track_caller]
fn still_settling(clock: &FakeClock, until: Instant, settled: &mpsc::Receiver<bool>, what: &str) {
    assert!(
        clock.await_parked(until, fakes::Deadline::after(DEADLINE)),
        "the settle parks at its wait: {what}"
    );
    assert!(
        matches!(settled.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "settle answered early: {what}"
    );
}

/// One park acknowledgement's wait of the drive's shared deadline: a
/// re-park after an advance lands in milliseconds, so expiry re-checks
/// completion and awaits the late park under what is left. A poll pause,
/// not a deadline.
const PARK_PAUSE: Duration = Duration::from_millis(200);

/// A cancel mid-spawn is settled before exit: the spawn blocks before it
/// spawns, the cancel lands, and the settle holds until the late spawn
/// lists or kills its group. Without the count the settle answers at once,
/// and the negative receive below turns red.
#[test]
fn a_cancel_mid_spawn_is_settled_before_exit() {
    let root = fakes::TempDir::new("fiber-read-settle-spawn");
    let marker = format!("fiber-read-settle-{}", root.path().display());
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    // The marker stays in the late shell's command line, so a leaked late
    // child still matches.
    let watching = fakes::Watchdog::matching(&marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let finished = credential_on_thread(
        &reads,
        sh(&format!("sleep 30 # {marker}")),
        Duration::from_secs(30),
        &clock,
    );
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    // Armed before the release, so the late spawn stops between its kill
    // and its reap: the acknowledgement proves the kill is sent while the
    // leader waits unreaped, so the settle below holds on the count, not
    // on process timing.
    let (reaping, is_reaping) = mpsc::channel::<()>();
    let (free_reap, reap_freed) = mpsc::channel::<()>();
    reads.pause_at_reap(reaping, reap_freed);
    reads.cancel();
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    // No group is listed yet, so the settle parks at its bound: reaching
    // the park proves it saw the raised count and cannot answer yet.
    still_settling(
        &clock,
        clock.origin() + doors::SHUTDOWN_BOUND,
        &settled,
        "a spawn in flight holds the settle",
    );
    release.send(()).expect("the spawn's release");
    Deadline::after(DEADLINE)
        .recv(&is_reaping)
        .expect("the spawn reached its reap");
    // The spawn listed its group before the kill, so the listing holds
    // the late PGID: the shell itself cannot report it, as the kill lands
    // before it could run.
    let groups = listed(&reads);
    assert_eq!(
        groups.len(),
        1,
        "only the late group is listed: {:?}",
        groups
    );
    let pgid = groups[0];
    // The spawn listed its group before the reap pause, so the cancelled
    // settle re-parks at its poll: reaching the park proves it saw the
    // raised count and cannot answer on the kill alone.
    still_settling(
        &clock,
        clock.origin() + GROUP_POLL,
        &settled,
        "the late child unreaped holds the settle",
    );
    free_reap.send(()).expect("the reap's release");
    // The late kill landed while the reap was held, so the shell is
    // already dead: the wall-clock waits confirm it before the fake clock
    // moves, never ahead of the real death. The command line only proves
    // the shell is gone; the group wait proves the whole group is empty
    // before the drive advances.
    assert!(
        fakes::matching_exits(&marker, fakes::Deadline::after(DEADLINE)),
        "the late group is gone"
    );
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the late group is empty"
    );
    // A fork that landed inside the kill still lists the group after the
    // reap: the settle re-kills on its poll cadence and parks between
    // passes with no further wake, so each advance follows a confirmed
    // park until the answer arrives, on the stdout-hold timeout's cadence.
    // The answer still arrives through `DEADLINE`.
    let drive = Deadline::after(DEADLINE);
    let mut answer: Option<bool> = None;
    while answer.is_none() {
        match settled.try_recv() {
            Ok(got) => {
                answer = Some(got);
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        if drive.left().is_zero() {
            break;
        }
        let poll = clock
            .now()
            .checked_add(GROUP_POLL)
            .expect("the poll is countable");
        let pause = drive.left().min(PARK_PAUSE);
        if pause.is_zero() {
            continue;
        }
        match clock.try_advance_when_parked(Some(poll), None, GROUP_POLL, pause) {
            Ok(_) => {}
            Err(Miss::NoPark) => {}
            Err(Miss::Mismatch {
                watched: (_, Some(at)),
                ..
            }) if at == clock.origin() + doors::SHUTDOWN_BOUND => {
                answer = drive.recv(&settled).ok();
            }
            Err(miss) => panic!("{miss}"),
        }
    }
    let answer = match answer {
        Some(answer) => answer,
        None => drive.recv(&settled).expect("the settle"),
    };
    assert!(answer, "settle waits out the late kill");
    let error = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the late read")
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(reads.lock().spawning, 0, "the late kill dropped the count");
    assert!(
        listed(&reads).is_empty(),
        "the settle retired the late group: {:?}",
        listed(&reads)
    );
    watching.stand_down(fakes::Deadline::after(DEADLINE));
}

/// The settle waits for the late child's reap, not just its kill: the
/// spawn stops between the kill and the wait, and the settle holds until
/// the reap releases it. A settle that ignored the count would answer at
/// once instead of parking, so the park proof below turns red.
#[test]
fn settle_waits_for_the_late_childs_reap() {
    let root = fakes::TempDir::new("fiber-read-settle-reap");
    let marker = format!("fiber-read-reap-{}", root.path().display());
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let watching = fakes::Watchdog::matching(&marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let finished = credential_on_thread(
        &reads,
        sh(&format!("sleep 30 # {marker}")),
        Duration::from_secs(30),
        &clock,
    );
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    // Armed before the release, so the late kill stops at the reap.
    let (reaping, is_reaping) = mpsc::channel::<()>();
    let (free_reap, reap_freed) = mpsc::channel::<()>();
    reads.pause_at_reap(reaping, reap_freed);
    reads.cancel();
    release.send(()).expect("the spawn's release");
    Deadline::after(DEADLINE)
        .recv(&is_reaping)
        .expect("the spawn reached its reap");
    // The spawn listed its group before the kill, so the listing holds
    // the late PGID: the shell itself cannot report it, as the kill lands
    // before it could run.
    let groups = listed(&reads);
    assert_eq!(
        groups.len(),
        1,
        "only the late group is listed: {:?}",
        groups
    );
    let pgid = groups[0];
    // Armed once the spawn reaches its reap pause: the settle's pass
    // before the reap's release probes scripted-alive and parks, its first pass
    // after probes scripted-alive again, and later probes fall back to
    // the real syscall, which finds the reaped group empty. The scripted
    // sends never touch the real group, so the kill below still reaps it
    // for real.
    reads.script_group(pgid, vec![Ok(()), Ok(())], vec![]);
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    // The leader is killed but not yet reaped: the count is still raised,
    // and the listed group parks the cancelled settle at its poll.
    still_settling(
        &clock,
        clock.origin() + GROUP_POLL,
        &settled,
        "the late child's reap holds the settle",
    );
    // Armed before the reap's release, so the late spawn holds between
    // its leader's reap and the count drop: the acknowledgement proves
    // the leader is reaped while the count stays raised, so the settle
    // holds on the count alone once the scripted probes run out.
    let (reaped, is_reaped) = mpsc::channel::<()>();
    let (free_reaped, reaped_freed) = mpsc::channel::<()>();
    reads.pause_at_reaped(reaped, reaped_freed);
    free_reap.send(()).expect("the reap's release");
    Deadline::after(DEADLINE)
        .recv(&is_reaped)
        .expect("the spawn reaped its leader");
    // The scripted probes keep the group listed past the reap, so one
    // pass after it is never enough: each advance drives one confirmed
    // poll park while the group is still listed, and the real probe then
    // retires the group so the settle parks at its end while only the
    // count is raised. Each advance follows a confirmed park, each wait
    // takes its own deadline, and the clock never moves past the end: at
    // a park at the end the test releases the held count and waits for
    // the answer the count drop delivers.
    let end = clock.origin() + doors::SHUTDOWN_BOUND;
    let mut answer: Option<bool> = None;
    while answer.is_none() {
        match settled.try_recv() {
            Ok(got) => {
                answer = Some(got);
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                panic!("the settle ended without answering")
            }
        }
        let poll = clock
            .now()
            .checked_add(GROUP_POLL)
            .expect("the poll is countable");
        // The next poll never passes the end: past it the settle parks at
        // the end itself, which the step below reports without advancing.
        assert!(
            poll <= end,
            "the next poll stays short of the settle's end: {poll:?} past {end:?}"
        );
        // Like `poll_step`, but an end park also reports no step: only
        // this drive waits out the spawn count, so only it accepts the
        // settle parking at its end. `poll_step` still fails any other
        // park for its other callers.
        let stepped: Option<Mark> =
            match clock.try_advance_when_parked(Some(poll), None, GROUP_POLL, PARK_PAUSE) {
                Ok(mark) => Some(mark),
                Err(Miss::NoPark) => None,
                Err(Miss::Mismatch {
                    watched: (_, Some(at)),
                    ..
                }) if at == end => None,
                Err(miss @ Miss::Mismatch { .. }) => {
                    panic!("{miss} at {:?}; parked: {:?}", clock.now(), clock.parked())
                }
            };
        match stepped {
            Some(_) => {
                assert!(
                    listed(&reads).contains(&pgid),
                    "the poll park still lists the late group: {:?}",
                    listed(&reads)
                );
            }
            None => {
                if let Ok(got) = settled.try_recv() {
                    answer = Some(got);
                    break;
                }
                // No fresh poll park: either the settle answered, handled
                // above, or it parks at its end while only the count is
                // raised. The park wait proves which without moving the
                // clock.
                assert!(
                    clock.await_parked(end, fakes::Deadline::after(DEADLINE)),
                    "the settle parks at its end once the group is gone"
                );
                assert!(
                    listed(&reads).is_empty(),
                    "only the count holds the settle at its end: {:?}",
                    listed(&reads)
                );
                assert_eq!(
                    reads.lock().spawning,
                    1,
                    "the held count holds the settle at its end"
                );
                free_reaped.send(()).expect("the reaped release");
                answer = Some(
                    Deadline::after(DEADLINE)
                        .recv_or_fail(&settled, "the settle after the count drop"),
                );
                break;
            }
        }
    }
    let answer = match answer {
        Some(answer) => answer,
        None => Deadline::after(DEADLINE)
            .recv(&settled)
            .expect("the settle"),
    };
    assert!(answer, "settle waits out the reap");
    let error = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the late read")
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(reads.lock().spawning, 0, "the late kill dropped the count");
    assert!(
        fakes::matching_exits(&marker, fakes::Deadline::after(DEADLINE)),
        "the late group is gone"
    );
    watching.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_failed_spawn_does_not_leave_the_count_raised() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let mut missing = Command::new("fiber-test-binary-that-cannot-exist");
    missing
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    // Within a wall-clock bound: a missed decrement hangs the settle, and
    // the bound fails the test instead of hanging the binary. The exact
    // count pins the decrement at once, so a `+=` or `/=` mutant fails
    // here instead of running out the settle below.
    let error = fakes::within("the failed spawn", Deadline::after(DEADLINE), {
        let reads = Arc::clone(&reads);
        move || reads.command(&mut missing)
    })
    .expect_err("a command that cannot start");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(reads.lock().spawning, 0, "the error arm dropped the count");
    // The settle runs on its own thread and its answer arrives through
    // `Deadline` in seconds: under the `==`/`!=` mutant the settle at zero
    // parks on the `FakeClock`, so this fails instead of hanging the suite.
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3)).recv_or_fail(&settled, "the settle"),
        "the error arm dropped the count"
    );
}

/// The settle gives up at the bound: one nanosecond short it is still
/// parked with no answer, at the bound it answers `false`. Pins the
/// boundary both sides.
#[test]
fn settle_gives_up_at_the_bound() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let bound = doors::SHUTDOWN_BOUND;
    let end = clock.origin() + bound;
    // A spawn held before it spawns: the count stays raised while the test
    // drives the clock.
    // The marker stays in the late shell's command line, so a leaked late
    // child still matches: the watchdog reaps the group on every path,
    // including a failed assertion before the cancel below.
    let marker = "fiber-settle-gives-up-at-the-bound";
    let watching = fakes::Watchdog::matching(marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let held = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&format!("sleep 30 # {marker}"))))
    };
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    let settled = settle_on_thread(&reads, &clock, bound);
    let mark = clock.advance_when_parked(
        Some(end),
        None,
        bound
            .checked_sub(Duration::from_nanos(1))
            .expect("the bound holds a nanosecond"),
        &Deadline::after(DEADLINE),
    );
    assert!(
        clock.await_parked_since(&mark, Some(end), fakes::Deadline::after(DEADLINE)),
        "the settle parked again short of its bound"
    );
    // One nanosecond short of the bound the settle still parks with no
    // answer: the park repeats the check above, and the empty channel
    // proves it has not answered yet.
    still_settling(&clock, end, &settled, "the settle short of its bound");
    clock.advance_when_parked(
        Some(end),
        None,
        Duration::from_nanos(1),
        &Deadline::after(DEADLINE),
    );
    assert!(
        !Deadline::after(DEADLINE)
            .recv(&settled)
            .expect("the settle"),
        "settle gives up at its bound"
    );
    reads.cancel();
    release.send(()).expect("the spawn's release");
    let error = fakes::within("the late command", Deadline::after(DEADLINE), move || {
        held.join().unwrap()
    })
    .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(marker, fakes::Deadline::after(DEADLINE)),
        "the late group is gone"
    );
    watching.stand_down(fakes::Deadline::after(DEADLINE));
}

/// The settle subscribes before it reads the count: on a clock nothing
/// else subscribed, the advance still wakes it. Without the subscription
/// the advance wakes nobody and the receive below fails at the deadline.
#[test]
fn settle_wakes_on_a_clock_advance_with_no_earlier_subscription() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let bound = doors::SHUTDOWN_BOUND;
    let end = clock.origin() + bound;
    // A spawn held before it spawns, on a clock nothing else subscribed:
    // the settle below is the first subscriber.
    // The marker stays in the late shell's command line, so a leaked late
    // child still matches: the watchdog reaps the group on every path,
    // including a failed assertion before the cancel below.
    let marker = "fiber-settle-wakes-with-no-earlier-subscription";
    let watching = fakes::Watchdog::matching(marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let held = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&format!("sleep 30 # {marker}"))))
    };
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    let settled = settle_on_thread(&reads, &clock, bound);
    clock.advance_when_parked(Some(end), None, bound, &Deadline::after(DEADLINE));
    assert!(
        !Deadline::after(DEADLINE)
            .recv(&settled)
            .expect("the settle"),
        "the advance woke the settle at its bound {end:?}"
    );
    reads.cancel();
    release.send(()).expect("the spawn's release");
    let error = fakes::within("the late command", Deadline::after(DEADLINE), move || {
        held.join().unwrap()
    })
    .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(marker, fakes::Deadline::after(DEADLINE)),
        "the late group is gone"
    );
    watching.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn settle_returns_true_at_once_with_nothing_in_flight() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    // The settle runs on its own thread and its answer arrives through
    // `Deadline` in seconds: under the `==`/`!=` mutant the settle at zero
    // parks on the `FakeClock`, so this fails instead of hanging the suite.
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3)).recv_or_fail(&settled, "the settle"),
        "nothing in flight settles at once"
    );
}

/// The settle's zero comparison pins both sides at once: `true` at count
/// zero immediately, and `false` within the bound while one spawn is held.
/// Each side fails fast on a wall-clock bound of a few seconds, so the
/// `!=` mutant is caught here instead of running out the suite's
/// deadlines. The bound stays on the injected clock: the test advances it
/// only after the parked settle proves it waits on it.
#[test]
fn settle_pins_zero_true_and_held_false_at_once() {
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    // Zero side first: true at once with nothing in flight.
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(Duration::from_secs(3))
            .recv_or_fail(&settled, "settle with nothing in flight"),
        "settle is true at count zero"
    );
    // Held side: one spawn held before it spawns keeps the count raised
    // while the test drives the clock past the bound.
    // The marker stays in the late shell's command line, so a leaked late
    // child still matches: the watchdog reaps the group on every path,
    // including a failed assertion before the cancel below.
    let marker = "fiber-settle-pins-zero-and-held";
    let watching = fakes::Watchdog::matching(marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let held = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&format!("sleep 30 # {marker}"))))
    };
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    let bound = doors::SHUTDOWN_BOUND;
    let end = clock.origin() + bound;
    let held_settle = settle_on_thread(&reads, &clock, bound);
    clock.advance_when_parked(Some(end), None, bound, &Deadline::after(DEADLINE));
    assert!(
        !Deadline::after(Duration::from_secs(3)).recv_or_fail(&held_settle, "the held settle"),
        "settle is false at its bound while a spawn is held"
    );
    reads.cancel();
    release.send(()).expect("the spawn's release");
    let error = fakes::within("the late command", Deadline::after(DEADLINE), move || {
        held.join().unwrap()
    })
    .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(marker, fakes::Deadline::after(DEADLINE)),
        "the late group is gone"
    );
    watching.stand_down(fakes::Deadline::after(DEADLINE));
}
