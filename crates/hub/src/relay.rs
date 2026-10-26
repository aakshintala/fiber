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
//! command, replacing its accepted full acknowledgement with `stream_reopened`
//! before the fold and muting other replay acknowledgements. A `subscribe`
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
//! last held, with no client command (`crate::rejoin`), and a full replay
//! sends `stream_reopened` before the fold. A running session
//! that closes the connection gets `stream_closed` and loses the kept
//! level first (`crate::closed`).

use std::io::BufReader;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// The session server the entry writes to, as a live pid, an
    /// unreadable live target, or a synthetic replay: the target the
    /// direct-parent exception compares against.
    pub(crate) target: RelayTarget,
}

/// What the relay entry's target process is: a live session server pid, a
/// live target whose pid, parent or walk could not be read (fail closed),
/// or a synthetic replay of an exited delegate, which has no target
/// process. Never an ambiguous `Option<u32>`: a live target
/// peer-credential failure cannot be mistaken for replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelayTarget {
    /// A live session server process.
    Live { pid: u32 },
    /// A live target the check could not read: refusals fail closed.
    LiveUnreadable,
    /// A synthetic replay of an exited delegate: no target process, so no
    /// refusal applies to it.
    SyntheticReplay,
}

/// The refused command class a `from_session` message names
/// (`docs/permissions.md`, "Sessions on the sockets").
pub(crate) enum FromSessionRefusal<'a> {
    /// An approval answer to a session.
    Approval {
        /// The session the approval is for.
        target: &'a SessionId,
    },
    /// `prompt`, `steer` or `reply` to a session on the caller's walk or
    /// sharing a session process with it.
    SessionCommand {
        /// The refused wire command.
        command: &'a str,
        /// The refused session.
        target: &'a SessionId,
    },
    /// `pairing_code`.
    PairingCode,
}

/// The shared session-host predicate as a walk visit: every match is
/// collected, nearest first.
fn is_session_host(argv: &[Vec<u8>]) -> support::peer::Visit {
    if contract::session_process::is_session_process(argv) {
        support::peer::Visit::Match
    } else {
        support::peer::Visit::Skip
    }
}

impl FromSessionRefusal<'_> {
    /// The wire command the refusal names in its fallback messages.
    fn command(&self) -> &str {
        match self {
            FromSessionRefusal::Approval { .. } => "reply",
            FromSessionRefusal::SessionCommand { command, .. } => command,
            FromSessionRefusal::PairingCode => "pairing_code",
        }
    }
}

/// The `from_session` message for a refused command: the exact sentence for
/// the refusal class when the caller's walk passes a session process, the
/// unreadable pid on an incomplete caller walk, otherwise the preserved
/// peer-credential error. No session id of the caller's is ever named:
/// which session it is, Fiber reads from the process tree, never from the
/// arguments.
pub(crate) fn from_session_message(
    attribution: &crate::connection::Attribution,
    refusal: FromSessionRefusal<'_>,
) -> String {
    match attribution {
        crate::connection::Attribution::Local { .. } => match refusal {
            FromSessionRefusal::Approval { target } => format!(
                "Only a person answers an approval. This connection comes from inside a session, so `reply` to session `{}` is refused.",
                target.0
            ),
            FromSessionRefusal::SessionCommand { command, target } => format!(
                "A connection from inside a session cannot send `{command}` to session `{}`.",
                target.0
            ),
            FromSessionRefusal::PairingCode => {
                "A connection from inside a session cannot ask for a pairing code.".to_owned()
            }
        },
        crate::connection::Attribution::Incomplete { pid: Some(pid), .. } => {
            let command = refusal.command();
            format!(
                "The process walk from this connection is incomplete at unreadable pid `{pid}`, so `{command}` is refused."
            )
        }
        crate::connection::Attribution::Incomplete { peer_error, .. } => {
            let command = refusal.command();
            format!(
                "This connection's process could not be identified ({}), so `{command}` is refused.",
                peer_error.as_deref().unwrap_or("unknown error")
            )
        }
        crate::connection::Attribution::Remote => {
            let command = refusal.command();
            format!(
                "This connection's process could not be identified (unknown error), so `{command}` is refused."
            )
        }
    }
}

/// The `from_session` message when the refused target session's own process
/// cannot be read: the target names the session, not the caller.
pub(crate) fn unreadable_target_message(target: &SessionId, command: &str) -> String {
    format!(
        "Session `{}`'s process could not be read, so `{command}` is refused.",
        target.0
    )
}

