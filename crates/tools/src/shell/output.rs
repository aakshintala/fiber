//! A command's output: the reader thread, the buffer a foreground call
//! streams from, and the file a job's output goes to
//! (`docs/tools.md`, "Result and output", "Background jobs").

use std::fs::File;
use std::io::{Read, Write};
use std::process::ExitStatus;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use contract::JobId;
use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{Event, JobDelta, Progress};

/// A job's output file stops growing past this: 5 GB, decimal
/// (`docs/tools.md`, "Background jobs").
pub(super) const OUTPUT_CAP: u64 = 5_000_000_000;

pub(super) struct Inner {
    pub(super) reaped: bool,
    pub(super) status: Option<ExitStatus>,
    pub(super) eof: bool,
    /// Foreground bytes. Cleared when a move hands the command to its file.
    pub(super) output: Vec<u8>,
    /// The job's output file, after a move.
    pub(super) file: Option<File>,
    pub(super) discard: bool,
    pub(super) seq: u64,
    /// Bytes in the job's file: the copy at the move, then what the reader
    /// wrote.
    pub(super) written: u64,
    /// The file stops at this many bytes. Tests set a small one.
    pub(super) cap: u64,
    /// The output passed `cap`: no byte is written after this is set.
    pub(super) cap_fired: bool,
    /// Bytes written to the file that no `job_delta` has carried yet.
    pub(super) pending: Vec<u8>,
    /// Types into the command's terminal; `Some` only for a `tty` command,
    /// until the job takes it.
    pub(super) input: Option<contract::jobs::Input>,
    /// A monitor's standard output not yet taken for its lines: what the
    /// file kept. `None` for any other command.
    pub(super) lines: Option<Vec<u8>>,
    /// A monitor's standard error. `None` for any other command.
    pub(super) errors: Option<Errors>,
}

/// A monitor's standard error: held until the move, then written to its own
/// file, which stops growing past the cap without stopping the job
/// (`docs/tools.md`, "Background jobs").
#[derive(Default)]
pub(super) struct Errors {
    /// Bytes read before the move.
    held: Vec<u8>,
    /// The stderr file, after the move.
    file: Option<File>,
    /// Bytes in the file.
    written: u64,
    /// The file stops at this many bytes.
    cap: u64,
    /// The pipe closed.
    pub(super) eof: bool,
}

impl Errors {
    /// Writes what was held to `file` and makes it the sink. Bytes past
    /// `cap` are dropped.
    pub(super) fn attach(&mut self, file: File, cap: u64) {
        self.file = Some(file);
        self.cap = cap;
        let held = std::mem::take(&mut self.held);
        self.sink(&held);
    }

    fn sink(&mut self, bytes: &[u8]) {
        let Some(file) = self.file.as_mut() else {
            self.held.extend_from_slice(bytes);
            return;
        };
        let room = usize::try_from(self.cap.saturating_sub(self.written)).unwrap_or(usize::MAX);
        let kept = bytes.get(..room.min(bytes.len())).unwrap_or(&[]);
        write_or_drop(file, kept);
        self.written = self
            .written
            .saturating_add(u64::try_from(kept.len()).unwrap_or(u64::MAX));
    }
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            reaped: false,
            status: None,
            eof: false,
            output: Vec::new(),
            file: None,
            discard: false,
            seq: 0,
            written: 0,
            cap: OUTPUT_CAP,
            cap_fired: false,
            pending: Vec::new(),
            input: None,
            lines: None,
            errors: None,
        }
    }
}

impl Inner {
    /// Copies the foreground bytes into `file` whole and makes it the sink.
    /// A copy past the cap is kept whole and fires the cap at once.
    pub(super) fn attach(&mut self, mut file: File, cap: u64) {
        let bytes = std::mem::take(&mut self.output);
        write_or_drop(&mut file, &bytes);
        if let Some(lines) = self.lines.as_mut() {
            lines.extend_from_slice(&bytes);
        }
        self.cap = cap;
        self.written = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        self.cap_fired = self.written > cap;
        self.file = Some(file);
    }

