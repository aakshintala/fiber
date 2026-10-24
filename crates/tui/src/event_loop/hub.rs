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
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::{Envelope, Seq, SessionId};

use super::batch::{HUB_BATCH, batch, batchable, ready};
use crate::Input;
use crate::app::{App, history_args, mint, session_command};
use crate::link::{self, Credits, Line};
use crate::retry::Retry;
use crate::tick::TickThread;

/// The most lines one `history` answer holds (`docs/invocation.md`,
/// "Driver commands").
pub(super) const HISTORY_LINES: u64 = 256;

/// How long a `history` wait holds for its answer on the injected clock
/// before ending as a lost connection, and the hub stream's write
/// timeout: one bound for both.
pub(super) const HISTORY_WAIT: Duration = Duration::from_secs(10);

/// What a lost connection fails a fetch with.
pub(super) const LOST: &str = "connection lost";

/// A history answer, or a reopening that ends the wait before its answer.
#[derive(Debug, PartialEq)]
pub(super) enum Answer {
    Lines(Vec<Envelope>),
    Reopened,
}

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

/// What wakes a history wait: the earlier of the frame's deadline and
/// the wait's own. A frame with nothing moving leaves the wait's.
fn earlier(frame_deadline: Option<Instant>, wait_deadline: Instant) -> Option<Instant> {
    Some(frame_deadline.map_or(wait_deadline, |at| at.min(wait_deadline)))
}

/// Sets the hub stream's write timeout: [`HISTORY_WAIT`] in production,
/// a short limit in the kernel test forcing a blocked write.
pub(super) fn set_write_timeout(stream: &UnixStream, timeout: Duration) -> std::io::Result<()> {
    stream.set_write_timeout(Some(timeout))
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
    /// The parsed hub lines the loop has not received yet: the reader
    /// takes a unit before it sends a line, and every receive of an
    /// `Input::Hub` from the channel gives one back.
    credits: Arc<Credits>,
    /// The high-water mark of the stash's length on the answer path:
    /// what `answer` holds while it waits for a history answer.
    #[cfg(test)]
    answer_stash_peak: usize,
    /// The hub thread's permit to connect again (`docs/tui.md`, "A
    /// dropped connection"), held by the connection: it quits the permit
    /// when it drops.
    retry: Arc<Retry>,
}

impl Connection {
    /// A disconnected connection holding nothing, with the hub thread's
    /// permit.
    pub(super) fn new(retry: Arc<Retry>) -> Self {
        Self {
            hub: None,
            stash: VecDeque::new(),
            credits: Credits::new(link::QUEUE),
            #[cfg(test)]
            answer_stash_peak: 0,
            retry,
        }
    }

    /// Holds the hub thread's permit after construction, for a loop that
    /// starts its hub thread once it already holds a connection.
    #[cfg(test)]
    pub(super) fn set_retry(&mut self, retry: Arc<Retry>) {
        self.retry = retry;
    }

    /// Whether a stream is connected.
    pub(super) fn has_stream(&self) -> bool {
        self.hub.is_some()
    }

    /// The credits the hub reader takes at: the loop's receives give
    /// them back.
    pub(super) fn credits(&self) -> Arc<Credits> {
        Arc::clone(&self.credits)
    }

