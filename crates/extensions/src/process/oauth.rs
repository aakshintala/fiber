//! Native `host.oauth.refresh` continuation (`docs/extensions.md`, "Host
//! calls"): the lock, the nested function request and the stored write,
//! as the Lua half holds its lock around its function.
//!
//! The worker owns the refresh's one answer: it holds the lock or slot
//! from acquisition to release, sends the nested function request with
//! key `"1"`, and settles the refresh from the nested reply, the refresh
//! deadline, its cancel signal or the hang guard. Only the dispatcher
//! that runs a provider's `credential()` supplies what the refresh
//! refreshes; every other callback is refused before this runs. Held
//! credentials and tokens travel only the protocol's own positions — the
//! nested arguments, the nested accepted value and the refresh answer —
//! never logs, errors, locks or `Debug`.

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use contract::ErrorCode;
use contract::clock::Clock;
use serde_json::Value;
use support::cancel::Cancel;
use support::lock;

use crate::host::admission::CallbackContext;
use crate::host::{LockError, Reply};
use crate::lua::CredentialFor;
use crate::lua_provider::CredentialPair;
use crate::oauth::held::Holder;
use crate::oauth::{self, held};

use super::requests::{Answer, Outcome, RequestKind, Table};
use super::transport::SendError;
use super::{Shared, answer_off, answer_on, envelope_line, hang_guard, line::ProcessLineKind};

/// The stored pair's file under Fiber home. The home alone names no
/// pair: the dispatcher carries the session's pair on the callback, and
/// a login carries its own slot instead.
#[derive(Debug)]
pub(crate) struct FileCredential {
    home: PathBuf,
}

impl FileCredential {
    /// Resolves pairs from the credential files under `home`.
    pub(crate) fn new(home: PathBuf) -> Self {
        Self { home }
    }

    /// Waits for the pair's holder until the deadline, the cancel signal
    /// or retirement, whichever is first. A login slot never reaches
    /// here: the continuation uses it directly, holding no file.
    pub(crate) fn resolve(
        &self,
        pair: &CredentialPair,
        clock: &dyn Clock,
        cancel: &dyn Cancel,
        deadline: Option<Instant>,
        live: &dyn Fn() -> bool,
    ) -> Result<Holder, (ErrorCode, String)> {
        // The cell the lock service delivers into: the deliver closure
        // fills it from the service's thread, and the waiter parks on
        // the injected clock, as dispatched requests do.
        let cell: Arc<(Mutex<Option<Reply>>, Condvar)> =
            Arc::new((Mutex::new(None), Condvar::new()));
        let deliver: oauth::Deliver = Arc::new({
            let cell = Arc::clone(&cell);
            move |reply| {
                *lock(&cell.0) = Some(reply);
                cell.1.notify_all();
            }
        });
        // `stop` held to the end: dropping it cancels the off-thread
        // wait, so no lock wait outlives this call.
        let _stop = match oauth::lock(&self.home, pair, &deliver) {
            Ok(stop) => Some(stop),
            Err(reply) => {
                *lock(&cell.0) = Some(reply);
                None
            }
        };
        let start = clock.now();
        // The hang backstop past the deadline, as dispatched requests
        // wait it; without a deadline the wait still ends.
        let bound = match deadline {
            Some(end) => end.checked_add(Duration::from_secs(300)).unwrap_or(end),
            None => start
                .checked_add(Duration::from_secs(3600))
                .unwrap_or(start),
        };
        loop {
            if let Some(reply) = lock(&cell.0).take() {
                // Only the lock service delivers here; any other reply
                // would be a service crossed with this wait.
                let crossed = || {
                    Err((
                        ErrorCode::IoFailed,
                        "host.oauth.refresh: the credential lock ended the wait".to_owned(),
                    ))
                };
                return match reply {
                    Reply::Lock(Ok(holder)) => Ok(holder),
                    Reply::Lock(Err(LockError::Coded(failure))) => Err(failure),
                    Reply::Lock(Err(LockError::Arg(message))) => {
                        Err((ErrorCode::InvalidArguments, message))
                    }
                    Reply::Http(_)
                    | Reply::Ask(_)
                    | Reply::Drive(_)
                    | Reply::Exec(_)
                    | Reply::Tool(_)
                    | Reply::Model(_)
                    | Reply::Delegate(_)
                    | Reply::Query(_)
                    | Reply::Slept => crossed(),
                };
            }
            if cancel.is_cancelled() || !live() {
                return Err((
                    ErrorCode::Cancelled,
                    "host.oauth.refresh: the refresh was cancelled".to_owned(),
                ));
            }
            let now = clock.now();
            if deadline.is_some_and(|end| now >= end) || now >= bound {
                return Err((
                    ErrorCode::Timeout,
                    "host.oauth.refresh: the credential lock timed out".to_owned(),
                ));
            }
            let until = Some(deadline.map_or(bound, |end| end.min(bound)));
            let guard = lock(&cell.0);
            if support::clock::park(
                clock,
                until,
                Some(Table::WAIT_POLL),
                &cell.1,
                guard,
                |state| state.is_some(),
            )
            .is_none()
            {
                continue;
            }
        }
    }
}

