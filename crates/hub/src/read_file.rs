//! `read_file` (`docs/invocation.md`, "A session's files"): one file under a
//! session's `artifacts/`, answered with its bytes in base64 and its media
//! type, so a client renders it over the connection it already
//! authenticated. Nothing outside `artifacts/` is answered.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use contract::{ErrorCode, SessionId};
use serde_json::{Map, Value};

use crate::connection::Refused;
use crate::feed::invalid;
use crate::relay::valid_session_id;

/// The largest file `read_file` answers: 10 MiB.
const LIMIT: u64 = 10 * 1024 * 1024;

/// `read_file`: one file under a session's `artifacts/`, `path` relative
/// to the session directory. The answer is always `Some`: the file's bytes
/// in standard base64 under `data`, and its media type under `mime_type`.
pub(crate) fn read_file(home: &Path, args: &Map<String, Value>) -> Result<Option<Value>, Refused> {
    let (session, path) = parse(args).ok_or_else(invalid)?;
    // The lexical rule runs before the session is looked up, so an
    // unknown session with a path outside still answers
    // `invalid_arguments`.
    if log::check_artifact_path(path).is_err() {
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

/// The session's log, its real `artifacts/`, then the client's original
/// `path` resolved from the session directory, checked inside and a
/// regular file, and opened. `path` also appears in messages. The
/// artifact check itself lives in [`log`], which this calls; what stays
/// here is the session lookup and the lookup's own guard, that the
/// session directory sits at its own place under the canonical Fiber
/// home, which runs after `log` reports a missing or unreadable
/// `artifacts/` so those keep today's codes.
fn resolve(home: &Path, session: &SessionId, path: &str) -> Result<File, Refused> {
    let Some((_, dir)) = crate::recent::lookup_logged(home, session) else {
        return Err(crate::resume::not_found(session));
    };
    let home_real = fs::canonicalize(home).map_err(|error| unreadable(path, &error))?;
    let opened = log::open_artifact(&dir, path);
    // A missing or unreadable `artifacts/` orders before the session
    // directory's own placement guard, as today's staging does: a linked
    // session directory without `artifacts/` is `not_found`.
    match &opened {
        Err(log::Refusal::NoArtifacts) => return Err(missing(session, path)),
        Err(log::Refusal::RootUnreadable(error)) => {
            return Err(unreadable(path, error));
        }
        Ok(_) | Err(_) => {}
    }
    // The session lookup's own guard: the session directory itself must
    // sit at its own place under the canonical Fiber home, so a link at
    // or above it is refused like a path outside. Both sides start from
    // the canonical Fiber home, so a home reached through a link still
    // matches. The session directory's own canonicalization lives in
    // `log`; a failure there is not placed, so it refuses as outside.
    // An open file is dropped with the refusal.
    // `dir` is built from a project directory entry and a validated
    // session id, so `relative` has only normal components. Joining it to
    // the canonical home gives the expected location without following a
    // link in the project/session path; canonicalizing that candidate
    // would accept the link this guard must refuse.
    let placed = dir
        .strip_prefix(home)
        .ok()
        .is_some_and(|relative| log::dir_is_at(&dir, &home_real.join(relative)));
    if !placed {
        return Err(outside(path));
    }
    match opened {
        Ok(file) => Ok(file),
        Err(log::Refusal::Outside) => Err(outside(path)),
        Err(log::Refusal::NotRegular) => Err(not_regular(path)),
        Err(log::Refusal::Missing) => Err(missing(session, path)),
        Err(log::Refusal::Unreadable(error)) => Err(unreadable(path, &error)),
        // Ordered before the placement guard above, so repeated here to
        // keep the match exhaustive with no wildcard arm.
        Err(log::Refusal::NoArtifacts) => Err(missing(session, path)),
        Err(log::Refusal::RootUnreadable(error)) => Err(unreadable(path, &error)),
    }
}

/// At most `LIMIT` bytes of the opened file, or `too_large`. The file's
/// length is not trusted: the read is bounded, and a read error is
/// `io_failed`, never a partial answer.
fn read_bounded(file: File, shown: &str) -> Result<Vec<u8>, Refused> {
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| unreadable(shown, &error))?;
    let limit = match usize::try_from(LIMIT) {
        Ok(limit) => limit,
        Err(_) => usize::MAX,
    };
    if bytes.len() > limit {
        return Err(Refused {
            code: ErrorCode::TooLarge,
            message: format!("`{shown}` is larger than 10 MiB."),
        });
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
fn outside(path: &str) -> Refused {
    Refused {
        code: ErrorCode::InvalidArguments,
        message: format!("`{path}` is not a file under the session's `artifacts/`."),
    }
}

/// `` `<path>` is not a regular file. ``
fn not_regular(path: &str) -> Refused {
    Refused {
        code: ErrorCode::InvalidArguments,
        message: format!("`{path}` is not a regular file."),
    }
}

/// `` No file `<path>` in session `<id>`. ``
fn missing(session: &SessionId, path: &str) -> Refused {
    Refused {
        code: ErrorCode::NotFound,
        message: format!("No file `{path}` in session `{}`.", session.0),
    }
}

/// `` `<path>` could not be read: <error>. `` The error names no path, so
/// it is safe to carry.
fn unreadable(path: &str, error: &io::Error) -> Refused {
    Refused {
        code: ErrorCode::IoFailed,
        message: format!("`{path}` could not be read: {error}."),
    }
}

#[cfg(test)]
#[path = "read_file_tests.rs"]
mod tests;
