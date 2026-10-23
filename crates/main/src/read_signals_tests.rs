//! `CancelOnSignal`: the lists it watches, the cancel-settle-finish order,
//! the closed failure, and the drop that joins its thread.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::ffi::c_int;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

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

/// Waits for `done` while driving a settle parked on `fake` past group-poll
/// parks it holds again after its first one, returning `done`'s value. The
/// caller holds the settle's second pass at its armed pause: reaching the
/// pause proves the first park answered on the bumps alone, with the clock
/// still frozen, so the first advance below covers the second park only.
/// A pass that finishes instead ends the wait at once, so a settle that
/// never parks again drives no advance. The whole wait shares `drive`.
/// No clock call happens before the second pause arrives; every advance
/// after that names the one parked thread and the poll instant it holds.
#[track_caller]
fn finish_past_one_park<D>(
    fake: &FakeClock,
    drive: &Deadline,
    done: &mpsc::Receiver<D>,
    done_what: &str,
    parked_again: mpsc::Receiver<()>,
    release_again: mpsc::Sender<()>,
) -> D {
    let mut poll = fake
        .now()
        .checked_add(GROUP_POLL)
        .expect("the poll is countable");
    loop {
        if let Ok(value) = done.try_recv() {
            return value;
        }
        match parked_again.try_recv() {
            Ok(()) => {
                release_again.send(()).expect("the settle's second release");
                break;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                panic!("the settle left without parking again or finishing")
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if drive.left().is_zero() {
            return drive.recv_or_fail(done, done_what);
        }
        std::thread::yield_now();
    }
    loop {
        if let Ok(value) = done.try_recv() {
            return value;
        }
        // The caller drained every bump before releasing the first pass,
        // and each advance wakes the one waiter once, so a marked park is
        // still held when the advance looks: only this advance can end it.
        if let Some(mark) = fake.mark_parked(
            poll,
            Deadline::after(drive.left().min(Duration::from_millis(200))),
        ) {
            let threads = mark.threads();
            assert!(
                threads.len() == 1,
                "one thread parks at the poll: {threads:?}"
            );
            let thread = threads.into_iter().next().expect("the one parked thread");
            fake.advance_when_parked(Some(poll), Some(thread), GROUP_POLL, drive);
            poll = poll.checked_add(GROUP_POLL).expect("the poll is countable");
        }
        if drive.left().is_zero() {
            return drive.recv_or_fail(done, done_what);
        }
        std::thread::yield_now();
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
    let releases = Arc::new(Releases {
        spawn: Mutex::new(None),
        reaper: Mutex::new(None),
        settle: Mutex::new(None),
    });
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    // The listed spawn's pause is armed before its thread starts, so its
    // report arrives after the spawn listed its group: receiving it orders
    // the listing bump before everything the test does next.
    let (listed_entered, is_listed_entered) = mpsc::channel::<()>();
    let (listed_release, listed_freed) = mpsc::channel::<()>();
    reads.pause_at_listed(listed_entered, listed_freed);
    let listed = {
        let reads = Arc::clone(&reads);
        std::thread::spawn(move || reads.command(&mut sh(&script)))
    };
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    Deadline::after(DEADLINE)
        .recv(&is_listed_entered)
        .expect("the listed spawn listed its group");
    listed_release.send(()).expect("the listed spawn's release");
    // Then a held spawn, so the shutdown parks on the count as well as
    // the group. The marker stays in its command line, so a leaked late
    // child still matches.
    let marker = format!("fiber-cancel-drains-{}", root.path().display());
    let holding = fakes::Watchdog::matching(&marker);
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    *releases
        .spawn
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(release);
    let late = credential_on_thread(
        &reads,
        sh(&format!("sleep 30 # {marker}")),
        Duration::from_secs(30),
        &clock,
    );
    Deadline::after(DEADLINE)
        .recv(&is_entered)
        .expect("the spawn reached its pause");
    // The settle's first pass pauses before its park: with the spawn still
    // counted the pass cannot finish, so reaching the pause proves the
    // shutdown is held while the spawn stays held.
    let (parked, is_parked) = mpsc::channel::<()>();
    let (resume, held) = mpsc::channel::<()>();
    reads.pause_at_settle_park(parked, held);
    *releases
        .settle
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(resume);
    let (is_fired, guard) = arm_recording(&reads, &clock, &[SIGTERM]);
    let _release = ReleaseOnDrop(Arc::clone(&releases));
    signal_hook::low_level::raise(SIGTERM).expect("the test's raise");
    Deadline::after(DEADLINE)
        .recv(&is_parked)
        .expect("the settle reached its park");
    // Armed while the first pass is still held, so the next pass pauses
    // too: reaching it proves the first park woke on the bumps, with the
    // clock frozen, before any advance below.
    let (parked_again, is_parked_again) = mpsc::channel::<()>();
    let (resume_again, held_again) = mpsc::channel::<()>();
    reads.pause_at_settle_park(parked_again, held_again);
    releases.release_spawn();
    // The late spawn listed its group, killed it, reaped its leader and
    // left its count before the settle resumes: its bumps land after the
    // captured generation, so the first park answers on them alone.
    let late = Deadline::after(DEADLINE)
        .recv(&late)
        .expect("the late read");
    releases.release_settle();
    let drive = Deadline::after(DEADLINE);
    let signal = finish_past_one_park(
        &fake,
        &drive,
        &is_fired,
        "the watcher's finish",
        is_parked_again,
        resume_again,
    );
    assert_eq!(signal, SIGTERM);
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the finish came after the group drained"
    );
    assert!(
        fakes::matching_exits(&marker, fakes::Deadline::after(DEADLINE)),
        "the late group is gone"
    );
    let error = late.expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    let _listed = fakes::within("the killed command", Deadline::after(DEADLINE), move || {
        listed.join().unwrap()
    });
    // The guard's drop joins the watcher: bounded on its own thread, so a
    // hung settle fails here instead of hanging the binary.
    fakes::within("the guard's drop", Deadline::after(DEADLINE), move || {
        drop(guard);
    });
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    holding.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
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
    let watcher = guard
        .thread
        .as_ref()
        .expect("the signal watcher")
        .thread()
        .id();
    signal_hook::low_level::raise(SIGTERM).expect("the test's raise");
    // The exact park pins the stored end: no fresh bound starts here.
    assert!(
        fake.await_parked(end, fakes::Deadline::after(DEADLINE)),
        "the first shutdown parked at its end"
    );
    // Raised while the first shutdown still parks, so it queues behind
    // it; every finish below carries the same number either way.
    signal_hook::low_level::raise(SIGTERM).expect("the test's second raise");
    // At the first end both shutdowns answer `false`.
    fake.advance_when_parked(
        Some(end),
        Some(watcher),
        doors::SHUTDOWN_BOUND,
        &Deadline::after(DEADLINE),
    );
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&is_fired)
            .expect("the first finish"),
        SIGTERM
    );
    // A fresh end would park this past every advance already made, so an
    // arrival proves the repeat reused the spent one.
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&is_fired)
            .expect("the second finish"),
        SIGTERM
    );
    // The normal-return guard drops onto the same spent end: it answers at
    // once instead of parking, so the bounded drop below would fail it.
    let plain = CancelAndSettle(Arc::clone(&reads), Arc::clone(&clock));
    fakes::within(
        "the return guard's drop",
        Deadline::after(DEADLINE),
        move || {
            drop(plain);
        },
    );
    release.send(()).expect("the spawn's release");
    let error = Deadline::after(DEADLINE)
        .recv(&late)
        .expect("the late read")
        .expect_err("a command spawned after the cancel");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(
        fakes::matching_exits(&marker, fakes::Deadline::after(DEADLINE)),
        "the late group is gone"
    );
    fakes::within("the guard's drop", Deadline::after(DEADLINE), move || {
        drop(guard);
    });
    holding.stand_down(fakes::Deadline::after(DEADLINE));
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
    let releases = Arc::new(Releases {
        spawn: Mutex::new(None),
        reaper: Mutex::new(None),
        settle: Mutex::new(None),
    });
    let script = format!("echo $$ > '{}'; exec sleep 600", ready.path().display());
    // Before the spawn: the group is known only after it starts, so a
    // command-line watchdog covers a spawn no later watchdog can reach.
    let pre = fakes::Watchdog::matching(&ready.path().display().to_string());
    // Every registration comes before the thread that emits its event:
    // the reaper hold keeps the leader unreaped, so the first settle pass
    // still sees the group alive and reaches its pause below.
    let (reaper_entered, is_reaper_entered) = mpsc::channel::<()>();
    let (reaper_release, reaper_freed) = mpsc::channel::<()>();
    reads.pause_at_reaper(reaper_entered, reaper_freed);
    *releases
        .reaper
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(reaper_release);
    // The settle parks on the `FakeClock`: one pause per pass proves each
    // park the test drives past, so no advance hides the bumps that must
    // wake the first park. Both completion notes land after their bump, so
    // draining them after the pause below proves the bumps came after the
    // captured generation.
    let (parked, is_parked) = mpsc::channel::<()>();
    let (resume, held) = mpsc::channel::<()>();
    reads.pause_at_settle_park(parked, held);
    *releases
        .settle
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(resume);
    let (noted, notes) = mpsc::channel::<()>();
    reads.note_completions(noted);
    let finished = credential_on_thread(&reads, sh(&script), Duration::from_secs(30), &clock);
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let (is_fired, guard) = arm_recording(&reads, &clock, &[SIGTERM]);
    let _release = ReleaseOnDrop(Arc::clone(&releases));
    Deadline::after(DEADLINE)
        .recv(&is_reaper_entered)
        .expect("the reaper reached its pause");
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
    // Armed while the first pass is still held, so the next pass pauses
    // too: reaching it proves the first park woke on the bumps, with the
    // clock frozen, before any advance below.
    let (parked_again, is_parked_again) = mpsc::channel::<()>();
    let (resume_again, held_again) = mpsc::channel::<()>();
    reads.pause_at_settle_park(parked_again, held_again);
    releases.release_reaper();
    let drain = Deadline::after(DEADLINE);
    drain.recv(&notes).expect("the first completion note");
    drain.recv(&notes).expect("the second completion note");
    releases.release_settle();
    let drive = Deadline::after(DEADLINE);
    let fired = finish_past_one_park(
        &fake,
        &drive,
        &is_fired,
        "the watcher's finish",
        is_parked_again,
        resume_again,
    );
    // The guard's drop joins the watcher: bounded on its own thread, so a
    // hung settle fails here instead of hanging the binary. `within` never
    // joins its thread.
    fakes::within("the guard's drop", Deadline::after(DEADLINE), move || {
        drop(guard);
    });
    assert_eq!(fired, SIGTERM);
    let error = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the cancelled read")
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
fn the_watcher_settles_an_in_flight_spawn_before_it_finishes() {
    let root = fakes::TempDir::new("fiber-cancel-on-signal-spawn");
    let marker = format!("fiber-cancel-settle-{}", root.path().display());
    let fake = FakeClock::new();
    let clock: Arc<dyn Clock> = fake.clone();
    let reads = Arc::new(Reads::default());
    let releases = Arc::new(Releases {
        spawn: Mutex::new(None),
        reaper: Mutex::new(None),
        settle: Mutex::new(None),
    });
    // The marker stays in the late shell's command line, so a leaked late
    // child still matches.
    let script = format!("sleep 30 # {marker}");
    let watching = fakes::Watchdog::matching(&marker);
    // The spawn blocks before it spawns; the release below lets it run.
    // The hold lives in `Releases`, so it drops before the guard's join
    // on every path.
    let (entered, is_entered) = mpsc::channel::<()>();
    let (release, freed) = mpsc::channel::<()>();
    reads.pause_at_spawn(entered, freed);
    *releases
        .spawn
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(release);
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
    *releases
        .settle
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(resume);
    let (is_fired, _guard) = arm_recording(&reads, &clock, &[SIGTERM]);
    let _release = ReleaseOnDrop(Arc::clone(&releases));
    signal_hook::low_level::raise(SIGTERM).expect("the test's raise");
    // The watcher took the signal once the cancel landed: a `run` past a
    // cancel refuses without starting its read. The spin ends on the
    // refusal, never on the clock.
    fakes::within("the watcher's cancel", Deadline::after(DEADLINE), {
        let reads = Arc::clone(&reads);
        move || loop {
            if reads.run("credential read", || ()).is_err() {
                return;
            }
            std::thread::yield_now();
        }
    });
    // Held: the spawn is still counted, so the settle the watcher runs
    // cannot finish yet. The park comes first: reaching the pause proves
    // the pass found work left, so the empty channel proves the watcher
    // did not finish while the spawn stays held.
    Deadline::after(DEADLINE)
        .recv(&is_parked)
        .expect("the settle reached its park (the watcher finished before the spawn settled)");
    assert!(
        matches!(is_fired.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the watcher finished before the spawn settled"
    );
    // Armed while the first pass is still held, so the next pass pauses
    // too: reaching it proves the first park woke on the bump, with the
    // clock frozen, before any advance below. A probe that still sees the
    // group alive after the reap parks the settle again at its poll: the
    // helper drives each such park to the finish.
    let (parked_again, is_parked_again) = mpsc::channel::<()>();
    let (resume_again, held_again) = mpsc::channel::<()>();
    reads.pause_at_settle_park(parked_again, held_again);
    releases.release_spawn();
    // The late spawn listed its group, killed it, reaped its leader and
    // left its count before the settle resumes: both bumps land after the
    // captured generation, so the first park answers on them alone and no
    // later bump can wake the second park early.
    let late = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the late read");
    releases.release_settle();
    let drive = Deadline::after(DEADLINE);
    assert_eq!(
        finish_past_one_park(
            &fake,
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
        fakes::matching_exits(&marker, fakes::Deadline::after(DEADLINE)),
        "the late group is gone"
    );
    watching.stand_down(fakes::Deadline::after(DEADLINE));
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
    assert_eq!(
        reads
            .run("credential read", || 7)
            .expect("a run past a refused guard"),
        7
    );
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

/// Owns the release senders of the watcher tests' holds, so one
/// drop releases every hold before the guard drops and joins the watcher.
struct Releases {
    spawn: Mutex<Option<mpsc::Sender<()>>>,
    reaper: Mutex<Option<mpsc::Sender<()>>>,
    settle: Mutex<Option<mpsc::Sender<()>>>,
}

impl Releases {
    /// Releases the held spawn, if it is still armed.
    fn release_spawn(&self) {
        if let Some(release) = self
            .spawn
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            release.send(()).unwrap_or(());
        }
    }
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
        self.release_spawn();
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

/// How long the reaper stays held in the slow-drop test, in injected time:
/// past the old 3 s bound, short of the shutdown end the settle parks at.
const HELD_FOR: Duration = Duration::from_secs(4);
/// Where the stand-in parks in the slow-drop test, in injected time: an
/// instant the held-run drive never reaches, which ends near the hold.
const STANDIN_PARK: Duration = Duration::from_secs(30);

#[test]
fn the_terminal_watcher_cancels_and_settles_without_exiting() {
    terminal_watcher_run(None);
}

/// A reaper held past the old 3 s bound still lets the terminal watcher's
/// drop complete: the fake clock passes the hold while the reaper stays
/// held, the drop cannot complete meanwhile, and the drop must finish
/// within its bound after the release.
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
        spawn: Mutex::new(None),
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
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
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
    // Armed while the first pass is still held, so the next pass pauses
    // too: reaching it proves the first park woke on the counted bumps,
    // with the clock frozen, before any advance below. A probe that still
    // sees the group alive after the reap parks the settle again at its
    // poll: the helper drives each such park to the drop.
    let (parked_again, is_parked_again) = mpsc::channel::<()>();
    let (resume_again, held_again) = mpsc::channel::<()>();
    reads.pause_at_settle_park(parked_again, held_again);
    releases.release_reaper();
    // Both bumps land before the settle resumes: the reaper's and the
    // stdout reader's, each noted after its bump. One deadline drains the
    // one stream; nothing else bumps afterwards.
    let drain = Deadline::after(DEADLINE);
    drain.recv(&notes).expect("the first completion note");
    drain.recv(&notes).expect("the second completion note");
    // The drop joins the watcher: started on its own thread before the
    // release and received within its bound, so a hang fails here instead
    // of hanging the binary.
    let (dropped, is_dropped) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        drop(guard);
        dropped.send(()).unwrap_or(());
    });
    releases.release_settle();
    let drive = Deadline::after(DEADLINE);
    finish_past_one_park(
        &fake,
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
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the cancel killed the group"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

/// The terminal watcher's cancel-settle-drop run, shared by the plain test
/// and the slow-drop test above. The credential reaper stays held in both
/// runs; `held_for` sets how long the hold lasts before the test releases it.
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
        spawn: Mutex::new(None),
        reaper: Mutex::new(None),
        settle: Mutex::new(None),
    });
    // Every registration comes before the thread that emits its event,
    // in both runs: the reaper hold keeps the leader unreaped, so the
    // first settle pass still sees the group alive and reaches its pause.
    let (reaper_entered, is_reaper_entered) = mpsc::channel::<()>();
    let (reaper_release, reaper_freed) = mpsc::channel::<()>();
    reads.pause_at_reaper(reaper_entered, reaper_freed);
    *releases
        .reaper
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(reaper_release);
    let (noted, notes) = mpsc::channel::<()>();
    reads.note_completions(noted);
    // The first pass pauses before its park: reaching it proves the pass
    // saw the group alive on the held reaper, with its wait computed.
    let (parked, is_parked) = mpsc::channel::<()>();
    let (resume, held) = mpsc::channel::<()>();
    reads.pause_at_settle_park(parked, held);
    *releases
        .settle
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(resume);
    let finished = credential_on_thread(&reads, sh(&script), Duration::from_secs(30), &clock);
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let guard = CancelOnSignal::watch(Arc::clone(&reads), Arc::clone(&clock), &[SIGTERM])
        .expect("the test's registration");
    let _release = ReleaseOnDrop(Arc::clone(&releases));
    Deadline::after(DEADLINE)
        .recv(&is_reaper_entered)
        .expect("the reaper reached its pause");
    signal_hook::low_level::raise(SIGTERM).expect("the test's raise");
    // The watcher cancels and settles, then keeps watching instead of
    // exiting: survival past the cancel proves no exit, and the
    // cancelled read proves the production calls ran. The spin ends on
    // the refusal, never on the clock.
    fakes::within("the watcher's cancel", Deadline::after(DEADLINE), {
        let reads = Arc::clone(&reads);
        move || loop {
            if reads.run("credential read", || ()).is_err() {
                return;
            }
            std::thread::yield_now();
        }
    });
    // A second signal still exits nothing: the process is here to drop
    // the guard.
    signal_hook::low_level::raise(SIGTERM).expect("the test's second raise");
    Deadline::after(DEADLINE)
        .recv(&is_parked)
        .expect("the settle reached its park");
    // A third signal still exits nothing: the process is here to drop
    // the guard.
    signal_hook::low_level::raise(SIGTERM).expect("the test's third raise");
    // The guard's drop joins the watcher: started on its own thread, the
    // test receives it within its bound, so a hang fails here instead of
    // hanging the binary. A probe that still sees the group alive after
    // the reap parks that settle again at its poll on the `FakeClock`:
    // the helper drives each such park to the drop.
    let drop_by = Deadline::after(DEADLINE);
    let (dropped, is_dropped) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        drop(guard);
        dropped.send(()).unwrap_or(());
    });
    if let Some(held_for) = held_for {
        // The reaper stays held while the fake clock passes the old
        // bound one poll at a time: each pass parks at its poll, so each
        // advance lands exactly on the named instant, and the empty
        // channel proves the drop cannot complete while the group stays
        // listed. The hold stays short of the shutdown end, so the settle
        // still parks instead of giving up.
        releases.release_settle();
        // A thread the test owns parks beside the settle at an instant
        // the drive never reaches: with the drive watching every thread
        // it would fail the first advance, as the credential park did in
        // CI. `await_parked` returns only after the park registered, so
        // the stand-in is in place before the drive looks: no race.
        let (standin_stop, standin_halt) = mpsc::channel::<()>();
        let (standin_done, standin_finished) = mpsc::channel::<()>();
        let standin_clock = Arc::clone(&fake);
        let standin = std::thread::spawn(move || {
            standin_clock.wait_until(Some(standin_clock.origin() + STANDIN_PARK), &mut |_| {
                match standin_halt.recv() {
                    Ok(()) | Err(_) => {}
                }
            });
            standin_done.send(()).unwrap_or(());
        });
        assert!(
            fake.await_parked(
                fake.origin() + STANDIN_PARK,
                fakes::Deadline::after(DEADLINE)
            ),
            "the stand-in parked at its instant"
        );
        assert!(
            matches!(is_dropped.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "the guard's drop finished with the reaper held"
        );
        // Every advance names the settle's thread through `mark_parked`,
        // as `finish_past_one_park` does, so the stand-in's park cannot
        // fail the drive.
        let hold_end = fake.origin() + held_for;
        let mut poll = fake.origin() + GROUP_POLL;
        let settle = fake
            .mark_parked(poll, fakes::Deadline::after(DEADLINE))
            .expect("the settle parked at its poll with the reaper held");
        let threads = settle.threads();
        assert!(
            threads.len() == 1,
            "one thread parks at the poll: {threads:?}"
        );
        let thread = threads.into_iter().next().expect("the one parked thread");
        let mut mark = fake.advance_when_parked(
            Some(poll),
            Some(thread),
            GROUP_POLL,
            &Deadline::after(DEADLINE),
        );
        poll = poll.checked_add(GROUP_POLL).expect("the hold is countable");
        while poll <= hold_end {
            mark = fake.advance_when_parked(
                Some(poll),
                Some(thread),
                GROUP_POLL,
                &Deadline::after(DEADLINE),
            );
            poll = poll.checked_add(GROUP_POLL).expect("the hold is countable");
        }
        assert!(
            fake.await_parked_since(
                &mark,
                Some(hold_end + GROUP_POLL),
                fakes::Deadline::after(DEADLINE)
            ),
            "the settle parked again past the old bound"
        );
        assert!(
            matches!(is_dropped.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "the guard's drop finished past the old bound"
        );
        // The drive is done: end the stand-in through its channel, never
        // through the clock, and join it before the settle runs again.
        // The `recv` below bounds the wait on its exit.
        standin_stop.send(()).expect("the stand-in's release");
        Deadline::after(DEADLINE)
            .recv(&standin_finished)
            .expect("the stand-in's exit");
        standin.join().expect("the stand-in's thread");
    }
    // Armed before the reaper's release, so the next pass pauses too:
    // reaching it proves the parked pass woke on the bumps before any
    // advance below. In the held run the clock already passed the hold
    // above; the helper starts its drive from now.
    let (parked_again, is_parked_again) = mpsc::channel::<()>();
    let (resume_again, held_again) = mpsc::channel::<()>();
    reads.pause_at_settle_park(parked_again, held_again);
    releases.release_reaper();
    // Both bumps land before the next pass resumes: the reaper's and the
    // stdout reader's, each noted after its bump. One deadline drains the
    // one stream; nothing else bumps afterwards.
    let drain = Deadline::after(DEADLINE);
    drain.recv(&notes).expect("the first completion note");
    drain.recv(&notes).expect("the second completion note");
    releases.release_settle();
    finish_past_one_park(
        &fake,
        &drop_by,
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
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the cancel killed the group"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}
