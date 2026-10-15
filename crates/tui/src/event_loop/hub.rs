//! The hub connection (`docs/tui.md`, "History and paging" and "A dropped
//! connection"): the stream commands are written on, the inputs held while a
//! frame waits for history, and the hub thread's permit to connect again.
//! The loop reaches all three only through this interface; the connection
//! never reaches the loop's state, taking the app and the tick as explicit
//! borrowed arguments instead.

use std::collections::VecDeque;
use std::ops::RangeInclusive;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use contract::{Envelope, Seq, SessionId};

use super::batch::batch;
use crate::Input;
use crate::app::{App, mint, session_command};
use crate::link::{self, Line};
use crate::retry::Retry;
use crate::tick::TickThread;

/// The most lines one `history` answer holds (`docs/invocation.md`,
/// "Driver commands").
pub(super) const HISTORY_LINES: u64 = 256;

/// What a lost connection fails a fetch with.
pub(super) const LOST: &str = "connection lost";

/// A batch always holds its first input, with the inputs ready behind
/// it folded after in arrival order.
pub(super) struct Batch {
    first: Input,
    rest: Vec<Input>,
}

impl Batch {
    /// The batch's first input, in arrival order.
    pub(super) fn first(&self) -> &Input {
        &self.first
    }
}

impl IntoIterator for Batch {
    type Item = Input;
    type IntoIter = std::iter::Chain<std::option::IntoIter<Input>, std::vec::IntoIter<Input>>;

    fn into_iter(self) -> Self::IntoIter {
        Some(self.first).into_iter().chain(self.rest)
    }
}

/// The hub stream, the held inputs and the hub thread's permit, with the
/// interface the loop uses. All state stays private: the loop passes the
/// app and the tick to each method that needs them.
pub(super) struct Connection {
    /// The hub stream commands are written on, once connected.
    hub: Option<UnixStream>,
    /// Inputs that arrived while a frame waited for history, handled next
    /// in arrival order.
    stash: VecDeque<Input>,
    /// The hub thread's permit to connect again (`docs/tui.md`, "A
    /// dropped connection"); none in tests with no hub thread.
    retry: Option<Arc<Retry>>,
}

impl Connection {
    /// A disconnected connection holding nothing, with the hub thread's
    /// permit when one is handed over.
    pub(super) fn new(retry: Option<Arc<Retry>>) -> Self {
        Self {
            hub: None,
            stash: VecDeque::new(),
            retry,
        }
    }

    /// Holds the hub thread's permit after construction, for a loop that
    /// starts its hub thread once it already holds a connection.
    #[cfg(test)]
    pub(super) fn set_retry(&mut self, retry: Arc<Retry>) {
        self.retry = Some(retry);
    }

    /// Whether a stream is connected.
    pub(super) fn has_stream(&self) -> bool {
        self.hub.is_some()
    }

    /// The connected stream, for the test that fails the write side.
    #[cfg(test)]
    pub(super) fn stream(&self) -> Option<&UnixStream> {
        self.hub.as_ref()
    }

    /// The inputs held while a frame waited, oldest first.
    #[cfg(test)]
    pub(super) fn stashed(&self) -> &VecDeque<Input> {
        &self.stash
    }

    /// Takes the oldest held input, for the test draining the stash in
    /// arrival order.
    #[cfg(test)]
    pub(super) fn pop_stashed(&mut self) -> Option<Input> {
        self.stash.pop_front()
    }

    /// The next batch `run` handles: the stash's front when one waits,
    /// else the channel's, with what is ready behind it folded in. None
    /// when the stash is empty and every sender is gone, which ends the
    /// loop.
    pub(super) fn take_batch(&mut self, rx: &Receiver<Input>) -> Option<Batch> {
        let first = match self.stash.pop_front() {
            Some(input) => input,
            None => rx.recv().ok()?,
        };
        // `batch` always returns its first, so splitting it off leaves
        // the rest behind it.
        let mut inputs = batch(first, &mut self.stash, rx).into_iter();
        let first = inputs.next()?;
        Some(Batch {
            first,
            rest: inputs.collect(),
        })
    }

    /// Stores the connected stream.
    pub(super) fn store(&mut self, stream: UnixStream) {
        self.hub = Some(stream);
    }

