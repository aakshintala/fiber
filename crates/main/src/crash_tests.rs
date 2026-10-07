//! Tests for the panic hook (`docs/code-quality.md`, "What a panic leaves"):
//! the report text and file naming in this process, and the hook itself in
//! child processes that install the real hook and panic, since the shipped
//! `fiber` carries no test-only switch (`docs/testing.md`).

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::panic::{AssertUnwindSafe, PanicHookInfo};
use std::path::Path;
use std::process::Output;
use std::time::Duration;

use super::{attach, file_name, install, message, report};

/// The child's marker: set, the test installs the hook and panics instead of
/// asserting, so the parent observes the file, stderr and the abort.
const CHILD: &str = "FIBER_CRASH_CHILD";

/// The child's Fiber home, as a path.
const HOME: &str = "FIBER_CRASH_HOME";

/// `fakes::clock::FakeClock::new()` wall time in milliseconds since the Unix
/// epoch, so every child below dates its file `*-1700000000000.txt`.
const MS: u128 = 1_700_000_000_000;

/// What the probe hook saw last: `message` of a real panic, caught so the
/// test process itself never panics.
static PROBE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Records `message` of a real panic's info.
fn probe(info: &PanicHookInfo<'_>) {
    if let Ok(mut seen) = PROBE.lock() {
        *seen = Some(message(info).to_owned());
    }
}

/// Runs `panics` under the probe hook, catches it, and returns the `message`
/// the hook saw.
fn message_of(panics: impl FnOnce()) -> String {
    let before = std::panic::take_hook();
    std::panic::set_hook(Box::new(probe));
    match std::panic::catch_unwind(AssertUnwindSafe(panics)) {
        Ok(()) | Err(_) => {}
    }
    std::panic::set_hook(before);
    PROBE.lock().unwrap().clone().unwrap()
}

/// This test's full path, for `fakes::rerun`: the crate name `module_path!`
/// carries is not part of a test's path.
fn this(test: &str) -> String {
    let module = module_path!();
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    format!("{module}::{test}")
}

/// How long a crash child may run. The panic hook symbolicates its backtrace,
/// which reads the debuginfo of every object file under `target/debug/deps`:
/// 8-22 s cold on macOS in a worktree with about 451k files under load, 1.2 s
/// at most on loaded Linux. Waiting for the exit and for the reap after a kill
/// take 2 x 30 s, half of nextest's 120 s kill (`docs/testing.md`, "Waits and
/// timeouts").
const CHILD_WITHIN: Duration = Duration::from_secs(30);

/// Re-runs this test binary's `test` with the scenario marker and `home`,
/// and returns the child's output, bounded by `CHILD_WITHIN` so a hook that
/// neither writes nor aborts still fails.
fn rerun(test: &str, scenario: &str, home: &Path) -> Output {
    let home = home.to_str().unwrap().to_owned();
    fakes::rerun_within(
        &this(test),
        &[(CHILD, scenario), (HOME, home.as_str())],
        CHILD_WITHIN,
    )
}

/// Whether this process is the `scenario` child.
fn is_child(scenario: &str) -> bool {
    std::env::var(CHILD).ok().as_deref() == Some(scenario)
}

/// The child's Fiber home, from its environment.
fn child_home() -> std::ffi::OsString {
    std::env::var_os(HOME).unwrap()
}

/// A hook with a temp Fiber home and a clock pinned at [`MS`].
fn child_hook() {
    install(Some(child_home()), None, fakes::clock::FakeClock::new());
}

/// SIGABRT is 6 on unix: the hook always ends in `abort()`, whether or not
/// the file was written.
fn assert_aborted(output: &Output) {
    assert_eq!(
        output.status.signal(),
        Some(6),
        "the child aborted, status {}",
        output.status
    );
}

