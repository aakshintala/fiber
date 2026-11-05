use crate::MUST_SUCCEED_WITHIN;
use std::io::{BufRead, BufReader, Write};
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
    bounded, group_alive, group_answer, group_empties, group_lives, kill_group, kill_matching,
    kill_pid, listed_exit, lives, matching, matching_exits, pattern, pid_alive, pids_exit,
    read_lookup, signal_group, signal_named, signal_pid, spawn_lookup, try_matching_exits,
    wait_exits,
};
use crate::deadline::Deadline;

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

/// Reaps `child` under [`MUST_SUCCEED_WITHIN`], naming `what` on expiry.
#[track_caller]
fn reaped(mut child: Child, what: &str) -> std::process::ExitStatus {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    match Deadline::after(MUST_SUCCEED_WITHIN).recv(&finished) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited {MUST_SUCCEED_WITHIN:?} for {what}"),
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

#[test]
fn group_alive_is_false_once_the_group_is_empty() {
    let mut child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let group = child.id();
    assert!(group_alive(group));
    assert!(kill_group(group, "KILL").unwrap());
    child.wait().unwrap();
    assert!(!group_alive(group));
}

#[test]
fn group_alive_sees_a_killed_child_until_it_is_reaped() {
    let child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let group = child.id();
    let mut guard = KillAndReap(Some(child), group);
    assert!(group_alive(group), "a live group must read as alive");
    assert!(
        kill_group(group, "KILL").unwrap(),
        "the kill must reach the group"
    );
    // No reap yet: the killed child is a zombie, and the instantaneous
    // probe still reports a zombie's group as alive. Holding the reap
    // holds the killed child as a zombie, forcing the probe to read the
    // killed-but-unreaped state. The stored answer is this probe's own
    // result, so a failure names the kernel's reply and the child's wait
    // state.
    let answer = group_answer(group);
    let waited = guard.0.as_mut().unwrap().try_wait();
    assert!(
        lives(answer),
        "an unreaped zombie's group must read as alive (probe {answer:?}, child {waited:?})"
    );
    let child = guard.0.take().unwrap();
    drop(guard);
    reaped(child, "the killed child");
    assert!(
        group_empties(group, Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited {MUST_SUCCEED_WITHIN:?} for the reaped group to empty"
    );
}

/// Unwinding through [`KillAndReap`] kills the group and reaps the child:
/// a simulated assertion failure after the kill leaves no zombie behind.
#[test]
fn kill_and_reap_kills_and_reaps_on_unwind() {
    let child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let group = child.id();
    let pid = child.id();
    let failed = panic::catch_unwind(AssertUnwindSafe(|| {
        let _guard = KillAndReap(Some(child), group);
        assert!(
            kill_group(group, "KILL").unwrap(),
            "the kill must reach the group"
        );
        panic!("simulated assertion failure");
    }));
    assert!(failed.is_err(), "the simulated failure must unwind");
    assert!(
        !alive(pid),
        "the unwound child must be reaped, not a zombie"
    );
    assert!(
        group_empties(group, Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited {MUST_SUCCEED_WITHIN:?} for the unwound group to empty"
    );
}

#[test]
fn pid_alive_is_false_once_the_process_is_reaped() {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    assert!(pid_alive(pid));
    child.wait().unwrap();
    assert!(!pid_alive(pid));
}

// Like the refusal tests above: the probe is the signal `0`, which
// signals nothing, so an id of 1 or less must panic rather than probe.
#[test]
fn group_alive_refuses_group_zero_and_one() {
    for group in [0, 1] {
        let sentinel = sentinel();
        let refused = panic::catch_unwind(AssertUnwindSafe(|| group_alive(group)));
        assert!(refused.is_err(), "group {group} was not refused");
        assert!(survived(sentinel), "group {group} signalled the sentinel");
    }
}

#[test]
fn pid_alive_refuses_pid_zero_and_one() {
    for pid in [0, 1] {
        let sentinel = sentinel();
        let refused = panic::catch_unwind(AssertUnwindSafe(|| pid_alive(pid)));
        assert!(refused.is_err(), "pid {pid} was not refused");
        assert!(survived(sentinel), "pid {pid} signalled the sentinel");
    }
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
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || bounded(child, "the hung shell", Duration::from_millis(100)),
    );
    let err = missed.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(err.to_string().contains("the hung shell"), "{err}");
    assert!(
        pids_exit(&[pid], Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited {MUST_SUCCEED_WITHIN:?} for the hung shell to be killed and reaped"
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
        ready.wait(Deadline::after(MUST_SUCCEED_WITHIN)),
        vec![group],
        "the ready line is the closed-stdout shell's pid"
    );
    let pid = child.id();
    let missed = crate::within(
        "bounded to give up on the closed-stdout shell",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || bounded(child, "the closed-stdout shell", Duration::from_millis(100)),
    );
    let err = missed.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    assert!(err.to_string().contains("the closed-stdout shell"), "{err}");
    assert!(
        pids_exit(&[pid], Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited {MUST_SUCCEED_WITHIN:?} for the closed-stdout shell to be killed and reaped"
    );
    watchdog.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));
}

#[test]
fn bounded_returns_the_status_and_stdout_of_a_child_that_exits() {
    let child = Command::new("sh")
        .args(["-c", "echo listed; exit 3"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (status, stdout) = crate::within(
        "bounded to return",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || bounded(child, "the listing shell", MUST_SUCCEED_WITHIN),
    )
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
    assert!(panic::catch_unwind(|| matching("", MUST_SUCCEED_WITHIN)).is_err());
}

#[test]
fn pattern_escapes_every_regex_metacharacter() {
    assert_eq!(pattern("/tmp/a-b_c"), "[/]tmp/a-b_c");
    assert_eq!(pattern(r".[]()*+?{}|^$\"), r"\.\[\]\(\)\*\+\?\{\}\|\^\$\\");
}

#[test]
fn listings_do_not_list_the_listing_pgrep() {
    // No metacharacters: the pattern's spelling equals the text, the way
    // a workspace path does.
    let text = format!("pg1837-{}-w", std::process::id());
    // A holder carrying the pattern in its argv the way a listing pgrep
    // does; its blocking read is the pause point, so it stays listed
    // until released.
    let mut holder = Command::new("sh")
        .args(["-c", "read -r l", "pgrep", "-f", "--", &pattern(&text)])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let holder_guard = KillOnDrop(holder.id());
    let holder_pid = holder.id();
    let mut hidden = holder.stdin.take().unwrap();
    // A real match.
    let mut marked_shell = marked(&text);
    let marked_guard = KillOnDrop(marked_shell.id());
    let marked_pid = marked_shell.id();
    let out = marked_shell.stdout.take().unwrap();
    Piped::new(out).forked("the marked shell to fork");
    // The listing names the match, never the holder.
    let found = matching(&text, MUST_SUCCEED_WITHIN).unwrap();
    assert!(found.contains(&marked_pid), "{found:?}");
    assert!(
        !found.contains(&holder_pid),
        "a listing pgrep must not list itself: {found:?}"
    );
    // A sweep kills the match and spares the holder.
    kill_matching(&text, MUST_SUCCEED_WITHIN / 2).unwrap();
    assert!(alive(holder_pid), "the sweep killed the listing holder");
    // Release and reap both.
    hidden.write_all(b"\n").unwrap();
    drop(hidden);
    reaped(holder, "the holder to exit once its stdin got a line");
    drop(holder_guard);
    drop(marked_guard);
    reaped(marked_shell, "the swept marked shell");
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
    assert!(
        matching(&marker, MUST_SUCCEED_WITHIN)
            .unwrap()
            .contains(&pid)
    );
    // The dot is literal: a near miss matches nothing.
    let near = marker.replace("a.b", "axb");
    assert!(matching(&near, MUST_SUCCEED_WITHIN).unwrap().is_empty());
    drop(guard);
    reaped(child, "the marked shell");
    assert!(matching(&marker, MUST_SUCCEED_WITHIN).unwrap().is_empty());
}

/// Kills a process group on drop, so a failed assertion leaves nothing
/// running.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        kill_group(self.0, "KILL").unwrap_or(false);
    }
}

/// Kills a process group and reaps its child on drop, so a failed
/// assertion leaves neither a running process nor a zombie.
struct KillAndReap(Option<Child>, u32);

impl Drop for KillAndReap {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            kill_group(self.1, "KILL").unwrap_or(false);
            match child.wait() {
                Ok(_) | Err(_) => {}
            }
        }
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
    kill_matching(&marker, MUST_SUCCEED_WITHIN / 2).unwrap();
    assert_eq!(reaped(child, "the killed shell").signal(), Some(9));
    piped.closed("the shell's group to empty");
}

/// Present in a re-executed child, absent in the parent: the child drives
/// production `matching` into a stalled `pgrep` under a derived bound.
const STALLED_PGREP_CHILD: &str = "FIBER_TEST_STALLED_PGREP_CHILD";

/// The stalled `pgrep`'s marker, set by the parent for the child alone.
const STALLED_PGREP_MARKER: &str = "FIBER_TEST_STALLED_PGREP_MARKER";

/// Production `matching` with a caller-supplied bound expires inside that
/// bound naming the `pgrep` step: the child derives a 200 ms wait on an
/// injected clock, halves it for the listing, and drives the stalled
/// `pgrep` shadow into it. The parent re-executes this test with a
/// `PATH`-shadowed `pgrep` that blocks on a FIFO only for the run's unique
/// marker and execs the real `pgrep` otherwise (per-test processes, so no
/// other test's listing can block on this FIFO), guards the stall by pid,
/// and asserts the child's success before releasing the FIFO; the expiry
/// already killed and reaped the shadow through production.
#[test]
fn a_stalled_pgrep_times_out_naming_the_step_with_the_halved_bound() {
    if std::env::var_os(STALLED_PGREP_CHILD).is_some() {
        stalled_pgrep_child();
        return;
    }
    let dir = crate::TempDir::new("fiber-stalled-pgrep");
    // Unique to the run: the shadow stalls only this marker, and the
    // watchdog guards only it, so no other test's process can match.
    let marker = format!("stall-pgrep-{}", std::process::id());
    let fifo = dir.path().join("fifo");
    crate::fifo(&fifo);
    // Resolved before shadowing: the shadow falls through to it for every
    // listing that is not this run's stall.
    let real = Command::new("sh")
        .args(["-c", "command -v pgrep"])
        .output()
        .unwrap()
        .stdout;
    let real = String::from_utf8_lossy(&real).trim().to_owned();
    assert!(!real.is_empty(), "a real pgrep resolves before shadowing");
    // The production call passes the pattern form, so the shadow matches
    // the pattern literally: the raw marker never appears contiguously.
    let body = format!(
        "for arg in \"$@\"; do\n  case \"$arg\" in\n    *'{}'*)\n      read line < '{}'\n      exit 0\n      ;;\n  esac\ndone\nexec '{}' \"$@\"\n",
        pattern(&marker),
        fifo.display(),
        real,
    );
    crate::script(dir.path(), "pgrep", &body);
    // Held open read+write across the rerun, so the shadow's open succeeds
    // and its read parks: the listing provably cannot complete before the
    // child's bound expires. Dropping it releases a shadow the expiry left
    // behind, which then exits at once.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .unwrap();
    // The guard kills this stall alone by its argv if the child hangs and
    // the expiry below never runs: never this test's own group.
    let stall_guard = crate::Watchdog::matching(&marker);
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap()
    );
    let output = crate::rerun_within(
        "process_group::tests::a_stalled_pgrep_times_out_naming_the_step_with_the_halved_bound",
        &[
            (STALLED_PGREP_CHILD, "1"),
            ("PATH", path.as_str()),
            (STALLED_PGREP_MARKER, marker.as_str()),
        ],
        // Twice the fixed step budget: a production body discarding the
        // derived bound still expires inside this wait, so the failure
        // names the wrong budget instead of the rerun timing out.
        Deadline::after(2 * MUST_SUCCEED_WITHIN),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the stalled-pgrep child proves the derived bound:\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("1 passed"),
        "the child ran exactly the stalled-pgrep test: {stdout}"
    );
    drop(held);
    stall_guard.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));
}

