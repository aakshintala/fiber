//! The hub reaper hands its child to its own thread: the thread waits
//! for the child and reports its status, so no hub child stays a zombie.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use fakes::Deadline;

/// One reap's deadline: the wait below fails the test at it instead of
/// hanging it.
const REAP_DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn on_thread_reaps_the_handed_off_child_and_reports_its_status() {
    let mut child = Command::new("/bin/sh");
    child
        .arg("-c")
        .arg("exit 3")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let child = child.spawn().expect("the hub child starts");
    // Taken before the hand-off: after it the child belongs to the reaper.
    let group = child.id();
    // Held until the join below: dropping it would signal a group that is
    // already empty.
    let watchdog = fakes::Watchdog::group(group);
    let handle = super::on_thread(child).expect("the reaper starts");
    // Joined on a helper thread, so a reap that never ends fails the test
    // at the deadline instead of hanging it.
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = match handle.join() {
            Ok(outcome) => outcome,
            Err(_) => panic!("the reap thread ends with the wait's answer"),
        };
        done.send(outcome).unwrap_or(());
    });
    let status = Deadline::after(REAP_DEADLINE)
        .recv(&finished)
        .expect("the reap ends before its deadline")
        .expect("the hub child's wait");
    assert_eq!(status.code(), Some(3));
    watchdog.stand_down(fakes::Deadline::after(REAP_DEADLINE));
}
