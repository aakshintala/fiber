//! `CancelOnSignal`: the lists it watches, the cancel-settle-finish order,
//! the closed failure, and the drop that joins its thread.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::ffi::c_int;
use std::process::{Command, Stdio};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use signal_hook::consts::{SIGTERM, SIGUSR1, SIGUSR2};

use super::{CancelOnSignal, FOREGROUND, TERMINAL, guarded};
use crate::switch::Reads;
use fakes::Deadline;
use fakes::children::Ready;
use fakes::clock::FakeClock;

/// The deadline of each wait in these tests.
const DEADLINE: Duration = Duration::from_secs(10);
/// The short bound of each negative receive: a watcher without its settle
/// finishes in milliseconds, so silence for this long proves it is held.
const SHORT: Duration = Duration::from_secs(2);
/// One probe of a signal the watcher must ignore: three in a row prove it.
const PROBE: Duration = Duration::from_millis(700);

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

/// Runs `reads.credential` for `command` on a thread of its own, returning
/// the channel its result arrives on. The test receives through
/// [`Deadline`], never a raw `recv_timeout`.
fn credential_on_thread(
    reads: &Arc<Reads>,
    command: Command,
    timeout: Duration,
    clock: &Arc<dyn Clock>,
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

/// Arms `arm` with a recorder as its finish: the channel the signal number
/// arrives on, and the guard. The recorder is the only injected part.
fn arm_recording(
    reads: &Arc<Reads>,
    clock: &Arc<dyn Clock>,
    signals: &[c_int],
) -> (mpsc::Receiver<c_int>, super::CancelOnSignal) {
    let (fired, is_fired) = mpsc::channel();
    let guard = CancelOnSignal::arm(
        Arc::clone(reads),
        Arc::clone(clock),
        signals,
        Box::new(move |signal| {
            fired.send(signal).unwrap_or(());
        }),
    )
    .expect("the test's registration");
    (is_fired, guard)
}

#[test]
fn the_lists_are_exactly_the_foreground_and_terminal_signals() {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT};
    assert_eq!(FOREGROUND, [SIGINT, SIGTERM, SIGHUP]);
    assert_eq!(TERMINAL, [SIGTERM, SIGHUP]);
    assert!(
        !FOREGROUND.contains(&SIGQUIT),
        "no list watches SIGQUIT: {FOREGROUND:?}"
    );
    assert!(
        !TERMINAL.contains(&SIGQUIT),
        "no list watches SIGQUIT: {TERMINAL:?}"
    );
    assert!(
        !TERMINAL.contains(&SIGINT),
        "the terminal keeps its SIGINT: {TERMINAL:?}"
    );
}

#[test]
fn a_registered_signal_cancels_settles_and_then_finishes_with_its_number() {
    let root = fakes::TempDir::new("fiber-cancel-on-signal");
    let ready = Ready::new(root.path());
    let clock: Arc<dyn Clock> = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), Duration::from_secs(30), &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let (is_fired, guard) = arm_recording(&reads, &clock, &[SIGTERM]);
    signal_hook::low_level::raise(SIGTERM).expect("the test's raise");
    // The watcher cancels, settles, then finishes: only the finish is
    // injected, so its number proves the production calls ran before it.
    // The finish arrives through `Deadline` in seconds: under the `==`/`!=`
    // mutant the watcher's settle at zero parks on the `FakeClock`, so no
    // finish arrives and this fails instead of hanging the suite.
    let fired = Deadline::after(Duration::from_secs(3)).recv(&is_fired);
    // The guard's drop joins the watcher: bounded on its own thread, so a
    // hung settle fails here instead of hanging the binary. `within` never
    // joins its thread.
    fakes::within("the guard's drop", Duration::from_secs(3), move || {
        drop(guard);
    });
    assert_eq!(fired.expect("the watcher's finish"), SIGTERM);
    let error = Deadline::after(Duration::from_secs(3))
        .recv(&finished)
        .expect("the cancelled read")
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
fn the_watcher_settles_an_in_flight_spawn_before_it_finishes() {
    let root = fakes::TempDir::new("fiber-cancel-on-signal-spawn");
    let marker = format!("fiber-cancel-settle-{}", root.path().display());
    let clock: Arc<dyn Clock> = FakeClock::new();
    let reads = Arc::new(Reads::default());
    // The marker stays in the late shell's command line, so a leaked late
    // child still matches.
    let script = format!("sleep 30 # {marker}");
    let watching = fakes::Watchdog::matching(&marker);
    // The spawn blocks before it spawns; the release below lets it run.
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let finished = credential_on_thread(&reads, sh(&script), Duration::from_secs(30), &clock);
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    let (is_fired, _guard) = arm_recording(&reads, &clock, &[SIGTERM]);
    signal_hook::low_level::raise(SIGTERM).expect("the test's raise");
    // The watcher took the signal once the cancel landed: a `run` past a
    // cancel refuses without starting its read. The spin ends on the
    // refusal, never on the clock.
    fakes::within("the watcher's cancel", DEADLINE, {
        let reads = Arc::clone(&reads);
        move || loop {
            if reads.run(|| ()).is_err() {
                return;
            }
            std::thread::yield_now();
        }
    });
    // Held: the spawn is still counted, so the settle the watcher runs
    // cannot finish yet.
    assert!(
        Deadline::after(SHORT).recv(&is_fired).is_err(),
        "the watcher finished before the spawn settled"
    );
    release.send(()).expect("the spawn's release");
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&is_fired)
            .expect("the watcher's finish"),
        SIGTERM
    );
    let error = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the late read")
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(&marker, DEADLINE),
        "the late group is gone"
    );
    watching.stand_down(DEADLINE);
}

