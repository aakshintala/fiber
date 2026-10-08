//! The extension case runner (`docs/testing.md`, "Testing an extension"):
//! discovery, child isolation, exit mapping and process-group cleanup.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::{Clock, Wake};
use fakes::{Watchdog, group_empties, matching_exits};
use rustix::process::Signal;
use serde_json::json;

use super::{
    ChildEnvironment, MAX_SOCKET_PATH, ProcessClock, RunDirectory, RunOptions, RunTimeouts,
    WaitSignal, child_command, discover_cases, extension_test, extension_test_with, group_alive,
    longest_planned_socket, process_group_id, signal_group, summary,
};

const TEST_WAIT: Duration = Duration::from_secs(15);
const READY_WAIT: Duration = Duration::from_secs(5);
const GROUP_EMPTY_WAIT: Duration = Duration::from_secs(5);
const PID_EXIT_WAIT: Duration = Duration::from_secs(5);
const PID_EXIT_POLL: Duration = Duration::from_millis(50);
const MATCHING_EXIT_WAIT: Duration = Duration::from_secs(5);
const WAIT_SIGNAL_BOUND: Duration = Duration::from_millis(50);

fn empty_group(_group: u32) -> io::Result<bool> {
    Ok(false)
}

struct Setup {
    root: fakes::TempDir,
    package: PathBuf,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-extension-test");
        let package = root.path().join("package");
        fs::create_dir_all(package.join("tests")).unwrap();
        fs::write(
            package.join("extension.json"),
            json!({"name":"github.com/acme/runner-fixture","version":"v1.0.0","fiber":"0.0.0","api":1}).to_string(),
        )
        .unwrap();
        Self { root, package }
    }

    fn case(&self, name: &str) -> PathBuf {
        let path = self.package.join("tests").join(name);
        fs::write(&path, "{}").unwrap();
        path
    }

    fn options(&self, clock: Arc<dyn Clock>, fiber_prefix: Vec<OsString>) -> RunOptions {
        RunOptions {
            clock,
            timeouts: RunTimeouts {
                case: Duration::from_secs(60),
                second_sigterm: Duration::from_secs(1),
                term_grace: Duration::from_secs(5),
                reap: Duration::from_secs(5),
            },
            environment: ChildEnvironment {
                path: Some(OsString::from("/bin:/usr/bin")),
                home: Some(OsString::from("/tmp/fiber-test-home")),
            },
            temp_root: PathBuf::from("/tmp"),
            fiber_prefix,
            group_signal: Arc::new(signal_group),
            group_probe: group_alive,
        }
    }
}

fn write_case(package: &Path, name: &str) {
    let path = package.join("tests").join(name);
    fs::write(&path, "{}").unwrap();
}

#[test]
fn discovery_sorts_direct_json_cases_and_ignores_other_files_and_directories() {
    let setup = Setup::new();
    write_case(&setup.package, "b.json");
    write_case(&setup.package, "a.json");
    fs::write(setup.package.join("tests/notes.md"), "not a case").unwrap();
    fs::create_dir_all(setup.package.join("tests/sub")).unwrap();
    write_case(&setup.package, "sub/c.json");
    fs::create_dir(setup.package.join("tests/ignored.json")).unwrap();

    let cases = discover_cases(&setup.package).unwrap();
    let names: Vec<_> = cases
        .iter()
        .map(|path| path.file_name().unwrap().as_bytes())
        .collect();
    assert_eq!(names, [b"a.json", b"b.json"]);
}

#[test]
fn a_missing_manifest_is_a_usage_error_naming_the_path() {
    let setup = Setup::new();
    fs::remove_file(setup.package.join("extension.json")).unwrap();
    let options = setup.options(fakes::clock::FakeClock::new(), Vec::new());
    let mut out = Vec::new();
    let mut err = Vec::new();

    let code = extension_test_with(
        Some(&setup.package),
        Path::new("/bin/false"),
        &options,
        &mut out,
        &mut err,
    );

    assert_eq!(code, 2);
    assert!(out.is_empty());
    let error = String::from_utf8(err).unwrap();
    assert!(
        error.contains(&setup.package.display().to_string()),
        "{error}"
    );
    assert_eq!(error.lines().count(), 1, "{error}");
}

