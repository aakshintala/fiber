//! The shared artifact path check (`docs/invocation.md`, "A session's
//! files"): one function that `read_file` (hub) and `artifact_page`
//! (doors) both call, so the lexical rule and the canonical-inside,
//! regular-file check live in exactly one place (`docs/code-quality.md`,
//! "When more than one mechanism can enforce a rule, the strongest wins").

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use contract::ErrorCode;

use crate::{ARTIFACTS, Error, Log, io_at};

/// Why an artifact could not be opened: what the caller maps to its own
/// rejection codes. It carries no path, so a message built from it leaks
/// nothing the client did not send.
#[derive(Debug, thiserror::Error)]
pub enum Refusal {
    /// There is no `artifacts/` directory in the session.
    #[error("this session keeps no `artifacts` directory")]
    NoArtifacts,
    /// The `artifacts/` directory itself could not be read.
    #[error("the session's `artifacts` directory could not be read: {0}")]
    RootUnreadable(io::Error),
    /// The path names something outside the session's `artifacts/`.
    #[error("not a file under the session's `artifacts`")]
    Outside,
    /// The path names something that is not a regular file.
    #[error("not a regular file")]
    NotRegular,
    /// The path names no file in the session's `artifacts/`.
    #[error("no such file in the session's `artifacts`")]
    Missing,
    /// The file could not be stated, resolved or opened.
    #[error("the file could not be read: {0}")]
    Unreadable(io::Error),
}

impl Refusal {
    /// The stable code a consumer switches on (`docs/errors.md`,
    /// "Registry").
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Outside | Self::NotRegular => ErrorCode::InvalidArguments,
            Self::NoArtifacts | Self::Missing => ErrorCode::NotFound,
            Self::RootUnreadable(_) | Self::Unreadable(_) => ErrorCode::IoFailed,
        }
    }
}

/// Whether the client's `path` keeps the lexical rule: no NUL byte, the
/// first component is `artifacts`, at least one component follows it, and
/// counting `Normal` as one level down and `ParentDir` as one up from
/// `artifacts/`, the count never goes below zero. It only validates: the
/// original string is what [`open_artifact`] resolves, so a remote client
/// that cannot plant links learns nothing about paths outside `artifacts/`
/// from which error it gets.
pub fn check_artifact_path(path: &str) -> Result<(), Refusal> {
    if path.contains('\0') {
        return Err(Refusal::Outside);
    }
    let mut components = Path::new(path).components();
    let Some(Component::Normal(first)) = components.next() else {
        return Err(Refusal::Outside);
    };
    if first != OsStr::new("artifacts") {
        return Err(Refusal::Outside);
    }
    let mut depth = 0u32;
    let mut rest = false;
    for component in components {
        match component {
            Component::Normal(_) => {
                depth += 1;
                rest = true;
            }
            Component::CurDir => {
                rest = true;
            }
            Component::ParentDir => {
                if depth == 0 {
                    return Err(Refusal::Outside);
                }
                depth -= 1;
                rest = true;
            }
            Component::RootDir | Component::Prefix(_) => return Err(Refusal::Outside),
        }
    }
    if rest { Ok(()) } else { Err(Refusal::Outside) }
}