    /// The answer wait's high-water mark: the most inputs `answer` held
    /// at once. Answer-path pushes only.
    #[cfg(test)]
    pub(super) fn answer_stash_peak(&self) -> usize {
        self.answer_stash_peak
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
    /// loop. The first chunk of a logical batch: at most [`HOLD`] for a
    /// hub or tick first, [`HUB_BATCH`] for a terminal-read first.
    pub(super) fn take_batch(&mut self, rx: &Receiver<Input>) -> Option<Batch> {
        let first = match self.stash.pop_front() {
            Some(input) => input,
            None => {
                let input = rx.recv().ok()?;
                self.credits.received(&input);
                input
            }
        };
        // `batch` always returns its first, so splitting it off leaves
        // the rest behind it.
        let mut inputs = batch(first, &mut self.stash, rx, &self.credits, HUB_BATCH).into_iter();
        let first = inputs.next()?;
        Some(Batch {
            first,
            rest: inputs.collect(),
        })
    }

    /// The next chunk of the logical batch `take_batch` started, after
    /// `taken` inputs: `None` when the batch holds [`HUB_BATCH`], when
    /// nothing is ready, or when the next input keeps its own batch — it
    /// waits at the stash's front, the lookahead. Takes nothing more
    /// then, and never blocks.
    pub(super) fn next_chunk(&mut self, rx: &Receiver<Input>, taken: usize) -> Option<Batch> {
        let left = HUB_BATCH.saturating_sub(taken);
        if left == 0 {
            return None;
        }
        let input = ready(&mut self.stash, rx, &self.credits)?;
        if !batchable(&input) {
            self.stash.push_front(input);
            return None;
        }
        let mut inputs = batch(input, &mut self.stash, rx, &self.credits, left).into_iter();
        let first = inputs.next()?;
        Some(Batch {
            first,
            rest: inputs.collect(),
        })
    }

    /// Stores the connected stream, with the history wait's bound as its
    /// write timeout: an expired write fails like a failed one, and
    /// `send` hangs up. A setup that fails leaves no stream: the
    /// `Connected` handler reads that as a failed connection.
    pub(super) fn store(&mut self, stream: UnixStream) {
        self.store_with(stream, |end| set_write_timeout(end, HISTORY_WAIT));
    }

    /// Stores the connected stream with `set` as its write-timeout
    /// setup. A setup that fails shuts the stream down both ways and
    /// closes it without adopting it, so the reader sees the end and
    /// the loop retries like any other lost connection. True when the
    /// stream is adopted. Tests fail `set`.
    pub(super) fn store_with(
        &mut self,
        stream: UnixStream,
        set: impl FnOnce(&UnixStream) -> std::io::Result<()>,
    ) -> bool {
        if set(&stream).is_err() {
            stream.shutdown(std::net::Shutdown::Both).unwrap_or(());
            drop(stream);
            return false;
        }
        self.hub = Some(stream);
        true
    }

    /// Reads `range` with `history`, in commands of at most
    /// [`HISTORY_LINES`] lines, each answered before the next is sent.
    #[allow(
        clippy::too_many_arguments,
        reason = "the fetch carries the range, the app, the tick, the clock and the frame's deadline"
    )]
    pub(super) fn fetch(
        &mut self,
        rx: &Receiver<Input>,
        session: &SessionId,
        range: &RangeInclusive<Seq>,
        app: &mut App,
        tick: &TickThread,
        clock: &dyn Clock,
        frame_deadline: Option<Instant>,
    ) -> Result<Vec<Envelope>, String> {
        let mut lines = Vec::new();
        let mut from = range.start().0;
        while from <= range.end().0 {
            let to = from.saturating_add(HISTORY_LINES - 1).min(range.end().0);
            let id = mint();
            // An ancestor window rides the serving session's chain read:
            // the command stays routed to the serving session, and the
            // args name the ancestor segment. Own windows carry no
            // session, exactly as today: when the serving session is the
            // window's own, it is the same value either way, so no guard
            // is left for mutants to flip.
            let top = app.paging_session().unwrap_or(session);
            let args = history_args(top, session, Some(from), Some(to));
            let command = session_command(&id, "history", top, Some(args)).to_string();
            let written = self
                .hub
                .as_ref()
                .is_some_and(|hub| link::write_line(hub, &command).is_ok());
            if !written {
                self.lost(app);
                return Err(LOST.to_owned());
            }
            let answer = match self.answer(rx, &id, app, tick, clock, frame_deadline)? {
                Answer::Lines(lines) => lines,
                Answer::Reopened => return Ok(Vec::new()),
            };
            let Some(last) = answer.last().and_then(|line| line.seq) else {
                break;
            };
            from = last.0.saturating_add(1);
            lines.extend(answer);
        }
        Ok(lines)
    }

    /// Reads the open's tail with `history`, oldest windows first, until
    /// it fills or ends: one command per window, each answered before
    /// the next is sent. A rejected or unreadable answer ends the tail as
    /// failed with its message; a reopening abandons it without one. A
    /// lost connection ends the read; the loop's `disconnected` already
    /// ended the overlay.
    pub(super) fn fetch_tail(
        &mut self,
        rx: &Receiver<Input>,
        session: &SessionId,
        app: &mut App,
        tick: &TickThread,
        clock: &dyn Clock,
        frame_deadline: Option<Instant>,
    ) -> Result<(), String> {
        while let Some(args) = app.tail_next() {
            let id = mint();
            let command = session_command(&id, "history", session, Some(args)).to_string();
            let written = self
                .hub
                .as_ref()
                .is_some_and(|hub| link::write_line(hub, &command).is_ok());
            if !written {
                self.lost(app);
                return Err(LOST.to_owned());
            }
            match self.answer(rx, &id, app, tick, clock, frame_deadline) {
                Ok(Answer::Lines(lines)) => app.tail_lines(lines),
                Ok(Answer::Reopened) => {
                    app.tail_ended(None);
                    return Ok(());
                }
                Err(message) if message == LOST => return Err(message),
                Err(message) => {
                    app.tail_ended(Some(&message));
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Waits for the answer to command `id`, holding every other input for
    /// after the frame. A lost connection ends the wait; a signal is held
    /// for after the frame and ends the wait as lost, so the loop quits
    /// with the signal's number next. The wait rides the working line's
    /// ticker: the earlier of the frame's deadline and the wait's own
    /// wakes it, so a silent hub ends the wait once the injected clock
    /// passes [`HISTORY_WAIT`].
    pub(super) fn answer(
        &mut self,
        rx: &Receiver<Input>,
        id: &str,
        app: &mut App,
        tick: &TickThread,
        clock: &dyn Clock,
        frame_deadline: Option<Instant>,
    ) -> Result<Answer, String> {
        let start = clock.now();
        // An unrepresentable deadline fails closed: the wait ends at
        // once, as past its bound.
        let wait_deadline = start.checked_add(HISTORY_WAIT).unwrap_or(start);
        tick.arm(earlier(frame_deadline, wait_deadline));
        loop {
            let Ok(input) = rx.recv() else {
                self.lost(app);
                return Err(LOST.to_owned());
            };
            self.credits.received(&input);
            // The deadline wins: at or past it the wait ends as a lost
            // connection, and the input waits for after, a matching
            // answer included. A tick only woke the wait: it is acked,
            // not stashed.
            if clock.now() >= wait_deadline {
                if matches!(input, Input::Tick) {
                    tick.ack();
                } else {
                    self.push_stash(input);
                }
                self.lost(app);
                return Err(LOST.to_owned());
            }
            match input {
                Input::Hub(Line::Session(line)) if link::answers(&line, id) => {
                    return link::history_answer(&line).map(Answer::Lines);
                }
                Input::Hub(Line::Hub(line)) if line.kind == "stream_reopened" => {
                    if let Some(session) = line
                        .payload
                        .get("session_id")
                        .and_then(serde_json::Value::as_str)
                    {
                        app.reopen_pending(SessionId(session.to_owned()));
                    }
                    app.abandon_history(id.to_owned());
                    self.push_stash(Input::Hub(Line::Hub(line)));
                    return Ok(Answer::Reopened);
                }
                // A tick while a frame waits for history is acked and
                // dropped, not stashed: the frame asks again after it
                // draws. The wait's own deadline wakes what is left of
                // the wait; the frame re-arms its own after it draws.
                Input::Tick => {
                    tick.ack();
                    tick.arm(Some(wait_deadline));
                }
                Input::Disconnected => {
                    self.on_disconnected(app);
                    return Err(LOST.to_owned());
                }
                // A signal while a frame waits for history is held ahead
                // of every later input, and the wait ends as lost: the
                // loop quits with the signal's number before handling
                // anything behind it.
                Input::Signal(signal) => {
                    self.push_stash(Input::Signal(signal));
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
                | Input::Feedback(_)
                | Input::Install { .. }
                | Input::Login { .. }
                | Input::Files { .. }) => self.push_stash(other),
            }
        }
    }

    /// Holds `input` for after the frame, recording the answer wait's
    /// high-water mark. Only answer-path pushes count.
    fn push_stash(&mut self, input: Input) {
        self.stash.push_back(input);
        #[cfg(test)]
        {
            self.answer_stash_peak = self.answer_stash_peak.max(self.stash.len());
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
        if let Some(delay) = app.next_retry() {
            self.retry.give(delay);
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
            app.outbox.wrote(line);
        }
    }

    /// Stops the hub reader, releases its capacity wait, shuts the hub
    /// stream down both ways and drops it. The reader thread then sees
    /// the end, even when the kernel misses the shutdown's wakeup
    /// (#1877). Ending the tracked read before cancelling means a reader
    /// `Retry` stops is always a cancelled reader.
    pub(super) fn hang_up(&mut self) {
        self.retry.end_read();
        self.credits.cancel();
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
        self.retry.quit();
        self.hang_up();
    }
}
