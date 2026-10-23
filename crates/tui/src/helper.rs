//! The copy, link-open and image-viewer commands (`docs/tui.md`, "Selection
//! and copy", "Links" and "Images"): one start, one deadline.
//!
//! A command runs in a process group of its own, so quitting the terminal
//! never takes it along, with nothing on stdout or stderr. It gets
//! [`READ_LIMIT`] on the injected clock. At the deadline the helper stops the
//! command's own process, never its group: what the command started, such as
//! the selection owner `wl-copy` leaves behind, keeps running.
//!
//! The wait parks on the clock through a wake pipe, as the paste reader
//! does: every clock move and the command's exit write a byte to the pipe,
//! so the poll wakes, the helper re-reads the clock and parks again. The
//! text for standard input goes out in the same poll, so a command that
//! never reads it, or a descendant that holds the pipe, never holds the
//! helper past the deadline.

use std::io;
use std::os::unix::io::AsFd as _;
use std::os::unix::process::CommandExt as _;
use std::process::{ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use contract::clock::{Clock, Wake};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};

use crate::paste_image::READ_LIMIT;

/// How a bounded command ended.
#[derive(Debug)]
pub(crate) enum Ended {
    /// The command exited, on its own or by a signal, before the deadline.
    Exited(ExitStatus),
    /// The command was still running at the deadline and was stopped. Not a
    /// failure: a viewer open for as long as the person looks is expected.
    DeadlinePassed,
}

/// Runs `argv` with `stdin` on its standard input, and returns when it
/// exits or [`READ_LIMIT`] has passed on `clock`, whichever is first: blocking,
/// on a thread of its own. At the deadline the command's own process gets
/// SIGKILL and is reaped; its process group is never signalled. An empty
/// `argv`, a command that cannot start, or a wake pipe or watcher thread
/// that cannot start is the error.
pub(crate) fn run_bounded(
    argv: &[String],
    stdin: Option<String>,
    clock: &Arc<dyn Clock>,
) -> io::Result<Ended> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::other("no command"))?;
    let deadline = clock
        .now()
        .checked_add(READ_LIMIT)
        .unwrap_or_else(|| clock.now());
    let (wake_read, wake_write) = std::io::pipe()?;
    nonblocking(&wake_read);
    nonblocking(&wake_write);
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    let id = child.id();
    let mut feed = stdin.zip(child.stdin.take()).map(|(text, stdin)| {
        nonblocking(&stdin);
        Feed {
            stdin,
            text: text.into_bytes(),
            sent: 0,
        }
    });
    let wake: Arc<dyn Wake> = Arc::new(PipeWake {
        write: Mutex::new(wake_write),
    });
    let weak: Weak<dyn Wake> = Arc::downgrade(&wake);
    clock.subscribe(weak);
    let watcher_wake = Arc::clone(&wake);
    let watcher = match crate::sources::builder("tui-helper-watch").spawn(move || {
        wait_for_exit(id);
        watcher_wake.wake();
    }) {
        Ok(watcher) => watcher,
        Err(error) => {
            drop(feed);
            stop(&mut child);
            return Err(error);
        }
    };
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(Ended::Exited(status)),
            Ok(None) => {}
            Err(error) => break Err(error),
        }
        if clock.now() >= deadline {
            break Ok(Ended::DeadlinePassed);
        }
        if feed.as_mut().is_some_and(Feed::pump) {
            // All sent, or the command closed its end: the end of the
            // text is the end of standard input.
            feed = None;
        }
        clock.wait_until(Some(deadline), &mut |bound| {
            let timeout = bound.map(|left| Timespec::try_from(left).unwrap_or_default());
            match feed.as_ref() {
                Some(feed) => {
                    let mut fds = [
                        PollFd::new(&wake_read, PollFlags::IN),
                        PollFd::new(&feed.stdin, PollFlags::OUT),
                    ];
                    match poll(&mut fds, timeout.as_ref()) {
                        Ok(_) | Err(_) => {}
                    }
                }
                None => {
                    let mut fds = [PollFd::new(&wake_read, PollFlags::IN)];
                    match poll(&mut fds, timeout.as_ref()) {
                        Ok(_) | Err(_) => {}
                    }
                }
            }
            drain(&wake_read);
        });
    };
    if !matches!(outcome, Ok(Ended::Exited(_))) {
        // The unwritten text is dropped with the write end, which never
        // blocks; the child is unreaped, so its pid is still its own.
        drop(feed);
        stop(&mut child);
    }
    match watcher.join() {
        Ok(()) | Err(_) => {}
    }
    outcome
}

