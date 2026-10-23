//! A command's output: the reader thread, the buffer a foreground call
//! streams from, and the file a job's output goes to
//! (`docs/tools.md`, "Result and output", "Background jobs").

use std::fs::File;
use std::io::{Read, Write};
use std::process::ExitStatus;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use contract::JobId;
use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{Event, JobDelta, Progress};

use super::spill::{Config as SpillConfig, SPILL_AT, Spill};

/// A job's output file stops growing past this: 5 GB, decimal
/// (`docs/tools.md`, "Background jobs").
pub(super) const OUTPUT_CAP: u64 = 5_000_000_000;

/// The most delta bytes a holder keeps: 64 KiB (`docs/events.md`,
/// `job_delta`). Past it the oldest bytes are dropped, so only the job's
/// output file holds the whole output.
const HELD_MAX: usize = 65_536;

/// Bytes written to the file that no `job_delta` has carried yet. Past
/// `HELD_MAX` the oldest bytes are dropped and the newest kept, starting
/// past at most 3 leading continuation bytes (`10xxxxxx`) after the cut;
/// further continuation bytes are kept, decoding to U+FFFD as they would
/// have uncut. No marker is inserted.
#[derive(Default)]
pub(super) struct Pending {
    bytes: Vec<u8>,
    /// The queue dropped bytes since the last take: its taker discards any
    /// incomplete sequence it carried from that take.
    dropped: bool,
}

impl Pending {
    /// Queues `bytes`, dropping the oldest past `HELD_MAX`.
    pub(super) fn push(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
        let cut = self.bytes.len().saturating_sub(HELD_MAX);
        let skip = self
            .bytes
            .get(cut..)
            .unwrap_or_default()
            .iter()
            .take(3)
            .take_while(|b| **b & 0xC0 == 0x80)
            .count();
        if cut > 0 {
            self.bytes.drain(..cut.saturating_add(skip));
            self.dropped = true;
        }
    }

    /// Takes the queued bytes and whether bytes were dropped since the last
    /// take, clearing both.
    pub(super) fn take(&mut self) -> (Vec<u8>, bool) {
        (
            std::mem::take(&mut self.bytes),
            std::mem::take(&mut self.dropped),
        )
    }
}

pub(super) struct Inner {
    pub(super) reaped: bool,
    pub(super) status: Option<ExitStatus>,
    pub(super) eof: bool,
    /// Foreground bytes. Cleared when a move hands the command to its file,
    /// or when past 1 MiB hands them to the spill. Never holds more than
    /// 1 MiB while a spill is armed.
    pub(super) output: Vec<u8>,
    /// The spill past 1 MiB goes to, once it starts. `None` for a shell
    /// with no artifacts directory, or a move on the first pass.
    pub(super) spill: Option<Spill>,
    /// Where a spill would go, armed for a foreground call. `Moved::resume`
    /// arms it after a failed job open.
    pub(super) spill_config: Option<SpillConfig>,
    /// Foreground bytes no `tool_call_delta` has carried yet. Past
    /// `HELD_MAX` the oldest bytes are dropped and the newest kept, as the
    /// job's queue is (`docs/events.md`, `tool_call_delta`).
    pub(super) stream: Pending,
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
    pub(super) pending: Pending,
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
/// (`docs/tools.md`, "Background jobs"). With no file after the move, the
/// bytes are dropped, so nothing is held past it.
#[derive(Default)]
pub(super) struct Errors {
    /// Bytes read before the move.
    held: Vec<u8>,
    /// The monitor moved: bytes go to `file`, or are dropped without one.
    moved: bool,
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
    /// `cap` are dropped, and every byte is dropped when `file` is `None`.
    pub(super) fn attach(&mut self, file: Option<File>, cap: u64) {
        self.moved = true;
        self.file = file;
        self.cap = cap;
        let held = std::mem::take(&mut self.held);
        self.sink(&held);
    }

