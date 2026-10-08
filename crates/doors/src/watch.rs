//! Reading a delegate's stream as an ordinary client (`docs/delegates.md`,
//! "Streams"): a `full` subscription folds the log by `seq` first, then
//! streams live lines. The caller folds every envelope until `fiber_exited`
//! or the end of the connection. The socket helpers are `attach`'s own
//! rather than a second client.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::Path;

use contract::{Envelope, SessionId};

use crate::attach::{check_version, is_answer, next_line, rejection, send};
use crate::mint;

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
    let socket = home.join("run").join(&id.0);
    let stream = UnixStream::connect(&socket)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let sub = mint("c_");
    send(
        &mut writer,
        id,
        &sub,
        "subscribe",
        serde_json::json!({"level": "full"}),
    )
    .map_err(failed)?;
    // The acknowledgement is written before the writer starts, so it is
    // the first line this connection reads, ahead of the fold.
    loop {
        let Some((_, line)) = next_line(&mut reader) else {
            return Err(closed(id));
        };
        if is_answer(&line, "command_rejected", &sub) {
            return Err(failed(rejection(&line)));
        }
        if is_answer(&line, "command_accepted", &sub) {
            check_version(id, &line).map_err(failed)?;
            break;
        }
    }
    let mut last_seq = None;
    loop {
        let Some((_, line)) = next_line(&mut reader) else {
            return Ok(Watched::Closed { last_seq });
        };
        // Only this connection's subscribe was answered above, and it
        // sends no more commands, so every later line is the session's
        // own. One that is not an envelope is skipped.
        let Ok(envelope) = serde_json::from_value::<Envelope>(line) else {
            continue;
        };
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

/// A Fiber failure as an I/O error: the watcher's only error type.
fn failed(failure: contract::shapes::Failure) -> std::io::Error {
    std::io::Error::other(failure.message)
}

/// The session ended before answering: its connection closed before the
/// subscription was acknowledged.
fn closed(id: &SessionId) -> std::io::Error {
    std::io::Error::other(format!("session {} ended before subscribing", id.0))
}
