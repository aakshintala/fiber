//! The built framing jig's old-mode journeys: what the wire line reader
//! returns for a torn fragment, a whole line, empty input, CRLF and an
//! over-long line.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{Deadline, Watchdog};

/// The built `framing` example beside the test binary: the jig the tests drive.
/// Warming before returning means every exec site holds only a warmed path.
fn example_executable() -> fakes::WarmedJig {
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/framing");
    fakes::WarmedJig::warm(path, &["--bogus"], 2)
}

struct Jig {
    out: mpsc::Receiver<String>,
    exited: mpsc::Receiver<std::io::Result<ExitStatus>>,
    group: u32,
    watchdog: Option<Watchdog>,
    reaped: bool,
}

impl Jig {
    fn spawn(file: &Path, extra: &[&str]) -> Self {
        let warmed = example_executable();
        let mut child = Command::new(warmed.path())
            .arg(file)
            .args(extra)
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0)
            .spawn()
            .unwrap();
        let group = child.id();
        let watchdog = Some(Watchdog::group(group));
        let mut stdout = child.stdout.take().unwrap();
        let (out_tx, out) = mpsc::channel();
        thread::spawn(move || {
            let mut text = String::new();
            let _read = std::io::Read::read_to_string(&mut stdout, &mut text);
            let _sent = out_tx.send(text);
        });
        let (exit_tx, exited) = mpsc::channel();
        thread::spawn(move || {
            let _sent = exit_tx.send(child.wait());
        });
        Self {
            out,
            exited,
            group,
            watchdog,
            reaped: false,
        }
    }

    /// The jig's stdout, after it exits successfully.
    fn output(mut self) -> String {
        let deadline = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
        let text = deadline.recv_or_fail(&self.out, "the jig printed its lines");
        let status = deadline
            .recv_or_fail(&self.exited, "the jig exited")
            .unwrap();
        self.reaped = true;
        assert!(status.success(), "the jig ran its file: {status}");
        assert!(!fakes::kill_group(self.group, "0").unwrap());
        self.watchdog
            .take()
            .unwrap()
            .stand_down(Deadline::after(fakes::MUST_SUCCEED_WITHIN));
        text
    }
}

impl Drop for Jig {
    fn drop(&mut self) {
        if !self.reaped {
            let _killed = fakes::kill_group(self.group, "KILL");
            let _reaped = Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&self.exited);
        }
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.stand_down(Deadline::after(fakes::MUST_SUCCEED_WITHIN));
        }
    }
}

/// Runs the jig over `bytes` with `extra` args and returns its stdout. The
/// temporary file lives until the jig exits.
fn run_case(bytes: &[u8], extra: &[&str]) -> String {
    let temp = fakes::TempDir::new("framing-jig");
    let file: PathBuf = temp.path().join("input");
    std::fs::write(&file, bytes).unwrap();
    Jig::spawn(&file, extra).output()
}

fn error_after_arg(cut: usize) -> String {
    cut.to_string()
}

#[test]
fn a_torn_fragment_is_lost_with_an_empty_follow_up() {
    let fragment = br#"{"id":"c_2","command":"prompt"}"#;
    let cut = error_after_arg(fragment.len() - 1);
    let out = run_case(fragment, &["--error-after", &cut]);
    assert_eq!(out, "first: err WouldBlock\nsecond: none\n");
}

#[test]
fn a_whole_line_reads_then_ends() {
    let line = br#"{"id":"c_2","command":"prompt"}"#.to_vec();
    let mut bytes = line.clone();
    bytes.push(b'\n');
    let cut = error_after_arg(bytes.len());
    let out = run_case(&bytes, &["--error-after", &cut]);
    assert_eq!(
        out,
        "first: ok {\"id\":\"c_2\",\"command\":\"prompt\"}\nsecond: none\n"
    );
}

#[test]
fn empty_input_ends_at_once() {
    let out = run_case(b"", &["--error-after", "0"]);
    assert_eq!(out, "first: none\nsecond: none\n");
}

#[test]
fn a_crlf_line_strips_the_cr() {
    let out = run_case(b"a\r\n", &["--error-after", "3"]);
    assert_eq!(out, "first: ok a\nsecond: none\n");
}

#[test]
fn an_over_long_line_errors_on_both_reads() {
    let out = run_case(b"abcd", &["--error-after", "4", "--max", "3"]);
    assert_eq!(out, "first: err InvalidData\nsecond: err InvalidData\n");
}

#[test]
fn the_new_mode_returns_the_torn_fragment_with_the_error() {
    let line = br#"{"id":"c_2","command":"prompt"}"#;
    let fragment = &line[..line.len() - 1];
    let cut = error_after_arg(fragment.len());
    let out = run_case(line, &["--error-after", &cut, "--mode", "new"]);
    assert_eq!(
        out,
        format!(
            "first: err WouldBlock partial {}\nsecond: none\n",
            String::from_utf8_lossy(fragment)
        )
    );
}

#[test]
fn the_new_mode_matches_the_old_mode_past_a_whole_line() {
    let mut bytes = br#"{"id":"c_2","command":"prompt"}"#.to_vec();
    bytes.push(b'\n');
    let cut = error_after_arg(bytes.len());
    let out = run_case(&bytes, &["--error-after", &cut, "--mode", "new"]);
    assert_eq!(
        out,
        "first: ok {\"id\":\"c_2\",\"command\":\"prompt\"}\nsecond: none\n"
    );
}

#[test]
fn the_new_mode_matches_the_old_mode_on_empty_input() {
    let out = run_case(b"", &["--error-after", "0", "--mode", "new"]);
    assert_eq!(out, "first: none\nsecond: none\n");
}

#[test]
fn the_new_mode_matches_the_old_mode_on_crlf() {
    let out = run_case(b"a\r\n", &["--error-after", "3", "--mode", "new"]);
    assert_eq!(out, "first: ok a\nsecond: none\n");
}

#[test]
fn the_new_mode_matches_the_old_mode_on_an_over_long_line() {
    let out = run_case(
        b"abcd",
        &["--error-after", "4", "--max", "3", "--mode", "new"],
    );
    assert_eq!(out, "first: err InvalidData\nsecond: err InvalidData\n");
}
