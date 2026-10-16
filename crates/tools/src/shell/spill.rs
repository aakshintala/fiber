//! A foreground call's output past 1 MiB: written to the call's artifact
//! file as it arrives, holding only the first and the last end in memory
//! (`docs/tools.md`, "Result and output").

use std::collections::hash_map::RandomState;
use std::fs::{self, File, OpenOptions};
use std::hash::BuildHasher;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::tool::{Bound, SpillFile, Spilled, Unsaved};

/// Output past this many bytes goes to the call's artifact file, from its
/// first byte, whatever the bound (`docs/tools.md`, "Result and output").
pub(super) const SPILL_AT: usize = 1_048_576;

/// Wraps a spill's file in a test's own writer.
pub(crate) type Wrap = Arc<dyn Fn(File) -> Box<dyn Write + Send> + Send + Sync>;

/// Where a spill writes and what it holds.
#[derive(Clone)]
pub(crate) struct Config {
    /// The directory the artifact file is created in.
    pub(crate) dir: PathBuf,
    /// The held ends follow this, not a literal 8 KiB (`docs/tools.md`,
    /// "Bounded results").
    pub(crate) bound: Bound,
    /// A test's writer around the file, if any.
    pub(crate) wrap: Option<Wrap>,
    /// A test's fixed file name, if any; else a random `s_` name.
    pub(crate) name: Option<String>,
}

/// The last end of a spilled output: the newest raw bytes, at most the
/// bound's end count.
#[derive(Default)]
pub(super) struct Ring {
    pub(super) bytes: Vec<u8>,
    cap: usize,
}

impl Ring {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        self.bytes
            .drain(..self.bytes.len().saturating_sub(self.cap));
    }
}

/// A spill in progress: the file the output goes to, the head and the ring
/// it keeps, and why the file was not kept, once it was not.
pub(crate) struct Spill {
    config: Config,
    path: PathBuf,
    /// The open file, for the move into a job's file.
    file: Option<File>,
    /// A test's writer around a duplicate handle; `None` writes `file`.
    writer: Option<Box<dyn Write + Send>>,
    /// The file is this spill's own and still on disk.
    created: bool,
    /// Why the file was not kept, once it was not.
    failed: Option<String>,
    /// A partial file this spill could not remove, for the loop to remove.
    left: Option<PathBuf>,
    /// The first raw bytes, at most the bound's start count.
    pub(super) head: Vec<u8>,
    pub(super) ring: Ring,
    /// Raw command-output bytes seen, closing lines included once closed.
    pub(super) total: u64,
}