#[test]
fn discover_cases_ignores_a_missing_tests_directory_but_rejects_a_file() {
    let setup = Setup::new();
    let tests = setup.package.join("tests");
    fs::remove_dir(&tests).unwrap();
    assert_eq!(
        discover_cases(&setup.package).unwrap(),
        Vec::<PathBuf>::new()
    );

    fs::write(&tests, "not a directory").unwrap();
    let error = discover_cases(&setup.package).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
}

#[test]
fn a_package_without_a_tests_directory_has_no_cases() {
    let setup = Setup::new();
    fs::remove_dir(setup.package.join("tests")).unwrap();
    let options = setup.options(fakes::clock::FakeClock::new(), Vec::new());
    let mut out = Vec::new();
    let mut err = Vec::new();

    let code = extension_test_with(
        Some(&setup.package),
        Path::new("/bin/false"),
        &options,
        &mut out,
        &mut err,
    );

    assert_eq!(code, 1);
    assert!(err.is_empty());
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!(
            "no cases in {}/tests\n",
            setup.package.canonicalize().unwrap().display()
        )
    );
}

#[test]
fn a_package_with_no_case_files_prints_the_no_cases_line_and_fails() {
    let setup = Setup::new();
    fs::write(setup.package.join("tests/notes.md"), "not a case").unwrap();
    fs::create_dir(setup.package.join("tests/sub")).unwrap();
    let options = setup.options(fakes::clock::FakeClock::new(), Vec::new());
    let mut out = Vec::new();
    let mut err = Vec::new();

    let code = extension_test_with(
        Some(&setup.package),
        Path::new("/bin/false"),
        &options,
        &mut out,
        &mut err,
    );

    assert_eq!(code, 1);
    assert!(err.is_empty());
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!(
            "no cases in {}/tests\n",
            setup.package.canonicalize().unwrap().display()
        )
    );
}

#[test]
fn summary_reports_passes_and_failures() {
    assert_eq!(summary(2, 1), "2 passed, 1 failed\n");
}

#[test]
fn planned_case_sockets_fit_a_fifty_byte_temp_root() {
    let root = PathBuf::from("r".repeat(50));
    let socket = longest_planned_socket(&root);
    assert!(
        socket.as_os_str().len() < MAX_SOCKET_PATH,
        "{socket:?} leaves no room for a 50-byte temp root"
    );
}

#[test]
fn a_temp_root_without_room_for_sockets_is_a_usage_error() {
    let setup = Setup::new();
    let mut options = setup.options(fakes::clock::FakeClock::new(), Vec::new());
    options.temp_root = PathBuf::from("r".repeat(60));
    let mut out = Vec::new();
    let mut err = Vec::new();

    let code = extension_test_with(
        Some(&setup.package),
        Path::new("/bin/false"),
        &options,
        &mut out,
        &mut err,
    );

    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert_eq!(
        String::from_utf8(err).unwrap(),
        format!(
            "fiber: {} would need a {}-byte session socket path, past the 100-byte limit; set TMPDIR to a shorter directory\n",
            options.temp_root.display(),
            longest_planned_socket(&options.temp_root).as_os_str().len(),
        )
    );
}

#[test]
fn a_case_child_uses_the_running_binary_and_only_the_explicit_environment() {
    let setup = Setup::new();
    let case = setup.case("one.json");
    let home = setup.root.path().join("case-home");
    let workspace = setup.root.path().join("case-workspace");
    let environment = ChildEnvironment {
        path: Some(OsString::from("/bin:/usr/bin")),
        home: Some(OsString::from("/home/owner")),
    };

    let command = child_command(
        Path::new("/opt/fiber"),
        &case,
        &home,
        &workspace,
        &environment,
        &[],
    );

    assert_eq!(command.get_program(), "/opt/fiber");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            OsString::from("extension-case"),
            case.as_os_str().to_owned()
        ]
    );
    assert_eq!(command.get_current_dir(), Some(workspace.as_path()));
    let environment: Vec<_> = command
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.map(OsString::from)))
        .collect();
    assert_eq!(
        environment,
        [
            (
                OsString::from("FIBER_HOME"),
                Some(home.as_os_str().to_owned())
            ),
            (OsString::from("HOME"), Some(OsString::from("/home/owner"))),
            (
                OsString::from("PATH"),
                Some(OsString::from("/bin:/usr/bin"))
            ),
        ]
    );
}

