//! Reads a file listed by the pending offer (`docs/invocation.md`, "Driver commands").

use contract::commands::OfferedFile;
use contract::events::{CommandResult, Event};
use contract::{CommandId, ErrorCode};

use crate::client::{Conn, ENDED, accept, reject};

const STALE: &str = "That offer is no longer pending.";

/// Answers on the connection's reader thread, without entering the inbox or
/// writing any durable event (`docs/architecture.md`, "One inbox").
pub(crate) fn answer(conn: &mut Conn, id: CommandId, args: &OfferedFile) {
    let Some(log) = conn.gate.log.upgrade() else {
        reject(conn, Some(id), ErrorCode::Closing, ENDED);
        return;
    };
    let pending = log.latest("repository_code_offered");
    drop(log);
    let offered = pending
        .as_ref()
        .and_then(|line| Event::from_envelope(line).ok())
        .flatten();
    let Some(Event::RepositoryCodeOffered(offered)) = offered else {
        reject(conn, Some(id), ErrorCode::StaleRequest, STALE);
        return;
    };
    if offered.request_id != args.request_id {
        reject(conn, Some(id), ErrorCode::StaleRequest, STALE);
        return;
    }
    let Some(repository) = conn.gate.repository() else {
        reject(conn, Some(id), ErrorCode::StaleRequest, STALE);
        return;
    };
    if !offered
        .items
        .iter()
        .flat_map(|item| &item.files)
        .any(|path| path == &args.path)
    {
        reject(
            conn,
            Some(id),
            ErrorCode::InvalidArguments,
            "That path is not in the pending offer.",
        );
        return;
    }
    match repository.file(&args.path) {
        Ok(text) => accept(conn, id, Some(CommandResult::OfferedFile { text })),
        Err(failure) => reject(conn, Some(id), failure.code, &failure.message),
    }
}

#[cfg(test)]
#[path = "offered_tests.rs"]
mod tests;
