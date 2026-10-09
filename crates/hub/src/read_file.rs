//! `read_file` (`docs/invocation.md`, "A session's files"): one file under a
//! session's `artifacts/`, answered with its bytes in base64 and its media
//! type, so a client renders it over the connection it already
//! authenticated. Nothing outside `artifacts/` is answered.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use contract::{ErrorCode, SessionId};
use serde_json::{Map, Value};

use crate::feed::{Refusal, invalid};
use crate::relay::valid_session_id;
use crate::resume::find_log;

/// The largest file `read_file` answers: 10 MiB.
const LIMIT: u64 = 10 * 1024 * 1024;

/// `read_file`: one file under a session's `artifacts/`, `path` relative
/// to the session directory. The answer is always `Some`: the file's bytes
/// in standard base64 under `data`, and its media type under `mime_type`.
pub(crate) fn read_file(home: &Path, args: &Map<String, Value>) -> Result<Option<Value>, Refusal> {
    let (session, path) = parse(args).ok_or_else(invalid)?;
    if !fits(path) {
        return Err(outside(path));
    }
    let file = resolve(home, &session, path)?;
    let bytes = read_bounded(file, path)?;
    let mut result = Map::new();
    result.insert("data".to_owned(), Value::String(STANDARD.encode(&bytes)));
    result.insert(
        "mime_type".to_owned(),
        Value::String(media_type(Path::new(path)).to_owned()),
    );
    Ok(Some(Value::Object(result)))
}

/// `session` and `path`, both strings and nothing else. `session` has the
/// minted shape, since it is joined into a path.
fn parse(args: &Map<String, Value>) -> Option<(SessionId, &str)> {
    if args.keys().any(|key| key != "session" && key != "path") {
        return None;
    }
    let session = args.get("session")?.as_str()?;
    if !valid_session_id(session) {
        return None;
    }
    let path = args.get("path")?.as_str()?;
    Some((SessionId(session.to_owned()), path))
}

/// Whether the client's `path` keeps the lexical rule: no NUL byte, the
/// first component is `artifacts`, at least one component follows it, and
/// counting `Normal` as one level down and `ParentDir` as one up from
/// `artifacts/`, the count never goes below zero. It only validates: the
/// original string is what [`resolve`] resolves, so a remote client that
/// cannot plant links learns nothing about paths outside `artifacts/` from
/// which error it gets.
fn fits(path: &str) -> bool {
    if path.contains('\0') {
        return false;
    }
    let mut components = Path::new(path).components();
    let Some(Component::Normal(first)) = components.next() else {
        return false;
    };
    if first != OsStr::new("artifacts") {
        return false;
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
                    return false;
                }
                depth -= 1;
                rest = true;
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    rest
}

/// The session's log, its real `artifacts/`, then the client's original
/// `path` resolved from the session directory, checked inside and a
/// regular file, and opened. `path` also appears in messages.
fn resolve(home: &Path, session: &SessionId, path: &str) -> Result<File, Refusal> {
    let dir = find_log(home, session).and_then(|log| log.parent().map(Path::to_path_buf));
    let Some(dir) = dir else {
        let refused = crate::resume::not_found(session);
        return Err((refused.code, refused.message));
    };
    let home_real = fs::canonicalize(home).map_err(|error| unreadable(path, &error))?;
    let root = dir.join("artifacts");
    let root_real = match fs::canonicalize(&root) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(missing(session, path));
        }
        Err(error) => return Err(unreadable(path, &error)),
    };
    // Only a real directory at its own place: a link at any level above
    // the file, or `artifacts` itself a link or a file, is refused like a
    // path outside. Both sides start from the canonical Fiber home, so a
    // home reached through a link still matches.
    let placed = root
        .strip_prefix(home)
        .ok()
        .is_some_and(|relative| root_real == home_real.join(relative))
        && root_real.is_dir();
    if !placed {
        return Err(outside(path));
    }
    let meta = match fs::metadata(dir.join(path)) {
        // The client's path is stated before it is resolved: stating it
        // answers `not_found` for a missing file and for a file named as
        // a directory (`a.png/`, which `canonicalize` resolves to the
        // file on macOS while stating or opening it fails
        // `NotADirectory` there as on Linux), on every platform
        // (`docs/invocation.md`, "A session's files"). A later
        // `canonicalize` or `open` of the same path can only fail on a
        // rename in between, so every error from either is `io_failed`.
        Ok(meta) => meta,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || error.kind() == io::ErrorKind::NotADirectory =>
        {
            return Err(missing(session, path));
        }
        Err(error) => return Err(unreadable(path, &error)),
    };
    let file_real = fs::canonicalize(dir.join(path)).map_err(|error| unreadable(path, &error))?;
    // Resolving, checking and opening are separate steps, so a process on
    // the person's own account that renames files inside the session
    // between them can have a file outside `artifacts/` answered, or hold
    // this connection's thread on a FIFO swapped in after the type check.
    // Only that account can write there, and it already has a shell
    // (`docs/invocation.md`, "Remote clients"), so this is accepted.
    if !file_real.starts_with(&root_real) {
        return Err(outside(path));
    }
    if !meta.is_file() {
        return Err(not_regular(path));
    }
    File::open(&file_real).map_err(|error| unreadable(path, &error))
}

/// At most `LIMIT` bytes of the opened file, or `too_large`. The file's
/// length is not trusted: the read is bounded, and a read error is
/// `io_failed`, never a partial answer.
fn read_bounded(file: File, shown: &str) -> Result<Vec<u8>, Refusal> {
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| unreadable(shown, &error))?;
    let limit = match usize::try_from(LIMIT) {
        Ok(limit) => limit,
        Err(_) => usize::MAX,
    };
    if bytes.len() > limit {
        return Err((
            ErrorCode::TooLarge,
            format!("`{shown}` is larger than 10 MiB."),
        ));
    }
    Ok(bytes)
}

/// The media type for a file name, by its extension, ASCII
/// case-insensitive. The requested name decides, never the resolved one.
/// A name Fiber does not know is `application/octet-stream`.
fn media_type(name: &Path) -> &'static str {
    let extension = name
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("html" | "htm") => "text/html",
        Some("txt" | "log") => "text/plain",
        Some("md") => "text/markdown",
        Some("json") => "application/json",
        Some("csv") => "text/csv",
        Some(_) | None => "application/octet-stream",
    }
}

/// `` `<path>` is not a file under the session's `artifacts/`. ``
fn outside(path: &str) -> Refusal {
    (
        ErrorCode::InvalidArguments,
        format!("`{path}` is not a file under the session's `artifacts/`."),
    )
}

/// `` `<path>` is not a regular file. ``
fn not_regular(path: &str) -> Refusal {
    (
        ErrorCode::InvalidArguments,
        format!("`{path}` is not a regular file."),
    )
}

/// `` No file `<path>` in session `<id>`. ``
fn missing(session: &SessionId, path: &str) -> Refusal {
    (
        ErrorCode::NotFound,
        format!("No file `{path}` in session `{}`.", session.0),
    )
}

/// `` `<path>` could not be read: <error>. `` The error names no path, so
/// it is safe to carry.
fn unreadable(path: &str, error: &io::Error) -> Refusal {
    (
        ErrorCode::IoFailed,
        format!("`{path}` could not be read: {error}."),
    )
}

#[cfg(test)]
#[path = "read_file_tests.rs"]
mod tests;