fn script_prefix(script: &Path) -> Vec<OsString> {
    vec![
        OsString::from("-c"),
        fs::read_to_string(script).unwrap().into(),
        OsString::from("stand-in"),
    ]
}

fn run_in_thread(
    package: PathBuf,
    fiber: PathBuf,
    options: RunOptions,
) -> mpsc::Receiver<(i32, String, String)> {
    let (send, receive) = mpsc::channel();
    thread::spawn(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = extension_test_with(Some(&package), &fiber, &options, &mut out, &mut err);
        let _sent = send.send((
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        ));
    });
    receive
}

#[test]
fn child_exit_codes_map_to_ok_and_fail_with_stdout_reasons() {
    let setup = Setup::new();
    setup.case("pass.json");
    setup.case("pass-again.json");
    setup.case("fail.json");
    let script = setup.root.path().join("stand-in.sh");
    fs::write(
        &script,
        "case \"$2\" in *pass.json) printf 'ok pass\\n'; exit 0;; *pass-again.json) printf 'ok pass again\\n'; exit 0;; *) printf 'FAIL fail\\nstand-in reason\\n'; exit 1;; esac\n",
    )
    .unwrap();
    let options = setup.options(fakes::clock::FakeClock::new(), script_prefix(&script));
    let receive = run_in_thread(setup.package.clone(), PathBuf::from("/bin/sh"), options);

    let (code, out, err) = receive.recv_timeout(TEST_WAIT).unwrap();
    assert_eq!(code, 1);
    assert_eq!(
        out,
        "FAIL fail\n  stand-in reason\nok pass again\nok pass\n2 passed, 1 failed\n"
    );
    assert!(err.is_empty());
}

#[test]
fn all_passing_cases_exit_zero() {
    let setup = Setup::new();
    setup.case("pass.json");
    let script = setup.root.path().join("pass.sh");
    fs::write(&script, "printf 'ok pass\\n'; exit 0\n").unwrap();
    let options = setup.options(fakes::clock::FakeClock::new(), script_prefix(&script));

    let (code, out, err) = run_in_thread(setup.package.clone(), PathBuf::from("/bin/sh"), options)
        .recv_timeout(TEST_WAIT)
        .unwrap();

    assert_eq!(code, 0);
    assert_eq!(out, "ok pass\n1 passed, 0 failed\n");
    assert!(err.is_empty());
}

#[test]
fn zero_exit_requires_one_ok_verdict_and_no_reason_lines() {
    let setup = Setup::new();
    setup.case("reason.json");
    setup.case("wrong-verdict.json");
    let script = setup.root.path().join("verdict.sh");
    fs::write(
        &script,
        r#"case "$2" in *reason.json) printf 'ok reason\nextra reason\n';; *) printf 'FAIL wrong verdict\n';; esac; exit 0
"#,
    )
    .unwrap();
    let options = setup.options(fakes::clock::FakeClock::new(), script_prefix(&script));

    let (code, out, err) = run_in_thread(setup.package.clone(), PathBuf::from("/bin/sh"), options)
        .recv_timeout(TEST_WAIT)
        .unwrap();

    assert_eq!(code, 1);
    assert_eq!(
        out,
        "FAIL reason\n  extra reason\nFAIL wrong verdict\n  case child exited with exit code 0 without a successful verdict\n0 passed, 2 failed\n"
    );
    assert!(err.is_empty());
}

#[test]
fn the_public_entry_maps_package_and_fiber_outcomes_to_exit_codes() {
    const CHILD: &str = "FIBER_EXTENSION_TEST_ENTRY_CHILD";
    if std::env::var_os(CHILD).is_some() {
        public_entry_exit_codes();
        return;
    }

    let name = module_path!().split_once("::").unwrap().1;
    let test = format!("{name}::the_public_entry_maps_package_and_fiber_outcomes_to_exit_codes");
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &test, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .env("TMPDIR", "/tmp")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let watchdog = Watchdog::group(child.id());
    let status = wait_child(child);
    assert!(status.success(), "public entry test child exited {status}");
    watchdog.stand_down(TEST_WAIT);
}

