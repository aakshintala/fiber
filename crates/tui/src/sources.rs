//! The threads that send the loop its inputs: the tty reader, which pauses
//! while another program has the terminal, the hub connection and its lines,
//! and SIGWINCH (`docs/tui.md`, "What the terminal is").

use std::fs::File;
use std::io::{self, PipeReader, PipeWriter};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;

use signal_hook::iterator::Signals;

use crate::link;
use crate::{Connect, Input};

/// The input reader's state, shared with the loop under one mutex.
#[derive(Debug, Default)]
struct ReaderState {
    /// The loop asked the reader to stop reading the tty.
    paused: bool,
    /// The reader stopped and waits for `paused` to clear.
    parked: bool,
    /// The reader returned, or never started: nothing to wait for.
    ended: bool,
}

/// The pause handshake: the state and the condition variable each change
/// is announced on.
#[derive(Debug, Default)]
struct Gate {
    state: Mutex<ReaderState>,
    changed: Condvar,
}

impl Gate {
    /// The state, even after a thread panicked holding it.
    fn lock(&self) -> MutexGuard<'_, ReaderState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Waits on the condition variable while `blocked` holds.
    fn wait_while<'a>(
        &self,
        guard: MutexGuard<'a, ReaderState>,
        blocked: impl FnMut(&mut ReaderState) -> bool,
    ) -> MutexGuard<'a, ReaderState> {
        self.changed
            .wait_while(guard, blocked)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records that the reader returned.
    fn end(&self) {
        self.lock().ended = true;
        self.changed.notify_all();
    }
}

/// The stack of each terminal thread. A thread's default 2 MiB stack can
/// land on a 2 MiB-aligned span of its own, and the kernel then backs its
/// first touch with one 2 MiB huge page: the idle terminal measured 2 MiB
/// more in about half its runs. A stack under 2 MiB cannot hold such a
/// span. The threads read, parse one line and send, so 1 MiB is ample.
const STACK: usize = 1_048_576;

/// A builder for the terminal thread `name`, with [`STACK`].
pub(crate) fn builder(name: &str) -> thread::Builder {
    thread::Builder::new()
        .name(name.to_owned())
        .stack_size(STACK)
}

/// Reads the tty on its own thread, polling it and a wake pipe, so the loop
/// can stop it from reading while another program has the terminal. Left
/// blocked on quit; it ends with the process.
pub(crate) struct Reader {
    gate: Arc<Gate>,
    /// Written to wake the reader from its poll.
    wake: PipeWriter,
}

impl Reader {
    /// Starts reading `tty`, sending each read as [`Input::Bytes`]. `None`
    /// when the reader cannot start: the terminal then reads no keys, the
    /// hub thread may still report, and Ctrl+C from the shell ends it.
    pub(crate) fn spawn(tty: &File, tx: Sender<Input>) -> Option<Self> {
        let tty = tty.try_clone().ok()?;
        let (woken, wake) = io::pipe().ok()?;
        let gate = Arc::new(Gate::default());
        let shared = Arc::clone(&gate);
        builder("tui-input")
            .spawn(move || {
                read_input(tty, &woken, &shared, &tx);
                shared.end();
            })
            .ok()?;
        Some(Self { gate, wake })
    }

    /// Stops the reader from reading the tty, returning once it has parked
    /// or ended. Bytes it read before parking are already sent.
    pub(crate) fn pause(&mut self) {
        let mut state = self.gate.lock();
        state.paused = true;
        drop(state);
        // A failed write leaves a reader blocked in poll; it parks on the
        // next tty byte, or the loop waits for it until it does.
        io::Write::write_all(&mut self.wake, &[0]).unwrap_or(());
        let state = self.gate.lock();
        drop(
            self.gate
                .wait_while(state, |state| !state.parked && !state.ended),
        );
    }

    /// Lets a paused reader read the tty again.
    pub(crate) fn resume(&self) {
        self.gate.lock().paused = false;
        self.gate.changed.notify_all();
    }
}

/// The reader thread: polls `tty` and `woken`; after every wake drains the
/// pipe, parks while paused, and otherwise reads the tty. Returns on the
/// tty's end, a failed read or poll, or a closed channel.
fn read_input(mut tty: File, mut woken: &PipeReader, gate: &Gate, tx: &Sender<Input>) {
    use rustix::event::{PollFd, PollFlags, poll};
    let mut buf = [0u8; 4096];
    loop {
        let mut fds = [
            PollFd::new(&tty, PollFlags::IN),
            PollFd::new(woken, PollFlags::IN),
        ];
        match poll(&mut fds, None) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(_) => return,
        }
        let [tty_ready, wake_ready] = fds.map(|fd| !fd.revents().is_empty());
        if wake_ready && io::Read::read(&mut woken, &mut buf).is_err() {
            return;
        }
        let mut state = gate.lock();
        if state.paused {
            state.parked = true;
            gate.changed.notify_all();
            state = gate.wait_while(state, |state| state.paused);
            state.parked = false;
            continue;
        }
        drop(state);
        if !tty_ready {
            continue;
        }
        let Ok(read) = io::Read::read(&mut tty, &mut buf) else {
            return;
        };
        let Some(bytes) = buf.get(..read).filter(|bytes| !bytes.is_empty()) else {
            return;
        };
        if tx.send(Input::Bytes(bytes.to_vec())).is_err() {
            return;
        }
    }
}

/// Connects to the hub on its own thread, after the first frame, then
/// reads its lines.
pub(crate) fn spawn_hub(connect: Connect, tx: Sender<Input>) {
    let hub = builder("tui-hub").spawn(move || {
        let connected = connect().and_then(|(stream, hello)| {
            let reader = stream.try_clone()?;
            Ok((stream, reader, hello))
        });
        match connected {
            Ok((stream, reader, hello)) => {
                if tx.send(Input::Connected(stream, hello)).is_ok() {
                    link::read_lines(reader, &tx);
                }
            }
            Err(error) => drop(tx.send(Input::ConnectFailed(error.to_string()))),
        }
    });
    drop(hub);
}

/// Turns SIGWINCH into [`Input::Resize`] on its own thread. Left blocked on
/// quit; it ends with the process.
pub(crate) fn spawn_resize(mut signals: Signals, tx: Sender<Input>) {
    let resize = builder("tui-resize").spawn(move || {
        for _ in signals.forever() {
            if tx.send(Input::Resize).is_err() {
                return;
            }
        }
    });
    drop(resize);
}

#[cfg(test)]
#[path = "sources_tests.rs"]
mod tests;