/// Whether the stripped session command answers an approval: a `reply`
/// carrying the approval answer key (`docs/invocation.md`, "Replying").
fn is_approval_reply(stripped: &Map<String, Value>) -> bool {
    stripped.get("command").and_then(Value::as_str) == Some("reply")
        && stripped
            .get("args")
            .and_then(Value::as_object)
            .is_some_and(|args| args.contains_key("decision"))
}

/// The `from_session` refusal for a session-targeted command, if any
/// (`docs/permissions.md`, "Sessions on the sockets"). Refusal 1 rejects
/// an approval answer from an owned or incomplete local connection, with
/// no exception. Refusal 2 rejects `prompt`, `steer` and `reply` when the
/// target pid is on the caller's walk, or when a session-host pid from the
/// target's parent walk is also on the caller's walk, unless the caller is
/// the target's current kernel parent with a readable target ancestry. A
/// synthetic replay has no target process, so no refusal applies to it. An
/// unreadable target pid, parent, or walk fails closed.
pub(crate) fn session_command_refusal(
    attribution: &crate::connection::Attribution,
    stripped: &Map<String, Value>,
    target: &SessionId,
    target_state: RelayTarget,
    read: &dyn support::peer::ProcessRead,
) -> Option<crate::connection::Refused> {
    use crate::connection::{Attribution, Refused};
    let caller_matches: &[u32] = match attribution {
        Attribution::Local {
            session_process_pids,
            ..
        } => {
            if session_process_pids.is_empty() {
                return None;
            }
            session_process_pids
        }
        Attribution::Incomplete { .. } => &[],
        Attribution::Remote => return None,
    };
    // A synthetic replay has no live target process and keeps its existing
    // not-found behavior. Refusal 1 applies to every live session socket,
    // regardless of the target ancestry.
    if matches!(target_state, RelayTarget::SyntheticReplay) {
        return None;
    }
    if is_approval_reply(stripped) {
        return Some(Refused {
            code: ErrorCode::FromSession,
            message: from_session_message(attribution, FromSessionRefusal::Approval { target }),
        });
    }
    // An incomplete caller walk fails closed: only the direct-parent
    // exception below still lets it through.
    let command = stripped
        .get("command")
        .and_then(Value::as_str)
        .filter(|command| *command == "prompt" || *command == "steer" || *command == "reply")?;
    let refusal = || FromSessionRefusal::SessionCommand { command, target };
    let target_pid = match target_state {
        RelayTarget::SyntheticReplay => unreachable!("synthetic replay returned above"),
        RelayTarget::LiveUnreadable => {
            return Some(Refused {
                code: ErrorCode::FromSession,
                message: unreadable_target_message(target, command),
            });
        }
        RelayTarget::Live { pid } => pid,
    };
    let target_walk = match support::peer::walk_ancestors(target_pid, read, is_session_host) {
        Ok(walk) => walk,
        Err(_) => {
            return Some(Refused {
                code: ErrorCode::FromSession,
                message: unreadable_target_message(target, command),
            });
        }
    };
    // Parent and session-host ancestors come from one kernel walk, so a
    // second parent read cannot authorize against a different process tree.
    if target_walk.parent_pid == attribution.peer_pid() {
        return None;
    }
    match attribution {
        Attribution::Incomplete { .. } => Some(Refused {
            code: ErrorCode::FromSession,
            message: from_session_message(attribution, refusal()),
        }),
        Attribution::Remote => None,
        Attribution::Local { .. } => {
            if caller_matches.contains(&target_pid)
                || target_walk
                    .matched_pids
                    .iter()
                    .any(|pid| caller_matches.contains(pid))
            {
                Some(Refused {
                    code: ErrorCode::FromSession,
                    message: from_session_message(attribution, refusal()),
                })
            } else {
                None
            }
        }
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
/// that fails is rejected with its failure. The refusal checks run in
/// [`route`], where the target session pid is known.
pub(crate) fn relay_command(
    id: &CommandId,
    session: &str,
    value: &Value,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    attribution: &crate::connection::Attribution,
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
    route(
        id,
        session,
        stripped,
        hub,
        writer,
        relays,
        None,
        false,
        Some(attribution),
    );
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
    admit: Option<&crate::connection::Attribution>,
) {
    let Some(bytes) = line_bytes(&stripped) else {
        return;
    };
    // An initial route receives attribution from the accepted connection.
    // A reroute can start in the relay thread or the queue pump, so recover
    // the same immutable connection attribution from its Relays state.
    let fallback_admit = admit
        .is_none()
        .then(|| lock(relays).attribution.clone())
        .flatten();
    let admit = admit.or(fallback_admit.as_ref());
    let order = lock(relays).order.clone();
    // Both refusal classes run against a live target below. Unknown and
    // exited sessions keep their existing not-found behavior.
    // A `full` subscribe for a finished delegate never reaches a relay,
    // an opener or a resume: the exited run's log is the view, replayed
    // below, and the kept subscription follows a resume. Above
    // `serve_existing`, so the live relay never sees the command. The
    // credential check runs first, so a malformed subscribe gains no
    // accepted answer.
    if stripped.get("command").and_then(Value::as_str) == Some("subscribe")
        && stripped
            .get("args")
            .and_then(|args| args.get("level"))
            .and_then(Value::as_str)
            == Some("full")
    {
        if let Err(refused) = startup_credential(&stripped) {
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
        if crate::resume::exited_delegate(&hub.home, &SessionId(session.to_owned())) {
            replay_exited_delegate(id, session, &stripped, hub, writer, relays);
            return;
        }
    }
    // Serves the command on the session's published relay, when one
    // outlives this call's check: every path below serves it and reports
    // true. False when no relay for the session is published: the caller
    // opens one. The re-check after taking the gate below reuses this
    // branch unchanged.
    let serve_existing = || -> bool {
        // Snapshot the target under the lock; the owner and parent reads
        // below hold no lock.
        let snapshot = {
            let held = lock(relays);
            held.entries.iter().find_map(|entry| {
                (entry.session == session && from.is_none_or(|from| entry.epoch > from))
                    .then_some((entry.epoch, entry.target))
            })
        };
        let Some((epoch, target_state)) = snapshot else {
            return false;
        };
        if let Some(attribution) = admit {
            let read = crate::connection::read_for(hub);
            if let Some(refused) = session_command_refusal(
                attribution,
                &stripped,
                &SessionId(session.to_owned()),
                target_state,
                read.as_ref(),
            ) {
                crate::retire::refuse(
                    writer,
                    hub,
                    relays,
                    session,
                    id,
                    &refused.code,
                    &refused.message,
                );
                return true;
            }
            // The entry may have turned over while the kernel was read:
            // only the snapshotted epoch may route.
            let current = lock(relays)
                .entries
                .iter()
                .any(|entry| entry.session == session && entry.epoch == epoch);
            if !current {
                return false;
            }
        }
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
                admit,
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
    // With no relay published, the target identity comes from the newly
    // connected socket: a peer-credential failure is a live unreadable
    // target, never a replay. The refusal runs before the relay publishes
    // or the pending command is written.
    let target_state = match support::peer::peer_pid(&stream) {
        Ok(pid) => RelayTarget::Live { pid },
        Err(_) => RelayTarget::LiveUnreadable,
    };
    if let Some(attribution) = admit {
        let read = crate::connection::read_for(hub);
        if let Some(refused) = session_command_refusal(
            attribution,
            &stripped,
            &SessionId(session.to_owned()),
            target_state,
            read.as_ref(),
        ) {
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
    }
    // The connection's subscription first, under an id of the hub's own,
    // so the client's command reaches a subscribed connection.
    attach_inner(
        session,
        stream,
        hub,
        writer,
        relays,
        kept,
        Some((id.clone(), bytes, stripped)),
        false,
        None,
        true,
    );
}

/// Refuses `id` for `session` when its exited log cannot be replayed:
/// the freshly cut socket pair, its clone, or the relay thread failed to
/// start. One `IoFailed` wording for all three, so a transient resource
/// shortage reads the same wherever the replay fails.
fn could_not_replay(
    writer: &Arc<Mutex<UnixStream>>,
    hub: &Arc<Hub>,
    relays: &Arc<Mutex<Relays>>,
    session: &str,
    id: &CommandId,
) {
    crate::retire::refuse(
        writer,
        hub,
        relays,
        session,
        id,
        &ErrorCode::IoFailed,
        &format!("The hub could not replay session `{session}`."),
    );
}

/// Serves a `full` subscribe for a finished delegate from its log: the
/// replay branch of [`route`] calls this when
/// [`crate::resume::exited_delegate`] holds, before any opener, relay or
/// resume. The fold the exited run left is the view: one relay thread owns
/// a socket pair preloaded with the accept and the fold, so settling,
/// forwarding and teardown run exactly as for a session close. The entry
/// is born retiring, so a command routed while it drains queues and is
/// passed on, never written into the preloaded void; the kept `full`
/// subscription the accept settles survives the teardown, so the rejoin
/// sweep follows the delegate into its resumed run with no client command.
/// A log that vanished answers `session_not_found`, one that cannot be
/// read or serialised `log_corrupt`: never a partial fold, never a hang.
fn replay_exited_delegate(
    id: &CommandId,
    session: &str,
    stripped: &Map<String, Value>,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
) {
    let Some((_, dir)) = crate::recent::lookup_logged(&hub.home, &SessionId(session.to_owned()))
    else {
        crate::retire::refuse_not_found(writer, hub, relays, session, id);
        return;
    };
    let fold = match exited_fold(&dir) {
        Ok(fold) => fold,
        Err(code) => {
            crate::retire::refuse(
                writer,
                hub,
                relays,
                session,
                id,
                &code,
                &format!("The log of session `{session}` cannot be replayed."),
            );
            return;
        }
    };
    let last = fold.last().and_then(|line| line.seq.map(|seq| seq.0));
    let mut result = Map::new();
    if let Some(last) = last {
        result.insert("last_seq".into(), Value::from(last));
    }
    let mut payload = Map::new();
    payload.insert("command_id".into(), Value::String(id.0.clone()));
    payload.insert("result".into(), Value::Object(result));
    let mut preloaded = crate::connection::line(hub, "command_accepted", payload);
    preloaded.push(b'\n');
    for line in &fold {
        let Ok(bytes) = serde_json::to_vec(line) else {
            crate::retire::refuse(
                writer,
                hub,
                relays,
                session,
                id,
                &ErrorCode::LogCorrupt,
                &format!("The log of session `{session}` cannot be replayed."),
            );
            return;
        };
        preloaded.extend_from_slice(&bytes);
        preloaded.push(b'\n');
    }
    let order = lock(relays).order.clone();
    crate::retire::enqueue_new(&order, session, id, stripped);
    let kept: Kept = Arc::new(Mutex::new(vec![(id.0.clone(), stripped.clone(), true)]));
    let mark = crate::rejoin::Mark::now(&hub.home, session);
    let (hub_end, preload_end) = match UnixStream::pair() {
        Ok(pair) => pair,
        Err(_) => {
            could_not_replay(writer, hub, relays, session, id);
            return;
        }
    };
    let reader = match hub_end.try_clone() {
        Ok(reader) => reader,
        Err(_) => {
            could_not_replay(writer, hub, relays, session, id);
            return;
        }
    };
    let mut held = lock(relays);
    // The entry is born retiring, so it takes no writes, and a command
    // routed while it drains queues on it and is passed on at its end.
    // Admission with exclusive=false always succeeds, so Refused never
    // fires here; it shares the failure answer.
    match establish(
        session,
        hub,
        writer,
        relays,
        &mut held,
        Establishment {
            mark,
            exclusive: false,
            stream: hub_end,
            reader,
            kept,
            reopen: None,
            replayed: Replayed::default(),
            retiring: Some(Retire::Dead),
            pending: None,
            // A synthetic replay has no target process: the socket pair
            // is the hub's own, so its peer pid says nothing.
            target: RelayTarget::SyntheticReplay,
        },
    ) {
        Established::Running => {}
        Established::SpawnFailed(_) | Established::Refused(_, _) => {
            drop(held);
            could_not_replay(writer, hub, relays, session, id);
            return;
        }
    }
    drop(held);
    // The relay thread drains this side: a write that fails leaves a
    // thread that ends at once, and its drain drops the kept command
    // without an answer, as a session close does today.
    if write_all(&preload_end, &preloaded).is_ok() {
        preload_end.shutdown(Shutdown::Write).unwrap_or(());
    }
}

/// The durable lines of an exited delegate's log, root first: the fold a
/// `full` subscribe replays. `NotFound` when the log vanished mid-route,
/// `LogCorrupt` when its history cannot be read.
fn exited_fold(dir: &std::path::Path) -> Result<Vec<contract::Envelope>, ErrorCode> {
    let segments = log::history(dir).map_err(|error| match error {
        log::Error::NotFound(_) => ErrorCode::SessionNotFound,
        // Only an append meets `Ended`; a history is read.
        log::Error::Ended { .. } => ErrorCode::Closing,
        log::Error::Held { .. }
        | log::Error::Poisoned { .. }
        | log::Error::Ambiguous { .. }
        | log::Error::Exists(_)
        | log::Error::Pointer { .. }
        | log::Error::Unreadable { .. }
        | log::Error::Encode(_)
        | log::Error::Io { .. } => ErrorCode::LogCorrupt,
    })?;
    let mut fold = Vec::new();
    for segment in &segments {
        match segment.lines(0) {
            Ok(lines) => fold.extend(lines),
            Err(_) => return Err(ErrorCode::LogCorrupt),
        }
    }
    if fold.is_empty() {
        return Err(ErrorCode::LogCorrupt);
    }
    Ok(fold)
}

/// Everything `establish` needs beyond the connection: the mark to admit
/// under, the stream the entry writes and the reader its thread drains,
/// the kept subscription, the replayed ids the thread mutes, why the
/// entry no longer takes writes, and `attach_inner`'s pending-replay guard,
/// moved into the pending slot once the worker can observe its
/// acknowledgement (the replay path passes none).
struct Establishment {
    mark: crate::rejoin::Mark,
    exclusive: bool,
    stream: UnixStream,
    reader: UnixStream,
    kept: Kept,
    reopen: Option<String>,
    replayed: Replayed,
    retiring: Option<Retire>,
    pending: Option<(String, log::diag::Timer)>,
    target: RelayTarget,
}

/// What `establish` leaves behind: a thread running behind a pushed entry,
/// a refused admission (no entry pushed, the stream back for the caller to
/// shut down), or a thread that never started (no entry pushed). The
/// pending-replay guard rides along for the caller to emit outside the
/// relays lock.
enum Established {
    Running,
    Refused(Option<log::diag::Timer>, UnixStream),
    SpawnFailed(Option<log::diag::Timer>),
}

/// Mints the epoch, admits the session, starts the relay thread and pushes
/// its entry: the establishment `attach_inner` and `replay_exited_delegate`
/// share. The entry is pushed only when the thread runs. The caller answers
/// the other two its own way: `attach_inner` shuts the returned stream on
/// a refusal and emits the guard outside the lock on either failure; the
/// replay path refuses the command.
fn establish(
    session: &str,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    held: &mut std::sync::MutexGuard<'_, Relays>,
    establishment: Establishment,
) -> Established {
    let Establishment {
        mark,
        exclusive,
        stream,
        reader,
        kept,
        reopen,
        replayed,
        retiring,
        pending,
        target,
    } = establishment;
    let epoch = held.mint();
    // The guard moves into the pending slot before the worker can run:
    // its acknowledgement then completes it instead of leaking, and a
    // failed admission or spawn below takes it back out again.
    let pending_key = pending.map(|(minted, timer)| {
        let key = (session.to_owned(), epoch, minted);
        held.pending_replay.insert(key.clone(), timer);
        key
    });
    let admitted = crate::rejoin::admit(held, session, exclusive, mark, epoch);
    // A non-exclusive admission always succeeds, so only `attach_inner`
    // sees a refusal.
    if !admitted {
        let guard = pending_key
            .as_ref()
            .and_then(|key| held.pending_replay.remove(key));
        return Established::Refused(guard, stream);
    }
    let owned = RelayThread {
        epoch,
        reopen,
        replayed: Arc::clone(&replayed),
        kept: Arc::clone(&kept),
        order: Arc::clone(&held.order),
        gone: held.rejoin.closed_flag(),
    };
    // The thread may run before its entry is pushed: on session EOF it
    // only removes an entry it finds.
    let relayed = thread::Builder::new().name("hub-relay".to_owned()).spawn({
        let hub = Arc::clone(hub);
        let writer = Arc::clone(writer);
        let relays = Arc::clone(relays);
        let session = session.to_owned();
        move || relay(owned, &session, reader, &hub, &writer, &relays)
    });
    let Ok(thread) = relayed else {
        let guard = pending_key
            .as_ref()
            .and_then(|key| held.pending_replay.remove(key));
        return Established::SpawnFailed(guard);
    };
    held.entries.push(Relay {
        session: session.to_owned(),
        epoch,
        writer: stream,
        kept,
        replayed,
        thread: Some(thread),
        retiring,
        target,
    });
    Established::Running
}

/// Sends the kept subscription again under a hub-minted id the relay
/// thread mutes (without `stream_reopened` for this redirect), then the
/// client's command when one is relayed, and
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
        session, stream, hub, writer, relays, replay, command, exclusive, None, false,
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
        true,
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
    reopen: bool,
) {
    let mark = crate::rejoin::Mark::now(&hub.home, session);
    let mut replay = replay;
    let mut replayed_id: Option<String> = None;
    let mut reopening = None;
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
        if reopen
            && line
                .get("args")
                .and_then(|args| args.get("level"))
                .and_then(Value::as_str)
                == Some("full")
        {
            reopening = Some(minted.clone());
        }
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
    let mut held = match admission {
        Some(held) => held,
        None => lock(relays),
    };
    // The target the direct-parent exception compares against: the kernel
    // peer of the session socket, read before the entry publishes. A
    // peer-credential failure is a live unreadable target, never a replay.
    let target = match support::peer::peer_pid(&stream) {
        Ok(pid) => RelayTarget::Live { pid },
        Err(_) => RelayTarget::LiveUnreadable,
    };
    // A thread that never started leaves no entry: the next command for
    // the session reconnects. Its pending replay guard goes back with
    // it, emitted outside the lock.
    match establish(
        session,
        hub,
        writer,
        relays,
        &mut held,
        Establishment {
            mark,
            exclusive,
            stream,
            reader,
            kept,
            reopen: reopening,
            replayed,
            retiring: None,
            pending: replayed_id.zip(replay_timer.take()),
            target,
        },
    ) {
        Established::Running => {}
        Established::Refused(guard, stream) => {
            drop(held);
            drop(guard);
            stream.shutdown(Shutdown::Both).unwrap_or(());
        }
        Established::SpawnFailed(guard) => {
            drop(held);
            drop(guard);
        }
    }
}

/// What one relay thread owns besides its streams: its epoch, the ids of
/// the subscriptions it sent again, its entry's unacknowledged commands,
/// which outlive the entry, and the connection's acknowledgement queue:
/// acknowledgements wait on it so the client reads them in command order
/// without the thread taking the relays lock to find it.
struct RelayThread {
    epoch: u64,
    reopen: Option<String>,
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
                    reopen: None,
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
/// again, replaced at full by `stream_reopened` before the fold on a
/// resume or reconnect, and a `closing` answer from a
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
        mut reopen,
    } = owned;
    let mut read = BufReader::new(reader);
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
        match support::framing::read_line(&mut read, support::framing::MAX_LINE) {
            Ok(None) | Err(_) => {
                ended = true;
                break;
            }
            Ok(Some(mut buf)) => {
                // The forwarded line keeps its one trailing line feed.
                buf.push(b'\n');
                let mut ack = acknowledgement(&buf);
                let replayed_ack = muted(ack.as_ref(), &replayed);
                #[cfg(test)]
                {
                    if let Some(after_filter) = lock(&hub.after_replay_filter).take() {
                        after_filter(&buf, replayed_ack);
                    }
                }
                if replayed_ack {
                    if let Some(ack) = ack.as_ref()
                        && reopen.as_deref() == Some(ack.id.as_str())
                    {
                        reopen = None;
                        if ack.verdict == Verdict::Accepted {
                            let mut payload = Map::new();
                            payload.insert("session_id".into(), Value::String(session.to_owned()));
                            if let Some(last_seq) = ack.last_seq {
                                payload.insert("last_seq".into(), Value::from(last_seq));
                            }
                            crate::connection::send(writer, hub, "stream_reopened", payload);
                        }
                    }
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
                        // A command re-routes once: the session it was
                        // handed to also ended without answering, so its
                        // own `closing` goes to the client as it is.
                        if order.hand_once(&id) {
                            if crate::retire::hand_over(&order, relays, session, epoch, &id, &line)
                            {
                                route(
                                    &CommandId(id),
                                    session,
                                    line,
                                    hub,
                                    writer,
                                    relays,
                                    Some(epoch),
                                    true,
                                    None,
                                );
                                crate::retire::pass_on(session, epoch, &kept, hub, writer, relays);
                            } else {
                                order.forget_handed(&id);
                            }
                            continue;
                        }
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
                                        last_seq: None,
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
    // A loop that broke on a failed `forward` did not see the session
    // close: the client is gone.
    let client_gone = gone.load(Ordering::SeqCst) || !ended;
    crate::retire::drain(
        session,
        epoch,
        &kept,
        &order,
        hub,
        writer,
        relays,
        exiting,
        client_gone,
    );
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
    // Pre-framed buffers carry their line feed; the writer takes the line.
    let line = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let mut out: &UnixStream = stream;
    support::framing::write_line(&mut out, line)
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;
