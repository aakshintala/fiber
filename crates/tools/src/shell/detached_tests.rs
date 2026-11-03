//! Unit tests for a detached shell command's log, launch and receipt
//! (`docs/tools.md`, "Background jobs"). Each test names the failure it
//! kills; each child is guarded from the moment its group id is read.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use fakes::children::Ready;

use super::detached::{create_log, launch, receipt};

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
        super::shell_program(),
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
        super::shell_program(),
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
        super::shell_program(),
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
        super::shell_program(),
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
fn receipt_names_the_pid_and_the_log_path() {
    let started = super::detached::Started {
        pid: 48213,
        log: std::path::PathBuf::from("/home/a/.fiber/detached/d_48200_0.log"),
    };
    assert_eq!(
        receipt(&started),
        "Started detached. PID 48213. Output: /home/a/.fiber/detached/d_48200_0.log. Fiber will not stop it.\n"
    );
}
