//! Starting the session a `rewound` or `moved` line names
//! (`docs/invocation.md`, "`rewind` starts a new session process"): the
//! relay starts it when it carries an accepted `rewind` or
//! `change_workspace`, and the feed starts it when a followed session's
//! socket closes on a log ending `rewound` or `moved`, so a transition no
//! hub client relays still starts the new session. Every start goes
//! through one function and a per-session start lock, never the shared
//! resume gate: a slow start blocks only the threads starting that same
//! session. A start that fails writes one diagnostic line with a fixed
//! sentence and no detail, and the relay answers the command with the
//! start's code instead of the acknowledgement.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use contract::{ErrorCode, SessionId};
use log::diag::Severity;
use serde_json::Value;

use crate::connection::{Hub, lock};
use crate::relay::{attach, valid_session_id};
use crate::start::{self, Bind};

/// The session `session`'s log continues in: `Some` when its log's last
/// line is `rewound` or `moved` naming a minted-shape new session, `None`
/// otherwise, so a crash or an exit starts nothing.
pub(crate) fn continued(home: &Path, session: &SessionId) -> Option<SessionId> {
    let (_, dir) = crate::recent::lookup_logged(home, session)?;
    continued_in(&dir)
}

/// The session the log in `dir` continues in, from its last line alone:
/// the child a `rewound` or `moved` line names. The child starts in the
/// old session's workspace either way; a moved child moves to its target
/// itself, so the hub never reads the target workspace here.
pub(crate) fn continued_in(dir: &Path) -> Option<SessionId> {
    let line = log::last_line(dir)?;
    if line.kind == "moved" {
        let moved: contract::events::Moved =
            serde_json::from_value(Value::Object(line.payload)).ok()?;
        if !valid_session_id(&moved.new_session_id.0) {
            return None;
        }
        return Some(moved.new_session_id);
    }
    if line.kind != "rewound" {
        return None;
    }
    let rewound: contract::events::Rewound =
        serde_json::from_value(Value::Object(line.payload)).ok()?;
    if !valid_session_id(&rewound.new_session_id.0) {
        return None;
    }
    Some(rewound.new_session_id)
}

/// Calls the feed's `on_rewound` with `(from, next)` when the log in
/// `found`'s directory ends `rewound` or `moved`: the callback's body, so
/// `feed.rs` holds only the field and this call.
pub(crate) fn notify_left(
    callback: &OnceLock<Box<dyn Fn(SessionId, SessionId) + Send + Sync>>,
    from: &str,
    found: &Option<(String, PathBuf, u64)>,
) {
    let Some(callback) = callback.get() else {
        return;
    };
    let Some((_, dir, _)) = found.as_ref() else {
        return;
    };
    if let Some(next) = continued_in(dir) {
        callback(SessionId(from.to_owned()), next);
    }
}

/// Connects to `next`'s socket, the session `from`'s `rewound` or `moved`
/// names: the running session's, a resumed one's when it has a log, or
/// one [`crate::Starter::rewind`] starts in the workspace `from`'s log
/// recorded, waiting for its socket. Under `next`'s own lock in
/// [`Hub::starting`], released before [`crate::resume::resume`], which
/// waits out an exiting process under the resume gate: no start ever
/// holds the shared gate, and no resume ever holds a start lock. A
/// failure writes one diagnostic line and starts nothing; the relay
/// answers the rewind with the start's code.
pub(crate) fn reach(
    hub: &Arc<Hub>,
    from: &SessionId,
    next: &SessionId,
) -> Result<UnixStream, crate::connection::Refused> {
    // Admission first: a rewind admitted before the store reaches its
    // bind or refuses, and one after is refused at once. The guard
    // spans the whole reach.
    let admitted = hub.admit()?;
    let guard = guard_for(hub, next);
    #[cfg(test)]
    crate::connection::probe_retry_contention(hub, &guard);
    let held = lock(&guard);
    reach_locked(hub, from, next, held, admitted)
}

