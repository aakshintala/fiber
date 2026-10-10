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

use super::{
    DRAIN, ExecRequest, GROUP_POLL, group_alive, kill_every_group, refused_group, signal_name,
};
/// How long a test waits on the run before it fails.
const DEADLINE: Duration = Duration::from_secs(10);
const DRAIN_BOUNDARY_DEADLINE: Duration = Duration::from_secs(4);

/// Runs `work` on a worker and returns its answer within `DEADLINE`: a call
/// that blocks, such as a `Command::status` or a fifo, is a wait too.
fn within<T: Send + 'static>(what: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = tx.send(work());
    });
    rx.recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("waited {DEADLINE:?} for {what}"))
}

/// Whether the group still holds a process: `kill -0` on a worker.
fn group_lives(pgid: u32) -> bool {
    within("kill -0 on the group", move || {
        fakes::kill_group(pgid, "0").unwrap()
    })
}

/// Sends SIGKILL to `pid` on a worker; an already-gone pid is fine.
fn kill_pid_now(pid: u32) {
    within("kill -KILL on the pid", move || {
        drop(fakes::kill_pid(pid, "KILL"));
    });
}

/// A ready fifo in `dir`, made on a worker: `mkfifo` is a `Command::status`.
fn ready_in(dir: &std::path::Path) -> fakes::children::Ready {
    let dir = dir.to_path_buf();
    within("the ready fifo", move || fakes::children::Ready::new(&dir))
}

/// The extension's memory cap in these tests, unless one sets its own.
const CAP: usize = 1 << 20;

fn request(program: &str, args: &[&str], cwd: PathBuf, cap: usize) -> ExecRequest {
    ExecRequest {
        program: program.into(),
        args: args.iter().map(|s| (*s).into()).collect(),
        cwd,
        cap,
        own_group: true,
    }
}

/// `sh -c script` staying in the test's process group: no group of its own.
fn sh_pid(script: &str, cwd: PathBuf, cap: usize) -> ExecRequest {
    ExecRequest {
        program: "sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd,
        cap,
        own_group: false,
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
    mpsc::Receiver<Result<super::Ran, Box<super::ExecError>>>,
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
    assert_eq!(err.code, contract::ErrorCode::NotFound);
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
    assert_eq!(err.code, contract::ErrorCode::TooLarge);
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
fn a_deadline_stop_reports_timed_out() {
    let (_dir, cwd) = dir("fiber-exec-deadline");
    let clock = FakeClock::new();
    let ready = ready_in(&cwd);
    let script = format!(
        "trap '' TERM\necho $$ > '{}'\nwhile :; do :; done\n",
        ready.path().display()
    );
    let deadline = clock.now() + Duration::from_millis(500);
    let (_cancel, done) = spawn(sh(&script, cwd, CAP), Arc::clone(&clock), Some(deadline));
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert!(
        clock.await_parked(clock.now() + GROUP_POLL, DEADLINE),
        "waited {DEADLINE:?} for the run to park while running"
    );
    // Past the callback's deadline the run stops, although nothing
    // dropped its cancel.
    clock.advance(Duration::from_millis(1000));
    let kill_at = clock.now() + Duration::from_millis(800);
    assert!(
        clock.await_parked(kill_at, DEADLINE),
        "waited {DEADLINE:?} for the run to park for the 800 ms grace"
    );
    clock.advance(Duration::from_millis(800));
    let ran = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the timed-out run")
        .expect("the timed-out run stops");
    assert_eq!(ran.signal.as_deref(), Some("SIGKILL"));
    assert!(ran.timed_out, "the deadline stopped the run");
    watchdog.stand_down(DEADLINE);
}

#[test]
fn cancel_sends_term_then_kill_after_800_ms() {
    let (_dir, cwd) = dir("fiber-exec-cancel");
    let clock = FakeClock::new();
    let ready = ready_in(&cwd);
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
    // The run polls its cancel every `GROUP_POLL` of real time, so it sees
    // the drop without a clock move; the stop starts at the frozen now.
    drop(cancel);
    let kill_at = clock.now() + Duration::from_millis(800);
    assert!(
        clock.await_parked(kill_at, DEADLINE),
        "waited {DEADLINE:?} for the run to park for the 800 ms grace"
    );
    assert!(group_lives(pgid), "SIGTERM leaves the ignoring group alive");
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
    let ready = ready_in(&cwd);
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
    // The run sees the drop on its own poll, at the frozen now.
    drop(cancel);
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
        kill_pid_now(pid);
    }
    watchdog.stand_down(DEADLINE);
}

/// The child's marker: set, a test that signals every listed group runs its
/// body in this process, which holds no other test's group.
const CHILD: &str = "FIBER_EXTENSIONS_EXEC_TEST_CHILD";

/// Runs the test `name` of this module alone in a child process and asserts
/// it passed: `kill_every_group` reaches every group this process lists,
/// so it runs where no other test's group is listed.
fn in_child(name: &str) {
    let module = module_path!().split_once("::").unwrap().1;
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{module}::{name}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(child.wait_with_output().unwrap()));
    let Ok(output) = rx.recv_timeout(DEADLINE) else {
        kill_pid_now(pid);
        panic!("waited {DEADLINE:?} for the child running {name}");
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{name} failed in its child: {stdout}"
    );
    assert!(
        stdout.contains("1 passed"),
        "the child ran exactly {name}: {stdout}"
    );
}

