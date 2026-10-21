//! One reload row per server: the classify boundary of the server sync
//! (`docs/mcp.md`, "Reload"). Required rows come first, each partition in
//! new-spec order, removals last in slot-list order, so every required
//! operation runs before any non-required one.

use std::sync::PoisonError;

use crate::start::{ServerSpec, Servers};

/// One row of the reload sync, classified from the new specs against the
/// live slots (`docs/mcp.md`, "Reload"). `Keep` re-lists in place with a
/// staged install; `Replace` and `Start` shadow-start first and install at
/// commit; `Remove` stops and drops at commit. Main drives the
/// classify/stage/commit entries and never touches slots directly.
pub enum SyncRow {
    /// The same executable with only declare-time differences, or an
    /// identical spec: re-list in place, rebuilding the declaration
    /// filters at declare time with no restart.
    Keep {
        /// The new spec to re-list and declare through.
        spec: ServerSpec,
    },
    /// A new executable or endpoint, a timeout/`required` difference, a
    /// dead slot, or a kept re-list that signalled the replace path:
    /// shadow-start the new spec fresh and swap at commit.
    Replace {
        /// The new spec to shadow-start fresh.
        spec: ServerSpec,
    },
    /// Declared now with no live slot: start fresh as the session start
    /// does. A lazy `NotStarted` slot never answers a reload.
    Start {
        /// The new spec to start.
        spec: ServerSpec,
    },
    /// Declared before with no declaration now: stop, drop, undeclare.
    /// A removed `required` server is a removal, never a start failure.
    Remove {
        /// The dropped server's name.
        name: String,
    },
}

/// Classifies the person's new specs against the live slots, required
/// operations first (program order on the prepare thread is the edge),
/// each partition in new-spec order, removals last in slot-list order.
/// A key mismatch or a `startup_timeout`/`call_timeout`/`required`
/// difference takes the replace path; a running slot with a matching
/// key and timeouts keeps; a running slot that died or expired replaces;
/// a `Down` or `Dead` slot replaces beside the old one; a `NotStarted`
/// slot and a name with no slot start fresh. Any other difference rebuilds at declare
/// time with no restart.
pub fn classify_sync(new_specs: Vec<ServerSpec>, servers: &Servers) -> Vec<SyncRow> {
    let slots = servers
        .slots
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let mut by_name = std::collections::HashMap::new();
    for slot in &slots {
        by_name.insert(slot.spec().name.clone(), slot.clone());
    }
    let mut new_names = std::collections::HashSet::new();
    for spec in &new_specs {
        new_names.insert(spec.name.clone());
    }
    let mut required = Vec::new();
    let mut optional = Vec::new();
    for spec in new_specs {
        let row = match by_name.get(&spec.name) {
            None => SyncRow::Start { spec },
            Some(slot) => {
                let old = slot.spec();
                // A lazy slot never answers a reload: force a real start.
                // A changed key or timeouts replace even a slot that never
                // served: serving in place would run the old spec.
                if old.key() != spec.key()
                    || old.startup_timeout != spec.startup_timeout
                    || old.call_timeout != spec.call_timeout
                    || old.required != spec.required
                {
                    SyncRow::Replace { spec }
                } else if slot.is_live() {
                    SyncRow::Keep { spec }
                } else if slot.is_running() {
                    // Running but gone or expired: it died while idle, so
                    // the re-list would only fall back; replace beside it.
                    SyncRow::Replace { spec }
                } else if slot.served_before() {
                    // `Down` or `Dead`: it served before and serves no
                    // more, so the sync swaps it beside the old slot and
                    // the commit files the name under `restarted`.
                    SyncRow::Replace { spec }
                } else {
                    // `NotStarted`: nothing ever served, so the
                    // single-spec start runs fresh and the commit swaps
                    // the leftover slot out.
                    SyncRow::Start { spec }
                }
            }
        };
        let is_required = match &row {
            SyncRow::Keep { spec } | SyncRow::Replace { spec } | SyncRow::Start { spec } => {
                spec.required
            }
            SyncRow::Remove { .. } => false,
        };
        if is_required {
            required.push(row);
        } else {
            optional.push(row);
        }
    }
    for slot in &slots {
        let name = slot.spec().name.clone();
        if !new_names.contains(&name) {
            required.push(SyncRow::Remove { name });
        }
    }
    required.extend(optional);
    required
}
