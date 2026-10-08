//! The relay from one client connection to session sockets
//! (`docs/invocation.md`, "What the hub speaks"): a command with a
//! `session_id` is passed to that session's socket without the key, and
//! what the session sends is passed back. A session whose socket accepts
//! no connection is resumed first.
//!
//! A subscription belongs to the hub connection, not to one session
//! socket: a `subscribe` is kept once the session accepts it, and a later
//! accepted `subscribe` replaces the kept one, so a reconnect to that
//! session sends it again under a hub-minted id before the client's
//! command, dropping the session's acknowledgement of it. A `subscribe`
//! the session rejects, or never answers, is not kept. A reconnect waits
//! for the old relay thread to deliver every acknowledgement it read
//! before it reads the kept subscription, so the replay is the accepted
//! level. The wait holds no lock; the joined thread waits only on the
//! session stream and the client writer. A client that stops reading can
//! hold the old thread in its forward of a later line, after that line is
//! already kept, until the client reads or disconnects; only that client's
//! own reconnect waits.
//!
//! A `closing` answer from a session whose log ends in `fiber_exited` is
//! not passed on: the command is routed again to the resumed session.

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread;

use contract::{CommandId, SessionId};
use serde_json::{Map, Value};

use crate::connection::{Hub, lock, reject};

/// The commands relayed on one session connection that the session has
/// not acknowledged yet: each command id and its line without the
/// `session_id`, so a `closing` answer can route it again. Shared by the
/// relay entry and its thread.
pub(crate) type Kept = Arc<Mutex<Vec<(String, Map<String, Value>)>>>;

/// The hub-minted `subscribe` ids whose acknowledgements one relay thread
/// drops: the replay it sent again, and any level transferred onto its
/// session later. Shared by the entry and its thread, so a transfer lands
/// even while the thread runs.
pub(crate) type Replayed = Arc<Mutex<Vec<String>>>;

/// One relay: the session connection, the writer the next command for it
/// uses, and the commands it has not acknowledged. The relay thread owns
/// the reader; both halves close together. The thread handle is what a
/// reconnect joins after a failed write, so every acknowledgement the old
/// thread read is kept before the kept subscription is read.
pub(crate) struct Relay {
    pub(crate) session: String,
    pub(crate) epoch: u64,
    pub(crate) writer: UnixStream,
    pub(crate) kept: Kept,
    pub(crate) replayed: Replayed,
    pub(crate) thread: Option<thread::JoinHandle<()>>,
}

/// A connection's relays, the last epoch minted on it, and the last
/// `subscribe` each session accepted from it. Epochs are never reused for the
/// connection's lifetime, so a stale relay thread never drops the entry of
/// a reconnect to the same session.
#[derive(Default)]
pub(crate) struct Relays {
    pub(crate) entries: Vec<Relay>,
    pub(crate) minted: u64,
    /// Per session, the last `subscribe` it accepted, without its
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

    /// Replaces `session`'s kept subscription with `line` when it is an
    /// accepted `subscribe` from the relay still in the map: a stale
    /// relay thread's buffered acknowledgement never overwrites the
    /// replacement's level. Adds it when none is kept. Anything else
    /// changes nothing.
    fn accepted(&mut self, session: &str, epoch: u64, line: Map<String, Value>) {
        if relay_slot(&self.entries, session, epoch).is_none() {
            return;
        }
        if line.get("command").and_then(Value::as_str) != Some("subscribe") {
            return;
        }
        keep_subscription(self, session, line);
    }

    /// Transfers the kept level `line` onto `session`'s existing relay:
    /// sends it under a hub-minted id the relay thread drops, and keeps it
    /// for the session. A relay already holding the level is left alone.
    /// The id registers before the write: the running relay answers even
    /// an instant reply after the registration, never before it, so its
    /// acknowledgement never leaks to the client. True when nothing more
    /// is needed: the level is kept, and either no relay exists or the
    /// existing one carries it. False when no relay exists and the level
    /// is only kept: the caller connects one.
    pub(crate) fn transfer(&mut self, session: &str, line: &Map<String, Value>) -> bool {
        if self.subscription(session).is_some() {
            return true;
        }
        keep_subscription(self, session, line.clone());
        let epoch = {
            let Some(entry) = self
                .entries
                .iter_mut()
                .find(|entry| entry.session == session)
            else {
                return false;
            };
            let mut line = line.clone();
            let minted = crate::start::mint("c_");
            line.insert("id".to_owned(), Value::String(minted.clone()));
            let Some(bytes) = line_bytes(&line) else {
                return true;
            };
            lock(&entry.replayed).push(minted.clone());
            if write_all(&entry.writer, &bytes).is_err() {
                lock(&entry.replayed).retain(|muted| *muted != minted);
                entry.epoch
            } else {
                return true;
            }
        };
        // The relay died with its socket: drop it and connect anew.
        self.finish(session, epoch);
        false
    }

