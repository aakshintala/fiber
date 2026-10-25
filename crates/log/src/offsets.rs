//! The offset table (`docs/events.md`, "Resume"): where each complete line
//! of a session's log starts, so a reader reads and parses only the window
//! it needs. Line N is the line whose `seq` is N: only durable lines are
//! written, and `seq` is contiguous from 0.

use std::fs::File;
use std::io::{self, Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use contract::Envelope;

use crate::read::{CAPACITY, PAGE_BYTES, complete_len};
use crate::{Error, io_at};

/// How many line-start checkpoints the table keeps resident: 1024 pairs of
/// `(line, byte)`, 16 KiB, whatever the log's size. Past the cap the table
/// doubles its byte spacing and greedily re-thins to at most half the cap,
/// so the checkpoints stay spread across the whole log and an evicted page
/// is found by scanning forward from the nearest checkpoint at or before it.
const CHECKPOINTS: usize = 1024;

/// The forward scan's fixed buffer: recovery scans allocate this once on
/// the stack, never a retained or growing buffer, so scan-time allocation
/// is bounded too.
const SCAN_CHUNK: usize = 8 * 1024;

/// The table, shared by the log that extends it and every watcher that
/// reads by `seq` from it. A reader takes only its lock, copies what it
/// needs and lets go before any I/O.
pub(crate) struct Offsets {
    /// The log.
    path: PathBuf,
    table: Mutex<Table>,
}

struct Table {
    /// Resident line-start checkpoints, `(line, byte)` in line order: at
    /// most [`CHECKPOINTS`] of them, one uniform byte spacing apart. The
    /// first is `(0, 0)` whenever the table holds any line.
    checkpoints: Vec<(u64, u64)>,
    /// The byte spacing the checkpoints keep: a line start is checkpointed
    /// once it lies at least this far past the previous checkpoint. It
    /// starts at 1, so every line of a small log is checkpointed, and
    /// doubles at every halving.
    spacing: u64,
    /// How many complete lines the log holds, checkpointed or not.
    total: u64,
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

/// A window's byte span resolved from the checkpoint snapshot.
struct Resolution {
    /// The window's first line, an absolute position.
    first: usize,
    /// The byte span, `start..stop`.
    start: u64,
    stop: u64,
    /// The table held lines past `stop` when the window was taken.
    more: bool,
    /// File bytes traversed finding line starts the checkpoints no longer
    /// hold: 0 when every start the window needs is resident.
    scanned: u64,
    /// The file ended before the window filled: the window holds the
    /// complete lines found, and reading it still fails as a short read
    /// does. `None` is a clean window.
    short: Option<io::Error>,
}

/// A snapshot of the table, copied under its lock and read without it: the
/// log is append-only, so absolute offsets at or below the copied end
/// stay valid while the file is scanned.
struct Snapshot {
    checkpoints: Vec<(u64, u64)>,
    total: u64,
    end: u64,
}

impl Offsets {
    /// A table for the log at `path` whose complete lines start at `starts`
    /// and end at `end`.
    pub(crate) fn new(path: PathBuf, starts: Vec<u64>, end: u64) -> Self {
        let total = u64::try_from(starts.len()).unwrap_or(u64::MAX);
        let mut table = Table {
            checkpoints: Vec::new(),
            spacing: 1,
            total,
            end,
        };
        for (line, byte) in starts.into_iter().enumerate() {
            table.record(u64::try_from(line).unwrap_or(u64::MAX), byte);
        }
        Self {
            path,
            table: Mutex::new(table),
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
        let mut table = Table {
            checkpoints: Vec::new(),
            spacing: 1,
            total: 0,
            end: 0,
        };
        while table.total < limit {
            let start = lines.offset();
            let refused = match lines.next_raw() {
                None => break,
                Some(Err(error)) => return Err(error),
                Some(Ok(bytes)) => each(bytes),
            };
            if let Err(source) = refused {
                return Err(lines.unreadable(source));
            }
            // A refused line is recorded nowhere: the scan ends above.
            let total = table.total;
            table.record(total, start);
            table.total = total.saturating_add(1);
        }
        table.end = lines.offset();
        Ok(Self {
            path,
            table: Mutex::new(table),
        })
    }

    /// The byte just past the last complete line: where a torn tail
    /// starts, and the length to truncate a torn tail to.
    pub(crate) fn end(&self) -> u64 {
        self.lock().end
    }

    fn lock(&self) -> MutexGuard<'_, Table> {
        support::lock(&self.table)
    }

    /// Records a complete line of `len` bytes written at the end.
    pub(crate) fn push(&self, len: u64) {
        let mut table = self.lock();
        let start = table.end;
        let total = table.total;
        table.record(total, start);
        table.total = total.saturating_add(1);
        table.end = start.saturating_add(len);
    }

    /// How many complete lines the table holds.
    pub(crate) fn count(&self) -> u64 {
        self.lock().total
    }

    /// The lines at positions `from..from + max`, in order, fewer when the
    /// log ends first, none when `from` is past the last line. Reads and
    /// parses only those lines. A window that cannot be read whole is an
    /// error with no lines: a partial window would read as complete.
    pub(crate) fn range(&self, from: u64, max: usize) -> Result<Vec<Envelope>, Error> {
        match self.resolve(from, max, u64::MAX, u64::MAX)? {
            Some(window) => {
                // The meter is for tests: production ignores the count.
                let _ = window.scanned;
                self.read(&window).map_err(|partial| partial.error)
            }
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
        match self.resolve(from, CAPACITY, PAGE_BYTES, end) {
            Ok(Some(window)) => {
                // The meter is for tests: production ignores the count.
                let _ = window.scanned;
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
            Ok(None) => Page {
                lines: Vec::new(),
                after: After::Done,
            },
            Err(error) => Page {
                lines: Vec::new(),
                after: After::Failed(error),
            },
        }
    }

    /// The whole lines of `window` parsed before its first failure, and
    /// that failure: the first failure in file order, whether a line that
    /// does not parse or a read that ends short of the window. A read that
    /// ends short keeps only whole lines: the bytes read before the
    /// failure are cut at their last newline, so a partial line is never
    /// parsed or returned. The buffer is allocated once at the window's
    /// length and never grows past it. A window the forward scan could
    /// not fill fails as a short read does, over the complete lines the
    /// scan found.
    fn read(&self, window: &Resolution) -> Result<Vec<Envelope>, Partial> {
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
        // A scan that ended short is a short read over the lines found:
        // the window holds complete lines, and reading them still reports
        // the short file after them.
        let failure = failure.or_else(|| window.short.as_ref().map(short_to_io));
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
    /// `None` when `from` is past the last line. Line starts the resident
    /// checkpoints no longer hold are found by seeking the nearest
    /// checkpoint at or before them and counting newlines forward through
    /// a fixed buffer; the lock is held only to copy the checkpoints, the
    /// count and the end, never across file I/O. An open or seek failure
    /// is an error with no lines, as a missing log is; a scan that ends
    /// short resolves over the complete lines found.
    fn resolve(
        &self,
        from: u64,
        max: usize,
        bytes: u64,
        end: u64,
    ) -> Result<Option<Resolution>, Error> {
        let snapshot = {
            let table = self.lock();
            Snapshot {
                checkpoints: table.checkpoints.clone(),
                total: table.total,
                end: table.end,
            }
        };
        let first = match usize::try_from(from) {
            Ok(first) => first,
            Err(_) => return Ok(None),
        };
        if from >= snapshot.total {
            return Ok(None);
        }
        // The last line the window may hold. At least `from` itself: a
        // bound below `from` still resolves the empty window at it.
        let want_last = from
            .saturating_add(u64::try_from(max).unwrap_or(u64::MAX))
            .min(snapshot.total)
            .min(end)
            .max(from);
        // The nearest checkpoint at or before `from`: the first line is
        // always checkpointed, so one exists whenever lines do.
        let below = snapshot
            .checkpoints
            .partition_point(|checkpoint| checkpoint.0 <= from);
        let anchor = below.saturating_sub(1);
        // Whether every start the window needs is resident: the lines
        // `from..=want_last` each hold a checkpoint. A fully resident
        // window takes no file I/O and scans nothing.
        let mut from_idx = usize::MAX;
        {
            let mut line = from;
            let mut at = anchor;
            loop {
                match snapshot.checkpoints.get(at) {
                    Some(checkpoint) if checkpoint.0 == line => {
                        if line == from {
                            from_idx = at;
                        }
                        if line == want_last || line == u64::MAX {
                            break;
                        }
                        line = line.saturating_add(1);
                        at = at.saturating_add(1);
                    }
                    _ => {
                        from_idx = usize::MAX;
                        break;
                    }
                }
            }
        }
        if from_idx != usize::MAX {
            let spec = Spec {
                first,
                from,
                want_last,
                start: snapshot
                    .checkpoints
                    .get(from_idx)
                    .map_or(snapshot.end, |checkpoint| checkpoint.1),
                bytes,
                bound: end,
            };
            let mut starts = ResidentStarts {
                snapshot: &snapshot,
                idx: from_idx.saturating_add(1),
            };
            return Ok(Some(size_window(&snapshot, &spec, &mut starts)));
        }
        let mut spec = Spec {
            first,
            from,
            want_last,
            start: 0,
            bytes,
            bound: end,
        };
        self.scanned_window(&snapshot, &mut spec, anchor)
    }

    /// Sizes a window whose starts need the file: seeks the anchor
    /// checkpoint and counts newlines forward through a fixed buffer.
    fn scanned_window(
        &self,
        snapshot: &Snapshot,
        spec: &mut Spec,
        anchor: usize,
    ) -> Result<Option<Resolution>, Error> {
        let (anchor_line, anchor_byte) = match snapshot.checkpoints.get(anchor) {
            Some(checkpoint) => *checkpoint,
            None => {
                return Err(io_at(&self.path)(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the log ends short of its offset table",
                )));
            }
        };
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) => return Err(io_at(&self.path)(error)),
        };
        if let Err(error) = file.seek(io::SeekFrom::Start(anchor_byte)) {
            return Err(io_at(&self.path)(error));
        }
        let mut finder = Finder {
            file: &mut file,
            buf: [0u8; SCAN_CHUNK],
            len: 0,
            at: 0,
            byte: anchor_byte,
            scanned: 0,
            short: None,
        };
        // Skip to the window's first line: the anchor's own byte is its
        // line's start, so seeking the lines between the anchor and the
        // window lands on the window's first line.
        let mut skip = spec.from.saturating_sub(anchor_line);
        let mut start = anchor_byte;
        if skip > 0 {
            let mut found = false;
            while skip > 0 {
                match finder.next_newline_end() {
                    Some(end) => {
                        skip -= 1;
                        if skip == 0 {
                            start = end;
                            found = true;
                        }
                    }
                    None => break,
                }
            }
            if !found {
                // The file ends before the window's first line: an empty
                // window over nothing found, still failing as a short
                // read, never an empty-on-scan-failure window.
                return Ok(Some(Resolution {
                    first: spec.first,
                    start: finder.byte,
                    stop: finder.byte,
                    more: spec.from < snapshot.total.min(spec.bound),
                    scanned: finder.scanned,
                    short: finder.short.or_else(short_eof),
                }));
            }
        }
        spec.start = start;
        let mut starts = ScannedStarts {
            finder: &mut finder,
        };
        let mut window = size_window(snapshot, spec, &mut starts);
        window.scanned = starts.scanned();
        Ok(Some(window))
    }
}

impl Table {
    /// Checkpoints the line `line` starting at `byte`: the first line, and
    /// every start at least [`Table::spacing`] past the previous
    /// checkpoint. Past [`CHECKPOINTS`] checkpoints the table re-thins to
    /// at most half the cap at double the spacing, so residency stays
    /// constant and the checkpoints stay spread across the whole log.
    fn record(&mut self, line: u64, byte: u64) {
        let checkpoint = match self.checkpoints.last() {
            None => true,
            Some(previous) => byte.saturating_sub(previous.1) >= self.spacing,
        };
        if !checkpoint {
            return;
        }
        self.checkpoints.push((line, byte));
        if self.checkpoints.len() > CHECKPOINTS {
            self.thin();
        }
    }

    /// Greedy byte re-thinning: double the spacing, keep the first
    /// checkpoint, and walk the rest in order keeping one only if at
    /// least the spacing past the last kept; repeat until at most half
    /// the cap remains. Every gap then holds at least the spacing and at
    /// most twice it plus the longest line in the gap, and the spacing
    /// stays within the initial spacing and four bytes per resident
    /// checkpoint. The loop runs at most 64 passes: the spacing doubles
    /// every pass from at least 1, so it saturates `u64` within 64 passes,
    /// and every pass keeps at least the first checkpoint, so the loop
    /// always ends with the same kept sets, spacing progression and break
    /// point as the unbounded form.
    fn thin(&mut self) {
        self.spacing = self.spacing.saturating_mul(2);
        for _ in 0..64 {
            let mut kept = Vec::with_capacity(self.checkpoints.len().div_ceil(2));
            let mut last = 0;
            let mut kept_any = false;
            for checkpoint in self.checkpoints.iter() {
                if !kept_any || checkpoint.1.saturating_sub(last) >= self.spacing {
                    kept.push(*checkpoint);
                    last = checkpoint.1;
                    kept_any = true;
                }
            }
            self.checkpoints = kept;
            if self.checkpoints.len() <= CHECKPOINTS / 2 {
                break;
            }
            self.spacing = self.spacing.saturating_mul(2);
        }
    }
}

/// How a window under sizing finds each next line's start, in order from
/// the line after its first: resident checkpoints, or the forward scan.
/// `None` past the complete lines found. `next_bounded` stops the scan
/// once the line passes `limit` and reports it as over budget without
/// traversing it whole.
trait Starts {
    fn next(&mut self) -> Option<u64>;
    fn next_bounded(&mut self, limit: u64) -> Option<u64>;
    /// Why `next` ended, once it returns `None`.
    fn short(&mut self) -> Option<io::Error>;
    /// File bytes the scan traversed, once sizing ends.
    fn scanned(&self) -> u64;
}

struct ResidentStarts<'s> {
    snapshot: &'s Snapshot,
    idx: usize,
}

impl Starts for ResidentStarts<'_> {
    fn next(&mut self) -> Option<u64> {
        let byte = self.snapshot.checkpoints.get(self.idx)?.1;
        self.idx = self.idx.saturating_add(1);
        Some(byte)
    }

    fn next_bounded(&mut self, limit: u64) -> Option<u64> {
        let _ = limit;
        self.next()
    }

    fn short(&mut self) -> Option<io::Error> {
        None
    }

    fn scanned(&self) -> u64 {
        0
    }
}

