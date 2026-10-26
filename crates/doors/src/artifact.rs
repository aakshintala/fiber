//! `artifact_page` (`docs/invocation.md`, "Driver commands"): one page of
//! a file under the session's `artifacts/`, answered like `history` on the
//! reader thread through one bounded read, so a file is never loaded whole.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

use contract::commands::ArtifactPageArgs;
use contract::events::CommandResult;
use contract::{CommandId, ErrorCode};

use crate::client::{Conn, ENDED, accept, reject};

/// At most this many lines in one page: the page size of history paging
/// (`docs/tui.md`, "History and paging").
const LINES: u64 = 64;
/// At most this many bytes in one page.
const BYTES: u64 = 64 * 1024;
/// How far past the page the read looks, so a cut never splits a
/// character and an offset inside one is refused.
const OVERLAP: u64 = 3;

/// One page of an artifact: whole lines from the requested offset.
pub(crate) struct Page {
    /// The page's text: `String::from_utf8_lossy` of the page's bytes, so
    /// bytes that are not valid UTF-8 become U+FFFD while `next` and
    /// `size` stay byte counts.
    pub(crate) text: String,
    /// The byte offset in the file where each line of `text` starts:
    /// computed from the page's raw bytes before the lossy decode, plus
    /// the request offset, so every entry is an absolute file offset and
    /// invalid bytes cannot shift the mapping. At most 64 entries.
    pub(crate) lines: Vec<u64>,
    /// The byte offset after the page, absent at the end of the file. It
    /// is never an offset the next call rejects.
    pub(crate) next: Option<u64>,
    /// The file's size in bytes when it was opened: a file that grew is
    /// paged only to this size, and the next call sees the new size.
    pub(crate) size: u64,
}

/// Why a page could not be read.
#[derive(Debug)]
pub(crate) enum PageError {
    /// `offset` is past the end of the file.
    Past,
    /// `offset` is inside a multi-byte character.
    Inside,
    /// The file could not be stated or read: it shrank under the read, or
    /// the file system refused. Never a short successful page.
    Io(io::Error),
}

/// One page of the open `file` from `offset`: at most [`LINES`] lines and
/// [`BYTES`] bytes, a longer line cut at a character boundary. `size` is
/// the handle's length at open, and the read never passes it.
pub(crate) fn page(file: File, offset: u64) -> Result<Page, PageError> {
    let size = file
        .metadata()
        .map(|meta| meta.len())
        .map_err(PageError::Io)?;
    page_at(file, offset, size, None)
}

/// [`page`] with the captured `size`, calling `between` after the capture
/// and before the read: tests grow or shrink the file there. Not a
/// `#[cfg(test)]` hook, so production and tests share the whole path.
fn page_at(
    mut file: File,
    offset: u64,
    size: u64,
    between: Option<&dyn Fn()>,
) -> Result<Page, PageError> {
    // Past the end first: `checked_sub` leaves `None` there, while an
    // offset at the end answers an empty page below.
    let rest = size.checked_sub(offset).ok_or(PageError::Past)?;
    let seek = offset.saturating_sub(OVERLAP);
    let read_end = offset
        .checked_add(BYTES)
        .and_then(|capped| capped.checked_add(OVERLAP))
        .map_or(size, |capped| capped.min(size));
    if let Some(between) = between {
        between();
    }
    // The view covers `[seek, read_end)`: the bytes before `offset`
    // decide whether it lies inside a character, the bytes past the cap
    // decide where a cut lands. A short view means the file shrank under
    // the read, which is `Io`, never a short successful page.
    let span = read_end.saturating_sub(seek);
    let len = usize::try_from(span).map_err(|_| too_big())?;
    let mut view = vec![0u8; len];
    file.seek(SeekFrom::Start(seek)).map_err(PageError::Io)?;
    file.read_exact(&mut view).map_err(PageError::Io)?;
    if is_inside(&view, seek, size, offset) {
        return Err(PageError::Inside);
    }
    let at = offset.saturating_sub(seek);
    let at_usize = usize::try_from(at).map_err(|_| too_big())?;
    let Some(tail) = view.get(at_usize..) else {
        return Err(too_big());
    };
    let cap = rest.min(BYTES);
    let cap_usize = usize::try_from(cap).map_err(|_| too_big())?;
    let width = tail.len().min(cap_usize);
    let Some(region) = tail.get(..width) else {
        return Err(too_big());
    };
    // Whether the file runs past the region: only then can a line need
    // the next page.
    let continues = offset.checked_add(cap).is_some_and(|edge| edge != size);
    // Whole lines while the count is below the line cap; a line running
    // past the byte cap starts the next page instead, unless it begins
    // this one, when it is cut at a character boundary.
    let mut taken_bytes = 0u64;
    let mut taken_lines = 0u64;
    let mut end = 0u64;
    for line in region.split_inclusive(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if LINES.saturating_sub(taken_lines) == 0 {
            break;
        }
        let complete = line.last().is_some_and(|byte| *byte == b'\n');
        if !complete && continues {
            if taken_bytes != 0 {
                break;
            }
            end = cut(&view, seek, size, offset, cap);
            break;
        }
        let Some(len) = u64::try_from(line.len()).ok() else {
            return Err(too_big());
        };
        taken_lines += 1;
        taken_bytes += len;
        end = taken_bytes;
    }
    let end_usize = usize::try_from(end).map_err(|_| too_big())?;
    let Some(bytes) = region.get(..end_usize) else {
        return Err(too_big());
    };
    let mut lines = Vec::new();
    let mut start = offset;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        lines.push(start);
        let Some(len) = u64::try_from(line.len()).ok() else {
            return Err(too_big());
        };
        let Some(at) = start.checked_add(len) else {
            return Err(too_big());
        };
        start = at;
    }
    let text = String::from_utf8_lossy(bytes).into_owned();
    let Some(edge) = offset.checked_add(end) else {
        return Err(too_big());
    };
    // `next` is absent exactly at the end of the file.
    let next = if edge == size { None } else { Some(edge) };
    Ok(Page {
        text,
        lines,
        next,
        size,
    })
}