/// The re-executed child: a 200 ms wait on an injected clock halves to the
/// exact nonzero listing bound production must expire inside of.
#[track_caller]
fn stalled_pgrep_child() {
    let marker = std::env::var(STALLED_PGREP_MARKER).expect("the parent sets the stall marker");
    let clock: &'static std::sync::Arc<crate::clock::FakeClock> =
        Box::leak(Box::new(crate::clock::FakeClock::new()));
    let wait = Deadline::on(&**clock).within(Duration::from_millis(200));
    // The inner step is half the enclosing wait, nonzero and exact: a
    // production body discarding `bound` cannot show this budget.
    let bound = wait.left() / 2;
    assert_eq!(
        bound,
        Duration::from_millis(100),
        "the listing step halves the enclosing wait"
    );
    let error = matching(&marker, bound).expect_err("the stalled pgrep outlives the derived bound");
    assert_eq!(
        error.kind(),
        io::ErrorKind::TimedOut,
        "the expiry is a timeout, not a pgrep failure"
    );
    assert_eq!(
        error.to_string(),
        "pgrep did not exit within 100ms",
        "the expiry names the pgrep step with the derived bound"
    );
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
fn the_matching_watchdog_script_bounds_its_lookup() {
    // The authored bound travels in the script text: the const and the
    // text cannot drift, or a bumped const leaves a stale sleep behind.
    let bound = format!("sleep {}", super::WATCHDOG_LOOKUP_WITHIN.as_secs());
    assert!(
        MATCHING_WATCHDOG_SCRIPT.contains(&bound),
        "the watchdog script bounds its lookup with {bound:?}: {MATCHING_WATCHDOG_SCRIPT:?}"
    );
    // No text-order pin: `the_matching_watchdog_script_refuses_an_empty_pattern`
    // proves the refusal behaviorally (exit 2, sentinel survives), so a move of
    // `pgrep` before the refusal still fails there.
    // A hung lookup still ends: the script kills the lookup, kills what it
    // listed, and exits, all within the bound plus kill time.
    let dir = crate::TempDir::new("watchdog-hung-lookup");
    let victim = Command::new("sh")
        .args(["-c", "sleep 30"])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let group = victim.id();
    let guard = KillOnDrop(group);
    // A `pgrep` that lists the victim and then hangs past the bound,
    // shadowing the real one on the script's path only: the test's own
    // environment keeps the real `pgrep`.
    let fake = dir.path().join("pgrep");
    std::fs::write(&fake, format!("#!/bin/sh\necho {group}\nexec sleep 30\n")).unwrap();
    let mut permissions = std::fs::metadata(&fake).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&fake, permissions).unwrap();
    let path = format!(
        "{}:{}",
        dir.path().to_string_lossy(),
        std::env::var("PATH").unwrap()
    );
    // Null stdin is EOF, so the script reaches the lookup at once.
    let status = crate::within(
        "the watchdog script to exit past a hung lookup",
        // The shared must-succeed bound: the script exits ~3 s past a hung lookup (WATCHDOG_LOOKUP_WITHIN), so 10 s keeps ~7 s margin and still bites if the lookup goes unbounded.
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            Command::new("sh")
                .args(["-c", MATCHING_WATCHDOG_SCRIPT, "watchdog"])
                .env(MATCHING_PATTERN_VAR, pattern("watchdog-hung-lookup"))
                .env("PATH", &path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
        },
    );
    assert!(
        status.success(),
        "the watchdog script exits past a hung lookup: {status}"
    );
    crate::wait_until(
        "the watchdog to kill what its timed-out lookup listed",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || !alive(group),
    );
    drop(guard);
    reaped(victim, "the listed victim");
}

