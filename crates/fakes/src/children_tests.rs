use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{Ready, escapes_group, ignores_sigterm, leaves_descendants};
use crate::temp_dir::TempDir;
use crate::watchdog::Watchdog;
use crate::{kill_group, kill_pid};

const DEADLINE: Duration = Duration::from_secs(5);

fn group_alive(group: u32) -> bool {
    kill_group(group, "0").unwrap()
}

fn pid_alive(pid: u32) -> bool {
    kill_pid(pid, "0").unwrap()
}

fn wait_child(mut child: Child) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    assert!(
        finished.recv_timeout(DEADLINE).is_ok(),
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
        wrote_rx.recv_timeout(DEADLINE).is_ok(),
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
