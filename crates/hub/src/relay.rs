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
//! last held, with no client command (`crate::rejoin`). A running session
//! that closes the connection gets `stream_closed` and loses the kept
//! level first (`crate::closed`).

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;

use contract::diag::DebugLog;
use contract::{CommandId, ErrorCode, SessionId};
use serde_json::{Map, Value};

mod ack;
mod relays;

use crate::connection::{Hub, Refused, lock, reject};
use crate::retire::{AckOrder, Retire};

#[cfg(test)]
pub(crate) use ack::ack_parses;
#[cfg(test)]
pub(crate) use ack::acknowledges;
pub(crate) use ack::{Ack, acknowledgement};
use ack::{Settled, Verdict, kept_has, muted, settle};
pub(crate) use relays::Relays;

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

/// The credential label an opening `subscribe` asks the resumed session
/// to start on: `None` when the command names no label. Only a subscribe
/// whose args name `credential` is read here, as the closed command line
/// (where an optional key is absent, never null), so a malformed value
/// or an unknown arg is refused `invalid_arguments` before any process
/// starts instead of launching one on a partially read hint. Anything
/// else is left for the session to read, as today.
fn startup_credential(stripped: &Map<String, Value>) -> Result<Option<String>, Refused> {
    const UNFIT: &str = "The arguments do not fit this command.";
    let is_subscribe = stripped.get("command").and_then(Value::as_str) == Some("subscribe");
    let names_credential = stripped
        .get("args")
        .and_then(Value::as_object)
        .is_some_and(|args| args.contains_key("credential"));
    if !is_subscribe || !names_credential {
        return Ok(None);
    }
    match serde_json::from_value::<contract::commands::CommandLine>(Value::Object(stripped.clone()))
    {
        Ok(line) => {
            if let contract::commands::Command::Subscribe(args) = line.command {
                Ok(args.credential)
            } else {
                Err(Refused {
                    code: ErrorCode::InvalidArguments,
                    message: UNFIT.to_owned(),
                })
            }
        }
        Err(_) => Err(Refused {
            code: ErrorCode::InvalidArguments,
            message: UNFIT.to_owned(),
        }),
    }
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
    #[cfg(test)]
    let at_gate = lock(relays).at_gate.take();
    #[cfg(test)]
    if let Some(at_gate) = at_gate {
        at_gate();
    }
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
    // The startup label comes from the current command, never from a
    // kept subscription. A label-bearing subscribe is read as the closed
    // command line before any process starts, so a value the session
    // would refuse never launches one; anything else is left for the
    // session to read, as today.
    let credential = match startup_credential(&stripped) {
        Ok(credential) => credential,
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
    let sid = SessionId(session.to_owned());
    let opened = if exited {
        crate::resume::resume_exited(hub, &sid, credential.as_deref())
    } else {
        UnixStream::connect(hub.home.join("run").join(session))
            .or_else(|_| crate::resume::resume(hub, &sid, credential.as_deref()))
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
    let mut replayed_id: Option<String> = None;
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
    // An actual replay starts its timer here, after the candidate
    // resolved: the guard drops at once on a failure below, and moves
    // into the pending slot once the worker can observe its
    // acknowledgement. At info there is no timer, and the early return
    // above keeps none.
    let mut replay_timer = replay.as_ref().and_then(|_| {
        log::diag::timed(
            &hub.diag,
            contract::diag::Op::HubSubscribeReplay,
            None,
            None,
        )
    });
    let replayed: Replayed = Arc::new(Mutex::new(Vec::new()));
    if let Some(line) = replay.as_mut() {
        let minted = crate::start::mint("c_");
        line.insert("id".to_owned(), Value::String(minted.clone()));
        let sent = line_bytes(line).is_some_and(|line| write_all(&stream, &line).is_ok());
        if !sent {
            drop(admission);
            drop(replay_timer);
            if let Some((id, _, _)) = &command {
                crate::retire::refuse_not_found(writer, hub, relays, session, id);
            }
            return;
        }
        lock(&replayed).push(minted.clone());
        replayed_id = Some(minted);
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
    // The guard moves into the pending slot before the worker can run:
    // its acknowledgement then completes it instead of leaking, and a
    // failed admission or spawn below takes it back out again.
    let pending_key = replayed_id.zip(replay_timer.take()).map(|(minted, timer)| {
        let key = (session.to_owned(), epoch, minted);
        held.pending_replay.insert(key.clone(), timer);
        key
    });
    if !crate::rejoin::admit(&mut held, session, exclusive, mark, epoch) {
        let guard = pending_key
            .as_ref()
            .and_then(|key| held.pending_replay.remove(key));
        drop(held);
        drop(guard);
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
            gone: held.rejoin.closed_flag(),
        };
        move || relay(owned, &session, reader, &hub, &writer, &relays)
    });
    // A thread that never started leaves no entry: the next command for
    // the session reconnects. Its pending replay guard goes back with
    // it, emitted outside the lock.
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
    } else if let Some(key) = pending_key {
        let guard = held.pending_replay.remove(&key);
        drop(held);
        drop(guard);
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
    /// The connection's closed flag (`Rejoin::closed_flag`), read at the
    /// thread's end without the relays lock.
    gone: Arc<AtomicBool>,
}

/// Tests only: runs a relay thread as `attach` would, without an entry:
/// the caller pushes the entry it reads. Each argument is one the relay
/// owns, as in `attach`.
#[cfg(test)]
#[allow(
    clippy::too_many_arguments,
    reason = "tests build the relay thread's parts the way attach does"
)]
pub(crate) fn spawn_for_test(
    session: String,
    epoch: u64,
    reader: UnixStream,
    hub: Arc<Hub>,
    writer: Arc<Mutex<UnixStream>>,
    relays: Arc<Mutex<Relays>>,
    kept: Kept,
) -> thread::JoinHandle<()> {
    let (order, gone) = {
        let held = lock(&relays);
        (held.order.clone(), held.rejoin.closed_flag())
    };
    thread::Builder::new()
        .name("hub-relay-test".to_owned())
        .spawn(move || {
            relay(
                RelayThread {
                    epoch,
                    replayed: Replayed::default(),
                    kept,
                    order,
                    gone,
                },
                &session,
                reader,
                &hub,
                &writer,
                &relays,
            );
        })
        .unwrap()
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
/// triggers a reconnect gets that level replayed. A running session that
/// closed the connection gets `stream_closed` and loses the kept level
/// first (`crate::closed`); any other close drops the map entry once the
/// queue is empty, and the next command for it reconnects.
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
        gone,
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
                let mut ack = acknowledgement(&buf);
                let replayed_ack = muted(ack.as_ref(), &replayed);
                #[cfg(test)]
                {
                    if let Some(after_filter) = lock(&hub.after_replay_filter).take() {
                        after_filter(&buf, replayed_ack);
                    }
                }
                if replayed_ack {
                    // The filter hook above already ran: waiting on the
                    // relays lock here cannot hold it back, so the forced
                    // transfer interleaving stays possible. The guard
                    // drops after the lock releases, never under it. The
                    // completion is timing-only: at info no replay was
                    // registered, so neither the id allocation nor the
                    // lock is taken.
                    let guard = if hub.diag.debugging() {
                        ack.as_ref()
                            .and_then(|ack| lock(relays).take_replay(session, epoch, &ack.id))
                    } else {
                        None
                    };
                    drop(guard);
                    continue;
                }
                // An acknowledgement for a command the relay no longer
                // keeps was passed on to the resumed session, which
                // answers it there: it is not forwarded again. Read before
                // `settle` removes the acknowledgement it consumes.
                let answered = ack.as_ref().map(|ack| ack.id.clone());
                let known = answered.as_ref().is_some_and(|id| kept_has(&kept, id));
                match settle(ack.as_ref(), &kept, hub, session, exiting) {
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
                        // it running. A start that fails is answered with
                        // the start's own code instead, through the same
                        // ordered forward below, and the connection keeps
                        // no subscription for the old session, so the
                        // redirect at the socket's end moves it nowhere.
                        match crate::rewind::reach(hub, &SessionId(session.to_owned()), &next) {
                            Ok(stream) => drop(stream),
                            Err(refused) => {
                                if let Some(id) = answered.clone() {
                                    let mut rejection = crate::connection::line(
                                        hub,
                                        "command_rejected",
                                        crate::connection::rejection(
                                            Some(&CommandId(id.clone())),
                                            &refused.code,
                                            &refused.message,
                                        ),
                                    );
                                    rejection.push(b'\n');
                                    buf = rejection;
                                    ack = Some(Ack {
                                        id,
                                        verdict: Verdict::Rejected,
                                        rewound: None,
                                    });
                                    lock(relays).subscribed.retain(|(kept, _)| kept != session);
                                }
                            }
                        }
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
                if crate::retire::forward(&buf, ack.as_ref(), writer, relays, hub, &order, epoch)
                    .is_err()
                {
                    break;
                }
                // An exiting relay passes its queue on once the
                // acknowledgement ahead of it reached the client, so the
                // replacement never answers a command first. A dead relay
                // passes its queue on only at its end, after the end is
                // classified, so nothing answers before `stream_closed`.
                if crate::retire::is_exiting(relays, session, epoch) {
                    crate::retire::pass_on(session, epoch, &kept, hub, writer, relays);
                }
            }
        }
    }
    crate::closed::on_end(session, ended && !exiting, hub, writer, relays, &gone);
    crate::retire::drain(session, epoch, &kept, &order, hub, writer, relays);
    // This worker's outstanding replays complete here, before any
    // follow-on rewind work: a disconnect or EOF before acknowledgement
    // ends the timer instead of waiting for a healthy close. Each guard
    // drops after the lock releases, never under it. Both drains are
    // timing-only: at info nothing was registered, so no lock is taken.
    let unfinished = if hub.diag.debugging() {
        lock(relays).take_session_replays(session, epoch)
    } else {
        Vec::new()
    };
    drop(unfinished);
    if ended {
        crate::rewind::follow(hub, writer, relays, session);
        // Failure-stamped transfers queued above emit here, outside the
        // lock, without waiting for the target reader to exit.
        let failed = if hub.diag.debugging() {
            lock(relays).take_completed_replays()
        } else {
            Vec::new()
        };
        drop(failed);
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