    fn sink(&mut self, bytes: &[u8]) {
        if !self.moved {
            self.held.extend_from_slice(bytes);
            return;
        }
        let Some(file) = self.file.as_mut() else {
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
            spill: None,
            spill_config: None,
            stream: Pending::default(),
            file: None,
            discard: false,
            seq: 0,
            written: 0,
            cap: OUTPUT_CAP,
            cap_fired: false,
            pending: Pending::default(),
            input: None,
            lines: None,
            errors: None,
        }
    }
}

impl Inner {
    /// Hands the command to `file`: a spill in progress goes to the job's
    /// output file at `path`, and the foreground bytes are copied whole.
    /// A copy past the cap is kept whole and fires the cap at once. The
    /// string names any raw file left behind, for the receipt.
    pub(super) fn attach(
        &mut self,
        mut file: File,
        path: &std::path::Path,
        cap: u64,
    ) -> Option<String> {
        if let Some(spill) = self.spill.take() {
            let (sink, line) = spill.into_job(path, file);
            self.cap = cap;
            self.written = sink.metadata().map(|file| file.len()).unwrap_or(0);
            self.cap_fired = self.written > cap;
            self.file = Some(sink);
            return line;
        }
        let bytes = std::mem::take(&mut self.output);
        write_or_drop(&mut file, &bytes);
        if let Some(lines) = self.lines.as_mut() {
            lines.extend_from_slice(&bytes);
        }
        self.cap = cap;
        self.written = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        self.cap_fired = self.written > cap;
        self.file = Some(file);
        None
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
        self.pending.push(kept);
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

pub(super) fn read_output(read: impl Read, shared: &Shared) {
    read_into(read, shared, |inner, bytes| {
        if inner.file.is_some() {
            inner.sink(bytes);
        } else {
            check_start(inner, bytes.len());
            if let Some(spill) = inner.spill.as_mut() {
                spill.push(bytes);
            } else {
                inner.output.extend_from_slice(bytes);
            }
            // Every chunk is queued under the same lock it is written
            // under, so every byte a delta carries was offered first.
            inner.stream.push(bytes);
        }
    });
    note_eof(shared);
}

/// Starts the armed spill once the buffer with `extra` more bytes would
/// cross past 1 MiB: the check runs before the chunk is appended, and the
/// spill takes the buffer with `mem::take`, freeing it. At most 1 MiB
/// never spills.
pub(super) fn check_start(inner: &mut Inner, extra: usize) {
    if inner.spill.is_some() || inner.spill_config.is_none() {
        return;
    }
    if inner.output.len().saturating_add(extra) <= SPILL_AT {
        return;
    }
    if let Some(config) = inner.spill_config.clone() {
        let buffer = std::mem::take(&mut inner.output);
        inner.spill = Some(Spill::start(config, buffer));
    }
}

/// Reads a monitor's standard error into [`Errors`], then marks it closed.
pub(super) fn read_errors(read: impl Read, shared: &Shared) {
    read_into(read, shared, |inner, bytes| {
        if let Some(errors) = inner.errors.as_mut() {
            errors.sink(bytes);
        }
    });
    let mut inner = lock(&shared.inner);
    if let Some(errors) = inner.errors.as_mut() {
        errors.eof = true;
    }
    // The drive loop waits for both streams to close: it wakes on this.
    bump(&mut inner);
    drop(inner);
    shared.cv.notify_all();
}

/// Reads `read` until it closes or fails, handing each chunk to `sink`
/// under the lock and waking the drive loop. Past the drain bound the bytes
/// are dropped and the read continues, so the program never blocks on the
/// pipe.
fn read_into(mut read: impl Read, shared: &Shared, sink: fn(&mut Inner, &[u8])) {
    let mut buf = [0_u8; 8192];
    loop {
        match read.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                let bytes = buf.get(..n).unwrap_or(&[]);
                let mut inner = lock(&shared.inner);
                if !inner.discard {
                    sink(&mut inner, bytes);
                }
                // The drive loop streams every chunk: it wakes on this.
                bump(&mut inner);
                drop(inner);
                shared.cv.notify_all();
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
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

/// Emits the queued output, holding back an incomplete UTF-8 tail for the
/// next pass in `carry`. When the queue dropped bytes, the carried tail is
/// discarded first, since its start is gone. Below the cap the delta texts
/// concatenate to the lossy whole output.
pub(super) fn stream_output(shared: &Shared, emit: &dyn Emit, carry: &mut Vec<u8>) {
    let (fresh, dropped) = lock(&shared.inner).stream.take();
    if dropped {
        carry.clear();
    }
    carry.extend_from_slice(&fresh);
    let complete = complete_prefix(carry);
    if let Some(prefix) = carry.get(..complete) {
        emit_delta(emit, String::from_utf8_lossy(prefix).into_owned());
    }
    carry.drain(..complete);
}

/// Emits what the queue still holds, lossily: an incomplete tail goes out
/// as U+FFFD, so the concatenated delta texts equal the lossy whole output
/// below the cap.
pub(super) fn stream_rest(shared: &Shared, emit: &dyn Emit, carry: &mut Vec<u8>) {
    let (fresh, dropped) = lock(&shared.inner).stream.take();
    if dropped {
        carry.clear();
    }
    carry.extend_from_slice(&fresh);
    emit_delta(emit, String::from_utf8_lossy(carry).into_owned());
    carry.clear();
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
    /// The encoded length of this stream's event with an empty `text`:
    /// the envelope every delta's pace starts from, serialised once.
    envelope: u64,
    /// An incomplete UTF-8 sequence waiting for the next bytes.
    carry: Vec<u8>,
    /// Decoded output no delta has carried yet: at most its newest
    /// `HELD_MAX` bytes.
    held: String,
    /// When the held text may go out. `None` is idle.
    next_due: Option<Instant>,
}

impl JobStream {
    pub(super) fn new(job_id: JobId, emit: Arc<dyn Emit>) -> Self {
        let empty = Event::JobDelta(JobDelta {
            job_id: job_id.clone(),
            progress: Progress {
                text: Some(String::new()),
                details: None,
            },
        });
        let envelope = serde_json::to_vec(&empty)
            .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        Self {
            job_id,
            emit,
            envelope,
            carry: Vec::new(),
            held: String::new(),
            next_due: None,
        }
    }

    /// Takes the bytes the reader queued, and writes the held text when it
    /// is due at `clock`'s now. When the take reports dropped bytes, any
    /// incomplete sequence carried from the last take is discarded first,
    /// since its start is gone.
    pub(super) fn pass(&mut self, shared: &Shared, clock: &dyn Clock) {
        let (fresh, dropped) = lock(&shared.inner).pending.take();
        if dropped {
            self.carry.clear();
        }
        let mut bytes = std::mem::take(&mut self.carry);
        bytes.extend_from_slice(&fresh);
        let complete = complete_prefix(&bytes);
        self.carry = bytes.split_off(complete);
        self.held.push_str(&String::from_utf8_lossy(&bytes));
        cap_held(&mut self.held);
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
    /// When the take reports dropped bytes, any incomplete sequence carried
    /// from the last take is discarded first, since its start is gone.
    pub(super) fn flush(&mut self, shared: &Shared) {
        let (fresh, dropped) = lock(&shared.inner).pending.take();
        if dropped {
            self.carry.clear();
        }
        self.carry.extend_from_slice(&fresh);
        let rest = std::mem::take(&mut self.carry);
        self.held.push_str(&String::from_utf8_lossy(&rest));
        cap_held(&mut self.held);
        if !self.held.is_empty() {
            self.send();
        }
    }

    /// Emits the held text and returns the delta's encoded size, counted
    /// without serialising: the envelope plus the text's escaped length.
    fn send(&mut self) -> u64 {
        let text = std::mem::take(&mut self.held);
        let bytes = self.envelope.saturating_add(escaped_len(&text));
        let event = Event::JobDelta(JobDelta {
            job_id: self.job_id.clone(),
            progress: Progress {
                text: Some(text),
                details: None,
            },
        });
        self.emit.emit(&event);
        bytes
    }
}

/// The length `text` takes inside its JSON string, counted in one pass
/// with serde_json's default escaping: `"` and `\` cost 2, the five
/// shorthand controls cost 2, any other byte below 0x20 costs 6 as
/// `\u00XX`, and every other byte costs 1. The quotes themselves belong
/// to the envelope, so a test compares against the string length minus 2.
fn escaped_len(text: &str) -> u64 {
    let mut len: u64 = 0;
    for byte in text.bytes() {
        len = len.saturating_add(match byte {
            b'"' | b'\\' => 2,
            0x08 | 0x0c | b'\n' | b'\r' | b'\t' => 2,
            0x00..=0x1f => 6,
            _ => 1,
        });
    }
    len
}

/// Drops the oldest bytes past `HELD_MAX`, keeping the newest starting at
/// a character boundary, so the text stays valid UTF-8.
fn cap_held(text: &mut String) {
    let cut = text.len().saturating_sub(HELD_MAX);
    text.drain(..text.ceil_char_boundary(cut));
}

/// Drops a partial character at the start of a cut, so a tail does not
/// begin with U+FFFD from a byte that belonged to the previous character.
pub(super) fn align_char_boundary(slice: &[u8]) -> &[u8] {
    let start = slice
        .iter()
        .position(|byte| byte & 0b1100_0000 != 0b1000_0000)
        .unwrap_or(slice.len());
    slice.get(start..).unwrap_or_default()
}

pub(super) use support::lock;

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