    /// `session`'s kept subscription, if any.
    pub(crate) fn subscription(&self, session: &str) -> Option<Map<String, Value>> {
        self.subscribed
            .iter()
            .find(|(kept, _)| kept == session)
            .map(|(_, line)| line.clone())
    }
}

/// Keeps `line` as `session`'s subscription, replacing the kept one.
fn keep_subscription(relays: &mut Relays, session: &str, line: Map<String, Value>) {
    if let Some((_, kept)) = relays
        .subscribed
        .iter_mut()
        .find(|(kept, _)| kept == session)
    {
        *kept = line;
    } else {
        relays.subscribed.push((session.to_owned(), line));
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
    route(id, session, stripped, hub, writer, relays, false);
}

/// Passes `stripped` to the session's open relay, or to a new connection:
/// the socket that accepts, or a resumed session. With `exited`, the
/// session's log ends in `fiber_exited` and a socket that accepts may be
/// its exiting process, so the connection comes from
/// [`crate::resume::resume_exited`].
fn route(
    id: &CommandId,
    session: &str,
    stripped: Map<String, Value>,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    exited: bool,
) {
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
            // Kept before the write, so the relay thread finds it when the
            // session answers.
            let sent = held.entries.get(at).is_some_and(|entry| {
                lock(&entry.kept).push((id.0.clone(), stripped.clone()));
                write_all(&entry.writer, &bytes).is_ok()
            });
            if sent {
                return;
            }
            // The write failed: the session is gone. Shut the entry's
            // writer so the old thread's read ends, then wait for it to
            // deliver every acknowledgement it read before reading the kept
            // level. The join holds no lock; the thread takes it to keep.
            let old = held.entries.get_mut(at).map(|entry| {
                let thread = entry.thread.take();
                let epoch = entry.epoch;
                match entry.writer.shutdown(Shutdown::Both) {
                    Ok(()) | Err(_) => {}
                }
                (thread, epoch)
            });
            drop(held);
            #[cfg(test)]
            {
                if let Some(before) = lock(&hub.before_join).take() {
                    before();
                }
            }
            if let Some((thread, epoch)) = old {
                if let Some(thread) = thread {
                    match thread.join() {
                        Ok(()) | Err(_) => {}
                    }
                }
                lock(relays).finish(session, epoch);
            }
            lock(relays).subscription(session)
        } else {
            held.subscription(session)
        }
    };
    let sid = SessionId(session.to_owned());
    let opened = if exited {
        crate::resume::resume_exited(hub, &sid)
    } else {
        UnixStream::connect(hub.home.join("run").join(session))
            .or_else(|_| crate::resume::resume(hub, &sid))
    };
    let stream = match opened {
        Ok(stream) => stream,
        Err(refused) => {
            reject(writer, hub, Some(id), &refused.code, &refused.message);
            return;
        }
    };
    // The connection's subscription first, under an id of the hub's own,
    // so the client's command reaches a subscribed connection.
    attach(
        session,
        stream,
        hub,
        writer,
        relays,
        kept,
        Some((id.clone(), bytes, stripped)),
    );
}

