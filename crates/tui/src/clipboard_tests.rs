//! Tests for OSC 52, the clipboard command pick and the pipe to it.

use super::{command, osc52, pipe};
use fakes::Deadline;
use std::ffi::OsString;
use std::io;
use std::process::ExitStatus;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn osc52_frames_the_base64_text() {
    assert_eq!(osc52("hi"), b"\x1b]52;c;aGk=\x07");
    assert_eq!(osc52(""), b"\x1b]52;c;\x07");
    assert_eq!(osc52("é\n"), b"\x1b]52;c;w6kK\x07");
}

/// No environment variable set.
fn no_env(_: &str) -> Option<OsString> {
    None
}

#[test]
fn the_first_command_found_is_picked() {
    assert_eq!(command(no_env, |_| true), Some(vec!["pbcopy"]));
    assert_eq!(
        command(no_env, |name| name == "wl-copy" || name == "xsel"),
        Some(vec!["wl-copy"])
    );
    assert_eq!(
        command(no_env, |name| name == "xclip"),
        Some(vec!["xclip", "-selection", "clipboard"])
    );
    assert_eq!(
        command(no_env, |name| name == "xsel"),
        Some(vec!["xsel", "--clipboard", "--input"])
    );
    assert_eq!(command(no_env, |_| false), None);
}

#[test]
fn a_session_over_ssh_runs_no_command() {
    for var in ["SSH_CONNECTION", "SSH_TTY"] {
        let env = |name: &str| (name == var).then(|| OsString::from("x"));
        assert_eq!(command(env, |_| true), None, "{var}");
    }
    let other = |name: &str| (name == "TERM").then(|| OsString::from("x"));
    assert_eq!(command(other, |_| true), Some(vec!["pbcopy"]));
}

/// Joins the pipe's thread under the deadline.
#[track_caller]
fn reap(handle: JoinHandle<io::Result<ExitStatus>>) -> io::Result<ExitStatus> {
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("clipboard-reap".to_owned())
        .spawn(move || {
            done.send(handle.join()).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    match Deadline::after(DEADLINE).recv(&finished) {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => panic!("the clipboard thread panicked"),
        Err(_) => panic!("waited {DEADLINE:?} for the clipboard command"),
    }
}

/// Waits until the file at `path` exists, failing after [`DEADLINE`].
/// The hung command creates its marker file first, which proves it has
/// exec'd: the watchdog's scan then matches it.
#[track_caller]
fn wait_for_marker(path: &str) {
    let (done, finished) = mpsc::channel();
    let path = path.to_owned();
    std::thread::Builder::new()
        .name("clipboard-wait".to_owned())
        .spawn(move || {
            // The park between polls reads no clock. The sender stays
            // alive so each `recv_timeout` parks, and the polls stop
            // after about [`DEADLINE`] in 1 ms parks.
            let (_pace_tx, pace) = mpsc::channel::<()>();
            for _ in 0..DEADLINE.as_millis() {
                if std::path::Path::new(&path).exists() {
                    done.send(()).unwrap_or(());
                    return;
                }
                Deadline::after(Duration::from_millis(1))
                    .recv(&pace)
                    .unwrap_or(());
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    if Deadline::after(DEADLINE).recv(&finished).is_err() {
        panic!("waited {DEADLINE:?} for the hung command to start");
    }
}

/// A command line that writes its standard input to `out`.
fn cat_to(out: &str) -> Vec<String> {
    ["/bin/sh", "-c", "cat > \"$0\"", out]
        .map(str::to_owned)
        .to_vec()
}

#[test]
fn the_command_receives_the_text_on_its_standard_input() {
    let dir = fakes::TempDir::new("tui-clipboard");
    let out = dir.path().join("copied").display().to_string();
    let watchdog = fakes::Watchdog::matching(&out);
    let text = "fn main() {}\n\tdone é";
    let handle = pipe(cat_to(&out), text.to_owned()).unwrap_or_else(|err| panic!("pipe: {err}"));
    let status = reap(handle).unwrap_or_else(|err| panic!("command: {err}"));
    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&out).ok().as_deref(), Some(text));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_hung_command_holds_only_its_own_thread() {
    let dir = fakes::TempDir::new("tui-clipboard");
    let marker = dir.path().join("hung").display().to_string();
    let watchdog = fakes::Watchdog::matching(&marker);
    let argv = ["/bin/sh", "-c", ": > \"$0\"; sleep 3600; exit 1", &marker]
        .map(str::to_owned)
        .to_vec();
    let handle = pipe(argv, "x".to_owned()).unwrap_or_else(|err| panic!("pipe: {err}"));
    // pipe returned while the command still runs.
    assert!(!handle.is_finished());
    // The command creates its marker file once it has exec'd; only then
    // can the watchdog's scan match it.
    wait_for_marker(&marker);
    // The watchdog kills the command, which ends the thread.
    drop(watchdog);
    let status = reap(handle).unwrap_or_else(|err| panic!("command: {err}"));
    assert!(!status.success());
}

#[test]
fn a_missing_command_is_an_error_on_its_thread() {
    let handle = pipe(vec!["/nonexistent/fiber-copy".to_owned()], "x".to_owned())
        .unwrap_or_else(|err| panic!("pipe: {err}"));
    assert!(reap(handle).is_err());
    let handle = pipe(Vec::new(), "x".to_owned()).unwrap_or_else(|err| panic!("pipe: {err}"));
    assert!(reap(handle).is_err());
}

#[test]
fn on_path_finds_a_program_in_a_path_directory() {
    assert!(super::on_path("sh"));
    assert!(!super::on_path("fiber-no-such-clipboard-program"));
}
