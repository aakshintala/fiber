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
//! not passed on: the command is routed again to the resumed session. A
//! relay that saw the exited window, or whose write failed, is retiring:
//! commands routed to it queue unsent, and its thread passes them on in
//! the order they were read (`crate::retire`).
//!
//! A session that resumes is subscribed again at the level the connection
//! last held, with no client command (`crate::rejoin`).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread;

use contract::{CommandId, SessionId};
use serde_json::{Map, Value};

use crate::connection::{Hub, lock, reject};
use crate::first::First;
use crate::retire::{AckOrder, Retire};

/// The commands relayed on one session connection that the session has
/// not acknowledged yet: each command id, its line without the
/// `session_id`, and whether it was written to the socket, so a `closing`
/// answer can route it again and a retiring relay can pass on what it
/// never wrote. Shared by the relay entry and its thread.
pub(crate) type Kept = Arc<Mutex<Vec<(String, Map<String, Value>, bool)>>>;

/// The hub-minted `subscribe` ids whose acknowledgements one relay thread
/// drops: the replay it sent again, and any level transferred onto its
/// session later. Shared by the entry and its thread, so a transfer lands
/// even while the thread runs.
pub(crate) type Replayed = Arc<Mutex<Vec<String>>>;

/// One relay: the session connection, the writer the next command for it
/// uses, and the commands it has not acknowledged. The relay thread owns
/// the reader; only the write half shuts down on a failed write, so every
/// answer already in the kernel buffer is still read. The thread handle is
/// what liveness checks read: an entry whose thread is gone is recovered
/// with its queue first (`crate::retire`), so a reconnect proceeds without
/// a thread or after a panic. Acknowledgements reach the client in the
/// order the connection read the commands: each new command waits on the
/// connection's acknowledgement queue (`crate::retire`), except one the
/// session answers when it ends.
pub(crate) struct Relay {
    pub(crate) session: String,
    pub(crate) epoch: u64,
    pub(crate) writer: UnixStream,
    pub(crate) kept: Kept,
    pub(crate) replayed: Replayed,
    pub(crate) thread: Option<thread::JoinHandle<()>>,
    /// Why the relay no longer takes writes, if it does not.
    pub(crate) retiring: Option<Retire>,
}

/// A connection's relays, the last epoch minted on it, and the last
/// `subscribe` each session accepted from it. Epochs are never reused for the
/// connection's lifetime, so a stale relay thread never drops the entry of
/// a reconnect to the same session.
#[derive(Default)]
pub(crate) struct Relays {
    pub(crate) entries: Vec<Relay>,
    pub(crate) minted: u64,
    /// One gate per session this connection opened: an opener holds its
    /// session's gate from its check until its attach publishes, so two
    /// openers never publish two relays for one session (see #1462).
    pub(crate) gates: HashMap<String, Arc<Mutex<()>>>,
    /// Tests only: pauses after a transfer write, before its result returns.
    #[cfg(test)]
    pub(crate) after_transfer_write: Option<Box<dyn FnOnce() + Send>>,
    /// Tests only: a one-shot pause in `route` between keeping a command
    /// as sent and writing it, with the relays lock held.
    #[cfg(test)]
    pub(crate) before_command_write: Option<Box<dyn FnOnce() + Send>>,
    /// Per session, the last `subscribe` it accepted, without its
    /// `session_id`: what a reconnect sends again.
    pub(crate) subscribed: Vec<(String, Map<String, Value>)>,
    /// One connection's commands per session in read order, except
    /// commands the session answers when they end: acknowledgements wait
    /// on it so the client reads them in command order across a re-route.
    pub(crate) order: Arc<AckOrder>,
    /// Per session this connection started with `content`, the first
    /// prompt that waits for its `full` subscription (`crate::first`).
    pub(crate) awaiting: Vec<(String, Arc<First>)>,
    /// What the rejoin sweep needs: each session's opening marks and marks.
    pub(crate) rejoin: crate::rejoin::Rejoin,
}

impl Relays {
    /// The next relay epoch: one past every epoch minted on this connection.
    pub(crate) fn mint(&mut self) -> u64 {
        self.minted += 1;
        self.minted
    }

