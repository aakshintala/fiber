//! The run: spawn, readers, the stop sequence and the shared group list
//! (`docs/extensions.md`, "Host calls"; `docs/tools.md`, "Stopping a command").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use contract::clock::Clock as _;
use fakes::Deadline;
use fakes::clock::FakeClock;

use super::ExecRequest;
use super::resolve_in_package;
use support::group::{self, DRAIN, GROUP_POLL};
/// How long a test waits on the run before it fails.
const DEADLINE: Duration = Duration::from_secs(10);
const DRAIN_BOUNDARY_DEADLINE: Duration = Duration::from_secs(4);

/// Whether the group still holds a process: `kill -0` starts no process,
/// so it runs directly.
#[track_caller]
fn group_lives(pgid: u32) -> bool {
    fakes::kill_group(pgid, "0").unwrap()
}

/// A ready fifo in `dir`, made on a worker: `mkfifo` is a `Command::status`.
#[track_caller]
fn ready_in(dir: &std::path::Path) -> fakes::children::Ready {
    let dir = dir.to_path_buf();
    fakes::within("the ready fifo", Deadline::after(DEADLINE), move || {
        fakes::children::Ready::new(&dir)
    })
}

/// The extension's memory cap in these tests, unless one sets its own.
const CAP: usize = 1 << 20;

fn request(program: &str, args: &[&str], cwd: PathBuf, cap: usize) -> ExecRequest {
    ExecRequest {
        program: program.into(),
        args: args.iter().map(|s| (*s).into()).collect(),
        cwd,
        env: BTreeMap::new(),
        input: None,
        cap,
        own_group: true,
        stdout_read: None,
    }
}

