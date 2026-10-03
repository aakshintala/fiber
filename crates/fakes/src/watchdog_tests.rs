use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::Watchdog;

const DEADLINE: Duration = Duration::from_secs(5);

fn group_alive(group: u32) -> bool {
    Command::new("kill")
        .args(["-0", "--", &format!("-{group}")])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[test]
fn dropping_the_watchdog_kills_the_group() {
    let mut child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let group = child.id();
    drop(Watchdog::group(group));
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = match finished.recv_timeout(DEADLINE) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited {DEADLINE:?} for the process group to die"),
    };
    assert_eq!(status.signal(), Some(9));
    assert!(!group_alive(group));
}

#[test]
fn stand_down_leaves_the_group_alive() {
    let mut child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let group = child.id();
    Watchdog::group(group).stand_down(DEADLINE);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    // Long enough that dropping the watchdog would have killed the group.
    assert!(
        finished.recv_timeout(DEADLINE).is_err(),
        "the group died after stand_down"
    );
    match Command::new("kill")
        .args(["-KILL", "--", &format!("-{group}")])
        .status()
    {
        Ok(_) | Err(_) => {}
    }
    assert!(
        finished.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the slept process to exit"
    );
}