fn public_entry_exit_codes() {
    let passing = Setup::new();
    passing.case("pass.json");
    let failing = Setup::new();
    failing.case("fail.json");
    let missing_manifest = fakes::TempDir::new("fiber-extension-test-no-manifest");
    let stand_in =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/extension-test-stand-in.sh");
    let watchdog = Watchdog::matching(&stand_in.display().to_string());

    assert_eq!(
        public_entry(passing.package.clone(), Ok(stand_in.clone())),
        0
    );
    assert_eq!(
        public_entry(failing.package.clone(), Ok(stand_in.clone())),
        1
    );
    assert_eq!(
        public_entry(passing.package.clone(), Err("fiber is missing".to_owned())),
        1
    );
    assert_eq!(
        public_entry(missing_manifest.path().to_path_buf(), Ok(stand_in)),
        2
    );

    watchdog.stand_down(TEST_WAIT);
}

fn public_entry(path: PathBuf, fiber: Result<PathBuf, String>) -> i32 {
    let (send, receive) = mpsc::channel();
    thread::spawn(move || {
        let _sent = send.send(extension_test(Some(&path), fiber));
    });
    receive
        .recv_timeout(TEST_WAIT)
        .expect("public extension test entry did not return")
}

#[test]
fn group_alive_distinguishes_a_reaped_group_from_a_live_group() {
    let exited = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .process_group(0)
        .spawn()
        .unwrap();
    let exited_group = exited.id();
    assert!(wait_child(exited).success());
    assert!(!group_alive(exited_group).unwrap());

    let live = Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .unwrap();
    let live_group = live.id();
    let watchdog = Watchdog::group(live_group);
    assert!(group_alive(live_group).unwrap());
    assert!(fakes::kill_group(live_group, "KILL").unwrap());
    let _status = wait_child(live);
    assert!(!group_alive(live_group).unwrap());
    watchdog.stand_down(TEST_WAIT);
}

fn wait_child(mut child: std::process::Child) -> ExitStatus {
    let (send, receive) = mpsc::channel();
    thread::spawn(move || {
        let _sent = send.send(child.wait());
    });
    receive
        .recv_timeout(TEST_WAIT)
        .expect("child did not exit before the test deadline")
        .unwrap()
}

fn pid_exits_or_is_zombie(pid: u32, deadline: Duration) -> bool {
    let (exited, result) = mpsc::channel();
    let (stop, stopped) = mpsc::channel::<()>();
    thread::spawn(move || {
        while fakes::kill_pid(pid, "0").unwrap() && !pid_is_zombie(pid) {
            if !matches!(
                stopped.recv_timeout(PID_EXIT_POLL),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                return;
            }
        }
        match exited.send(()) {
            Ok(()) | Err(_) => {}
        }
    });
    let finished = result.recv_timeout(deadline).is_ok();
    drop(stop);
    finished
}

#[cfg(target_os = "linux")]
fn pid_is_zombie(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .map(|(_, fields)| fields.starts_with('Z'))
        })
        .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
fn pid_is_zombie(pid: u32) -> bool {
    Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .any(|state| state.starts_with('Z'))
        })
        .unwrap_or(false)
}

#[test]
fn run_directory_cleanup_removes_it_and_reports_removal_errors() {
    let root = fakes::TempDir::new("fiber-extension-test-cleanup");
    let directory = RunDirectory::new(root.path()).unwrap();
    let path = directory.path.clone();
    directory.cleanup().unwrap();
    assert!(!path.exists());

    let directory = RunDirectory::new(root.path()).unwrap();
    let path = directory.path.clone();
    fs::remove_dir_all(&path).unwrap();
    fs::write(&path, "not a directory").unwrap();
    assert_eq!(
        directory.cleanup().unwrap_err().kind(),
        io::ErrorKind::NotADirectory
    );
}

#[test]
fn dropping_a_run_directory_removes_it() {
    let root = fakes::TempDir::new("fiber-extension-test-drop");
    let path = {
        let directory = RunDirectory::new(root.path()).unwrap();
        directory.path.clone()
    };
    assert!(!path.exists());
}

