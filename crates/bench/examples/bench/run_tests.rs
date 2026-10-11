use std::ffi::OsStr;
use std::path::Path;

use serde_json::json;

use std::process::{Command, Stdio};
use std::time::Duration;

use contract::clock::Clock;
use fakes::children::{Ready, leaves_descendants};
use fakes::{Deadline, TempDir, Watchdog};

use super::{Proc, Startup, System, command, parse_line, run_to_end};

/// The wall-clock bound on each test below that runs a child.
const WALL: Duration = Duration::from_secs(30);

/// How long a ready line from a test's child may take.
const READY: Duration = Duration::from_secs(5);

const PROXIES: [&str; 8] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

#[test]
fn a_child_gets_a_cleared_environment_with_only_the_named_variables() {
    let command = command(
        Path::new("/r/fiber"),
        Path::new("/r"),
        Path::new("/r/h"),
        Some(OsStr::new("/usr/bin")),
    );
    let mut set: Vec<(String, String)> = command
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.unwrap().to_string_lossy().into_owned(),
            )
        })
        .collect();
    set.sort();
    assert_eq!(
        set,
        [
            ("FIBER_HOME", "/r/h"),
            ("FIBER_TEST_FAKE_KEY", "sk-bench"),
            ("HOME", "/r"),
            ("PATH", "/usr/bin"),
        ]
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
    );
    for proxy in PROXIES {
        assert!(
            set.iter().all(|(key, _)| key != proxy),
            "{proxy} reached the child"
        );
    }
    // The inherited environment is cleared, so a proxy variable the
    // harness runs under is not passed on either.
    assert!(
        format!("{command:?}").contains(" env -i "),
        "the environment is not cleared: {command:?}"
    );
}

#[test]
fn a_stdout_line_that_is_not_json_is_an_error() {
    assert!(
        parse_line("panicked at main.rs")
            .unwrap_err()
            .contains("not JSON")
    );
    assert_eq!(
        parse_line(r#"{"kind":"session_started"}"#).unwrap()["kind"],
        "session_started"
    );
}

#[test]
fn startup_finishes_at_the_first_session_status_after_extensions_loaded() {
    let mut startup = Startup::default();
    let lines = [
        ("session_started", false),
        // A status before the extensions are loaded is not the end.
        ("session_status", false),
        ("fiber_started", false),
        ("extensions_loaded", false),
        ("notice", false),
        ("session_status", true),
    ];
    for (kind, done) in lines {
        assert_eq!(startup.line(&json!({"kind": kind})), done, "{kind}");
    }
    assert!(!Startup::default().line(&json!({"no_kind": 1})));
}

#[test]
fn a_startup_timing_starts_at_the_clock_read_just_before_the_spawn() {
    let clock = fakes::clock::FakeClock::new();
    let before = clock.now();
    clock.advance(Duration::from_millis(7));
    let proc = Proc::spawn(&mut Command::new("true"), &*clock).unwrap();
    assert_eq!(proc.spawned, before + Duration::from_millis(7));
    proc.stop(&super::System).unwrap();
}

#[test]
fn a_command_run_to_its_end_returns_its_status_and_output() {
    let finished = fakes::within("a short command", WALL, || {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo out; echo err >&2; exit 3"]);
        run_to_end(&mut command, &System, WALL, "the short command")
    })
    .unwrap();
    assert_eq!(finished.status.code(), Some(3));
    assert_eq!(finished.stdout, "out\n");
    assert_eq!(finished.stderr, "err\n");
}

/// A `perl` that sleeps for an hour with `marker` on its command line; the
/// second form first leaves the process group, keeping stdout open.
fn sleeper(marker: &Path, escapes: bool) -> String {
    let leave = if escapes { "POSIX::setsid(); " } else { "" };
    format!("perl -MPOSIX -e '{leave}sleep 3600' '{}'", marker.display())
}

#[test]
fn a_command_past_its_deadline_errs_and_leaves_nothing_in_its_group() {
    let dir = TempDir::new("fiber-bench-deadline");
    let marker = dir.path().to_string_lossy().into_owned();
    let watchdog = Watchdog::matching(&marker);
    let script = format!(
        "{} & {}",
        sleeper(dir.path(), false),
        sleeper(dir.path(), false)
    );
    let clock = fakes::clock::FakeClock::new();
    let err = fakes::within("a command past its deadline", WALL, move || {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script]);
        run_to_end(
            &mut command,
            &*clock,
            Duration::from_millis(200),
            "the sleeper",
        )
    })
    .unwrap_err();
    // A slow group cleanup under load adds its own sentence after this one.
    assert!(
        err.starts_with("timed out waiting for the sleeper"),
        "{err}"
    );
    assert_eq!(fakes::matching(&marker).unwrap(), Vec::<u32>::new());
    watchdog.stand_down(READY);
}

