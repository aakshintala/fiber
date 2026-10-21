#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failure is the test's"
)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;

use contract::clock::Clock;
use fakes::clock::FakeClock;

use super::settle;
use super::{FIRST_FRAME_READY, IDLE_READY};
use crate::linux::Counts;
use crate::pty::{OFFER_NEEDLE, Terminal};
use crate::run::PROBE;
use crate::run::{Proc, System};

/// Past this many reads a reader errs, so a loop that never ends fails
/// fast.
const GUARD: usize = 10_000;

fn reading(state: char, voluntary: u64) -> Result<BTreeMap<u32, Counts>, String> {
    Ok(BTreeMap::from([(
        4121,
        Counts {
            voluntary,
            involuntary: 0,
            name: Some("fiber".to_owned()),
            state,
        },
    )]))
}

/// Runs `settle` over `within` with a reader that returns `script(n)` on
/// its `n`th call (from 0). Returns the result, the reads made and how far
/// the clock moved.
fn run(
    within: Duration,
    script: impl Fn(usize) -> Result<BTreeMap<u32, Counts>, String>,
) -> (Result<BTreeMap<u32, Counts>, String>, usize, Duration) {
    let clock = FakeClock::new();
    let start = clock.now();
    let calls = Cell::new(0);
    let got = settle(&*clock, within, || {
        let n = calls.get();
        calls.set(n + 1);
        if n >= GUARD {
            return Err("progress guard".to_owned());
        }
        script(n)
    });
    if let Err(err) = &got {
        assert_ne!(err, "progress guard");
    }
    (got, calls.get(), clock.now() - start)
}

fn from(
    script: Vec<Result<BTreeMap<u32, Counts>, String>>,
) -> impl Fn(usize) -> Result<BTreeMap<u32, Counts>, String> {
    move |n| {
        script
            .get(n)
            .cloned()
            .unwrap_or(Err("past the script".to_owned()))
    }
}

#[test]
fn two_equal_sleeping_readings_settle_on_the_second() {
    let (got, reads, moved) = run(3 * PROBE, from(vec![reading('S', 1), reading('S', 1)]));
    assert_eq!(got, reading('S', 1));
    assert_eq!(reads, 2);
    assert_eq!(moved, PROBE);
}

#[test]
fn a_count_that_moves_waits_for_the_next_reading() {
    let script = vec![reading('S', 1), reading('S', 2), reading('S', 2)];
    let (got, reads, moved) = run(3 * PROBE, from(script));
    assert_eq!(got, reading('S', 2));
    assert_eq!(reads, 3);
    assert_eq!(moved, 2 * PROBE);
}

#[test]
fn an_awake_thread_waits_until_it_sleeps() {
    let script = vec![reading('R', 1), reading('S', 1), reading('S', 1)];
    let (got, reads, _) = run(3 * PROBE, from(script));
    assert_eq!(got, reading('S', 1));
    assert_eq!(reads, 3);
}

#[test]
fn a_reading_that_settles_at_the_deadline_is_taken() {
    let script = vec![
        reading('S', 1),
        reading('S', 2),
        reading('S', 3),
        reading('S', 3),
    ];
    let (got, reads, moved) = run(3 * PROBE, from(script));
    assert_eq!(got, reading('S', 3));
    assert_eq!(reads, 4);
    assert_eq!(moved, 3 * PROBE);
}

#[test]
fn threads_that_never_settle_err_naming_them() {
    let (got, reads, moved) = run(3 * PROBE, |_| reading('R', 1));
    assert_eq!(
        got,
        Err("the threads did not settle within 15 ms: 4121 (fiber) is R".to_owned())
    );
    assert_eq!(reads, 4);
    assert_eq!(moved, 3 * PROBE);
}

#[test]
fn within_zero_errs_after_one_reading() {
    let (got, reads, _) = run(Duration::ZERO, from(vec![reading('S', 1), reading('S', 1)]));
    assert!(got.is_err());
    assert_eq!(reads, 1);
}

