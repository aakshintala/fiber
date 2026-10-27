//! Binary-level test of `fiber image --png` (`docs/tui.md`, "Images"): the
//! real binary converts a committed JPEG fixture, piped on stdin through
//! `/dev/stdin`, to PNG bytes on stdout. Every wait carries the test's
//! deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::io::Write;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;

use support::{KillGroup, Setup};

/// A committed JPEG fixture, 64 by 48, piped to the child on stdin.
const JPEG: &[u8] = include_bytes!("../../picture/tests/fixtures/cmyk.jpg");

#[test]
fn the_real_binary_converts_a_piped_jpeg_to_png() {
    let setup = Setup::new();
    let mut command = setup.fiber(&["image", "--png", "/dev/stdin"]);
    command.stdin(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let group = child.id();
    let _guard = KillGroup(group);
    let watchdog = fakes::Watchdog::group(group);
    let mut pipe = child.stdin.take().unwrap();
    thread::spawn(move || pipe.write_all(JPEG).unwrap());
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match setup.deadline.recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(
            setup.deadline,
            group,
            &finished,
            "`fiber image --png` to exit",
        ),
    };
    assert!(
        !fakes::group_alive(group),
        "`fiber image --png` left a process behind"
    );
    watchdog.stand_down(setup.deadline.cleanup_phase());
    assert_eq!(output.status.code(), Some(0), "{:?}", output.stderr);
    assert!(
        output.stderr.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout.get(12..24),
        Some([b'I', b'H', b'D', b'R', 0, 0, 0, 64, 0, 0, 0, 48].as_slice())
    );
}