    /// `session`'s open gate, inserting it when absent: the opener clones
    /// this Arc under the relays lock, drops that lock, then holds the
    /// gate until its attach publishes.
    pub(crate) fn gate(&mut self, session: &str) -> Arc<Mutex<()>> {
        self.gates.get(session).cloned().unwrap_or_else(|| {
            let gate = Arc::new(Mutex::new(()));
            self.gates.insert(session.to_owned(), Arc::clone(&gate));
            gate
        })
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
        self.rejoin.close();
        for entry in self.entries.drain(..) {
            match entry.writer.shutdown(Shutdown::Both) {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Replaces `session`'s kept subscription with `line` when it is an
    /// accepted `subscribe` from the relay still in the map: a stale
    /// relay thread's buffered acknowledgement never overwrites the
    /// replacement's level. A relay retiring as `Exited` keeps nothing:
    /// its entry is already gone by the time it reads an acceptance, so
    /// the acknowledgement is still forwarded but the level is not kept.
    /// Adds it when none is kept. Anything else
    /// changes nothing. When the level is `full`, removes and returns the
    /// first prompt waiting for it, for the caller to release once the
    /// relays lock is dropped.
    fn accepted(
        &mut self,
        session: &str,
        epoch: u64,
        line: Map<String, Value>,
    ) -> Option<Arc<First>> {
        let at = relay_slot(&self.entries, session, epoch)?;
        if self.entries.get(at).is_some_and(|entry| {
            entry
                .retiring
                .as_ref()
                .is_some_and(|retiring| matches!(retiring, Retire::Exited))
        }) {
            return None;
        }
        if line.get("command").and_then(Value::as_str) != Some("subscribe") {
            return None;
        }
        let full = line
            .get("args")
            .and_then(|args| args.get("level"))
            .and_then(Value::as_str)
            == Some("full");
        keep_subscription(self, session, line);
        if !full {
            return None;
        }
        let at = self
            .awaiting
            .iter()
            .position(|(waiting, _)| waiting == session)?;
        Some(self.awaiting.remove(at).1)
    }

    /// Transfers the kept level `line` onto `session`'s live relay:
    /// sends it under a hub-minted id the relay thread drops, and keeps it
    /// for the session. A relay already holding the level is left alone.
    /// The id registers before the write: the running relay answers even
    /// an instant reply after the registration, never before it, so its
    /// acknowledgement never leaks to the client. With only retiring
    /// relays the level is kept and nothing is opened: the next open
    /// replays it, and a client command queues on the retiring relay
    /// meanwhile. True when nothing more
    /// is needed: the level is kept, and either no relay exists or the
    /// existing one carries it. False when no relay exists and the level
    /// is only kept: the caller connects one.
    pub(crate) fn transfer(&mut self, session: &str, line: &Map<String, Value>) -> bool {
        if self.subscription(session).is_some() {
            return true;
        }
        keep_subscription(self, session, line.clone());
        let at = self
            .entries
            .iter()
            .position(|entry| entry.session == session && entry.retiring.is_none());
        let Some(at) = at else {
            // No live relay: with only retiring relays the next open
            // replays the kept level, so nothing is opened here. With no
            // relay at all the caller connects one.
            return self.entries.iter().any(|entry| entry.session == session);
        };
        let mut line = line.clone();
        let minted = crate::start::mint("c_");
        line.insert("id".to_owned(), Value::String(minted.clone()));
        let Some(bytes) = line_bytes(&line) else {
            return true;
        };
        let Some(entry) = self.entries.get_mut(at) else {
            return true;
        };
        lock(&entry.replayed).push(minted.clone());
        if write_all(&entry.writer, &bytes).is_err() {
            lock(&entry.replayed).retain(|muted| *muted != minted);
            // The relay died with its socket: it keeps no more writes,
            // but its thread still reads every buffered answer. The
            // thread handle stays in the entry.
            entry.retiring = Some(Retire::Dead);
            match entry.writer.shutdown(Shutdown::Write) {
                Ok(()) | Err(_) => {}
            }
            return true;
        }
        #[cfg(test)]
        if let Some(after_write) = self.after_transfer_write.take() {
            after_write();
        }
        true
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
    route(id, session, stripped, hub, writer, relays, None, false);
    #[cfg(test)]
    {
        if let Some(after) = lock(&hub.after_relay).take() {
            after();
        }
    }
}

/// Passes `stripped` to the session's oldest relay newer than `from`
/// (`None`: the oldest of all), or to a new connection: the socket that
/// accepts, or a resumed session. With `exited`, the session's log ends
/// in `fiber_exited` and a socket that accepts may be its exiting process,
/// so the connection comes from [`crate::resume::resume_exited`].
///
/// A command for a retiring relay queues unsent when its thread is alive,
/// and its thread passes the queue on in read order. A relay whose thread
/// is gone is recovered with its queue first (`crate::retire`), so a
/// reconnect proceeds without a thread or after a panic. A new command is
/// enqueued on the session's acknowledgement queue in read order, except
/// one the session answers when it ends, such as `shell`: acknowledgements
/// wait on that queue so the client reads them in command order.
#[allow(
    clippy::too_many_arguments,
    reason = "the command, its session, the hub, the client and the relays are one hand-off"
)]
pub(crate) fn route(
    id: &CommandId,
    session: &str,
    stripped: Map<String, Value>,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    from: Option<u64>,
    exited: bool,
) {
    let Some(bytes) = line_bytes(&stripped) else {
        return;
    };
    let order = lock(relays).order.clone();
    // Serves the command on the session's published relay, when one
    // outlives this call's check: every path below serves it and reports
    // true. False when no relay for the session is published: the caller
    // opens one. The re-check after taking the gate below reuses this
    // branch unchanged.
    let serve_existing = || -> bool {
        let mut held = lock(relays);
        let Some(at) = held.entries.iter().position(|entry| {
            entry.session == session && from.is_none_or(|from| entry.epoch > from)
        }) else {
            return false;
        };
        // Liveness before enqueue: nobody passes the queue on when
        // the thread is gone, so its unsent commands are routed
        // first, then this command, each with this bound.
        let alive = held.entries.get(at).is_some_and(|entry| {
            entry
                .thread
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
        });
        if !alive {
            let entry = held.entries.remove(at);
            drop(held);
            crate::retire::recover(
                entry,
                id,
                session,
                stripped.clone(),
                exited,
                hub,
                writer,
                relays,
                from,
            );
            return true;
        }
        // A new command joins the session's acknowledgement queue in
        // read order, except one the session answers when it ends.
        // After the liveness check, so a recovered queue keeps its
        // read order ahead of this command.
        if from.is_none() {
            crate::retire::enqueue_new(&order, session, id, &stripped);
        }
        if held
            .entries
            .get(at)
            .is_some_and(|entry| entry.retiring.is_some())
        {
            // Kept before anything else, so the relay thread passes
            // it on in the order it was read. Never written: the
            // session is exiting or gone.
            if let Some(entry) = held.entries.get(at) {
                lock(&entry.kept).push((id.0.clone(), stripped.clone(), false));
            }
            return true;
        }
        // Kept before the write, so the relay thread finds it when the
        // session answers. The drain never sees the push without the
        // write's outcome: it collects under the relays lock, which
        // this hold keeps until the write's result flips the entry.
        #[cfg(test)]
        let before_write = held.before_command_write.take();
        let sent = held.entries.get(at).is_some_and(|entry| {
            lock(&entry.kept).push((id.0.clone(), stripped.clone(), true));
            #[cfg(test)]
            if let Some(before_write) = before_write {
                before_write();
            }
            write_all(&entry.writer, &bytes).is_ok()
        });
        if sent {
            return true;
        }
        // The write failed: the session is gone. The relay keeps the
        // command unsent and reads on: only the write half shuts, so
        // every answer already in the kernel buffer is still read. A
        // command the session never answered is dropped, as when a
        // live relay's socket closes.
        if let Some(entry) = held.entries.get_mut(at) {
            entry.retiring = Some(Retire::Dead);
            if let Some(queued) = lock(&entry.kept)
                .iter_mut()
                .find(|(kept, _, _)| *kept == id.0)
            {
                queued.2 = false;
            }
            match entry.writer.shutdown(Shutdown::Write) {
                Ok(()) | Err(_) => {}
            }
        }
        true
    };
    if serve_existing() {
        return;
    }
    // No relay for the session: at most one opener runs past here. The
    // gate is cloned under the relays lock, which is dropped before
    // blocking on it; the guard is held until the attach below
    // publishes. An opener ahead publishes first: the re-check finds its
    // entry and serves there instead.
    let gate = lock(relays).gate(session);
    let _held_gate = lock(&gate);
    // Mark opening before the re-check, under one lock hold with the
    // kept read: a rejoin worker either published before this mark (the
    // check below sees its entry) or sees the mark and refuses its
    // exclusive attach, so none publishes between the check and this
    // opener's attach.
    let (kept, _opening) = {
        let mut held = lock(relays);
        (
            held.subscription(session),
            crate::rejoin::Opening::mark(&mut held, relays, session),
        )
    };
    if serve_existing() {
        // An opener ahead published first: serve there. The Opening
        // guard drops here with no lock held; its Drop takes the relays
        // lock itself.
        return;
    }
    #[cfg(test)]
    {
        // Taken out first: the pause below blocks, and must not hold
        // the hub lock while parked, so a test can re-arm it meanwhile.
        let before_open = lock(&hub.before_open).take();
        if let Some(before_open) = before_open {
            before_open();
        }
    }
    if from.is_none() {
        crate::retire::enqueue_new(&order, session, id, &stripped);
    }
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
            crate::retire::refuse(
                writer,
                hub,
                relays,
                session,
                id,
                &refused.code,
                &refused.message,
            );
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
        false,
    );
}

/// Sends the kept subscription again under a hub-minted id the relay
/// thread drops, then the client's command when one is relayed, and
/// follows the session on a relay thread: shared by a new relay and the
/// rewind redirect, which sends only the subscription (its caller keeps
/// the level first). A write that fails answers the client's command
/// `session_not_found` when there is one, in command order; without one
/// the connection is gone, so nothing is answered. Exclusive gives up when
/// the connection already relays the session or is opening it: the sweep's
/// rejoin, never a client command.
#[allow(
    clippy::too_many_arguments,
    reason = "the session, its stream, the hub, the client, the relays, the replay, the command and its exclusivity are one hand-off"
)]
pub(crate) fn attach(
    session: &str,
    stream: UnixStream,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    replay: Option<Map<String, Value>>,
    command: Option<(CommandId, Vec<u8>, Map<String, Value>)>,
    exclusive: bool,
) {
    attach_inner(
        session, stream, hub, writer, relays, replay, command, exclusive, None,
    );
}

