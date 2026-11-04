use crate::MUST_SUCCEED_WITHIN;
use std::io::{BufRead, BufReader};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::panic::{self, AssertUnwindSafe};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use super::Watchdog;
use crate::deadline::Deadline;
use crate::{group_alive, kill_group};

/// A process in the test's own group that a stray group signal would kill.
fn sentinel() -> Child {
    Command::new("sleep").arg("30").spawn().unwrap()
}

/// Whether the sentinel is still running; ends it either way.
fn survived(mut sentinel: Child) -> bool {
    let alive = sentinel.try_wait().unwrap().is_none();
    sentinel.kill().unwrap();
    sentinel.wait().unwrap();
    alive
}

#[test]
fn group_refuses_group_zero_and_one() {
    for group in [0, 1] {
        let sentinel = sentinel();
        let refused = panic::catch_unwind(AssertUnwindSafe(|| Watchdog::group(group)));
        assert!(refused.is_err(), "group {group} was not refused");
        assert!(survived(sentinel), "group {group} signalled the sentinel");
    }
}

#[test]
fn dropping_the_watchdog_kills_the_group() {
    let mut child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let group = child.id();
    drop(Watchdog::group(group));
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = match Deadline::after(MUST_SUCCEED_WITHIN).recv(&finished) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited {MUST_SUCCEED_WITHIN:?} for the process group to die"),
    };
    assert_eq!(status.signal(), Some(9));
    assert!(!group_alive(group));
}

#[test]
fn stand_down_leaves_the_group_alive() {
    let mut child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let watchdog_pid = watchdog.child.as_ref().expect("a running watchdog").id();
    // `stand_down` returns once the watchdog is reaped: from here nothing
    // else can signal the group.
    watchdog.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));
    assert!(
        !crate::kill_pid(watchdog_pid, "0").unwrap(),
        "stand_down returned before reaping the watchdog"
    );
    match kill_group(group, "TERM") {
        Ok(_) | Err(_) => {}
    }
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = match Deadline::after(MUST_SUCCEED_WITHIN).recv(&finished) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited {MUST_SUCCEED_WITHIN:?} for the group to end on SIGTERM"),
    };
    // A SIGKILL the watchdog sent before it exited would still be pending,
    // and it wins over the later SIGTERM.
    assert_eq!(
        status.signal(),
        Some(15),
        "the watchdog signalled the group after stand_down"
    );
}

#[test]
fn matching_refuses_an_empty_match() {
    let refused = panic::catch_unwind(AssertUnwindSafe(|| Watchdog::matching("")));
    assert!(refused.is_err());
}

/// A shell in its own process group whose command line carries `marker`.
/// It forks `sleep 30` into the group, echoes the sleep's pid, then waits:
/// the sleep holds the piped stdout, and the pid line follows its fork.
fn marked(marker: &str) -> Child {
    Command::new("sh")
        .args(["-c", "sleep 30 & echo $!; wait", marker])
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Lines of a `marked` shell's stdout, ending at end-of-file.
struct Piped(mpsc::Receiver<Option<String>>);

impl Piped {
    fn new(out: ChildStdout) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(out);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if tx.send(Some(line)).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
            match tx.send(None) {
                Ok(()) | Err(_) => {}
            }
        });
        Self(rx)
    }

    /// Waits [`MUST_SUCCEED_WITHIN`] for the forked `sleep`'s pid line.
    #[track_caller]
    fn forked(&self, what: &str) {
        match Deadline::after(MUST_SUCCEED_WITHIN).recv(&self.0) {
            Ok(Some(_)) => {}
            _ => panic!("waited {MUST_SUCCEED_WITHIN:?} for {what}"),
        }
    }

    /// Waits [`MUST_SUCCEED_WITHIN`] for end-of-file, proving the `sleep` exited.
    #[track_caller]
    fn closed(self, what: &str) {
        let wait = Deadline::after(MUST_SUCCEED_WITHIN);
        loop {
            match wait.recv(&self.0) {
                Ok(Some(_)) => {}
                Ok(None) => return,
                Err(_) => panic!("waited {MUST_SUCCEED_WITHIN:?} for {what}"),
            }
        }
    }
}

#[test]
fn dropping_a_matching_watchdog_kills_every_match() {
    let dir = crate::TempDir::new("wm");
    let marker = dir.path().to_string_lossy().into_owned();
    let mut child = marked(&marker);
    let out = child.stdout.take().unwrap();
    let piped = Piped::new(out);
    piped.forked("the sleep to fork");
    drop(Watchdog::matching(&marker));
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = match Deadline::after(MUST_SUCCEED_WITHIN).recv(&finished) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited {MUST_SUCCEED_WITHIN:?} for the matching process to die"),
    };
    assert_eq!(status.signal(), Some(9));
    piped.closed("the matching process's group to empty");
}

