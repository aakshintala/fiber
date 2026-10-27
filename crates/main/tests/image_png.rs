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

/// A PNG's first chunk: the 8-byte signature, then the IHDR length, kind
/// and 13-byte body, whose width and height sit at bytes 16..24.
fn ihdr_size(png: &[u8]) -> Option<(u32, u32)> {
    let header: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D',
        b'R',
    ];
    let body = png.get(..24)?;
    if !body.starts_with(header) {
        return None;
    }
    let width = body
        .get(16..20)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(u32::from_be_bytes)?;
    let height = body
        .get(20..24)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(u32::from_be_bytes)?;
    Some((width, height))
}

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
    assert_eq!(ihdr_size(&output.stdout), Some((64, 48)));
}
