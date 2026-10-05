//! A monitor's deliveries (`docs/tools.md`, "Background jobs"): the line
//! splitter, the cuts, the rate budget and the flood clock, and the feed
//! the job's drive thread runs them through.

use std::time::{Duration, Instant};

use contract::JobId;
use contract::clock::Clock;
use contract::events::JobLine;
use contract::jobs::{Lines, Stop};

use super::output::{Shared, lock};

/// A line keeps this many characters.
const LINE_LIMIT: usize = 500;

/// A delivery keeps this many characters.
const DELIVERY_LIMIT: usize = 3_000;

/// The budget holds at most this many deliveries.
const BUDGET: u64 = 10;

/// One delivery is added to the budget this often. A run of suppression
/// also ends when this long passes with no delivery dropped.
const REFILL: Duration = Duration::from_millis(2_000);

/// A run of suppression this long stops the monitor.
const FLOOD_AFTER: Duration = Duration::from_millis(30_000);

/// Splits bytes into complete lines. An incomplete last line waits for the
/// next bytes or the flush.
#[derive(Default)]
pub(super) struct Splitter {
    carry: Vec<u8>,
}

impl Splitter {
    /// Every line `bytes` completes, without its newline, decoded lossily.
    pub(super) fn take(&mut self, bytes: &[u8]) -> Vec<String> {
        self.carry.extend_from_slice(bytes);
        let Some(last) = self.carry.iter().rposition(|byte| *byte == b'\n') else {
            return Vec::new();
        };
        let rest = self.carry.split_off(last.saturating_add(1));
        let mut complete = std::mem::replace(&mut self.carry, rest);
        // The last newline ends the last line; it starts no empty one.
        complete.pop();
        complete
            .split(|byte| *byte == b'\n')
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .collect()
    }

    /// The incomplete last line, if any, decoded lossily.
    pub(super) fn flush(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.carry);
        (!rest.is_empty()).then(|| String::from_utf8_lossy(&rest).into_owned())
    }
}

/// `lines` joined into one delivery: each line cut at [`LINE_LIMIT`]
/// characters and the whole at [`DELIVERY_LIMIT`], each with a marker.
pub(super) fn batch(lines: &[String]) -> String {
    let joined = lines
        .iter()
        .map(|line| cut_line(line))
        .collect::<Vec<_>>()
        .join("\n");
    let total = joined.chars().count();
    if total <= DELIVERY_LIMIT {
        return joined;
    }
    let mut cut: String = joined.chars().take(DELIVERY_LIMIT).collect();
    cut.push_str(&format!(
        "\n[cut: {} more characters]",
        total.saturating_sub(DELIVERY_LIMIT)
    ));
    cut
}

fn cut_line(line: &str) -> String {
    if line.chars().count() <= LINE_LIMIT {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(LINE_LIMIT).collect();
    cut.push_str(" [cut]");
    cut
}

/// What the budget did with one delivery.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Admit {
    /// Send it, carrying the deliveries suppressed since the last one sent.
    Send(Option<u64>),
    /// Drop it. `flood` is set once the current run of suppression has
    /// lasted [`FLOOD_AFTER`].
    Drop {
        /// The monitor should stop.
        flood: bool,
    },
}

/// The rate budget and the flood clock, on instants the caller reads from
/// the injected clock.
pub(super) struct Budget {
    tokens: u64,
    /// When the last token was added, or when the budget was last seen full.
    refilled_at: Instant,
    /// Dropped since the last delivery sent.
    suppressed: u64,
    /// The first drop of the current run of suppression.
    run_start: Option<Instant>,
    /// The latest drop.
    last_drop: Option<Instant>,
}

impl Budget {
    /// A full budget at `now`.
    pub(super) fn new(now: Instant) -> Self {
        Self {
            tokens: BUDGET,
            refilled_at: now,
            suppressed: 0,
            run_start: None,
            last_drop: None,
        }
    }