/// The session's real `artifacts/`, then the client's original `path`
/// resolved from the session directory `dir`, checked inside and a regular
/// file, and opened.
pub fn open_artifact(dir: &Path, path: &str) -> Result<File, Refusal> {
    check_artifact_path(path)?;
    let root = dir.join("artifacts");
    let root_real = match fs::canonicalize(&root) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(Refusal::NoArtifacts);
        }
        Err(error) => return Err(Refusal::RootUnreadable(error)),
    };
    let dir_real = fs::canonicalize(dir).map_err(Refusal::RootUnreadable)?;
    // Only a real directory at its own place: `artifacts` itself a link
    // or a file is refused like a path outside.
    if root_real != dir_real.join("artifacts") || !root_real.is_dir() {
        return Err(Refusal::Outside);
    }
    let meta = match fs::metadata(dir.join(path)) {
        // The client's path is stated before it is resolved: stating it
        // answers `Missing` for a missing file and for a file named as
        // a directory (`a.png/`, which `canonicalize` resolves to the
        // file on macOS while stating or opening it fails
        // `NotADirectory` there as on Linux), on every platform. A later
        // `canonicalize` or `open` of the same path can only fail on a
        // rename in between, so every error from either is `Unreadable`.
        Ok(meta) => meta,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || error.kind() == io::ErrorKind::NotADirectory =>
        {
            return Err(Refusal::Missing);
        }
        Err(error) => return Err(Refusal::Unreadable(error)),
    };
    let file_real = fs::canonicalize(dir.join(path)).map_err(Refusal::Unreadable)?;
    // Resolving, checking and opening are separate steps, so a process on
    // the person's own account that renames files inside the session
    // between them can have a file outside `artifacts/` answered, or hold
    // the caller's thread on a FIFO swapped in after the type check.
    // Only that account can write there, and it already has a shell, so
    // this is accepted.
    if !file_real.starts_with(&root_real) {
        return Err(Refusal::Outside);
    }
    if !meta.is_file() {
        return Err(Refusal::NotRegular);
    }
    File::open(&file_real).map_err(Refusal::Unreadable)
}

/// The file name when `relative`, relative to the session directory,
/// names exactly one file directly under the session's `artifacts/`:
/// `None` for a nested path, a traversal, a session file, or an absolute
/// path. The loop classifies a path lexically before calling into `log`;
/// this keeps anything but one direct artifact child from reaching the
/// fallback or the filesystem.
pub(crate) fn direct_artifact_name(relative: &Path) -> Option<String> {
    let mut components = relative.to_str()?.split('/');
    if components.next()? != "artifacts" {
        return None;
    }
    let name = components.next()?;
    if name.is_empty() || matches!(name, "." | "..") || components.next().is_some() {
        return None;
    }
    Some(name.to_owned())
}

/// What one capped output-file read found: the whole file when it fits,
/// else its length, taken from the open handle after the read.
#[derive(Debug)]
pub enum OutputRead {
    /// The whole file, at most `limit` bytes.
    Small(Vec<u8>),
    /// The file did not fit: its length.
    Large {
        /// The file's length.
        size: u64,
    },
}

/// Why the output-file read failed: the path is not a regular file, or
/// the read itself failed. Both are the delivery pass's failure.
#[derive(Debug)]
pub enum Unreadable {
    /// The path is a link, a FIFO, a socket, a directory, or another
    /// non-regular file.
    NotRegular,
    /// The file is missing or unreadable, or the path is not one direct
    /// artifact child.
    Io,
}

impl Log {
    /// The recorded output path resolved for hook reporting: `recorded`
    /// joined to the session directory, absolute as recorded when it is.
    pub fn output_path(&self, recorded: &Path) -> PathBuf {
        self.dir().join(recorded)
    }

