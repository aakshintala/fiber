use crate::MUST_SUCCEED_WITHIN;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{Ready, escapes_group, grouped_output, ignores_sigterm, leaves_descendants};

use crate::deadline::Deadline;
use crate::temp_dir::TempDir;
use crate::watchdog::Watchdog;
use crate::within;
use crate::{group_alive, kill_group, kill_pid, pid_alive};

#[track_caller]
fn wait_child(mut child: Child) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    assert!(
        Deadline::after(MUST_SUCCEED_WITHIN).recv(&finished).is_ok(),
        "waited {MUST_SUCCEED_WITHIN:?} for the command to exit"
    );
}

#[test]
fn a_written_line_is_its_pids() {
    let dir = TempDir::new("fiber-ready");
    let ready = Ready::new(dir.path());
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .open(ready.path())
        .unwrap();
    writeln!(writer, "12 34").unwrap();
    assert_eq!(
        ready.wait(Deadline::after(MUST_SUCCEED_WITHIN)),
        vec![12, 34]
    );
}

#[test]
fn a_writer_that_closes_without_a_line_does_not_drop_the_next_line() {
    let dir = TempDir::new("fiber-ready-eof");
    let ready = Ready::new(dir.path());
    drop(
        std::fs::OpenOptions::new()
            .write(true)
            .open(ready.path())
            .unwrap(),
    );
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .open(ready.path())
        .unwrap();
    writeln!(writer, "7").unwrap();
    drop(writer);
    assert_eq!(ready.wait(Deadline::after(MUST_SUCCEED_WITHIN)), vec![7]);
}

#[test]
fn a_line_that_never_comes_fails_at_the_deadline() {
    let dir = TempDir::new("fiber-ready-deadline");
    let ready = Ready::new(dir.path());
    let within = Duration::from_millis(200);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ready.wait(Deadline::after(within))
    }));
    assert!(result.is_err(), "waited {within:?} should fail the wait");
}