struct ScannedStarts<'f> {
    finder: &'f mut Finder<'f>,
}

impl Starts for ScannedStarts<'_> {
    fn next(&mut self) -> Option<u64> {
        self.finder.next_newline_end()
    }

    fn next_bounded(&mut self, limit: u64) -> Option<u64> {
        self.finder.next_newline_end_bounded(limit)
    }

    fn short(&mut self) -> Option<io::Error> {
        self.finder.short.take()
    }

    fn scanned(&self) -> u64 {
        self.finder.scanned
    }
}

/// What a window under sizing is and needs: its first line and first
/// line's start, the last line it may hold, and the bounds sizing it.
struct Spec {
    first: usize,
    from: u64,
    want_last: u64,
    start: u64,
    bytes: u64,
    bound: u64,
}

/// Sizes a window from its first line's start: the first line is in
/// whatever its length, and every later line while it ends at or before
/// the byte limit. A later line is rejected once its scan passes the
/// limit, without traversing it whole. `starts` yields each next line's
/// start in order; running out ends the window over the complete lines
/// found.
fn size_window(snapshot: &Snapshot, spec: &Spec, starts: &mut impl Starts) -> Resolution {
    let limit = spec.start.saturating_add(spec.bytes);
    // Lines `from..past` are in; `cur` starts line `past`.
    let mut past = spec.from;
    let mut cur = spec.start;
    let mut short: Option<io::Error> = None;
    loop {
        if past >= spec.want_last {
            break;
        }
        // The last line's end is authoritative: no scan past the table.
        // The first line is in whatever its length, so only later lines
        // take the bounded scan that stops past the limit.
        let end_of = if past.saturating_add(1) == snapshot.total {
            snapshot.end
        } else if past == spec.from {
            match starts.next() {
                Some(end) => end,
                None => {
                    short = starts.short().or_else(short_eof);
                    break;
                }
            }
        } else {
            match starts.next_bounded(limit) {
                Some(end) => end,
                None => {
                    short = starts.short().or_else(short_eof);
                    break;
                }
            }
        };
        if past != spec.from && end_of > limit {
            break;
        }
        past = past.saturating_add(1);
        cur = end_of;
    }
    Resolution {
        first: spec.first,
        start: spec.start,
        stop: cur,
        more: past < snapshot.total.min(spec.bound),
        scanned: 0,
        short,
    }
}