/// Runs one admitted `host.oauth.refresh` to its one answer: the lock or
/// slot around the nested function request, the stored write, and the
/// release on every exit. The nested request inherits the refresh
/// request's context and deadline, uses key `"1"`, and stays routable
/// while the refresh awaits it. A retired generation's answer is
/// dropped, never leaking an old request context into the new
/// generation.
pub(crate) fn refresh_continue(
    shared: &Arc<Shared>,
    id: &contract::CommandId,
    callback: CallbackContext,
    scope: CredentialFor,
    credential: &FileCredential,
) {
    let generation = shared.table.generation();
    let live = || shared.table.generation() == generation;
    let end = run_refresh(shared, id, &callback, scope, credential, &live);
    #[cfg(test)]
    shared.pause_refresh_completion();
    if !live() {
        // Death or restart retired the call: its answer belongs to a dead
        // generation, and must not leak into the new one.
        return;
    }

    match end {
        End::Accept(value) => answer_on(
            shared,
            id,
            Some(contract::events::CommandResult::Call(
                contract::events::CallResult { value: Some(value) },
            )),
        ),
        End::Reject(code, message) => {
            answer_off(shared, id, contract::inbox::Rejection { code, message })
        }
        End::Silent => {}
    }
}

/// How the continuation ends: the one answer it sends, or none.
enum End {
    /// Accept with the stored or refreshed credential.
    Accept(Value),
    /// Reject with the failure the Lua half would raise.
    Reject(ErrorCode, String),
    /// The child is gone: send nothing.
    Silent,
}

