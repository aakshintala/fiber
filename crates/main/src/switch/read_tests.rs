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
use std::time::Duration;

use contract::ErrorCode;

use super::{CancelOnDrop, Reads, deadline_due, signallable};
use fakes::Deadline;
use fakes::children::Ready;
use fakes::clock::FakeClock;

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

/// Waits under [`DEADLINE`] for `file` to hold a line, and returns it.
#[track_caller]
fn ready_line(file: &std::path::Path) -> String {
    let file = file.to_path_buf();
    fakes::within("the ready file", DEADLINE, move || {
        loop {
            if let Ok(text) = std::fs::read_to_string(&file)
                && text.ends_with('\n')
            {
                return text.trim().to_owned();
            }
            std::thread::yield_now();
        }
    })
}

#[test]
fn run_returns_the_reads_value() {
    let reads = Reads::default();
    let value = fakes::within("the read", DEADLINE, move || reads.run(|| 7));
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
            reads.run(move || {
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
    let rejected = fakes::within("the cancelled run", DEADLINE, move || {
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
            reads.run(move || {
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
    let quick = fakes::within("the quick run", DEADLINE, move || probe.run(|| 1));
    assert_eq!(quick.unwrap(), 1);
    reads.cancel();
    let rejected = fakes::within("the cancelled run", DEADLINE, move || {
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
fn run_after_cancel_is_closing_without_running_the_read() {
    let reads = Reads::default();
    reads.cancel();
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let failure = fakes::within("the refused read", DEADLINE, move || {
        reads.run(move || flag.store(true, Ordering::SeqCst))
    })
    .expect_err("a cancelled read");
    assert_eq!(failure.code, ErrorCode::Closing);
    assert!(!ran.load(Ordering::SeqCst), "the read never ran");
}

#[test]
fn a_command_leads_its_own_group_and_is_listed_until_the_group_empties() {
    let root = fakes::TempDir::new("fiber-read-group");
    let ready = root.path().join("ready");
    let go = root.path().join("go");
    let reads = Arc::new(Reads::default());
    let script = format!(
        "echo \"$$ $(ps -o pgid= -p $$ | tr -d ' ')\" > '{}'; while [ ! -e '{}' ]; do sleep 0.01; done; echo key",
        ready.display(),
        go.display()
    );
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&go.display().to_string());
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let line = ready_line(&ready);
    let (pid, pgid) = line.split_once(' ').unwrap();
    let pid: u32 = pid.parse().unwrap();
    let watchdog = fakes::Watchdog::group(pid);
    assert_eq!(pid.to_string(), pgid, "the command leads its own group");
    assert_eq!(listed(&reads), vec![pid], "listed while it runs");
    std::fs::write(&go, "").unwrap();
    let output = fakes::within("the command", DEADLINE, move || running.join().unwrap()).unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "key\n");
    assert!(fakes::group_empties(pid, DEADLINE));
    // The next command prunes the empty group.
    let probe = Arc::clone(&reads);
    let pruned = fakes::within("the pruning command", DEADLINE, move || {
        probe.command(&mut sh("true"))
    });
    pruned.unwrap();
    assert!(!listed(&reads).contains(&pid), "{:?}", listed(&reads));
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

#[test]
fn cancel_kills_a_running_command_and_it_errs() {
    let root = fakes::TempDir::new("fiber-read-cancel");
    let ready = root.path().join("ready");
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 30", ready.display());
    // Before the spawn, on the ready path the command line carries until
    // it execs; the group watchdog below takes over once the pid is known.
    let pre = fakes::Watchdog::matching(&ready.display().to_string());
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pid: u32 = ready_line(&ready).parse().unwrap();
    let watchdog = fakes::Watchdog::group(pid);
    reads.cancel();
    let error = fakes::within("the killed command", DEADLINE, move || {
        running.join().unwrap()
    })
    .expect_err("a killed command");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(fakes::group_empties(pid, DEADLINE));
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

#[test]
fn a_descendant_keeps_its_group_listed_and_cancel_kills_it() {
    let root = fakes::TempDir::new("fiber-read-descendant");
    let ready = root.path().join("ready");
    let reads = Arc::new(Reads::default());
    let script = format!(
        "sleep 30 >/dev/null 2>&1 & echo $$ > '{}'; echo key",
        ready.display()
    );
    // Before the spawn: the shell exits fast and leaves `sleep` behind,
    // so only a command-line watchdog covers the spawn itself.
    let pre = fakes::Watchdog::matching(&ready.display().to_string());
    let first = Arc::clone(&reads);
    let output = fakes::within("the first command", DEADLINE, move || {
        first.command(&mut sh(&script)).unwrap()
    });
    assert_eq!(String::from_utf8_lossy(&output.stdout), "key\n");
    let group: u32 = ready_line(&ready).parse().unwrap();
    let watchdog = fakes::Watchdog::group(group);
    assert!(fakes::kill_group(group, "0").unwrap(), "sleep lives"); // A later command prunes only empty groups: `sleep` keeps this one.
    let probe = Arc::clone(&reads);
    fakes::within("the pruning command", DEADLINE, move || {
        probe.command(&mut sh("true")).unwrap()
    });
    assert!(listed(&reads).contains(&group), "{:?}", listed(&reads));
    reads.cancel();
    assert!(fakes::group_empties(group, DEADLINE), "cancel killed sleep");
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
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
    let error = fakes::within("the refused command", DEADLINE, move || job.join().unwrap())
        .expect_err("a cancelled command");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(!marker.exists(), "the command never started");
    assert!(listed(&reads).is_empty());
    pre.stand_down(DEADLINE);
}

#[test]
fn only_a_group_above_one_is_signallable() {
    assert!(!signallable(0));
    assert!(!signallable(1));
    assert!(signallable(2));
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
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the wait parked at its deadline"
    );
    let mark = clock.advance_marked(timeout.checked_sub(Duration::from_millis(1)).unwrap());
    assert!(
        clock.await_parked_since(&mark, Some(deadline), DEADLINE),
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
        clock.await_parked(deadline, DEADLINE),
        "the wait parked at its deadline again"
    );
    assert!(
        fakes::kill_group(pgid, "0").unwrap(),
        "the group died before its timeout"
    );
    clock.advance(Duration::from_millis(1));
    let error = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv_or_fail(&finished, "the timed-out read")
        .expect_err("a command still running at its timeout");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the timeout killed the group"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
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
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the wait parked at its deadline"
    );
    let mark = clock.advance_marked(timeout.checked_sub(Duration::from_millis(1)).unwrap());
    assert!(
        clock.await_parked_since(&mark, Some(deadline), DEADLINE),
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
        clock.await_parked(deadline, DEADLINE),
        "the wait parked at its deadline again"
    );
    clock.advance(Duration::from_millis(1));
    let error = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv_or_fail(&finished, "the timed-out read")
        .expect_err("a command holding stdout past its timeout");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the timeout killed the group"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
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
    guard.stand_down(DEADLINE);
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
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    Deadline::after(DEADLINE).recv_or_fail(&is_checked, "the wait's checks");
    // Both wakes land while the wait is held past its checks: the park
    // below already holds them in an earlier generation, so it must
    // return at once. Each thread bumps after its send, so two bumps mean
    // both answers are in.
    let wake = Arc::clone(&reads.wake);
    let seen = wake.generation();
    std::fs::write(&go, "").unwrap();
    fakes::within("the two wakes", DEADLINE, move || {
        loop {
            if wake.generation() >= seen + 2 {
                return;
            }
            std::thread::yield_now();
        }
    });
    release.send(()).unwrap();
    let output = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the interleaved read")
        .unwrap();
    assert_eq!(output.stdout, b"key");
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the group reaped its leader"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
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
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the wait parked at its deadline"
    );
    // The helper finishes while the wait is held here, then the clock
    // moves past the deadline: the wait's next checks see a finish past
    // its end, which times out instead of answering the late key.
    let wake = Arc::clone(&reads.wake);
    let seen = wake.generation();
    {
        let _held = reads.lock();
        std::fs::write(&go, "").unwrap();
        fakes::within("the two wakes", DEADLINE, move || {
            loop {
                if wake.generation() >= seen + 2 {
                    return;
                }
                std::thread::yield_now();
            }
        });
        clock.advance(timeout + Duration::from_secs(1));
    }
    let error = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the late read")
        .expect_err("a key finished after its timeout");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the timeout killed the group"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
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
    let pgid = ready.wait(DEADLINE)[0];
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
    fakes::within("the leader's wake", DEADLINE, {
        let wake = Arc::clone(&wake);
        move || loop {
            if wake.generation() >= sent + 2 {
                return;
            }
            std::thread::yield_now();
        }
    });
    release_two.send(()).unwrap();
    let output = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the cleaning-up leader")
        .unwrap();
    assert_eq!(output.stdout, b"key");
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the group reaped its leader"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
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
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    reads.cancel();
    let error = Deadline::after(DEADLINE)
        .recv_or_fail(&finished, "the cancelled read")
        .expect_err("a cancelled read");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the cancel killed the group"
    );
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
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
    guard.stand_down(DEADLINE);
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
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(fakes::kill_group(pgid, "0").unwrap(), "sleep lives");
    drop(CancelOnDrop(Arc::clone(&reads)));
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "dropping the guard killed sleep"
    );
    let error = reads
        .credential(&mut sh("true"), Duration::from_secs(30), &*clock)
        .expect_err("a read after the drop");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    watchdog.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

/// A cancel lands while a spawn is blocked before it spawns: the cancel
/// returns at once and kills every listed group, and the late spawn kills
/// its own group and is `Interrupted` without ever listing it. Before the
/// fix the spawn holds the state lock across the spawn, so the cancel
/// blocks behind it and the deadline below fails the test.
#[test]
fn cancel_passes_a_blocked_spawn_and_the_late_child_is_killed() {
    let root = fakes::TempDir::new("fiber-read-blocked-spawn");
    let ready_a = root.path().join("ready-a");
    let marker = format!("fiber-read-late-{}", root.path().display());
    let reads = Arc::new(Reads::default());
    let script_a = format!("echo $$ > '{}'; exec sleep 30", ready_a.display());
    let pre_a = fakes::Watchdog::matching(&ready_a.display().to_string());
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
    let pid_a: u32 = ready_line(&ready_a).parse().unwrap();
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
        fakes::group_empties(pid_a, DEADLINE),
        "the cancel killed group A"
    );
    match release_tx.send(()) {
        Ok(()) | Err(_) => {}
    }
    let error = fakes::within("the late command", DEADLINE, move || {
        running_b.join().unwrap()
    })
    .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(
        listed(&reads),
        vec![pid_a],
        "the late group is never listed"
    );
    assert!(
        fakes::matching_exits(&marker, DEADLINE),
        "the late group is gone"
    );
    let late_a = fakes::within("the killed command", DEADLINE, move || {
        running_a.join().unwrap()
    })
    .expect_err("a killed command");
    assert_eq!(late_a.kind(), std::io::ErrorKind::Interrupted);
    watchdog_a.stand_down(DEADLINE);
    pre_a.stand_down(DEADLINE);
    late.stand_down(DEADLINE);
}

/// A cancelled group with a live member is settled by killing it, not by
/// counting spawns: the command below blocks in `sleep` with nothing in
/// flight, so a count-only settle answers `true` while the group lives.
/// The `true`-means-empty assertion fails at Base on every platform; the
/// fix kills the pending group and answers `true` only once it is empty.
#[test]
fn settle_rekills_a_cancelled_survivor_before_answering_true() {
    let root = fakes::TempDir::new("fiber-read-settle-survivor");
    let ready = root.path().join("ready");
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.display());
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.display().to_string());
    // Paused after listing: the acknowledgement proves the group is listed
    // with nothing in flight before the survivor state is seeded below.
    let (listed_tx, listed_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_tx, release_rx);
    let running = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pgid: u32 = ready_line(&ready).parse().unwrap();
    let watchdog = fakes::Watchdog::group(pgid);
    Deadline::after(DEADLINE)
        .recv(&listed_rx)
        .expect("the group listed");
    assert_eq!(listed(&reads), vec![pgid], "listed while it runs");
    assert_eq!(reads.lock().spawning, 0, "nothing in flight");
    release_tx.send(()).unwrap();
    assert!(
        fakes::kill_group(pgid, "0").unwrap(),
        "the member lives before the settle"
    );
    // The probe's post-cancel survivor: cancelled with a live listed group
    // and no kill sent, as a fork during the kernel kill walk leaves one.
    // Set directly: `cancel` would kill the group and hide the survivor.
    reads.lock().cancelled = true;
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    match Deadline::after(SHORT).recv(&settled) {
        Ok(answer) => {
            let lives = fakes::kill_group(pgid, "0").unwrap_or(false);
            // Guarded cleanup before the assertion: the survivor must not
            // outlive the test either way.
            match fakes::kill_group(pgid, "KILL") {
                Ok(_) | Err(_) => {}
            }
            let _finished = fakes::within("the killed command", DEADLINE, move || {
                running.join().unwrap()
            });
            watchdog.stand_down(DEADLINE);
            pre.stand_down(DEADLINE);
            assert!(
                !answer || !lives,
                "settle answered `true` while group {pgid} still lives"
            );
            assert!(answer, "settle answers `true` once the group is empty");
        }
        Err(_) => {
            // Parked on the clock: the fix kills and rechecks. Drive it to
            // the bound, then the answer must arrive with the group empty.
            let end = clock.origin() + doors::SHUTDOWN_BOUND;
            loop {
                if settled.try_recv().is_ok() {
                    break;
                }
                if !clock.await_parked(end, DEADLINE) {
                    break;
                }
                clock.advance(Duration::from_millis(50));
            }
            let answer = Deadline::after(DEADLINE)
                .recv(&settled)
                .expect("the settle");
            let lives = fakes::kill_group(pgid, "0").unwrap_or(false);
            match fakes::kill_group(pgid, "KILL") {
                Ok(_) | Err(_) => {}
            }
            let _finished = fakes::within("the killed command", DEADLINE, move || {
                running.join().unwrap()
            });
            watchdog.stand_down(DEADLINE);
            pre.stand_down(DEADLINE);
            assert!(answer, "settle answers `true` once the group is empty");
            assert!(
                !lives,
                "settle answered `true` while group {pgid} still lives"
            );
        }
    }
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

/// The bound each negative settle receive waits: a settle that may answer
/// does so in milliseconds, so silence for this long proves it is held.
const SHORT: Duration = Duration::from_secs(2);

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
    reads.cancel();
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    assert!(
        Deadline::after(SHORT).recv(&settled).is_err(),
        "settle returned with a spawn in flight"
    );
    release.send(()).expect("the spawn's release");
    assert!(
        Deadline::after(Duration::from_secs(3))
            .recv(&settled)
            .expect("the settle"),
        "settle waits out the late kill"
    );
    let error = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the late read")
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(reads.lock().spawning, 0, "the late kill dropped the count");
    assert!(
        listed(&reads).is_empty(),
        "the late group is never listed: {:?}",
        listed(&reads)
    );
    assert!(
        fakes::matching_exits(&marker, DEADLINE),
        "the late group is gone"
    );
    watching.stand_down(DEADLINE);
}

/// The settle waits for the late child's reap, not just its kill: the
/// spawn stops between the kill and the wait, and the settle holds until
/// the reap releases it. A decrement at the relock answers early and turns
/// the negative receive red.
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
    let settled = settle_on_thread(&reads, &clock, doors::SHUTDOWN_BOUND);
    // The leader is killed but not yet reaped: the count is still raised.
    assert!(
        Deadline::after(SHORT).recv(&settled).is_err(),
        "settle returned before the late child's reap"
    );
    free_reap.send(()).expect("the reap's release");
    assert!(
        Deadline::after(Duration::from_secs(3))
            .recv(&settled)
            .expect("the settle"),
        "settle waits out the reap"
    );
    let error = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the late read")
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(reads.lock().spawning, 0, "the late kill dropped the count");
    assert!(
        fakes::matching_exits(&marker, DEADLINE),
        "the late group is gone"
    );
    watching.stand_down(DEADLINE);
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
    let error = fakes::within("the failed spawn", DEADLINE, {
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
    assert!(
        clock.await_parked(end, DEADLINE),
        "the settle parked at its bound"
    );
    let mark = clock.advance_marked(
        bound
            .checked_sub(Duration::from_nanos(1))
            .expect("the bound holds a nanosecond"),
    );
    assert!(
        clock.await_parked_since(&mark, Some(end), DEADLINE),
        "the settle parked again short of its bound"
    );
    assert!(
        Deadline::after(SHORT).recv(&settled).is_err(),
        "settle answered short of its bound"
    );
    clock.advance(Duration::from_nanos(1));
    assert!(
        !Deadline::after(DEADLINE)
            .recv(&settled)
            .expect("the settle"),
        "settle gives up at its bound"
    );
    reads.cancel();
    release.send(()).expect("the spawn's release");
    let error = fakes::within("the late command", DEADLINE, move || held.join().unwrap())
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(marker, DEADLINE),
        "the late group is gone"
    );
    watching.stand_down(DEADLINE);
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
    assert!(
        clock.await_parked(end, DEADLINE),
        "the settle parked at its bound"
    );
    clock.advance(bound + Duration::from_secs(1));
    assert!(
        !Deadline::after(DEADLINE)
            .recv(&settled)
            .expect("the settle"),
        "the advance woke the settle past its bound"
    );
    reads.cancel();
    release.send(()).expect("the spawn's release");
    let error = fakes::within("the late command", DEADLINE, move || held.join().unwrap())
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(marker, DEADLINE),
        "the late group is gone"
    );
    watching.stand_down(DEADLINE);
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
    assert!(
        clock.await_parked(end, DEADLINE),
        "the settle parked at its bound"
    );
    clock.advance(bound + Duration::from_secs(1));
    assert!(
        !Deadline::after(Duration::from_secs(3)).recv_or_fail(&held_settle, "the held settle"),
        "settle is false at its bound while a spawn is held"
    );
    reads.cancel();
    release.send(()).expect("the spawn's release");
    let error = fakes::within("the late command", DEADLINE, move || held.join().unwrap())
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(marker, DEADLINE),
        "the late group is gone"
    );
    watching.stand_down(DEADLINE);
}