/// The page's end when its first line runs past the byte cap: the cap
/// backed up to a character boundary, so the page after this one starts
/// on one. Backing up moves at most one character, since a lead byte is
/// never inside one.
fn cut(view: &[u8], seek: u64, size: u64, offset: u64, cap: u64) -> u64 {
    let mut end = cap;
    while end != 0 {
        let Some(at) = offset.checked_add(end) else {
            break;
        };
        if !is_inside(view, seek, size, at) {
            break;
        }
        let Some(prior) = end.checked_sub(1) else {
            break;
        };
        end = prior;
    }
    end
}

/// Whether byte `i` lies inside a multi-byte character: let `j` be the
/// nearest byte at or before `i` that is not a continuation byte
/// (`0b10xxxxxx`), with `i - j <= 3`; byte `i` is inside one exactly when
/// byte `j` starts a character of length `L` covering `i`, every byte of
/// it is still in the file, and those bytes are valid UTF-8. Anything
/// else, a stray byte or a malformed sequence, is not inside, so `next`
/// is never an offset the next call rejects.
fn is_inside(view: &[u8], seek: u64, size: u64, i: u64) -> bool {
    let mut j = i;
    let lead = loop {
        let byte = j
            .checked_sub(seek)
            .and_then(|at| usize::try_from(at).ok())
            .and_then(|at| view.get(at).copied());
        let Some(byte) = byte else {
            return false;
        };
        if !is_continuation(byte) {
            break byte;
        }
        if j == seek {
            return false;
        }
        let Some(prior) = j.checked_sub(1) else {
            return false;
        };
        j = prior;
    };
    let gap = i.saturating_sub(j);
    if gap == 0 || gap > 3 {
        return false;
    }
    let Some(len) = lead_len(lead) else {
        return false;
    };
    if gap >= len {
        return false;
    }
    let Some(stop) = j.checked_add(len) else {
        return false;
    };
    if stop > size {
        return false;
    }
    let Some(from) = j.checked_sub(seek).and_then(|at| usize::try_from(at).ok()) else {
        return false;
    };
    let Some(len) = usize::try_from(len).ok() else {
        return false;
    };
    let Some(slice) = view.get(from..).and_then(|tail| tail.get(..len)) else {
        return false;
    };
    std::str::from_utf8(slice).is_ok()
}

/// Whether `byte` continues a multi-byte character.
fn is_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

/// The length a lead byte starts, if it starts a character.
fn lead_len(byte: u8) -> Option<u64> {
    match byte {
        0xC0..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF7 => Some(4),
        _ => None,
    }
}

/// Larger than memory on every platform Fiber builds for: unreachable,
/// carried as `Io` so the shape stays total with no wildcard arm.
fn too_big() -> PageError {
    PageError::Io(io::Error::other("the page does not fit memory"))
}

/// `artifact_page`: one page of a file under the session's `artifacts/`.
/// Runs on the reader thread, as `history` does: one bounded read, and
/// the log is dropped before it, so this connection does not hold the
/// session lock.
pub(crate) fn run(conn: &mut Conn, id: CommandId, args: ArtifactPageArgs) {
    let Some(log) = conn.gate.log.upgrade() else {
        reject(conn, Some(id), ErrorCode::Closing, ENDED);
        return;
    };
    let dir = log.dir().to_path_buf();
    drop(log);
    let path = args.path.as_str();
    let file = match log::open_artifact(&dir, path) {
        Ok(file) => file,
        Err(refusal) => {
            let (code, message) = refused(path, &refusal);
            reject(conn, Some(id), code, &message);
            return;
        }
    };
    match page(file, args.offset) {
        Ok(found) => accept(
            conn,
            id,
            Some(CommandResult::ArtifactPage {
                text: found.text,
                lines: found.lines,
                next: found.next,
                size: found.size,
            }),
        ),
        Err(PageError::Past) => reject(
            conn,
            Some(id),
            ErrorCode::InvalidArguments,
            "`offset` is past the end of the file.",
        ),
        Err(PageError::Inside) => reject(
            conn,
            Some(id),
            ErrorCode::InvalidArguments,
            "`offset` is inside a character.",
        ),
        Err(PageError::Io(error)) => reject(
            conn,
            Some(id),
            ErrorCode::IoFailed,
            &format!("`{path}` could not be read: {error}."),
        ),
    }
}

/// The code comes from the shared refusal; the wording mirrors hub's,
/// over the session socket, which names no session.
fn refused(path: &str, refusal: &log::Refusal) -> (ErrorCode, String) {
    let code = refusal.code();
    let message = match refusal {
        log::Refusal::Outside => {
            format!("`{path}` is not a file under the session's `artifacts/`.")
        }
        log::Refusal::NotRegular => format!("`{path}` is not a regular file."),
        log::Refusal::NoArtifacts | log::Refusal::Missing => {
            format!("No file `{path}` in this session.")
        }
        log::Refusal::RootUnreadable(error) | log::Refusal::Unreadable(error) => {
            format!("`{path}` could not be read: {error}.")
        }
    };
    (code, message)
}

#[cfg(test)]
#[path = "artifact_tests.rs"]
mod tests;