    /// Reads `range` with `history`, in commands of at most
    /// [`HISTORY_LINES`] lines, each answered before the next is sent.
    pub(super) fn fetch(
        &mut self,
        rx: &Receiver<Input>,
        session: &SessionId,
        range: &RangeInclusive<Seq>,
        app: &mut App,
        tick: &TickThread,
    ) -> Result<Vec<Envelope>, String> {
        let mut lines = Vec::new();
        let mut from = range.start().0;
        while from <= range.end().0 {
            let to = from.saturating_add(HISTORY_LINES - 1).min(range.end().0);
            let id = mint();
            let args = serde_json::json!({"from_seq": from, "to_seq": to});
            let command = session_command(&id, "history", session, Some(args)).to_string();
            let written = self
                .hub
                .as_ref()
                .is_some_and(|hub| link::write_line(hub, &command).is_ok());
            if !written {
                self.lost(app);
                return Err(LOST.to_owned());
            }
            let answer = self.answer(rx, &id, app, tick)?;
            let Some(last) = answer.last().and_then(|line| line.seq) else {
                break;
            };
            from = last.0.saturating_add(1);
            lines.extend(answer);
        }
        Ok(lines)
    }

    /// Waits for the answer to command `id`, holding every other input for
    /// after the frame. A lost connection ends the wait; a signal is held
    /// for after the frame and ends the wait as lost, so the loop quits
    /// with the signal's number next.
    pub(super) fn answer(
        &mut self,
        rx: &Receiver<Input>,
        id: &str,
        app: &mut App,
        tick: &TickThread,
    ) -> Result<Vec<Envelope>, String> {
        loop {
            let Ok(input) = rx.recv() else {
                self.lost(app);
                return Err(LOST.to_owned());
            };
            match input {
                Input::Hub(Line::Session(line)) if link::answers(&line, id) => {
                    return link::history_answer(&line);
                }
                // A tick while a frame waits for history is acked and
                // dropped, not stashed: the frame asks again after it
                // draws.
                Input::Tick => tick.ack(),
                Input::Disconnected => {
                    self.on_disconnected(app);
                    return Err(LOST.to_owned());
                }
                // A signal while a frame waits for history is held ahead
                // of every later input, and the wait ends as lost: the
                // loop quits with the signal's number before handling
                // anything behind it.
                Input::Signal(signal) => {
                    self.stash.push_back(Input::Signal(signal));
                    return Err(LOST.to_owned());
                }
                other @ (Input::Bytes(_)
                | Input::Hub(_)
                | Input::Connected(..)
                | Input::ConnectFailed(_)
                | Input::Resize
                | Input::FindDue(_)
                | Input::Image { .. }
                | Input::Viewed { .. }
                | Input::Models(_)
                | Input::Login { .. }
                | Input::Files { .. }) => self.stash.push_back(other),
            }
        }
    }

    /// The hub connection ended: the reader sends this once per stream,
    /// so each ended connection gives the hub thread one permit.
    pub(super) fn on_disconnected(&mut self, app: &mut App) {
        self.hub = None;
        app.disconnected();
        self.retry_later(app);
    }

    /// Gives the hub thread its permit for one failure, after the delay
    /// the app names; none once the hub refused this terminal's schema.
    pub(super) fn retry_later(&mut self, app: &mut App) {
        if let (Some(delay), Some(retry)) = (app.next_retry(), &self.retry) {
            retry.give(delay);
        }
    }

    /// The connection ended during a fetch: nothing more is asked. The
    /// reader's end of the stream gives the permit.
    pub(super) fn lost(&mut self, app: &mut App) {
        self.hang_up();
        app.disconnected();
    }

    /// Writes command lines to the hub, each one written kept for resending
    /// until it is answered. A failed write hangs up: the connection is
    /// lost, and the text of the lines not sent returns to the draft.
    pub(super) fn send(&mut self, lines: &[String], app: &mut App) {
        for (at, line) in lines.iter().enumerate() {
            let Some(hub) = &mut self.hub else {
                return;
            };
            if link::write_line(hub, line).is_err() {
                self.hang_up();
                app.write_failed(lines.get(at..).unwrap_or_default());
                return;
            }
            app.wrote(line);
        }
    }

    /// Stops the hub reader, shuts the hub stream down both ways and drops
    /// it. The reader thread then sees the end, even when the kernel misses
    /// the shutdown's wakeup (#1877).
    pub(super) fn hang_up(&mut self) {
        if let Some(retry) = &self.retry {
            retry.end_read();
        }
        if let Some(hub) = self.hub.take() {
            hub.shutdown(std::net::Shutdown::Both).unwrap_or(());
        }
    }
}

/// The connection is gone: the hub thread stops waiting for a permit, and
/// a reader blocked on the stream sees its end. The permit's quit comes
/// before the hang-up.
impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(retry) = &self.retry {
            retry.quit();
        }
        self.hang_up();
    }
}