#[test]
fn a_failed_read_ends_the_wait_with_its_error() {
    let script = vec![
        reading('S', 1),
        Err("reading /proc/1/task: gone".to_owned()),
    ];
    let (got, reads, _) = run(3 * PROBE, from(script));
    assert_eq!(got, Err("reading /proc/1/task: gone".to_owned()));
    assert_eq!(reads, 2);
}

/// A terminal around `sleep` on a fresh pty, mirroring the scenario
/// prelude: the child ignores its input, keystrokes echo back through
/// canonical echo, and whatever the test draws on `drawer` reaches the
/// screen like a child drawing. The slave keeps echo but not `ECHOCTL`,
/// so Esc echoes literally instead of as `^[`.
fn loopback() -> (Terminal, std::fs::File) {
    let (main, name) = fakes::pty::open().expect("opening a pty for the test");
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&name)
        .expect("opening the pty slave for the test");
    let mut modes = rustix::termios::tcgetattr(&slave).expect("reading the pty flags");
    modes
        .local_modes
        .remove(rustix::termios::LocalModes::ECHOCTL);
    rustix::termios::tcsetattr(&slave, rustix::termios::OptionalActions::Now, &modes)
        .expect("clearing ECHOCTL on the pty");
    let drawer = std::fs::OpenOptions::new()
        .write(true)
        .open(&name)
        .expect("opening the pty drawer for the test");
    let side = || {
        slave
            .try_clone()
            .map(Stdio::from)
            .expect("cloning the pty slave for the test")
    };
    let mut command = std::process::Command::new("sleep");
    command
        .arg("30")
        .stdin(side())
        .stdout(side())
        .stderr(side());
    let proc = Proc::spawn(&mut command, &System).expect("spawning sleep for the test");
    (
        Terminal::watch(proc, main, 60, 12).expect("watching the pty for the test"),
        drawer,
    )
}

/// How long the preludes may take: the bytes round-trip through the pty
/// in milliseconds, so seconds are generous.
const PRELUDE_WITHIN: Duration = Duration::from_secs(10);

/// Draws `marker` the way the child would after the offer is gone: the
/// echoed Esc opens an escape sequence swallowing one byte, while a real
/// child consumes the keystroke and redraws, so the prelude pads one
/// blank first.
fn draw_after_dismiss(drawer: &mut std::fs::File, marker: &str) {
    use std::io::Write;

    drawer
        .write_all(format!(" {marker}").as_bytes())
        .expect("drawing past the echoed Esc");
}

#[test]
fn the_first_frame_prelude_dismisses_the_offer_before_waiting_for_the_prompt() {
    use std::io::Write;

    let (mut terminal, mut drawer) = loopback();
    drawer
        .write_all(OFFER_NEEDLE.as_bytes())
        .expect("showing the offer");
    terminal
        .wait_for(&System, System.now() + PRELUDE_WITHIN, OFFER_NEEDLE)
        .expect("the offer draws");
    terminal
        .dismiss_first_run_offer(&System, System.now() + PRELUDE_WITHIN)
        .expect("dismissing the offer");
    draw_after_dismiss(&mut drawer, FIRST_FRAME_READY);
    terminal
        .wait_for(&System, System.now() + PRELUDE_WITHIN, FIRST_FRAME_READY)
        .expect("the prompt waits after the dismissal");
}

#[test]
fn the_idle_prelude_dismisses_the_offer_before_waiting_for_the_session_line() {
    use std::io::Write;

    let (mut terminal, mut drawer) = loopback();
    drawer
        .write_all(OFFER_NEEDLE.as_bytes())
        .expect("showing the offer");
    terminal
        .wait_for(&System, System.now() + PRELUDE_WITHIN, OFFER_NEEDLE)
        .expect("the offer draws");
    terminal
        .dismiss_first_run_offer(&System, System.now() + PRELUDE_WITHIN)
        .expect("dismissing the offer");
    draw_after_dismiss(&mut drawer, IDLE_READY);
    terminal
        .wait_for(&System, System.now() + PRELUDE_WITHIN, IDLE_READY)
        .expect("the session line waits after the dismissal");
}