/// The child's stderr as text.
fn stderr_of(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// The mode bits of `path`.
fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn report_matches_the_shipped_shape() {
    assert_eq!(
        report("writer", "boom", "src/x.rs:1:2", "<bt>"),
        "thread 'writer' panicked at src/x.rs:1:2:\nboom\nstack backtrace:\n<bt>\n"
    );
}

#[test]
fn file_name_uses_the_session_id_or_the_process_id() {
    assert_eq!(file_name(Some("s_abc"), 7, MS), "s_abc-1700000000000.txt");
    assert_eq!(file_name(None, 1234, MS), "1234-1700000000000.txt");
}

#[test]
fn message_is_the_payload_or_box_dyn_any() {
    assert_eq!(message_of(|| panic!("boom")), "boom");
    assert_eq!(message_of(|| std::panic::panic_any(7_u32)), "Box<dyn Any>");
}

#[test]
fn a_session_panic_on_the_writer_thread_writes_its_file_and_aborts() {
    if is_child("session") {
        child_hook();
        attach(&contract::SessionId("s_test".to_owned()));
        // The main thread is blocked joining this one: the hook must not
        // need it, nor the log's thread, which does not exist here at all.
        let writer = std::thread::Builder::new()
            .name("writer".to_owned())
            .spawn(|| {
                panic!("boom");
            })
            .unwrap();
        match writer.join() {
            Ok(()) | Err(_) => {}
        }
        return;
    }
    let root = fakes::TempDir::new("fiber-crash");
    let home = root.path().join("home");
    let output = rerun(
        "a_session_panic_on_the_writer_thread_writes_its_file_and_aborts",
        "session",
        &home,
    );
    assert_aborted(&output);
    // The home did not exist: the hook created it, mode 0700.
    assert_eq!(mode(&home), 0o700);
    assert_eq!(mode(&home.join("crashes")), 0o700);
    let path = home.join("crashes/s_test-1700000000000.txt");
    assert_eq!(mode(&path), 0o600);
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(contents.contains("writer"), "{contents}");
    assert!(contents.contains("boom"), "{contents}");
    assert!(contents.contains("crash_tests.rs"), "{contents}");
    assert!(contents.contains("stack backtrace:"), "{contents}");
    // The file's text and the report on stderr are byte-identical; stderr
    // adds exactly one line after it.
    let err = stderr_of(&output);
    assert!(err.starts_with(&contents), "{err}");
    assert!(
        err.ends_with(&format!(
            "fiber: crash report written to {}\n",
            path.display()
        )),
        "{err}"
    );
}

#[test]
fn a_panic_with_no_session_names_its_file_by_process_id() {
    if is_child("no_session") {
        child_hook();
        // The parent learns the pid from stdout, before the panic. The
        // harness shares stdout, so the pid rides on a marker line.
        let pid = format!("crash-pid:{}\n", std::process::id());
        std::io::stdout().write_all(pid.as_bytes()).unwrap();
        panic!("boom");
    }
    let root = fakes::TempDir::new("fiber-crash");
    let home = root.path().join("home");
    let output = rerun(
        "a_panic_with_no_session_names_its_file_by_process_id",
        "no_session",
        &home,
    );
    assert_aborted(&output);
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let pid = stdout.rsplit("crash-pid:").next().unwrap_or("").trim();
    assert!(
        !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()),
        "the child printed its process id, stdout: {stdout}"
    );
    let path = home.join(format!("crashes/{pid}-1700000000000.txt"));
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("panicked at crates/main/src/crash_tests.rs:"),
        "{contents}"
    );
    assert!(contents.contains("boom"), "{contents}");
    let err = stderr_of(&output);
    assert!(err.starts_with(&contents), "{err}");
    assert!(
        err.ends_with(&format!(
            "fiber: crash report written to {}\n",
            path.display()
        )),
        "{err}"
    );
}

#[test]
fn a_panic_leaves_the_session_log_untouched() {
    if is_child("untouched") {
        child_hook();
        attach(&contract::SessionId("s_test".to_owned()));
        panic!("boom");
    }
    let root = fakes::TempDir::new("fiber-crash");
    let home = root.path().join("home");
    let events = home.join("sessions/p/s_test/events.jsonl");
    std::fs::create_dir_all(events.parent().unwrap()).unwrap();
    std::fs::write(&events, "{\"seq\":1}\n").unwrap();
    let output = rerun(
        "a_panic_leaves_the_session_log_untouched",
        "untouched",
        &home,
    );
    assert_aborted(&output);
    // The log reads as a process that died: its bytes are unchanged, and the
    // hook wrote nothing but the one crash file.
    assert_eq!(std::fs::read(&events).unwrap(), b"{\"seq\":1}\n");
    let mut entries: Vec<String> = std::fs::read_dir(&home)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_owned())
        .collect();
    entries.sort();
    assert_eq!(entries, ["crashes", "sessions"]);
    let mut crashes: Vec<String> = std::fs::read_dir(home.join("crashes"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_owned())
        .collect();
    crashes.sort();
    assert_eq!(crashes, ["s_test-1700000000000.txt"]);
}