/// Sends the kept subscription again under a hub-minted id the relay
/// thread drops, then the client's command when one is relayed, and
/// follows the session on a relay thread: shared by a new relay and the
/// rewind redirect, which sends only the subscription (its caller keeps
/// the level first). A write that fails answers the client's command
/// `session_not_found` when there is one; without one the connection is
/// gone, so nothing is answered.
pub(crate) fn attach(
    session: &str,
    stream: UnixStream,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    replay: Option<Map<String, Value>>,
    command: Option<(CommandId, Vec<u8>, Map<String, Value>)>,
) {
    let mut replay = replay;
    let replayed: Replayed = Arc::new(Mutex::new(Vec::new()));
    if let Some(line) = replay.as_mut() {
        let minted = crate::start::mint("c_");
        line.insert("id".to_owned(), Value::String(minted.clone()));
        let sent = line_bytes(line).is_some_and(|line| write_all(&stream, &line).is_ok());
        if !sent {
            if let Some((id, _, _)) = &command {
                not_found(writer, hub, id, session);
            }
            return;
        }
        lock(&replayed).push(minted);
    };
    let kept: Kept = Arc::new(Mutex::new(
        command
            .as_ref()
            .map(|(id, _, stripped)| (id.0.clone(), stripped.clone()))
            .into_iter()
            .collect(),
    ));
    if let Some((id, bytes, _)) = &command
        && write_all(&stream, bytes).is_err()
    {
        not_found(writer, hub, id, session);
        return;
    }
    let reader = match stream.try_clone() {
        Ok(reader) => reader,
        Err(_) => {
            if let Some((id, _, _)) = &command {
                not_found(writer, hub, id, session);
            }
            return;
        }
    };
    let mut held = lock(relays);
    let epoch = held.mint();
    // The thread may run before its entry is pushed: on session EOF it
    // only removes an entry it finds.
    let relayed = thread::Builder::new().name("hub-relay".to_owned()).spawn({
        let hub = Arc::clone(hub);
        let writer = Arc::clone(writer);
        let relays = Arc::clone(relays);
        let session = session.to_owned();
        let kept = Arc::clone(&kept);
        let owned = RelayThread {
            epoch,
            replayed: Arc::clone(&replayed),
            kept,
        };
        move || relay(owned, &session, reader, &hub, &writer, &relays)
    });
    // A thread that never started leaves no entry: the next command for
    // the session reconnects.
    if let Ok(thread) = relayed {
        held.entries.push(Relay {
            session: session.to_owned(),
            epoch,
            writer: stream,
            kept,
            replayed,
            thread: Some(thread),
        });
    }
}

/// What one relay thread owns besides its streams: its epoch, the ids of
/// the subscriptions it sent again, and its entry's unacknowledged
/// commands, which outlive the entry.
struct RelayThread {
    epoch: u64,
    replayed: Replayed,
    kept: Kept,
}

fn not_found(writer: &Arc<Mutex<UnixStream>>, hub: &Hub, id: &CommandId, session: &str) {
    let refused = crate::resume::not_found(&SessionId(session.to_owned()));
    reject(writer, hub, Some(id), &refused.code, &refused.message);
}

/// Copies every session line back verbatim onto the client's shared
/// writer, except the acknowledgement of the subscription the hub sent
/// again, which the client never sent, and a `closing` answer from a
/// session whose log ends in `fiber_exited`: that drops this thread's map
/// entry, without shutting the stream, and routes the command again. An
/// accepted `subscribe` becomes the connection's kept subscription before
/// its acknowledgement is forwarded, so a client that has read it and
/// triggers a reconnect gets that level replayed. The session closing
/// its socket drops the map entry; the next command for it reconnects.
fn relay(
    owned: RelayThread,
    session: &str,
    reader: UnixStream,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
) {
    let RelayThread {
        epoch,
        replayed,
        kept,
    } = owned;
    let mut read = BufReader::new(reader);
    let mut buf = Vec::new();
    let mut exiting = false;
    // Set once the session closes its socket: only then does the
    // connection follow a `rewound` to the next session. A client that
    // disconnects first fails its forward, and its dead connection
    // follows nothing.
    let mut ended = false;
    loop {
        buf.clear();
        match read.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => {
                ended = true;
                break;
            }
            Ok(_) => {
                if muted(&buf, &replayed) {
                    continue;
                }
                match settle(&buf, &kept, hub, session, exiting) {
                    Some(Settled::Reroute(id, line)) => {
                        exiting = true;
                        lock(relays).finish(session, epoch);
                        route(&CommandId(id), session, line, hub, writer, relays, true);
                        continue;
                    }
                    Some(Settled::Rewound(next)) => {
                        // The new session starts before the client reads
                        // the acknowledgement, so its next command finds
                        // it running; a start that fails still forwards
                        // the acknowledgement below.
                        drop(crate::rewind::reach(
                            hub,
                            &SessionId(session.to_owned()),
                            &next,
                        ));
                    }
                    Some(Settled::Subscribed(line)) => {
                        #[cfg(test)]
                        {
                            if let Some(before) = lock(&hub.before_accepted).take() {
                                before(&buf, relays);
                            }
                        }
                        lock(relays).accepted(session, epoch, line);
                    }
                    None => {}
                }
                if forward(&buf, writer, relays, hub).is_err() {
                    break;
                }
            }
        }
    }
    if ended {
        crate::rewind::follow(hub, writer, relays, session);
    }
    lock(relays).finish(session, epoch);
}

