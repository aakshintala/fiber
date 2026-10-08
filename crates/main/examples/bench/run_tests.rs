use std::ffi::OsStr;
use std::path::Path;

use serde_json::json;

use std::process::{Command, Stdio};
use std::time::Duration;

use contract::clock::Clock;
use fakes::children::{Ready, leaves_descendants};
use fakes::{TempDir, Watchdog};

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
            Duration::from_secs(120),
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
    fn sleep(&self, d: Duration) {
        let ready = self
            .ready
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(ready) = ready {
            // One deadline for both lines: a scoped thread reads them.
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::scope(|scope| {
                scope.spawn(move || {
                    ready.wait(READY);
                    ready.wait(READY);
                    match tx.send(()) {
                        Ok(()) | Err(_) => {}
                    }
                });
                assert!(
                    rx.recv_timeout(READY).is_ok(),
                    "the child's two ready lines"
                );
            });
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
            Duration::from_secs(120),
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
