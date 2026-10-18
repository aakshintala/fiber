//! Hub lines out and in: command lines written one per line, and the
//! reader that splits hub lines from session envelopes.

use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{SendError, Sender};
use std::sync::{Arc, Condvar, Mutex};

use contract::{Envelope, HubLine};

use crate::Input;

/// One line read from the hub.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Line {
    /// A line for the connection: `hub_hello`, or the answer to a hub
    /// command.
    Hub(HubLine),
    /// A session's line, relayed by the hub.
    Session(Envelope),
}

/// Splits one line: a session's lines carry `session_id`, the hub's own do
/// not. A line that is neither is `None`, and the reader drops it.
pub(crate) fn parse_line(text: &str) -> Option<Line> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if value.get("session_id").is_some() {
        serde_json::from_value(value).ok().map(Line::Session)
    } else {
        serde_json::from_value(value).ok().map(Line::Hub)
    }
}

/// Whether `line` answers command `id`: the accepted or rejected answer
/// carrying its id.
pub(crate) fn answers(line: &Envelope, id: &str) -> bool {
    matches!(line.kind.as_str(), "command_accepted" | "command_rejected")
        && line
            .payload
            .get("command_id")
            .and_then(serde_json::Value::as_str)
            == Some(id)
}

/// Reads a `history` answer: its durable lines on `command_accepted`,
/// or why there are none on `command_rejected` or an unreadable answer.
/// The loop and the search both read answers through it, so the two
/// cannot read one differently (`docs/tui.md`, "History and paging").
pub(crate) fn history_answer(line: &Envelope) -> Result<Vec<Envelope>, String> {
    if line.kind != "command_accepted" {
        return Err(line
            .payload
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("rejected")
            .to_owned());
    }
    line.payload
        .get("result")
        .and_then(|result| result.get("lines"))
        .cloned()
        .and_then(|lines| serde_json::from_value(lines).ok())
        .ok_or_else(|| "the answer could not be read".to_owned())
}

/// Writes one command line.
pub(crate) fn write_line(mut stream: &UnixStream, line: &str) -> std::io::Result<()> {
    static WRITE: Mutex<()> = Mutex::new(());
    let _write = support::lock(&WRITE);
    stream.write_all(format!("{line}\n").as_bytes())?;
    stream.flush()
}

/// The parsed hub lines allowed in the channel: K counts
/// `1 + QUEUE + HOLD + 1 = 130` live parsed hub events at once — the
/// reader's pending line, the channel, the batch and the one lookahead.
pub(crate) const QUEUE: usize = 64;

/// Counts the parsed hub lines the loop has not received yet: the
/// reader takes a unit before it sends a line, and the connection gives
/// one back at every receive of an `Input::Hub` from the channel.
/// A `Mutex` plus a `Condvar` (`support::lock`), no clock, no timeout.
pub(crate) struct Credits {
    cap: usize,
    state: Mutex<State>,
    changed: Condvar,
}

/// What the units count, the epoch cancellation moves on, and how
/// many lanes wait for a unit.
struct State {
    held: usize,
    epoch: u64,
    waiting: usize,
    lends: usize,
}

/// Extra queue capacity held while the loop waits for a routed answer.
pub(crate) struct Lend(Arc<Credits>);

impl Drop for Lend {
    fn drop(&mut self) {
        support::lock(&self.0.state).lends -= 1;
        self.0.changed.notify_all();
    }
}

impl Credits {
    /// Counts up to `cap` held units.
    pub(crate) fn new(cap: usize) -> Arc<Credits> {
        Arc::new(Self {
            cap,
            state: Mutex::new(State {
                held: 0,
                epoch: 0,
                waiting: 0,
                lends: 0,
            }),
            changed: Condvar::new(),
        })
    }

    /// The current epoch: a lane takes at its own, and a moved epoch
    /// cancels it.
    fn epoch(&self) -> u64 {
        support::lock(&self.state).epoch
    }

    /// Holds a unit for the line about to be sent, waiting while `cap`
    /// units are held. `false` without taking when the epoch moved on,
    /// even when room exists: a cancelled reader drops its pending
    /// line. Held never passes `cap`.
    fn take(&self, epoch: u64) -> bool {
        let mut state = support::lock(&self.state);
        // Counted under the same lock hold as the predicate check, so
        // a test that sees a waiter knows it parked: with the cap
        // filled first, the waiter can only be in the wait below.
        state.waiting += 1;
        while state.held >= self.cap && state.epoch == epoch && state.lends == 0 {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(|poison| poison.into_inner());
        }
        state.waiting -= 1;
        if state.epoch != epoch || state.held >= self.cap.saturating_add(QUEUE) {
            return false;
        }
        state.held += 1;
        true
    }

