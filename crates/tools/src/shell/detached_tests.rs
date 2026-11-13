//! Unit tests for a detached shell command's log, launch and receipt
//! (`docs/tools.md`, "Background jobs"). Each test names the failure it
//! kills; each child is guarded from the moment its group id is read.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::clock::Clock as _;
use contract::jobs::Jobs as _;
use contract::tool::Tool;
use fakes::children::Ready;
use fakes::clock::FakeClock;
use fakes::jobs::FakeJobs;
use fakes::{CancelToken, Recorder};
use serde_json::{Map, Value, json};

use super::{Spawner, create_log, launch, receipt};

/// The Fiber pid the log names: fixed, so a stale file from an earlier
/// process with a reused pid is just another collision.
const FIBER_PID: u32 = 4242;

/// What holds the group open after the script reports: the script's own
/// process, which `launch` starts as a session leader.
const LINGER: &str = "exec sleep 60\n";

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn existing_logs(dir: &Path, count: u64) {
    for n in 0..count {
        std::fs::write(dir.join(format!("d_{FIBER_PID}_{n}.log")), b"stale").unwrap();
    }
}

fn write_script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let script = dir.join(name);
    std::fs::write(&script, body).unwrap();
    script
}

#[test]
fn create_log_starts_at_zero_and_hands_out_each_value_once() {
    let dir = fakes::TempDir::new("fiber-detached-first");
    let next = AtomicU64::new(0);
    let (_, path) = create_log(dir.path(), FIBER_PID, &next).unwrap();
    assert_eq!(path, dir.path().join(format!("d_{FIBER_PID}_0.log")));
    assert_eq!(
        mode_of(&path),
        0o600,
        "the log is readable only by its owner"
    );
    assert_eq!(next.load(Ordering::SeqCst), 1);
    // A second call after success takes the next value, not a collision.
    let (_, second) = create_log(dir.path(), FIBER_PID, &next).unwrap();
    assert_eq!(second, dir.path().join(format!("d_{FIBER_PID}_1.log")));
    assert_eq!(next.load(Ordering::SeqCst), 2);
}

#[test]
fn create_log_skips_taken_names_below_the_cap() {
    let dir = fakes::TempDir::new("fiber-detached-below-cap");
    existing_logs(dir.path(), 99);
    let next = AtomicU64::new(0);
    let (_, path) = create_log(dir.path(), FIBER_PID, &next).unwrap();
    assert_eq!(path, dir.path().join(format!("d_{FIBER_PID}_99.log")));
    assert_eq!(next.load(Ordering::SeqCst), 100);
}

