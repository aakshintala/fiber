//! The relay from one client connection to session sockets
//! (`docs/invocation.md`, "What the hub speaks"): a command with a
//! `session_id` is passed to that session's socket without the key, and
//! what the session sends is passed back. A session whose socket accepts
//! no connection is resumed first.
//!
//! A subscription belongs to the hub connection, not to one session
//! socket: the connection's first `subscribe` for a session is kept, and a
//! reconnect to that session sends it again under a hub-minted id before
//! the client's command, dropping the session's acknowledgement of it.

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread;

use contract::{CommandId, SessionId};
use serde_json::{Map, Value};

use crate::connection::{Hub, lock, reject};

/// One relay: the session connection, and the writer the next command for
/// it uses. The relay thread owns the reader; both halves close together.
pub(crate) struct Relay {
    pub(crate) session: String,
    pub(crate) epoch: u64,
    pub(crate) writer: UnixStream,
}

/// A connection's relays, the last epoch minted on it, and the first
/// `subscribe` it relayed to each session. Epochs are never reused for the
/// connection's lifetime, so a stale relay thread never drops the entry of
/// a reconnect to the same session.
#[derive(Default)]
pub(crate) struct Relays {
    pub(crate) entries: Vec<Relay>,
    pub(crate) minted: u64,
    /// Per session, the first `subscribe` relayed to it, without its
    /// `session_id`: what a reconnect sends again.
    pub(crate) subscribed: Vec<(String, Map<String, Value>)>,
}

impl Relays {
    /// The next relay epoch: one past every epoch minted on this connection.
    pub(crate) fn mint(&mut self) -> u64 {
        self.minted += 1;
        self.minted
    }

    /// Drops a finished relay thread's entry: its own session and epoch
    /// only, so a stale thread never drops a reconnect's entry.
    pub(crate) fn finish(&mut self, session: &str, epoch: u64) {
        if let Some(at) = relay_slot(&self.entries, session, epoch) {
            self.entries.remove(at);
        }
    }