#[test]
fn prepare_wait_clears_an_earlier_wake_and_a_later_wake_releases_the_wait() {
    let signal = Arc::new(WaitSignal::default());
    signal.wake();
    signal.prepare_wait();

    let guard = super::lock(&signal.notified);
    let (guard, timeout) = signal
        .changed
        .wait_timeout_while(guard, WAIT_SIGNAL_BOUND, |notified| !*notified)
        .unwrap();
    assert!(timeout.timed_out(), "a prior wake released the wait");
    assert!(!*guard, "the cleared wake remained set");
    drop(guard);

    signal.prepare_wait();
    let waiting = Arc::clone(&signal);
    let (ready, started) = mpsc::channel();
    let (finished, result) = mpsc::channel();
    thread::spawn(move || {
        let guard = super::lock(&waiting.notified);
        ready.send(()).unwrap();
        let (guard, timeout) = waiting
            .changed
            .wait_timeout_while(guard, WAIT_SIGNAL_BOUND, |notified| !*notified)
            .unwrap();
        finished.send((timeout.timed_out(), *guard)).unwrap();
    });
    started
        .recv_timeout(TEST_WAIT)
        .expect("waiter holds the signal lock before waiting");
    signal.wake();
    assert_eq!(
        result
            .recv_timeout(TEST_WAIT)
            .expect("wake releases the bounded wait"),
        (false, true)
    );
}

#[test]
fn process_clock_wait_until_calls_once_with_the_remaining_duration() {
    let clock = ProcessClock;
    let mut unbounded = Vec::new();
    clock.wait_until(None, &mut |duration| unbounded.push(duration));
    assert_eq!(unbounded, [None]);

    let distance = Duration::from_secs(86_400);
    let deadline = clock.now().checked_add(distance).unwrap();
    let mut calls = 0;
    clock.wait_until(Some(deadline), &mut |duration| {
        calls += 1;
        let Some(duration) = duration else {
            panic!("a deadline must have a remaining duration");
        };
        assert!(duration > Duration::ZERO);
        assert!(duration <= distance);
    });
    assert_eq!(calls, 1);

    let past = clock.now().checked_sub(Duration::from_secs(1)).unwrap();
    let mut expired = Vec::new();
    clock.wait_until(Some(past), &mut |duration| expired.push(duration));
    assert_eq!(expired, [Some(Duration::ZERO)]);
}

#[test]
fn process_clock_sleep_advances_its_own_time_by_the_requested_duration() {
    let clock = ProcessClock;
    let duration = Duration::from_millis(5);
    let before = clock.now();
    clock.sleep(duration);
    assert!(clock.now().saturating_duration_since(before) >= duration);
}

#[test]
fn process_group_ids_that_could_signal_everyone_are_refused() {
    assert!(process_group_id(0).is_err());
    assert!(process_group_id(1).is_err());
    assert!(process_group_id(2).is_ok());
    assert!(process_group_id(u32::MAX).is_err());
}

#[test]
fn an_exited_child_does_not_leave_a_sigterm_ignoring_descendant_in_its_group() {
    let setup = Setup::new();
    setup.case("descendant.json");
    let ready = fakes::children::Ready::new(setup.root.path());
    let block = setup.root.path().join("descendant.fifo");
    let script = setup.root.path().join("descendant.pl");
    fs::write(
        &script,
        r#"exec perl - "$@" <<'PERL'
use strict;
use warnings;
use POSIX;
my ($ready, $block) = @ARGV;
POSIX::mkfifo($block, 0600) == 0 or die $!;
$SIG{TERM} = "IGNORE";
$| = 1;
sub emit {
    open my $file, ">", $ready or die $!;
    print {$file} "$_[0]\n";
    close $file;
}
emit($$);
my $descendant = fork();
die $! unless defined $descendant;
if ($descendant == 0) {
    $SIG{TERM} = "IGNORE";
    emit($$);
    open my $hold, "<", $block or die $!;
    while (<$hold>) {}
    exit 0;
}
open my $hold, "<", $block or die $!;
while (<$hold>) {}
PERL
"#,
    )
    .unwrap();
    let mut prefix = script_prefix(&script);
    prefix.extend([
        ready.path().as_os_str().to_owned(),
        block.as_os_str().to_owned(),
    ]);
    let clock = fakes::clock::FakeClock::new();
    let case_limit = Duration::from_millis(100);
    let options = setup.options(Arc::clone(&clock) as Arc<dyn Clock>, prefix);
    let options = RunOptions {
        timeouts: RunTimeouts {
            case: case_limit,
            ..options.timeouts
        },
        ..options
    };
    let receive = run_in_thread(setup.package.clone(), PathBuf::from("/bin/sh"), options);
    let group = ready.wait(READY_WAIT)[0];
    let descendant = ready.wait(READY_WAIT)[0];
    let watchdog = Watchdog::group(group);

    assert!(
        clock.await_parked(clock.origin() + case_limit, TEST_WAIT),
        "runner did not wait on its case deadline"
    );
    clock.advance(case_limit);
    let second_signal = clock.now() + Duration::from_secs(1);
    assert!(
        clock.await_parked(second_signal, TEST_WAIT),
        "runner did not wait before its second SIGTERM"
    );
    clock.advance(Duration::from_secs(1));
    let grace_end = clock.now() + Duration::from_secs(4);
    let marked = clock
        .mark_parked(grace_end, TEST_WAIT)
        .expect("runner did not wait through TERM_GRACE");
    assert!(fakes::kill_pid(group, "KILL").unwrap());
    assert!(
        clock.await_parked_since(&marked, Some(grace_end), TEST_WAIT),
        "child reaper did not wake the grace wait"
    );
    clock.advance(Duration::from_secs(4));

    let (code, out, err) = receive.recv_timeout(TEST_WAIT).unwrap();
    assert_eq!(code, 1);
    assert!(
        out.contains("FAIL descendant\n  did not finish within"),
        "{out}"
    );
    assert!(err.is_empty());
    assert!(
        pid_exits_or_is_zombie(group, PID_EXIT_WAIT),
        "the process-group leader {group} did not exit"
    );
    assert!(
        group_empties(group, GROUP_EMPTY_WAIT),
        "SIGKILL did not empty process group {group}"
    );
    assert!(
        pid_exits_or_is_zombie(descendant, PID_EXIT_WAIT),
        "SIGKILL did not stop descendant {descendant}"
    );
    watchdog.stand_down(TEST_WAIT);
}

