//! The offset table (`docs/events.md`, "Resume"): where each complete line
//! of a session's log starts, so a reader reads and parses only the window
//! it needs. Line N is the line whose `seq` is N: only durable lines are
//! written, and `seq` is contiguous from 0.

use std::fs::File;
use std::io::{self, Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use contract::Envelope;

use crate::read::{CAPACITY, PAGE_BYTES, complete_len};
use crate::{Error, io_at};

/// The table, shared by the log that extends it and every watcher that
/// reads by `seq` from it. A reader takes only its lock, copies the window's
/// two offsets and lets go before any I/O.
pub(crate) struct Offsets {
    /// The log.
    path: PathBuf,
    table: Mutex<Table>,
}

struct Table {
    /// The first byte of each complete line.
    starts: Vec<u64>,
    /// The byte just past the last complete line.
    end: u64,
}

/// A watcher's page: the lines read, in order, and what follows them.
pub(crate) struct Page {
    pub(crate) lines: Vec<Envelope>,
    pub(crate) after: After,
}

/// What follows a page's lines.
pub(crate) enum After {
    /// Nothing below the page's `end` bound remains.
    Done,
    /// Lines below `end` remain past the page.
    More,
    /// The line after `lines` could not be read: the first failure in
    /// file order.
    Failed(Error),
}

/// The whole lines of a window parsed before its first failure, and that
/// failure.
struct Partial {
    lines: Vec<Envelope>,
    error: Error,
}

impl Offsets {
    /// A table for the log at `path` whose complete lines start at `starts`
    /// and end at `end`.
    pub(crate) fn new(path: PathBuf, starts: Vec<u64>, end: u64) -> Self {
        Self {
            path,
            table: Mutex::new(Table { starts, end }),
        }
    }

    /// The table of the log in `dir`: where each of its first `limit`
    /// complete lines starts, and the end of the last of them. It parses
    /// nothing itself, a torn tail is not a line, and a missing log is
    /// [`Error::NotFound`]. `each` runs on every complete line's bytes in
    /// order, before the next line is read; its first refusal ends the
    /// scan with that line named.
    pub(crate) fn scan(
        dir: &Path,
        limit: u64,
        mut each: impl FnMut(&[u8]) -> Result<(), serde_json::Error>,
    ) -> Result<Offsets, Error> {
        let mut lines = crate::read::lines(dir)?;
        let path = dir.join(crate::EVENTS);
        let mut starts = Vec::new();
        while u64::try_from(starts.len()).unwrap_or(u64::MAX) < limit {
            let start = lines.offset();
            let refused = match lines.next_raw() {
                None => break,
                Some(Err(error)) => return Err(error),
                Some(Ok(bytes)) => each(bytes),
            };
            if let Err(source) = refused {
                return Err(lines.unreadable(source));
            }
            starts.push(start);
        }
        let end = lines.offset();
        Ok(Offsets::new(path, starts, end))
    }

    /// The byte just past the last complete line: where a torn tail
    /// starts, and the length to truncate a torn tail to.
    pub(crate) fn end(&self) -> u64 {
        self.lock().end
    }

    fn lock(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a complete line of `len` bytes written at the end.
    pub(crate) fn push(&self, len: u64) {
        let mut table = self.lock();
        let start = table.end;
        table.starts.push(start);
        table.end = start.saturating_add(len);
    }

    /// How many complete lines the table holds.
    pub(crate) fn count(&self) -> u64 {
        u64::try_from(self.lock().starts.len()).unwrap_or(u64::MAX)
    }

    /// The lines at positions `from..from + max`, in order, fewer when the
    /// log ends first, none when `from` is past the last line. Reads and
    /// parses only those lines. A window that cannot be read whole is an
    /// error with no lines: a partial window would read as complete.
    pub(crate) fn range(&self, from: u64, max: usize) -> Result<Vec<Envelope>, Error> {
        match self.window(from, max, u64::MAX, u64::MAX) {
            Some(window) => self.read(&window).map_err(|partial| partial.error),
            None => Ok(Vec::new()),
        }
    }

    /// A watcher's page: the lines from position `from` below `end`, in
    /// order, at most [`CAPACITY`] of them and at most [`PAGE_BYTES`] of
    /// them but never fewer than one, and what follows them. No lines and
    /// done when `from` is past the last line below `end`. A page whose
    /// line does not parse, or whose read fails partway, keeps every whole
    /// line before the failure, then the failure. A backlog page passes
    /// the table's line count at subscribe time as `end`, so a line
    /// appended later arrives through the watcher's queue, in queue order;
    /// a catch-up passes `u64::MAX` to read to the table's current end.
    pub(crate) fn page(&self, from: u64, end: u64) -> Page {
        match self.window(from, CAPACITY, PAGE_BYTES, end) {
            Some(window) => {
                let more = window.more;
                match self.read(&window) {
                    Ok(lines) => Page {
                        lines,
                        after: if more { After::More } else { After::Done },
                    },
                    Err(partial) => Page {
                        lines: partial.lines,
                        after: After::Failed(partial.error),
                    },
                }
            }
            None => Page {
                lines: Vec::new(),
                after: After::Done,
            },
        }
    }

    /// The whole lines of `window` parsed before its first failure, and
    /// that failure: the first failure in file order, whether a line that
    /// does not parse or a read that ends short of the window. A read that
    /// ends short keeps only whole lines: the bytes read before the
    /// failure are cut at their last newline, so a partial line is never
    /// parsed or returned. The buffer is allocated once at the window's
    /// length and never grows past it.
    fn read(&self, window: &Window) -> Result<Vec<Envelope>, Partial> {
        let len = window.stop.saturating_sub(window.start);
        let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(usize::MAX));
        let mut open = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) => {
                return Err(Partial {
                    lines: Vec::new(),
                    error: io_at(&self.path)(error),
                });
            }
        };
        let failure = match open.seek(io::SeekFrom::Start(window.start)) {
            Ok(_) => match open.take(len).read_to_end(&mut bytes) {
                Ok(read) if u64::try_from(read).unwrap_or(u64::MAX) >= len => None,
                Ok(_) => Some(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the log ends short of its offset table",
                )),
                Err(error) => Some(error),
            },
            Err(error) => Some(error),
        };
        bytes.truncate(complete_len(&bytes));
        let mut lines = Vec::new();
        for (i, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
            match serde_json::from_slice(line) {
                Ok(envelope) => lines.push(envelope),
                Err(source) => {
                    return Err(Partial {
                        lines,
                        error: Error::Unreadable {
                            path: self.path.clone(),
                            line: window.first.saturating_add(i).saturating_add(1),
                            source,
                        },
                    });
                }
            }
        }
        match failure {
            None => Ok(lines),
            Some(error) => Err(Partial {
                lines,
                error: io_at(&self.path)(error),
            }),
        }
    }

    /// The window of at most `max` lines from position `from` below `end`,
    /// stopping before a line that would take it past `bytes`; its first
    /// line is in it whatever its length, so a window always moves forward.
    /// `None` when `from` is past the last line.
    fn window(&self, from: u64, max: usize, bytes: u64, end: u64) -> Option<Window> {
        let table = self.lock();
        let first = usize::try_from(from).ok()?;
        let start = *table.starts.get(first)?;
        let limit = start.saturating_add(bytes);
        let bound = usize::try_from(end).unwrap_or(usize::MAX);
        let last = first.saturating_add(max).min(table.starts.len()).min(bound);
        let end_of = |line: usize| {
            let next = line.saturating_add(1);
            table.starts.get(next).copied().unwrap_or(table.end)
        };
        let mut past = first;
        while past < last && (past == first || end_of(past) <= limit) {
            past = past.saturating_add(1);
        }
        let stop = table.starts.get(past).copied().unwrap_or(table.end);
        let below = table.starts.len().min(bound);
        Some(Window {
            first,
            start,
            stop,
            more: past < below,
        })
    }
}

/// A run of whole lines in the log.
struct Window {
    /// The first line's position.
    first: usize,
    /// The byte span, `start..stop`.
    start: u64,
    stop: u64,
    /// The table held lines past `stop` when the window was taken.
    more: bool,
}

#[cfg(test)]
#[path = "offsets_tests.rs"]
mod tests;
