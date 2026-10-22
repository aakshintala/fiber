//! The running-server re-list: a fresh `tools/list` against the running
//! server, returning declare-ready lists and touching nothing. The slot
//! entry extracts what the re-list needs into [`RelistTarget`] and passes
//! it explicitly; this module does the RPC and payload work.

use std::sync::Arc;
use std::time::Duration;

use contract::clock::Clock;
use contract::tool::ServerRecord;

use crate::cache::Cached;
use crate::server::{CallError, Server};
use crate::server_json::{ListedPrompt, ListedTool};

/// What [`fetch`] found: fresh lists, or the failure the slot entry maps
/// to the replace path, whose records carry the death and the ready.
pub(crate) enum Relisted {
    /// The server answered with fresh lists; the caller stages them.
    Up { listed: Cached },
    /// The server died, expired, stopped or missed the re-list deadline:
    /// shadow-start its replacement instead. The death outcome the shared
    /// path already yields, built without moving the slot: expired and
    /// refused lists carry none. Each row carries at most one death
    /// record; the commit extends the aggregate from the option.
    Replace {
        /// The death record the commit writes before the ready, if any.
        record: Option<ServerRecord>,
    },
}

/// What the re-list needs, extracted by the slot entry under its lease:
/// the running handle, whether this is the restart, which sections to
/// ask for, the listing bound, and the resources flag the cache write
/// keeps. This module never touches the slot itself.
pub(crate) struct RelistTarget {
    /// The running server to list.
    pub(crate) server: Arc<Server>,
    /// Whether this is the restart, so the next death is final.
    pub(crate) restarted: bool,
    /// Whether the handshake advertised the tools capability.
    pub(crate) list_tools: bool,
    /// Whether the handshake advertised the prompts capability.
    pub(crate) list_prompts: bool,
    /// The listing bound: the spec's `startup_timeout`.
    pub(crate) timeout: Duration,
    /// Whether the slot offers resources, kept in the cache write.
    pub(crate) resources: bool,
}

/// What [`fetch`] answered: fresh lists, or the wire failure the slot
/// entry maps to the replace path.
pub(crate) enum Fetched {
    /// The server answered with fresh lists.
    Up { listed: Cached },
    /// The listing failed: a timeout misses the deadline, a death
    /// carries the death this fetch is the first to see, and any other
    /// end — expired and refused lists included — replaces with no
    /// record.
    Failed { error: Option<CallError> },
}

/// Lists the running server's advertised sections under the target's
/// bound: installs nothing — the caller stages the outcome, and the
/// prompt payload feeds the cache write only — and any transport
/// failure rides the `Failed` outcome instead.
pub(crate) fn fetch(target: &RelistTarget, clock: &Arc<dyn Clock>) -> Fetched {
    let bound = crate::wait::deadline(clock.as_ref(), target.timeout);
    // Any failed page rides the outcome: a timeout misses the deadline,
    // a death carries the death this fetch is the first to see, and any
    // other end — expired and refused lists included — carries none.
    let tools = if target.list_tools {
        match crate::server::list_pages::<ListedTool>(
            &target.server,
            clock,
            "tools/list",
            "tools",
            bound,
            &crate::wait::NoCancel,
        ) {
            Ok(tools) => tools,
            Err(error) => return Fetched::Failed { error },
        }
    } else {
        Vec::new()
    };
    let prompts = if target.list_prompts {
        // Any prompts/list error falls back to the replace path: a
        // failed listing never writes a cache from an empty list.
        match crate::server::list_pages::<ListedPrompt>(
            &target.server,
            clock,
            "prompts/list",
            "prompts",
            bound,
            &crate::wait::NoCancel,
        ) {
            Ok(prompts) => prompts,
            Err(error) => return Fetched::Failed { error },
        }
    } else {
        Vec::new()
    };
    Fetched::Up {
        listed: Cached {
            tools,
            prompts,
            resources: target.resources,
        },
    }
}
