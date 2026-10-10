use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::panic::{self, AssertUnwindSafe};
use std::process::{Child, ChildStdout, Command, Stdio};

use std::os::unix::process::ExitStatusExt;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use std::cell::Cell;
use std::io;

use rustix::process::{Pid, Signal};

use super::{
    GONE_ARGS, MATCHING_PATTERN_VAR, MATCHING_WATCHDOG_SCRIPT, PS_TIMEOUT, WATCHDOG_SCRIPT, alive,
    bounded, group_empties, group_lives, kill_group, kill_matching, kill_pid, listed_exit,
    matching, matching_exits, pattern, pids_exit, read_lookup, signal_group, signal_named,
    signal_pid, spawn_lookup, try_matching_exits,
};
use crate::deadline::Deadline;

const DEADLINE: Duration = Duration::from_secs(5);

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

    /// Waits [`DEADLINE`] for the forked `sleep`'s pid line.
    #[track_caller]
    fn forked(&self, what: &str) {
        match Deadline::after(DEADLINE).recv(&self.0) {
            Ok(Some(_)) => {}
            _ => panic!("waited {DEADLINE:?} for {what}"),
        }
    }

    /// Waits [`DEADLINE`] for end-of-file, proving the `sleep` exited.
    #[track_caller]
    fn closed(self, what: &str) {
        let wait = Deadline::after(DEADLINE);
        loop {
            match wait.recv(&self.0) {
                Ok(Some(_)) => {}
                Ok(None) => return,
                Err(_) => panic!("waited {DEADLINE:?} for {what}"),
            }
        }
    }
}

/// Reaps `child` under [`DEADLINE`], naming `what` on expiry.
#[track_caller]
fn reaped(mut child: Child, what: &str) -> std::process::ExitStatus {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    match Deadline::after(DEADLINE).recv(&finished) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited {DEADLINE:?} for {what}"),
    }
}

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

// Refusal tests pass the probe signal `0`: a mutant that lets an id of 1
// or less through then sends `kill -0`, which signals nothing, instead of
// SIGKILL to every process the user owns.
#[test]
fn kill_group_refuses_group_zero_and_one() {
    for group in [0, 1] {
        let sentinel = sentinel();
        let refused = panic::catch_unwind(AssertUnwindSafe(|| kill_group(group, "0")));
        assert!(refused.is_err(), "group {group} was not refused");
        assert!(survived(sentinel), "group {group} signalled the sentinel");
    }
}

#[test]
fn kill_group_names_the_refused_group() {
    let message = *panic::catch_unwind(|| kill_group(1, "0"))
        .unwrap_err()
        .downcast::<String>()
        .unwrap();
    assert!(message.contains("process group 1"), "{message}");
}

#[test]
fn kill_group_signals_a_live_group() {
    let mut child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    assert!(kill_group(child.id(), "0").unwrap());
    assert!(kill_group(child.id(), "KILL").unwrap());
    child.wait().unwrap();
}

#[test]
fn kill_pid_refuses_pid_zero_and_one() {
    for pid in [0, 1] {
        let sentinel = sentinel();
        let refused = panic::catch_unwind(AssertUnwindSafe(|| kill_pid(pid, "0")));
        assert!(refused.is_err(), "pid {pid} was not refused");
        assert!(survived(sentinel), "pid {pid} signalled the sentinel");
    }
}

#[test]
fn kill_pid_names_the_refused_pid() {
    let message = *panic::catch_unwind(|| kill_pid(0, "0"))
        .unwrap_err()
        .downcast::<String>()
        .unwrap();
    assert!(message.contains("pid 0"), "{message}");
}

#[test]
fn kill_pid_signals_a_live_process() {
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    assert!(kill_pid(child.id(), "0").unwrap());
    assert!(kill_pid(child.id(), "KILL").unwrap());
    child.wait().unwrap();
}

/// One refused id through `signal_group` or `signal_pid`, with a `deliver`
/// that records being called instead of signalling anything.
type Refusal = fn(u32, &str, &Cell<bool>) -> io::Result<bool>;