#[test]
fn a_panic_with_crashes_blocked_writes_no_file_and_aborts() {
    if is_child("blocked") {
        child_hook();
        // Each test runs on a thread the harness named, so the panic is
        // on an unnamed thread instead, which the report names
        // `<unnamed>`.
        let unnamed = std::thread::spawn(|| {
            panic!("boom");
        });
        match unnamed.join() {
            Ok(()) | Err(_) => {}
        }
        return;
    }
    let root = fakes::TempDir::new("fiber-crash");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    // `crashes` exists as a regular file, so the directory cannot be made.
    std::fs::write(home.join("crashes"), "blocked").unwrap();
    let output = rerun(
        "a_panic_with_crashes_blocked_writes_no_file_and_aborts",
        "blocked",
        &home,
    );
    assert_aborted(&output);
    let err = stderr_of(&output);
    assert!(err.contains("thread '<unnamed>' panicked at"), "{err}");
    assert!(err.contains("boom"), "{err}");
    assert!(err.contains("stack backtrace:"), "{err}");
    assert!(err.contains("fiber: no crash report was written:"), "{err}");
    assert!(home.join("crashes").is_file());
}

#[test]
fn a_panic_with_a_relative_home_writes_no_file_and_aborts() {
    if is_child("relative") {
        // `config::fiber_home` refuses a relative `FIBER_HOME`.
        install(
            Some("fiber-crash-relative-home".into()),
            None,
            fakes::clock::FakeClock::new(),
        );
        let unnamed = std::thread::spawn(|| {
            panic!("boom");
        });
        match unnamed.join() {
            Ok(()) | Err(_) => {}
        }
        return;
    }
    let root = fakes::TempDir::new("fiber-crash");
    let output = rerun(
        "a_panic_with_a_relative_home_writes_no_file_and_aborts",
        "relative",
        root.path(),
    );
    assert_aborted(&output);
    let err = stderr_of(&output);
    assert!(err.contains("thread '<unnamed>' panicked at"), "{err}");
    assert!(err.contains("boom"), "{err}");
    assert!(err.contains("fiber: no crash report was written:"), "{err}");
    assert!(!Path::new("fiber-crash-relative-home").exists());
}

#[test]
fn a_non_string_panic_still_writes_its_file_and_aborts() {
    if is_child("non_string") {
        child_hook();
        attach(&contract::SessionId("s_test".to_owned()));
        std::panic::panic_any(7_u32);
    }
    let root = fakes::TempDir::new("fiber-crash");
    let home = root.path().join("home");
    let output = rerun(
        "a_non_string_panic_still_writes_its_file_and_aborts",
        "non_string",
        &home,
    );
    assert_aborted(&output);
    let path = home.join("crashes/s_test-1700000000000.txt");
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(contents.contains("Box<dyn Any>"), "{contents}");
    assert!(contents.contains("stack backtrace:"), "{contents}");
    let err = stderr_of(&output);
    assert!(err.starts_with(&contents), "{err}");
}

#[test]
fn install_creates_nothing_until_a_panic_happens() {
    if is_child("no_create") {
        child_hook();
        return;
    }
    let root = fakes::TempDir::new("fiber-crash");
    let home = root.path().join("home");
    let output = rerun(
        "install_creates_nothing_until_a_panic_happens",
        "no_create",
        &home,
    );
    assert!(
        output.status.success(),
        "the child exited without panicking: {}",
        output.status
    );
    assert!(!home.exists());
}

#[test]
fn a_panic_in_an_occupied_millisecond_writes_no_file_and_aborts() {
    if is_child("collision") {
        child_hook();
        attach(&contract::SessionId("s_test".to_owned()));
        panic!("boom");
    }
    let root = fakes::TempDir::new("fiber-crash");
    let home = root.path().join("home");
    // The file this millisecond already holds another panic's report: the
    // file is opened `create_new`, so this panic gets no file.
    let path = home.join("crashes/s_test-1700000000000.txt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "another panic\n").unwrap();
    let output = rerun(
        "a_panic_in_an_occupied_millisecond_writes_no_file_and_aborts",
        "collision",
        &home,
    );
    assert_aborted(&output);
    assert_eq!(std::fs::read(&path).unwrap(), b"another panic\n");
    let err = stderr_of(&output);
    assert!(err.contains("boom"), "{err}");
    assert!(err.contains("fiber: no crash report was written:"), "{err}");
}
