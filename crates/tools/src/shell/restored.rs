//! Jobs a keeper handed over, running again after the session took them
//! back (`docs/tools.md`, "The keeper"). The keeper keeps writing each
//! job's files until the session says it is ready; from then on a job's
//! output arrives as frames, and the runner here appends it after the
//! keeper's bytes and reports the ending through the job's `End`, as any
//! job's.

use std::fs::File;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use contract::JobId;
use contract::clock::Clock;
use contract::emit::Emit;
use contract::events::{Event, JobDelta, Progress};
use contract::jobs::{End, Opened, Stop};
use contract::keeper::{HostEvent, Stream};

use super::hosted::{adopted_end, adopted_flooded_end};
use super::monitor::Feed;

/// What the runner's append handles are, shared with whoever may have to
/// close them while the runner is blocked waiting for its next event.
struct Sinks {
    /// `None` once the handover was abandoned.
    output: Option<File>,
    errors: Option<File>,
}

/// A revocable handle to a runner's append files: closing takes both files
/// synchronously, so a runner blocked waiting for its next event appends
/// nothing after, and the keeper can take the files back. Cloning shares
/// the revocation; closing never waits for a receiver thread.
#[derive(Clone)]
pub struct AdoptedSink(Arc<Mutex<Sinks>>);

impl AdoptedSink {
    /// Closes both append handles before returning.
    pub fn close(&self) {
        let mut sinks = support::lock(&self.0);
        sinks.output = None;
        sinks.errors = None;
    }
}

/// A handed-over job's execution: its appended output, monitor deliveries
/// and completion. Built from the registry's [`Opened`] job, so the job's
/// identity is the recorded one, never a second inconsistent id or path.
pub struct AdoptedRun {
    job_id: JobId,
    /// The job's output file, for the tail of its ending.
    output_path: PathBuf,
    events: Receiver<HostEvent>,
    sinks: Arc<Mutex<Sinks>>,
    /// Taken by the runner's one report, or forgotten with a revoked or
    /// unstarted handover: reporting is the registry's to make, and a
    /// teardown's drop must never record one.
    end: Option<End>,
    emit: Arc<dyn Emit>,
    /// A monitor's deliveries under the rate budget and flood stop.
    feed: Option<Feed>,
    clock: Arc<dyn Clock>,
}

impl AdoptedRun {
    /// Records the handed-over job for running: its files stay the
    /// registry's append handles until revoked, and its monitor's budget
    /// starts now. The monitor's `stop` spends the job's own stop.
    pub fn new(
        opened: Opened,
        events: Receiver<HostEvent>,
        errors: Option<File>,
        stop: Stop,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let feed = opened
            .lines
            .map(|lines| Feed::new(opened.started.job_id.clone(), lines, stop, clock.now()));
        Self {
            job_id: opened.started.job_id.clone(),
            output_path: opened.path.clone(),
            events,
            sinks: Arc::new(Mutex::new(Sinks {
                output: Some(opened.file),
                errors,
            })),
            end: Some(opened.end),
            emit: opened.emit,
            feed,
            clock,
        }
    }

    /// The runner's revocation handle: closing it takes both append files.
    pub fn sink(&self) -> AdoptedSink {
        AdoptedSink(Arc::clone(&self.sinks))
    }