    /// Spends a token on one delivery at `now`, or drops it.
    pub(super) fn admit(&mut self, now: Instant) -> Admit {
        if self
            .last_drop
            .is_some_and(|last| now.saturating_duration_since(last) >= REFILL)
        {
            self.run_start = None;
        }
        self.refill(now);
        if self.tokens > 0 {
            self.tokens = self.tokens.saturating_sub(1);
            return Admit::Send(self.take_suppressed());
        }
        self.suppressed = self.suppressed.saturating_add(1);
        self.last_drop = Some(now);
        let start = *self.run_start.get_or_insert(now);
        Admit::Drop {
            flood: now.saturating_duration_since(start) >= FLOOD_AFTER,
        }
    }

    /// The deliveries suppressed since the last one sent, and resets the
    /// count. `None` when there were none.
    pub(super) fn take_suppressed(&mut self) -> Option<u64> {
        let suppressed = std::mem::take(&mut self.suppressed);
        (suppressed > 0).then_some(suppressed)
    }

    /// Adds one token per [`REFILL`] since the last, never past
    /// [`BUDGET`]. A full budget restarts the refill clock, so a token
    /// spent from it comes back one whole interval later.
    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.refilled_at);
        let gained = elapsed.as_millis() / REFILL.as_millis();
        let room = BUDGET.saturating_sub(self.tokens);
        if gained >= u128::from(room) {
            self.tokens = BUDGET;
            self.refilled_at = now;
            return;
        }
        // `gained` is below `room`, which is at most `BUDGET`.
        let gained = u32::try_from(gained).unwrap_or(0);
        self.tokens = self.tokens.saturating_add(u64::from(gained));
        self.refilled_at = self
            .refilled_at
            .checked_add(REFILL.saturating_mul(gained))
            .unwrap_or(now);
    }
}

/// A monitor's deliveries, run by its job's drive thread: each pass takes
/// the lines the reader appended and sends them as one batch under the
/// budget. Only the drive thread holds one, and it finishes the feed before
/// the job's end, so every batch reaches the inbox before the end.
pub(super) struct Feed {
    job_id: JobId,
    lines: Lines,
    /// The monitor's own stop, which a flood sends.
    stop: Stop,
    splitter: Splitter,
    budget: Budget,
    flooded: bool,
}

impl Feed {
    /// A feed whose budget is full at `now`.
    pub(super) fn new(job_id: JobId, lines: Lines, stop: Stop, now: Instant) -> Self {
        Self {
            job_id,
            lines,
            stop,
            splitter: Splitter::default(),
            budget: Budget::new(now),
            flooded: false,
        }
    }

    /// Takes the bytes queued since the last pass and delivers their
    /// complete lines as one batch, or drops it. `running` is false once a
    /// stop began: a flood found then does not mark the monitor flooded.
    pub(super) fn pass(&mut self, shared: &Shared, clock: &dyn Clock, running: bool) {
        let fresh = lock(&shared.inner)
            .lines
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        let lines = self.splitter.take(&fresh);
        if lines.is_empty() {
            return;
        }
        if self.offer(batch(&lines), clock.now()) && running && !self.flooded {
            self.flooded = true;
            (self.stop.0)();
        }
    }

    /// The monitor ended: what is left goes out, an incomplete last line as
    /// a final batch under the budget, then any suppressed count, exempt
    /// from it.
    pub(super) fn finish(&mut self, shared: &Shared, clock: &dyn Clock) {
        self.pass(shared, clock, false);
        if let Some(rest) = self.splitter.flush() {
            self.offer(batch(&[rest]), clock.now());
        }
        if let Some(suppressed) = self.budget.take_suppressed() {
            self.send(String::new(), Some(suppressed));
        }
    }

    /// Whether a flood stopped the monitor.
    pub(super) fn flooded(&self) -> bool {
        self.flooded
    }

    /// Sends `lines` or drops it; true when the drop found a flood.
    fn offer(&mut self, lines: String, now: Instant) -> bool {
        match self.budget.admit(now) {
            Admit::Send(suppressed) => {
                self.send(lines, suppressed);
                false
            }
            Admit::Drop { flood } => flood,
        }
    }

    fn send(&self, lines: String, suppressed: Option<u64>) {
        (self.lines.0)(JobLine {
            job_id: self.job_id.clone(),
            lines,
            suppressed,
        });
    }
}

#[cfg(test)]
#[path = "monitor_tests.rs"]
mod tests;