/// Rejoins only if the candidate's mark is still current. It takes the kept
/// subscription under the relays lock and holds that lock through replay and
/// admission, so a superseding subscription can neither be missed nor
/// overwritten by the old candidate.
pub(crate) fn attach_rejoin(
    session: &str,
    stream: UnixStream,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    candidate_epoch: u64,
) {
    attach_inner(
        session,
        stream,
        hub,
        writer,
        relays,
        None,
        None,
        true,
        Some(candidate_epoch),
    );
}

#[allow(
    clippy::too_many_arguments,
    reason = "the session, its stream, the hub, the client, the relays, the replay, the command and its admission are one hand-off"
)]
fn attach_inner(
    session: &str,
    stream: UnixStream,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    replay: Option<Map<String, Value>>,
    command: Option<(CommandId, Vec<u8>, Map<String, Value>)>,
    exclusive: bool,
    candidate_epoch: Option<u64>,
) {
    let mark = crate::rejoin::Mark::now(&hub.home, session);
    let mut replay = replay;
    let admission = candidate_epoch.map(|_| lock(relays));
    if let Some(candidate_epoch) = candidate_epoch {
        let Some(current) = admission
            .as_ref()
            .and_then(|held| crate::rejoin::kept_for_candidate(held, session, candidate_epoch))
        else {
            return;
        };
        replay = Some(current);
    }
    let replayed: Replayed = Arc::new(Mutex::new(Vec::new()));
    if let Some(line) = replay.as_mut() {
        let minted = crate::start::mint("c_");
        line.insert("id".to_owned(), Value::String(minted.clone()));
        let sent = line_bytes(line).is_some_and(|line| write_all(&stream, &line).is_ok());
        if !sent {
            drop(admission);
            if let Some((id, _, _)) = &command {
                crate::retire::refuse_not_found(writer, hub, relays, session, id);
            }
            return;
        }
        lock(&replayed).push(minted);
    };
    let kept: Kept = Arc::new(Mutex::new(
        command
            .as_ref()
            .map(|(id, _, stripped)| (id.0.clone(), stripped.clone(), true))
            .into_iter()
            .collect(),
    ));
    if let Some((id, bytes, _)) = &command
        && write_all(&stream, bytes).is_err()
    {
        drop(admission);
        crate::retire::refuse_not_found(writer, hub, relays, session, id);
        return;
    }
    let reader = match stream.try_clone() {
        Ok(reader) => reader,
        Err(_) => {
            drop(admission);
            if let Some((id, _, _)) = &command {
                crate::retire::refuse_not_found(writer, hub, relays, session, id);
            }
            return;
        }
    };
    let order = admission
        .as_ref()
        .map_or_else(|| lock(relays).order.clone(), |held| held.order.clone());
    let mut held = match admission {
        Some(held) => held,
        None => lock(relays),
    };
    let epoch = held.mint();
    if !crate::rejoin::admit(&mut held, session, exclusive, mark, epoch) {
        drop(held);
        stream.shutdown(Shutdown::Both).unwrap_or(());
        return;
    }
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
            order: Arc::clone(&order),
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
            retiring: None,
        });
    }
}

