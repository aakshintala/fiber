//! Jobs a keeper handed over, running again in the session's registry
//! (`docs/tools.md`, "The keeper": a resume takes the log back). The keeper
//! keeps writing each job's files until the session says it is ready; from
//! then on a job's output arrives as frames, and the runner here appends it
//! after the keeper's bytes and reports the ending through the job's `End`,
//! as any job's.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use contract::JobId;
use contract::emit::Emit;
use contract::events::{Event, EventKind, JobDelta, JobLine, JobStarted, Progress};
use contract::jobs::{End, Input, Lines, Stop};
use contract::keeper::{HostEvent, Stream};

use crate::keeper::Adopted;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What the session's append handles are, shared with whoever may have to
/// close them while the runner is blocked waiting for its next event.
struct Sinks {
    /// `None` once the handover was abandoned.
    output: Option<File>,
    errors: Option<File>,
}

struct Runner {
    job_id: JobId,
    events: Receiver<HostEvent>,
    sinks: Arc<Mutex<Sinks>>,
    /// Taken by the runner's one report, or dropped with an abandoned
    /// handover: reporting is the registry's to make, never a teardown's.
    end: Option<End>,
    emit: Arc<dyn Emit>,
    lines: Option<Lines>,
}

impl Drop for Runner {
    fn drop(&mut self) {
        // A runner that never reported was torn down before it ran, or
        // after the handover was abandoned. Dropping its `End` would record
        // the job failed `indeterminate`, a completion under a writer that
        // may no longer be the session's, for a job the keeper still runs.
        if let Some(end) = self.end.take() {
            std::mem::forget(end);
        }
    }
}

/// Handed-over jobs recorded in the registry, their runners not started.
/// Dropped, they close every append handle and report nothing.
pub(crate) struct Restored {
    ids: Vec<JobId>,
    runners: Vec<Runner>,
}

impl Restored {
    /// The jobs now recorded, to take out of the resume's orphan candidates.
    pub(crate) fn job_ids(&self) -> &[JobId] {
        &self.ids
    }

    /// Starts every runner. Call it once the registry's inbox is wired: an
    /// ending reported before that would not reach the loop.
    pub(crate) fn activate(mut self) -> Active {
        let runners = std::mem::take(&mut self.runners);
        let sinks = runners
            .iter()
            .map(|runner| Arc::clone(&runner.sinks))
            .collect();
        for runner in runners {
            thread::spawn(move || runner.run());
        }
        Active { sinks, armed: true }
    }
}

/// Running runners. Dropped before [`Active::keep`], it closes every append
/// handle at once, so the keeper can take the files back.
pub(crate) struct Active {
    sinks: Vec<Arc<Mutex<Sinks>>>,
    armed: bool,
}

impl Active {
    /// The handover is complete: the runners own their files for good.
    pub(crate) fn keep(mut self) {
        self.armed = false;
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for sinks in &self.sinks {
            let mut sinks = lock(sinks);
            sinks.output = None;
            sinks.errors = None;
        }
    }
}