    /// Reads at most `limit` + 1 bytes of the artifact `relative`, one
    /// file directly under this log's `artifacts/`. The open follows no
    /// link and never blocks on a FIFO
    /// (`OFlags::NOFOLLOW | OFlags::NONBLOCK`); the `fstat` below covers
    /// the descriptor actually opened, so a path swapped for a link or a
    /// FIFO before the open changes nothing about what is read. A path
    /// that is not one direct artifact child reads as [`Unreadable::Io`],
    /// never reaching the filesystem.
    pub fn read_output(&self, relative: &Path, limit: u64) -> Result<OutputRead, Unreadable> {
        let name = direct_artifact_name(relative).ok_or(Unreadable::Io)?;
        let path = self.dir().join(ARTIFACTS).join(name);
        // No `symlink_metadata` first: the open itself refuses a link, and
        // `NONBLOCK` keeps a FIFO from blocking it. The `fstat` covers the
        // descriptor actually opened, not the path.
        let fd = rustix::fs::open(
            &path,
            rustix::fs::OFlags::RDONLY
                .union(rustix::fs::OFlags::NOFOLLOW)
                .union(rustix::fs::OFlags::NONBLOCK)
                .union(rustix::fs::OFlags::CLOEXEC),
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| {
            // A link refuses with `LOOP`; a socket with `NXIO` on Linux and
            // `OPNOTSUPP` on macOS: none is a regular file, without ever
            // following or blocking on the path. Anything else is a read
            // failure (missing, unreadable).
            if error == rustix::io::Errno::LOOP
                || error == rustix::io::Errno::NXIO
                || error == rustix::io::Errno::OPNOTSUPP
            {
                Unreadable::NotRegular
            } else {
                Unreadable::Io
            }
        })?;
        let opened = rustix::fs::fstat(&fd).map_err(|_| Unreadable::Io)?;
        if rustix::fs::FileType::from_raw_mode(opened.st_mode) != rustix::fs::FileType::RegularFile
        {
            return Err(Unreadable::NotRegular);
        }
        let file = std::fs::File::from(fd);
        let mut capped = file.take(limit.saturating_add(1));
        let mut bytes = Vec::new();
        capped.read_to_end(&mut bytes).map_err(|_| Unreadable::Io)?;
        if bytes.len() as u64 > limit {
            let size = capped
                .get_ref()
                .metadata()
                .map_err(|_| Unreadable::Io)?
                .len()
                .max(bytes.len() as u64);
            return Ok(OutputRead::Large { size });
        }
        Ok(OutputRead::Small(bytes))
    }

    /// Replaces the artifact `relative`, one file directly under this
    /// log's `artifacts/`, with `bytes`, keeping the original's permission
    /// bits: the bytes go to a sibling file first, which is given those
    /// bits and `rename`d over the path (over a link, the link). The
    /// sibling is created exclusively and written through its own handle,
    /// so a pre-existing sibling link never redirects the write; only a
    /// sibling this call created is removed on an error path. A path that
    /// is not one direct artifact child is [`Error::Io`] naming the
    /// resolved path, and nothing is written.
    pub fn replace_output(&self, relative: &Path, bytes: &[u8]) -> Result<(), Error> {
        let name = direct_artifact_name(relative).ok_or_else(|| {
            io_at(&self.dir().join(relative))(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not one file directly under the session's `artifacts/`",
            ))
        })?;
        replace_output_file(&self.dir().join(ARTIFACTS).join(name), bytes)
    }
}

/// Replaces the file at `path` with `bytes`, keeping the original's
/// permission bits, as [`Log::replace_output`] does.
fn replace_output_file(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::Write as _;
    let mut name = path
        .file_name()
        .map(|name| name.to_owned())
        .unwrap_or_default();
    name.push(".fiber-redact");
    let sibling = path.with_file_name(name);
    let permissions = std::fs::metadata(path).map(|stat| stat.permissions()).ok();
    let replaced = (|| -> std::io::Result<()> {
        // A sibling this call did not create (a job could leave it as
        // a link): unlink the name itself without following it, then
        // create exclusively. Removing an absent name fails silently,
        // so the open below reports the real error.
        let _removed = std::fs::remove_file(&sibling);
        let mut sibling_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&sibling)?;
        sibling_file.write_all(bytes)?;
        sibling_file.sync_all()?;
        drop(sibling_file);
        if let Some(permissions) = permissions {
            std::fs::set_permissions(&sibling, permissions)?;
        }
        std::fs::rename(&sibling, path)?;
        Ok(())
    })();
    if replaced.is_err() {
        let _removed = std::fs::remove_file(&sibling);
    }
    replaced.map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
#[path = "artifact_tests.rs"]
mod tests;