#[test]
fn the_matching_watchdog_kills_without_tmpdir() {
    let dir = crate::TempDir::new("watchdog-no-tmpdir");
    let marker = dir.path().to_string_lossy().into_owned();
    let mut child = marked(&marker);
    let out = child.stdout.take().unwrap();
    Piped::new(out).forked("the marked shell to fork");
    let group = child.id();
    let guard = KillOnDrop(group);
    // `TMPDIR` leaves only the spawned script's environment: the `:-`
    // fallback still resolves under `/tmp`, never `/`, so the kill flow
    // runs. No test touches its own process environment.
    let status = crate::within(
        "the watchdog script to kill without TMPDIR",
        // The shared must-succeed bound: the script exits ~3 s past a hung lookup (WATCHDOG_LOOKUP_WITHIN), so 10 s keeps ~7 s margin and still bites if the lookup goes unbounded.
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            Command::new("sh")
                .args(["-c", MATCHING_WATCHDOG_SCRIPT, "watchdog"])
                .env(MATCHING_PATTERN_VAR, pattern(&marker))
                .env_remove("TMPDIR")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
        },
    );
    assert!(
        status.success(),
        "the watchdog script exits without TMPDIR: {status}"
    );
    // Before the guard drops: the kill below must be the script's, not the
    // guard's, or a neutered script still passes.
    crate::wait_until(
        "the watchdog to kill the marked shell without TMPDIR",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || !alive(group),
    );
    drop(guard);
    reaped(child, "the marked shell the watchdog killed without TMPDIR");
}