    /// Writes what fits under the cap to the file and queues it for the
    /// next `job_delta`. Bytes past the cap are dropped and fire the cap.
    fn sink(&mut self, bytes: &[u8]) {
        if self.cap_fired {
            return;
        }
        let room = usize::try_from(self.cap.saturating_sub(self.written)).unwrap_or(usize::MAX);
        let kept = bytes.get(..room.min(bytes.len())).unwrap_or(&[]);
        if let Some(file) = self.file.as_mut() {
            write_or_drop(file, kept);
        }
        self.written = self
            .written
            .saturating_add(u64::try_from(kept.len()).unwrap_or(u64::MAX));
        self.pending.extend_from_slice(kept);
        if let Some(lines) = self.lines.as_mut() {
            lines.extend_from_slice(kept);
        }
        self.cap_fired = kept.len() < bytes.len();
    }

    /// Both streams closed: standard output, and a monitor's standard
    /// error when it has one.
    pub(super) fn all_eof(&self) -> bool {
        self.eof && self.errors.as_ref().is_none_or(|errors| errors.eof)
    }
}

#[derive(Default)]
pub(super) struct Shared {
    pub(super) inner: Mutex<Inner>,
    pub(super) cv: Condvar,
}

impl Wake for Shared {
    fn wake(&self) {
        // The sequence moves under the same lock as the wait, so a cancel
        // or a clock advance that lands before `cv.wait` is still visible
        // when the waiter checks.
        let mut guard = lock(&self.inner);
        bump(&mut guard);
        self.cv.notify_all();
    }
}

pub(super) fn read_output(mut read: impl Read, shared: &Shared) {
    let mut buf = [0_u8; 8192];
    loop {
        match read.read(&mut buf) {
            Ok(0) => {
                note_eof(shared);
                return;
            }
            Ok(n) => {
                let bytes = buf.get(..n).unwrap_or(&[]);
                let mut inner = lock(&shared.inner);
                // Past the drain bound the bytes are dropped and the read
                // continues, so the program never blocks on the pipe.
                if !inner.discard {
                    if inner.file.is_some() {
                        inner.sink(bytes);
                    } else {
                        inner.output.extend_from_slice(bytes);
                    }
                }
                // The drive loop streams every chunk: it wakes on this.
                bump(&mut inner);
                drop(inner);
                shared.cv.notify_all();
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                note_eof(shared);
                return;
            }
        }
    }
}

