//! Fiber's own unpacker for the release's docs and extensions archives
//! (`docs/releasing.md`, "Installing"): gzip through `flate2`, then plain
//! ustar, where one parser decides what is allowed and writes it. It runs
//! no `gzip` and no `tar`. A member is a file, a directory or a relative
//! symlink that stays inside its directory, and nothing else is written.
//! Every path is walked with `symlink_metadata` and no link is followed, so
//! nothing lands outside `into`.

use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, ErrorKind, Read, Take, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use flate2::bufread::GzDecoder;

use crate::Error;

/// How much one archive may hold once decompressed.
pub(crate) struct Limits {
    /// Decoded bytes in all, the padding after the end blocks included.
    pub(crate) bytes: u64,
    /// Headers read.
    pub(crate) members: usize,
}

/// The release archives' limits.
pub(crate) const LIMITS: Limits = Limits {
    bytes: 256 * 1024 * 1024,
    members: 100_000,
};

const BLOCK: usize = 512;

/// Why an unpack stopped: a refusal of the archive, or a failed write.
enum Fail {
    Bad(String),
    Io(PathBuf, io::Error),
}

/// Decompresses the gzip `archive` and writes its plain-ustar members under
/// `into`, an empty directory the caller made. A refusal is
/// `Error::BadArchive { archive: name, why }` naming the member; a failed
/// write under `into` is `Error::Io`.
pub(crate) fn unpack(
    archive: &[u8],
    into: &Path,
    name: &str,
    limits: &Limits,
) -> Result<(), Error> {
    let mut decoder = GzDecoder::new(archive);
    let result = members(&mut decoder, into, limits).and_then(|()| {
        let rest = decoder.into_inner().len();
        if rest > 0 {
            return Err(Fail::Bad(format!("{rest} bytes follow the gzip stream")));
        }
        Ok(())
    });
    result.map_err(|fail| match fail {
        Fail::Bad(why) => Error::BadArchive {
            archive: name.into(),
            why,
        },
        Fail::Io(path, source) => Error::Io { path, source },
    })
}

/// The decoded stream, capped one byte past `limits.bytes` so going over
/// is seen rather than read as an early end.
struct Stream<R> {
    take: Take<R>,
    bytes: u64,
}

impl<R: Read> Stream<R> {
    /// Fills `buf` as far as the stream goes; fewer bytes means it ended.
    fn fill(&mut self, buf: &mut [u8]) -> Result<usize, Fail> {
        let mut got = 0;
        while let Some(rest) = buf.get_mut(got..)
            && !rest.is_empty()
        {
            let n = self
                .take
                .read(rest)
                .map_err(|e| Fail::Bad(format!("is not a valid gzip stream: {e}")))?;
            if self.take.limit() == 0 {
                return Err(Fail::Bad(format!(
                    "holds more than {} bytes uncompressed",
                    self.bytes
                )));
            }
            if n == 0 {
                break;
            }
            got += n;
        }
        Ok(got)
    }

    /// The next 512-byte block, or `None` at the stream's end.
    fn block(&mut self) -> Result<Option<[u8; BLOCK]>, Fail> {
        let mut block = [0u8; BLOCK];
        match self.fill(&mut block)? {
            0 => Ok(None),
            BLOCK => Ok(Some(block)),
            _ => Err(Fail::Bad("the archive ends inside a header".into())),
        }
    }
}

fn members(decoder: &mut GzDecoder<&[u8]>, into: &Path, limits: &Limits) -> Result<(), Fail> {
    let mut stream = Stream {
        take: decoder.take(limits.bytes.saturating_add(1)),
        bytes: limits.bytes,
    };
    let ended = || Fail::Bad("the archive ends before its two end blocks".into());
    let mut count = 0;
    loop {
        let block = stream.block()?.ok_or_else(ended)?;
        if zero(&block) {
            let next = stream.block()?.ok_or_else(ended)?;
            if !zero(&next) {
                return Err(Fail::Bad("data follows the first end block".into()));
            }
            break;
        }
        count += 1;
        if count > limits.members {
            return Err(Fail::Bad(format!(
                "holds more than {} members",
                limits.members
            )));
        }
        let member = header(&block).map_err(Fail::Bad)?;
        write(&mut stream, into, &member)?;
    }
    drain(&mut stream)
}

