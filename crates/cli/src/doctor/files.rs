//! What `fiber doctor` reads without running anything: the log scan, the
//! crash directory, ages and the one control-character cleaner every
//! rendered field passes through.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, fstat, open, openat, statat};

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;

/// How much of each log file the scan reads, from the end.
const WINDOW: u64 = 10 * 1024 * 1024;

/// How much of one `code: message` the report prints.
const DISPLAY_CAP: usize = 64 * 1024;

/// One read from a log file: allocation stays bounded by the window.
const CHUNK: usize = 64 * 1024;

/// Every rendered field passes through this once: a control character
/// becomes a space, so one line is always one line.
pub(crate) fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// `now − then` in the largest whole unit, rounded down; the future is
/// `0s`.
pub(crate) fn age(now: SystemTime, then: SystemTime) -> String {
    let secs = now.duration_since(then).unwrap_or(Duration::ZERO).as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Opens `dir` for listing without following a link, and checks it is a
/// directory: a linked or missing directory reads as nothing.
fn open_dir(dir: &Path) -> Option<OwnedFd> {
    seam::fire_before_dir_open(dir);
    let fd = open(
        dir,
        OFlags::RDONLY
            .union(OFlags::DIRECTORY)
            .union(OFlags::NOFOLLOW)
            .union(OFlags::CLOEXEC),
        Mode::empty(),
    )
    .ok()?;
    let owned: OwnedFd = fd;
    if fstat(&owned).ok()?.st_mode & 0o170_000 != 0o040_000 {
        return None;
    }
    Some(owned)
}

/// The entry names of the open directory, in name order. A directory entry
/// whose type is known not to be a regular file is left out here; anything
/// else still faces the descriptor checks, which decide.
fn entry_names(dir: &OwnedFd) -> Vec<OsString> {
    let Ok(listing) = dir.try_clone() else {
        return Vec::new();
    };
    let Ok(listing) = Dir::read_from(listing) else {
        return Vec::new();
    };
    let mut names: Vec<OsString> = listing
        .flatten()
        .filter(|entry| {
            !matches!(
                entry.file_type(),
                FileType::Directory
                    | FileType::Symlink
                    | FileType::Fifo
                    | FileType::Socket
                    | FileType::CharacterDevice
                    | FileType::BlockDevice
            )
        })
        .map(|entry| OsStr::from_bytes(entry.file_name().to_bytes()).to_os_string())
        .collect();
    names.sort();
    names
}

/// Opens `name` relative to the open directory without following a link
/// and checks the descriptor is a regular file: anything else is skipped
/// without being read, so the scan never waits on a writer.
fn open_regular(dir: &OwnedFd, name: &OsStr) -> Option<(File, u64)> {
    seam::fire_before_file_open(name);
    let fd = openat(
        dir,
        name,
        OFlags::RDONLY
            .union(OFlags::NOFOLLOW)
            .union(OFlags::NONBLOCK)
            .union(OFlags::CLOEXEC),
        Mode::empty(),
    )
    .ok()?;
    let owned: OwnedFd = fd;
    let stat = fstat(&owned).ok()?;
    if stat.st_mode & 0o170_000 != 0o100_000 {
        return None;
    }
    let length = u64::try_from(stat.st_size).unwrap_or(u64::MAX);
    Some((File::from(owned), length))
}

/// The newest error line in `logs/`: its file name, `ts`, code and
/// message. Only the last 10 MiB of each file, up to the length its fstat
/// gave, is read; a line straddling the window's start is not parsed, and
/// an over-long line's display is cut to 64 KiB at a char boundary while
/// its `ts` still compares.
pub(crate) fn newest_error(logs: &Path) -> Option<(String, u64, String, String)> {
    let dir = open_dir(logs)?;
    seam::fire_after_dir_open(logs);
    let mut best: Option<(u64, String, String, String)> = None;
    for name in entry_names(&dir) {
        let Some((mut file, length)) = open_regular(&dir, &name) else {
            continue;
        };
        let start = length.saturating_sub(WINDOW);
        if file.seek(SeekFrom::Start(start)).is_err() {
            continue;
        }
        let mut bytes = Vec::new();
        let mut left = length - start;
        let mut offset = start;
        // `!= 0`, not `> 0`: `left` only shrinks to exactly zero, so both
        // read the same bytes; `> 0` leaves an equivalent `>= 0` mutant no
        // test can tell apart.
        while left != 0 {
            let want = usize::try_from(left.min(CHUNK as u64)).unwrap_or(CHUNK);
            seam::fire_before_chunk(&name, offset);
            let mut chunk = vec![0; want];
            let mut got = 0;
            // `!=`, not `<`: `got` only grows to exactly `want`, so both
            // read the same bytes; `<` leaves an equivalent `<=` mutant
            // whose extra empty read breaks at once, which no test can see.
            while got != want {
                let Some(slot) = chunk.get_mut(got..) else {
                    break;
                };
                match file.read(slot) {
                    Ok(0) => break,
                    Ok(n) => got += n,
                    Err(_) => break,
                }
            }
            if got == 0 {
                break;
            }
            seam::note_chunk(&name, offset, got);
            offset += got as u64;
            left -= got as u64;
            if let Some(seen) = chunk.get(..got) {
                bytes.extend_from_slice(seen);
            }
            if got < want {
                break;
            }
        }
        for line in lines_after(&bytes, start) {
            let Some((ts, code, message)) = parse_error(line) else {
                continue;
            };
            // Later in name order wins ties; later in a file wins.
            if best.as_ref().is_none_or(|(at, ..)| ts >= *at) {
                best = Some((ts, lossy(&name), code, message));
            }
        }
    }
    best.map(|(ts, file, code, message)| {
        let (code, message) = cut_display(&code, &message);
        (file, ts, code, message)
    })
}

/// The window's lines: when it starts after byte 0, the bytes before its
/// first newline belong to a line that began earlier and are not parsed. A
/// final line without a newline is eligible.
fn lines_after(bytes: &[u8], start: u64) -> impl Iterator<Item = &[u8]> {
    let mut lines = bytes.split(|b| *b == b'\n').peekable();
    if start > 0 {
        lines.next();
    }
    lines
}

/// A log line that is a JSON object with `level == "error"`, an integer
/// `ts` in milliseconds, and string `code` and `message`.
fn parse_error(line: &[u8]) -> Option<(u64, String, String)> {
    let value: serde_json::Value = serde_json::from_slice(line).ok()?;
    let object = value.as_object()?;
    if object.get("level")?.as_str()? != "error" {
        return None;
    }
    let ts = object.get("ts")?.as_u64()?;
    let code = object.get("code")?.as_str()?.to_owned();
    let message = object.get("message")?.as_str()?.to_owned();
    Some((ts, code, message))
}

/// Cuts `code: message` to 64 KiB at a char boundary, keeping the code
/// whole; an exactly 64 KiB line prints whole.
fn cut_display(code: &str, message: &str) -> (String, String) {
    if code.len() + 2 + message.len() <= DISPLAY_CAP {
        return (code.to_owned(), message.to_owned());
    }
    let mut end = DISPLAY_CAP
        .saturating_sub(code.len() + 2)
        .min(message.len());
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    (code.to_owned(), message[..end].to_owned())
}

/// The newest crash in `crashes/`: its full path and mtime, compared at
/// full resolution with the later name winning ties. Entries are stated without following links and no crash
/// file is opened or read.
pub(crate) fn newest_crash(crashes: &Path) -> Option<(PathBuf, SystemTime)> {
    let dir = open_dir(crashes)?;
    seam::fire_after_dir_open(crashes);
    let mut best: Option<(SystemTime, OsString)> = None;
    for name in entry_names(&dir) {
        let Ok(stat) = statat(&dir, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) else {
            continue;
        };
        if stat.st_mode & 0o170_000 != 0o100_000 {
            continue;
        }
        let secs = stat.st_mtime.max(0).cast_unsigned();
        let nanos = u32::try_from(stat.st_mtime_nsec).unwrap_or(0);
        let mtime = SystemTime::UNIX_EPOCH + Duration::new(secs, nanos);
        if best.as_ref().is_none_or(|(at, ..)| mtime >= *at) {
            best = Some((mtime, name));
        }
    }
    best.map(|(mtime, name)| (crashes.join(name), mtime))
}

fn lossy(name: &OsStr) -> String {
    String::from_utf8_lossy(name.as_bytes()).into_owned()
}

#[cfg(not(test))]
pub(crate) mod seam {
    //! Production seams: all three compile to nothing.

    use std::ffi::OsStr;

    /// No pause outside tests.
    pub(crate) fn fire_before_dir_open(_dir: &std::path::Path) {}

    /// No pause outside tests.
    pub(crate) fn fire_after_dir_open(_dir: &std::path::Path) {}

    /// No pause outside tests.
    pub(crate) fn fire_before_file_open(_name: &OsStr) {}

    /// No chunk pause outside tests.
    pub(crate) fn fire_before_chunk(_name: &OsStr, _offset: u64) {}

    /// No chunk mark outside tests.
    pub(crate) fn note_chunk(_name: &OsStr, _offset: u64, _len: usize) {}
}

#[cfg(test)]
pub(crate) mod seam {
    //! Test pauses and observers: exact signals with test-owned release
    //! senders, so a blocking-open mutant fails at the wall guard instead
    //! of hanging the suite.

    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::{Mutex, mpsc};

    /// One armed pause: what it waits on, and the channels that tell the
    /// test it is waiting and release it.
    struct Gate {
        /// The dir path or file name whose step waits.
        target: OsString,
        /// Tells the test the scan is waiting.
        arrived: mpsc::Sender<()>,
        /// The test releases the scan through it.
        release: mpsc::Receiver<()>,
    }

    /// Pauses before a directory's open, by dir path.
    static BEFORE_DIR: Mutex<Vec<Gate>> = Mutex::new(Vec::new());
    /// Pauses after a directory's open, by dir path.
    static AFTER_DIR: Mutex<Vec<Gate>> = Mutex::new(Vec::new());
    /// Pauses before a file's open, by file name.
    static BEFORE_FILE: Mutex<Vec<Gate>> = Mutex::new(Vec::new());
    /// Pauses before one chunk read, by file name then offset.
    static BEFORE_CHUNK: Mutex<Vec<ChunkGate>> = Mutex::new(Vec::new());
    /// Every chunk read, by file name: its window offset and length.
    static CHUNKS: Mutex<Vec<(OsString, u64, usize)>> = Mutex::new(Vec::new());
    /// Every file open, by file name: entries the listing filtered out
    /// never appear here.
    static OPENS: Mutex<Vec<OsString>> = Mutex::new(Vec::new());

    /// Holds an armed pause; dropping it disarms the target, so a test
    /// that fails before releasing leaves no gate behind.
    pub(crate) struct Pause {
        /// Which gate list holds the pause.
        before: bool,
        /// After-dir pauses share the list marker.
        after: bool,
        /// The target the pause was armed for.
        target: OsString,
    }

    impl Drop for Pause {
        fn drop(&mut self) {
            let list = if self.after {
                &AFTER_DIR
            } else if self.before {
                &BEFORE_DIR
            } else {
                &BEFORE_FILE
            };
            support::lock(list).retain(|gate| gate.target != self.target);
        }
    }

    /// Arms a pause: the arrival signals the scan through, the release lets
    /// it go on, and the guard disarms the target when it drops.
    fn arm(
        list: &Mutex<Vec<Gate>>,
        before: bool,
        after: bool,
        target: OsString,
    ) -> (mpsc::Receiver<()>, mpsc::Sender<()>, Pause) {
        let (arrived_tx, arrived_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        support::lock(list).push(Gate {
            target: target.clone(),
            arrived: arrived_tx,
            release: release_rx,
        });
        (
            arrived_rx,
            release_tx,
            Pause {
                before,
                after,
                target,
            },
        )
    }

    /// Pauses the next open of the directory `target` before it opens.
    pub(crate) fn arm_before_dir(target: &Path) -> (mpsc::Receiver<()>, mpsc::Sender<()>, Pause) {
        arm(
            &BEFORE_DIR,
            true,
            false,
            OsStr::from_bytes(target.as_os_str().as_bytes()).to_os_string(),
        )
    }

    /// Pauses the next open of the directory `target` after it opens.
    pub(crate) fn arm_after_dir(target: &Path) -> (mpsc::Receiver<()>, mpsc::Sender<()>, Pause) {
        arm(
            &AFTER_DIR,
            false,
            true,
            OsStr::from_bytes(target.as_os_str().as_bytes()).to_os_string(),
        )
    }

    /// Pauses the next open of the file `name` before it opens.
    pub(crate) fn arm_before_file(name: &str) -> (mpsc::Receiver<()>, mpsc::Sender<()>, Pause) {
        arm(&BEFORE_FILE, false, false, OsString::from(name))
    }

    /// One chunk pause: the file and offset whose read waits.
    struct ChunkGate {
        /// The file whose chunk waits.
        name: OsString,
        /// The window offset whose read waits.
        offset: u64,
        /// Tells the test the scan is waiting.
        arrived: mpsc::Sender<()>,
        /// The test releases the scan through it.
        release: mpsc::Receiver<()>,
    }

    /// Holds an armed chunk pause; dropping it disarms the chunk.
    pub(crate) struct ChunkPause {
        /// The file the pause was armed for.
        name: OsString,
        /// The offset the pause was armed for.
        offset: u64,
    }

    impl Drop for ChunkPause {
        fn drop(&mut self) {
            support::lock(&BEFORE_CHUNK)
                .retain(|gate| gate.name != self.name || gate.offset != self.offset);
        }
    }

    /// Pauses the next read of `name` at window `offset`, once.
    pub(crate) fn arm_before_chunk(
        name: &str,
        offset: u64,
    ) -> (mpsc::Receiver<()>, mpsc::Sender<()>, ChunkPause) {
        let (arrived_tx, arrived_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        support::lock(&BEFORE_CHUNK).push(ChunkGate {
            name: OsString::from(name),
            offset,
            arrived: arrived_tx,
            release: release_rx,
        });
        (
            arrived_rx,
            release_tx,
            ChunkPause {
                name: OsString::from(name),
                offset,
            },
        )
    }

    /// A chunk read waits here when armed for its file and offset.
    pub(crate) fn fire_before_chunk(name: &OsStr, offset: u64) {
        let gate = {
            let mut gates = support::lock(&BEFORE_CHUNK);
            gates
                .iter()
                .position(|gate| gate.name == name && gate.offset == offset)
                .map(|at| gates.remove(at))
        };
        let Some(gate) = gate else { return };
        gate.arrived.send(()).unwrap_or(());
        gate.release.recv().unwrap_or(());
    }

    /// Runs an armed pause for `target`, once: an armed step signals its
    /// arrival and waits for the test's release; any other step returns
    /// at once.
    fn fire(list: &Mutex<Vec<Gate>>, target: &OsStr) {
        let gate = {
            let mut gates = support::lock(list);
            gates
                .iter()
                .position(|gate| gate.target == target)
                .map(|at| gates.remove(at))
        };
        let Some(gate) = gate else { return };
        gate.arrived.send(()).unwrap_or(());
        gate.release.recv().unwrap_or(());
    }

    /// The directory's open waits here first.
    pub(crate) fn fire_before_dir_open(dir: &Path) {
        fire(&BEFORE_DIR, dir.as_os_str());
    }

    /// The directory's listing waits here, with the descriptor open.
    pub(crate) fn fire_after_dir_open(dir: &Path) {
        fire(&AFTER_DIR, dir.as_os_str());
    }

    /// A file's open waits here.
    pub(crate) fn fire_before_file_open(name: &OsStr) {
        support::lock(&OPENS).push(name.to_owned());
        fire(&BEFORE_FILE, name);
    }

    /// Marks one chunk read: the file, its window offset and its length.
    /// Called only after the descriptor proved regular.
    pub(crate) fn note_chunk(name: &OsStr, offset: u64, len: usize) {
        support::lock(&CHUNKS).push((name.to_os_string(), offset, len));
    }

    /// Drains every recorded file open, by file name.
    pub(crate) fn take_opens() -> Vec<OsString> {
        std::mem::take(&mut *support::lock(&OPENS))
    }

    /// Drains the recorded chunk reads for one file name, leaving other
    /// files' entries for the tests that own them: a global drain would
    /// steal entries from a test scanning in parallel.
    pub(crate) fn take_chunks_for(name: &str) -> Vec<(u64, usize)> {
        let want = OsStr::new(name);
        let mut chunks = support::lock(&CHUNKS);
        let mut kept = Vec::new();
        let mut found = Vec::new();
        for (entry, offset, len) in chunks.drain(..) {
            if entry.as_os_str() == want {
                found.push((offset, len));
            } else {
                kept.push((entry, offset, len));
            }
        }
        *chunks = kept;
        found
    }
}
