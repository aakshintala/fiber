//! `Hold`'s own tests: the order the FIFO keeps.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::io::{BufRead, BufReader, Write as _};
use std::os::unix::fs::FileTypeExt as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use super::*;
use crate::deadline::Deadline;
use crate::{MUST_SUCCEED_WITHIN, TempDir};

/// The drop unlinks the FIFO: a holder that has not opened it yet
/// fails its open with ENOENT and exits instead of parking on the
/// FIFO forever past the test. The reap of a parked holder stays
/// pinned by `drop_reaps_a_parked_holder`.
#[test]
fn drop_unlinks_the_fifo_so_late_starters_fail_open() {
    let dir = TempDir::new("fiber-hold-drop");
    let path = dir.path().join("hold");
    let command = format!("head -n 1 {}", path.display());
    let hold = Hold::park(&path, &command);
    assert_eq!(
        hold.path(),
        path.as_path(),
        "path names the FIFO park made, so the drop below unlinks that FIFO"
    );
    let file_type = std::fs::symlink_metadata(&path).unwrap().file_type();
    assert!(file_type.is_fifo(), "{} is not a FIFO", path.display());
    drop(hold);
    assert!(
        !path.exists(),
        "the drop unlinks the FIFO so a late holder fails open"
    );
}

#[test]
fn park_makes_a_fifo_and_on_opens_an_existing_one() {
    let dir = TempDir::new("fiber-hold-park");
    let path = dir.path().join("hold");
    let command = format!("head -n 1 {}", path.display());
    let hold = Hold::park(&path, &command);
    let file_type = std::fs::symlink_metadata(&path).unwrap().file_type();
    assert!(
        file_type.is_fifo(),
        "park leaves a FIFO at {}",
        path.display()
    );
    assert_eq!(hold.path(), path.as_path(), "path names the FIFO park made");
    hold.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));

    let other = dir.path().join("other");
    crate::fifo(&other);
    // `fifo` panics on an existing path, so reaching `on`'s open below
    // proves `on` made nothing itself.
    let pattern = other.to_str().unwrap().to_owned();
    let reopened = Hold::on(&other, &pattern);
    assert_eq!(
        reopened.path(),
        other.as_path(),
        "on opens the FIFO the test made"
    );
}

/// A release written before the reader opens the FIFO is buffered and
/// read when the reader opens it (`docs/testing.md:297-299`): the
/// release end stays open until the hold is dropped or stood down.
/// The fake keeps the order the FIFO keeps: a release before the next
/// hold (`docs/testing/fakes.md`, "Fakes").
#[test]
fn a_release_written_before_the_reader_opens_is_read() {
    let dir = TempDir::new("fiber-hold-release");
    let path = dir.path().join("hold");
    let command = format!("head -n 1 {}", path.display());
    let mut hold = Hold::park(&path, &command);
    hold.release().write_all(b"\n").unwrap();
    let child = Command::new("head")
        .arg("-n")
        .arg("1")
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let output = crate::wait_output(child, "the FIFO reader to exit");
    assert!(
        output.status.success(),
        "the reader read the buffered release: {}",
        output.status
    );
    assert_eq!(
        output.stdout, b"\n",
        "the reader read the release written before it opened"
    );
    hold.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));
}

