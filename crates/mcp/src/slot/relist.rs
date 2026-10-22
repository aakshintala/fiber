//! The running-server re-list: a fresh `tools/list` against the running
//! server, returning declare-ready lists and touching nothing.

use std::sync::Arc;

use contract::tool::ServerRecord;

use super::{Slot, State, died, lock};
use crate::cache::Cached;
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

impl Slot {
    /// Re-lists the running server under the slot lease, bounded by the
    /// spec's `startup_timeout`: installs nothing — the caller stages the
    /// outcome, and the prompt payload feeds the cache write only — and
    /// any transport failure signals the replace path instead.
    pub(crate) fn relist(&self) -> Relisted {
        // The lease, held across the RPCs as `serve` holds it across a
        // launch: the edge against a concurrent serve or stop.
        let _flight = self.acquire();
        let (server, restarted, list_tools, list_prompts) = {
            let state = lock(&self.state);
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
                    return replace(self, *restarted, Some(CallError::Gone));
                }
                State::Running { .. }
                | State::NotStarted { .. }
                | State::Down { .. }
                | State::Dead { .. } => {
                    return replace(self, false, None);
                }
            }
        };
        let timeout = self.spec.startup_timeout;
        let bound = crate::wait::deadline(self.clock.as_ref(), timeout);
        // Any failed page signals the replace path: a timeout misses the
        // deadline, a death carries the death this re-list is the first to
        // see, and any other end — expired and refused lists included —
        // replaces with no records.
        let tools = if list_tools {
            match crate::server::list_pages::<ListedTool>(
                &server,
                &self.clock,
                "tools/list",
                "tools",
                bound,
                &crate::wait::NoCancel,
            ) {
                Ok(tools) => tools,
                Err(error) => return replace(self, restarted, error),
            }
        } else {
            Vec::new()
        };
        let prompts = if list_prompts {
            // Any prompts/list error falls back to the replace path: a
            // failed listing never writes a cache from an empty list.
            match crate::server::list_pages::<ListedPrompt>(
                &server,
                &self.clock,
                "prompts/list",
                "prompts",
                bound,
                &crate::wait::NoCancel,
            ) {
                Ok(prompts) => prompts,
                Err(error) => return replace(self, restarted, error),
            }
        } else {
            Vec::new()
        };
        Relisted::Up {
            listed: Cached {
                tools,
                prompts,
                resources: self.resources,
            },
        }
    }
}

/// The replace path for a re-list that cannot serve: the death outcome
/// the shared path already yields, built without moving the slot. A
/// timeout carries the missed-deadline death, a death carries the death
/// this re-list is the first to see, and any other end — expired and
/// refused lists included — carries none.
fn replace(slot: &Slot, restarted: bool, error: Option<CallError>) -> Relisted {
    let records = match error {
        Some(CallError::Timeout) => vec![deadline_death(slot)],
        Some(CallError::Gone) => {
            let death = died(&slot.spec.name, !restarted, slot.spec.remote());
            vec![ServerRecord::Failed(death.record)]
        }
        _ => Vec::new(),
    };
    Relisted::Replace { records }
}

/// The missed-deadline record for a re-list the bound outlived: the
/// startup deadline the handshake's listing shares, restartable as a
/// death the next call would restart. Built without moving the slot.
fn deadline_death(slot: &Slot) -> ServerRecord {
    let mut death = crate::start::not_started(
        &slot.spec.name,
        &crate::server::StartError::Deadline,
        slot.spec.startup_timeout,
        slot.spec.required,
        slot.spec.remote(),
    );
    death.record.will_restart = true;
    ServerRecord::Failed(death.record)
}
