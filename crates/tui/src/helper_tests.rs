//! Tests for the helper: the exit, the text on standard input, the deadline
//! on the injected clock, and the group the helper leaves alone.

use std::io;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use contract::clock::Clock;
use fakes::children::Ready;
use fakes::clock::FakeClock;
use fakes::{Deadline, TempDir, Watchdog, group_empties, pid_alive, pids_exit};

use super::{Ended, run_bounded, spawn_detached};
use crate::paste_image::READ_LIMIT;

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// 1 MiB: over the 64 KiB a pipe holds.
const MIB: usize = 1024 * 1024;

/// A test's directory and the ready FIFO its command writes to.
struct Fixture {
    dir: TempDir,
    ready: Ready,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new("fiber-helper");
        let ready = Ready::new(dir.path());
        Self { dir, ready }
    }

    /// `sh -c` over `script`, whose first line writes the group id: the
    /// command is its group's leader. `{ready}` in the script is the FIFO.
    fn argv(&self, script: &str) -> Vec<String> {
        let ready = self.ready.path().display().to_string();
        let script = script.replace("{ready}", &ready);
        [
            "sh".to_owned(),
            "-c".to_owned(),
            format!("echo $$ > '{ready}'\n{script}"),
        ]
        .to_vec()
    }

    /// The next ready line's first id.
    #[track_caller]
    fn next(&self) -> u32 {
        self.ready
            .wait(Deadline::after(DEADLINE))
            .first()
            .copied()
            .unwrap_or_else(|| panic!("no ready line"))
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// `run_bounded` on a named thread, answering on the channel.
fn start(
    clock: &Arc<FakeClock>,
    argv: Vec<String>,
    stdin: Option<String>,
) -> mpsc::Receiver<io::Result<Ended>> {
    let (tx, rx) = mpsc::channel();
    let clock: Arc<dyn Clock> = Arc::<FakeClock>::clone(clock);
    std::thread::Builder::new()
        .name("helper-test-run".to_owned())
        .spawn(move || {
            tx.send(run_bounded(&argv, stdin, &clock)).unwrap();
        })
        .unwrap();
    rx
}

/// The answer within [`DEADLINE`] of wall time.
#[track_caller]
fn answer(rx: &mpsc::Receiver<io::Result<Ended>>, what: &str) -> io::Result<Ended> {
    match Deadline::after(DEADLINE).recv(rx) {
        Ok(answer) => answer,
        Err(_) => panic!("waited {DEADLINE:?} for {what}"),
    }
}

/// The group emptied, then the watchdog stands down.
#[track_caller]
fn reaped(watchdog: Watchdog, pgid: u32, what: &str) {
    assert!(
        group_empties(pgid, Deadline::after(DEADLINE)),
        "{what} left its group behind"
    );
    watchdog.stand_down(Deadline::after(DEADLINE));
}

/// Waits for the run to park at `end`, then moves the clock to the last
/// millisecond before it and proves the run parked there again, with no
/// answer yet.
#[track_caller]
fn to_the_edge(
    clock: &Arc<FakeClock>,
    rx: &mpsc::Receiver<io::Result<Ended>>,
    end: Instant,
    what: &str,
) {
    assert!(
        clock.await_parked(end, Deadline::after(DEADLINE)),
        "{what}: the run never parked at its deadline"
    );
    let mark = clock.advance_when_parked(
        Some(end),
        None,
        READ_LIMIT - Duration::from_millis(1),
        &Deadline::after(DEADLINE),
    );
    assert!(
        clock.await_parked_since(&mark, Some(end), Deadline::after(DEADLINE)),
        "{what}: the run never parked again before its deadline"
    );
    assert!(rx.try_recv().is_err(), "{what}: ended a millisecond early");
}

/// The last millisecond: the run is parked, so the move is the signal it
/// waits on.
#[track_caller]
fn past_the_edge(clock: &Arc<FakeClock>, end: Instant) {
    clock.advance_when_parked(
        Some(end),
        None,
        Duration::from_millis(1),
        &Deadline::after(DEADLINE),
    );
}

#[test]
fn a_child_that_exits_is_reaped_and_its_status_returned() {
    let test = Fixture::new();
    let clock = FakeClock::new();
    let begin = clock.now();
    let rx = start(&clock, test.argv("exit 3"), None);
    let pgid = test.next();
    let watchdog = Watchdog::group(pgid);
    match answer(&rx, "the exit") {
        Ok(Ended::Exited(status)) => assert_eq!(status.code(), Some(3)),
        other => panic!("expected an exit, got {other:?}"),
    }
    assert_eq!(clock.now(), begin);
    reaped(watchdog, pgid, "the exit");
}

#[test]
fn the_text_reaches_the_command_in_full() {
    let test = Fixture::new();
    let out = test.path().join("copied");
    let script = format!("cat > '{}'", out.display());
    // A pattern that repeats every 251 bytes, so a dropped or repeated
    // chunk shows.
    let big: String = (0..MIB)
        .map(|i| char::from(b'a' + u8::try_from(i % 251 % 26).unwrap()))
        .collect();
    for text in [big, "fn main() {}\n\tdone é".to_owned()] {
        let clock = FakeClock::new();
        let rx = start(&clock, test.argv(&script), Some(text.clone()));
        let pgid = test.next();
        let watchdog = Watchdog::group(pgid);
        match answer(&rx, "the copy") {
            Ok(Ended::Exited(status)) => assert!(status.success()),
            other => panic!("expected an exit, got {other:?}"),
        }
        let copied = std::fs::read_to_string(&out).unwrap();
        assert_eq!(copied.len(), text.len());
        assert!(copied == text, "the command read different text");
        reaped(watchdog, pgid, "the copy");
    }
}

#[test]
fn empty_text_still_ends_the_command_s_input() {
    let test = Fixture::new();
    let out = test.path().join("copied");
    let clock = FakeClock::new();
    let argv = test.argv(&format!("cat > '{}'", out.display()));
    let rx = start(&clock, argv, Some(String::new()));
    let pgid = test.next();
    let watchdog = Watchdog::group(pgid);
    match answer(&rx, "the empty copy") {
        Ok(Ended::Exited(status)) => assert!(status.success()),
        other => panic!("expected an exit, got {other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "");
    reaped(watchdog, pgid, "the empty copy");
}

#[test]
fn a_hung_child_is_killed_at_the_deadline_and_its_group_is_left_alone() {
    let test = Fixture::new();
    let block = test.path().join("block.fifo");
    fakes::fifo(&block);
    let clock = FakeClock::new();
    let end = clock.now() + READ_LIMIT;
    // The member of the group answers on the ready FIFO once released.
    let script = format!(
        "( read -r _ < '{}'; echo 7 > '{{ready}}' ) &\necho $! > '{{ready}}'\nwait",
        block.display()
    );
    let rx = start(&clock, test.argv(&script), None);
    let pgid = test.next();
    let member = test.next();
    let watchdog = Watchdog::group(pgid);
    to_the_edge(&clock, &rx, end, "the hung child");
    // A millisecond before the deadline the child still runs.
    assert!(pid_alive(pgid));
    past_the_edge(&clock, end);
    match answer(&rx, "the deadline") {
        Ok(Ended::DeadlinePassed) => {}
        other => panic!("expected the deadline, got {other:?}"),
    }
    // The leader was killed and reaped. The member was not signalled: it
    // still runs and answers when released. A killed member would leave
    // the release without a reader, which fails at the wall deadline.
    assert!(pids_exit(&[pgid], Deadline::after(DEADLINE)));
    assert!(pid_alive(member), "the member of the group was killed");
    release(&block);
    assert_eq!(test.next(), 7, "the member of the group did not answer");
    fakes::kill_group(pgid, "KILL").unwrap();
    reaped(watchdog, pgid, "the hung child");
}

#[test]
fn a_child_that_never_reads_stdin_still_ends_at_the_deadline() {
    let test = Fixture::new();
    let clock = FakeClock::new();
    let end = clock.now() + READ_LIMIT;
    let rx = start(&clock, test.argv("exec sleep 3600"), Some("x".repeat(MIB)));
    let pgid = test.next();
    let watchdog = Watchdog::group(pgid);
    to_the_edge(&clock, &rx, end, "the child that never reads");
    past_the_edge(&clock, end);
    match answer(&rx, "the deadline") {
        Ok(Ended::DeadlinePassed) => {}
        other => panic!("expected the deadline, got {other:?}"),
    }
    reaped(watchdog, pgid, "the child that never reads");
}

#[test]
fn a_descendant_holding_stdin_does_not_hold_the_helper() {
    let test = Fixture::new();
    let block = test.path().join("block.fifo");
    fakes::fifo(&block);
    let clock = FakeClock::new();
    let end = clock.now() + READ_LIMIT;
    let script = format!(
        "( read -r _ < '{}'; echo 7 > '{{ready}}' ) <&0 &\necho $! > '{{ready}}'\nexec sleep 3600",
        block.display()
    );
    let rx = start(&clock, test.argv(&script), Some("x".repeat(MIB)));
    let pgid = test.next();
    let descendant = test.next();
    let watchdog = Watchdog::group(pgid);
    to_the_edge(&clock, &rx, end, "the descendant");
    past_the_edge(&clock, end);
    match answer(&rx, "the deadline") {
        Ok(Ended::DeadlinePassed) => {}
        other => panic!("expected the deadline, got {other:?}"),
    }
    // The helper neither waited on the descendant nor killed it: it still
    // answers when released.
    assert!(pid_alive(descendant));
    release(&block);
    assert_eq!(test.next(), 7, "the descendant did not answer");
    fakes::kill_group(pgid, "KILL").unwrap();
    reaped(watchdog, pgid, "the descendant");
}

#[test]
fn a_child_that_exits_without_reading_ends_the_write() {
    let test = Fixture::new();
    let clock = FakeClock::new();
    let begin = clock.now();
    let rx = start(&clock, test.argv("exit 0"), Some("x".repeat(MIB)));
    let pgid = test.next();
    let watchdog = Watchdog::group(pgid);
    match answer(&rx, "the exit") {
        Ok(Ended::Exited(status)) => assert!(status.success()),
        other => panic!("expected an exit, got {other:?}"),
    }
    assert_eq!(clock.now(), begin);
    reaped(watchdog, pgid, "the exit");
}

#[test]
fn a_child_that_closes_stdin_and_keeps_running_ends_when_it_exits() {
    let test = Fixture::new();
    let block = test.path().join("block.fifo");
    fakes::fifo(&block);
    let clock = FakeClock::new();
    let end = clock.now() + READ_LIMIT;
    let script = format!(
        "exec 0<&-\necho 1 > '{{ready}}'\nread -r _ < '{}'",
        block.display()
    );
    let rx = start(&clock, test.argv(&script), Some("x".repeat(MIB)));
    let pgid = test.next();
    let watchdog = Watchdog::group(pgid);
    // The write met a closed pipe; the helper parks without it.
    test.next();
    assert!(clock.await_parked(end, Deadline::after(DEADLINE)));
    release(&block);
    match answer(&rx, "the exit") {
        Ok(Ended::Exited(status)) => assert!(status.success()),
        other => panic!("expected an exit, got {other:?}"),
    }
    reaped(watchdog, pgid, "the closed stdin");
}

/// Writes a line to the FIFO `block`, under the wall-clock deadline.
#[track_caller]
fn release(block: &Path) {
    let block = block.to_path_buf();
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        done.send(std::fs::write(&block, "go\n")).unwrap();
    });
    match Deadline::after(DEADLINE).recv(&finished) {
        Ok(written) => written.unwrap(),
        Err(_) => panic!("waited {DEADLINE:?} for the command to open its FIFO"),
    }
}

#[test]
fn a_child_exit_wakes_the_parked_wait_without_the_clock_moving() {
    let test = Fixture::new();
    let block = test.path().join("block.fifo");
    fakes::fifo(&block);
    let clock = FakeClock::new();
    let begin = clock.now();
    let end = begin + READ_LIMIT;
    let script = format!("read -r _ < '{}'\nexit 0", block.display());
    let rx = start(&clock, test.argv(&script), None);
    let pgid = test.next();
    let watchdog = Watchdog::group(pgid);
    assert!(clock.await_parked(end, Deadline::after(DEADLINE)));
    release(&block);
    match answer(&rx, "the exit") {
        Ok(Ended::Exited(status)) => assert!(status.success()),
        other => panic!("expected an exit, got {other:?}"),
    }
    assert_eq!(clock.now(), begin);
    reaped(watchdog, pgid, "the exit");
}

#[test]
fn a_missing_or_empty_command_is_an_error_and_spawn_detached_returns_at_once() {
    let clock = FakeClock::new();
    let dyn_clock: Arc<dyn Clock> = Arc::<FakeClock>::clone(&clock);
    for argv in [
        vec!["/nonexistent/fiber-copy".to_owned()],
        Vec::new(),
        vec!["/".to_owned()],
    ] {
        assert!(run_bounded(&argv, Some("x".to_owned()), &dyn_clock).is_err());
    }
    let test = Fixture::new();
    spawn_detached(test.argv("sleep 3600"), None, dyn_clock)
        .unwrap_or_else(|err| panic!("spawn_detached: {err}"));
    let pgid = test.next();
    let watchdog = Watchdog::group(pgid);
    // spawn_detached returned while the command still runs.
    assert!(pid_alive(pgid));
    fakes::kill_group(pgid, "KILL").unwrap();
    reaped(watchdog, pgid, "the detached command");
}