#[test]
fn create_log_fails_at_the_cap_without_an_extra_attempt() {
    let dir = fakes::TempDir::new("fiber-detached-at-cap");
    existing_logs(dir.path(), 100);
    let next = AtomicU64::new(0);
    let failed = create_log(dir.path(), FIBER_PID, &next).unwrap_err();
    assert_eq!(failed.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(next.load(Ordering::SeqCst), 100);
}

#[test]
fn create_log_fails_past_the_cap_without_an_extra_attempt() {
    let dir = fakes::TempDir::new("fiber-detached-past-cap");
    existing_logs(dir.path(), 101);
    let next = AtomicU64::new(0);
    let failed = create_log(dir.path(), FIBER_PID, &next).unwrap_err();
    assert_eq!(failed.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(next.load(Ordering::SeqCst), 100);
}

#[test]
fn create_log_returns_a_non_collision_open_error_at_once() {
    let dir = fakes::TempDir::new("fiber-detached-open-error");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let next = AtomicU64::new(0);
    let failed = create_log(dir.path(), FIBER_PID, &next).unwrap_err();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        failed.kind(),
        std::io::ErrorKind::PermissionDenied,
        "a refused open is no name collision"
    );
    assert_eq!(
        next.load(Ordering::SeqCst),
        1,
        "a non-collision error returns at once instead of retrying as a collision"
    );
}

#[test]
fn create_log_creates_a_private_directory() {
    let root = fakes::TempDir::new("fiber-detached-new-dir");
    let dir = root.path().join("detached");
    let next = AtomicU64::new(0);
    let (_, path) = create_log(&dir, FIBER_PID, &next).unwrap();
    assert_eq!(mode_of(&dir), 0o700, "the log directory is private");
    assert_eq!(
        mode_of(&path),
        0o600,
        "the log is readable only by its owner"
    );
}

#[test]
fn create_log_keeps_an_existing_directory_mode() {
    let dir = fakes::TempDir::new("fiber-detached-kept-mode");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let next = AtomicU64::new(0);
    let (_, path) = create_log(dir.path(), FIBER_PID, &next).unwrap();
    assert_eq!(
        mode_of(dir.path()),
        0o755,
        "a directory that exists keeps its mode"
    );
    assert_eq!(mode_of(&path), 0o600, "the new log is still private");
}

#[test]
fn launch_streams_both_outputs_to_the_log_and_reports_the_child() {
    let dir = fakes::TempDir::new("fiber-detached-output");
    let ready = Ready::new(dir.path());
    let script = write_script(
        dir.path(),
        "report.sh",
        &format!(
            "echo up\necho err >&2\nread -r line\nstatus=$?\nprintf '%s %s\\n' \"$status\" \"$$\" > {}\n{LINGER}",
            ready.path().display(),
        ),
    );
    let command = format!("sh {}", script.display());
    let started = launch(
        dir.path(),
        FIBER_PID,
        super::super::shell_program(),
        &command,
        dir.path(),
    )
    .unwrap();
    let line = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let watchdog = fakes::Watchdog::group(line[1]);
    // The read from `/dev/null` ends at once with end-of-file, and the log
    // writes precede the ready line in program order.
    assert_eq!(
        line,
        vec![1, started.pid],
        "EOF status, then the child's pid"
    );
    assert_eq!(
        std::fs::read_to_string(&started.log).unwrap(),
        "up\nerr\n",
        "both streams land in the log in arrival order"
    );
    fakes::kill_group(started.pid, "KILL").unwrap();
    assert!(
        fakes::group_empties(
            started.pid,
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the group empties once the reaper thread reaps the child"
    );
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

#[test]
fn launch_leaves_a_group_no_shutdown_list_holds() {
    let dir = fakes::TempDir::new("fiber-detached-survives");
    let ready = Ready::new(dir.path());
    let script = write_script(
        dir.path(),
        "linger.sh",
        &format!("echo $$ > {}\n{LINGER}", ready.path().display()),
    );
    let command = format!("sh {}", script.display());
    let started = launch(
        dir.path(),
        FIBER_PID,
        super::super::shell_program(),
        &command,
        dir.path(),
    )
    .unwrap();
    let pgid = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    assert_eq!(
        pgid, started.pid,
        "the child is its own session and group leader"
    );
    assert!(
        fakes::group_alive(pgid),
        "the group outlives the launch call"
    );
    fakes::kill_group(pgid, "KILL").unwrap();
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "no zombie is left: the reaper thread reaped the child"
    );
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

#[test]
fn launch_removes_the_log_when_spawn_fails() {
    let dir = fakes::TempDir::new("fiber-detached-no-spawn");
    let failed = launch(
        dir.path(),
        FIBER_PID,
        Path::new("/no/such/fiber-detached-program"),
        "true",
        dir.path(),
    );
    assert!(failed.is_err(), "a missing program starts nothing");
    assert!(
        !dir.path().join(format!("d_{FIBER_PID}_0.log")).exists(),
        "a log created before a failed spawn is removed"
    );
}

#[test]
fn launch_fails_when_the_directory_is_a_regular_file() {
    let dir = fakes::TempDir::new("fiber-detached-file-dir");
    let taken = dir.path().join("taken");
    std::fs::write(&taken, b"x").unwrap();
    let failed = launch(
        &taken,
        FIBER_PID,
        super::super::shell_program(),
        "true",
        dir.path(),
    );
    assert!(failed.is_err(), "a file is no log directory");
}

#[cfg(target_os = "linux")]
#[test]
fn launch_with_a_non_utf8_directory_fails_before_spawn() {
    use std::os::unix::ffi::OsStrExt as _;
    let dir = fakes::TempDir::new("fiber-detached-non-utf8");
    let ready = Ready::new(dir.path());
    let script = write_script(
        dir.path(),
        "marker.sh",
        &format!(
            "touch {}\necho $$ > {}\n{LINGER}",
            dir.path().join("started").display(),
            ready.path().display(),
        ),
    );
    let bad = dir
        .path()
        .join(std::ffi::OsStr::from_bytes(b"bad-\xff-dir"));
    let command = format!("sh {}", script.display());
    let failed = launch(
        &bad,
        FIBER_PID,
        super::super::shell_program(),
        &command,
        dir.path(),
    );
    assert!(failed.is_err(), "a log with no readable path fails");
    // No process started: the script would have left its marker, and a wait
    // on the ready line is no proof that it never will.
    assert!(!dir.path().join("started").exists());
    assert!(
        !bad.exists(),
        "nothing is created for a path the model cannot read"
    );
}

#[test]
fn reaped_leader_does_not_end_while_its_group_lives() {
    let dir = fakes::TempDir::new("fiber-detached-reaped-leader");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let clock = FakeClock::new();
    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let spawner: Spawner = Arc::new(move |task| {
        let supervisor_tx = supervisor_tx.clone();
        std::thread::Builder::new()
            .name("test-detached-supervisor".into())
            .spawn(move || {
                let _sent = supervisor_tx.send(std::thread::current().id());
                task();
            })
            .map(drop)
    });
    let shell = super::super::Shell::new(dir.path().to_path_buf(), clock.clone())
        .with_jobs(jobs.clone())
        .with_detached(dir.path().join("detached"))
        .with_detached_spawner(spawner);
    let command = format!("sleep 60 & echo $$ > '{}'; exit 0", ready.path().display());
    let mut arguments = Map::new();
    arguments.insert("command".into(), Value::String(command));
    arguments.insert("detach".into(), json!(true));
    let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
    assert!(
        output.error.is_none(),
        "detached launch: {:?}",
        output.error
    );
    let pgid = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let started = jobs.started().pop().expect("detached row");
    let supervisor = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&supervisor_rx)
        .expect("supervisor started");
    fakes::wait_until(
        "leader reaped while descendant lives",
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || !fakes::pid_alive(pgid),
    );
    assert!(
        fakes::group_alive(pgid),
        "the background sleep holds the group"
    );
    for _ in 0..3 {
        clock.advance_when_parked(
            None,
            Some(supervisor),
            support::group::GROUP_POLL,
            &fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        );
        assert!(
            jobs.ended(fakes::Deadline::after(std::time::Duration::ZERO))
                .is_none(),
            "no End while the group remains alive"
        );
    }
    assert!(fakes::kill_group(pgid, "KILL").unwrap());
    let completed = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("End after the group empties");
    assert_eq!(completed.job_id, started.job_id);
    assert!(fakes::group_empties(
        pgid,
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    assert_eq!(jobs.end_count(), 1);
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

#[test]
fn detached_stop_escalates_after_the_session_clock_grace() {
    let dir = fakes::TempDir::new("fiber-detached-stop-supervisor");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let clock = FakeClock::new();
    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let spawner: Spawner = Arc::new(move |task| {
        let supervisor_tx = supervisor_tx.clone();
        std::thread::Builder::new()
            .name("test-detached-supervisor".to_owned())
            .spawn(move || {
                let _sent = supervisor_tx.send(std::thread::current().id());
                task();
            })
            .map(drop)
    });
    let shell = super::super::Shell::new(dir.path().to_path_buf(), clock.clone())
        .with_jobs(jobs.clone())
        .with_detached(dir.path().join("detached"))
        .with_detached_spawner(spawner);
    let command = format!(
        "trap '' TERM; echo $$ > '{}'; exec sleep 60",
        ready.path().display()
    );
    let mut arguments = Map::new();
    arguments.insert("command".to_owned(), Value::String(command));
    arguments.insert("detach".to_owned(), json!(true));
    let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
    assert!(
        output.error.is_none(),
        "detached launch: {:?}",
        output.error
    );
    let pgid = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let started = jobs.started().pop().expect("the detached row was recorded");
    assert_eq!(started.pgid, Some(pgid));
    let supervisor = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&supervisor_rx)
        .expect("the supervisor thread started");
    let stopped = Arc::new(AtomicBool::new(false));
    let stop_result = Arc::clone(&stopped);
    let mark = clock.advance_when_parked_then(
        None,
        Some(supervisor),
        Duration::ZERO,
        &fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        || {
            stop_result.store(jobs.stop(&started.job_id), Ordering::SeqCst);
        },
    );
    assert!(
        stopped.load(Ordering::SeqCst),
        "the recorded job accepts stop"
    );
    assert!(fakes::group_alive(pgid), "TERM is ignored during grace");

    let deadline = clock.now().checked_add(support::group::GRACE).unwrap();
    assert!(clock.await_parked_since(
        &mark,
        Some(deadline),
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    clock.advance_when_parked(
        Some(deadline),
        Some(supervisor),
        support::group::GRACE,
        &fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let completed = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the supervisor reports after the group empties");
    assert_eq!(completed.status, contract::events::Outcome::Cancelled);
    assert!(fakes::group_empties(
        pgid,
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    assert_eq!(jobs.end_count(), 1, "the supervisor reports one End");
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

fn failed_supervisor_fixture() -> (
    super::Supervisor,
    mpsc::Receiver<contract::events::JobCompleted>,
    String,
) {
    let dir = fakes::TempDir::new("fiber-detached-supervisor-failure");
    let jobs = FakeJobs::new(dir.path());
    let mut opened = jobs
        .open(contract::jobs::Opening {
            tool: "shell".into(),
            description: "detached command".into(),
            stop: contract::jobs::Stop(Box::new(|| {})),
            lines: false,
            input: None,
            identity: None,
            detached: None,
        })
        .unwrap();
    let id = opened.started.job_id.0.clone();
    let (report_tx, report_rx) = mpsc::channel();
    let old_end = std::mem::replace(&mut opened.end, contract::jobs::End(Box::new(|_| {})));
    old_end.0(contract::events::JobCompleted {
        job_id: opened.started.job_id.clone(),
        status: contract::events::Outcome::Completed,
        error: None,
        process: None,
        output_tail: None,
        notice: None,
        changed_by: None,
        protected_paths: None,
    });
    opened.end = contract::jobs::End(Box::new(move |completed| {
        let _sent = report_tx.send(completed);
    }));
    let (done_tx, done) = mpsc::channel();
    drop(done_tx);
    let started = super::Started {
        pid: 2,
        log: opened.path.clone(),
        identity: contract::jobs::JobIdentity {
            pgid: 2,
            start_time: 3,
        },
        done,
        control: super::StopControl::new(),
    };
    let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
    (
        super::Supervisor {
            opened,
            started,
            clock,
        },
        report_rx,
        id,
    )
}

#[test]
fn supervisor_indeterminate_failure_reports_one_failed_end() {
    let (supervisor, reported, id) = failed_supervisor_fixture();
    supervisor.fail_indeterminate();
    let completed = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&reported)
        .expect("indeterminate supervisor failure reports an End");
    assert_eq!(completed.job_id.0, id);
    assert_eq!(completed.status, contract::events::Outcome::Failed);
    assert_eq!(
        completed.error.unwrap().code,
        contract::ErrorCode::Indeterminate
    );
    assert!(reported.try_recv().is_err(), "one End is reported");
}

#[test]
fn supervisor_signal_failure_reports_one_failed_end() {
    let (supervisor, reported, id) = failed_supervisor_fixture();
    supervisor.fail_signal(std::io::Error::other("signal denied"));
    let completed = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&reported)
        .expect("signal failure reports an End");
    assert_eq!(completed.job_id.0, id);
    assert_eq!(completed.status, contract::events::Outcome::Failed);
    let error = completed.error.unwrap();
    assert_eq!(error.code, contract::ErrorCode::ToolError);
    assert!(error.message.contains("signal denied"), "{}", error.message);
    assert!(reported.try_recv().is_err(), "one End is reported");
}

struct NoAsk;

impl contract::tool::Ask for NoAsk {
    fn action(&self) -> contract::ActionId {
        contract::ActionId("a_test".into())
    }

    fn answerable(&self) -> bool {
        false
    }

    fn ask(&self, _asking: contract::tool::Asking) -> contract::tool::Answered {
        contract::tool::Answered::NoAnswer
    }
}

fn restored_fixture(
    dir: &Path,
    ready: &Ready,
    _clock: Arc<FakeClock>,
    command: &str,
) -> (
    Arc<FakeJobs>,
    contract::jobs::Opened,
    super::Restored,
    u32,
    fakes::Watchdog,
) {
    let started = launch(dir, FIBER_PID, super::super::shell_program(), command, dir).unwrap();
    let pgid = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let control = super::StopControl::new();
    let stop_control = Arc::clone(&control);
    let jobs = FakeJobs::new(dir);
    let opened = jobs
        .open(contract::jobs::Opening {
            tool: "shell".into(),
            description: "restored detached command".into(),
            stop: contract::jobs::Stop(Box::new(move || stop_control.request_stop(pgid))),
            lines: false,
            input: None,
            identity: Some(started.identity),
            detached: Some(started.log.clone()),
        })
        .unwrap();
    drop(started);
    let log_root = opened.path.parent().unwrap().to_path_buf();
    (
        jobs,
        opened,
        super::Restored {
            pgid,
            control,
            log_root,
            #[cfg(debug_assertions)]
            liveness: None,
        },
        pgid,
        watchdog,
    )
}

#[test]
fn restored_signal_failure_reports_one_failed_end_without_signalling() {
    let dir = fakes::TempDir::new("fiber-detached-restored-signal-failure");
    let ready = Ready::new(dir.path());
    let clock = FakeClock::new();
    let (jobs, opened, restored, pgid, watchdog) = restored_fixture(
        dir.path(),
        &ready,
        clock.clone(),
        &format!("echo $$ > '{}'; exec sleep 60", ready.path().display()),
    );
    {
        let mut stop = support::lock(&restored.control.stop);
        stop.requested = true;
        stop.result = Some(Err(std::io::Error::other("injected TERM failure")));
    }
    let (started_tx, started_rx) = mpsc::channel();
    let spawner: Spawner = Arc::new(move |task| {
        let started_tx = started_tx.clone();
        std::thread::Builder::new()
            .name("test-restored-signal-failure".into())
            .spawn(move || {
                let _sent = started_tx.send(());
                task();
            })
            .map(drop)
    });
    super::track_restored(opened, restored, clock, &spawner).unwrap();
    fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&started_rx)
        .expect("restored supervisor started");
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("restored signal failure reports End");
    assert_eq!(ended.status, contract::events::Outcome::Failed);
    let error = ended.error.expect("failed signal carries an error");
    assert_eq!(error.code, contract::ErrorCode::ToolError);
    assert!(error.message.contains("injected TERM failure"));
    assert_eq!(jobs.end_count(), 1);
    assert!(
        fakes::group_alive(pgid),
        "failed TERM did not signal the group"
    );
    assert!(fakes::kill_group(pgid, "KILL").unwrap());
    assert!(fakes::group_empties(
        pgid,
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

#[test]
fn restored_group_disappearance_reports_one_failed_indeterminate_end() {
    let dir = fakes::TempDir::new("fiber-detached-restored-spontaneous");
    let ready = Ready::new(dir.path());
    let clock = FakeClock::new();
    let (jobs, opened, restored, pgid, watchdog) = restored_fixture(
        dir.path(),
        &ready,
        clock.clone(),
        &format!("echo $$ > '{}'; exec sleep 60", ready.path().display()),
    );
    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let spawner: Spawner = Arc::new(move |task| {
        let supervisor_tx = supervisor_tx.clone();
        std::thread::Builder::new()
            .name("test-restored-supervisor".into())
            .spawn(move || {
                let _sent = supervisor_tx.send(std::thread::current().id());
                task();
            })
            .map(drop)
    });
    super::track_restored(opened, restored, clock, &spawner).unwrap();
    let _supervisor = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&supervisor_rx)
        .expect("restored supervisor started");
    assert!(fakes::group_alive(pgid));
    assert!(fakes::kill_group(pgid, "KILL").unwrap());
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("spontaneous disappearance reports End");
    assert_eq!(ended.status, contract::events::Outcome::Failed);
    assert_eq!(
        ended.error.unwrap().code,
        contract::ErrorCode::Indeterminate
    );
    assert_eq!(jobs.end_count(), 1);
    assert!(fakes::group_empties(
        pgid,
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

#[test]
fn restored_stop_escalates_after_grace_and_reports_one_cancelled_end() {
    let dir = fakes::TempDir::new("fiber-detached-restored-stop");
    let ready = Ready::new(dir.path());
    let clock = FakeClock::new();
    let (jobs, opened, restored, pgid, watchdog) = restored_fixture(
        dir.path(),
        &ready,
        clock.clone(),
        &format!(
            "trap '' TERM; echo $$ > '{}'; exec sleep 60",
            ready.path().display()
        ),
    );
    let job_id = opened.started.job_id.clone();
    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let spawner: Spawner = Arc::new(move |task| {
        let supervisor_tx = supervisor_tx.clone();
        std::thread::Builder::new()
            .name("test-restored-stop-supervisor".into())
            .spawn(move || {
                let _sent = supervisor_tx.send(std::thread::current().id());
                task();
            })
            .map(drop)
    });
    super::track_restored(opened, restored, clock.clone(), &spawner).unwrap();
    let supervisor = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&supervisor_rx)
        .expect("restored supervisor started");
    let stopped = Arc::new(AtomicBool::new(false));
    let stop_result = Arc::clone(&stopped);
    let mark = clock.advance_when_parked_then(
        None,
        Some(supervisor),
        Duration::ZERO,
        &fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        || stop_result.store(jobs.stop(&job_id), Ordering::SeqCst),
    );
    assert!(
        stopped.load(Ordering::SeqCst),
        "Registry approves and sends TERM"
    );
    assert!(
        fakes::group_alive(pgid),
        "TERM is ignored during the grace period"
    );
    let deadline = clock.now().checked_add(support::group::GRACE).unwrap();
    assert!(clock.await_parked_since(
        &mark,
        Some(deadline),
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    clock.advance_when_parked(
        Some(deadline),
        Some(supervisor),
        support::group::GRACE,
        &fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("restored supervisor reports after escalation empties the group");
    assert_eq!(ended.status, contract::events::Outcome::Failed);
    assert_eq!(
        ended.error.as_ref().map(|error| error.code.clone()),
        Some(contract::ErrorCode::Indeterminate)
    );
    assert_eq!(jobs.end_count(), 1);
    assert!(fakes::group_empties(
        pgid,
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

#[test]
fn restored_supervisor_spawn_failure_reports_once_without_signalling() {
    let dir = fakes::TempDir::new("fiber-detached-restored-spawn-failure");
    let ready = Ready::new(dir.path());
    let clock = FakeClock::new();
    let (jobs, opened, restored, pgid, watchdog) = restored_fixture(
        dir.path(),
        &ready,
        clock.clone(),
        &format!("echo $$ > '{}'; exec sleep 60", ready.path().display()),
    );
    let spawner: Spawner = Arc::new(|_| Err(std::io::Error::other("thread refused")));
    let error = super::track_restored(opened, restored, clock, &spawner).unwrap_err();
    assert!(error.to_string().contains("thread refused"));
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("spawn failure reports End synchronously");
    assert_eq!(ended.status, contract::events::Outcome::Failed);
    assert_eq!(
        ended.error.unwrap().code,
        contract::ErrorCode::Indeterminate
    );
    assert_eq!(jobs.end_count(), 1);
    assert!(
        fakes::group_alive(pgid),
        "failure must not signal the group"
    );
    assert!(fakes::kill_group(pgid, "KILL").unwrap());
    assert!(fakes::group_empties(
        pgid,
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

#[test]
fn restored_confirmed_leader_uses_bare_kill_fast_path() {
    use contract::tool::Tool as _;

    let dir = fakes::TempDir::new("fiber-detached-restored-owned-kill");
    let ready = Ready::new(dir.path());
    let started = launch(
        dir.path(),
        FIBER_PID,
        super::super::shell_program(),
        &format!("echo $$ > '{}'; exec sleep 60", ready.path().display()),
        dir.path(),
    )
    .unwrap();
    let pgid = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    let jobs = FakeJobs::new(dir.path());
    let opened = jobs
        .open(contract::jobs::Opening {
            tool: "shell".into(),
            description: "keeper-restored detached command".into(),
            stop: contract::jobs::Stop(Box::new(|| {})),
            lines: false,
            input: None,
            identity: Some(started.identity),
            detached: Some(started.log.clone()),
        })
        .unwrap();
    let shell = super::super::Shell::new(dir.path().to_path_buf(), FakeClock::new())
        .with_jobs(jobs)
        .with_detached(dir.path().join("detached"));
    assert!(
        shell.owns_live_leader(pgid),
        "restored identity is confirmed"
    );
    let arguments = json!({"command": format!("kill {pgid}")})
        .as_object()
        .unwrap()
        .clone();
    let effects = shell.effects(&arguments).unwrap();
    assert!(
        effects.declared.effects.is_empty(),
        "the owned pid is fast-pathed"
    );
    let output = shell.run_with_effects(
        &arguments,
        &effects,
        &CancelToken::new(),
        &Recorder::default(),
        &NoAsk,
    );
    assert!(
        output.error.is_none(),
        "bare kill fast path: {:?}",
        output.error
    );
    assert!(fakes::group_empties(
        pgid,
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    drop(opened.end);
    drop(started);
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

#[test]
fn receipt_names_the_job_log_and_stop_action() {
    assert_eq!(
        receipt(
            Path::new("/home/a/.fiber/detached/d_48200_0.log"),
            "j_abc123"
        ),
        "Started detached. Job j_abc123. Output: /home/a/.fiber/detached/d_48200_0.log. Stop it with `jobs stop j_abc123`.\n"
    );
}

#[test]
fn restored_disappearance_preserves_indeterminate_failure_payload() {
    let dir = fakes::TempDir::new("fiber-detached-restored-payload");
    let ready = Ready::new(dir.path());
    let clock = FakeClock::new();
    let marker = "restored-payload-unique-7f3a";
    let (jobs, opened, restored, pgid, watchdog) = restored_fixture(
        dir.path(),
        &ready,
        clock.clone(),
        &format!(
            "printf '%s\\n' '{marker}'; echo $$ > '{}'; exec sleep 60",
            ready.path().display()
        ),
    );
    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let spawner: Spawner = Arc::new(move |task| {
        let supervisor_tx = supervisor_tx.clone();
        std::thread::Builder::new()
            .name("test-restored-payload-supervisor".into())
            .spawn(move || {
                let _sent = supervisor_tx.send(std::thread::current().id());
                task();
            })
            .map(drop)
    });
    super::track_restored(opened, restored, clock, &spawner).unwrap();
    let _supervisor = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&supervisor_rx)
        .expect("restored supervisor started");
    let log_path = std::fs::read_dir(dir.path())
        .unwrap()
        .map(Result::unwrap)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("d_")
        })
        .expect("restored job log exists");
    assert_eq!(
        std::fs::read_to_string(log_path).unwrap(),
        format!("{marker}\n"),
        "the unique payload is in the restored job log before disappearance"
    );
    assert!(fakes::group_alive(pgid));
    assert!(fakes::kill_group(pgid, "KILL").unwrap());
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("restored group disappearance reports End");
    assert_eq!(ended.status, contract::events::Outcome::Failed);
    let error = ended.error.expect("failed End carries an error");
    assert_eq!(error.code, contract::ErrorCode::Indeterminate);
    assert_eq!(
        error.message,
        "The command was stopped, and Fiber cannot tell whether it completed."
    );
    let tail = ended
        .output_tail
        .expect("failed End preserves the log tail");
    assert!(!tail.is_empty(), "the output tail is non-empty");
    assert!(tail.contains(marker), "output tail: {tail:?}");
    assert_eq!(jobs.end_count(), 1, "exactly one failed End is reported");
    assert!(fakes::group_empties(
        pgid,
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

#[cfg(debug_assertions)]
#[test]
fn restored_drain_deadline_survivor_reports_only_at_deadline() {
    use super::RestoredLivenessEvent::{AfterKillLoopTop, DrainDeadline};

    let dir = fakes::TempDir::new("fiber-detached-restored-drain-deadline");
    let ready = Ready::new(dir.path());
    let clock = FakeClock::new();
    let (jobs, opened, mut restored, pgid, watchdog) = restored_fixture(
        dir.path(),
        &ready,
        clock.clone(),
        &format!(
            "trap '' TERM; echo $$ > '{}'; exec sleep 60",
            ready.path().display()
        ),
    );
    let (observed_tx, observed_rx) = mpsc::channel();
    let (answers_tx, answers_rx) = mpsc::channel();
    restored.liveness = Some(super::RestoredLivenessHook {
        observed: observed_tx,
        answers: answers_rx,
    });
    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let spawner: Spawner = Arc::new(move |task| {
        let supervisor_tx = supervisor_tx.clone();
        std::thread::Builder::new()
            .name("test-restored-drain-deadline".into())
            .spawn(move || {
                let _sent = supervisor_tx.send(std::thread::current().id());
                task();
            })
            .map(drop)
    });
    let job_id = opened.started.job_id.clone();
    super::track_restored(opened, restored, clock.clone(), &spawner).unwrap();
    let supervisor = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&supervisor_rx)
        .expect("restored supervisor started");
    let stopped = Arc::new(AtomicBool::new(false));
    let stop_result = Arc::clone(&stopped);
    let mark = clock.advance_when_parked_then(
        None,
        Some(supervisor),
        Duration::ZERO,
        &fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        || stop_result.store(jobs.stop(&job_id), Ordering::SeqCst),
    );
    assert!(stopped.load(Ordering::SeqCst));
    assert!(fakes::group_alive(pgid), "TERM is ignored during grace");
    let grace = clock.now().checked_add(support::group::GRACE).unwrap();
    assert!(clock.await_parked_since(
        &mark,
        Some(grace),
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    let after_grace = clock.advance_when_parked(
        Some(grace),
        Some(supervisor),
        support::group::GRACE,
        &fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&observed_rx)
            .unwrap(),
        AfterKillLoopTop
    );
    answers_tx.send(true).unwrap();
    let drain = grace.checked_add(support::group::DRAIN).unwrap();
    assert!(clock.await_parked_since(
        &after_grace,
        Some(drain),
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    let before = support::group::DRAIN / 2;
    let before_mark = clock.advance_when_parked(
        Some(drain),
        Some(supervisor),
        before,
        &fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&observed_rx)
            .unwrap(),
        AfterKillLoopTop
    );
    answers_tx.send(true).unwrap();
    assert!(clock.await_parked_since(
        &before_mark,
        Some(drain),
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    assert!(
        jobs.ended(fakes::Deadline::after(Duration::ZERO)).is_none(),
        "no End before deadline"
    );
    assert_eq!(
        observed_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty),
        "drain condition not reached early"
    );
    clock.advance_when_parked(
        Some(drain),
        Some(supervisor),
        support::group::DRAIN - before,
        &fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&observed_rx)
            .unwrap(),
        AfterKillLoopTop
    );
    answers_tx.send(true).unwrap();
    assert_eq!(
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&observed_rx)
            .unwrap(),
        DrainDeadline
    );
    answers_tx.send(true).unwrap();
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("surviving group reports End");
    assert_eq!(ended.status, contract::events::Outcome::Failed);
    let error = ended.error.expect("failed End includes error");
    assert_eq!(error.code, contract::ErrorCode::Indeterminate);
    assert_eq!(
        error.message,
        "The command was stopped, and Fiber cannot tell whether it completed."
    );
    assert_eq!(jobs.end_count(), 1, "exactly one End");
    assert!(fakes::group_empties(
        pgid,
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}