/// Reads on to the decoder's end, so the gzip trailer's CRC32 and length
/// are checked. Only zero padding may follow the end blocks.
fn drain(stream: &mut Stream<impl Read>) -> Result<(), Fail> {
    let mut buf = [0u8; 8 * BLOCK];
    loop {
        let n = stream.fill(&mut buf)?;
        if !zero(buf.get(..n).unwrap_or_default()) {
            return Err(Fail::Bad("data follows the end blocks".into()));
        }
        if n < buf.len() {
            return Ok(());
        }
    }
}

fn zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|b| *b == 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Dir,
    Symlink,
}

/// One member's header, checked.
struct Header {
    /// The name as the archive spells it, for messages.
    shown: String,
    /// Its components under `into`; empty for the root.
    path: Vec<String>,
    kind: Kind,
    size: u64,
    mode: u32,
    link: String,
}

/// The `width` bytes of `block` at `at`.
fn field(block: &[u8; BLOCK], at: usize, width: usize) -> &[u8] {
    block.get(at..at + width).unwrap_or_default()
}

/// A text field: up to its first NUL, or its full width.
fn text(block: &[u8; BLOCK], at: usize, width: usize) -> Result<&str, ()> {
    let bytes = field(block, at, width);
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    std::str::from_utf8(bytes.get(..end).unwrap_or_default()).map_err(|_| ())
}

/// A numeric field: octal digits, optionally ending in spaces or NULs.
fn octal(bytes: &[u8]) -> Option<u64> {
    let end = bytes
        .iter()
        .rposition(|b| *b != 0 && *b != b' ')
        .map_or(0, |i| i + 1);
    let digits = bytes.get(..end)?;
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0u64, |n, d| match d {
        b'0'..=b'7' => n.checked_mul(8)?.checked_add(u64::from(d - b'0')),
        _ => None,
    })
}

/// Checks the magic, the checksum, the fields, the type and the mode.
fn header(block: &[u8; BLOCK]) -> Result<Header, String> {
    let raw = String::from_utf8_lossy(field(block, 0, 100)).replace('\0', "");
    let fail = |why: &str| format!("`{raw}` {why}");
    if field(block, 257, 6) != b"ustar\0" || field(block, 263, 2) != b"00" {
        return Err(fail("has no ustar magic and version 00"));
    }
    let sum: u64 = block
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if (148..156).contains(&i) {
                u64::from(b' ')
            } else {
                u64::from(*b)
            }
        })
        .sum();
    if octal(field(block, 148, 8)) != Some(sum) {
        return Err(fail("has a header checksum that does not match"));
    }
    let not_utf8 = |_| fail("has a name that is not UTF-8");
    let name = text(block, 0, 100).map_err(not_utf8)?;
    let prefix = text(block, 345, 155).map_err(not_utf8)?;
    let link = text(block, 157, 100).map_err(not_utf8)?;
    let shown = if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    };
    let fail = |why: &str| format!("`{shown}` {why}");
    let size = octal(field(block, 124, 12)).ok_or_else(|| fail("has a size that is not octal"))?;
    let mode = octal(field(block, 100, 8))
        .and_then(|m| u32::try_from(m).ok())
        .ok_or_else(|| fail("has a mode that is not octal"))?;
    let kind = match block.get(156).copied().unwrap_or_default() {
        b'0' | 0 => Kind::File,
        b'5' => Kind::Dir,
        b'2' => Kind::Symlink,
        other => {
            return Err(fail(&format!(
                "has type `{}`, which is not a file, a directory or a symlink",
                char::from(other)
            )));
        }
    };
    let path = components(&shown).map_err(|why| fail(&why))?;
    if path.is_empty() && kind != Kind::Dir {
        return Err(fail("has an empty name"));
    }
    if kind != Kind::File && size != 0 {
        return Err(fail("is a directory or symlink with a size"));
    }
    if kind == Kind::Dir && mode & 0o700 != 0o700 {
        return Err(fail("is a directory without owner rwx"));
    }
    if kind == Kind::File && mode & 0o600 != 0o600 {
        return Err(fail("is a file without owner rw"));
    }
    if kind == Kind::Symlink {
        if link.is_empty() {
            return Err(fail("has an empty link target"));
        }
        if link.starts_with('/') {
            return Err(fail("has an absolute link target"));
        }
        if link.split('/').any(|c| c == "..") {
            return Err(fail("has a link target with `..`"));
        }
    }
    Ok(Header {
        shown,
        path,
        kind,
        size,
        mode,
        link: link.to_owned(),
    })
}