/// Reads a monitor's standard error into [`Errors`]. Past the drain bound
/// the bytes are dropped and the read continues, as for standard output.
pub(super) fn read_errors(mut read: impl Read, shared: &Shared) {
    let mut buf = [0_u8; 8192];
    loop {
        match read.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let bytes = buf.get(..n).unwrap_or(&[]);
                let mut inner = lock(&shared.inner);
                if !inner.discard
                    && let Some(errors) = inner.errors.as_mut()
                {
                    errors.sink(bytes);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let mut inner = lock(&shared.inner);
    if let Some(errors) = inner.errors.as_mut() {
        errors.eof = true;
    }
    // The drive loop waits for both streams to close: it wakes on this.
    bump(&mut inner);
    drop(inner);
    shared.cv.notify_all();
}

pub(super) fn note_eof(shared: &Shared) {
    let mut inner = lock(&shared.inner);
    if inner.eof {
        return;
    }
    inner.eof = true;
    bump(&mut inner);
    shared.cv.notify_all();
}

pub(super) fn bump(inner: &mut Inner) {
    inner.seq = inner.seq.wrapping_add(1);
}

/// The longest prefix of `chunk` that ends on a complete UTF-8 sequence:
/// an incomplete sequence at the end waits for the next chunk, while an
/// invalid one is consumed, decoding as U+FFFD as `String::from_utf8_lossy`
/// does. The held tail is the last chunk's invalid bytes when they are an
/// incomplete sequence, else nothing; splits fall where the decoder is
/// clean, so the emitted texts concatenate to the lossy whole.
pub(super) fn complete_prefix(chunk: &[u8]) -> usize {
    let tail = match chunk.utf8_chunks().last() {
        Some(last) => match str::from_utf8(last.invalid()) {
            Err(err) if err.error_len().is_none() => last.invalid().len(),
            Ok(_) | Err(_) => 0,
        },
        None => 0,
    };
    chunk.len() - tail
}

/// Emits `text` as one text-only `tool_call_delta`. Empty texts carry
/// nothing and are not written.
fn emit_delta(emit: &dyn Emit, text: String) {
    if !text.is_empty() {
        emit.emit(&Event::ToolCallDelta(Progress {
            text: Some(text),
            details: None,
        }));
    }
}

/// Emits the output past `streamed`, holding back an incomplete UTF-8 tail
/// for the next chunk, and returns the new streamed offset. Output only
/// grows, so bytes read between the copy and the return stay past it.
pub(super) fn stream_output(shared: &Shared, emit: &dyn Emit, streamed: usize) -> usize {
    let chunk = {
        lock(&shared.inner)
            .output
            .get(streamed..)
            .unwrap_or_default()
            .to_vec()
    };
    let complete = complete_prefix(&chunk);
    if let Some(prefix) = chunk.get(..complete) {
        emit_delta(emit, String::from_utf8_lossy(prefix).into_owned());
    }
    streamed.saturating_add(complete)
}

/// Emits `output` past `streamed`, lossily: an incomplete tail goes out as
/// U+FFFD, so the concatenated delta texts equal the lossy whole output.
pub(super) fn stream_tail(output: &[u8], emit: &dyn Emit, streamed: usize) {
    let rest = output.get(streamed..).unwrap_or_default();
    emit_delta(emit, String::from_utf8_lossy(rest).into_owned());
}

pub(super) fn write_or_drop(file: &mut File, bytes: &[u8]) {
    // debt: output lost to a failed write is not reported to the model, until docs/errors.md has a code for it
    if let Err(err) = file.write_all(bytes) {
        let _lost = err;
    }
}

/// At most one `job_delta` every 100 ms (`docs/tools.md`, "Progress").
const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// The byte rate the interval grows past the minimum with: 100 KiB/s.
const BYTES_PER_SECOND: u64 = 102_400;

/// A running job's `job_delta` lines (`docs/events.md`, `job_delta`), paced
/// as `tool_call_delta` is (`docs/tools.md`, "Progress"): the first change
/// after idle goes out at once, held changes collapse into one per
/// interval, and the end flushes what is held. Only the job's drive thread
/// holds one, so nothing emits after the job's end is reported.
// debt: the pacing duplicates crates/loop/src/progress.rs, until a third paced emitter moves it to a shared place
pub(super) struct JobStream {
    job_id: JobId,
    emit: Arc<dyn Emit>,
    /// An incomplete UTF-8 sequence waiting for the next bytes.
    carry: Vec<u8>,
    /// Decoded output no delta has carried yet.
    held: String,
    /// When the held text may go out. `None` is idle.
    next_due: Option<Instant>,
}

impl JobStream {
    pub(super) fn new(job_id: JobId, emit: Arc<dyn Emit>) -> Self {
        Self {
            job_id,
            emit,
            carry: Vec::new(),
            held: String::new(),
            next_due: None,
        }
    }

    /// Takes the bytes the reader queued, and writes the held text when it
    /// is due at `clock`'s now.
    pub(super) fn pass(&mut self, shared: &Shared, clock: &dyn Clock) {
        let fresh = std::mem::take(&mut lock(&shared.inner).pending);
        let mut bytes = std::mem::take(&mut self.carry);
        bytes.extend_from_slice(&fresh);
        let complete = complete_prefix(&bytes);
        self.carry = bytes.split_off(complete);
        self.held.push_str(&String::from_utf8_lossy(&bytes));
        let now = clock.now();
        if !self.held.is_empty() && self.next_due.is_none_or(|due| now >= due) {
            let bytes = self.send();
            let paced =
                Duration::from_nanos(bytes.saturating_mul(1_000_000_000) / BYTES_PER_SECOND);
            self.next_due = now.checked_add(paced.max(MIN_INTERVAL)).or(Some(now));
        }
    }

    /// When the held text is next due, so the drive loop wakes then.
    pub(super) fn deadline(&self) -> Option<Instant> {
        if self.held.is_empty() {
            None
        } else {
            self.next_due
        }
    }

    /// Writes everything left, whatever the interval. An incomplete tail
    /// goes out as U+FFFD, so the texts concatenate to the lossy whole.
    pub(super) fn flush(&mut self, shared: &Shared) {
        let fresh = std::mem::take(&mut lock(&shared.inner).pending);
        self.carry.extend_from_slice(&fresh);
        let rest = std::mem::take(&mut self.carry);
        self.held.push_str(&String::from_utf8_lossy(&rest));
        if !self.held.is_empty() {
            self.send();
        }
    }

    /// Emits the held text and returns the delta's encoded size.
    fn send(&mut self) -> u64 {
        let event = Event::JobDelta(JobDelta {
            job_id: self.job_id.clone(),
            progress: Progress {
                text: Some(std::mem::take(&mut self.held)),
                details: None,
            },
        });
        self.emit.emit(&event);
        serde_json::to_vec(&event).map_or(0, |bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
    }
}

pub(super) fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