#[allow(
    clippy::too_many_arguments,
    reason = "one continuation over the refresh call's whole context"
)]
fn run_refresh(
    shared: &Arc<Shared>,
    id: &contract::CommandId,
    callback: &CallbackContext,
    scope: CredentialFor,
    credential: &FileCredential,
    live: &dyn Fn() -> bool,
) -> End {
    let clock = shared.clock.as_ref();
    // The stored pair's lock, or the login's slot which holds no file.
    // The holder lives to the end: dropping the file lock releases it,
    // and a login keeps its slot for `fiber login`.
    let holder = match &scope {
        CredentialFor::Stored(pair) => {
            match credential.resolve(pair, clock, &callback.cancel, callback.deadline, live) {
                Ok(holder) => holder,
                Err((code, message)) => return End::Reject(code, message),
            }
        }
        CredentialFor::Login(slot) => Holder::Login(Arc::clone(slot)),
    };
    let stored = match &holder {
        Holder::File(lock) => match lock.read() {
            // A read failure rejects with its message, as `held:read`
            // raises it.
            Ok(stored) => stored,
            Err(e) => return End::Reject(ErrorCode::IoFailed, e.to_string()),
        },
        // A login holds no file: its function logs in when it sees nil.
        Holder::Login(_) => None,
    };
    let now = held::unix_seconds(clock.wall());
    if let Some(stored) = stored.as_ref()
        && !held::due_at(now, stored)
    {
        return End::Accept(stored.clone());
    }
    // The nested function request inherits the refresh request context
    // and deadline, uses key `"1"`, and stays routable while the refresh
    // awaits it: concurrent with everything, never behind the stream
    // gate. Its registration is the refresh call, as the wire shows.
    let minted = shared.table.mint(
        RequestKind::Function,
        &shared.owner,
        callback.ran_by().unwrap_or_default().to_owned(),
        callback.deadline,
        callback.state_call,
        Some(scope),
    );
    // The refresh ending cancels its nested function: the nested
    // request is attributed to the refresh, as dispatched requests are
    // to their caller.
    shared.table.watch_caller(&callback.cancel, &minted.id);
    let args = vec![stored.clone().unwrap_or(Value::Null)];
    let payload = match serde_json::to_value(&contract::events::ExtensionRequest {
        request_id: minted.id.clone(),
        registration: id.clone(),
        key: "1".to_owned(),
        args,
    }) {
        Ok(payload) => payload,
        Err(e) => {
            shared.table.cancel(&minted.id);
            return End::Reject(
                ErrorCode::IoFailed,
                format!("host.oauth.refresh: the request never serialized: {e}"),
            );
        }
    };
    let line = envelope_line(
        shared,
        ProcessLineKind::ExtensionRequest,
        payload.as_object().cloned().unwrap_or_default(),
    );
    match lock(&shared.transport).send(line) {
        Ok(()) => {}
        Err(SendError::TooLarge) => {
            // The stored pair does not fit the wire cap: fail the
            // refresh with the request's own size sentence.
            shared.table.cancel(&minted.id);
            return End::Reject(
                ErrorCode::TooLarge,
                "host.oauth.refresh: the request does not fit the 64 MiB wire cap".to_owned(),
            );
        }
        Err(SendError::Gone) => {
            // The child is gone: cancel the request and send nothing, as
            // no answer has anyone left to reach.
            shared.table.cancel(&minted.id);
            return End::Silent;
        }
    }
    let hang = hang_guard(clock, callback.deadline);
    let outcome =
        match Table::wait_for(&minted.wait, callback.deadline, &minted.cancel, clock, hang) {
            Some(Outcome::Answered(answer)) => Outcome::Answered(answer),
            Some(Outcome::TimedOut) => {
                shared
                    .table
                    .end_race(&minted.id, &minted.wait, Outcome::TimedOut)
            }
            Some(Outcome::Cancelled) | None => {
                shared
                    .table
                    .end_race(&minted.id, &minted.wait, Outcome::Cancelled)
            }
        };
    match outcome {
        Outcome::Answered(Answer::Value(fresh)) => {
            // The function answered: validate before anything is
            // stored, refusing an expired value as `held:write` does.
            let now = held::unix_seconds(clock.wall());
            if let Err((code, message)) = held::check_fresh(now, &fresh) {
                return End::Reject(code, message);
            }
            match &holder {
                Holder::File(lock) => match lock.write(&fresh) {
                    Ok(()) => End::Accept(fresh),
                    Err(e) => End::Reject(ErrorCode::IoFailed, e.to_string()),
                },
                // A login keeps its result for `fiber login`, which
                // stores it after the flow; the refresh answers it.
                Holder::Login(slot) => {
                    *lock(slot) = Some(fresh.clone());
                    End::Accept(fresh)
                }
            }
        }
        // The function raised: its failure passes through unchanged, as
        // a raised table does.
        Outcome::Answered(Answer::Failure { code, message }) => End::Reject(code, message),
        // The function never answered: it never ran, as an unreached
        // refresh fails `credential_failed`.
        Outcome::TimedOut => End::Reject(
            ErrorCode::CredentialFailed,
            "host.oauth.refresh: the refresh function timed out".to_owned(),
        ),
        Outcome::Cancelled => End::Reject(
            ErrorCode::CredentialFailed,
            "host.oauth.refresh: the refresh ended before its function answered".to_owned(),
        ),
    }
}
