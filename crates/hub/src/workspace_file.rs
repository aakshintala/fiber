//! `read_workspace_file` and `write_workspace_file`
//! (`docs/invocation.md`, "What the hub speaks"): one file under the
//! session's workspace on the hub, answered with its UTF-8 text and its
//! content version, written only while the version still matches.
//!
//! The workspace is the only bound: the path is resolved against it with
//! symbolic links included, and anything resolving outside is refused like
//! a bad shape. Both directions refuse text over 10 MiB; a missing file
//! reads as empty text with version `missing`, and a write at `missing`
//! creates it. The hub's `Machine` and the main `Seam` share this one
//! place: both the read and the write go through [`resolve`], and nothing
//! else resolves a workspace path.

use std::fs::{self, File};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{ErrorKind, Read};
use std::path::{Component, Path, PathBuf};

use contract::ErrorCode;
use contract::shapes::Failure;

use crate::connection::Refused;
use crate::feed::invalid as bad_shape;

/// The largest workspace file answered or stored: 10 MiB, matching
/// `read_file`.
const LIMIT: u64 = 10 * 1024 * 1024;

/// Resolves `path` against `workspace`, one component at a time: a
/// relative path starts from the canonical workspace, an absolute path
/// from the root. An existing component canonicalizes, a missing one
/// extends the path unchecked, and a `..` pops back (`/` never pops).
/// The resolved path must start with the canonical workspace: symbolic
/// links at the parent, the file, or the workspace itself resolve before
/// the check, so an escape through one is refused like a bad shape. A
/// missing tail answers the joined path, and the read below sees it as
/// missing; an existing target that does not resolve (a dangling link)
/// answers the same way.
pub(crate) fn resolve(workspace: &Path, path: &str) -> Result<PathBuf, Refused> {
    if !workspace.is_absolute() {
        return Err(invalid());
    }
    if path.contains('\0') {
        return Err(outside(path));
    }
    let workspace_real = fs::canonicalize(workspace)
        .map_err(|error| unreadable(&workspace.display().to_string(), &error))?;
    if !workspace_real.is_dir() {
        return Err(unreadable(
            &workspace.display().to_string(),
            &std::io::Error::new(ErrorKind::NotADirectory, "the workspace is not a directory"),
        ));
    }
    let relative = !Path::new(path).is_absolute();
    let mut current = if relative {
        workspace_real.clone()
    } else {
        Path::new("/").to_path_buf()
    };
    for component in Path::new(path).components() {
        match component {
            // A prefix or root in a relative path can never appear: the
            // components of one hold neither. An absolute path walks from
            // the root, and the workspace bound below still applies.
            Component::Prefix(_) | Component::RootDir => {
                if relative {
                    return Err(outside(path));
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                current.pop();
            }
            Component::Normal(part) => {
                let next = current.join(part);
                match fs::symlink_metadata(&next) {
                    Ok(_) => match fs::canonicalize(&next) {
                        Ok(real) => current = real,
                        // A dangling link: answered as missing below.
                        Err(_) => current = next,
                    },
                    Err(error) if error.kind() == ErrorKind::NotFound => current = next,
                    Err(error) => return Err(unreadable(path, &error)),
                }
            }
        }
    }
    if !current.starts_with(&workspace_real) {
        return Err(outside(path));
    }
    Ok(current)
}

/// Reads one workspace file: its UTF-8 text with the content-hash
/// version, or empty text with `missing` when the file is not there. At
/// most `LIMIT` bytes are answered; a non-regular file is a bad shape,
/// non-UTF-8 bytes fail, and anything larger is `too_large`.
pub fn read(workspace: &Path, path: &Path) -> Result<(String, String), Failure> {
    read_inner(workspace, path).map_err(into_failure)
}

fn read_inner(workspace: &Path, path: &Path) -> Result<(String, String), Refused> {
    let shown = path.to_str().ok_or_else(invalid)?;
    let resolved = resolve(workspace, shown)?;
    match fs::metadata(&resolved) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok((String::new(), "missing".to_owned()));
        }
        Err(error) => return Err(unreadable(shown, &error)),
        Ok(meta) if !meta.is_file() => return Err(not_regular(shown)),
        Ok(_) => {}
    }
    let bytes = read_bounded(&resolved, shown)?.unwrap_or_default();
    let text = String::from_utf8(bytes).map_err(|error| unreadable(shown, &error))?;
    let version = version_of(text.as_bytes());
    Ok((text, version))
}

/// Writes `text` only while the file still has `version`: a mismatch
/// writes nothing and is `stale_request`. A write at `missing` creates
/// the file with its parents. Text over `LIMIT` bytes is `too_large`;
/// a non-regular target is a bad shape.
pub fn write(workspace: &Path, path: &Path, text: &str, version: &str) -> Result<(), Failure> {
    write_inner(workspace, path, text, version).map_err(into_failure)
}