// The seams take the kernel call as a closure, so these refusals pass
// `KILL` itself: a refusal that let an id through would only set the flag.
#[test]
fn refused_groups_and_pids_never_reach_the_kernel_call() {
    let seams: [(&str, Refusal); 2] = [
        ("group", |id, name, called| {
            signal_group(id, name, |_, _| {
                called.set(true);
                Ok(())
            })
        }),
        ("pid", |id, name, called| {
            signal_pid(id, name, |_, _| {
                called.set(true);
                Ok(())
            })
        }),
    ];
    for (seam, refuse) in seams {
        for id in [0, 1] {
            // `NONE` is no signal's name: the refusal comes before the name
            // is read, so it panics rather than returning the name's error.
            for name in ["KILL", "TERM", "0", "NONE"] {
                let called = Cell::new(false);
                let refused = panic::catch_unwind(AssertUnwindSafe(|| refuse(id, name, &called)));
                assert!(refused.is_err(), "{seam} {id} with {name} was not refused");
                assert!(
                    !called.get(),
                    "{seam} {id} with {name} reached the kernel call"
                );
            }
        }
    }
}

#[test]
fn an_accepted_id_reaches_the_kernel_call_with_its_signal() {
    let seen = Cell::new(None);
    let sent = signal_group(4242, "TERM", |id, sig| {
        seen.set(Some((id, sig)));
        Ok(())
    });
    assert!(sent.unwrap(), "an accepted send reads as sent");
    assert_eq!(
        seen.get(),
        Some((Pid::from_raw(4242).unwrap(), Some(Signal::TERM)))
    );
    let refused = signal_pid(4243, "0", |id, sig| {
        seen.set(Some((id, sig)));
        Err(rustix::io::Errno::SRCH)
    });
    assert!(
        !refused.unwrap(),
        "a send the kernel refused reads as not sent"
    );
    assert_eq!(seen.get(), Some((Pid::from_raw(4243).unwrap(), None)));
}