#[test]
fn group_empties_reports_an_empty_group_and_a_live_one() {
    let child = marked("group_empties");
    let group = child.id();
    assert!(
        !group_empties(group, Deadline::after(Duration::from_millis(200))),
        "a live group must not read as empty"
    );
    kill_group(group, "KILL").unwrap();
    reaped(child, "the killed group leader");
    assert!(
        group_empties(group, Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited {MUST_SUCCEED_WITHIN:?} for the killed group to empty"
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
    thread::spawn(move || {
        answered
            .send(group_empties(group, Deadline::after(MUST_SUCCEED_WITHIN)))
            .unwrap()
    });
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
        Deadline::after(MUST_SUCCEED_WITHIN).recv(&answer),
        Ok(true),
        "waited {MUST_SUCCEED_WITHIN:?} for group_empties to see the emptied group"
    );
}

/// Only an `ESRCH` probe answer retires a group: `Ok`, `EPERM` and any
/// other errno read live. The answers are forced values, never timing.
#[test]
fn group_probe_reads_empty_only_on_esrch() {
    assert!(lives(Ok(())), "an Ok probe must read as live");
    assert!(
        lives(Err(rustix::io::Errno::PERM)),
        "an EPERM probe must read as live"
    );
    assert!(
        lives(Err(rustix::io::Errno::INTR)),
        "an errno other than ESRCH must read as live"
    );
    assert!(
        !lives(Err(rustix::io::Errno::SRCH)),
        "an ESRCH probe must read as empty"
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
    // On macOS the probe can keep answering EPERM, which reads live, for
    // milliseconds after the reap, so the fall is awaited under a deadline.
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    while group_lives(group) {
        if deadline.left().is_zero() {
            panic!("waited {MUST_SUCCEED_WITHIN:?} for a reaped group to read as gone");
        }
        thread::yield_now();
    }
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
    thread::spawn(move || {
        answered
            .send(pids_exit(&[pa, pb], Deadline::after(MUST_SUCCEED_WITHIN)))
            .unwrap()
    });
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
        Deadline::after(MUST_SUCCEED_WITHIN).recv(&answer),
        Ok(true),
        "waited {MUST_SUCCEED_WITHIN:?} for both pids to exit"
    );
}

#[test]
fn wait_exits_probes_a_live_pid_about_every_10_ms() {
    let mut child = held();
    let pid = child.id();
    let stdin = child.stdin.take().unwrap();
    let (stop, stopped) = mpsc::channel::<()>();
    let (answered, answer) = mpsc::channel();
    thread::spawn(move || {
        let mut probes = 0;
        let exited = wait_exits(&[pid], &stopped, |id| {
            probes += 1;
            alive(id)
        });
        match answered.send((exited, probes)) {
            Ok(()) | Err(_) => {}
        }
    });
    // The pid lives, so no answer arrives within 100 ms: the receive must
    // time out, bounding the probes below. Dropping `stop` then ends the
    // wait, so a mutant that stopped probing cannot hang the test either.
    assert!(
        Deadline::after(Duration::from_millis(100))
            .recv(&answer)
            .is_err(),
        "wait_exits answered while its pid lived"
    );
    drop(stop);
    let (exited, probes) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&answer)
        .expect("waited {MUST_SUCCEED_WITHIN:?} for wait_exits to end once stopped");
    assert!(!exited, "a live pid must not read as exited");
    assert!(
        probes <= 20,
        "100 ms at a 10 ms poll is about 11 probes, got {probes}"
    );
    drop(stdin);
    reaped(child, "the held child to exit once its stdin closed");
}