    /// Shuts down every relay stream: each session sees this client leave.
    pub(crate) fn close_all(&mut self) {
        for entry in self.entries.drain(..) {
            match entry.writer.shutdown(Shutdown::Both) {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Keeps `line` as `session`'s subscription when it is a `subscribe`
    /// and none is kept yet.
    fn keep(&mut self, session: &str, line: &Map<String, Value>) {
        let subscribes = line.get("command").and_then(Value::as_str) == Some("subscribe");
        if subscribes && !self.subscribed.iter().any(|(kept, _)| kept == session) {
            self.subscribed.push((session.to_owned(), line.clone()));
        }
    }

    /// `session`'s kept subscription, if any.
    fn subscription(&self, session: &str) -> Option<Map<String, Value>> {
        self.subscribed
            .iter()
            .find(|(kept, _)| kept == session)
            .map(|(_, line)| line.clone())
    }
}

/// Whether `session` names a session the hub can reach: `s_` plus 16
/// lowercase hex digits, the shape the hub mints and `parse_session_id`
/// accepts. Anything else names no session, so it is rejected without
/// touching the filesystem: an absolute path or `..` never escapes `run/`
/// or a project's `sessions/`, and `"hub"` never routes back to the hub.
pub(crate) fn valid_session_id(session: &str) -> bool {
    let hex = session.strip_prefix("s_").unwrap_or("");
    hex.len() == 16
        && hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Passes the line to the session's socket without the `session_id` key,
/// resuming the session first when its socket accepts no connection. A
/// session the hub cannot find is rejected `session_not_found`; a resume
/// that fails is rejected with its failure.
pub(crate) fn relay_command(
    id: &CommandId,
    session: &str,
    value: &Value,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
) {
    if !valid_session_id(session) {
        not_found(writer, hub, id, session);
        return;
    }
    let Some(object) = value.as_object() else {
        return;
    };
    let mut stripped = object.clone();
    stripped.remove("session_id");
    let Some(bytes) = line_bytes(&stripped) else {
        return;
    };
    let kept = {
        let mut held = lock(relays);
        if let Some(at) = held
            .entries
            .iter()
            .position(|entry| entry.session == session)
        {
            if held
                .entries
                .get(at)
                .is_some_and(|entry| write_all(&entry.writer, &bytes).is_ok())
            {
                held.keep(session, &stripped);
                return;
            }
            held.entries.remove(at);
        }
        held.subscription(session)
    };
    let socket = hub.home.join("run").join(session);
    let stream = match UnixStream::connect(&socket) {
        Ok(stream) => stream,
        Err(_) => match crate::resume::resume(hub, &SessionId(session.to_owned())) {
            Ok(stream) => stream,
            Err(refused) => {
                reject(writer, hub, Some(id), &refused.code, &refused.message);
                return;
            }
        },
    };
    // The connection's subscription first, under an id of the hub's own,
    // so the client's command reaches a subscribed connection.
    let replayed = match kept {
        Some(mut line) => {
            let minted = crate::start::mint("c_");
            line.insert("id".to_owned(), Value::String(minted.clone()));
            let sent = line_bytes(&line).is_some_and(|line| write_all(&stream, &line).is_ok());
            if !sent {
                not_found(writer, hub, id, session);
                return;
            }
            Some(minted)
        }
        None => None,
    };
    if write_all(&stream, &bytes).is_err() {
        not_found(writer, hub, id, session);
        return;
    }
    let reader = match stream.try_clone() {
        Ok(reader) => reader,
        Err(_) => {
            not_found(writer, hub, id, session);
            return;
        }
    };
    let mut held = lock(relays);
    held.keep(session, &stripped);
    let epoch = held.mint();
    // The thread may run before its entry is pushed: on session EOF it
    // only removes an entry it finds.
    let relayed = thread::Builder::new().name("hub-relay".to_owned()).spawn({
        let writer = Arc::clone(writer);
        let relays = Arc::clone(relays);
        let session = session.to_owned();
        move || relay(epoch, &session, reader, &writer, &relays, replayed)
    });
    // A thread that never started leaves no entry: the next command for
    // the session reconnects.
    if relayed.is_ok() {
        held.entries.push(Relay {
            session: session.to_owned(),
            epoch,
            writer: stream,
        });
    }
}

fn not_found(writer: &Arc<Mutex<UnixStream>>, hub: &Hub, id: &CommandId, session: &str) {
    let refused = crate::resume::not_found(&SessionId(session.to_owned()));
    reject(writer, hub, Some(id), &refused.code, &refused.message);
}

/// Copies every session line back verbatim onto the client's shared
/// writer, except the acknowledgement of `replayed`, the subscription the
/// hub sent again, which the client never sent. The session closing its
/// socket drops the map entry; the next command for it reconnects.
fn relay(
    epoch: u64,
    session: &str,
    reader: UnixStream,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    replayed: Option<String>,
) {
    let mut replayed = replayed;
    let mut read = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match read.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if replayed
                    .as_deref()
                    .is_some_and(|minted| acknowledges(&buf, minted))
                {
                    replayed = None;
                    continue;
                }
                let mut out = lock(writer);
                if out.write_all(&buf).and_then(|()| out.flush()).is_err() {
                    break;
                }
            }
        }
    }
    lock(relays).finish(session, epoch);
}

/// Whether `line` is a session's `command_accepted` or `command_rejected`
/// for `command_id`.
pub(crate) fn acknowledges(line: &[u8], command_id: &str) -> bool {
    let Ok(line) = serde_json::from_slice::<Value>(line) else {
        return false;
    };
    let acknowledgement = matches!(
        line.get("kind").and_then(Value::as_str),
        Some("command_accepted" | "command_rejected")
    );
    acknowledgement
        && line
            .get("payload")
            .and_then(|payload| payload.get("command_id"))
            .and_then(Value::as_str)
            == Some(command_id)
}

/// The entry a finished relay thread drops: its own session and epoch, so
/// a stale thread never drops a reconnect's entry.
pub(crate) fn relay_slot(entries: &[Relay], session: &str, epoch: u64) -> Option<usize> {
    entries
        .iter()
        .position(|entry| entry.session == session && entry.epoch == epoch)
}

/// `line` as one JSON line.
fn line_bytes(line: &Map<String, Value>) -> Option<Vec<u8>> {
    let mut bytes = serde_json::to_vec(line).ok()?;
    bytes.push(b'\n');
    Some(bytes)
}

fn write_all(stream: &UnixStream, bytes: &[u8]) -> std::io::Result<()> {
    let mut stream = stream;
    stream.write_all(bytes)?;
    stream.flush()
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;