/// Whether the write target is absent, so a `missing` write creates it.
/// A non-NotFound stat error after a successful resolve fires only when the
/// target changes between the two stats (a TOCTOU rename or chmod by another
/// thread): `resolve` already refuses an unsearchable parent, so no
/// single-threaded test reaches that arm, and the later metadata and
/// bounded-read steps still fail closed.
// Exempt: the non-NotFound error arm changes no behaviour any test observes;
// it fires only in the TOCTOU race above, which no test seam reaches.
#[cfg_attr(false, mutants::skip)]
fn write_target_absent(resolved: &Path, shown: &str) -> Result<bool, Refused> {
    match fs::symlink_metadata(resolved) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(true),
        Err(error) => Err(unreadable(shown, &error)),
        Ok(_) => Ok(false),
    }
}

fn write_inner(workspace: &Path, path: &Path, text: &str, version: &str) -> Result<(), Refused> {
    if text.len() as u64 > LIMIT {
        let shown = path.to_str().unwrap_or("");
        return Err(too_large(shown));
    }
    let shown = path.to_str().ok_or_else(invalid)?;
    let resolved = resolve(workspace, shown)?;
    if write_target_absent(&resolved, shown)? {
        if version != "missing" {
            return Err(stale(shown));
        }
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent).map_err(|error| unreadable(shown, &error))?;
        }
        fs::write(&resolved, text.as_bytes()).map_err(|error| unreadable(shown, &error))?;
        return Ok(());
    }
    // A link swapped in for a directory between `resolve` and this
    // metadata, or a target that never resolves, is not a regular file,
    // so no write follows a link out (`open_artifact`'s accepted TOCTOU
    // class: only the person's own account can rename here, and it
    // already has a shell).
    if !fs::metadata(&resolved)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
    {
        return Err(not_regular(shown));
    }
    let current = match read_bounded(&resolved, shown)? {
        Some(current) => current,
        None => return Err(stale(shown)),
    };
    if version_of(&current) != version {
        return Err(stale(shown));
    }
    fs::write(&resolved, text.as_bytes()).map_err(|error| unreadable(shown, &error))?;
    Ok(())
}

/// At most `LIMIT` bytes of the file, or `too_large`. The file's length
/// is not trusted: the read is bounded. `None` when the file is gone
/// between the metadata check and this read.
fn read_bounded(resolved: &Path, shown: &str) -> Result<Option<Vec<u8>>, Refused> {
    let file = match File::open(resolved) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unreadable(shown, &error)),
    };
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| unreadable(shown, &error))?;
    let limit = match usize::try_from(LIMIT) {
        Ok(limit) => limit,
        Err(_) => usize::MAX,
    };
    if bytes.len() > limit {
        return Err(too_large(shown));
    }
    Ok(Some(bytes))
}

/// The content version: 16-digit lowercase hexadecimal content hash.
/// `DefaultHasher` builds with fixed keys, so the hash compares equal
/// across hub restarts. A missing file answers `missing` separately,
/// where the read and the write name it.
fn version_of(bytes: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The refusal for a bad shape: the arguments do not fit this command.
/// The wire failure a [`Refused`] carries: the seam answers it as-is.
fn into_failure(refused: Refused) -> Failure {
    Failure {
        code: refused.code,
        message: refused.message,
        retry_after_ms: None,
        provider: None,
    }
}

fn invalid() -> Refused {
    bad_shape()
}

/// `` `<path>` is not a file under the workspace. ``
fn outside(path: &str) -> Refused {
    Refused {
        code: ErrorCode::InvalidArguments,
        message: format!("`{path}` is not a file under the workspace."),
    }
}

/// `` `<path>` is not a regular file. ``
fn not_regular(path: &str) -> Refused {
    Refused {
        code: ErrorCode::InvalidArguments,
        message: format!("`{path}` is not a regular file."),
    }
}

/// `` `<path>` changed since it was read. `` Nothing is written.
fn stale(path: &str) -> Refused {
    Refused {
        code: ErrorCode::StaleRequest,
        message: format!("{path} changed since it was read."),
    }
}

/// `` `<path>` is larger than 10 MiB. ``
fn too_large(path: &str) -> Refused {
    Refused {
        code: ErrorCode::TooLarge,
        message: format!("`{path}` is larger than 10 MiB."),
    }
}

/// `` `<path>` could not be read: <error>. `` The error names no path, so
/// it is safe to carry.
fn unreadable(path: &str, error: &impl std::fmt::Display) -> Refused {
    Refused {
        code: ErrorCode::IoFailed,
        message: format!("`{path}` could not be read: {error}."),
    }
}

#[cfg(test)]
#[path = "workspace_file_tests.rs"]
mod tests;