fn invalid(reason: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

/// The jobs the log started and never ended, by id. The lock is the
/// session's, so no writer adds a line while this reads.
fn outstanding(dir: &Path) -> io::Result<HashMap<JobId, JobStarted>> {
    let mut open = HashMap::new();
    let lines = log::lines(dir).map_err(io::Error::other)?;
    for line in lines {
        let line = line.map_err(io::Error::other)?;
        if !matches!(line.kind(), EventKind::JobStarted | EventKind::JobCompleted) {
            continue;
        }
        match Event::from_envelope(&line).map_err(io::Error::other)? {
            Some(Event::JobStarted(started)) => {
                open.insert(started.job_id.clone(), started);
            }
            Some(Event::JobCompleted(ended)) => {
                open.remove(&ended.job_id);
            }
            Some(_) | None => {}
        }
    }
    Ok(open)
}

/// The stderr file beside `output`, named as the job's monitor receipt says.
fn errors_beside(output: &Path) -> PathBuf {
    let stem = output
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    output.with_file_name(format!("{stem}.stderr.log"))
}

/// Records each job the keeper handed over, in the registry, under the
/// `job_started` the log already holds. Nothing is opened or recorded until
/// every job's description matches a job the log holds open and names the
/// files that job owns, so a mismatched keeper cannot make the session write
/// anywhere else. A failure after that leaves the registry as it is for the
/// jobs recorded so far; the caller abandons the whole resume.
pub(crate) fn restore(
    registry: &jobs::Registry,
    dir: &log::SessionDir,
    handed: Vec<Adopted>,
) -> io::Result<Restored> {
    let open = outstanding(dir.path())?;
    let mut checked = Vec::with_capacity(handed.len());
    for adopted in handed {
        let Some(started) = open.get(&adopted.job.job_id) else {
            return Err(invalid(format!(
                "the keeper handed over {}, which the log does not hold open",
                adopted.job.job_id.0
            )));
        };
        let output = dir.path().join(&started.output_path);
        if adopted.job.output_path != output {
            return Err(invalid(format!(
                "the keeper names {} for {}, not {}",
                adopted.job.output_path.display(),
                adopted.job.job_id.0,
                output.display()
            )));
        }
        if adopted
            .job
            .errors_path
            .as_ref()
            .is_some_and(|errors| *errors != errors_beside(&output))
        {
            return Err(invalid(format!(
                "the keeper names another stderr file for {}",
                adopted.job.job_id.0
            )));
        }
        checked.push((started.clone(), adopted));
    }
    let mut restored = Restored {
        ids: Vec::new(),
        runners: Vec::new(),
    };
    for (started, adopted) in checked {
        let Adopted { job, events, owner } = adopted;
        // Opened before the registry records anything, so a file that
        // cannot be held leaves the job unrecorded.
        let errors = job
            .errors_path
            .as_ref()
            .map(|path| OpenOptions::new().append(true).open(path))
            .transpose()?;
        let stop = {
            let (owner, id) = (owner.clone(), job.id.clone());
            Stop(Box::new(move || {
                owner.stop(&id);
            }))
        };
        let input = job.tty.then(|| {
            let (owner, id) = (owner.clone(), job.id.clone());
            Input(Box::new(move |bytes, _clock, cancel| {
                if cancel.is_cancelled() {
                    return Ok(0);
                }
                owner
                    .write(&id, &String::from_utf8_lossy(bytes))
                    .map_err(|failure| io::Error::other(failure.to_string()))?;
                Ok(bytes.len())
            }))
        });
        let opened = registry
            .restore(started, stop, input, job.lines)
            .map_err(|failure| io::Error::other(failure.to_string()))?;
        restored.ids.push(job.job_id.clone());
        restored.runners.push(Runner {
            job_id: job.job_id,
            events,
            sinks: Arc::new(Mutex::new(Sinks {
                output: Some(opened.file),
                errors,
            })),
            end: Some(opened.end),
            emit: opened.emit,
            lines: opened.lines,
        });
    }
    Ok(restored)
}

impl Runner {
    /// Appends what the keeper sends after its own bytes, delivers a
    /// monitor's lines, and reports the ending once. Everything that
    /// arrived while the runner was not yet running is waiting in `events`.
    fn run(mut self) {
        let mut feed = self.lines.is_some().then(Feed::default);
        let mut text = Text::default();
        loop {
            let Ok(first) = self.events.recv() else {
                // The keeper's connection ended with no ending: the registry
                // records the job failed `indeterminate`, as for any job
                // whose host is lost, unless the handover was abandoned.
                if lock(&self.sinks).output.is_none() {
                    return;
                }
                drop(self.end.take());
                return;
            };
            // What is already queued goes out together: one delta per burst
            // keeps a flood from becoming one event per frame.
            let mut burst = vec![first];
            while !matches!(burst.last(), Some(HostEvent::Exit(_)))
                && let Ok(more) = self.events.try_recv()
            {
                burst.push(more);
            }
            let mut shown = String::new();
            for event in burst {
                match event {
                    HostEvent::Output { stream, bytes } => {
                        let Some(output) = self.append(stream, &bytes) else {
                            return;
                        };
                        if let Some(feed) = &mut feed {
                            feed.push(&output);
                        }
                        shown.push_str(&text.decode(&output));
                    }
                    HostEvent::Exit(exit) => {
                        // An abandoned handover reports nothing: the keeper
                        // still holds the job and a later resume owns it.
                        if lock(&self.sinks).output.is_none() {
                            return;
                        }
                        self.show(&shown);
                        self.deliver(feed.as_mut(), true);
                        if let Some(end) = self.end.take() {
                            (end.0)(tools::kept_end(self.job_id.clone(), &exit));
                        }
                        return;
                    }
                }
            }
            self.show(&shown);
            self.deliver(feed.as_mut(), false);
        }
    }

    /// Appends `bytes` to the file `stream` belongs to; returns what went to
    /// the output file. `None` when the handover was abandoned. A write that
    /// fails loses those bytes from the file only: the job's ending still
    /// reaches the model.
    fn append(&self, stream: Stream, bytes: &[u8]) -> Option<Vec<u8>> {
        let mut sinks = lock(&self.sinks);
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

    /// Sends the monitor's complete lines, and at the ending the incomplete
    /// last line too, before the job's end.
    fn deliver(&self, feed: Option<&mut Feed>, last: bool) {
        let (Some(feed), Some(lines)) = (feed, &self.lines) else {
            return;
        };
        let batch = feed.take(last);
        if batch.is_empty() {
            return;
        }
        (lines.0)(JobLine {
            job_id: self.job_id.clone(),
            lines: cut(&batch),
            suppressed: None,
            changed_by: None,
        });
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

/// A monitor's lines: the bytes of a line wait here until its newline.
// debt: no rate budget or flood stop, one batch per burst of frames, until the monitor feed in tools can be reused by a restored job
#[derive(Default)]
struct Feed {
    carry: Vec<u8>,
}

impl Feed {
    fn push(&mut self, bytes: &[u8]) {
        self.carry.extend_from_slice(bytes);
    }

    /// The complete lines so far, or at the ending every line left.
    fn take(&mut self, last: bool) -> Vec<String> {
        let end = if last {
            self.carry.len()
        } else {
            match self.carry.iter().rposition(|byte| *byte == b'\n') {
                Some(at) => at.saturating_add(1),
                None => return Vec::new(),
            }
        };
        if end == 0 {
            return Vec::new();
        }
        let rest = self.carry.split_off(end);
        let mut done = std::mem::replace(&mut self.carry, rest);
        // The last newline ends the last line; it starts no empty one.
        if done.last() == Some(&b'\n') {
            done.pop();
        }
        done.split(|byte| *byte == b'\n')
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .collect()
    }
}

/// One delivery: each line cut at 500 characters and the whole at 3,000,
/// each with a marker (`docs/tools.md`, "Background jobs").
fn cut(lines: &[String]) -> String {
    const LINE_LIMIT: usize = 500;
    const DELIVERY_LIMIT: usize = 3_000;
    let joined = lines
        .iter()
        .map(|line| {
            if line.chars().count() <= LINE_LIMIT {
                return line.clone();
            }
            let mut cut: String = line.chars().take(LINE_LIMIT).collect();
            cut.push_str(" [cut]");
            cut
        })
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

#[cfg(test)]
#[path = "keeper_restore_tests.rs"]
mod tests;
