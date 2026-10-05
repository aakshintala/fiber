//! A command's output: the reader thread, the buffer a foreground call
//! streams from, and the file a job's output goes to
//! (`docs/tools.md`, "Result and output", "Background jobs").

use std::fs::File;
use std::io::{Read, Write};
use std::process::ExitStatus;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use contract::clock::Wake;
use contract::emit::Emit;
use contract::events::{Event, Progress};

#[derive(Default)]
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
                    if let Some(file) = inner.file.as_mut() {
                        write_or_drop(file, bytes);
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

pub(super) fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}