    /// Appends what the keeper sends after its own bytes, delivers a
    /// monitor's lines, and reports the ending once. Everything that
    /// arrived while the runner was not yet running is waiting in `events`.
    /// A live connection lost before an exit returns the uncalled `End`
    /// for the launcher to route; a revoked or unstarted run reports
    /// nothing.
    pub fn run(mut self) -> Option<End> {
        let mut feed = self.feed.take();
        let mut text = Text::default();
        loop {
            let Ok(first) = self.events.recv() else {
                // The keeper's connection ended with no ending: the caller
                // records the job failed `indeterminate`, as for any job
                // whose host is lost, unless the handover was abandoned.
                if support::lock(&self.sinks).output.is_none() {
                    self.forget_end();
                    return None;
                }
                return self.end.take();
            };
            // What is already queued goes out together: one delta per burst
            // keeps a flood from becoming one event per frame, and one
            // delivery per burst spends one token of a monitor's budget.
            let mut burst = vec![first];
            while !matches!(burst.last(), Some(HostEvent::Exit(_)))
                && let Ok(more) = self.events.try_recv()
            {
                burst.push(more);
            }
            let mut shown = String::new();
            let mut fresh = Vec::new();
            for event in burst {
                match event {
                    HostEvent::Output { stream, bytes } => {
                        let Some(output) = self.append(stream, &bytes) else {
                            self.forget_end();
                            return None;
                        };
                        if feed.is_some() {
                            fresh.extend_from_slice(&output);
                        }
                        shown.push_str(&text.decode(&output));
                    }
                    HostEvent::Exit(exit) => {
                        // A revoked handover reports nothing: the keeper
                        // still holds the job and a later resume owns it.
                        if support::lock(&self.sinks).output.is_none() {
                            self.forget_end();
                            return None;
                        }
                        self.show(&shown);
                        let flooded = feed.as_mut().is_some_and(|feed| {
                            let now = self.clock.now();
                            feed.deliver(&fresh, now, false);
                            feed.end(now);
                            feed.flooded()
                        });
                        if let Some(end) = self.end.take() {
                            let ended = if flooded {
                                adopted_flooded_end
                            } else {
                                adopted_end
                            };
                            (end.0)(ended(self.job_id.clone(), &self.output_path, &exit));
                        }
                        return None;
                    }
                }
            }
            self.show(&shown);
            if let Some(feed) = &mut feed {
                feed.deliver(&fresh, self.clock.now(), true);
            }
        }
    }

    /// Appends `bytes` to the file `stream` belongs to; returns what went to
    /// the output file. `None` when the handover was revoked. A write that
    /// fails loses those bytes from the file only: the job's ending still
    /// reaches the model.
    fn append(&self, stream: Stream, bytes: &[u8]) -> Option<Vec<u8>> {
        let mut sinks = support::lock(&self.sinks);
        sinks.output.as_ref()?;
        if stream == Stream::Err
            && let Some(errors) = &mut sinks.errors
        {
            match errors.write_all(bytes) {
                Ok(()) | Err(_) => {}
            }
            return Some(Vec::new());
        }
        if let Some(output) = &mut sinks.output {
            match output.write_all(bytes) {
                Ok(()) | Err(_) => {}
            }
        }
        Some(bytes.to_vec())
    }

    fn show(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.emit.emit(&Event::JobDelta(JobDelta {
            job_id: self.job_id.clone(),
            progress: Progress {
                text: Some(newest(text).to_owned()),
                details: None,
            },
        }));
    }

    /// Drops the uncalled report without recording: a revoked or unstarted
    /// run is the registry's to complete, never a teardown's.
    fn forget_end(&mut self) {
        if let Some(end) = self.end.take() {
            std::mem::forget(end);
        }
    }
}

impl Drop for AdoptedRun {
    fn drop(&mut self) {
        // A runner that never reported was torn down before it ran, or
        // after the handover was revoked. Dropping its `End` would record
        // the job failed `indeterminate`, a completion under a writer that
        // may no longer be the session's, for a job the keeper still runs.
        self.forget_end();
    }
}

/// The newest 64 KiB of a delta, from a character boundary
/// (`docs/events.md`, `job_delta`).
fn newest(text: &str) -> &str {
    const HELD_MAX: usize = 64 << 10;
    let mut from = text.len().saturating_sub(HELD_MAX);
    while !text.is_char_boundary(from) {
        from = from.saturating_add(1);
    }
    text.get(from..).unwrap_or_default()
}

/// Decodes a stream's bytes, holding an incomplete character for the next.
#[derive(Default)]
struct Text {
    carry: Vec<u8>,
}

impl Text {
    fn decode(&mut self, bytes: &[u8]) -> String {
        self.carry.extend_from_slice(bytes);
        let whole = match std::str::from_utf8(&self.carry) {
            Ok(_) => self.carry.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => self.carry.len(),
        };
        let rest = self.carry.split_off(whole);
        let text = String::from_utf8_lossy(&self.carry).into_owned();
        self.carry = rest;
        text
    }
}

#[cfg(test)]
#[path = "restored_tests.rs"]
mod tests;