/// A clock whose first sleep, the poll for the deadline, waits for the
/// child's two ready lines: the deadline cannot fire before the child has
/// set up.
struct AfterReady {
    ready: std::sync::Mutex<Option<Ready>>,
    inner: std::sync::Arc<fakes::clock::FakeClock>,
}

impl Clock for AfterReady {
    fn now(&self) -> std::time::Instant {
        self.inner.now()
    }
    fn wall(&self) -> std::time::SystemTime {
        self.inner.wall()
    }
    #[track_caller]
    fn sleep(&self, d: Duration) {
        let ready = self
            .ready
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(ready) = ready {
            // One deadline for both lines: a scoped thread reads them.
            let (tx, rx) = std::sync::mpsc::channel();
            let deadline = Deadline::after(READY);
            std::thread::scope(|scope| {
                let deadline = &deadline;
                scope.spawn(move || {
                    ready.wait(deadline.left());
                    ready.wait(deadline.left());
                    match tx.send(()) {
                        Ok(()) | Err(_) => {}
                    }
                });
            });
            // Outside the scope closure, which `#[track_caller]` does not cross.
            assert!(deadline.recv(&rx).is_ok(), "the child's two ready lines");
        }
        self.inner.sleep(d);
    }
    fn wait_until(
        &self,
        until: Option<std::time::Instant>,
        wait: &mut dyn FnMut(Option<Duration>),
    ) {
        self.inner.wait_until(until, wait);
    }
    fn subscribe(&self, waker: std::sync::Weak<dyn contract::clock::Wake>) {
        self.inner.subscribe(waker);
    }
}

#[test]
fn a_deadline_error_wins_over_a_group_cleanup_error() {
    let dir = TempDir::new("fiber-bench-deadline-cleanup");
    let ready = Ready::new(dir.path());
    // The leader outlives the deadline; its child ignores SIGTERM, so the
    // group needs SIGKILL and the stop reports a cleanup error too.
    let script = leaves_descendants(ready.path());
    let clock = AfterReady {
        ready: std::sync::Mutex::new(Some(ready)),
        inner: fakes::clock::FakeClock::new(),
    };
    let err = fakes::within("a deadline with a stubborn child", WALL, move || {
        let mut command = Command::new("/bin/bash");
        command.args(["-c", &script]);
        run_to_end(
            &mut command,
            &clock,
            Duration::from_millis(200),
            "the sleeper",
        )
    })
    .unwrap_err();
    assert!(
        err.starts_with("timed out waiting for the sleeper"),
        "{err}"
    );
    assert!(
        err.contains("left a process in its group behind"),
        "the cleanup failure is still reported: {err}"
    );
}

#[test]
fn output_held_open_by_a_process_outside_the_group_errs_instead_of_hanging() {
    let dir = TempDir::new("fiber-bench-held");
    let marker = dir.path().to_string_lossy().into_owned();
    let watchdog = Watchdog::matching(&marker);
    let escaped = dir.path().join("escaped");
    // The leader waits for the descendant's marker before exiting, so
    // `Proc::stop` cannot catch the holder while it is still in the group.
    // The wait has no timeout of its own: the wall-clock limit below fails
    // the test naming "output held open" if the descendant never escapes.
    let script = format!(
        "perl -MPOSIX -e 'POSIX::setsid(); open my $f, \">>\", $ARGV[1] or die $!; print $f \"x\\n\"; close $f; sleep 3600' '{}' '{}' & while [ ! -e '{}' ]; do sleep 0.05; done",
        dir.path().display(),
        escaped.display(),
        escaped.display(),
    );
    let err = fakes::within("output held open", WALL, move || {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script]);
        run_to_end(&mut command, &System, WALL, "the holder")
    })
    .unwrap_err();
    assert_eq!(err, "the output of the holder stayed open");
    fakes::kill_matching(&marker).unwrap();
    assert!(fakes::matching_exits(&marker, READY));
    watchdog.stand_down(READY);
}