#[test]
fn pids_exit_is_false_while_a_pid_lives() {
    let mut child = held();
    let pid = child.id();
    let stdin = child.stdin.take().unwrap();
    assert!(
        !pids_exit(&[pid], Deadline::after(Duration::from_millis(200))),
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
        matching_exits(&marker, Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited {MUST_SUCCEED_WITHIN:?} for nothing to match"
    );
}

#[test]
fn matching_exits_is_false_while_a_match_lives() {
    let dir = crate::TempDir::new("py");
    let marker = dir.path().to_string_lossy().into_owned();
    let child = marked(&marker);
    let group = child.id();
    assert!(
        !matching_exits(&marker, Deadline::after(Duration::from_millis(200))),
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
            .send(matching_exits(
                &marker_text,
                Deadline::after(MUST_SUCCEED_WITHIN),
            ))
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
        Deadline::after(MUST_SUCCEED_WITHIN).recv(&answer),
        Ok(true),
        "waited {MUST_SUCCEED_WITHIN:?} for the match to exit"
    );
}

#[test]
fn listed_exit_names_a_first_listing_failure() {
    let err = listed_exit(
        || Err::<Vec<u32>, _>(io::Error::other("pgrep blew up")),
        MUST_SUCCEED_WITHIN,
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
        MUST_SUCCEED_WITHIN,
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
    let text = match Deadline::after(MUST_SUCCEED_WITHIN).recv(&answer) {
        Ok(text) => text,
        Err(_) => panic!("waited {MUST_SUCCEED_WITHIN:?} for the stalled lookup to time out"),
    };
    assert_eq!(text, PS_TIMEOUT, "a stalled lookup must read as timed out");
    assert!(
        pids_exit(&[pid], Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited {MUST_SUCCEED_WITHIN:?} for the timed-out lookup to be reaped"
    );
    watchdog.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));
}

#[test]
fn a_lookup_that_fails_or_prints_nothing_reads_gone() {
    let child = spawn_lookup("sh", &["-c", "exit 3"]).unwrap();
    assert_eq!(
        read_lookup(child, "the failed lookup", MUST_SUCCEED_WITHIN),
        GONE_ARGS
    );
    let child = spawn_lookup("sh", &["-c", "exit 0"]).unwrap();
    assert_eq!(
        read_lookup(child, "the silent lookup", MUST_SUCCEED_WITHIN),
        GONE_ARGS
    );
}

#[test]
fn a_lookup_that_prints_then_fails_reads_gone_not_its_output() {
    let child = spawn_lookup("sh", &["-c", "echo stale; exit 1"]).unwrap();
    assert_eq!(
        read_lookup(child, "the failed lookup", MUST_SUCCEED_WITHIN),
        GONE_ARGS,
        "a failed lookup's output must not be read as the process's command line"
    );
}

#[test]
fn a_lookup_that_succeeds_reads_its_trimmed_output() {
    let child = spawn_lookup("sh", &["-c", "printf '  listed  \\n'; exit 0"]).unwrap();
    assert_eq!(
        read_lookup(child, "the listed lookup", MUST_SUCCEED_WITHIN),
        "listed"
    );
}

#[test]
fn a_lookup_that_cannot_be_bounded_reads_gone_not_timed_out() {
    // A child with no piped stdout: `bounded` fails with an error that is
    // not a timeout, before it waits on anything. The shell is in its own
    // group, watched, and killed by the test, then reaped by the test.
    let child = Command::new("sh")
        .args(["-c", "exec sleep 30"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let group = child.id();
    let pid = Pid::from_raw(i32::try_from(group).unwrap()).unwrap();
    let watchdog = crate::Watchdog::group(group);
    assert_eq!(
        read_lookup(child, "the unbounded lookup", MUST_SUCCEED_WITHIN),
        GONE_ARGS,
        "a lookup that failed for a reason other than a timeout reads gone"
    );
    kill_group(group, "KILL").unwrap();
    // `read_lookup` dropped the child unreaped: reap it here, on a thread
    // under the deadline, and check it was the signalled lookup.
    let (waited, reap) = mpsc::channel();
    thread::spawn(move || {
        match waited.send(rustix::process::waitpid(
            Some(pid),
            rustix::process::WaitOptions::empty(),
        )) {
            Ok(()) | Err(_) => {}
        }
    });
    let reaped = match Deadline::after(MUST_SUCCEED_WITHIN).recv(&reap) {
        Ok(result) => result.unwrap(),
        Err(_) => panic!("waited {MUST_SUCCEED_WITHIN:?} for the unbounded lookup to be reaped"),
    };
    let (reaped_pid, status) = reaped.expect("the killed lookup is waited on");
    assert_eq!(reaped_pid, pid);
    assert_eq!(status.terminating_signal(), Some(9));
    watchdog.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));
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
        move || match unanswered.recv() {
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
    assert!(err.to_string().starts_with("first listing: "), "{err}");
    drop(never);
}

#[test]
fn a_stalled_first_listing_fails_naming_the_listing_inside_the_callers_wait() {
    // The outer wait is made before the listing thread starts, so with
    // equal bounds the outer wait wins the race and the failure names no
    // step; the listing's own budget must expire first instead.
    let outer = Deadline::after(MUST_SUCCEED_WITHIN);
    let (never, unanswered) = mpsc::channel::<()>();
    let (answered, answer) = mpsc::channel();
    thread::spawn(move || {
        let result = listed_exit(
            move || match unanswered.recv() {
                Ok(()) | Err(_) => {
                    Err::<Vec<u32>, _>(io::Error::other("the listing never answered"))
                }
            },
            MUST_SUCCEED_WITHIN,
        );
        match answered.send(result) {
            Ok(()) | Err(_) => {}
        }
    });
    let result = match outer.recv(&answer) {
        Ok(result) => result,
        Err(_) => panic!("the caller's wait expired before the listing step named itself"),
    };
    let err = result.expect_err("a stalled first listing must fail");
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    let text = err.to_string();
    assert!(text.starts_with("first listing: "), "{text}");
    assert!(text.contains("deadline expired waiting for exit"), "{text}");
    drop(never);
}

#[test]
fn listing_budgets_nest_by_halves() {
    use super::{LISTING_WITHIN, PGREP_WITHIN};
    assert_eq!(PGREP_WITHIN * 2, LISTING_WITHIN);
    assert_eq!(LISTING_WITHIN * 2, MUST_SUCCEED_WITHIN);
}

#[test]
fn listed_exit_names_a_held_holder_when_the_exit_budget_is_spent() {
    // A spent exit budget still leaves the holder listing its own
    // budget: the first listing blocks on a test-owned channel, and the
    // wait must stay pending past a bound shorter than that budget.
    let mut child = held();
    let pid = child.id();
    let stdin = child.stdin.take().unwrap();
    let (release, released) = mpsc::channel::<()>();
    let (answered, answer) = mpsc::channel();
    thread::spawn(move || {
        let mut calls = 0;
        let result = listed_exit(
            move || {
                calls += 1;
                if calls == 1 {
                    match released.recv() {
                        Ok(()) => Ok(vec![pid]),
                        Err(_) => Err(io::Error::other("the listing was never released")),
                    }
                } else {
                    Ok(vec![pid])
                }
            },
            Duration::ZERO,
        );
        match answered.send(result) {
            Ok(()) | Err(_) => {}
        }
    });
    assert!(
        Deadline::after(Duration::from_millis(500))
            .recv(&answer)
            .is_err(),
        "a spent exit budget must still wait for the holder listing"
    );
    match release.send(()) {
        Ok(()) | Err(_) => {}
    }
    let err = match Deadline::after(MUST_SUCCEED_WITHIN).recv(&answer) {
        Ok(result) => result.expect_err("the holder outlives the spent budget, so it fails"),
        Err(_) => panic!("waited {MUST_SUCCEED_WITHIN:?} for listed_exit to name the held holder"),
    };
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    let text = err.to_string();
    assert!(text.contains("deadline expired waiting for exit"), "{text}");
    assert!(text.contains(&pid.to_string()), "{text}");
    drop(stdin);
    reaped(child, "the held child to exit once its stdin closed");
}

#[test]
fn try_matching_exits_is_ok_when_nothing_matches() {
    let dir = crate::TempDir::new("px-try");
    let marker = dir
        .path()
        .join("nothing-matches-this-marker")
        .to_string_lossy()
        .into_owned();
    assert!(try_matching_exits(&marker, Deadline::after(MUST_SUCCEED_WITHIN)).is_ok());
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
            MUST_SUCCEED_WITHIN,
        );
        match answered.send(result) {
            Ok(()) | Err(_) => {}
        }
    });
    // The child started after the first listing: it is released only
    // once the second listing has seen it, forcing the interleaving.
    assert!(
        Deadline::after(MUST_SUCCEED_WITHIN).recv(&second).is_ok(),
        "waited {MUST_SUCCEED_WITHIN:?} for the second listing to see the late process"
    );
    drop(stdin);
    reaped(child, "the held child to exit once its stdin closed");
    match Deadline::after(MUST_SUCCEED_WITHIN).recv(&answer) {
        Ok(result) => assert!(
            result.is_ok(),
            "waited {MUST_SUCCEED_WITHIN:?} for the late process to exit: {result:?}"
        ),
        Err(_) => panic!("waited {MUST_SUCCEED_WITHIN:?} for listed_exit to answer"),
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
        MUST_SUCCEED_WITHIN,
    );
    assert!(
        listed.is_ok(),
        "waited {MUST_SUCCEED_WITHIN:?} for the listed pid to exit"
    );
}

/// A child of the test that has exited but is not yet reaped is a zombie:
/// kill(pid, 0) still succeeds on it, but it is gone for the wait (#2048).
#[test]
fn listed_exit_counts_an_unreaped_child_as_exited() {
    let mut child = held();
    let pid = child.id();
    let mut calls = 0;
    // The child exits once its stdin closes, which happens only after the
    // first listing named it; the test reaps it after the wait.
    let stdin = child.stdin.take().unwrap();
    let mut stdin = Some(stdin);
    let result = listed_exit(
        move || {
            calls += 1;
            if calls == 1 {
                drop(stdin.take());
                Ok(vec![pid])
            } else {
                Ok(Vec::new())
            }
        },
        MUST_SUCCEED_WITHIN,
    );
    reaped(child, "the held child to exit once its stdin closed");
    result.unwrap();
}
