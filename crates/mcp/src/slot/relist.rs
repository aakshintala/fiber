//! The running-server re-list behind its own module boundary, so
//! `slot.rs` stays under the size guideline: a fresh `tools/list`
//! (`prompts/list` only for the cache write) against the running server,
//! returning declare-ready lists and touching nothing. Identical shapes:
//! [`Slot::relist`](super::Slot) delegates here, which the sync boundary
//! calls.

use std::sync::Arc;

use contract::events::{McpServerFailed, ServerFailure};
use contract::shapes::Failure;
use contract::tool::ServerRecord;

use super::{Slot, State, died, lock};
use crate::cache::Cached;
use crate::error::McpError;
use crate::server::CallError;
use crate::server_json::{ListedPrompt, ListedTool};

/// What [`Slot::relist`](super::Slot) found: fresh lists, or the signal
/// to take the replace path, whose records carry the death and the ready.
pub(crate) enum Relisted {
    /// The server answered with fresh lists; the caller stages them.
    Up { listed: Cached },
    /// The server died, expired, stopped or missed the re-list deadline:
    /// shadow-start its replacement instead. The death outcome the shared
    /// path already yields, built without moving the slot: expired and
    /// refused lists carry none.
    Replace {
        /// The death records the commit writes before the ready.
        records: Vec<ServerRecord>,
    },
}

/// A fresh `tools/list` (`prompts/list` only for the cache write)
/// against the running server, under the slot lease, bounded by the
/// spec's `startup_timeout`, the same bound as the handshake's listing
/// it repeats. Returns declare-ready lists and touches nothing: the
/// caller stages the outcome. Any transport failure (a timeout, a
/// death mid-reload, an expiry, or a `tools/list` that errors) signals
/// the replace path, carrying the death outcome the shared path
/// already yields, built here without moving the slot: dead, expired,
/// wedged (timeout), and gone servers all return the replace path
/// with no state change. The prompt payload feeds the cache write
/// only, never the session.
pub(super) fn relist(slot: &Slot) -> Relisted {
    // The lease, held across the RPCs as `serve` holds it across a
    // launch: the edge against a concurrent serve or stop.
    let _flight = slot.acquire();
    let (server, restarted, list_tools, list_prompts) = {
        let state = lock(&slot.state);
        match &*state {
            State::Running {
                server,
                restarted,
                advertises_tools,
                advertises_prompts,
                ..
            } if !server.is_gone() && !server.is_expired() => {
                // The handshake lists only advertised capabilities: the
                // slot holds what it advertised, so an advertised-but-
                // empty section re-lists on reload, and a server that
                // advertised an empty list picks up a tool added since.
                // Listing an unadvertised section would hang a server
                // that answers it with malformed JSON.
                (
                    Arc::clone(server),
                    *restarted,
                    *advertises_tools,
                    *advertises_prompts,
                )
            }
            State::Running {
                server, restarted, ..
            } if server.is_gone() => {
                // It died while idle: this re-list is the first to see
                // it, as the entry of `serve` is for a call.
                let death = died(&slot.spec.name, !*restarted, slot.spec.remote());
                return Relisted::Replace {
                    records: vec![ServerRecord::Failed(death.record)],
                };
            }
            State::Running { .. }
            | State::NotStarted { .. }
            | State::Down { .. }
            | State::Dead { .. } => {
                return Relisted::Replace {
                    records: Vec::new(),
                };
            }
        }
    };
    let timeout = slot.spec.startup_timeout;
    let bound = crate::wait::deadline(slot.clock.as_ref(), timeout);
    // Maps a failed page to the replace path: a timeout misses the
    // deadline, a death carries the death this re-list is the first to
    // see, and any other end replaces with no records.
    let fail = |error: Option<CallError>| match error {
        Some(CallError::Timeout) => Relisted::Replace {
            records: vec![deadline_death(slot)],
        },
        Some(CallError::Gone) => {
            let death = died(&slot.spec.name, !restarted, slot.spec.remote());
            Relisted::Replace {
                records: vec![ServerRecord::Failed(death.record)],
            }
        }
        _ => Relisted::Replace {
            records: Vec::new(),
        },
    };
    let tools = if list_tools {
        match crate::server::list_pages::<ListedTool, Relisted>(
            &server,
            &slot.clock,
            "tools/list",
            "tools",
            bound,
            &crate::wait::NoCancel,
            |error| Err(fail(error)),
        ) {
            Ok(tools) => tools,
            Err(relisted) => return relisted,
        }
    } else {
        Vec::new()
    };
    let prompts = if list_prompts {
        // Any prompts/list error falls back to the replace path: a
        // failed listing never writes a cache from an empty list.
        match crate::server::list_pages::<ListedPrompt, Relisted>(
            &server,
            &slot.clock,
            "prompts/list",
            "prompts",
            bound,
            &crate::wait::NoCancel,
            |error| Err(fail(error)),
        ) {
            Ok(prompts) => prompts,
            Err(relisted) => return relisted,
        }
    } else {
        Vec::new()
    };
    Relisted::Up {
        listed: Cached {
            tools,
            prompts,
            resources: slot.resources,
        },
    }
}

/// The missed-deadline record for a re-list the bound outlived: the
/// startup deadline the handshake's listing shares, restartable as a
/// death the next call would restart. Built without moving the slot.
fn deadline_death(slot: &Slot) -> ServerRecord {
    let error = McpError::StartDeadline {
        server: slot.spec.name.clone(),
        required: slot.spec.required,
        startup_ms: slot.spec.startup_timeout.as_millis(),
    };
    ServerRecord::Failed(McpServerFailed {
        server: slot.spec.name.clone(),
        reason: ServerFailure::Deadline,
        will_restart: true,
        error: Failure::from(&error),
    })
}