#[test]
fn the_guarded_helper_returns_the_registration_error_and_builds_nothing() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let failure = guarded(&reads, &clock, &FOREGROUND, |_, _, _| {
        Err(std::io::Error::other("refused"))
    });
    let Err(failure) = failure else {
        panic!("a refused registration fails closed")
    };
    assert_eq!(failure.code, ErrorCode::IoFailed);
    assert!(
        failure.message.starts_with("signals: "),
        "the message starts `signals: `: {}",
        failure.message
    );
    // No `Reads` method ran: a later run still answers.
    assert_eq!(reads.run(|| 7).expect("a run past a refused guard"), 7);
}

#[test]
fn a_signal_not_in_the_list_does_not_fire() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let (is_fired, _guard) = arm_recording(&reads, &clock, &[SIGUSR1]);
    // The test's own registration for the unrelated signal: raising it
    // cannot kill the process, and the watcher's list never holds it.
    let _unrelated =
        signal_hook::iterator::Signals::new([SIGUSR2]).expect("the test's registration");
    signal_hook::low_level::raise(SIGUSR2).expect("the test's raise");
    for _ in 0..3 {
        assert!(
            Deadline::after(PROBE).recv(&is_fired).is_err(),
            "the watcher fired on a signal outside its list"
        );
    }
    signal_hook::low_level::raise(SIGUSR1).expect("the test's raise");
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&is_fired)
            .expect("the watcher's finish"),
        SIGUSR1
    );
}

#[test]
fn dropping_the_guard_joins_its_thread_and_unregisters() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let (is_fired, guard) = arm_recording(&reads, &clock, &[SIGUSR1]);
    // The drop runs on a thread of its own: its return arrives through
    // `Deadline`, so a join that hung would fail here instead of hanging
    // the test binary.
    let (dropped, is_dropped) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        drop(guard);
        dropped.send(()).unwrap_or(());
    });
    Deadline::after(DEADLINE)
        .recv(&is_dropped)
        .expect("the guard's drop");
    // No signal was ever sent: the recorder heard nothing.
    assert!(
        is_fired.try_recv().is_err(),
        "the dropped guard finished without a signal"
    );
}

#[test]
fn a_dropped_guard_leaves_a_later_signal_unhandled_by_the_callback() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let reads = Arc::new(Reads::default());
    let (is_fired, guard) = arm_recording(&reads, &clock, &[SIGUSR1]);
    // The drop runs on a thread of its own: its return arrives through
    // `Deadline`, so a join that hung would fail here instead of hanging
    // the test binary.
    let (dropped, is_dropped) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        drop(guard);
        dropped.send(()).unwrap_or(());
    });
    Deadline::after(DEADLINE)
        .recv(&is_dropped)
        .expect("the guard's drop");
    // SIGUSR1 has no other handler in this test, so the test registers its
    // own before raising: the raise reaches that registration, never the
    // dropped guard's callback, and the process survives.
    let _own = signal_hook::iterator::Signals::new([SIGUSR1]).expect("the test's registration");
    signal_hook::low_level::raise(SIGUSR1).expect("the test's raise");
    assert!(
        Deadline::after(SHORT).recv(&is_fired).is_err(),
        "a dropped guard answered a later signal"
    );
}
