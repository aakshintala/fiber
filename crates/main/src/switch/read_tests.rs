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
    assert!(
        matches!(
            Deadline::after(Duration::from_millis(100)).recv(&finished),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "the wait answered before its timeout"
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
    assert!(
        matches!(
            Deadline::after(Duration::from_millis(100)).recv(&finished),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "the wait answered before its timeout"
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
    guard.stand_down(DEADLINE);
}

/// A finish that lands while the wait is checking is not missed: the state
/// lock holds the wait between its checks while the helper finishes, so
/// both the stdout reader's and the leader's wakes land between the wait's
/// checks and its park. The generation captured first ends that park at
/// once; read after the checks, the park would miss them and hang on the
/// frozen clock until this test's deadline.
#[test]
fn a_completion_between_the_checks_and_the_park_is_not_missed() {
    let root = fakes::TempDir::new("fiber-read-credential-interleave");
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
    // Both wakes land while the wait is held here: the park below already
    // has them in its generation, so it must return at once. Each thread
    // bumps after its send, so two bumps mean both answers are in.
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
    }
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
#[test]
fn a_leader_that_closes_stdout_before_it_exits_answers_without_the_clock_moving() {
    let root = fakes::TempDir::new("fiber-read-credential-cleanup");
    let ready = Ready::new(root.path());
    let clock = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!(
        "echo $$ > '{}'; printf key; exec >&-; exec sleep 1",
        ready.path().display()
    );
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), Duration::from_secs(30), &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
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