#[test]
fn an_id_past_the_pid_range_is_an_error_and_sends_nothing() {
    let called = Cell::new(false);
    let sent = signal_group(u32::MAX, "0", |_, _| {
        called.set(true);
        Ok(())
    });
    assert_eq!(sent.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    assert!(
        !called.get(),
        "an id past the pid range reached the kernel call"
    );
}

#[test]
fn each_signal_name_reads_as_its_signal_and_others_are_refused() {
    let names = [
        ("0", None),
        ("HUP", Some(Signal::HUP)),
        ("INT", Some(Signal::INT)),
        ("KILL", Some(Signal::KILL)),
        ("TERM", Some(Signal::TERM)),
        ("WINCH", Some(Signal::WINCH)),
    ];
    for (name, signal) in names {
        assert_eq!(signal_named(name).unwrap(), signal, "{name}");
    }
    for name in ["", "SIGKILL", "kill", "9", "QUIT"] {
        assert_eq!(
            signal_named(name).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "{name:?}"
        );
    }
}

/// A shell blocked reading a stdin pipe the test holds, its stdout piped
/// for `bounded`.
fn hung() -> Child {
    Command::new("sh")
        .args(["-c", "read line"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

#[test]
fn bounded_returns_a_timeout_on_a_miss_and_kills_the_child() {
    let mut child = hung();
    let pid = child.id();
    let stdin = child.stdin.take().unwrap();
    let missed = crate::within(
        "bounded to give up on the hung shell",
        DEADLINE,
        move || bounded(child, "the hung shell", Duration::from_millis(100)),
    );
    let err = missed.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(err.to_string().contains("the hung shell"), "{err}");
    assert!(
        pids_exit(&[pid], DEADLINE),
        "waited {DEADLINE:?} for the hung shell to be killed and reaped"
    );
    drop(stdin);
}

/// A shell in its own process group that closes its piped stdout, signals
/// readiness past the close, then blocks in `sleep`: at `bounded`'s
/// deadline stdout is already at end-of-file while the child lives, so a
/// worker that held its lock across `wait` could never be killed.
#[test]
fn bounded_kills_a_child_that_closed_stdout_then_hangs() {
    let dir = crate::TempDir::new("bc");
    let ready = crate::children::Ready::new(dir.path());
    let fifo = ready.path().to_owned();
    let child = Command::new("sh")
        .args(["-c", "exec 1>&-; echo $$ > \"$1\"; exec sleep 600", "sh"])
        .arg(&fifo)
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let group = child.id();
    let watchdog = crate::Watchdog::group(group);
    // The line is written past the close, so stdout is at end-of-file
    // while the `sleep` lives.
    assert_eq!(
        ready.wait(DEADLINE),
        vec![group],
        "the ready line is the closed-stdout shell's pid"
    );
    let pid = child.id();
    let missed = crate::within(
        "bounded to give up on the closed-stdout shell",
        DEADLINE,
        move || bounded(child, "the closed-stdout shell", Duration::from_millis(100)),
    );
    let err = missed.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(err.to_string().contains("the closed-stdout shell"), "{err}");
    assert!(
        pids_exit(&[pid], DEADLINE),
        "waited {DEADLINE:?} for the closed-stdout shell to be killed and reaped"
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn bounded_returns_the_status_and_stdout_of_a_child_that_exits() {
    let child = Command::new("sh")
        .args(["-c", "echo listed; exit 3"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (status, stdout) = crate::within("bounded to return", DEADLINE, move || {
        bounded(child, "the listing shell", DEADLINE)
    })
    .unwrap();
    assert_eq!(status.code(), Some(3));
    assert_eq!(stdout, b"listed\n");
}

#[test]
fn the_watchdog_script_refuses_group_zero_and_one() {
    for group in ["0", "1"] {
        let sentinel = sentinel();
        // Null stdin is EOF: unguarded, the script would reach `kill`.
        let status = Command::new("sh")
            .args(["-c", WATCHDOG_SCRIPT, "watchdog", group])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(2), "group {group}");
        assert!(survived(sentinel), "group {group} signalled the sentinel");
    }
}

// The empty-match refusals list processes or run nothing: none signals.
#[test]
fn pattern_refuses_an_empty_match() {
    let message = *panic::catch_unwind(|| pattern(""))
        .unwrap_err()
        .downcast::<&str>()
        .unwrap();
    assert!(message.contains("empty command-line match"), "{message}");
}

#[test]
fn matching_refuses_an_empty_match() {
    assert!(panic::catch_unwind(|| matching("")).is_err());
}

#[test]
fn pattern_escapes_every_regex_metacharacter() {
    assert_eq!(pattern("/tmp/a-b_c"), "/tmp/a-b_c");
    assert_eq!(pattern(r".[]()*+?{}|^$\"), r"\.\[\]\(\)\*\+\?\{\}\|\^\$\\");
}

#[test]
fn matching_finds_a_process_by_its_command_line() {
    let dir = crate::TempDir::new("pm");
    let marker = dir.path().join("a.b").to_string_lossy().into_owned();
    let child = marked(&marker);
    let pid = child.id();
    let guard = KillOnDrop(pid);
    // The shell's forked `sleep` may briefly carry the same command line,
    // so the shell's pid is asserted present, not alone.
    assert!(matching(&marker).unwrap().contains(&pid));
    // The dot is literal: a near miss matches nothing.
    let near = marker.replace("a.b", "axb");
    assert!(matching(&near).unwrap().is_empty());
    drop(guard);
    reaped(child, "the marked shell");
    assert!(matching(&marker).unwrap().is_empty());
}

/// Kills a process group on drop, so a failed assertion leaves nothing
/// running.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        kill_group(self.0, "KILL").unwrap_or(false);
    }
}

#[test]
fn kill_matching_kills_each_match_and_its_group() {
    let dir = crate::TempDir::new("pk");
    let marker = dir.path().to_string_lossy().into_owned();
    let mut child = marked(&marker);
    let out = child.stdout.take().unwrap();
    let piped = Piped::new(out);
    piped.forked("the sleep to fork");
    kill_matching(&marker).unwrap();
    assert_eq!(reaped(child, "the killed shell").signal(), Some(9));
    piped.closed("the shell's group to empty");
}

#[test]
fn the_matching_watchdog_script_refuses_an_empty_pattern() {
    let sentinel = sentinel();
    // Null stdin is EOF: unguarded, the script would reach `pgrep`.
    let status = Command::new("sh")
        .args(["-c", MATCHING_WATCHDOG_SCRIPT, "watchdog"])
        .env(MATCHING_PATTERN_VAR, "")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
    assert!(
        survived(sentinel),
        "the empty pattern signalled the sentinel"
    );
}

#[test]
fn group_empties_reports_an_empty_group_and_a_live_one() {
    let child = marked("group_empties");
    let group = child.id();
    assert!(
        !group_empties(group, Duration::from_millis(200)),
        "a live group must not read as empty"
    );
    kill_group(group, "KILL").unwrap();
    reaped(child, "the killed group leader");
    assert!(
        group_empties(group, DEADLINE),
        "waited {DEADLINE:?} for the killed group to empty"
    );
}

#[test]
fn group_empties_keeps_waiting_while_the_group_lives_and_returns_once_it_empties() {
    let mut child = Command::new("sh")
        .args(["-c", "read line"])
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let group = child.id();
    let stdin = child.stdin.take().unwrap();
    let (answered, answer) = mpsc::channel();
    thread::spawn(move || answered.send(group_empties(group, DEADLINE)).unwrap());
    // The group lives until its stdin closes, so the wait must still be
    // running after several probe intervals: a wait that gave up after one
    // live probe would have answered `false` by now.
    assert!(
        Deadline::after(Duration::from_millis(500))
            .recv(&answer)
            .is_err(),
        "group_empties answered while the group was alive"
    );
    drop(stdin);
    reaped(child, "the group leader to exit once its stdin closed");
    assert_eq!(
        Deadline::after(DEADLINE).recv(&answer),
        Ok(true),
        "waited {DEADLINE:?} for group_empties to see the emptied group"
    );
}

/// A shell held alive on a stdin pipe the test owns: dropping the pipe ends
/// it, and the test reaps it.
fn held() -> Child {
    Command::new("sh")
        .args(["-c", "read line"])
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

#[test]
#[should_panic(expected = "refusing")]
fn alive_refuses_pid_zero() {
    alive(0);
}

#[test]
#[should_panic(expected = "refusing")]
fn alive_refuses_pid_one() {
    alive(1);
}

#[test]
#[should_panic(expected = "refusing")]
fn group_probe_refuses_group_zero() {
    group_lives(0);
}

#[test]
#[should_panic(expected = "refusing")]
fn group_probe_refuses_group_one() {
    group_lives(1);
}

#[test]
fn alive_holds_while_the_process_lives_and_falls_once_it_is_reaped() {
    let mut child = held();
    let pid = child.id();
    assert!(alive(pid), "a live child must read as alive");
    drop(child.stdin.take().unwrap());
    reaped(child, "the held child to exit once its stdin closed");
    assert!(!alive(pid), "a reaped child must not read as alive");
}

#[test]
fn group_probe_holds_while_the_group_lives_and_falls_once_it_is_reaped() {
    let mut child = held();
    let group = child.id();
    assert!(group_lives(group), "a live group must read as live");
    drop(child.stdin.take().unwrap());
    reaped(child, "the group leader to exit once its stdin closed");
    assert!(!group_lives(group), "a reaped group must not read as live");
}

#[test]
fn pids_exit_waits_for_both_pids() {
    let mut a = held();
    let mut b = held();
    let pa = a.id();
    let pb = b.id();
    let stdin_a = a.stdin.take().unwrap();
    let stdin_b = b.stdin.take().unwrap();
    let (answered, answer) = mpsc::channel();
    thread::spawn(move || answered.send(pids_exit(&[pa, pb], DEADLINE)).unwrap());
    drop(stdin_a);
    reaped(a, "the first child to exit once its stdin closed");
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&answer)
            .is_err(),
        "pids_exit answered while the second child lived"
    );
    drop(stdin_b);
    reaped(b, "the second child to exit once its stdin closed");
    assert_eq!(
        Deadline::after(DEADLINE).recv(&answer),
        Ok(true),
        "waited {DEADLINE:?} for both pids to exit"
    );
}

#[test]
fn pids_exit_is_false_while_a_pid_lives() {
    let mut child = held();
    let pid = child.id();
    let stdin = child.stdin.take().unwrap();
    assert!(
        !pids_exit(&[pid], Duration::from_millis(200)),
        "a live pid must not read as exited"
    );
    drop(stdin);
    reaped(child, "the held child to exit once its stdin closed");
}

#[test]
fn matching_exits_is_true_when_nothing_matches() {
    let dir = crate::TempDir::new("px");
    let marker = dir
        .path()
        .join("nothing-matches-this-marker")
        .to_string_lossy()
        .into_owned();
    assert!(
        matching_exits(&marker, DEADLINE),
        "waited {DEADLINE:?} for nothing to match"
    );
}

#[test]
fn matching_exits_is_false_while_a_match_lives() {
    let dir = crate::TempDir::new("py");
    let marker = dir.path().to_string_lossy().into_owned();
    let child = marked(&marker);
    let group = child.id();
    assert!(
        !matching_exits(&marker, Duration::from_millis(200)),
        "a live match must not read as exited"
    );
    kill_group(group, "KILL").unwrap();
    reaped(child, "the killed marked shell");
}

#[test]
fn matching_exits_waits_for_a_live_match() {
    let dir = crate::TempDir::new("pz");
    let marker = dir.path().to_string_lossy().into_owned();
    let child = marked(&marker);
    let group = child.id();
    let marker_text = marker.clone();
    let (answered, answer) = mpsc::channel();
    thread::spawn(move || {
        answered
            .send(matching_exits(&marker_text, DEADLINE))
            .unwrap()
    });
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&answer)
            .is_err(),
        "matching_exits answered while the match lived"
    );
    kill_group(group, "KILL").unwrap();
    reaped(child, "the killed marked shell");
    assert_eq!(
        Deadline::after(DEADLINE).recv(&answer),
        Ok(true),
        "waited {DEADLINE:?} for the match to exit"
    );
}

#[test]
fn listed_exit_names_a_first_listing_failure() {
    let err = listed_exit(
        || Err::<Vec<u32>, _>(io::Error::other("pgrep blew up")),
        DEADLINE,
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Other);
    let text = err.to_string();
    assert!(text.contains("first pgrep failed"), "{text}");
    assert!(text.contains("pgrep blew up"), "{text}");
}

#[test]
fn listed_exit_names_a_second_listing_failure() {
    let mut child = held();
    let pid = child.id();
    drop(child.stdin.take().unwrap());
    reaped(child, "the held child to exit once its stdin closed");
    let mut calls = 0;
    let err = listed_exit(
        move || {
            calls += 1;
            if calls == 1 {
                Ok(vec![pid])
            } else {
                Err(io::Error::other("pgrep fell over"))
            }
        },
        DEADLINE,
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Other);
    let text = err.to_string();
    assert!(text.contains("second pgrep failed"), "{text}");
    assert!(text.contains("pgrep fell over"), "{text}");
}

#[test]
fn a_stalled_lookup_times_out_and_reaps_the_lookup() {
    // A lookup that never prints: `sleep` ignores stdin, so a null one
    // still stalls it past the lookup's own deadline. Its own process
    // group, watched: a hung helper still leaves nothing behind, on pass,
    // on panic, and when the test process dies.
    let child = spawn_lookup("sh", &["-c", "exec sleep 30"]).unwrap();
    let group = child.id();
    let watchdog = crate::Watchdog::group(group);
    let pid = child.id();
    let (answered, answer) = mpsc::channel();
    thread::spawn(move || {
        match answered.send(read_lookup(
            child,
            "the stalled lookup",
            Duration::from_millis(200),
        )) {
            Ok(()) | Err(_) => {}
        }
    });
    let text = match Deadline::after(DEADLINE).recv(&answer) {
        Ok(text) => text,
        Err(_) => panic!("waited {DEADLINE:?} for the stalled lookup to time out"),
    };
    assert_eq!(text, PS_TIMEOUT, "a stalled lookup must read as timed out");
    assert!(
        pids_exit(&[pid], DEADLINE),
        "waited {DEADLINE:?} for the timed-out lookup to be reaped"
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_lookup_that_fails_or_prints_nothing_reads_gone() {
    let child = spawn_lookup("sh", &["-c", "exit 3"]).unwrap();
    assert_eq!(read_lookup(child, "the failed lookup", DEADLINE), GONE_ARGS);
    let child = spawn_lookup("sh", &["-c", "exit 0"]).unwrap();
    assert_eq!(read_lookup(child, "the silent lookup", DEADLINE), GONE_ARGS);
}

#[test]
fn listed_exit_names_the_holders_when_the_deadline_expires() {
    let mut child = held();
    let pid = child.id();
    let stdin = child.stdin.take().unwrap();
    let err = listed_exit(move || Ok(vec![pid]), Duration::from_millis(200)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    let text = err.to_string();
    assert!(text.contains("deadline expired waiting for exit"), "{text}");
    assert!(text.contains(&pid.to_string()), "{text}");
    assert!(text.contains("sh"), "{text}");
    drop(stdin);
    reaped(child, "the held child to exit once its stdin closed");
}

#[test]
fn listed_exit_reports_expiry_with_no_listing_yet() {
    // A listing that never answers: the deadline expires with nothing
    // published, so the error names no holder.
    let (never, unanswered) = mpsc::channel::<()>();
    let err = listed_exit(
        move || match Deadline::after(DEADLINE).recv(&unanswered) {
            Ok(()) | Err(_) => Err::<Vec<u32>, _>(io::Error::other("the listing never answered")),
        },
        Duration::from_millis(200),
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(
        err.to_string()
            .contains("deadline expired waiting for exit"),
        "{err}"
    );
    drop(never);
}

#[test]
fn try_matching_exits_is_ok_when_nothing_matches() {
    let dir = crate::TempDir::new("px-try");
    let marker = dir
        .path()
        .join("nothing-matches-this-marker")
        .to_string_lossy()
        .into_owned();
    assert!(try_matching_exits(&marker, DEADLINE).is_ok());
}

#[test]
fn listed_exit_waits_for_a_process_started_after_the_first_listing() {
    let mut child = held();
    let pid = child.id();
    let stdin = child.stdin.take().unwrap();
    let (second_listed, second) = mpsc::channel::<()>();
    let (answered, answer) = mpsc::channel();
    thread::spawn(move || {
        let mut calls = 0;
        let result = listed_exit(
            move || {
                calls += 1;
                if calls == 1 {
                    Ok(vec![])
                } else if calls == 2 {
                    match second_listed.send(()) {
                        Ok(()) | Err(_) => {}
                    }
                    Ok(vec![pid])
                } else {
                    Ok(vec![])
                }
            },
            DEADLINE,
        );
        match answered.send(result) {
            Ok(()) | Err(_) => {}
        }
    });
    // The child started after the first listing: it is released only
    // once the second listing has seen it, forcing the interleaving.
    assert!(
        Deadline::after(DEADLINE).recv(&second).is_ok(),
        "waited {DEADLINE:?} for the second listing to see the late process"
    );
    drop(stdin);
    reaped(child, "the held child to exit once its stdin closed");
    match Deadline::after(DEADLINE).recv(&answer) {
        Ok(result) => assert!(
            result.is_ok(),
            "waited {DEADLINE:?} for the late process to exit: {result:?}"
        ),
        Err(_) => panic!("waited {DEADLINE:?} for listed_exit to answer"),
    }
}

#[test]
fn listed_exit_reports_expiry_when_a_late_process_never_exits() {
    let mut child = held();
    let pid = child.id();
    let stdin = child.stdin.take().unwrap();
    let mut calls = 0;
    let err = listed_exit(
        move || {
            calls += 1;
            if calls == 1 {
                Ok(vec![])
            } else {
                Ok(vec![pid])
            }
        },
        Duration::from_millis(200),
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    let text = err.to_string();
    assert!(
        text.contains("deadline expired waiting for exit"),
        "a late process that never exits must expire the deadline: {text}"
    );
    assert!(text.contains(&pid.to_string()), "{text}");
    drop(stdin);
    reaped(child, "the held child to exit once its stdin closed");
}

#[test]
fn listed_exit_is_true_when_the_second_listing_is_empty() {
    let mut child = held();
    let pid = child.id();
    drop(child.stdin.take().unwrap());
    reaped(child, "the held child to exit once its stdin closed");
    let mut calls = 0;
    let listed = listed_exit(
        move || {
            calls += 1;
            if calls == 1 {
                Ok(vec![pid])
            } else {
                Ok(vec![])
            }
        },
        DEADLINE,
    );
    assert!(
        listed.is_ok(),
        "waited {DEADLINE:?} for the listed pid to exit"
    );
}
