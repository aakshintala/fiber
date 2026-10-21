//! Binary-level tests of `fiber feedback` (`docs/testing.md`, "Levels"):
//! the built `fiber` writes a report file and prints its path with the
//! issue link, or fails as a usage error. Each run goes to its exit under
//! the test's [`support::Deadline`].

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

mod support;

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::Watchdog;
use support::{Setup, expired, group_alive};

/// Runs `fiber` with `args`: `stdin` writes the text on a thread, as
/// `ask`'s runner does, since a child that never reads still ends the
/// exit wait within its bound. A missing `stdin` inherits the null pipe.
#[track_caller]
fn run(
    setup: &Setup,
    what: &str,
    args: &[&str],
    stdin: Option<Stdio>,
    text: Option<&str>,
) -> Output {
    let mut command: Command = setup.fiber(args);
    if let Some(stdin) = stdin {
        command.stdin(stdin);
    }
    let mut child = command.spawn().unwrap();
    if let (Some(mut pipe), Some(text)) = (child.stdin.take(), text) {
        let text = text.to_owned();
        thread::spawn(move || match pipe.write_all(text.as_bytes()) {
            Ok(()) | Err(_) => {}
        });
    }
    wait(setup, what, child)
}

/// The exit wait `run_to_exit` runs: the output under the deadline, and
/// no process left in the group behind.
#[track_caller]
fn wait(setup: &Setup, what: &str, child: Child) -> Output {
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match setup.deadline.recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => expired(setup.deadline, group, &finished, &format!("{what} to exit")),
    };
    assert!(
        !group_alive(setup.deadline, group),
        "{what} left a process in its group behind"
    );
    watchdog.stand_down(setup.deadline.cleanup_phase());
    output
}

// debt: one stdin-terminal opener in main/tests/support shared with ask.rs and login.rs, when a third lane touches any of the three copies
/// A pseudo-terminal, opened through rustix's safe calls. The main side
/// stays open while the run uses the terminal side, so stdin is a
/// terminal, as `ask`'s runner holds it.
struct Terminal {
    #[allow(dead_code, reason = "holding the main side keeps the terminal open")]
    main: OwnedFd,
    terminal: fs::File,
}

impl Terminal {
    #[track_caller]
    fn open() -> Self {
        let main =
            rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)
                .unwrap();
        rustix::pty::grantpt(&main).unwrap();
        rustix::pty::unlockpt(&main).unwrap();
        let name = rustix::pty::ptsname(&main, Vec::new()).unwrap();
        let path = PathBuf::from(OsStr::from_bytes(name.as_bytes()));
        let terminal = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        Self { main, terminal }
    }

    fn stdin(&self) -> Stdio {
        Stdio::from(self.terminal.try_clone().unwrap())
    }
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn report_path(output: &Output) -> PathBuf {
    let text = stdout_text(output);
    let mut lines = text.lines();
    let path = lines.next().unwrap();
    let link = lines.next().unwrap();
    assert!(
        link.starts_with("https://github.com/aakshintala/fiber/issues/new?"),
        "{link}"
    );
    assert!(lines.next().is_none());
    PathBuf::from(path)
}

/// A fake home holding a session marker: the report must never carry
/// session artifacts, only version, platform, description and doctor
/// output.
fn home_with_session_marker(setup: &Setup) {
    let marker = setup
        .home()
        .join("sessions")
        .join("s_0123456789abcdef")
        .join("marker");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, "session marker s_0123456789abcdef").unwrap();
}

fn assert_report(output: &Output, description: &str) -> String {
    let path = report_path(output);
    assert!(path.is_absolute());
    let report = fs::read_to_string(&path).unwrap();
    assert!(
        report.contains(env!("CARGO_PKG_VERSION")),
        "version: {report}"
    );
    assert!(
        report.contains(std::env::consts::OS) && report.contains(std::env::consts::ARCH),
        "platform: {report}"
    );
    assert!(report.contains(description), "description: {report}");
    assert!(
        report.contains("hub: not running"),
        "doctor output: {report}"
    );
    assert!(
        !report.contains("s_0123456789abcdef") && !report.contains("session marker"),
        "no session marker: {report}"
    );
    report
}

#[test]
fn feedback_by_argument_writes_the_report_and_prints_the_link() {
    let setup = Setup::new();
    home_with_session_marker(&setup);
    let output = run(
        &setup,
        "feedback by argument",
        &["feedback", "the sound stopped"],
        None,
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_report(&output, "the sound stopped");
}

#[test]
fn feedback_by_stdin_with_trailing_dash_writes_the_same_report() {
    let setup = Setup::new();
    home_with_session_marker(&setup);
    let output = run(
        &setup,
        "feedback by stdin",
        &["feedback", "-"],
        Some(Stdio::piped()),
        Some("cold air"),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_report(&output, "cold air");
}

#[test]
fn feedback_empty_description_is_a_usage_error() {
    let setup = Setup::new();
    let output = run(
        &setup,
        "feedback without a description",
        &["feedback"],
        None,
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(
        stderr_text(&output),
        "fiber: No description. Run `fiber feedback \"<description>\"` or `fiber feedback < <file>`.\n"
    );
}

#[test]
fn feedback_stdin_terminal_without_arg_is_a_usage_error() {
    let setup = Setup::new();
    let terminal = Terminal::open();
    let output = run(
        &setup,
        "feedback on a terminal without a description",
        &["feedback"],
        Some(terminal.stdin()),
        None,
    );
    drop(terminal);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(
        stderr_text(&output),
        "fiber: No description. Run `fiber feedback \"<description>\"` or `fiber feedback < <file>`.\n"
    );
}

#[test]
fn feedback_extra_positionals_name_the_shape() {
    let setup = Setup::new();
    let output = run(
        &setup,
        "feedback with two descriptions",
        &["feedback", "one", "two"],
        None,
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(
        stderr_text(&output),
        "fiber: `fiber feedback` takes one description, then an optional `-`; quote the description. Run `fiber --help` for usage.\n"
    );
}

#[test]
fn feedback_unreadable_stdin_reports_the_read() {
    let setup = Setup::new();
    let directory = fs::File::open(setup.workspace()).unwrap();
    let output = run(
        &setup,
        "feedback with unreadable stdin",
        &["feedback"],
        Some(Stdio::from(directory)),
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_text(&output)
    );
    assert!(
        stderr_text(&output).starts_with("fiber: stdin could not be read: "),
        "stderr: {}",
        stderr_text(&output)
    );
}
