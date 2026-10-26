//! Reading a delegate's stream as an ordinary client (`docs/delegates.md`,
//! "Streams"): a `full` subscription folds the log by `seq` first, then
//! streams live lines. The caller folds every envelope until `fiber_exited`
//! or the end of the connection. The socket helpers are `attach`'s own
//! rather than a second client.

use std::path::Path;

use contract::{Envelope, SessionId};

use crate::SessionError;
use crate::attach::{Unsubscribed, next_envelope, subscribe_full};

/// What a watch ended on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Watched {
    /// A `fiber_exited` line arrived: the session wrote its last line.
    Exited,
    /// The connection closed first: the last `seq` seen, when any line
    /// arrived.
    Closed {
        /// The last `seq` seen.
        last_seq: Option<u64>,
    },
}

/// Watches the session `id` running under `home`: subscribes `full` and
/// calls `on_line` for each envelope, in arrival order, until its
/// `fiber_exited` or the end of the connection. Acknowledgements stay on
/// this connection, as they do for any client. A refused connection is an
/// error; a connection that closes first ends [`Watched::Closed`].
pub fn watch(
    home: &Path,
    id: &SessionId,
    on_line: &mut dyn FnMut(&Envelope),
) -> std::io::Result<Watched> {
    watch_after(home, id, None, on_line)
}

/// [`watch`] starting past a boundary: an envelope whose `seq` is at or
/// before `after` is neither passed to `on_line` nor ends the watch, and
/// does not move [`Watched::Closed`]'s `last_seq`. One without `seq` is
/// delivered. A resume watches past the last line its earlier run wrote,
/// so an earlier run's `fiber_exited` never stands in for the new run's
/// end.
pub fn watch_after(
    home: &Path,
    id: &SessionId,
    after: Option<u64>,
    on_line: &mut dyn FnMut(&Envelope),
) -> std::io::Result<Watched> {
    let socket = home.join("run").join(&id.0);
    let (mut reader, _) = match subscribe_full(&socket, id) {
        Ok(pair) => pair,
        Err(Unsubscribed::Connect(error) | Unsubscribed::Clone(error)) => return Err(error),
        Err(Unsubscribed::Write(failure)) => return Err(failed(failure)),
        Err(Unsubscribed::Closed) => return Err(closed(id)),
        Err(Unsubscribed::Rejected(failure) | Unsubscribed::Version(failure)) => {
            return Err(failed(failure));
        }
    };
    let mut last_seq = None;
    loop {
        let Some((_, envelope)) = next_envelope(&mut reader) else {
            return Ok(Watched::Closed { last_seq });
        };
        // Only this connection's subscribe was answered above, and it
        // sends no more commands, so every later line is the session's
        // own.
        // Past the boundary: an earlier run's lines, its `fiber_exited`
        // included, are neither delivered nor end the watch.
        if after.is_some_and(|bound| envelope.seq.is_some_and(|seq| seq.0 <= bound)) {
            continue;
        }
        // Ephemeral lines carry no `seq`: the last one seen stands.
        if let Some(seq) = envelope.seq.map(|seq| seq.0) {
            last_seq = Some(seq);
        }
        let exited = envelope.kind == "fiber_exited";
        on_line(&envelope);
        if exited {
            return Ok(Watched::Exited);
        }
    }
}

/// A session failure as an I/O error: the watcher's only error type. The
/// sentence is the failure's own, so it reads as it did before.
fn failed(error: SessionError) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

/// The session ended before answering: its connection closed before the
/// subscription was acknowledged.
fn closed(id: &SessionId) -> std::io::Error {
    std::io::Error::other(format!("session {} ended before subscribing", id.0))
}
