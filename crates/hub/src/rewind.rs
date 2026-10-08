//! Starting the session a `rewound` line names (`docs/invocation.md`,
//! "`rewind` starts a new session process"): the relay starts it when it
//! carries an accepted `rewind`, and the feed starts it when a followed
//! session's socket closes on a log ending `rewound`, so a rewind no hub
//! client relays still starts the new session. Every start goes through
//! one function and a per-session start lock, never the shared resume
//! gate: a slow start blocks only the threads starting that same session.
//! A start that fails writes one diagnostic line with a fixed sentence and
//! no detail, and the relay still forwards its acknowledgement.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use contract::SessionId;
use serde_json::Value;

use crate::connection::{Hub, lock};
use crate::relay::{attach, valid_session_id};
use crate::start::{self, Bind};

/// The session `session`'s log continues in: `Some` when its log's last
/// line is `rewound` naming a minted-shape new session, `None` otherwise,
/// so a crash or an exit starts nothing.
pub(crate) fn continued(home: &Path, session: &SessionId) -> Option<SessionId> {
    let log = crate::resume::find_log(home, session)?;
    continued_in(log.parent()?)
}

/// The session the log in `dir` continues in, from its last line alone.
pub(crate) fn continued_in(dir: &Path) -> Option<SessionId> {
    let line = log::last_line(dir)?;
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
/// `found`'s directory ends `rewound`: the callback's body, so `feed.rs`
/// holds only the field and this call.
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

/// Connects to `next`'s socket, the session `from`'s `rewound` names:
/// the running session's, a resumed one's when it has a log, or one
/// [`crate::Starter::rewind`] starts in the workspace `from`'s log
/// recorded, waiting for its socket. Under `next`'s own lock in
/// [`Hub::starting`], released before [`crate::resume::resume`], which
/// waits out an exiting process under the resume gate: no start ever
/// holds the shared gate, and no resume ever holds a start lock. A
/// failure writes one diagnostic line and starts nothing; the relay
/// forwards its acknowledgement either way.
pub(crate) fn reach(hub: &Arc<Hub>, from: &SessionId, next: &SessionId) -> Option<UnixStream> {
    let guard = guard_for(hub, next);
    let held = lock(&guard);
    let socket = hub.home.join("run").join(&next.0);
    if let Ok(stream) = UnixStream::connect(&socket) {
        return Some(stream);
    }
    if crate::resume::find_log(&hub.home, next).is_some() {
        drop(held);
        return match crate::resume::resume(hub, next) {
            Ok(stream) => Some(stream),
            Err(refused) => {
                hub.diag.warn_session(
                    next,
                    &start::code_name(&refused.code),
                    &format!("Session {} could not resume.", next.0),
                );
                None
            }
        };
    }
    let workspace = match crate::resume::find_log(&hub.home, from)
        .and_then(|log| crate::resume::recorded(&log))
    {
        Some(recorded) => recorded.workspace,
        None => {
            hub.diag.warn_session(
                next,
                "io_failed",
                &format!("Session {} could not start.", next.0),
            );
            return None;
        }
    };
    let started = match hub.starter.rewind(next, &workspace, from) {
        Ok(started) => started,
        Err(_) => {
            hub.diag.warn_session(
                next,
                "io_failed",
                &format!("Session {} could not start.", next.0),
            );
            return None;
        }
    };
    match start::await_bind(hub, &socket, started.as_ref()) {
        Bind::Connected(stream) => {
            hub.diag.info_session(
                next,
                "session_started",
                &format!("Session {} started after a rewind.", next.0),
            );
            Some(stream)
        }
        Bind::Exited | Bind::TimedOut | Bind::Failed(_) => {
            hub.diag.warn_session(
                next,
                "io_failed",
                &format!("Session {} could not start.", next.0),
            );
            None
        }
    }
}

/// Subscribes the connection to the session `old`'s log continues in, at
/// the level it held on `old`: connects to the new session, starting or
/// resuming it, and sends the kept subscription under a hub-minted id
/// whose acknowledgement the relay thread drops, as a reconnect does. A
/// connection with no kept level, one already holding the level on the
/// new session, or a log that continues nowhere, changes nothing. When
/// the client already relayed the new session, still unsubscribed -- its
/// command for it landed while the old relay waited for EOF -- the level
/// is transferred onto that relay instead of starting a second one.
pub(crate) fn follow(
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<crate::relay::Relays>>,
    old: &str,
) {
    let replay = lock(relays).subscription(old);
    let Some(replay) = replay else {
        return;
    };
    let from = SessionId(old.to_owned());
    let Some(next) = continued(&hub.home, &from) else {
        return;
    };
    // Under one lock hold: the client may have relayed the new session,
    // still unsubscribed, while the old relay waited for EOF. `transfer`
    // keeps the level and carries it onto that relay, or reports that no
    // relay exists and the level is only kept.
    if lock(relays).transfer(&next.0, &replay) {
        return;
    }
    let Some(stream) = reach(hub, &from, &next) else {
        return;
    };
    attach(&next.0, stream, hub, writer, relays, Some(replay), None);
}

/// `next`'s lock in [`Hub::starting`]: one mutex per next session, so two
/// starts of one session serialize while any other start runs at once.
fn guard_for(hub: &Hub, next: &SessionId) -> Arc<Mutex<()>> {
    let mut starting = lock(&hub.starting);
    starting.get(next).cloned().unwrap_or_else(|| {
        let guard = Arc::new(Mutex::new(()));
        starting.insert(next.clone(), Arc::clone(&guard));
        guard
    })
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