#[test]
fn timed_output_held_open_by_a_process_outside_the_group_errs_instead_of_hanging() {
    let dir = TempDir::new("fiber-bench-timed-held");
    let marker = dir.path().to_string_lossy().into_owned();
    let watchdog = Watchdog::matching(&marker);
    let escaped = dir.path().join("escaped");
    // As above, the leader waits for the descendant's marker before
    // exiting, so `Proc::stop` cannot catch the holder while it is still
    // in the group. The wall-clock limit fails the test naming "timed
    // output held open" if the call joins a reader blocked on the open
    // pipe instead of returning at its deadline.
    let script = format!(
        "perl -MPOSIX -e 'POSIX::setsid(); open my $f, \">>\", $ARGV[1] or die $!; print $f \"x\\n\"; close $f; sleep 3600' '{}' '{}' & while [ ! -e '{}' ]; do sleep 0.05; done",
        dir.path().display(),
        escaped.display(),
        escaped.display(),
    );
    let err = fakes::within("timed output held open", WALL, move || {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script]);
        super::timed_to_end(&mut command, &System, Duration::from_secs(5), "the holder")
    })
    .unwrap_err();
    assert!(err.starts_with("timed out waiting for the holder"), "{err}");
    fakes::kill_matching(&marker).unwrap();
    assert!(fakes::matching_exits(&marker, READY));
    watchdog.stand_down(READY);
}

#[test]
fn a_stop_that_needs_a_second_sigkill_errs_after_the_group_empties() {
    let dir = TempDir::new("fiber-bench-stop");
    let ready = Ready::new(dir.path());
    let mut command = Command::new("/bin/bash");
    command
        .arg("-c")
        .arg(leaves_descendants(ready.path()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let proc = Proc::spawn(&mut command, &System).unwrap();
    let group = ready.wait(READY)[0];
    assert_eq!(group, proc.pid());
    // The descendant ignores SIGTERM and holds the group after its leader
    // exits; it is up once its pid is written.
    ready.wait(READY);
    let err = fakes::within("the stop", WALL, move || proc.stop(&System)).unwrap_err();
    assert!(err.contains("left a process in its group behind"), "{err}");
    assert!(err.contains("SIGKILL emptied it"), "{err}");
    assert!(
        !fakes::kill_group(group, "0").unwrap(),
        "group {group} still has a process"
    );
}

/// A clock whose first sleep removes `marker`, waits for the child's exit
/// line, then advances: the exit poll only runs after the test's child
/// closed its stdout, so the run's timing ends before the clock first
/// moves, and the wait that follows is a signal, never a sleep.
struct AfterEof {
    marker: std::sync::Mutex<Option<std::path::PathBuf>>,
    ready: std::sync::Mutex<Ready>,
    inner: std::sync::Arc<fakes::clock::FakeClock>,
}

impl Clock for AfterEof {
    fn now(&self) -> std::time::Instant {
        self.inner.now()
    }
    fn wall(&self) -> std::time::SystemTime {
        self.inner.wall()
    }
    #[track_caller]
    fn sleep(&self, d: Duration) {
        if let Some(marker) = self.marker.lock().unwrap().take() {
            std::fs::remove_file(&marker).unwrap();
            // The child writes its exit line once its loop ends; the poll
            // that follows reaps it from there.
            self.ready.lock().unwrap().wait(READY);
        }
        self.inner.sleep(d);
    }
    fn wait_until(
        &self,
        until: Option<std::time::Instant>,
        wait: &mut dyn FnMut(Option<Duration>),
    ) {
        self.inner.wait_until(until, wait);
    }
    fn subscribe(&self, waker: std::sync::Weak<dyn contract::clock::Wake>) {
        self.inner.subscribe(waker);
    }
}

/// The child closes its stdout at once, spins until `marker` is gone, then
/// writes its exit line: the timing ends at the close, the run at the exit.
/// The spin pauses for nothing, so the line follows the marker at once.
fn eof_then_exit(marker: &Path, ready: &Path) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        &format!(
            "printf x; exec 1>&-; while [ -e '{}' ]; do :; done; echo $$ > '{}'",
            marker.display(),
            ready.display()
        ),
    ]);
    command
}

#[test]
fn a_timed_run_ends_when_stdout_closes_not_when_the_process_exits() {
    let dir = TempDir::new("fiber-bench-timed");
    let one = dir.path().join("one");
    std::fs::create_dir(&one).unwrap();
    let marker = one.join("marker");
    std::fs::write(&marker, "").unwrap();
    let ready = Ready::new(&one);
    let ready_path = ready.path().to_path_buf();
    let clock = std::sync::Arc::new(AfterEof {
        marker: std::sync::Mutex::new(Some(marker.clone())),
        ready: std::sync::Mutex::new(ready),
        inner: fakes::clock::FakeClock::new(),
    });
    let start = clock.now();
    let ran = std::sync::Arc::clone(&clock);
    let (finished, took) = fakes::within("a timed run", WALL, move || {
        super::timed_to_end(
            &mut eof_then_exit(&marker, &ready_path),
            ran.as_ref(),
            WALL,
            "the child",
        )
    })
    .unwrap();
    assert_eq!(took, Duration::ZERO);
    assert!(clock.now() - start >= super::PROBE);
    assert_eq!(finished.status.code(), Some(0));
    assert_eq!(finished.stdout, "x");

    let two = dir.path().join("two");
    std::fs::create_dir(&two).unwrap();
    let marker = two.join("marker");
    std::fs::write(&marker, "").unwrap();
    let ready = Ready::new(&two);
    let ready_path = ready.path().to_path_buf();
    let clock = AfterEof {
        marker: std::sync::Mutex::new(Some(marker.clone())),
        ready: std::sync::Mutex::new(ready),
        inner: fakes::clock::FakeClock::new(),
    };
    let finished = fakes::within("the same child under run_to_end", WALL, move || {
        run_to_end(
            &mut eof_then_exit(&marker, &ready_path),
            &clock,
            WALL,
            "the child",
        )
    })
    .unwrap();
    assert_eq!(finished.status.code(), Some(0));
    assert_eq!(finished.stdout, "x");
}

