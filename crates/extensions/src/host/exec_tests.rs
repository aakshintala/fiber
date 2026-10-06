//! The run: spawn, readers, the stop sequence, the group list,
//! `signal_name` and `refused_group` (`docs/extensions.md`, "Host calls";
//! `docs/tools.md`, "Stopping a command").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use contract::clock::Clock as _;
use fakes::clock::FakeClock;

use super::{ExecRequest, GROUP_POLL, group_alive, kill_every_group, refused_group, signal_name};

/// How long a test waits on the run before it fails.
const DEADLINE: Duration = Duration::from_secs(10);

/// The extension's memory cap in these tests, unless one sets its own.
const CAP: usize = 1 << 20;

fn request(program: &str, args: &[&str], cwd: PathBuf, cap: usize) -> ExecRequest {
    ExecRequest {
        program: program.into(),
        args: args.iter().map(|s| (*s).into()).collect(),
        cwd,
        cap,
    }
}

fn sh(script: &str, cwd: PathBuf, cap: usize) -> ExecRequest {
    request("sh", &["-c", script], cwd, cap)
}

/// Runs `req` on a worker with `clock` and no deadline. The sender drops
/// the run's cancel only when the test drops it; the receiver carries the
/// run's answer. A call that blocks is a wait too: every receive names its
/// deadline.
fn spawn(
    req: ExecRequest,
    clock: Arc<FakeClock>,
    deadline: Option<Instant>,
) -> (
    mpsc::Sender<()>,
    mpsc::Receiver<Result<super::Ran, super::ExecError>>,
) {
    let (cancel_tx, cancel_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
    let clock_ref: Arc<dyn contract::clock::Clock> = clock;
    std::thread::spawn(move || {
        let ran = super::run(&req, clock_ref.as_ref(), deadline, cancel_rx);
        let _sent = done_tx.send(ran);
    });
    (cancel_tx, done_rx)
}

fn dir(name: &str) -> (fakes::TempDir, PathBuf) {
    let dir = fakes::TempDir::new(name);
    let path = dir.path().to_path_buf();
    (dir, path)
}

#[test]
fn echo_returns_exit_3_and_both_streams() {
    let (_dir, cwd) = dir("fiber-exec-echo");
    let clock = FakeClock::new();
    let (_cancel, done) = spawn(
        sh("echo out; echo err >&2; exit 3", cwd, CAP),
        Arc::clone(&clock),
        None,
    );
    let ran = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the echo run")
        .expect("the echo run spawns");
    assert_eq!(ran.exit_code, Some(3), "the echo run exits 3");
    assert_eq!(ran.signal, None);
    assert_eq!(ran.stdout, b"out\n");
    assert_eq!(ran.stderr, b"err\n");
    assert!(!ran.timed_out);
}

#[test]
fn a_self_term_reports_sigterm_and_no_exit_code() {
    let (_dir, cwd) = dir("fiber-exec-term");
    let clock = FakeClock::new();
    let (_cancel, done) = spawn(sh("kill -TERM $$", cwd, CAP), Arc::clone(&clock), None);
    let ran = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the term run")
        .expect("the term run spawns");
    assert_eq!(ran.exit_code, None);
    assert_eq!(ran.signal.as_deref(), Some("SIGTERM"));
}

#[test]
fn the_child_runs_in_its_own_group() {
    let (_dir, cwd) = dir("fiber-exec-group");
    let clock = FakeClock::new();
    let (_cancel, done) = spawn(sh("ps -o pgid= -p $$", cwd, CAP), Arc::clone(&clock), None);
    let ran = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the group run")
        .expect("the group run spawns");
    let printed = String::from_utf8_lossy(&ran.stdout).trim().to_owned();
    let pgid: u32 = printed.parse().expect("the group run prints its pgid");
    assert!(pgid > 1, "the child has a real group id");
    // Its own group: `ps` prints the pid's group, which is the child's pid.
    // The run reaps the child, so the group is empty now; the print proves
    // the child saw itself as a leader while it ran.
    assert!(!group_alive(pgid), "the reaped group is empty");
}

#[test]
fn cwd_is_the_given_directory() {
    let (_dir, cwd) = dir("fiber-exec-cwd");
    let clock = FakeClock::new();
    let (_cancel, done) = spawn(sh("pwd", cwd.clone(), CAP), Arc::clone(&clock), None);
    let ran = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the pwd run")
        .expect("the pwd run spawns");
    let expected = cwd.canonicalize().unwrap();
    let got = PathBuf::from(String::from_utf8_lossy(&ran.stdout).trim());
    assert_eq!(got, expected);
}

#[test]
fn the_session_environment_is_visible() {
    let (_dir, cwd) = dir("fiber-exec-env");
    let clock = FakeClock::new();
    let home = std::env::var("HOME").unwrap();
    let (_cancel, done) = spawn(
        sh("printf %s \"$HOME\"", cwd, CAP),
        Arc::clone(&clock),
        None,
    );
    let ran = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the env run")
        .expect("the env run spawns");
    assert_eq!(String::from_utf8_lossy(&ran.stdout), home);
}

#[test]
fn a_missing_program_is_the_spawn_error() {
    let (_dir, cwd) = dir("fiber-exec-missing");
    let clock = FakeClock::new();
    let (_cancel, done) = spawn(
        request("fiber-definitely-missing-xyz", &[], cwd, CAP),
        Arc::clone(&clock),
        None,
    );
    let err = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the missing run")
        .expect_err("a missing program never runs");
    assert!(
        err.message
            .starts_with("host.exec: fiber-definitely-missing-xyz: "),
        "the spawn error names the program: {}",
        err.message
    );
    assert!(err.ran.is_none(), "a spawn failure never ran");
}

#[test]
fn output_past_the_cap_stops_the_run_with_the_cap_error() {
    let (_dir, cwd) = dir("fiber-exec-cap");
    let clock = FakeClock::new();
    let cap = 100;
    let (_cancel, done) = spawn(
        sh("head -c 2000 /dev/zero | tr '\\0' x", cwd, cap),
        Arc::clone(&clock),
        None,
    );
    let err = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the capped run")
        .expect_err("output past the cap never returns");
    assert_eq!(
        err.message,
        format!("host.exec: sh: output passed the extension's memory cap of {cap} bytes")
    );
    assert!(
        err.ran.is_some(),
        "a capped run started, so its end is logged"
    );
}

#[test]
fn cancel_sends_term_then_kill_after_800_ms() {
    let (_dir, cwd) = dir("fiber-exec-cancel");
    let clock = FakeClock::new();
    let ready = fakes::children::Ready::new(&cwd);
    let script = format!(
        "echo $$ > '{}'\ntrap '' TERM\nwhile :; do :; done\n",
        ready.path().display()
    );
    let (cancel, done) = spawn(sh(&script, cwd, CAP), Arc::clone(&clock), None);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let running_until = clock.now() + GROUP_POLL;
    assert!(
        clock.await_parked(running_until, DEADLINE),
        "waited {DEADLINE:?} for the run to park while running"
    );
    drop(cancel);
    // Wake the parked run so it sees the drop; the stop starts here.
    clock.advance(Duration::from_millis(1));
    let kill_at = clock.now() + Duration::from_millis(800);
    assert!(
        clock.await_parked(kill_at, DEADLINE),
        "waited {DEADLINE:?} for the run to park for the 800 ms grace"
    );
    assert!(
        fakes::kill_group(pgid, "0").unwrap(),
        "SIGTERM leaves the ignoring group alive"
    );
    clock.advance(Duration::from_millis(800));
    let ran = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the cancelled run")
        .expect("the cancelled run stops");
    assert_eq!(ran.signal.as_deref(), Some("SIGKILL"));
    assert_eq!(ran.exit_code, None);
    assert!(!ran.timed_out, "a drop is not the deadline");
    watchdog.stand_down(DEADLINE);
}

#[test]
fn output_held_open_after_the_kill_stops_after_2_s() {
    let (_dir, cwd) = dir("fiber-exec-drain");
    let clock = FakeClock::new();
    let ready = fakes::children::Ready::new(&cwd);
    let script = fakes::children::escapes_group(ready.path());
    let (cancel, done) = spawn(sh(&script, cwd, CAP), Arc::clone(&clock), None);
    let first = ready.wait(DEADLINE)[0];
    let _second = ready.wait(DEADLINE);
    let watchdog = fakes::Watchdog::group(first);
    let running_until = clock.now() + GROUP_POLL;
    assert!(
        clock.await_parked(running_until, DEADLINE),
        "waited {DEADLINE:?} for the run to park while running"
    );
    drop(cancel);
    clock.advance(Duration::from_millis(1));
    // The shell dies on TERM; the escaped grandchild holds stdout open, so
    // the run drains until the 2 s bound.
    let drain_until = clock.now() + Duration::from_secs(2);
    assert!(
        clock.await_parked(drain_until, DEADLINE),
        "waited {DEADLINE:?} for the run to park for the 2 s drain"
    );
    clock.advance(Duration::from_secs(2));
    let ran = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the drained run")
        .expect("the drained run returns");
    assert!(!ran.timed_out);
    // The escaped grandchild still holds the pipe: clean it by pid.
    for pid in _second {
        drop(fakes::kill_pid(pid, "KILL"));
    }
    watchdog.stand_down(DEADLINE);
}

#[test]
fn kill_every_group_kills_a_listed_live_group() {
    use std::os::unix::process::CommandExt as _;
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .unwrap();
    let pgid = child.id();
    let watchdog = fakes::Watchdog::group(pgid);
    super::groups::register(pgid);
    kill_every_group();
    let (done, waited) = mpsc::channel();
    std::thread::spawn(move || done.send(child.wait()).unwrap());
    let status = waited
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the killed child")
        .unwrap();
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(status.signal(), Some(9));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn group_ids_0_and_1_are_refused() {
    assert!(refused_group(0));
    assert!(refused_group(1));
    assert!(!refused_group(2));
}

#[test]
fn signal_name_pins_sigterm() {
    assert_eq!(signal_name(15), "SIGTERM");
}