#[test]
fn kill_every_group_kills_a_listed_live_group() {
    if std::env::var_os(CHILD).is_none() {
        in_child("kill_every_group_kills_a_listed_live_group");
        return;
    }
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

/// Group ids no process can have (above `i32::MAX`): never signalled, so a
/// test may list them freely.
const NO_GROUP_A: u32 = 0x9000_0001;
const NO_GROUP_B: u32 = 0x9000_0002;

#[test]
fn finished_unlists_only_its_own_group_once_seen_empty() {
    super::groups::register(NO_GROUP_A);
    super::groups::register(NO_GROUP_B);
    super::finished(NO_GROUP_A, false);
    assert!(
        super::groups::listed().contains(&NO_GROUP_A),
        "a group not seen empty stays listed"
    );
    super::finished(NO_GROUP_A, true);
    let listed = super::groups::listed();
    assert!(!listed.contains(&NO_GROUP_A), "a seen-empty group leaves");
    assert!(listed.contains(&NO_GROUP_B), "another group stays");
    super::finished(NO_GROUP_B, true);
}

/// Each stream is bounded on its own: exactly the cap returns, one byte
/// more is refused. Each run writes one stream only, so its length alone
/// decides; the cap check reads the length the run ended with, so the
/// result does not depend on how the pipe splits the bytes into reads.
#[test]
fn output_of_exactly_the_cap_returns_and_one_byte_more_is_refused() {
    let (_dir, cwd) = dir("fiber-exec-cap-edge");
    let clock = FakeClock::new();
    let cap = 100;
    for (stream, redirect) in [("stdout", ""), ("stderr", " >&2")] {
        let (_cancel, done) = spawn(
            sh(
                &format!("head -c 100 /dev/zero | tr '\\0' x{redirect}"),
                cwd.clone(),
                cap,
            ),
            Arc::clone(&clock),
            None,
        );
        let ran = done
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|_| panic!("waited {DEADLINE:?} for the {stream} run at the cap"))
            .unwrap_or_else(|e| panic!("{stream} of exactly the cap returns: {}", e.message));
        let kept = if stream == "stdout" {
            ran.stdout.len()
        } else {
            ran.stderr.len()
        };
        assert_eq!(kept, cap, "every {stream} byte up to the cap is kept");
        let (_cancel, done) = spawn(
            sh(
                &format!("head -c 101 /dev/zero | tr '\\0' x{redirect}"),
                cwd.clone(),
                cap,
            ),
            Arc::clone(&clock),
            None,
        );
        let err = done
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|_| {
                panic!("waited {DEADLINE:?} for the {stream} run one byte past the cap")
            })
            .expect_err("one byte past the cap is refused");
        assert_eq!(
            err.message,
            format!("host.exec: sh: output passed the extension's memory cap of {cap} bytes"),
            "{stream}"
        );
    }
}