/// The body of [`reach`] after admission and the per-child guard: the
/// socket fast path, the resume fallback, and the recorded-workspace
/// start with its bind wait. The caller holds both guards across the
/// call, so two starts of one session serialize; the resume path drops
/// the start guard before the shared gate, as [`reach`] documents.
fn reach_locked(
    hub: &Arc<Hub>,
    from: &SessionId,
    next: &SessionId,
    held: std::sync::MutexGuard<'_, ()>,
    _admitted: crate::connection::Admitted<'_>,
) -> Result<UnixStream, crate::connection::Refused> {
    let socket = hub.home.join("run").join(&next.0);
    if let Ok(stream) = UnixStream::connect(&socket) {
        return Ok(stream);
    }
    if crate::recent::lookup_logged(&hub.home, next).is_some() {
        drop(held);
        return match crate::resume::resume(hub, next, None) {
            Ok(stream) => Ok(stream),
            Err(refused) => {
                // A stopping refusal passes through unchanged: the
                // session is not resumed because the hub is stopping,
                // not because the resume failed.
                if refused == crate::connection::stopping() {
                    return Err(refused);
                }
                hub.diag.line(
                    Severity::Warn,
                    Some(next),
                    &start::code_name(&refused.code),
                    &format!("Session {} could not resume.", next.0),
                );
                // The resume's own message names the session or the io
                // detail, so the client gets a fixed sentence instead.
                Err(crate::connection::Refused {
                    code: refused.code,
                    message: "The new session could not be resumed.".to_owned(),
                })
            }
        };
    }
    let workspace = match crate::recent::lookup_logged(&hub.home, from)
        .and_then(|(_, dir)| crate::resume::recorded(&dir))
    {
        Some(recorded) => recorded.workspace,
        None => {
            hub.diag.line(
                Severity::Warn,
                Some(next),
                "io_failed",
                &format!("Session {} could not start.", next.0),
            );
            return Err(crate::connection::Refused {
                code: ErrorCode::IoFailed,
                message: "The new session could not start.".to_owned(),
            });
        }
    };
    let started = match hub.starter.rewind(next, &workspace, from) {
        Ok(started) => started,
        Err(_) => {
            hub.diag.line(
                Severity::Warn,
                Some(next),
                "io_failed",
                &format!("Session {} could not start.", next.0),
            );
            return Err(crate::connection::Refused {
                code: ErrorCode::IoFailed,
                message: "The new session could not start.".to_owned(),
            });
        }
    };
    match start::await_bind(hub, &socket, started.as_ref()) {
        Bind::Connected(stream) => {
            hub.diag.line(
                Severity::Info,
                Some(next),
                "session_started",
                &format!("Session {} started after a rewind.", next.0),
            );
            Ok(stream)
        }
        Bind::Exited => {
            let failure = started.exited();
            // The log keeps the code and a fixed sentence with the id:
            // the message may hold starter or io text. The client gets
            // the new process's own sentence, unless it is `io_failed`
            // or names the new session, when it gets a fixed sentence.
            let code = failure.as_ref().map(|failure| failure.code.clone());
            hub.diag.line(
                Severity::Warn,
                Some(next),
                &start::code_name(code.as_ref().unwrap_or(&ErrorCode::IoFailed)),
                &format!("Session {} could not start.", next.0),
            );
            Err(match failure {
                Some(failure) if failure.code == ErrorCode::IoFailed => {
                    crate::connection::Refused {
                        code: ErrorCode::IoFailed,
                        message: "The new session could not start.".to_owned(),
                    }
                }
                // A structured sentence naming the new session is not
                // forwarded either: the code is kept with the fixed
                // sentence, so no new id reaches a client.
                Some(failure) if failure.message.contains(&next.0) => crate::connection::Refused {
                    code: failure.code,
                    message: "The new session could not start.".to_owned(),
                },
                Some(failure) => crate::connection::Refused {
                    code: failure.code,
                    message: failure.message,
                },
                None => crate::connection::Refused {
                    code: ErrorCode::IoFailed,
                    message: "The new session could not start.".to_owned(),
                },
            })
        }
        Bind::Stopping => {
            started.kill();
            Err(crate::connection::stopping())
        }
        Bind::TimedOut | Bind::Failed(_) => {
            // The child outlives the wait on both arms, as on `Stopping`:
            // it must never bind after the stop's scans.
            started.kill();
            hub.diag.line(
                Severity::Warn,
                Some(next),
                "io_failed",
                &format!("Session {} could not start.", next.0),
            );
            Err(crate::connection::Refused {
                code: ErrorCode::IoFailed,
                message: "The new session could not start.".to_owned(),
            })
        }
    }
}