    /// Lends one extra bounded queue and wakes a capacity-blocked reader.
    pub(crate) fn lend(self: &Arc<Self>) -> Lend {
        support::lock(&self.state).lends += 1;
        self.changed.notify_all();
        Lend(Arc::clone(self))
    }

    /// How many lanes wait for a unit. Tests fill the cap first on
    /// their own thread, so a waiter seen here has parked at the
    /// full-capacity branch.
    #[cfg(test)]
    pub(crate) fn waiters(&self) -> usize {
        support::lock(&self.state).waiting
    }

    /// Gives back the unit `input` held, if it held one: parsed hub
    /// lines only, saturating at zero. Wakes one waiter.
    pub(crate) fn received(&self, input: &Input) {
        if !matches!(input, Input::Hub(_)) {
            return;
        }
        let mut state = support::lock(&self.state);
        state.held = state.held.saturating_sub(1);
        drop(state);
        self.changed.notify_one();
    }

    /// Cancels every lane taken before this call: their waits return
    /// `false`. Lanes taken after it are not cancelled.
    pub(crate) fn cancel(&self) {
        support::lock(&self.state).epoch += 1;
        self.changed.notify_all();
    }
}

/// What one hub reader sends on: the loop's sender with the connection's
/// credits, at the epoch the lane captured.
pub(crate) struct Feed {
    tx: Sender<Input>,
    credits: Arc<Credits>,
    epoch: u64,
    router: Option<Arc<crate::remote::Router>>,
}

impl Feed {
    /// Sends on `tx`, taking at `credits`.
    pub(crate) fn new(tx: Sender<Input>, credits: Arc<Credits>) -> Feed {
        let epoch = credits.epoch();
        Self {
            tx,
            credits,
            epoch,
            router: None,
        }
    }

    /// The lane this reader sends on: the same sender and credits at the
    /// current epoch, captured before `Retry` tracks the stream, so a
    /// reader `Retry` stops is always a cancelled reader.
    pub(crate) fn lane(&self) -> Feed {
        let mut lane = Self::new(self.tx.clone(), Arc::clone(&self.credits));
        lane.router = self.router.clone();
        lane
    }

    /// Takes registered answers off this connection before queue accounting.
    pub(crate) fn routed(mut self, router: Arc<crate::remote::Router>) -> Self {
        router.set_feed(self.tx.clone(), Arc::clone(&self.credits));
        self.router = Some(router);
        self
    }

    /// Sends `input` without taking a unit: everything but parsed lines.
    /// The error is the channel's own, as `Sender::send` returns it.
    #[allow(
        clippy::result_large_err,
        reason = "the error is the channel's own send error"
    )]
    pub(crate) fn send(&self, input: Input) -> Result<(), SendError<Input>> {
        if let Some(router) = &self.router {
            if let Input::Connected(stream, _) = &input {
                if let Ok(stream) = stream.try_clone() {
                    router.attach(stream);
                }
            } else if matches!(input, Input::Disconnected) {
                router.detach();
            }
        }
        self.tx.send(input)
    }
}

#[cfg(test)]
impl From<Sender<Input>> for Feed {
    /// Tests with no connection send unbounded: every take succeeds.
    fn from(tx: Sender<Input>) -> Feed {
        Feed::new(tx, Credits::new(usize::MAX))
    }
}

/// Reads the hub stream, sending each line to the loop, then
/// [`Input::Disconnected`] once the stream ends. A cancelled wait drops
/// the pending line and ends the stream: the reader sends only its
/// [`Input::Disconnected`], never capacity-gated.
pub(crate) fn read_lines(stream: impl std::io::Read, feed: &Feed) {
    let mut read = BufReader::new(stream);
    loop {
        let raw = match support::framing::read_line(&mut read, support::framing::MAX_LINE) {
            Ok(None) | Err(_) => break,
            Ok(Some(raw)) => raw,
        };
        // A non-UTF-8 line ends the stream, as an `Err` does: the hub
        // speaks JSON text, and a relayed byte line is never a hub line.
        let Ok(text) = String::from_utf8(raw) else {
            break;
        };
        // A line that parses takes a unit before it parses the next;
        // a malformed line takes none.
        let Some(line) = parse_line(&text) else {
            continue;
        };
        if feed
            .router
            .as_ref()
            .is_some_and(|router| router.route(&line))
        {
            continue;
        }
        if !feed.credits.take(feed.epoch) {
            break;
        }
        if feed.send(Input::Hub(line)).is_err() {
            return;
        }
    }
    drop(feed.send(Input::Disconnected));
}

#[cfg(test)]
#[path = "link_tests.rs"]
mod tests;