/// What one relay thread owns besides its streams: its epoch, the ids of
/// the subscriptions it sent again, its entry's unacknowledged commands,
/// which outlive the entry, and the connection's acknowledgement queue:
/// acknowledgements wait on it so the client reads them in command order
/// without the thread taking the relays lock to find it.
struct RelayThread {
    epoch: u64,
    replayed: Replayed,
    kept: Kept,
    order: Arc<AckOrder>,
}

fn not_found(writer: &Arc<Mutex<UnixStream>>, hub: &Hub, id: &CommandId, session: &str) {
    let refused = crate::resume::not_found(&SessionId(session.to_owned()));
    reject(writer, hub, Some(id), &refused.code, &refused.message);
}

/// Copies every session line back verbatim onto the client's shared
/// writer, except the acknowledgement of the subscription the hub sent
/// again, which the client never sent, and a `closing` answer from a
/// session whose log ends in `fiber_exited`: that retires this thread's
/// relay and routes the command again, passing its queue on after. An
/// accepted `subscribe` becomes the connection's kept subscription before
/// its acknowledgement is forwarded, so a client that has read it and
/// triggers a reconnect gets that level replayed. The session closing
/// its socket drops the map entry once its queue is empty; the next
/// command for it reconnects.
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
        order,
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
        #[cfg(test)]
        if let Some(before_read) = lock(&order.before_read).take() {
            before_read();
        }
        buf.clear();
        match read.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => {
                ended = true;
                break;
            }
            Ok(_) => {
                let replayed_ack = muted(&buf, &replayed);
                #[cfg(test)]
                {
                    if let Some(after_filter) = lock(&hub.after_replay_filter).take() {
                        after_filter(&buf, replayed_ack);
                    }
                }
                if replayed_ack {
                    continue;
                }
                // An acknowledgement for a command the relay no longer
                // keeps was passed on to the resumed session, which
                // answers it there: it is not forwarded again. Read before
                // `settle` removes the acknowledgement it consumes.
                let answered = acknowledgement(&buf).map(|(id, _)| id);
                let known = answered.as_ref().is_some_and(|id| kept_has(&kept, id));
                match settle(&buf, &kept, hub, session, exiting) {
                    Some(Settled::Reroute(id, line)) => {
                        exiting = true;
                        if crate::retire::hand_over(&order, relays, session, epoch, &id, &line) {
                            route(
                                &CommandId(id),
                                session,
                                line,
                                hub,
                                writer,
                                relays,
                                Some(epoch),
                                true,
                            );
                            crate::retire::pass_on(session, epoch, &kept, hub, writer, relays);
                        }
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
                        let first = lock(relays).accepted(session, epoch, line);
                        // The session counts this client before it
                        // acknowledges, so the prompt finds it connected.
                        if let Some(first) = first {
                            first.release();
                        }
                    }
                    None => {
                        if answered.is_some() && !known {
                            continue;
                        }
                    }
                }
                if crate::retire::forward(&buf, writer, relays, hub, &order, epoch).is_err() {
                    break;
                }
                // A retiring relay passes its queue on once the
                // acknowledgement ahead of it reached the client, so the
                // replacement never answers a command first.
                if crate::retire::is_retiring(relays, session, epoch) {
                    crate::retire::pass_on(session, epoch, &kept, hub, writer, relays);
                }
            }
        }
    }
    crate::retire::drain(session, epoch, &kept, &order, hub, writer, relays);
    if ended {
        crate::rewind::follow(hub, writer, relays, session);
    }
}

/// Whether `kept` still holds `id`: an acknowledgement for one it does
/// not was passed on, and is answered where it went.
fn kept_has(kept: &Kept, id: &str) -> bool {
    lock(kept).iter().any(|(kept, _, _)| kept == id)
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
        let at = kept.iter().position(|(kept, _, _)| *kept == id)?;
        let (_, command, _) = kept.remove(at);
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

pub(crate) fn acknowledgement(line: &[u8]) -> Option<(String, Verdict)> {
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