/// `sh -c script` staying in the test's process group: no group of its own.
fn sh_pid(script: &str, cwd: PathBuf, cap: usize) -> ExecRequest {
    ExecRequest {
        program: "sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd,
        env: BTreeMap::new(),
        input: None,
        cap,
        own_group: false,
        stdout_read: None,
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

/// Steps the clock to `deadline` through each poll park: each pass names
/// the next park (`deadline` or one `GROUP_POLL` ahead), so the run never
/// moves past a park it holds.
#[track_caller]
fn step_to_deadline(clock: &FakeClock, deadline: Instant) {
    while clock.now() < deadline {
        let park = deadline.min(clock.now() + GROUP_POLL);
        clock.advance_when_parked(
            Some(park),
            None,
            park - clock.now(),
            &Deadline::after(DEADLINE),
        );
    }
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
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
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
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
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
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the group run")
        .expect("the group run spawns");
    let printed = String::from_utf8_lossy(&ran.stdout).trim().to_owned();
    let pgid: u32 = printed.parse().expect("the group run prints its pgid");
    assert!(pgid > 1, "the child has a real group id");
    // Its own group: `ps` prints the pid's group, which is the child's pid.
    // The run reaps the child, so the group is empty now; the print proves
    // the child saw itself as a leader while it ran.
    assert!(!group::alive(pgid), "the reaped group is empty");
}

#[test]
fn cwd_is_the_given_directory() {
    let (_dir, cwd) = dir("fiber-exec-cwd");
    let clock = FakeClock::new();
    let (_cancel, done) = spawn(sh("pwd", cwd.clone(), CAP), Arc::clone(&clock), None);
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
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
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the env run")
        .expect("the env run spawns");
    assert_eq!(String::from_utf8_lossy(&ran.stdout), home);
}

#[test]
fn env_overrides_an_inherited_variable() {
    let (_dir, cwd) = dir("fiber-exec-env-override");
    let clock = FakeClock::new();
    let mut req = sh("printf %s \"$HOME\"", cwd, CAP);
    req.env.insert("HOME".to_owned(), "/from-env".to_owned());
    let (_cancel, done) = spawn(req, Arc::clone(&clock), None);
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the env run")
        .expect("the env run spawns");
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "/from-env");
}

#[test]
fn env_is_empty_by_default() {
    let (_dir, cwd) = dir("fiber-exec-env-default");
    assert!(sh("exit 0", cwd, CAP).env.is_empty());
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
    let err = Deadline::after(DEADLINE)
        .recv(&done)
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

/// A group error no spawn returns is `io_failed` with nothing logged: the
/// refusal names the program and the cause.
#[test]
fn a_group_error_no_spawn_returns_is_io_failed() {
    let (_dir, cwd) = dir("fiber-exec-spawn-error");
    let req = request("sh", &["-c", "exit 0"], cwd, CAP);
    let err = super::spawn_error(&req, group::Error::Refused(0));
    assert_eq!(err.code, contract::ErrorCode::IoFailed);
    assert_eq!(
        err.message,
        format!("host.exec: sh: {}", group::Error::Refused(0)),
        "the refusal names the program and the cause"
    );
    assert!(err.ran.is_none(), "a spawn failure never ran");
    assert!(err.source.is_none(), "a refusal carries no spawn error");
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
    let err = Deadline::after(DEADLINE)
        .recv(&done)
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
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    // At the callback's deadline the run stops, although nothing
    // dropped its cancel.
    step_to_deadline(&clock, deadline);
    let kill_at = clock.now() + Duration::from_millis(800);
    clock.advance_when_parked(
        Some(kill_at),
        None,
        Duration::from_millis(800),
        &Deadline::after(DEADLINE),
    );
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the timed-out run")
        .expect("the timed-out run stops");
    assert_eq!(ran.signal.as_deref(), Some("SIGKILL"));
    assert!(ran.timed_out, "the deadline stopped the run");
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
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
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let running_until = clock.now() + GROUP_POLL;
    assert!(
        clock.await_parked(running_until, fakes::Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the run to park while running"
    );
    // The run polls its cancel every `GROUP_POLL` of real time, so it sees
    // the drop without a clock move; the stop starts at the frozen now.
    drop(cancel);
    let kill_at = clock.now() + Duration::from_millis(800);
    assert!(
        clock.await_parked(kill_at, fakes::Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the run to park for the 800 ms grace"
    );
    assert!(group_lives(pgid), "SIGTERM leaves the ignoring group alive");
    clock.advance_when_parked(
        Some(kill_at),
        None,
        Duration::from_millis(800),
        &Deadline::after(DEADLINE),
    );
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the cancelled run")
        .expect("the cancelled run stops");
    assert_eq!(ran.signal.as_deref(), Some("SIGKILL"));
    assert_eq!(ran.exit_code, None);
    assert!(!ran.timed_out, "a drop is not the deadline");
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn output_held_open_after_the_kill_stops_after_2_s() {
    let (_dir, cwd) = dir("fiber-exec-drain");
    let clock = FakeClock::new();
    let ready = ready_in(&cwd);
    let script = fakes::children::escapes_group(ready.path());
    let (cancel, done) = spawn(sh(&script, cwd, CAP), Arc::clone(&clock), None);
    let first = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let _second = ready.wait(fakes::Deadline::after(DEADLINE));
    let watchdog = fakes::Watchdog::group(first);
    let running_until = clock.now() + GROUP_POLL;
    assert!(
        clock.await_parked(running_until, fakes::Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the run to park while running"
    );
    // The run sees the drop on its own poll, at the frozen now.
    drop(cancel);
    // The shell dies on TERM; the escaped grandchild holds stdout open, so
    // the run drains until the 2 s bound.
    let drain_until = clock.now() + Duration::from_secs(2);
    assert!(
        clock.await_parked(drain_until, fakes::Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the run to park for the 2 s drain"
    );
    clock.advance_when_parked(
        Some(drain_until),
        None,
        Duration::from_secs(2),
        &Deadline::after(DEADLINE),
    );
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the drained run")
        .expect("the drained run returns");
    assert!(!ran.timed_out);
    // The escaped grandchild still holds the pipe: clean it by pid.
    for pid in _second {
        drop(fakes::kill_pid(pid, "KILL"));
    }
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

/// The child's marker: set, a test that signals every listed group runs its
/// body in this process, which holds no other test's group.
const CHILD: &str = "FIBER_EXTENSIONS_EXEC_TEST_CHILD";

/// Runs the test `name` of this module alone in a child process and asserts
/// it passed: `kill_every_group` reaches every group this process lists,
/// so it runs where no other test's group is listed.
#[track_caller]
fn in_child(name: &str) {
    use std::os::unix::process::CommandExt as _;
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
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = child.id();
    // Its own group, reaped on every path: dropping the watchdog kills
    // the group when this test panics or its wait below times out.
    let watchdog = fakes::Watchdog::group(pid);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(child.wait_with_output().unwrap()));
    let Ok(output) = Deadline::after(DEADLINE).recv(&rx) else {
        drop(fakes::kill_pid(pid, "KILL"));
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
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
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
    let _listed = group::live().list(pgid).expect("the sleep group lists");
    group::kill_every_group();
    let (done, waited) = mpsc::channel();
    std::thread::spawn(move || done.send(child.wait()).unwrap());
    let status = Deadline::after(DEADLINE)
        .recv(&waited)
        .expect("waited {DEADLINE:?} for the killed child")
        .unwrap();
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(status.signal(), Some(9));
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

/// Group ids no process can have (above `i32::MAX`): never signalled, so a
/// test may list them freely.
const NO_GROUP_A: u32 = 0x9000_0001;
const NO_GROUP_B: u32 = 0x9000_0002;

#[test]
fn unlisting_drops_only_the_entry_its_token_names() {
    let stale = group::live().list(NO_GROUP_A).expect("group A lists");
    let kept = group::live().list(NO_GROUP_B).expect("group B lists");
    group::live().unlist(stale);
    assert!(
        !group::live().contains(NO_GROUP_A),
        "the unlisted group leaves"
    );
    assert!(group::live().contains(NO_GROUP_B), "another group stays");
    group::live().unlist(kept);
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
        let ran = Deadline::after(DEADLINE)
            .recv(&done)
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
        let err = Deadline::after(DEADLINE)
            .recv(&done)
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
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let _member = ready.wait(fakes::Deadline::after(DEADLINE));
    drop(cancel);
    let kill_at = clock.now() + Duration::from_millis(800);
    assert!(
        clock.await_parked(kill_at, fakes::Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the run to park for the 800 ms grace"
    );
    assert!(
        group_lives(pgid),
        "the member ignores SIGTERM, so the group lives through the grace"
    );
    clock.advance_when_parked(
        Some(kill_at),
        None,
        Duration::from_millis(800),
        &Deadline::after(DEADLINE),
    );
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the stopped run")
        .expect("the stopped run returns");
    assert!(
        !group_lives(pgid),
        "the run returned only once the group was empty"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
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
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let escaped = ready.wait(fakes::Deadline::after(DEADLINE));
    let watchdog = fakes::Watchdog::group(pgid);
    drop(cancel);
    let drain_until = clock.now() + Duration::from_secs(2);
    assert!(
        clock.await_parked(drain_until, fakes::Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the run to park for the 2 s drain"
    );
    clock.advance_when_parked(
        Some(drain_until),
        None,
        Duration::from_secs(2),
        &Deadline::after(DEADLINE),
    );
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the drained run")
        .expect("the drained run returns");
    for pid in escaped {
        drop(fakes::kill_pid(pid, "KILL"));
    }
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

/// Spawns `script` under `sh` in its own listed group, as `run` does, with
/// no pipes: the startup abort below has no reader to wait for.
fn spawned(script: &str, cwd: &std::path::Path) -> (std::process::Child, group::Listing) {
    use std::os::unix::process::CommandExt as _;
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", script])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    group::spawn(&mut cmd).expect("the group spawns")
}

/// Runs `abort_startup` on a worker, its answer on the receiver.
fn abort(
    child: std::process::Child,
    listing: group::Listing,
    cwd: PathBuf,
    clock: Arc<FakeClock>,
) -> mpsc::Receiver<Box<super::ExecError>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let req = sh("startup", cwd, CAP);
        let shared = super::Shared::default();
        let clock_ref: Arc<dyn contract::clock::Clock> = clock;
        let err = super::abort_startup(
            &req,
            Some(listing),
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
    let (child, listing) = spawned(&script, &cwd);
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let kill_at = clock.now() + Duration::from_millis(800);
    let done = abort(child, listing, cwd, Arc::clone(&clock));
    assert!(
        clock.await_parked(kill_at, fakes::Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the abort to park for the 800 ms grace"
    );
    assert!(
        group_lives(pgid),
        "SIGTERM leaves the ignoring group alive through the grace"
    );
    assert!(group::live().contains(pgid), "a live group stays listed");
    clock.advance_when_parked(
        Some(kill_at),
        None,
        Duration::from_millis(800),
        &Deadline::after(DEADLINE),
    );
    let err = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the aborted run");
    assert_eq!(err.message, "host.exec: sh: no reader thread");
    assert_eq!(err.code, contract::ErrorCode::IoFailed);
    let ran = err.ran.expect("a started run is logged");
    assert_eq!(ran.signal.as_deref(), Some("SIGKILL"));
    assert!(
        !group::live().contains(pgid),
        "the group leaves the list once seen empty"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
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
    let (child, listing) = spawned(&script, &cwd);
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let done = abort(child, listing, cwd, Arc::clone(&clock));
    // No clock move: the group ends on SIGTERM inside the grace.
    let err = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the aborted run");
    let ran = err.ran.expect("a started run is logged");
    assert_eq!(ran.signal.as_deref(), Some("SIGTERM"));
    assert!(
        !group::live().contains(pgid),
        "the group leaves the list once seen empty"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
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
    let err = Deadline::after(DEADLINE)
        .recv(&done)
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
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
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
    let pid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    // The spinner shares this test's group, so the guard matches its argv
    // by pid alone, never the group: a panic anywhere below still kills it.
    let watchdog = fakes::Watchdog::matching(&ready.path().display().to_string());
    // At the deadline the run stops, although nothing dropped its cancel.
    step_to_deadline(&clock, deadline);
    let kill_at = clock.now() + Duration::from_millis(800);
    let early = crate::stall::await_grace_or_answer(&clock, &done, kill_at);
    let early = crate::stall::advance_past_grace_or_answer(
        &clock,
        &done,
        kill_at,
        Duration::from_millis(800),
        early,
    );
    let outcome = match early {
        Some(outcome) => Ok(outcome),
        None => Deadline::after(DEADLINE).recv(&done),
    };
    if outcome.is_err() {
        drop(fakes::kill_pid(pid, "KILL"));
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
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
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
    let pid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    // The spinner shares this test's group: match its argv by pid alone.
    let watchdog = fakes::Watchdog::matching(&ready.path().display().to_string());
    assert!(
        !group::live().contains(pid),
        "a pid-mode run is never listed while it runs"
    );
    drop(cancel);
    let outcome = Deadline::after(DEADLINE).recv(&done);
    if outcome.is_err() {
        drop(fakes::kill_pid(pid, "KILL"));
    }
    outcome
        .expect("waited {DEADLINE:?} for the cancelled pid-mode run")
        .expect("the cancelled pid-mode run stops");
    assert!(
        !group::live().contains(pid),
        "a pid-mode run is never listed after it ends"
    );
    assert!(
        !fakes::kill_pid(pid, "0").expect("a pid probe runs"),
        "the cancelled program is gone"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
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
        env: BTreeMap::new(),
        input: None,
        cap: CAP,
        own_group: false,
        stdout_read: None,
    };
    let (_cancel, done) = spawn(req, Arc::clone(&clock), None);
    let err = Deadline::after(DEADLINE)
        .recv(&done)
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
    fakes::within(
        "the exited child",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            rustix::process::waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
            )
            .expect("waitid sees the exit");
        },
    );
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
    match Deadline::after(DEADLINE).recv(&waited) {
        Ok(status) => {
            use std::os::unix::process::ExitStatusExt as _;
            assert_eq!(
                status.expect("the running child reaps").signal(),
                Some(15),
                "the running child dies of SIGTERM"
            );
            watchdog.stand_down(fakes::Deadline::after(DEADLINE));
        }
        Err(_) => {
            drop(fakes::kill_pid(pid, "KILL"));
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
    let (stdout_read_tx, stdout_read_rx) = mpsc::channel();
    let mut req = sh_pid(&script, cwd, CAP);
    req.stdout_read = Some(stdout_read_tx);
    let (_cancel, done) = spawn(req, Arc::clone(&clock), Some(deadline));
    // The holder's argv carries its pid file: the guard matches it alone.
    let watchdog = fakes::Watchdog::matching(&holder_file.display().to_string());
    // The reap starts the 2 s drain on the fake clock.
    let drain_until = clock.now() + DRAIN;
    clock
        .mark_parked(drain_until, fakes::Deadline::after(DEADLINE))
        .expect("waited {DEADLINE:?} for the pid-mode run to park for the 2 s drain");
    Deadline::after(DRAIN_BOUNDARY_DEADLINE)
        .recv(&stdout_read_rx)
        .expect("waited for stdout to be read before the drain bound");
    let unanswered_before_advance = matches!(done.try_recv(), Err(mpsc::TryRecvError::Empty));
    clock.advance_when_parked(Some(drain_until), None, DRAIN, &Deadline::after(DEADLINE));
    let outcome = Deadline::after(DEADLINE).recv(&done);
    // The holder keeps the pipe whether the wait ended or not: kill it by
    // its pid file before asserting, so no `sleep` outlives the test.
    let holder: Option<u32> = std::fs::read_to_string(&holder_file)
        .ok()
        .and_then(|text| text.trim().parse().ok());
    if let Some(holder) = holder {
        drop(fakes::kill_pid(holder, "KILL"));
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
    assert!(
        fakes::pids_exit(&[holder], fakes::Deadline::after(DEADLINE)),
        "the holder is gone"
    );
    assert!(
        unanswered_before_advance,
        "the run is unanswered while it is still draining"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
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
        let (stdout_read_tx, stdout_read_rx) = mpsc::channel();
        let mut req = sh_pid(&script, cwd.clone(), CAP);
        req.stdout_read = Some(stdout_read_tx);
        let (_cancel, done) = spawn(req, Arc::clone(&clock), Some(deadline));
        // The reap starts the 2 s drain on the fake clock.
        clock
            .mark_parked(drain_until, fakes::Deadline::after(DRAIN_BOUNDARY_DEADLINE))
            .unwrap_or_else(|| {
                panic!("waited {DRAIN_BOUNDARY_DEADLINE:?} for the pid-mode run to park for the 2 s drain ({case})")
            });
        Deadline::after(DRAIN_BOUNDARY_DEADLINE)
            .recv(&stdout_read_rx)
            .unwrap_or_else(|_| {
                panic!("waited for stdout to be read before the drain bound ({case})")
            });
        let unanswered_before_advance = matches!(done.try_recv(), Err(mpsc::TryRecvError::Empty));
        // Reach one step before the drain bound first. For the `before`
        // case this is also the deadline: expiry must not start a stop once
        // the child has been reaped and draining has begun.
        let before_drain = drain_until
            .checked_sub(step)
            .expect("the before-drain boundary stays after the start");
        let mark = clock.advance_when_parked(
            Some(drain_until),
            None,
            before_drain - clock.now(),
            &Deadline::after(DEADLINE),
        );
        assert!(
            clock.await_parked_since(
                &mark,
                Some(drain_until),
                fakes::Deadline::after(DRAIN_BOUNDARY_DEADLINE)
            ),
            "waited {DRAIN_BOUNDARY_DEADLINE:?} for the pid-mode run to keep draining ({case})"
        );
        let unanswered_before_bound = matches!(done.try_recv(), Err(mpsc::TryRecvError::Empty));

        // At the drain bound the child status wins for deadlines just before,
        // exactly at, and just after it. Do not jump past the bound before
        // observing the result.
        clock.advance_when_parked(Some(drain_until), None, step, &Deadline::after(DEADLINE));
        let outcome = Deadline::after(DRAIN_BOUNDARY_DEADLINE).recv(&done);
        // The holder keeps the pipe whether the wait ended or not: kill it
        // by its pid file before asserting, so no `sleep` outlives the test.
        let holder: Option<u32> = std::fs::read_to_string(&holder_file)
            .ok()
            .and_then(|text| text.trim().parse().ok());
        if let Some(holder) = holder {
            drop(fakes::kill_pid(holder, "KILL"));
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
            fakes::pids_exit(&[holder], fakes::Deadline::after(DRAIN_BOUNDARY_DEADLINE)),
            "the holder is gone ({case})"
        );
        assert!(
            unanswered_before_advance,
            "the run is unanswered while it is still draining ({case})"
        );
        assert!(
            unanswered_before_bound,
            "the pid-mode run is still draining just before the bound ({case})"
        );
    }
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
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
    let pid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    // The spinner shares this test's group: match its argv by pid alone.
    let watchdog = fakes::Watchdog::matching(&ready.path().display().to_string());
    // At the deadline the run stops, although nothing dropped its cancel.
    step_to_deadline(&clock, deadline);
    let outcome = Deadline::after(DEADLINE).recv(&done);
    if outcome.is_err() {
        drop(fakes::kill_pid(pid, "KILL"));
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
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
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
    let pid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    // The spinner shares this test's group: match its argv by pid alone.
    let watchdog = fakes::Watchdog::matching(&ready.path().display().to_string());
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let req = sh_pid("startup", cwd, CAP);
        let shared = super::Shared::default();
        let clock_ref: Arc<dyn contract::clock::Clock> = clock;
        let err = super::abort_startup(
            &req,
            None,
            child,
            &shared,
            clock_ref.as_ref(),
            [false, false],
            std::io::Error::other("no reader thread"),
        );
        let _sent = done_tx.send(err);
    });
    let outcome = Deadline::after(DEADLINE).recv(&done_rx);
    if outcome.is_err() {
        drop(fakes::kill_pid(pid, "KILL"));
    }
    let err = outcome.expect("waited {DEADLINE:?} for the aborted pid-mode run");
    assert_eq!(err.message, "host.exec: sh: no reader thread");
    assert_eq!(err.code, contract::ErrorCode::IoFailed);
    let ran = err.ran.expect("a started run is logged");
    assert_eq!(ran.signal.as_deref(), Some("SIGTERM"));
    assert!(!group::live().contains(pid), "a pid-mode abort never lists");
    assert!(
        !fakes::kill_pid(pid, "0").expect("a pid probe runs"),
        "the aborted program is gone"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

/// A run that starts after the kill pass is killed at its listing: the
/// shutdown kill reaches the new group before it runs, so the run ends
/// with its signal instead of an error. Runs in a child process: the pass
/// sets a sticky flag in this process.
#[test]
fn a_run_after_the_kill_pass_is_killed_at_its_listing() {
    if std::env::var_os(CHILD).is_none() {
        in_child("a_run_after_the_kill_pass_is_killed_at_its_listing");
        return;
    }
    use std::os::unix::process::CommandExt as _;
    let (_dir, cwd) = dir("fiber-exec-post-kill");
    let mut first = std::process::Command::new("sleep");
    first.arg("60").process_group(0);
    let (mut child_a, listing_a) = group::spawn(&mut first).expect("group A spawns");
    let pgid_a = child_a.id();
    let watchdog_a = fakes::Watchdog::group(pgid_a);
    group::kill_every_group();
    let (done, waited) = mpsc::channel();
    std::thread::spawn(move || done.send(child_a.wait()).unwrap());
    let reaped = Deadline::after(DEADLINE)
        .recv(&waited)
        .expect("waited {DEADLINE:?} for the killed child")
        .unwrap();
    assert!(
        fakes::group_empties(pgid_a, fakes::Deadline::after(DEADLINE)),
        "the pass killed group A"
    );
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(reaped.signal(), Some(9));
    group::live().unlist(listing_a);
    // A spawn after the pass still lists, already SIGKILLed at its
    // listing: the run below ends with that signal, not an error.
    let marker = format!("fiber-exec-post-kill-{}", cwd.display());
    let script = format!("sleep 30 # {marker}");
    let _guard = fakes::Watchdog::matching(&marker);
    let clock = FakeClock::new();
    let (_cancel, done) = spawn(sh(&script, cwd, CAP), Arc::clone(&clock), None);
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the post-kill run")
        .expect("the post-kill run ends with its signal");
    assert_eq!(ran.signal.as_deref(), Some("SIGKILL"));
    watchdog_a.stand_down(fakes::Deadline::after(DEADLINE));
}

/// The package directory, canonicalized as `Vm::load` does, with its guard.
fn package(name: &str) -> (fakes::TempDir, PathBuf) {
    let dir = fakes::TempDir::new(name);
    let path = dir.path().canonicalize().unwrap();
    (dir, path)
}

#[test]
fn resolve_in_package_a_slash_relative_program_resolves_inside_the_package() {
    let (_guard, package) = package("fiber-resolve-inside");
    std::fs::create_dir_all(package.join("bin")).unwrap();
    let expected = package.join("bin").join("acme").display().to_string();
    for program in ["bin/acme", "./bin/acme", "bin/../bin/acme"] {
        assert_eq!(
            resolve_in_package(program, &package),
            Ok(expected.clone()),
            "{program}"
        );
    }
}

#[test]
fn resolve_in_package_a_dot_dot_past_the_package_is_refused_naming_both() {
    let (_guard, package) = package("fiber-resolve-escape");
    for program in ["../x", "bin/../../x"] {
        let err = resolve_in_package(program, &package).unwrap_err();
        assert!(err.contains(program), "the refusal names the path: {err}");
        assert!(
            err.contains(package.display().to_string().as_str()),
            "the refusal names the directory: {err}"
        );
    }
}

#[test]
fn resolve_in_package_a_dot_dot_that_stays_inside_resolves() {
    let (_guard, package) = package("fiber-resolve-fold");
    assert_eq!(
        resolve_in_package("a/../b", &package),
        Ok(package.join("b").display().to_string())
    );
    assert_eq!(
        resolve_in_package("x/..", &package),
        Ok(package.display().to_string())
    );
    let err = resolve_in_package("a/../../b", &package).unwrap_err();
    assert!(err.contains("a/../../b"), "{err}");
}

#[test]
fn resolve_in_package_a_dot_dot_that_pops_past_the_package_is_refused_even_when_it_reenters() {
    let (_guard, package) = package("fiber-resolve-reentry");
    std::fs::create_dir_all(package.join("bin")).unwrap();
    std::fs::write(package.join("bin").join("ok"), "x").unwrap();
    // The second `..` pops past the package, which is a refusal even
    // though the path re-enters it and exists: the fold never climbs
    // back in. A fold that miscounted its depth would resolve this.
    let base = package.file_name().unwrap().to_str().unwrap().to_owned();
    let program = format!("a/../../{base}/bin/ok");
    let err = resolve_in_package(&program, &package).unwrap_err();
    assert!(
        err.contains(program.as_str()),
        "the refusal names the path: {err}"
    );
    assert!(
        err.contains(package.display().to_string().as_str()),
        "the refusal names the directory: {err}"
    );
}

#[test]
fn resolve_in_package_a_nul_byte_is_refused() {
    let (_guard, package) = package("fiber-resolve-nul");
    // A NUL byte can never name a runnable program: canonicalizing it
    // fails with an error other than `NotFound`, which is a refusal,
    // never a pass. A check that popped past every error would resolve it.
    let program = "bin/\0tool";
    let err = resolve_in_package(program, &package).unwrap_err();
    assert!(err.contains(program), "the refusal names the path: {err}");
    assert!(
        err.contains(package.display().to_string().as_str()),
        "the refusal names the directory: {err}"
    );
}

#[test]
fn resolve_in_package_a_symlink_out_of_the_package_is_refused() {
    use std::os::unix::fs::symlink;
    let (_outer, outer) = package("fiber-resolve-link-outer");
    let (_guard, package) = package("fiber-resolve-link-out");
    std::fs::create_dir_all(package.join("bin")).unwrap();
    symlink(&outer, package.join("bin").join("link")).unwrap();
    for program in ["bin/link", "bin/link/tool"] {
        let err = resolve_in_package(program, &package).unwrap_err();
        assert!(err.contains(program), "the refusal names the path: {err}");
        assert!(
            err.contains(package.display().to_string().as_str()),
            "the refusal names the directory: {err}"
        );
    }
}

#[test]
fn resolve_in_package_a_symlink_that_stays_inside_resolves() {
    use std::os::unix::fs::symlink;
    let (_guard, package) = package("fiber-resolve-link-in");
    std::fs::create_dir_all(package.join("bin")).unwrap();
    std::fs::write(package.join("bin").join("real"), "x").unwrap();
    symlink("real", package.join("bin").join("link")).unwrap();
    assert_eq!(
        resolve_in_package("bin/link", &package),
        Ok(package.join("bin").join("link").display().to_string())
    );
}

#[test]
fn resolve_in_package_a_symlink_loop_is_refused() {
    use std::os::unix::fs::symlink;
    let (_guard, package) = package("fiber-resolve-loop");
    symlink("b", package.join("a")).unwrap();
    symlink("a", package.join("b")).unwrap();
    for program in ["./a", "a/tool"] {
        let err = resolve_in_package(program, &package).unwrap_err();
        assert!(err.contains(program), "{err}");
    }
}

#[test]
fn resolve_in_package_a_dangling_link_in_the_middle_is_refused() {
    use std::os::unix::fs::symlink;
    let (_outer, outer) = package("fiber-resolve-dangling-outer");
    let (_guard, package) = package("fiber-resolve-dangling");
    std::fs::create_dir_all(package.join("bin")).unwrap();
    symlink(outer.join("not-yet"), package.join("bin").join("dang")).unwrap();
    let err = resolve_in_package("bin/dang/tool", &package).unwrap_err();
    assert!(err.contains("bin/dang/tool"), "{err}");
    assert!(
        err.contains(package.display().to_string().as_str()),
        "{err}"
    );
}

#[test]
fn resolve_in_package_a_dangling_link_is_refused_even_pointing_inside() {
    use std::os::unix::fs::symlink;
    let (_outer, outer) = package("fiber-resolve-dangling-in-outer");
    let (_guard, package) = package("fiber-resolve-dangling-in");
    std::fs::create_dir_all(package.join("bin")).unwrap();
    symlink(outer.join("not-yet"), package.join("bin").join("out")).unwrap();
    symlink("not-yet", package.join("bin").join("in")).unwrap();
    std::fs::write(outer.join("real"), "x").unwrap();
    symlink(outer.join("real"), package.join("bin").join("file")).unwrap();
    for program in ["bin/out", "bin/in", "bin/file"] {
        let err = resolve_in_package(program, &package).unwrap_err();
        assert!(err.contains(program), "the refusal names the path: {err}");
    }
}

#[test]
fn resolve_in_package_a_tool_under_a_symlinked_directory_out_is_refused() {
    use std::os::unix::fs::symlink;
    let (_outer, outer) = package("fiber-resolve-subdir-outer");
    let (_guard, package) = package("fiber-resolve-subdir");
    std::fs::write(outer.join("tool"), "x").unwrap();
    symlink(&outer, package.join("sub")).unwrap();
    let err = resolve_in_package("sub/tool", &package).unwrap_err();
    assert!(err.contains("sub/tool"), "{err}");
    let err = resolve_in_package("sub/missing", &package).unwrap_err();
    assert!(err.contains("sub/missing"), "{err}");
}

#[test]
fn resolve_in_package_a_bare_name_and_an_absolute_path_pass_through() {
    let (_outer, outer) = package("fiber-resolve-passthrough-outer");
    let (_guard, package) = package("fiber-resolve-passthrough");
    assert_eq!(resolve_in_package("node", &package), Ok("node".into()));
    assert_eq!(resolve_in_package("", &package), Ok(String::new()));
    assert_eq!(
        resolve_in_package("/usr/bin/env", &package),
        Ok("/usr/bin/env".into())
    );
    let absolute = outer.join("tool").display().to_string();
    assert_eq!(
        resolve_in_package(&absolute, &package),
        Ok(absolute.clone())
    );
}

#[test]
fn resolve_in_package_a_missing_program_inside_resolves_for_the_spawn_to_report() {
    let (_guard, package) = package("fiber-resolve-missing");
    assert_eq!(
        resolve_in_package("bin/missing", &package),
        Ok(package.join("bin").join("missing").display().to_string())
    );
}

/// `host.exec` `input` reaches the child's stdin: `cat` echoes it
/// byte-exact, then EOF ends the run.
#[test]
fn stdin_echoes_byte_exact_through_cat() {
    let (_dir, cwd) = dir("fiber-exec-stdin-echo");
    let clock = FakeClock::new();
    let mut req = sh("cat", cwd, CAP);
    req.input = Some(b"hello stdin\n".to_vec());
    let (_cancel, done) = spawn(req, Arc::clone(&clock), None);
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the stdin run")
        .expect("the stdin run spawns");
    assert_eq!(ran.exit_code, Some(0));
    assert_eq!(ran.stdout, b"hello stdin\n");
    assert!(!ran.timed_out);
}

/// Empty input streams empty, then EOF: `cat` ends with no output.
#[test]
fn empty_input_streams_empty_then_eof() {
    let (_dir, cwd) = dir("fiber-exec-stdin-empty");
    let clock = FakeClock::new();
    let mut req = sh("cat", cwd, CAP);
    req.input = Some(Vec::new());
    let (_cancel, done) = spawn(req, Arc::clone(&clock), None);
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the empty-input run")
        .expect("the empty-input run spawns");
    assert_eq!(ran.exit_code, Some(0));
    assert!(ran.stdout.is_empty(), "empty input echoes nothing");
}

/// No input keeps today's null stdin: `cat` reads EOF at once.
#[test]
fn missing_input_keeps_null_stdin() {
    let (_dir, cwd) = dir("fiber-exec-stdin-null");
    let clock = FakeClock::new();
    let req = sh("cat", cwd, CAP);
    assert!(req.input.is_none(), "the helper passes no input");
    let (_cancel, done) = spawn(req, Arc::clone(&clock), None);
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the null-stdin run")
        .expect("the null-stdin run spawns");
    assert_eq!(ran.exit_code, Some(0));
    assert!(ran.stdout.is_empty(), "null stdin reads EOF at once");
}

/// Input past the stream cap is streamed, not retained: 2 MiB through a
/// 1 MiB cap, and the child counts the bytes instead of echoing them.
#[test]
fn large_input_is_streamed_not_capped() {
    let (_dir, cwd) = dir("fiber-exec-stdin-large");
    let clock = FakeClock::new();
    let input = vec![b'x'; 2 << 20];
    let mut req = sh("wc -c", cwd, 1 << 20);
    req.input = Some(input);
    let (_cancel, done) = spawn(req, Arc::clone(&clock), None);
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the large-input run")
        .expect("the large-input run is not capped");
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout).trim(),
        "2097152",
        "every input byte reached the child"
    );
}

/// A full-duplex echo neither deadlocks nor retains: 2 MiB in while the
/// readers drain 2 MiB out, and `cat` echoes byte-exact. Writing everything
/// before reading, or retaining a copy past the stream, would either hang
/// on the full pipe or fail the cap.
#[test]
fn full_duplex_echo_neither_deadlocks_nor_retains() {
    let (_dir, cwd) = dir("fiber-exec-stdin-duplex");
    let clock = FakeClock::new();
    let input: Vec<u8> = (0..(2 << 20))
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let mut req = sh("cat", cwd, 3 << 20);
    req.input = Some(input.clone());
    let (_cancel, done) = spawn(req, Arc::clone(&clock), None);
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the duplex run")
        .expect("the duplex run spawns");
    assert_eq!(ran.stdout, input, "the echo is byte-exact");
}

/// A spawn failure with input never ran: it names the program as a
/// failure without input does.
#[test]
fn a_spawn_failure_with_input_never_ran() {
    let (_dir, cwd) = dir("fiber-exec-stdin-missing");
    let clock = FakeClock::new();
    let mut req = request("fiber-definitely-missing-xyz", &[], cwd, CAP);
    req.input = Some(b"lost".to_vec());
    let (_cancel, done) = spawn(req, Arc::clone(&clock), None);
    let err = Deadline::after(DEADLINE)
        .recv(&done)
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

/// An escaped stdin holder does not hang the run: the child forks a
/// background sleep inheriting stdin, then exits without draining, with
/// input larger than the pipe. The hook's deadline stops the run within
/// the existing bounds (deadline, `GRACE`, `DRAIN`): the group is
/// signalled first, the writer closes stdin at the run's end, and the
/// run returns instead of parking on the full pipe.
#[test]
fn an_escaped_stdin_holder_does_not_hang_the_run() {
    let (_dir, cwd) = dir("fiber-exec-stdin-holder");
    let clock = FakeClock::new();
    let ready = ready_in(&cwd);
    let script = format!(
        "echo $$ > '{}'\n(exec sleep 1000) &\nexit 0\n",
        ready.path().display()
    );
    let deadline = clock.now() + Duration::from_millis(500);
    let mut req = sh(&script, cwd, CAP);
    req.input = Some(vec![b'y'; 1 << 18]);
    let (_cancel, done) = spawn(req, Arc::clone(&clock), Some(deadline));
    let pgid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    // At the callback's deadline the run stops, although nothing
    // dropped its cancel; the group signal frees the parked writer.
    step_to_deadline(&clock, deadline);
    let kill_at = clock.now() + Duration::from_millis(800);
    clock.advance_when_parked(
        Some(kill_at),
        None,
        Duration::from_millis(800),
        &Deadline::after(DEADLINE),
    );
    let ran = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("waited {DEADLINE:?} for the stdin-holder run")
        .expect("the stdin-holder run returns within its bounds");
    assert!(ran.timed_out, "the deadline stopped the run");
    assert!(
        !group::alive(pgid),
        "the run returned only once the group was empty"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}