#[test]
fn ignores_sigterm_survives_sigterm() {
    let dir = TempDir::new("fiber-ignore-term");
    let ready = Ready::new(dir.path());
    let mut child = Command::new("/bin/bash")
        .arg("-c")
        .arg(ignores_sigterm(ready.path()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let pgid = ready.wait(Deadline::after(MUST_SUCCEED_WITHIN))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(Deadline::after(MUST_SUCCEED_WITHIN));
    kill_group(pgid, "TERM").unwrap();
    let block = super::block_of(ready.path());
    let fifo = block.clone();
    let (wrote, wrote_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut release = std::fs::OpenOptions::new().write(true).open(&fifo).unwrap();
        writeln!(release, "go").unwrap();
        wrote.send(()).unwrap();
    });
    assert!(
        Deadline::after(MUST_SUCCEED_WITHIN).recv(&wrote_rx).is_ok(),
        "waited {MUST_SUCCEED_WITHIN:?} for {} to be read after SIGTERM",
        block.display()
    );
    assert_eq!(ready.wait(Deadline::after(MUST_SUCCEED_WITHIN)), vec![pgid]);
    assert!(
        child.try_wait().unwrap().is_none(),
        "SIGTERM killed a command that ignores it"
    );
    drop(watchdog);
    wait_child(child);
}

#[test]
fn leaves_descendants_keeps_the_child_after_sigterm() {
    let dir = TempDir::new("fiber-descendants");
    let ready = Ready::new(dir.path());
    let child = Command::new("/bin/bash")
        .arg("-c")
        .arg(leaves_descendants(ready.path()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let pgid = ready.wait(Deadline::after(MUST_SUCCEED_WITHIN))[0];
    let watchdog = Watchdog::group(pgid);
    let descendant = ready.wait(Deadline::after(MUST_SUCCEED_WITHIN))[0];
    kill_group(pgid, "TERM").unwrap();
    wait_child(child);
    assert!(
        pid_alive(descendant),
        "SIGTERM killed the descendant {descendant}"
    );
    drop(watchdog);
    kill_pid(descendant, "KILL").unwrap();
}

#[test]
fn escapes_group_leaves_and_is_not_in_the_group() {
    let dir = TempDir::new("fiber-escape");
    let ready = Ready::new(dir.path());
    let child = Command::new("/bin/bash")
        .arg("-c")
        .arg(escapes_group(ready.path()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let pgid = ready.wait(Deadline::after(MUST_SUCCEED_WITHIN))[0];
    let watchdog = Watchdog::group(pgid);
    let escapee = ready.wait(Deadline::after(MUST_SUCCEED_WITHIN))[0];
    assert_ne!(escapee, pgid);
    assert!(
        !in_group(escapee, pgid),
        "escapee {escapee} stayed in {pgid}"
    );
    assert!(pid_alive(escapee));
    drop(watchdog);
    wait_child(child);
    kill_pid(escapee, "KILL").unwrap();
    assert!(!group_alive(pgid));
}

fn in_group(pid: u32, group: u32) -> bool {
    let output = Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .and_then(|pgid| pgid.parse().ok())
        == Some(group)
}

#[test]
fn ready_at_reads_an_existing_fifo() {
    let dir = TempDir::new("fiber-ready-at");
    let fifo = dir.path().join("made.fifo");
    crate::fifo(&fifo);
    let ready = Ready::at(&fifo, &|| MUST_SUCCEED_WITHIN);
    assert_eq!(ready.path(), fifo);
    let child = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("echo $$ > {}", super::quote(&fifo)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    assert_eq!(ready.wait(Deadline::after(MUST_SUCCEED_WITHIN)), vec![pid]);
    wait_child(child);
}

#[test]
fn ready_at_zero_fails_naming_the_fifo_without_opening_it() {
    let dir = TempDir::new("fiber-ready-at-zero");
    let fifo = dir.path().join("zero.fifo");
    crate::fifo(&fifo);
    let at = fifo.clone();
    let failed = std::panic::catch_unwind(|| {
        within(
            "Ready::at with no time left",
            Deadline::after(MUST_SUCCEED_WITHIN),
            move || {
                drop(Ready::at(&at, &|| Duration::ZERO));
            },
        );
    })
    .expect_err("Ready::at fails with no time left");
    let message = failed.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(
        message.contains(&fifo.display().to_string()),
        "the failure names the fifo: {message}"
    );
    // A writer's non-blocking open fails with no reader: nothing opened it.
    let opened = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits().cast_signed())
        .open(&fifo);
    assert_eq!(
        opened.map(drop).map_err(|err| err.raw_os_error()),
        Err(Some(rustix::io::Errno::NXIO.raw_os_error())),
        "Ready::at opened the fifo with no time left"
    );
}

#[test]
fn grouped_output_runs_the_command_in_its_own_group_and_returns_its_output() {
    let mut printf = Command::new("printf");
    printf.arg("key").stdout(Stdio::piped());
    let output = grouped_output(&mut printf, Duration::from_secs(30)).unwrap();
    assert!(
        output.status.success(),
        "printf failed: {:?}",
        output.status
    );
    assert_eq!(output.stdout, b"key");

    let mut group_of = Command::new("sh");
    group_of
        .args(["-c", "echo $$; ps -o pgid= -p $$"])
        .stdout(Stdio::piped());
    let output = grouped_output(&mut group_of, Duration::from_secs(30)).unwrap();
    assert!(output.status.success(), "sh failed: {:?}", output.status);
    let printed = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = printed.lines().collect();
    assert_eq!(lines.len(), 2, "expected pid and pgid lines: {lines:?}");
    assert!(!lines[0].trim().is_empty(), "the pid line was empty");
    assert_eq!(
        lines[0].trim(),
        lines[1].trim(),
        "the command did not lead its own process group"
    );
}

#[test]
fn grouped_output_reaps_a_descendant_left_in_the_group() {
    let mut lingering = Command::new("sh");
    lingering
        .args(["-c", "sleep 600 >/dev/null 2>&1 & echo $$"])
        .stdout(Stdio::piped());
    let output = grouped_output(&mut lingering, Duration::from_secs(30)).unwrap();
    assert!(output.status.success(), "sh failed: {:?}", output.status);
    let pgid: u32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap();
    assert!(
        !kill_group(pgid, "0").unwrap(),
        "the descendant outlived the run in group {pgid}"
    );
}
