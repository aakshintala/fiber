#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a failure is the test's"
)]

use std::process::Stdio;

use crate::pty::Terminal;
use crate::run::{Proc, System};

/// A terminal around `sleep` on a fresh pty, with a second slave handle for
/// drawing: the child ignores its input, and whatever the test draws on
/// `drawer` reaches the screen like a child drawing. The slave keeps echo
/// but not `ECHOCTL`, so Esc echoes literally instead of as `^[`.
pub(crate) fn loopback() -> (Terminal, std::fs::File) {
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
