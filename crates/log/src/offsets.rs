//! The offset table (`docs/events.md`, "Resume"): where each complete line
//! of a session's log starts, so a reader reads and parses only the window
//! it needs. Line N is the line whose `seq` is N: only durable lines are
//! written, and `seq` is contiguous from 0.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use contract::Envelope;

use crate::read::CAPACITY;
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

impl Offsets {
    /// A table for the log at `path` whose complete lines start at `starts`
    /// and end at `end`.
    pub(crate) fn new(path: PathBuf, starts: Vec<u64>, end: u64) -> Self {
        Self {
            path,
            table: Mutex::new(Table { starts, end }),
        }
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
    /// parses only those lines.
    pub(crate) fn range(&self, from: u64, max: usize) -> Result<Vec<Envelope>, Error> {
        match self.window(from, max) {
            Some(window) => self.read(&window),
            None => Ok(Vec::new()),
        }
    }

    /// A watcher's page: the lines from position `from`, in order, at most
    /// [`CAPACITY`] of them, and whether the table held lines past them when
    /// they were read. No lines and `false` when `from` is past the last
    /// line.
    pub(crate) fn page(&self, from: u64) -> Result<(Vec<Envelope>, bool), Error> {
        match self.window(from, CAPACITY) {
            Some(window) => Ok((self.read(&window)?, window.more)),
            None => Ok((Vec::new(), false)),
        }
    }

    /// Reads and parses the lines of `window`. A line that does not parse
    /// fails the whole window, naming its line number.
    fn read(&self, window: &Window) -> Result<Vec<Envelope>, Error> {
        let len = usize::try_from(window.stop.saturating_sub(window.start)).unwrap_or(usize::MAX);
        let mut bytes = vec![0; len];
        File::open(&self.path)
            .and_then(|file| file.read_exact_at(&mut bytes, window.start))
            .map_err(io_at(&self.path))?;
        bytes
            .split_inclusive(|b| *b == b'\n')
            .enumerate()
            .map(|(i, line)| {
                serde_json::from_slice(line).map_err(|source| Error::Unreadable {
                    path: self.path.clone(),
                    line: window.first.saturating_add(i).saturating_add(1),
                    source,
                })
            })
            .collect()
    }

    /// The window of at most `max` lines from position `from`; `None` when
    /// `from` is past the last line. A window running past the last line
    /// stops at the end of it.
    fn window(&self, from: u64, max: usize) -> Option<Window> {
        let table = self.lock();
        let first = usize::try_from(from).ok()?;
        let start = *table.starts.get(first)?;
        let past = first.saturating_add(max);
        let stop = table.starts.get(past).copied().unwrap_or(table.end);
        Some(Window {
            first,
            start,
            stop,
            more: past < table.starts.len(),
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