/// Starts [`run_bounded`] on a thread of its own and returns at once. The
/// outcome is dropped: a command that fails, or runs past the deadline, is
/// not reported. The error is the thread that could not start.
pub(crate) fn spawn_detached(
    argv: Vec<String>,
    stdin: Option<String>,
    clock: Arc<dyn Clock>,
) -> io::Result<()> {
    crate::sources::builder("tui-helper")
        .spawn(move || drop(run_bounded(&argv, stdin, &clock)))
        .map(drop)
}

/// SIGKILL to the child's own process, then the reap. Only for a child not
/// yet reaped, whose pid is therefore still its own. The result is
/// discarded: the child may have exited already.
fn stop(child: &mut std::process::Child) {
    drop(support::group::signal_process(child.id(), Signal::KILL));
    match child.wait() {
        Ok(_) | Err(_) => {}
    }
}

/// Sets `fd` non-blocking. A failure is ignored: a blocking descriptor only
/// loses the bound the poll gives, never the kill.
fn nonblocking(fd: &impl std::os::unix::io::AsFd) {
    match rustix::io::ioctl_fionbio(fd.as_fd(), true) {
        Ok(_) | Err(_) => {}
    }
}

/// The text still to write to the command's standard input.
struct Feed {
    /// The write end, non-blocking.
    stdin: ChildStdin,
    /// All of the text.
    text: Vec<u8>,
    /// How many bytes have gone out.
    sent: usize,
}

impl Feed {
    /// Writes what the pipe takes. `true` when nothing is left to write: all
    /// of it went out, or the command closed its end (`EPIPE`, which the
    /// runtime leaves as an error), so the exit status alone decides.
    fn pump(&mut self) -> bool {
        loop {
            let Some(rest) = self.text.get(self.sent..).filter(|rest| !rest.is_empty()) else {
                return true;
            };
            match rustix::io::write(self.stdin.as_fd(), rest) {
                Ok(0) => return true,
                Ok(written) => self.sent += written,
                Err(Errno::INTR) => {}
                Err(Errno::AGAIN) => return false,
                Err(_) => return true,
            }
        }
    }
}

/// The wake pipe's writer: every clock move writes one byte to it, so the
/// worker's poll wakes. A write that fails, on a full pipe or once the
/// worker has gone, is ignored: a byte already waiting wakes as well.
pub(crate) struct PipeWake {
    /// The pipe's write end.
    pub(crate) write: Mutex<std::io::PipeWriter>,
}

impl Wake for PipeWake {
    fn wake(&self) {
        let write = self.write.lock().unwrap_or_else(PoisonError::into_inner);
        match rustix::io::write(write.as_fd(), b"x") {
            Ok(_) | Err(_) => {}
        }
    }
}

/// Drains the wake pipe after each wake, until it would block: the pipe
/// holds at most its capacity, so the drain ends.
pub(crate) fn drain(read: &std::io::PipeReader) {
    let mut buf = [0u8; 64];
    loop {
        match rustix::io::read(read.as_fd(), &mut buf) {
            Ok(0) => return,
            Ok(_) => {}
            Err(Errno::AGAIN) => return,
            Err(Errno::INTR) => {}
            Err(_) => return,
        }
    }
}

/// Blocks in `waitid` for the child `id`, seeing its exit without reaping
/// it. An interrupted wait retries; any other error counts as exited.
pub(crate) fn wait_for_exit(id: u32) {
    if support::group::refused(id) {
        return;
    }
    let Some(pid) = i32::try_from(id).ok().and_then(Pid::from_raw) else {
        return;
    };
    loop {
        match rustix::process::waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED.union(WaitIdOptions::NOWAIT),
        ) {
            Ok(_) => return,
            Err(Errno::INTR) => {}
            Err(_) => return,
        }
    }
}

#[cfg(test)]
#[path = "helper_tests.rs"]
mod tests;
