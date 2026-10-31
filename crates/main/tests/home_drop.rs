//! Binary-level test that a child outliving its home fails the test
//! instead of leaking the directory (`docs/testing.md`, "Levels"): `fiber
//! hub serve` still runs when the home drops, so the drop panics naming the
//! directory, the test and the process.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;

use support::{HubProc, Setup};

/// The failure's text, or a fixed line when it is not a string.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "a non-string panic".to_owned())
}

#[test]
fn a_child_outliving_its_home_fails_naming_the_test_and_the_process() {
    let setup = Setup::new();
    let root = setup.root.path().to_path_buf();
    let test_name = thread::current().name().unwrap().to_owned();
    // The readiness wait is armed before the spawn, so a hub that listens
    // before the first probe is still seen: the predicate is level, paced
    // by the wait, and connects without starting any process.
    let socket = setup.hub_socket();
    let deadline = setup.deadline;
    let (listening, listened) = mpsc::channel();
    thread::spawn(move || {
        fakes::wait_until("the hub to listen", deadline, move || {
            UnixStream::connect(&socket).is_ok()
        });
        match listening.send(()) {
            Ok(()) | Err(_) => {}
        }
    });
    let hub = HubProc::spawn(&setup);
    let pid = hub.child.id();
    deadline.recv_or_fail(&listened, "the hub to listen");
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(setup)));
    let message = match caught {
        Err(payload) => panic_message(&payload),
        Ok(()) => String::new(),
    };
    // The cleanup runs whether or not the drop panicked, so the test
    // itself leaks nothing: kill the group, reap the child through its
    // handle, and remove the directory.
    hub.kill("KILL");
    let HubProc {
        mut child,
        watchdog,
        group,
        ..
    } = hub;
    let (done, finished) = mpsc::channel();
    thread::spawn(move || match done.send(child.wait()) {
        Ok(()) | Err(_) => {}
    });
    let status = match deadline.recv(&finished) {
        Ok(status) => status.unwrap(),
        Err(_) => support::expired(deadline, group, &finished, "the killed hub to exit"),
    };
    assert!(!status.success(), "SIGKILL leaves no clean exit");
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    watchdog.stand_down(deadline.cleanup_phase());
    if message.is_empty() {
        panic!("dropping the home with the hub still running did not fail");
    }
    assert!(
        message.contains(&root.display().to_string()),
        "the failure names no directory: {message}"
    );
    assert!(
        message.contains(&test_name),
        "the failure names no creating test: {message}"
    );
    assert!(
        message.contains(&format!("pid {pid}")),
        "the failure names no holder: {message}"
    );
    assert!(!root.exists(), "the test left {}", root.display());
    assert!(
        fakes::group_empties(group, deadline.cleanup_phase()),
        "the hub left its group behind"
    );
}