/// The only writer of session lines to the client: every session line the
/// relay passes back goes through it. It runs the one-shot
/// `before_forward` test hook first, with no lock held, so the hook
/// observes the acknowledgement after it is recorded.
fn forward(
    buf: &[u8],
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    hub: &Hub,
) -> std::io::Result<()> {
    #[cfg(test)]
    {
        if let Some(before) = lock(&hub.before_forward).take() {
            before(buf, relays);
        }
    }
    #[cfg(not(test))]
    {
        let _ = (relays, hub);
    }
    let mut out = lock(writer);
    out.write_all(buf).and_then(|()| out.flush())
}

/// What settling an acknowledged command decides: either the command is
/// routed again, an accepted `rewind` starts the session it names, or an
/// accepted `subscribe` becomes the kept subscription.
enum Settled {
    Reroute(String, Map<String, Value>),
    Rewound(SessionId),
    Subscribed(Map<String, Value>),
}

/// Settles a kept command `line` acknowledges: it is no longer kept. A
/// `closing` rejection of it returns it, to be routed again, once this
/// thread has seen the exited window, whether in this answer
/// (`crate::resume::exited`) or an earlier one (`exiting`). An accepted
/// `subscribe` returns its line, to become the kept subscription.
fn settle(line: &[u8], kept: &Kept, hub: &Hub, session: &str, exiting: bool) -> Option<Settled> {
    let ((id, command), verdict) = {
        let mut kept = lock(kept);
        if kept.is_empty() {
            return None;
        }
        let (id, verdict) = acknowledgement(line)?;
        let at = kept.iter().position(|(kept, _)| *kept == id)?;
        let (_, command) = kept.remove(at);
        ((id, command), verdict)
    };
    // The kept lock is released before the caller takes the relays lock:
    // `route` takes relays and then kept, so the reverse order deadlocks.
    match verdict {
        Verdict::Closing
            if exiting || crate::resume::exited(&hub.home, &SessionId(session.to_owned())) =>
        {
            Some(Settled::Reroute(id, command))
        }
        Verdict::Accepted
            if command.get("command").and_then(Value::as_str) == Some("subscribe") =>
        {
            Some(Settled::Subscribed(command))
        }
        Verdict::Accepted if command.get("command").and_then(Value::as_str) == Some("rewind") => {
            rewind_next(line).map(Settled::Rewound)
        }
        Verdict::Accepted | Verdict::Rejected | Verdict::Closing => None,
    }
}

/// The session an accepted `rewind` starts: `result.new_session_id` with
/// the shape the hub mints. `None` for an acknowledgement that names none
/// or names one of another shape, which starts nothing.
fn rewind_next(line: &[u8]) -> Option<SessionId> {
    let line = serde_json::from_slice::<Value>(line).ok()?;
    if line.get("kind").and_then(Value::as_str)? != "command_accepted" {
        return None;
    }
    let next = line
        .get("payload")?
        .get("result")?
        .get("new_session_id")?
        .as_str()?;
    if !valid_session_id(next) {
        return None;
    }
    Some(SessionId(next.to_owned()))
}

/// The `command_id` of a session's `command_accepted` or
/// `command_rejected`, and what the acknowledgement settles.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Accepted,
    Rejected,
    Closing,
}

fn acknowledgement(line: &[u8]) -> Option<(String, Verdict)> {
    let line = serde_json::from_slice::<Value>(line).ok()?;
    let kind = line.get("kind").and_then(Value::as_str)?;
    if !matches!(kind, "command_accepted" | "command_rejected") {
        return None;
    }
    let payload = line.get("payload")?;
    let id = payload.get("command_id").and_then(Value::as_str)?;
    let verdict = if kind == "command_accepted" {
        Verdict::Accepted
    } else if payload.get("code").and_then(Value::as_str) == Some("closing") {
        Verdict::Closing
    } else {
        Verdict::Rejected
    };
    Some((id.to_owned(), verdict))
}

/// Whether `line` is a session's `command_accepted` or `command_rejected`
/// for `command_id`: tests only, since the relay thread drops replays
/// through [`muted`].
#[cfg(test)]
pub(crate) fn acknowledges(line: &[u8], command_id: &str) -> bool {
    acknowledgement(line).is_some_and(|(id, _)| id == command_id)
}

/// Whether `buf` acknowledges a subscription the hub sent again on this
/// relay: its id is dropped from the shared list once, so a transferred
/// level never leaks its acknowledgement to the client, while a later
/// transfer for the same relay still drops its own.
fn muted(buf: &[u8], replayed: &Replayed) -> bool {
    let Some((id, _)) = acknowledgement(buf) else {
        return false;
    };
    let mut replayed = lock(replayed);
    if let Some(at) = replayed.iter().position(|muted| *muted == id) {
        replayed.remove(at);
        true
    } else {
        false
    }
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