#[test]
fn a_timed_run_past_its_deadline_errs_and_leaves_nothing_behind() {
    let dir = TempDir::new("fiber-bench-timed-deadline");
    let marker = dir.path().to_string_lossy().into_owned();
    let watchdog = Watchdog::matching(&marker);
    let clock = fakes::clock::FakeClock::new();
    let script = format!("while [ -e '{}' ]; do sleep 30; done", dir.path().display());
    let err = fakes::within("a timed run past its deadline", WALL, move || {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script]);
        super::timed_to_end(
            &mut command,
            &*clock,
            Duration::from_millis(50),
            "the sleeper",
        )
    })
    .unwrap_err();
    assert_eq!(err, "timed out waiting for the sleeper");
    assert_eq!(fakes::matching(&marker).unwrap(), Vec::<u32>::new());
    watchdog.stand_down(READY);
}

/// A fake clock that removes `hold` on the sleep after the old exit bound
/// of fake time has passed: the child is released only once a wait on the
/// fake clock would have given up.
struct ReleaseAfterBound {
    hold: std::path::PathBuf,
    sleeps: std::sync::atomic::AtomicU64,
    inner: std::sync::Arc<fakes::clock::FakeClock>,
}

impl Clock for ReleaseAfterBound {
    fn now(&self) -> std::time::Instant {
        self.inner.now()
    }
    fn wall(&self) -> std::time::SystemTime {
        self.inner.wall()
    }
    #[track_caller]
    fn sleep(&self, d: Duration) {
        self.inner.sleep(d);
        let slept = self
            .sleeps
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let bound = WALL.as_millis() / super::PROBE.as_millis();
        if u128::from(slept) == bound + 1 {
            std::fs::remove_file(&self.hold).unwrap();
        }
    }
    fn wait_until(
        &self,
        until: Option<std::time::Instant>,
        wait: &mut dyn FnMut(Option<Duration>),
    ) {
        self.inner.wait_until(until, wait);
    }
    fn subscribe(&self, waker: std::sync::Weak<dyn contract::clock::Wake>) {
        self.inner.subscribe(waker);
    }
}

/// A clock for a child that writes its ready file, then holds until the
/// clock's sleeps have passed [`WALL`] of fake time.
fn held_child(dir: &Path) -> (Command, ReleaseAfterBound, Ready) {
    let hold = dir.join("hold");
    std::fs::write(&hold, "").unwrap();
    let ready = Ready::new(dir);
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        &format!(
            "echo $$ > '{}'; while [ -e '{}' ]; do :; done",
            ready.path().display(),
            hold.display()
        ),
    ]);
    let clock = ReleaseAfterBound {
        hold,
        sleeps: std::sync::atomic::AtomicU64::new(0),
        inner: fakes::clock::FakeClock::new(),
    };
    (command, clock, ready)
}

#[test]
fn waiting_for_a_process_to_exit_outlasts_fake_time_that_runs_ahead_of_it() {
    let dir = TempDir::new("fiber-bench-hold-exit");
    let (mut command, clock, ready) = held_child(dir.path());
    command.stdin(Stdio::null()).stdout(Stdio::null());
    let exited = fakes::within("the exit wait", WALL, move || {
        let mut proc = Proc::spawn(&mut command, &clock).unwrap();
        ready.wait(READY);
        let exited = proc.exits(&clock, WALL).unwrap();
        proc.stop(&System).unwrap();
        exited
    });
    assert!(exited, "the wait gave up while the child was held");
}

#[test]
fn a_run_to_its_end_outlasts_fake_time_that_runs_ahead_of_the_child() {
    let dir = TempDir::new("fiber-bench-hold-run");
    let (mut command, clock, _ready) = held_child(dir.path());
    let finished = fakes::within("the held run", WALL, move || {
        run_to_end(&mut command, &clock, WALL, "the holder")
    })
    .unwrap();
    assert_eq!(finished.status.code(), Some(0));
}