impl Spill {
    /// Starts a spill from the held `buffer`: creates the file and feeds
    /// the buffer through it as the first chunk.
    pub(super) fn start(config: Config, buffer: Vec<u8>) -> Self {
        let name = config.name.clone().unwrap_or_else(random_name);
        let path = config.dir.join(&name);
        let mut spill = Self {
            ring: Ring {
                bytes: Vec::new(),
                cap: config.bound.end,
            },
            config,
            path,
            file: None,
            writer: None,
            created: false,
            failed: None,
            left: None,
            head: Vec::new(),
            total: 0,
        };
        if spill.path.to_str().is_none() {
            spill.failed = Some(format!(
                "could not save the output to {}: the path is not valid UTF-8",
                spill.path.display()
            ));
        } else {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&spill.path)
            {
                Ok(file) => {
                    spill.created = true;
                    match file.try_clone() {
                        Ok(dup) => {
                            spill.file = Some(file);
                            if let Some(wrap) = spill.config.wrap.clone() {
                                spill.writer = Some(wrap(dup));
                            }
                        }
                        Err(error) => spill.fail_create(error),
                    }
                }
                Err(error) => spill.fail_create(error),
            }
        }
        spill.push(&buffer);
        spill
    }

    /// Records a create failure: no file was created, so none is removed.
    fn fail_create(&mut self, error: io::Error) {
        self.failed = Some(format!(
            "could not save the output to {}: {error}",
            self.path.display()
        ));
    }

    /// Takes the next output `chunk`: the head keeps the first bytes, the
    /// ring the last, and the file gets every byte until a write fails.
    /// After a failure only the head and the ring are held, and the
    /// command runs on.
    pub(super) fn push(&mut self, chunk: &[u8]) {
        let want = self
            .config
            .bound
            .start
            .saturating_sub(self.head.len())
            .min(chunk.len());
        self.head
            .extend_from_slice(chunk.get(..want).unwrap_or_default());
        self.ring.push(chunk);
        self.total = self
            .total
            .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if self.failed.is_none() {
            let result = match self.writer.as_mut() {
                Some(writer) => writer.write_all(chunk),
                None => match self.file.as_mut() {
                    Some(file) => file.write_all(chunk),
                    None => Ok(()),
                },
            };
            if let Err(error) = result {
                self.fail_write(error);
            }
        }
    }

    /// Records a failed write: the partial file this spill created is
    /// closed and removed, unless the removal fails, which leaves it as
    /// `left` with the removal's error appended to the message.
    fn fail_write(&mut self, error: io::Error) {
        self.file = None;
        self.writer = None;
        let mut message = format!(
            "could not save the output to {}: {error}",
            self.path.display()
        );
        if self.created {
            self.created = false;
            if let Err(remove) = fs::remove_file(&self.path) {
                message.push_str(&format!(". It could not be removed: {remove}"));
                self.left = Some(self.path.clone());
            }
        }
        self.failed = Some(message);
    }

    /// Ends the spill: appends the result's closing lines (`extra`, when
    /// the job could not be opened, then `line`) to the file and hands
    /// back the decoded ends with the file. `head`, then `removed` bytes
    /// of the file, then `tail` reconstruct the file.
    pub(super) fn close(mut self, extra: Option<&str>, line: &str) -> Spilled {
        let mut closing = Vec::new();
        let last = self.ring.bytes.last().or(self.head.last());
        if self.total > 0 && last.is_none_or(|byte| *byte != b'\n') {
            closing.push(b'\n');
        }
        if let Some(extra) = extra {
            closing.extend_from_slice(extra.as_bytes());
            closing.push(b'\n');
        }
        closing.extend_from_slice(line.as_bytes());
        closing.push(b'\n');
        self.push(&closing);
        if self.failed.is_none() {
            let flushed = match self.writer.as_mut() {
                Some(writer) => writer.flush(),
                None => match self.file.as_mut() {
                    Some(file) => file.flush(),
                    None => Ok(()),
                },
            };
            if let Err(error) = flushed {
                self.fail_write(error);
            }
        }
        let size = self.total;
        let (head, head_used, tail, tail_used) = self.cut();
        let removed = size.saturating_sub(head_used).saturating_sub(tail_used);
        let file = match self.failed {
            Some(message) => Err(Unsaved {
                message,
                left: self.left,
            }),
            None => Ok(SpillFile {
                path: self.path,
                size,
            }),
        };
        Spilled {
            head,
            tail,
            head_bytes: head_used,
            tail_bytes: tail_used,
            removed,
            file,
        }
    }

    /// The decoded ends: `head` is the longest raw prefix whose lossy
    /// decoding fits the bound's start, `tail` the longest raw suffix,
    /// not overlapping that prefix, fitting the end, each with its raw
    /// byte count. When the ends overlap they join to the whole file.
    fn cut(&self) -> (String, u64, String, u64) {
        let bound = self.config.bound;
        let ring_len = u64::try_from(self.ring.bytes.len()).unwrap_or(u64::MAX);
        let head_len = u64::try_from(self.head.len()).unwrap_or(u64::MAX);
        if head_len.saturating_add(ring_len) > self.total {
            let overlap = head_len.saturating_add(ring_len).saturating_sub(self.total);
            let overlap = usize::try_from(overlap).unwrap_or(usize::MAX);
            let mut whole = self.head.clone();
            whole.extend_from_slice(self.ring.bytes.get(overlap..).unwrap_or_default());
            let (head, used) = decode_prefix(&whole, bound.start);
            let head_used = u64::try_from(used).unwrap_or(u64::MAX);
            let (tail, used) = decode_suffix(whole.get(used..).unwrap_or_default(), bound.end);
            let tail_used = u64::try_from(used).unwrap_or(u64::MAX);
            (head, head_used, tail, tail_used)
        } else {
            let (head, used) = decode_prefix(&self.head, bound.start);
            let head_used = u64::try_from(used).unwrap_or(u64::MAX);
            let (tail, used) = decode_suffix(trim_split_start(&self.ring.bytes), bound.end);
            let tail_used = u64::try_from(used).unwrap_or(u64::MAX);
            (head, head_used, tail, tail_used)
        }
    }

    /// Hands the spill to the job's output file at `job_path`: renames the
    /// spilled file onto it and keeps the spill's own handle, positioned
    /// at its end, as the sink. When the rename fails, copies the spilled
    /// file into the registry's handle instead and removes the spill. A
    /// spill that already failed writes its held head, then its ring, to
    /// the job's file, and retries removing its `left` file. The string
    /// names any raw file left behind, for the receipt.
    pub(super) fn into_job(
        mut self,
        job_path: &Path,
        mut registry: File,
    ) -> (File, Option<String>) {
        if self.failed.is_none() {
            // The writer wraps a duplicate handle: dropping it keeps the
            // spill's own handle as the sink.
            drop(self.writer.take());
            if fs::rename(&self.path, job_path).is_ok() {
                self.created = false;
                if let Some(mut file) = self.file.take() {
                    // The handle named the inode the rename moved: seek it
                    // to the end, so later output appends to the job's file.
                    let _position = file.seek(SeekFrom::End(0));
                    return (file, None);
                }
            } else {
                let copied = File::open(&self.path)
                    .and_then(|mut source| io::copy(&mut source, &mut registry).map(|_| ()))
                    .is_ok();
                if copied {
                    self.created = false;
                    return match fs::remove_file(&self.path) {
                        Ok(()) => (registry, None),
                        Err(_) => (registry, Some(still_there(&self.path))),
                    };
                }
                return (registry, Some(still_there(&self.path)));
            }
        }
        // A failed spill, or a rename that left no handle: the job's file
        // gets the held head, then the ring, then later output.
        let mut head = self.head;
        head.extend_from_slice(&self.ring.bytes);
        super::output::write_or_drop(&mut registry, &head);
        let mut line = None;
        if let Some(left) = self.left.take() {
            // One more try; a file that survives is named in the receipt.
            if fs::remove_file(&left).is_err() {
                line = Some(still_there(&left));
            }
        }
        (registry, line)
    }
}