#[test]
fn an_unreaped_child_gets_sigkill_even_when_the_group_probe_is_empty() {
    let setup = Setup::new();
    setup.case("hang.json");
    let ready = fakes::children::Ready::new(setup.root.path());
    let block = setup.root.path().join("release.fifo");
    let script = setup.root.path().join("wait-for-release.sh");
    fs::write(
        &script,
        "printf '%s\\n' \"$$\" > \"$1\"; mkfifo \"$2\"; printf '%s\\n' \"$$\" >> \"$1\"; read -r _ < \"$2\"; printf 'ok late\\n'",
    )
    .unwrap();
    let mut prefix = script_prefix(&script);
    prefix.extend([
        ready.path().as_os_str().to_owned(),
        block.as_os_str().to_owned(),
    ]);
    let clock = fakes::clock::FakeClock::new();
    let case_limit = Duration::from_millis(100);
    let mut options = setup.options(Arc::clone(&clock) as Arc<dyn Clock>, prefix);
    options.timeouts = RunTimeouts {
        case: case_limit,
        ..options.timeouts
    };
    let sent = Arc::new(Mutex::new(Vec::new()));
    let sent_by_signal = Arc::clone(&sent);
    options.group_signal = Arc::new(move |_group, signal| {
        sent_by_signal.lock().unwrap().push(signal);
        Ok(())
    });
    // Keep the child alive while presenting the empty-group outcome to run_child.
    options.group_probe = empty_group;
    let reap_timeout = options.timeouts.reap;
    let receive = run_in_thread(setup.package.clone(), PathBuf::from("/bin/sh"), options);
    let group = ready.wait(READY_WAIT)[0];
    let _child = ready.wait(READY_WAIT);
    let watchdog = Watchdog::group(group);

    assert!(
        clock.await_parked(clock.origin() + case_limit, TEST_WAIT),
        "runner did not wait on its case deadline"
    );
    clock.advance(case_limit);
    let second_signal = clock.now() + Duration::from_secs(1);
    assert!(
        clock.await_parked(second_signal, TEST_WAIT),
        "runner did not wait before its second SIGTERM"
    );
    clock.advance(Duration::from_secs(1));
    let grace_end = clock.now() + Duration::from_secs(4);
    assert!(
        clock.await_parked(grace_end, TEST_WAIT),
        "runner did not wait through TERM_GRACE"
    );
    clock.advance(Duration::from_secs(4));
    assert!(
        clock.await_parked(clock.now() + reap_timeout, TEST_WAIT),
        "runner did not wait for the child to be reaped"
    );
    assert_eq!(
        *sent.lock().unwrap(),
        [Signal::TERM, Signal::TERM, Signal::KILL]
    );

    let (released, released_rx) = mpsc::channel();
    thread::spawn(move || {
        let result = fs::OpenOptions::new()
            .write(true)
            .open(block)
            .and_then(|mut writer| writeln!(writer, "release"));
        let _sent = released.send(result);
    });
    released_rx
        .recv_timeout(TEST_WAIT)
        .expect("child opened its release fifo")
        .unwrap();

    let (code, out, err) = receive.recv_timeout(TEST_WAIT).unwrap();
    assert_eq!(code, 1);
    assert!(out.contains("did not finish within"), "{out}");
    assert!(err.is_empty());
    assert!(group_empties(group, GROUP_EMPTY_WAIT));
    watchdog.stand_down(TEST_WAIT);
}

