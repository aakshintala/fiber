//! The shared artifact path check (`docs/invocation.md`, "A session's
//! files"): one function that `read_file` (hub) and `artifact_page`
//! (doors) both call, so the lexical rule and the canonical-inside,
//! regular-file check live in exactly one place (`docs/code-quality.md`,
//! "When more than one mechanism can enforce a rule, the strongest wins").

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::path::{Component, Path};

use contract::ErrorCode;

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

#[cfg(test)]
#[path = "artifact_tests.rs"]
mod tests;