/// The receipt's extra line naming a raw file left behind: a copy that
/// failed, a removal that failed, or a failed spill's surviving file.
fn still_there(path: &Path) -> String {
    format!("Output before the move is also in {}.", path.display())
}

/// A random `s_<16 hex digits>.txt` name, as `web_fetch` names its
/// artifacts.
fn random_name() -> String {
    format!("s_{:016x}.txt", RandomState::new().hash_one(()))
}

/// The longest raw prefix whose lossy decoding is at most `limit` bytes,
/// on decoder boundaries, with its raw byte count: an invalid byte
/// decodes to the 3-byte U+FFFD, so it counts as 3.
fn decode_prefix(raw: &[u8], limit: usize) -> (String, usize) {
    let mut text = String::new();
    let mut used = 0;
    let mut remaining = limit;
    for chunk in raw.utf8_chunks() {
        let valid = chunk.valid();
        let take = valid.floor_char_boundary(remaining.min(valid.len()));
        text.push_str(valid.get(..take).unwrap_or_default());
        used += take;
        remaining -= take;
        if take < valid.len() {
            break;
        }
        let invalid = chunk.invalid();
        if invalid.is_empty() {
            continue;
        }
        if remaining < 3 {
            break;
        }
        text.push('\u{FFFD}');
        used += invalid.len();
        remaining -= 3;
    }
    (text, used)
}

/// The longest raw suffix whose lossy decoding is at most `limit` bytes,
/// with its raw byte count. Whole decoder units (a character, or one
/// invalid run decoding to U+FFFD) are dropped from the front until the
/// rest fits.
fn decode_suffix(raw: &[u8], limit: usize) -> (String, usize) {
    let mut units: Vec<(usize, usize)> = Vec::new();
    for chunk in raw.utf8_chunks() {
        for character in chunk.valid().chars() {
            units.push((character.len_utf8(), character.len_utf8()));
        }
        if !chunk.invalid().is_empty() {
            units.push((chunk.invalid().len(), 3));
        }
    }
    let mut decoded: usize = units.iter().map(|(_, decoded)| decoded).sum();
    let mut start = 0;
    for (raw_len, decoded_len) in &units {
        if decoded <= limit {
            break;
        }
        start += raw_len;
        decoded = decoded.saturating_sub(*decoded_len);
    }
    let tail = raw.get(start..).unwrap_or_default();
    (String::from_utf8_lossy(tail).into_owned(), tail.len())
}

/// Drops a split character at the start of a cut: at most 3 leading
/// continuation bytes (`10xxxxxx`), the most a split valid character can
/// leave. Further continuation bytes are kept, decoding to U+FFFD as they
/// would have uncut.
fn trim_split_start(bytes: &[u8]) -> &[u8] {
    let skip = bytes
        .iter()
        .take(3)
        .take_while(|byte| **byte & 0xC0 == 0x80)
        .count();
    bytes.get(skip..).unwrap_or_default()
}

#[cfg(test)]
#[path = "spill_tests.rs"]
mod tests;