#[test]
fn a_child_that_ignores_term_is_killed_as_a_group_at_the_injected_deadline() {
    let setup = Setup::new();
    setup.case("hang.json");
    let ready = fakes::children::Ready::new(setup.root.path());
    let script = setup.root.path().join("hang.sh");
    fs::write(&script, fakes::children::ignores_sigterm(ready.path())).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let case_limit = Duration::from_millis(100);
    let options = setup.options(Arc::clone(&clock) as Arc<dyn Clock>, script_prefix(&script));
    let options = RunOptions {
        timeouts: RunTimeouts {
            case: case_limit,
            ..options.timeouts
        },
        ..options
    };
    let tag = ready.path().to_string_lossy().into_owned();
    let watchdog = Watchdog::matching(&tag);
    let receive = run_in_thread(setup.package.clone(), PathBuf::from("/bin/sh"), options);
    let group = ready.wait(READY_WAIT)[0];
    assert!(
        clock.await_parked(clock.origin() + case_limit, TEST_WAIT),
        "runner did not wait on its case deadline"
    );
    clock.advance(case_limit);
    let second_signal = clock.now() + Duration::from_secs(1);
    assert!(
        clock.await_parked(second_signal, TEST_WAIT),
        "runner did not wait before its second SIGTERM"
    );
    clock.advance(Duration::from_secs(1));
    let grace_end = clock.now() + Duration::from_secs(4);
    assert!(
        clock.await_parked(grace_end, TEST_WAIT),
        "runner did not wait through TERM_GRACE"
    );
    clock.advance(Duration::from_secs(4));

    let (code, out, err) = receive.recv_timeout(TEST_WAIT).unwrap();
    assert_eq!(code, 1);
    assert!(out.contains("FAIL hang\n  did not finish within"), "{out}");
    assert!(err.is_empty());
    assert!(
        group_empties(group, GROUP_EMPTY_WAIT),
        "group {group} survived"
    );
    assert!(matching_exits(&tag, MATCHING_EXIT_WAIT), "{tag} survived");
    watchdog.stand_down(MATCHING_EXIT_WAIT);
}