#[test]
fn a_failing_scoped_watchdog_leaves_a_sibling_holder_alive() {
    use std::io::Write as _;

    const CHILD: &str = "FIBER_SCOPED_WATCHDOG_CHILD";
    const PANIC: &str = "deliberate scoped watchdog failure";
    const CLEANED: &str = "scoped watchdog cleanup complete";

    // Owns only a spawned fixture's group and its single reaper. On any
    // failed assertion, kill that known group before waiting for the reaper.
    struct Reaper {
        group: Option<u32>,
        exit: Option<mpsc::Receiver<std::io::Result<std::process::ExitStatus>>>,
    }
    impl Reaper {
        fn new(mut child: Child) -> Self {
            let group = child.id();
            let (tx, exit) = mpsc::channel();
            thread::spawn(move || drop(tx.send(child.wait())));
            Self {
                group: Some(group),
                exit: Some(exit),
            }
        }

        fn wait(&mut self, what: &str) -> std::process::ExitStatus {
            let status = Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(self.exit.as_ref().unwrap(), what)
                .unwrap();
            self.exit = None;
            status
        }
    }
    impl Drop for Reaper {
        fn drop(&mut self) {
            if let Some(group) = self.group.take() {
                drop(kill_group(group, "KILL"));
                if let Some(exit) = self.exit.take() {
                    drop(Deadline::after(MUST_SUCCEED_WITHIN).recv(&exit));
                }
            }
        }
    }

    if std::env::var_os(CHILD).is_some() {
        let dir = crate::TempDir::new("scoped-watchdog-owned");
        let marker = dir.path().to_str().unwrap();
        let mut owned = marked(marker);
        let out = owned.stdout.take().unwrap();
        let mut owned = Reaper::new(owned);
        let piped = Piped::new(out);
        piped.forked("the owned holder's acknowledged fork");
        let mut watchdog = Watchdog::matching(marker);
        // Retain the real watchdog's reaper outside the unwind, so dropping
        // its stdin is followed by an observed script exit, not a timing guess.
        let mut watchdog_exit = Reaper::new(watchdog.child.take().unwrap());
        let failure = panic::catch_unwind(AssertUnwindSafe(move || {
            let _guard = watchdog;
            panic!("{PANIC}");
        }))
        .unwrap_err();
        assert!(
            watchdog_exit
                .wait("the matching watchdog's cleanup")
                .success()
        );
        assert_eq!(
            owned.wait("the owned holder to be killed").signal(),
            Some(9)
        );
        piped.closed("the owned holder's group stdout to close");
        owned.group = None;
        watchdog_exit.group = None;
        writeln!(std::io::stdout(), "\n{CLEANED}").unwrap();
        panic::resume_unwind(failure);
    }

    let dir = crate::TempDir::new("scoped-watchdog-sibling");
    let path = dir.path().join("hold");
    let command = format!(
        "exec 3<'{}'; echo opened; read -r line <&3; printf '%s\\n' \"$line\"",
        path.display().to_string().replace('\'', "'\\''")
    );
    let mut hold = crate::Hold::park(&path, &command);
    let marker = "head -n 1 hold sibling";
    assert!(marker.contains("head -n 1 hold"));
    // Register the stdout reader before the holder can announce its open.
    let (reader, writer) = std::io::pipe().unwrap();
    let (tx, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    let holder = Command::new("sh")
        .args(["-c", &command, marker])
        .stdout(writer)
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = holder.id();
    let mut holder = Reaper::new(holder);
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&lines, "the sibling holder to open its FIFO")
            .unwrap(),
        "opened"
    );
    let output = crate::rerun_prepared(
        "watchdog::tests::a_failing_scoped_watchdog_leaves_a_sibling_holder_alive",
        &[(CHILD, "1")],
        Deadline::after(crate::deadline::WAIT),
        |_| {},
    );
    assert!(!output.status.success(), "the child must really fail");
    assert!(String::from_utf8_lossy(&output.stderr).contains(PANIC));
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == CLEANED),
        "child stdout: {}\nchild stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // The child reaped its watchdog and owned holder before acknowledging
    // cleanup. Keep this sibling's release open across that entire chain.
    assert!(crate::pid_alive(pid), "cleanup killed the sibling holder");
    hold.release().write_all(b"sibling survived\n").unwrap();
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&lines, "the sibling holder to consume its release")
            .unwrap(),
        "sibling survived"
    );
    assert!(holder.wait("the released sibling holder to exit").success());
    holder.group = None;
    hold.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));
}

#[test]
fn stand_down_leaves_matching_processes_alive() {
    let dir = crate::TempDir::new("ws");
    let marker = dir.path().to_string_lossy().into_owned();
    let mut child = marked(&marker);
    let group = child.id();
    let watchdog = Watchdog::matching(&marker);
    let watchdog_pid = watchdog.child.as_ref().expect("a running watchdog").id();
    // `stand_down` returns once the watchdog is reaped: from here nothing
    // else can signal the group.
    watchdog.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));
    assert!(
        !crate::kill_pid(watchdog_pid, "0").unwrap(),
        "stand_down returned before reaping the watchdog"
    );
    match kill_group(group, "TERM") {
        Ok(_) | Err(_) => {}
    }
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = match Deadline::after(MUST_SUCCEED_WITHIN).recv(&finished) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited {MUST_SUCCEED_WITHIN:?} for the group to end on SIGTERM"),
    };
    // A SIGKILL the watchdog sent before it exited would still be pending,
    // and it wins over the later SIGTERM.
    assert_eq!(
        status.signal(),
        Some(15),
        "the watchdog signalled the group after stand_down"
    );
}