/// A member left in the group after the leader exits keeps the run going:
/// the group is not empty, so a cancel still sends SIGTERM and, past the
/// grace, SIGKILL, and the run returns only once the member is gone. The
/// member writes to neither pipe, so both streams reach EOF while it lives.
#[test]
fn a_member_outliving_the_leader_is_stopped_with_its_group() {
    let (_dir, cwd) = dir("fiber-exec-member");
    let clock = FakeClock::new();
    let ready = ready_in(&cwd);
    let ready_path = ready.path().display().to_string();
    let script = format!(
        "echo $$ > '{ready_path}'\n\
         sh -c 'trap \"\" TERM HUP; echo $$ >> \"$1\"; while :; do :; done' _ '{ready_path}' \
           >/dev/null 2>&1 &\n\
         exit 0\n"
    );
    let (cancel, done) = spawn(sh(&script, cwd, CAP), Arc::clone(&clock), None);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let _member = ready.wait(DEADLINE);
    drop(cancel);
    let kill_at = clock.now() + Duration::from_millis(800);
    assert!(
        clock.await_parked(kill_at, DEADLINE),
        "waited {DEADLINE:?} for the run to park for the 800 ms grace"
    );
    assert!(
        group_lives(pgid),
        "the member ignores SIGTERM, so the group lives through the grace"
    );
    clock.advance(Duration::from_millis(800));
    done.recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the stopped run")
        .expect("the stopped run returns");
    assert!(
        !group_lives(pgid),
        "the run returned only once the group was empty"
    );
    watchdog.stand_down(DEADLINE);
}

/// A stream still open keeps a stopped run draining although the group is
/// empty: an escaped process holds stdout while stderr is closed, so the
/// run reads until the 2 s bound, not until one stream ends.
#[test]
fn one_stream_still_open_keeps_the_drain_to_its_bound() {
    let (_dir, cwd) = dir("fiber-exec-one-open");
    let clock = FakeClock::new();
    let ready = ready_in(&cwd);
    let ready_path = ready.path().display().to_string();
    let script = format!(
        "echo $$ > '{ready_path}'\n\
         perl -MPOSIX -e 'POSIX::setsid(); $SIG{{HUP}} = \"IGNORE\"; $SIG{{TERM}} = \"IGNORE\"; \
           open my $f, \">>\", $ARGV[0] or die $!; print $f \"$$\\n\"; close $f; \
           sleep 3600 while 1' '{ready_path}' 2>/dev/null &\n\
         exit 0\n"
    );
    let (cancel, done) = spawn(sh(&script, cwd, CAP), Arc::clone(&clock), None);
    let pgid = ready.wait(DEADLINE)[0];
    let escaped = ready.wait(DEADLINE);
    let watchdog = fakes::Watchdog::group(pgid);
    drop(cancel);
    let drain_until = clock.now() + Duration::from_secs(2);
    assert!(
        clock.await_parked(drain_until, DEADLINE),
        "waited {DEADLINE:?} for the run to park for the 2 s drain"
    );
    clock.advance(Duration::from_secs(2));
    done.recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the drained run")
        .expect("the drained run returns");
    for pid in escaped {
        kill_pid_now(pid);
    }
    watchdog.stand_down(DEADLINE);
}

/// Spawns `script` under `sh` in its own listed group, as `run` does, with
/// no pipes: the startup abort below has no reader to wait for.
fn spawned(script: &str, cwd: &std::path::Path) -> std::process::Child {
    use std::os::unix::process::CommandExt as _;
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", script])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    super::spawn(&mut cmd).unwrap()
}

/// Runs `abort_startup` on a worker, its answer on the receiver.
fn abort(
    child: std::process::Child,
    cwd: PathBuf,
    clock: Arc<FakeClock>,
) -> mpsc::Receiver<Box<super::ExecError>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let req = sh("startup", cwd, CAP);
        let shared = super::Shared::default();
        let pgid = child.id();
        let clock_ref: Arc<dyn contract::clock::Clock> = clock;
        let err = super::abort_startup(
            &req,
            pgid,
            child,
            &shared,
            clock_ref.as_ref(),
            [false, false],
            std::io::Error::other("no reader thread"),
        );
        let _sent = tx.send(err);
    });
    rx
}