/// Dropping the helper without any release write reaps the parked
/// holder: the parent-death cleanup path, which is the leak this
/// helper closes. The pre-drop guard pins the test against vacuity:
/// `try_matching_exits` passes when nothing matches, so without a
/// live match on the holder's own pid the reap below would prove
/// nothing. The test does not pin the drop's own synchronous kill:
/// the watchdog kills the same pattern asynchronously, so no
/// ordering edge separates the two.
#[test]
fn drop_reaps_a_parked_holder() {
    let dir = TempDir::new("fiber-hold-reap");
    let path = dir.path().join("hold");
    // Pid-scoped, so parallel tests cannot cross-match; the absolute
    // path keeps the helper on its hold-path derivation.
    let command = format!("head -n 1 {} #{}", path.display(), std::process::id());
    let hold = Hold::park(&path, &command);
    // A second read+write handle, held open across the drop below: the
    // helper's own handle closing must not EOF-release the holder, so
    // the reap proves the kill rather than the EOF path. Still no
    // release write anywhere in this test.
    let second = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let holder = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = holder.id();
    assert!(crate::pid_alive(pid), "the holder lives before the drop");
    let pattern = path.to_str().unwrap().to_owned();
    let listed = pattern.clone();
    crate::wait_until(
        "the holder to park",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            crate::matching(&listed)
                .map(|pids| pids.contains(&pid))
                .unwrap_or(false)
        },
    );
    drop(hold);
    crate::try_matching_exits(pattern.as_str(), Deadline::after(MUST_SUCCEED_WITHIN))
        .unwrap_or_else(|error| panic!("the dropped helper reaps its holder: {error}"));
    drop(second);
    let _output = crate::finished(
        "the reaped holder",
        holder,
        &[],
        Deadline::after(MUST_SUCCEED_WITHIN),
    );
}

/// `stand_down` closes the release end and disarms the reap: the
/// holder then exits by end-of-file, killed by no signal. The
/// holder's `echo opened` follows its `exec 3<FIFO` open in program
/// order, and the test's receive of that line (a pipe read, then a
/// channel) precedes `stand_down`, so the only writer closes after
/// the reader has opened; a holder that had not opened would
/// otherwise block in `open` forever.
#[test]
fn stand_down_closes_the_end_and_disarms_the_reap() {
    let dir = TempDir::new("fiber-hold-stand-down");
    let path = dir.path().join("hold");
    let script = format!("exec 3<'{}'; echo opened; read x <&3", path.display());
    let hold = Hold::park(&path, &script);
    let mut holder = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let out = holder.stdout.take().unwrap();
    let (line_tx, line_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(out);
        let mut line = String::new();
        if reader.read_line(&mut line).is_ok() {
            match line_tx.send(line) {
                Ok(()) | Err(_) => {}
            }
        }
    });
    let line = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&line_rx, "the holder to open the FIFO and print");
    assert_eq!(
        line, "opened\n",
        "the holder opened the FIFO before the stand-down below"
    );
    hold.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(holder.wait()).unwrap());
    let status = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&finished, "the EOF-released holder to exit")
        .expect("the EOF-released holder exited");
    assert!(
        status.code().is_some(),
        "the holder exited, never killed by a signal: {status}"
    );
}

#[test]
fn the_watchdog_pattern_is_the_path_when_the_command_embeds_it() {
    let dir = TempDir::new("fiber-hold-pattern-path");
    let path = dir.path().join("hold");
    let command = format!("head -n 1 {}", path.display());
    assert_eq!(
        derive_pattern(&command, &path),
        path.to_str().unwrap(),
        "the command embeds the hold path"
    );
}

#[test]
fn the_watchdog_pattern_is_the_pid_scoped_hold_fragment_otherwise() {
    let dir = TempDir::new("fiber-hold-pattern-fragment");
    let path = dir.path().join("hold");
    assert_eq!(
        derive_pattern("head -n 1 hold #123  ", &path),
        "hold #123",
        "the fragment keeps the pid scope with no trailing space"
    );
}

#[test]
fn the_embedded_path_wins_over_the_hold_fragment() {
    let dir = TempDir::new("fiber-hold-pattern-both");
    let path = dir.path().join("hold");
    let command = format!("head -n 1 {} #123", path.display());
    assert_eq!(
        derive_pattern(&command, &path),
        path.to_str().unwrap(),
        "the embedded path wins over the pid-scoped fragment"
    );
}

#[test]
#[should_panic(expected = "derives its watchdog pattern from the command")]
fn a_command_with_neither_panics() {
    let dir = TempDir::new("fiber-hold-pattern-neither");
    let path = dir.path().join("hold");
    let _pattern = derive_pattern("head -n 1", &path);
}
