use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{Ready, escapes_group, ignores_sigterm, leaves_descendants};
use contract::clock::Clock;

use crate::clock::FakeClock;
use crate::deadline::Deadline;
use crate::temp_dir::TempDir;
use crate::watchdog::Watchdog;
use crate::within;
use crate::{kill_group, kill_pid};

const DEADLINE: Duration = Duration::from_secs(5);

fn group_alive(group: u32) -> bool {
    kill_group(group, "0").unwrap()
}

fn pid_alive(pid: u32) -> bool {
    kill_pid(pid, "0").unwrap()
}

#[track_caller]
fn wait_child(mut child: Child) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    assert!(
        Deadline::after(DEADLINE).recv(&finished).is_ok(),
        "waited {DEADLINE:?} for the command to exit"
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
    assert_eq!(ready.wait(DEADLINE), vec![12, 34]);
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
    assert_eq!(ready.wait(DEADLINE), vec![7]);
}

#[test]
fn a_line_that_never_comes_fails_at_the_deadline() {
    let dir = TempDir::new("fiber-ready-deadline");
    let ready = Ready::new(dir.path());
    let within = Duration::from_millis(200);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ready.wait(within)));
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
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
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
        Deadline::after(DEADLINE).recv(&wrote_rx).is_ok(),
        "waited {DEADLINE:?} for {} to be read after SIGTERM",
        block.display()
    );
    assert_eq!(ready.wait(DEADLINE), vec![pgid]);
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
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let descendant = ready.wait(DEADLINE)[0];
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
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let escapee = ready.wait(DEADLINE)[0];
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

#[track_caller]
fn mkfifo(path: &Path) {
    let path = path.to_path_buf();
    let shown = path.display().to_string();
    let status = within("mkfifo to exit", DEADLINE, move || {
        Command::new("mkfifo").arg(&path).status().unwrap()
    });
    assert!(status.success(), "mkfifo {shown} exited {status}");
}

#[test]
fn ready_at_reads_an_existing_fifo() {
    let dir = TempDir::new("fiber-ready-at");
    let fifo = dir.path().join("made.fifo");
    mkfifo(&fifo);
    let clock = FakeClock::new();
    let end = clock.origin() + DEADLINE;
    let ready = Ready::at(&fifo, &|| end.saturating_duration_since(clock.now()));
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
    assert_eq!(ready.wait(DEADLINE), vec![pid]);
    wait_child(child);
}

#[test]
fn ready_at_zero_fails_naming_the_fifo_without_opening_it() {
    let dir = TempDir::new("fiber-ready-at-zero");
    let fifo = dir.path().join("zero.fifo");
    mkfifo(&fifo);
    let at = fifo.clone();
    let failed = std::panic::catch_unwind(|| {
        within("Ready::at with no time left", DEADLINE, move || {
            drop(Ready::at(&at, &|| Duration::ZERO));
        });
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