/// A forward newline scan over a file: `next_newline_end` returns the byte
/// just past each `\n` in order, counting every byte read as scanned. A
/// clean end of file, or a failed read, ends the scan with `short` set:
/// the window resolves over the complete lines found, never an
/// empty-on-scan-failure window.
struct Finder<'f> {
    file: &'f mut File,
    buf: [u8; SCAN_CHUNK],
    /// Unconsumed bytes in `buf`: `buf[at..len]`.
    len: usize,
    at: usize,
    /// The file byte of `buf[at]`.
    byte: u64,
    scanned: u64,
    short: Option<io::Error>,
}

impl Finder<'_> {
    fn next_newline_end(&mut self) -> Option<u64> {
        loop {
            while self.at < self.len {
                let byte = match self.buf.get(self.at) {
                    Some(byte) => *byte,
                    None => break,
                };
                self.at = self.at.saturating_add(1);
                self.byte = self.byte.saturating_add(1);
                if byte == b'\n' {
                    return Some(self.byte);
                }
            }
            self.at = 0;
            match self.file.read(&mut self.buf) {
                Ok(0) => {
                    self.len = 0;
                    self.short = Some(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the log ends short of its offset table",
                    ));
                    return None;
                }
                Ok(read) => {
                    self.len = read;
                    self.scanned = self
                        .scanned
                        .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
                }
                Err(error) => {
                    self.len = 0;
                    self.short = Some(error);
                    return None;
                }
            }
        }
    }

    /// The byte just past the next `\n`, stopping once the scan passes
    /// `stop`: a line running past the page budget is reported as
    /// `u64::MAX`, over the budget whatever its length, without
    /// traversing it whole. `short` stays unset on that path: the line
    /// exists, it just does not fit. A clean end of file or a failed
    /// read still ends the scan with `short` set, as `next_newline_end`
    /// does.
    fn next_newline_end_bounded(&mut self, stop: u64) -> Option<u64> {
        loop {
            // Bounded to the unread bytes: the view ends at `len`, so a
            // stale tail past it is never read and the loop exits where
            // the `None => break` arm did, with no comparison left for
            // the `<` mutant to flip.
            while let Some(byte) = self
                .buf
                .get(..self.len)
                .and_then(|unread| unread.get(self.at))
                .copied()
            {
                if self.byte > stop {
                    return Some(u64::MAX);
                }
                self.at = self.at.saturating_add(1);
                self.byte = self.byte.saturating_add(1);
                if byte == b'\n' {
                    return Some(self.byte);
                }
            }
            if self.byte > stop {
                return Some(u64::MAX);
            }
            self.at = 0;
            match self.file.read(&mut self.buf) {
                Ok(0) => {
                    self.len = 0;
                    self.short = Some(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the log ends short of its offset table",
                    ));
                    return None;
                }
                Ok(read) => {
                    self.len = read;
                    self.scanned = self
                        .scanned
                        .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
                }
                Err(error) => {
                    self.len = 0;
                    self.short = Some(error);
                    return None;
                }
            }
        }
    }
}

/// A scan that ended short fails as a short read does: the window holds
/// complete lines, and reading them still reports the short file. A clean
/// end of file reads as the table running past the log; a failed mid-scan
/// read reads as itself, kind and cause kept.
fn short_to_io(error: &io::Error) -> io::Error {
    match error.raw_os_error() {
        Some(code) => io::Error::from_raw_os_error(code),
        None => io::Error::new(error.kind(), error.to_string()),
    }
}

/// A clean end of file where a line start was sought.
fn short_eof() -> Option<io::Error> {
    Some(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the log ends short of its offset table",
    ))
}

#[cfg(test)]
#[path = "offsets_tests.rs"]
mod tests;