#[test]
fn a_startup_abort_kills_only_after_the_800_ms_grace() {
    let (_dir, cwd) = dir("fiber-exec-abort-grace");
    let clock = FakeClock::new();
    let ready = ready_in(&cwd);
    let script = format!(
        "trap '' TERM\necho $$ > '{}'\nwhile :; do :; done\n",
        ready.path().display()
    );
    let child = spawned(&script, &cwd);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let kill_at = clock.now() + Duration::from_millis(800);
    let done = abort(child, cwd, Arc::clone(&clock));
    assert!(
        clock.await_parked(kill_at, DEADLINE),
        "waited {DEADLINE:?} for the abort to park for the 800 ms grace"
    );
    assert!(
        group_lives(pgid),
        "SIGTERM leaves the ignoring group alive through the grace"
    );
    assert!(
        super::groups::listed().contains(&pgid),
        "a live group stays listed"
    );
    clock.advance(Duration::from_millis(800));
    let err = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the aborted run");
    assert_eq!(err.message, "host.exec: sh: no reader thread");
    assert_eq!(err.code, contract::ErrorCode::IoFailed);
    let ran = err.ran.expect("a started run is logged");
    assert_eq!(ran.signal.as_deref(), Some("SIGKILL"));
    assert!(
        !super::groups::listed().contains(&pgid),
        "the group leaves the list once seen empty"
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_startup_abort_of_a_group_that_ends_on_term_sends_no_kill() {
    let (_dir, cwd) = dir("fiber-exec-abort-term");
    let clock = FakeClock::new();
    let ready = ready_in(&cwd);
    let script = format!(
        "echo $$ > '{}'\nwhile :; do :; done\n",
        ready.path().display()
    );
    let child = spawned(&script, &cwd);
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let done = abort(child, cwd, Arc::clone(&clock));
    // No clock move: the group ends on SIGTERM inside the grace.
    let err = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the aborted run");
    let ran = err.ran.expect("a started run is logged");
    assert_eq!(ran.signal.as_deref(), Some("SIGTERM"));
    assert!(
        !super::groups::listed().contains(&pgid),
        "the group leaves the list once seen empty"
    );
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

#[test]
fn a_snapshot_with_no_end_takes_the_final_wait_status_and_keeps_its_own() {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;
    let snapshot = |exit_code: Option<i32>, signal: Option<&str>| super::Ran {
        exit_code,
        signal: signal.map(str::to_owned),
        stdout: b"out".to_vec(),
        stderr: Vec::new(),
        timed_out: false,
    };
    // A raw wait status: the exit code in the second byte, a signal in the first.
    let exited_3 = Some(ExitStatus::from_raw(3 << 8));
    let killed = Some(ExitStatus::from_raw(9));
    let filled = super::completed(snapshot(None, None), killed);
    assert_eq!(filled.signal.as_deref(), Some("SIGKILL"));
    assert_eq!(filled.exit_code, None);
    assert_eq!(filled.stdout, b"out");
    let filled = super::completed(snapshot(None, None), exited_3);
    assert_eq!((filled.exit_code, filled.signal), (Some(3), None));
    let kept = super::completed(snapshot(Some(0), None), killed);
    assert_eq!((kept.exit_code, kept.signal), (Some(0), None));
    let kept = super::completed(snapshot(None, Some("SIGTERM")), exited_3);
    assert_eq!(
        (kept.exit_code, kept.signal.as_deref()),
        (None, Some("SIGTERM"))
    );
    let unknown = super::completed(snapshot(None, None), None);
    assert_eq!((unknown.exit_code, unknown.signal), (None, None));
}

#[test]
fn a_missing_working_directory_is_not_found() {
    let (_dir, cwd) = dir("fiber-exec-missing-cwd");
    let clock = FakeClock::new();
    let (_cancel, done) = spawn(
        request("sh", &["-c", "exit 0"], cwd.join("gone"), CAP),
        Arc::clone(&clock),
        None,
    );
    let err = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the missing-cwd run")
        .expect_err("a missing working directory never runs");
    assert_eq!(err.code, contract::ErrorCode::NotFound);
    assert!(
        err.message.starts_with("host.exec: sh: "),
        "the spawn error names the program: {}",
        err.message
    );
    assert!(err.ran.is_none(), "a spawn failure never ran");
}

/// Without its own group the program stays in the test's process group:
/// `ps` prints the test process's group, and the test surviving proves no
/// group-wide signal ever went out.
#[test]
fn a_pid_mode_run_stays_in_the_test_process_group() {
    let (_dir, cwd) = dir("fiber-exec-pid-group");
    let clock = FakeClock::new();
    let (_cancel, done) = spawn(
        sh_pid("ps -o pgid= -p $$", cwd, CAP),
        Arc::clone(&clock),
        None,
    );
    let ran = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the pid-mode group run")
        .expect("the pid-mode group run spawns");
    let printed = String::from_utf8_lossy(&ran.stdout).trim().to_owned();
    let pgid: u32 = printed.parse().expect("the pid-mode run prints its pgid");
    let expected = u32::try_from(rustix::process::getpgrp().as_raw_pid())
        .expect("the test's process group fits in a u32");
    assert_eq!(
        pgid, expected,
        "a run without its own group stays in the test's group"
    );
}

/// At its deadline a run without its own group is stopped by a signal to
/// its pid alone: the test process shares the group and survives, and the
/// program is gone afterwards.
#[test]
fn a_pid_mode_deadline_stop_signals_only_the_pid() {
    let (_dir, cwd) = dir("fiber-exec-pid-deadline");
    let clock = FakeClock::new();
    let ready = ready_in(&cwd);
    let script = format!(
        "trap '' TERM\necho $$ > '{}'\nwhile :; do :; done\n",
        ready.path().display()
    );
    let deadline = clock.now() + Duration::from_millis(500);
    let (_cancel, done) = spawn(
        sh_pid(&script, cwd, CAP),
        Arc::clone(&clock),
        Some(deadline),
    );
    let pid = ready.wait(DEADLINE)[0];
    // The spinner shares this test's group, so the guard matches its argv
    // by pid alone, never the group: a panic anywhere below still kills it.
    let watchdog = fakes::Watchdog::matching(&ready.path().display().to_string());
    assert!(
        clock.await_parked(clock.now() + GROUP_POLL, DEADLINE),
        "waited {DEADLINE:?} for the pid-mode run to park while running"
    );
    // Past the deadline the run stops, although nothing dropped its cancel.
    clock.advance(Duration::from_millis(1000));
    let kill_at = clock.now() + Duration::from_millis(800);
    assert!(
        clock.await_parked(kill_at, DEADLINE),
        "waited {DEADLINE:?} for the pid-mode run to park for the 800 ms grace"
    );
    clock.advance(Duration::from_millis(800));
    let outcome = done.recv_timeout(DEADLINE);
    if outcome.is_err() {
        kill_pid_now(pid);
    }
    let ran = outcome
        .expect("waited {DEADLINE:?} for the timed-out pid-mode run")
        .expect("the timed-out pid-mode run stops");
    assert_eq!(ran.signal.as_deref(), Some("SIGKILL"));
    assert!(ran.timed_out, "the deadline stopped the pid-mode run");
    assert!(
        !fakes::kill_pid(pid, "0").expect("a pid probe runs"),
        "the stopped program is gone"
    );
    watchdog.stand_down(DEADLINE);
}

/// A run without its own group is never listed: neither while it runs nor
/// after it ends.
#[test]
fn a_pid_mode_run_is_never_listed() {
    let (_dir, cwd) = dir("fiber-exec-pid-listed");
    let clock = FakeClock::new();
    let ready = ready_in(&cwd);
    let script = format!(
        "echo $$ > '{}'\nwhile :; do :; done\n",
        ready.path().display()
    );
    let (cancel, done) = spawn(sh_pid(&script, cwd, CAP), Arc::clone(&clock), None);
    let pid = ready.wait(DEADLINE)[0];
    // The spinner shares this test's group: match its argv by pid alone.
    let watchdog = fakes::Watchdog::matching(&ready.path().display().to_string());
    assert!(
        !super::groups::listed().contains(&pid),
        "a pid-mode run is never listed while it runs"
    );
    drop(cancel);
    let outcome = done.recv_timeout(DEADLINE);
    if outcome.is_err() {
        kill_pid_now(pid);
    }
    outcome
        .expect("waited {DEADLINE:?} for the cancelled pid-mode run")
        .expect("the cancelled pid-mode run stops");
    assert!(
        !super::groups::listed().contains(&pid),
        "a pid-mode run is never listed after it ends"
    );
    assert!(
        !fakes::kill_pid(pid, "0").expect("a pid probe runs"),
        "the cancelled program is gone"
    );
    watchdog.stand_down(DEADLINE);
}

/// A program that cannot start reports its spawn failure, whatever group it
/// would have run in: `NotFound` for a missing program.
#[test]
fn a_pid_mode_missing_program_reports_its_spawn_error() {
    let (_dir, cwd) = dir("fiber-exec-pid-missing");
    let clock = FakeClock::new();
    let req = ExecRequest {
        program: "fiber-definitely-missing-xyz".into(),
        args: Vec::new(),
        cwd,
        cap: CAP,
        own_group: false,
    };
    let (_cancel, done) = spawn(req, Arc::clone(&clock), None);
    let err = done
        .recv_timeout(DEADLINE)
        .expect("waited {DEADLINE:?} for the missing pid-mode run")
        .expect_err("a missing program never runs");
    assert_eq!(err.code, contract::ErrorCode::NotFound);
    assert!(err.ran.is_none(), "a spawn failure never ran");
    assert!(
        err.source
            .as_ref()
            .is_some_and(|source| source.kind() == std::io::ErrorKind::NotFound),
        "the spawn failure carries its `NotFound` error"
    );
}

/// `stop_pid` on a child that has exited but is not yet reaped reaps it and
/// sends no signal. `waitid` with `EXITED | NOWAIT` blocks on a worker until
/// the child has exited and leaves it waitable, so the child is
/// deterministically unreaped when `stop_pid` runs.
#[test]
fn stop_pid_reaps_an_exited_child_without_signalling() {
    use rustix::process::{Signal, WaitId, WaitIdOptions};
    let (_dir, cwd) = dir("fiber-exec-stop-exited");
    let mut child = std::process::Command::new("sh")
        .args(["-c", "exit 0"])
        .current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the exited child spawns");
    let pid = rustix::process::Pid::from_raw(
        i32::try_from(child.id()).expect("the exited child's pid fits in an i32"),
    )
    .expect("the exited child's pid is not zero");
    fakes::within("the exited child", fakes::MUST_SUCCEED_WITHIN, move || {
        rustix::process::waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        )
        .expect("waitid sees the exit");
    });
    let shared = super::Shared::default();
    assert!(
        !super::stop_pid(&mut child, &shared, Signal::TERM),
        "an exited child is reaped, never signalled"
    );
    // `Child::try_wait` caches the status, so a second one cannot tell a
    // reap from an unreaped exit: the shared state shows the reap instead.
    let inner = super::lock(&shared.inner);
    assert!(inner.reaped, "the exited child is reaped afterwards");
    assert!(
        inner.status.as_ref().and_then(|status| status.code()) == Some(0),
        "the reap keeps the child's own exit"
    );
}

/// `stop_pid` on a running child signals it.
#[test]
fn stop_pid_signals_a_running_child() {
    use rustix::process::Signal;
    let (_dir, cwd) = dir("fiber-exec-stop-running");
    // The loop's comment carries the directory: the guard below matches
    // this spinner alone by its argv.
    let script = format!("while :; do :; done # {}", cwd.display());
    let mut child = std::process::Command::new("sh")
        .args(["-c", script.as_str()])
        .current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the running child spawns");
    let pid = child.id();
    let watchdog = fakes::Watchdog::matching(&cwd.display().to_string());
    let shared = super::Shared::default();
    assert!(
        super::stop_pid(&mut child, &shared, Signal::TERM),
        "a running child is signalled"
    );
    // SIGTERM ends the loop: the wait below, not the signal, is what the
    // test fails on when the signal never went out.
    let (done, waited) = mpsc::channel();
    std::thread::spawn(move || done.send(child.wait()));
    match waited.recv_timeout(DEADLINE) {
        Ok(status) => {
            use std::os::unix::process::ExitStatusExt as _;
            assert_eq!(
                status.expect("the running child reaps").signal(),
                Some(15),
                "the running child dies of SIGTERM"
            );
            watchdog.stand_down(DEADLINE);
        }
        Err(_) => {
            kill_pid_now(pid);
            panic!("waited {DEADLINE:?} for the signalled child to die");
        }
    }
}

/// A program that exits behind a helper holding the pipe ends at the drain,
/// not the deadline: the reap starts the 2 s bound, and the run returns the
/// child's own status with no timeout, well before any deadline.
#[test]
fn a_reaped_child_with_a_held_pipe_returns_at_the_drain() {
    let (_dir, cwd) = dir("fiber-exec-pid-drain");
    let clock = FakeClock::new();
    let holder_file = cwd.join("holder-pid");
    // The background `sleep` inherits stdout and holds the pipe open after
    // the shell exits, so EOF never comes on its own.
    let script = format!(
        "(exec sleep 1000) & echo $! > '{}'; echo out; exit 0",
        holder_file.display()
    );
    let deadline = clock.now() + Duration::from_secs(60);
    let (_cancel, done) = spawn(
        sh_pid(&script, cwd, CAP),
        Arc::clone(&clock),
        Some(deadline),
    );
    // The holder's argv carries its pid file: the guard matches it alone.
    let watchdog = fakes::Watchdog::matching(&holder_file.display().to_string());
    // The reap starts the 2 s drain on the fake clock.
    let drain_until = clock.now() + DRAIN;
    assert!(
        clock.mark_parked(drain_until, DEADLINE).is_some(),
        "waited {DEADLINE:?} for the pid-mode run to park for the 2 s drain"
    );
    clock.advance(DRAIN);
    let outcome = done.recv_timeout(DEADLINE);
    // The holder keeps the pipe whether the wait ended or not: kill it by
    // its pid file before asserting, so no `sleep` outlives the test.
    let holder: Option<u32> = std::fs::read_to_string(&holder_file)
        .ok()
        .and_then(|text| text.trim().parse().ok());
    if let Some(holder) = holder {
        kill_pid_now(holder);
    }
    let ran = outcome
        .unwrap_or_else(|_| panic!("waited {DEADLINE:?} for the drained pid-mode run"))
        .unwrap_or_else(|err| panic!("the drained pid-mode run returns: {}", err.message));
    assert_eq!(
        ran.exit_code,
        Some(0),
        "the run keeps the child's own status"
    );
    assert_eq!(ran.stdout, b"out\n");
    assert!(!ran.timed_out, "no stop started, so no timeout");
    assert!(
        clock.now() < deadline,
        "the run returned well before its deadline"
    );
    let holder = holder.expect("the script wrote its holder's pid");
    assert!(fakes::pids_exit(&[holder], DEADLINE), "the holder is gone");
    watchdog.stand_down(DEADLINE);
}

/// A pid-mode drain that crosses the deadline still returns the child's
/// own status with no timeout: once the reap starts draining, the deadline
/// never starts a stop. The deadline sits just before, exactly at, and
/// just after the drain bound.
#[test]
fn a_drain_crossing_the_deadline_returns_without_timeout() {
    let (_dir, cwd) = dir("fiber-exec-drain-deadline");
    let holder_file = cwd.join("holder-pid");
    // The background `sleep` inherits stdout and holds the pipe open after
    // the shell exits, so EOF never comes on its own.
    let script = format!(
        "(exec sleep 1000) & echo $! > '{}'; echo out; exit 0",
        holder_file.display()
    );
    // One step of the fake clock: the three deadlines straddle the drain
    // bound by it.
    let step = Duration::from_millis(1);
    // The holder's argv carries its pid file: one guard covers all cases.
    let watchdog = fakes::Watchdog::matching(&holder_file.display().to_string());
    for (case, shift) in [("before", -1), ("at", 0), ("after", 1)] {
        let clock = FakeClock::new();
        let drain_until = clock.now() + DRAIN;
        let deadline = if shift < 0 {
            drain_until
                .checked_sub(step)
                .expect("the deadline stays past the start")
        } else if shift > 0 {
            drain_until + step
        } else {
            drain_until
        };
        let (_cancel, done) = spawn(
            sh_pid(&script, cwd.clone(), CAP),
            Arc::clone(&clock),
            Some(deadline),
        );
        // The reap starts the 2 s drain on the fake clock.
        assert!(
            clock
                .mark_parked(drain_until, DRAIN_BOUNDARY_DEADLINE)
                .is_some(),
            "waited {DRAIN_BOUNDARY_DEADLINE:?} for the pid-mode run to park for the 2 s drain ({case})"
        );
        // Reach one step before the drain bound first. For the `before`
        // case this is also the deadline: expiry must not start a stop once
        // the child has been reaped and draining has begun.
        let before_drain = drain_until
            .checked_sub(step)
            .expect("the before-drain boundary stays after the start");
        let mark = clock.advance_marked(before_drain - clock.now());
        assert!(
            clock.await_parked_since(&mark, Some(drain_until), DRAIN_BOUNDARY_DEADLINE),
            "waited {DRAIN_BOUNDARY_DEADLINE:?} for the pid-mode run to keep draining ({case})"
        );
        assert!(
            matches!(done.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "the pid-mode run is still draining just before the bound ({case})"
        );

        // At the drain bound the child status wins for deadlines just before,
        // exactly at, and just after it. Do not jump past the bound before
        // observing the result.
        clock.advance(step);
        let outcome = done.recv_timeout(DRAIN_BOUNDARY_DEADLINE);
        // The holder keeps the pipe whether the wait ended or not: kill it
        // by its pid file before asserting, so no `sleep` outlives the test.
        let holder: Option<u32> = std::fs::read_to_string(&holder_file)
            .ok()
            .and_then(|text| text.trim().parse().ok());
        if let Some(holder) = holder {
            kill_pid_now(holder);
        }
        let ran = outcome
            .unwrap_or_else(|_| {
                panic!("waited {DRAIN_BOUNDARY_DEADLINE:?} for the drained run ({case})")
            })
            .unwrap_or_else(|err| panic!("the drained run returns ({case}): {}", err.message));
        assert_eq!(
            ran.exit_code,
            Some(0),
            "the run keeps the child's own status ({case})"
        );
        assert_eq!(ran.stdout, b"out\n", "the run keeps what it read ({case})");
        assert!(!ran.timed_out, "no stop started, so no timeout ({case})");
        let holder = holder.expect("the script wrote its holder's pid");
        assert!(
            fakes::pids_exit(&[holder], DRAIN_BOUNDARY_DEADLINE),
            "the holder is gone ({case})"
        );
    }
    watchdog.stand_down(DEADLINE);
}

/// A run without its own group is stopped by a signal to its pid: the
/// program dies of SIGTERM itself, so no grace elapses.
#[test]
fn a_pid_mode_term_stop_reports_sigterm() {
    let (_dir, cwd) = dir("fiber-exec-pid-term");
    let clock = FakeClock::new();
    let ready = ready_in(&cwd);
    let script = format!(
        "echo $$ > '{}'\nwhile :; do :; done\n",
        ready.path().display()
    );
    let deadline = clock.now() + Duration::from_millis(500);
    let (_cancel, done) = spawn(
        sh_pid(&script, cwd, CAP),
        Arc::clone(&clock),
        Some(deadline),
    );
    let pid = ready.wait(DEADLINE)[0];
    // The spinner shares this test's group: match its argv by pid alone.
    let watchdog = fakes::Watchdog::matching(&ready.path().display().to_string());
    assert!(
        clock.await_parked(clock.now() + GROUP_POLL, DEADLINE),
        "waited {DEADLINE:?} for the pid-mode run to park while running"
    );
    // Past the deadline the run stops, although nothing dropped its cancel.
    clock.advance(Duration::from_millis(1000));
    let outcome = done.recv_timeout(DEADLINE);
    if outcome.is_err() {
        kill_pid_now(pid);
    }
    let ran = outcome
        .expect("waited {DEADLINE:?} for the stopped pid-mode run")
        .expect("the stopped pid-mode run returns");
    assert_eq!(ran.signal.as_deref(), Some("SIGTERM"));
    assert!(ran.timed_out, "the deadline stopped the pid-mode run");
    assert!(
        !fakes::kill_pid(pid, "0").expect("a pid probe runs"),
        "the stopped program is gone"
    );
    watchdog.stand_down(DEADLINE);
}

/// A startup abort without its own group signals only the pid: the child
/// shares the test's process group, which survives, and the child is gone
/// afterwards, never listed.
#[test]
fn a_pid_mode_startup_abort_signals_only_the_pid() {
    let (_dir, cwd) = dir("fiber-exec-abort-pid");
    let clock = FakeClock::new();
    let ready = ready_in(&cwd);
    let script = format!(
        "echo $$ > '{}'\nwhile :; do :; done\n",
        ready.path().display()
    );
    // A plain spawn, no process group of its own: the child shares the
    // test's group, so a group signal would reach the test itself.
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", &script])
        .current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = cmd.spawn().expect("the pid-mode child spawns");
    let pid = ready.wait(DEADLINE)[0];
    // The spinner shares this test's group: match its argv by pid alone.
    let watchdog = fakes::Watchdog::matching(&ready.path().display().to_string());
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let req = sh_pid("startup", cwd, CAP);
        let shared = super::Shared::default();
        let pgid = pid;
        let clock_ref: Arc<dyn contract::clock::Clock> = clock;
        let err = super::abort_startup(
            &req,
            pgid,
            child,
            &shared,
            clock_ref.as_ref(),
            [false, false],
            std::io::Error::other("no reader thread"),
        );
        let _sent = done_tx.send(err);
    });
    let outcome = done_rx.recv_timeout(DEADLINE);
    if outcome.is_err() {
        kill_pid_now(pid);
    }
    let err = outcome.expect("waited {DEADLINE:?} for the aborted pid-mode run");
    assert_eq!(err.message, "host.exec: sh: no reader thread");
    assert_eq!(err.code, contract::ErrorCode::IoFailed);
    let ran = err.ran.expect("a started run is logged");
    assert_eq!(ran.signal.as_deref(), Some("SIGTERM"));
    assert!(
        !super::groups::listed().contains(&pid),
        "a pid-mode abort never lists"
    );
    assert!(
        !fakes::kill_pid(pid, "0").expect("a pid probe runs"),
        "the aborted program is gone"
    );
    watchdog.stand_down(DEADLINE);
}
