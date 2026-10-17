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
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use contract::ErrorCode;
use contract::clock::Clock;
use signal_hook::consts::{SIGTERM, SIGUSR1, SIGUSR2};

use super::{CancelAndSettle, CancelOnSignal, FOREGROUND, TERMINAL, guarded};
use crate::switch::Reads;
use fakes::Deadline;
use fakes::children::Ready;
use fakes::clock::FakeClock;
use support::group::GROUP_POLL;

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

/// Advances a settle parked on `fake` past the group-poll park it holds
/// at `poll`, then steps `poll` to the next cadence. The caller holds the
/// settle past its checks, so the park is the settle's own wait past its
/// clock check, and the named instant is the wait it computed.
#[track_caller]
fn drive_poll_park(fake: &FakeClock, poll: &mut Instant, drive: &Deadline) {
    fake.advance_when_parked(Some(*poll), None, GROUP_POLL, drive);
    *poll = poll
        .checked_add(GROUP_POLL)
        .expect("the next poll is countable");
}

/// Waits for `done` while driving a settle parked on `fake` past group-poll
/// parks it holds again after its first one, returning `done`'s value. The
/// caller holds the settle's second pass at its armed pause: reaching the
/// pause proves the first park answered on the bumps alone, with the clock
/// still frozen, so the first advance below covers the second park only.
/// A pass that finishes instead ends the wait at once, so a settle that
/// never parks again drives no advance. The whole wait shares `drive`.
#[track_caller]
fn finish_past_one_park<D>(
    fake: &FakeClock,
    poll: &mut Instant,
    drive: &Deadline,
    done: &mpsc::Receiver<D>,
    done_what: &str,
    parked_again: mpsc::Receiver<()>,
    release_again: mpsc::Sender<()>,
) -> D {
    let mut waiting = true;
    loop {
        if let Ok(value) = done.try_recv() {
            return value;
        }
        if waiting {
            match parked_again.try_recv() {
                Ok(()) => {
                    release_again.send(()).expect("the settle's second release");
                    waiting = false;
                    drive_poll_park(fake, poll, drive);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("the settle left without parking again or finishing")
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        } else if fake.await_parked(*poll, Duration::from_millis(200)) {
            drive_poll_park(fake, poll, drive);
        }
        if drive.left().is_zero() {
            return drive.recv_or_fail(done, done_what);
        }
        std::thread::yield_now();
    }
}

/// Drives a terminal settle's group-poll parks to the guard's drop,
/// ending at once when the drop arrives without another park. The
/// `await_parked` names the settle's poll park, and the thread from the
/// `mark_parked` below names it again for the advance, so the
/// credential's 30 s timeout park held beside it is ignored. The whole
/// drive shares `drive`.
#[track_caller]
fn drive_settle_polls_to_drop(
    fake: &FakeClock,
    poll: &mut Instant,
    drive: &Deadline,
    is_dropped: &mpsc::Receiver<()>,
) {
    loop {
        if is_dropped.try_recv().is_ok() {
            break;
        }
        if fake.await_parked(*poll, Duration::from_millis(200)) {
            if is_dropped.try_recv().is_ok() {
                break;
            }
            let settle = fake
                .mark_parked(*poll, Duration::ZERO)
                .expect("the settle parked at its poll")
                .threads()
                .into_iter()
                .next()
                .expect("the settle's thread");
            fake.advance_when_parked(Some(*poll), Some(settle), GROUP_POLL, drive);
            *poll = poll.checked_add(GROUP_POLL).expect("the poll is countable");
        }
        if drive.left().is_zero() {
            drive.recv_or_fail(is_dropped, "the guard's drop");
            break;
        }
    }
}

/// The watcher reaches its finish only after every cancelled group is
/// empty: a listed sleep group and a held spawn both hold the shared
/// shutdown. Releasing the spawn drains both, and only then does the
/// recorded finish arrive, with the group gone.
#[test]
fn the_watcher_drains_a_listed_group_before_it_finishes() {
    let root = fakes::TempDir::new("fiber-cancel-drains-group");
    let ready = Ready::new(root.path());
    let fake = FakeClock::new();
    let clock: Arc<dyn Clock> = fake.clone();
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    // The listed group first: its ready line arrives after the spawn
    // listed it, so the pause armed next holds only the spawn below.
    let listed = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    // Then a held spawn, so the shutdown parks on the count as well as
    // the group. The marker stays in its command line, so a leaked late
    // child still matches.
    let marker = format!("fiber-cancel-drains-{}", root.path().display());
    let holding = fakes::Watchdog::matching(&marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let late = credential_on_thread(
        &reads,
        sh(&format!("sleep 30 # {marker}")),
        Duration::from_secs(30),
        &clock,
    );
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    let (is_fired, guard) = arm_recording(&reads, &clock, &[SIGTERM]);
    signal_hook::low_level::raise(SIGTERM).expect("the test's raise");
    // Held: the spawn is still counted, so the shared shutdown parks
    // instead of finishing: the park proves it is held while the spawn
    // remains held. The cancel can kill and retire the listed group
    // before the settlement checks it; with only the held spawn left,
    // the settlement parks at its end instead of its poll. Either park
    // proves it is held.
    let mut poll = fake.origin() + GROUP_POLL;
    let end = fake.origin() + doors::SHUTDOWN_BOUND;
    let held = Deadline::after(DEADLINE);
    loop {
        if fake.parked().contains(&Some(poll)) || fake.parked().contains(&Some(end)) {
            break;
        }
        assert!(
            !held.left().is_zero(),
            "the shutdown parked while the spawn is held"
        );
        // A short ack of either park; expiry re-checks both under the
        // shared deadline instead of pinning one.
        if fake.await_parked(poll, held.left().min(Duration::from_millis(200)))
            || fake.await_parked(end, held.left().min(Duration::from_millis(200)))
        {
            break;
        }
    }
    release.send(()).expect("the spawn's release");
    // The shutdown re-parks at its poll until every group is empty: each
    // advance follows a confirmed park, and a park that never comes means
    // the finish already arrived. The whole drive shares one deadline.
    let drive = Deadline::after(DEADLINE);
    let signal = loop {
        match is_fired.try_recv() {
            Ok(signal) => break signal,
            Err(mpsc::TryRecvError::Disconnected) => {
                panic!("the watcher ended without a finish")
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if fake.await_parked(poll, Duration::from_millis(200)) {
            fake.advance(GROUP_POLL);
            poll += GROUP_POLL;
        } else if drive.left().is_zero() {
            break drive.recv_or_fail(&is_fired, "the watcher's finish");
        }
    };
    assert_eq!(signal, SIGTERM);
    assert!(
        fakes::group_empties(pgid, DEADLINE),
        "the finish came after the group drained"
    );
    assert!(
        fakes::matching_exits(&marker, DEADLINE),
        "the late group is gone"
    );
    let error = Deadline::after(DEADLINE)
        .recv(&late)
        .expect("the late read")
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    let _listed = fakes::within("the killed command", DEADLINE, move || {
        listed.join().unwrap()
    });
    // The guard's drop joins the watcher: bounded on its own thread, so a
    // hung settle fails here instead of hanging the binary.
    fakes::within("the guard's drop", DEADLINE, move || {
        drop(guard);
    });
    watchdog.stand_down(DEADLINE);
    holding.stand_down(DEADLINE);
    pre.stand_down(DEADLINE);
}

/// Repeat signals and the normal-return guard share the first shutdown
/// end: the second signal earns no fresh bound, and the dropped guard
/// settles on the spent end without parking. The held spawn keeps every
/// wait parked until the test releases it.
#[test]
fn repeat_signals_and_the_return_guard_share_the_first_end() {
    let fake = FakeClock::new();
    let clock: Arc<dyn Clock> = fake.clone();
    let end = fake.origin() + doors::SHUTDOWN_BOUND;
    let reads = Arc::new(Reads::default());
    // The marker stays in the held shell's command line, so a leaked late
    // child still matches: the watchdog reaps the group on every path,
    // including a failed assertion before the release below.
    let root = fakes::TempDir::new("fiber-cancel-shared-end");
    let marker = format!("fiber-cancel-shared-end-{}", root.path().display());
    let holding = fakes::Watchdog::matching(&marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    let late = credential_on_thread(
        &reads,
        sh(&format!("sleep 30 # {marker}")),
        Duration::from_secs(30),
        &clock,
    );
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    let (is_fired, guard) = arm_recording(&reads, &clock, &[SIGTERM]);
    signal_hook::low_level::raise(SIGTERM).expect("the test's raise");
    // The exact park pins the stored end: no fresh bound starts here.
    assert!(
        fake.await_parked(end, DEADLINE),
        "the first shutdown parked at its end"
    );
    // Raised while the first shutdown still parks, so it queues behind
    // it; every finish below carries the same number either way.
    signal_hook::low_level::raise(SIGTERM).expect("the test's second raise");
    // Past the first end both shutdowns answer `false`.
    fake.advance(doors::SHUTDOWN_BOUND + Duration::from_secs(1));
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&is_fired)
            .expect("the first finish"),
        SIGTERM
    );
    // On the short bound: a fresh end would park this past every advance
    // already made, so an arrival proves the repeat reused the spent one.
    assert_eq!(
        Deadline::after(SHORT)
            .recv(&is_fired)
            .expect("the second finish"),
        SIGTERM
    );
    // The normal-return guard drops onto the same spent end: it answers at
    // once instead of parking, so the bounded drop below would fail it.
    let plain = CancelAndSettle(Arc::clone(&reads), Arc::clone(&clock));
    fakes::within("the return guard's drop", DEADLINE, move || {
        drop(plain);
    });
    release.send(()).expect("the spawn's release");
    let error = Deadline::after(DEADLINE)
        .recv(&late)
        .expect("the late read")
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(&marker, DEADLINE),
        "the late group is gone"
    );
    fakes::within("the guard's drop", DEADLINE, move || {
        drop(guard);
    });
    holding.stand_down(DEADLINE);
}
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
    let fake = FakeClock::new();
    let clock: Arc<dyn Clock> = fake.clone();
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let finished = credential_on_thread(&reads, sh(&script), Duration::from_secs(30), &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    // The settle parks on the `FakeClock`: one pause per pass proves each
    // park the test drives past, so no advance hides the bumps that must
    // wake the first park. Both completion notes land after their bump, so
    // draining them after the pause below proves the bumps came after the
    // captured generation.
    let (parked, is_parked) = mpsc::channel::<()>();
    let (resume, held) = mpsc::channel::<()>();
    reads.pause_at_settle_park(parked, held);
    let (noted, notes) = mpsc::channel::<()>();
    reads.note_completions(noted);
    let (is_fired, guard) = arm_recording(&reads, &clock, &[SIGTERM]);
    signal_hook::low_level::raise(SIGTERM).expect("the test's raise");
    // The watcher cancels, settles, then finishes: only the finish is
    // injected, so its number proves the production calls ran before it.
    // The finish arrives through `Deadline` in seconds: under the `==`/`!=`
    // mutant the watcher's settle at zero parks on the `FakeClock`, so no
    // finish arrives and this fails instead of hanging the suite. A probe
    // that still sees the group alive after the reap parks the settle
    // again at its poll: the helper drives each such park to the finish.
    Deadline::after(DEADLINE)
        .recv(&is_parked)
        .expect("the settle reached its park");
    let drain = Deadline::after(DEADLINE);
    drain.recv(&notes).expect("the first completion note");
    drain.recv(&notes).expect("the second completion note");
    // Armed while the first pass is still held, so the next pass pauses
    // too: reaching it proves the first park woke on the bumps, with the
    // clock frozen, before any advance below.
    let (parked_again, is_parked_again) = mpsc::channel::<()>();
    let (resume_again, held_again) = mpsc::channel::<()>();
    reads.pause_at_settle_park(parked_again, held_again);
    resume.send(()).expect("the settle's release");
    let drive = Deadline::after(DEADLINE);
    let mut poll = fake
        .origin()
        .checked_add(GROUP_POLL)
        .expect("the poll is countable");
    let fired = finish_past_one_park(
        &fake,
        &mut poll,
        &drive,
        &is_fired,
        "the watcher's finish",
        is_parked_again,
        resume_again,
    );
    // The guard's drop joins the watcher: bounded on its own thread, so a
    // hung settle fails here instead of hanging the binary. `within` never
    // joins its thread.
    fakes::within("the guard's drop", DEADLINE, move || {
        drop(guard);
    });
    assert_eq!(fired, SIGTERM);
    let error = Deadline::after(DEADLINE)
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
    let fake = FakeClock::new();
    let clock: Arc<dyn Clock> = fake.clone();
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
    // The settle's first pass pauses before its park at the shutdown end:
    // released only after the late spawn is freed below, its bump lands
    // after the captured generation, so the first park answers on it alone.
    let (parked, is_parked) = mpsc::channel::<()>();
    let (resume, held) = mpsc::channel::<()>();
    reads.pause_at_settle_park(parked, held);
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
    Deadline::after(DEADLINE)
        .recv(&is_parked)
        .expect("the settle reached its park");
    // Armed while the first pass is still held, so the next pass pauses
    // too: reaching it proves the first park woke on the bump, with the
    // clock frozen, before any advance below. A probe that still sees the
    // group alive after the reap parks the settle again at its poll: the
    // helper drives each such park to the finish.
    let (parked_again, is_parked_again) = mpsc::channel::<()>();
    let (resume_again, held_again) = mpsc::channel::<()>();
    reads.pause_at_settle_park(parked_again, held_again);
    release.send(()).expect("the spawn's release");
    // The late spawn listed its group, killed it, reaped its leader and
    // left its count before the settle resumes: both bumps land after the
    // captured generation, so the first park answers on them alone and no
    // later bump can wake the second park early.
    let late = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the late read");
    resume.send(()).expect("the settle's release");
    let drive = Deadline::after(DEADLINE);
    let mut poll = fake
        .origin()
        .checked_add(GROUP_POLL)
        .expect("the poll is countable");
    assert_eq!(
        finish_past_one_park(
            &fake,
            &mut poll,
            &drive,
            &is_fired,
            "the watcher's finish",
            is_parked_again,
            resume_again,
        ),
        SIGTERM
    );
    let error = late.expect_err("a command spawned after the cancel");
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

/// Owns the release senders of the terminal watcher tests' holds, so one
/// drop releases every hold before the guard drops and joins the watcher.
struct Releases {
    reaper: Mutex<Option<mpsc::Sender<()>>>,
    settle: Mutex<Option<mpsc::Sender<()>>>,
}

impl Releases {
    /// Releases the credential reaper's hold, if it is still armed.
    fn release_reaper(&self) {
        if let Some(release) = self
            .reaper
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            release.send(()).unwrap_or(());
        }
    }

    /// Releases the settle park's hold, if it is still armed.
    fn release_settle(&self) {
        if let Some(release) = self
            .settle
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            release.send(()).unwrap_or(());
        }
    }

    /// Releases every hold still armed.
    fn release_all(&self) {
        self.release_reaper();
        self.release_settle();
    }
}

/// Releases every hold when it drops: declared on the line right after the
/// guard's creation and before any assertion that can fail, it drops before
/// the guard on every path, including a failed assertion, so a hold never
/// outlives the join that waits on it.
struct ReleaseOnDrop(Arc<Releases>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release_all();
    }
}

/// How long the reaper stays held in the slow-drop test: longer than the
/// 3 s bound the guards used to take, shorter than `DEADLINE`.
const HELD_FOR: Duration = Duration::from_secs(4);

#[test]
fn the_terminal_watcher_cancels_and_settles_without_exiting() {
    terminal_watcher_run(None);
}

/// A reaper held past the old 3 s bound still lets the terminal watcher's
/// drop complete: the hold ends on a bounded receive on the wall clock, and
/// the drop must finish within its bound after the release.
#[test]
fn a_reaper_held_past_the_old_bound_still_lets_the_terminal_watcher_drop_complete() {
    terminal_watcher_run(Some(HELD_FOR));
}

/// The reaper's and the reader's bumps before the settle's park still wake
/// it: both bumps are counted before the settle resumes, so no later bump
/// can rescue a park that ignored its generation.
#[test]
fn a_reapers_and_a_readers_bumps_before_the_settles_park_still_wake_it() {
    let root = fakes::TempDir::new("fiber-terminal-watch");
    let ready = Ready::new(root.path());
    let fake = FakeClock::new();
    let clock: Arc<dyn Clock> = fake.clone();
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let releases = Arc::new(Releases {
        reaper: Mutex::new(None),
        settle: Mutex::new(None),
    });
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_reaper(entered, freed);
    *releases
        .reaper
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(release);
    let (noted, notes) = mpsc::channel::<()>();
    reads.note_completions(noted);
    let (parked, is_parked) = mpsc::channel::<()>();
    let (resume, held) = mpsc::channel::<()>();
    reads.pause_at_settle_park(parked, held);
    *releases
        .settle
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(resume);
    let finished = credential_on_thread(&reads, sh(&script), Duration::from_secs(30), &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let guard = CancelOnSignal::watch(Arc::clone(&reads), Arc::clone(&clock), &[SIGTERM])
        .expect("the test's registration");
    let _release = ReleaseOnDrop(Arc::clone(&releases));
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the reaper reached its pause");
    signal_hook::low_level::raise(SIGTERM).expect("the test's raise");
    // The first pass saw the group alive, captured its generation, and
    // sits before its park.
    Deadline::after(DEADLINE)
        .recv(&is_parked)
        .expect("the settle reached its park");
    releases.release_reaper();
    // Both bumps land before the settle resumes: the reaper's and the
    // stdout reader's, each noted after its bump. One deadline drains the
    // one stream; nothing else bumps afterwards.
    let drain = Deadline::after(DEADLINE);
    drain.recv(&notes).expect("the first completion note");
    drain.recv(&notes).expect("the second completion note");
    // The drop joins the watcher: started on its own thread before the
    // release and received within its bound, so a hang fails here instead
    // of hanging the binary. The first park answers on the counted bumps
    // alone: the pause below proves it with the clock frozen, before any
    // advance. A probe that still sees the group alive after the reap parks
    // the settle again at its poll: the helper drives each such park to
    // the drop.
    let (parked_again, is_parked_again) = mpsc::channel::<()>();
    let (resume_again, held_again) = mpsc::channel::<()>();
    reads.pause_at_settle_park(parked_again, held_again);
    let (dropped, is_dropped) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        drop(guard);
        dropped.send(()).unwrap_or(());
    });
    releases.release_settle();
    let drive = Deadline::after(DEADLINE);
    let mut poll = fake
        .origin()
        .checked_add(GROUP_POLL)
        .expect("the poll is countable");
    finish_past_one_park(
        &fake,
        &mut poll,
        &drive,
        &is_dropped,
        "the guard's drop",
        is_parked_again,
        resume_again,
    );
    let error = Deadline::after(DEADLINE)
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

/// The terminal watcher's cancel-settle-drop run, shared by the plain test
/// and the slow-drop test above. With `Some(held_for)` the credential
/// reaper stays held for that long before the test releases it.
fn terminal_watcher_run(held_for: Option<Duration>) {
    let root = fakes::TempDir::new("fiber-terminal-watch");
    let ready = Ready::new(root.path());
    let fake = FakeClock::new();
    let clock: Arc<dyn Clock> = fake.clone();
    let reads = Arc::new(Reads::default());
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    let releases = Arc::new(Releases {
        reaper: Mutex::new(None),
        settle: Mutex::new(None),
    });
    let reaper_entered = held_for.map(|_| {
        let (entered, is_entered) = mpsc::channel::<()>();
        let (release, freed) = mpsc::channel::<()>();
        reads.pause_at_reaper(entered, freed);
        *releases
            .reaper
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(release);
        is_entered
    });
    let finished = credential_on_thread(&reads, sh(&script), Duration::from_secs(30), &clock);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let guard = CancelOnSignal::watch(Arc::clone(&reads), Arc::clone(&clock), &[SIGTERM])
        .expect("the test's registration");
    let _release = ReleaseOnDrop(Arc::clone(&releases));
    if let Some(is_entered) = reaper_entered {
        Deadline::after(DEADLINE)
            .recv(&is_entered)
            .expect("the reaper reached its pause");
    }
    signal_hook::low_level::raise(SIGTERM).expect("the test's raise");
    // The watcher cancels and settles, then keeps watching instead of
    // exiting: survival past the cancel proves no exit, and the
    // cancelled read proves the production calls ran. The spin ends on
    // the refusal, never on the clock.
    fakes::within("the watcher's cancel", DEADLINE, {
        let reads = Arc::clone(&reads);
        move || loop {
            if reads.run(|| ()).is_err() {
                return;
            }
            std::thread::yield_now();
        }
    });
    // A second signal still exits nothing: the process is here to drop
    // the guard.
    signal_hook::low_level::raise(SIGTERM).expect("the test's second raise");
    if held_for.is_some() {
        // The settle parked on its poll while the reaper is held: the
        // leader is killed and unreaped, so the group probes alive and the
        // poll is the pass's wait.
        assert!(
            fake.await_parked(fake.origin() + GROUP_POLL, DEADLINE),
            "the settle parked on the held reaper"
        );
    }
    // A third signal still exits nothing: the process is here to drop
    // the guard.
    signal_hook::low_level::raise(SIGTERM).expect("the test's third raise");
    let bound = DEADLINE;
    // The guard's drop joins the watcher: bounded on its own thread, so a
    // hung settle fails here instead of hanging the binary. A probe that
    // still sees the group alive after the reap parks that settle again at
    // its poll on the `FakeClock`: the loop drives each such park to the
    // drop, and ends at once when the drop arrives without one.
    match held_for {
        Some(held_for) => {
            let drop_by = Deadline::after(bound);
            let (dropped, is_dropped) = mpsc::channel::<()>();
            std::thread::spawn(move || {
                drop(guard);
                dropped.send(()).unwrap_or(());
            });
            // The hold ends on a bounded receive on the wall clock, not a
            // sleep: nothing sends on it, so it must time out.
            let (left_sent, is_left) = mpsc::channel::<Duration>();
            let releases = Arc::clone(&releases);
            std::thread::spawn(move || {
                let (_never, never) = mpsc::channel::<()>();
                assert!(
                    Deadline::after(held_for).recv(&never).is_err(),
                    "the hold ended before {held_for:?}"
                );
                left_sent.send(drop_by.left()).unwrap_or(());
                releases.release_reaper();
            });
            // Armed while the first pass is still parked, so the next pass
            // pauses too: reaching it proves the first park woke on the
            // reaper's bump, with the clock frozen, before any advance
            // below. The clock stays frozen until then, so a lost first
            // wake never reaches the drive and the drop below fails.
            let (parked_again, is_parked_again) = mpsc::channel::<()>();
            let (resume_again, held_again) = mpsc::channel::<()>();
            reads.pause_at_settle_park(parked_again, held_again);
            let mut second = false;
            loop {
                if is_dropped.try_recv().is_ok() {
                    break;
                }
                if is_parked_again.try_recv().is_ok() {
                    resume_again.send(()).expect("the settle's second release");
                    second = true;
                    break;
                }
                if drop_by.left().is_zero() {
                    drop_by.recv_or_fail(&is_dropped, "the guard's drop");
                    break;
                }
                std::thread::yield_now();
            }
            if second {
                let mut poll = fake
                    .origin()
                    .checked_add(GROUP_POLL)
                    .expect("the poll is countable");
                drive_settle_polls_to_drop(&fake, &mut poll, &drop_by, &is_dropped);
            }
            match Deadline::after(DEADLINE).recv(&is_left) {
                Ok(left) => assert!(!left.is_zero(), "waited {bound:?} for the guard's drop"),
                Err(_) => panic!("waited {bound:?} for the guard's drop"),
            }
        }
        None => {
            let (dropped, is_dropped) = mpsc::channel::<()>();
            std::thread::spawn(move || {
                drop(guard);
                dropped.send(()).unwrap_or(());
            });
            let drive = Deadline::after(bound);
            let mut poll = fake
                .origin()
                .checked_add(GROUP_POLL)
                .expect("the poll is countable");
            drive_settle_polls_to_drop(&fake, &mut poll, &drive, &is_dropped);
        }
    }
    let error = Deadline::after(DEADLINE)
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