#[test]
fn the_second_sigterm_kills_escaped_work_and_the_parent_kills_its_group_after_grace() {
    let setup = Setup::new();
    setup.case("signals.json");
    let ready = fakes::children::Ready::new(setup.root.path());
    let trace = setup.root.path().join("signals.txt");
    let same_group_block = setup.root.path().join("same-group.fifo");
    let escaped_block = setup.root.path().join("escaped.fifo");
    let script = setup.root.path().join("signals.sh");
    fs::write(
        &script,
        r#"exec perl -MPOSIX - "$@" <<'PERL'
use strict;
use warnings;
$| = 1;
my ($ready, $trace, $same_block, $escaped_block) = @ARGV;
my $terms = 0;
my $escaped;
sub emit {
    my @ids = @_;
    open my $file, ">", $ready or die $!;
    print {$file} join(" ", @ids), "\n";
    close $file;
}
POSIX::mkfifo($same_block, 0600) == 0 or die $!;
POSIX::mkfifo($escaped_block, 0600) == 0 or die $!;
$SIG{TERM} = sub {
    $terms++;
    open my $file, ">>", $trace or die $!;
    print {$file} "$terms\n";
    close $file;
    if ($terms == 1) {
        emit($$);
    } else {
        kill "KILL", $escaped;
        emit($$);
        exit 0;
    }
};
emit($$);
pipe(my $same_read, my $same_write) or die $!;
my $same = fork();
die $! unless defined $same;
if ($same == 0) {
    close $same_read;
    $SIG{TERM} = "IGNORE";
    print {$same_write} "$$\n";
    close $same_write;
    open my $hold, "<", $same_block or die $!;
    while (<$hold>) {}
    exit 0;
}
close $same_write;
my $same_id = <$same_read>;
close $same_read;
die "same-group child did not start" unless defined $same_id;
pipe(my $escaped_read, my $escaped_write) or die $!;
$escaped = fork();
die $! unless defined $escaped;
if ($escaped == 0) {
    close $escaped_read;
    POSIX::setsid() or die $!;
    $SIG{TERM} = "IGNORE";
    print {$escaped_write} "$$\n";
    close $escaped_write;
    open my $hold, "<", $escaped_block or die $!;
    while (<$hold>) {}
    exit 0;
}
close $escaped_write;
my $escaped_id = <$escaped_read>;
close $escaped_read;
die "escaped child did not start" unless defined $escaped_id;
chomp $same_id;
chomp $escaped_id;
emit($same_id, $escaped_id);
while (1) {}
PERL
"#,
    )
    .unwrap();
    let clock = fakes::clock::FakeClock::new();
    let case_limit = Duration::from_millis(100);
    let options = setup.options(
        Arc::clone(&clock) as Arc<dyn Clock>,
        vec![
            script.as_os_str().to_owned(),
            ready.path().as_os_str().to_owned(),
            trace.as_os_str().to_owned(),
            same_group_block.as_os_str().to_owned(),
            escaped_block.as_os_str().to_owned(),
        ],
    );
    let options = RunOptions {
        timeouts: RunTimeouts {
            case: case_limit,
            ..options.timeouts
        },
        ..options
    };
    let tag = ready.path().to_string_lossy().into_owned();
    let watchdog = Watchdog::matching(&tag);
    let receive = run_in_thread(setup.package.clone(), PathBuf::from("/bin/sh"), options);
    let group = ready.wait(READY_WAIT)[0];
    let children = ready.wait(READY_WAIT);
    let same_group_child = *children.first().unwrap();
    let escaped_child = *children.get(1).unwrap();
    let same_group_pid =
        rustix::process::Pid::from_raw(i32::try_from(same_group_child).unwrap()).unwrap();
    let escaped_pid =
        rustix::process::Pid::from_raw(i32::try_from(escaped_child).unwrap()).unwrap();
    assert_eq!(
        rustix::process::getpgid(Some(same_group_pid))
            .unwrap()
            .as_raw_pid(),
        i32::try_from(group).unwrap()
    );
    assert_ne!(
        rustix::process::getpgid(Some(escaped_pid))
            .unwrap()
            .as_raw_pid(),
        i32::try_from(group).unwrap()
    );
    assert!(
        clock.await_parked(clock.origin() + case_limit, TEST_WAIT),
        "runner did not wait on its case deadline"
    );
    clock.advance(case_limit);
    assert_eq!(ready.wait(READY_WAIT), [group]);
    assert!(
        clock.await_parked(clock.now() + Duration::from_secs(1), TEST_WAIT),
        "runner did not wait before its second SIGTERM"
    );
    clock.advance(Duration::from_secs(1));
    assert_eq!(ready.wait(READY_WAIT), [group]);
    let grace_deadline = clock.now() + Duration::from_secs(4);
    assert!(
        clock.await_parked(grace_deadline, TEST_WAIT),
        "runner did not wait through TERM_GRACE after the second signal"
    );
    clock.advance(Duration::from_secs(4));

    let (code, out, err) = receive.recv_timeout(TEST_WAIT).unwrap();
    assert_eq!(code, 1);
    assert!(
        out.contains("FAIL signals\n  did not finish within"),
        "{out}"
    );
    assert!(err.is_empty());
    assert_eq!(fs::read_to_string(&trace).unwrap(), "1\n2\n");
    assert!(
        group_empties(group, GROUP_EMPTY_WAIT),
        "group {group} survived"
    );
    assert!(
        pid_exits_or_is_zombie(escaped_child, PID_EXIT_WAIT),
        "escaped child {escaped_child} survived"
    );
    assert!(
        pid_exits_or_is_zombie(same_group_child, PID_EXIT_WAIT),
        "same-group child {same_group_child} survived"
    );
    assert!(matching_exits(&tag, MATCHING_EXIT_WAIT), "{tag} survived");
    watchdog.stand_down(MATCHING_EXIT_WAIT);
}