/// Starts `next` after `from`'s old process exits: the retry both
/// old-exit callbacks (the relay-end `follow` and the feed's `on_rewound`)
/// share, so the two paths reach one decision point. The move-time
/// `reach` stays the initial attempt. The claim runs under the child's
/// start guard, so the first callback to lock it starts and the loser
/// blocks until that start finishes before connecting: it attaches to
/// the winner's child when it runs, and starts nothing when the claimed
/// attempt failed.
pub(crate) fn retry_after_exit(
    hub: &Arc<Hub>,
    from: &SessionId,
    next: &SessionId,
) -> Option<UnixStream> {
    let guard = guard_for(hub, next);
    #[cfg(test)]
    crate::connection::probe_retry_contention(hub, &guard);
    let held = lock(&guard);
    if !hub.claim_retry(next) {
        drop(held);
        let socket = hub.home.join("run").join(&next.0);
        return UnixStream::connect(&socket).ok();
    }
    let Ok(admitted) = hub.admit() else {
        return None;
    };
    reach_locked(hub, from, next, held, admitted).ok()
}

/// Subscribes the connection to the session `old`'s log continues in, at
/// the level it held on `old`: connects to the new session, starting or
/// resuming it, and sends the kept subscription under a hub-minted id
/// whose acknowledgement the relay thread drops, as a reconnect does. A
/// connection with no kept level still retries the recorded child once,
/// attaching nothing: the retry is not conditional on the level, so it
/// runs even when the feed never observes the old process exit. One
/// already holding the level on the new session, or a log that continues
/// nowhere, changes nothing. When the client already relayed the new
/// session, still unsubscribed -- its command for it landed while the old
/// relay waited for EOF -- the level is transferred onto that relay
/// instead of starting a second one.
pub(crate) fn follow(
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<crate::relay::Relays>>,
    old: &str,
) {
    let from = SessionId(old.to_owned());
    let Some(next) = continued(&hub.home, &from) else {
        return;
    };
    let Some(replay) = lock(relays).subscription(old) else {
        drop(retry_after_exit(hub, &from, &next));
        return;
    };
    // The new session opens once: the sweep never opens it while this
    // transfer, reach and attach hold it opening, and a client command
    // racing this redirect waits on its gate instead of publishing a
    // second relay. The relays lock is never held while blocking on the
    // gate; the guard is held through the attach below.
    let gate = lock(relays).gate(&next.0);
    #[cfg(test)]
    let at_gate = lock(relays).at_gate.take();
    #[cfg(test)]
    if let Some(at_gate) = at_gate {
        at_gate();
    }
    #[cfg(test)]
    let blocked_gate = lock(relays).blocked_gate.take();
    #[cfg(test)]
    let _held_gate = crate::connection::lock_gate(&gate, blocked_gate);
    #[cfg(not(test))]
    let _held_gate = lock(&gate);
    let _opening = {
        let mut held = lock(relays);
        crate::rejoin::Opening::mark(&mut held, relays, &next.0)
    };
    #[cfg(test)]
    {
        // Taken out first: the pause below blocks, and must not hold
        // the hub lock while parked, so a test can re-arm it meanwhile.
        let before_open = lock(&hub.before_open).take();
        if let Some(before_open) = before_open {
            before_open();
        }
    }
    // Under one lock hold: the client may have relayed the new session,
    // still unsubscribed, while the old relay waited for EOF. `transfer`
    // keeps the level and carries it onto that relay, or reports that no
    // relay exists and the level is only kept.
    if lock(relays).transfer(&next.0, &replay) {
        return;
    }
    let Some(stream) = retry_after_exit(hub, &from, &next) else {
        return;
    };
    attach(
        &next.0,
        stream,
        hub,
        writer,
        relays,
        Some(replay),
        None,
        false,
    );
}

/// `next`'s lock in [`Hub::starting`]: one mutex per next session, so two
/// starts of one session serialize while any other start runs at once.
fn guard_for(hub: &Hub, next: &SessionId) -> Arc<Mutex<()>> {
    let mut starting = lock(&hub.starting);
    Arc::clone(
        &starting
            .entry(next.clone())
            .or_insert_with(crate::connection::StartSlot::new)
            .guard,
    )
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