/// A name's components: empty and `.` ones dropped; an absolute name or a
/// `..` component refused.
fn components(name: &str) -> Result<Vec<String>, String> {
    if name.starts_with('/') {
        return Err("is absolute".into());
    }
    let mut out = Vec::new();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err("has a `..` component".into()),
            part => out.push(part.to_owned()),
        }
    }
    Ok(out)
}

/// Walks `path`'s parents from `into`, making each one that is missing,
/// and returns where the member itself goes. A parent that is a symlink,
/// a file or anything else but a directory is refused, never followed.
fn parent(into: &Path, member: &Header) -> Result<PathBuf, Fail> {
    let mut at = into.to_path_buf();
    let Some((last, parents)) = member.path.split_last() else {
        return Ok(at);
    };
    for part in parents {
        at.push(part);
        match fs::symlink_metadata(&at) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(Fail::Bad(format!(
                    "`{}` passes through `{}`, which is not a directory",
                    member.shown,
                    at.strip_prefix(into).unwrap_or(&at).display()
                )));
            }
            Err(e) if e.kind() == ErrorKind::NotFound => make_dir(&at)?,
            Err(e) => return Err(Fail::Io(at, e)),
        }
    }
    at.push(last);
    Ok(at)
}

fn make_dir(path: &Path) -> Result<(), Fail> {
    fs::create_dir(path)
        .and_then(|()| fs::set_permissions(path, Permissions::from_mode(0o700)))
        .map_err(|e| Fail::Io(path.to_path_buf(), e))
}

/// Writes one member. Its mode is set after creation, so neither the umask
/// nor the archive decides it.
fn write(stream: &mut Stream<impl Read>, into: &Path, member: &Header) -> Result<(), Fail> {
    let path = parent(into, member)?;
    let twice = || Fail::Bad(format!("`{}` is in the archive twice", member.shown));
    let failed = |e: io::Error| {
        if e.kind() == ErrorKind::AlreadyExists {
            twice()
        } else {
            Fail::Io(path.clone(), e)
        }
    };
    match member.kind {
        Kind::Dir => match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => Ok(()),
            Ok(_) => Err(twice()),
            Err(e) if e.kind() == ErrorKind::NotFound => make_dir(&path),
            Err(e) => Err(Fail::Io(path, e)),
        },
        Kind::Symlink => symlink(&member.link, &path).map_err(failed),
        Kind::File => {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(failed)?;
            let mode = if member.mode & 0o100 == 0 {
                0o600
            } else {
                0o700
            };
            file.set_permissions(Permissions::from_mode(mode))
                .map_err(|e| Fail::Io(path.clone(), e))?;
            copy(stream, member, &mut file, &path)
        }
    }
}

/// Copies a file's `size` bytes, then skips its padding to the next block.
fn copy(
    stream: &mut Stream<impl Read>,
    member: &Header,
    out: &mut fs::File,
    path: &Path,
) -> Result<(), Fail> {
    let short = || Fail::Bad(format!("the archive ends inside `{}`", member.shown));
    let mut buf = [0u8; 8 * BLOCK];
    let mut left = member.size;
    while left > 0 {
        let want = usize::try_from(left).map_or(buf.len(), |l| l.min(buf.len()));
        let chunk = buf.get_mut(..want).unwrap_or_default();
        if stream.fill(chunk)? < want {
            return Err(short());
        }
        out.write_all(chunk)
            .map_err(|e| Fail::Io(path.to_path_buf(), e))?;
        left -= want as u64;
    }
    let pad = usize::try_from(member.size % BLOCK as u64).map_or(0, |r| (BLOCK - r) % BLOCK);
    let mut skip = [0u8; BLOCK];
    let skip = skip.get_mut(..pad).unwrap_or_default();
    if stream.fill(skip)? < pad {
        return Err(short());
    }
    Ok(())
}

#[cfg(test)]
#[path = "unpack_tests.rs"]
mod tests;
