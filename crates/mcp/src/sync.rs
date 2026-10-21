//! The reload's server sync (`docs/mcp.md`, "Reload"): classify each new
//! spec against the live slots, stage (re-list keeps, shadow-start
//! replacements and starts), then commit (stop and remove old slots,
//! install the new ones, write caches, declare). `main` prepare drives
//! this through the crate root and never touches slots directly.

use std::path::Path;
use std::sync::{Arc, PoisonError};

use contract::clock::Clock;
use contract::events::{McpServerFailed, ToolInfo};
use contract::images::Images;
use contract::tool::{ServerRecord, Tool};

use crate::cache::{self, Cached};
use crate::slot::Slot;
use crate::start::{
    Declaration, ServerSpec, Servers, Shadowed, declare, open_staged, resources_info,
};

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
/// a slot that never serves (`NotStarted`, `Down`, `Dead`) and a name
/// with no slot start fresh. Any other difference rebuilds at declare
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
                } else {
                    // `NotStarted`, `Down` or `Dead`: nothing serves, so
                    // the single-spec start runs fresh and the commit
                    // swaps the leftover slot out.
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
    required.extend(optional);
    for slot in &slots {
        let name = slot.spec().name.clone();
        if !new_names.contains(&name) {
            required.push(SyncRow::Remove { name });
        }
    }
    required
}

/// What one staged row holds: the outcome with the cache payload, without
/// touching the live slots or the cache files until every required start
/// succeeds.
enum StagedRow {
    Keep {
        spec: ServerSpec,
        listed: Cached,
        used: Vec<Declaration>,
    },
    Replaced {
        spec: ServerSpec,
        slot: Arc<Slot>,
        listed: Cached,
        used: Vec<Declaration>,
        ready: contract::tool::ServerRecord,
        /// The old death the commit writes before the ready.
        records: Vec<contract::tool::ServerRecord>,
        /// Whether the commit files the shadow under `restarted` (a
        /// replacement, or a fallback for a slot that left) or
        /// `started` (a fresh start).
        restarted: bool,
    },
    ReplaceFailed {
        spec: ServerSpec,
        failure: McpServerFailed,
    },
    StartFailed {
        spec: ServerSpec,
        failure: McpServerFailed,
    },
    Remove {
        name: String,
    },
}

/// The staged sync: every kept re-list outcome and every shadow-started
/// replacement, with staged tools and staged cache writes. Opaque to
/// `main`, which passes it from stage to commit.
pub struct StagedSync {
    rows: Vec<StagedRow>,
}

/// Stops every shadow started so far: the stage's required-failure
/// path, so no replacement child outlives the rejected reload.
fn stop_shadows(shadows: &[Arc<Slot>]) {
    for shadow in shadows {
        shadow.stop();
    }
}

/// The start's failure for a required shadow that failed to start: the
/// reload rejects with the start's code.
fn required_failed(failure: &McpServerFailed) -> contract::shapes::Failure {
    contract::shapes::Failure {
        code: contract::ErrorCode::McpRequiredServerFailed,
        message: failure.error.message.clone(),
        retry_after_ms: None,
        provider: None,
    }
}

/// Stages the classified rows without touching the live slots or the cache
/// files: kept servers re-list in place with staged installs, replacements
/// and starts shadow-start first. Required rows already come first from
/// `classify_sync`, so a required failure returns before any non-required
/// row runs. On a required shadow-start failure the shadows are stopped,
/// the staged outcomes dropped, and the start's failure propagates: the
/// old slots keep serving and the old caches stay byte-identical.
#[allow(
    clippy::too_many_arguments,
    reason = "one sync's startup inputs, as at session start"
)]
pub fn stage_sync(
    rows: Vec<SyncRow>,
    servers: &Servers,
    workspace: &Path,
    artifacts: &Path,
    images: &Arc<dyn Images>,
    cache: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
) -> Result<StagedSync, contract::shapes::Failure> {
    let mut staged = Vec::new();
    let mut shadows: Vec<Arc<Slot>> = Vec::new();
    // Shadow-starts one fresh server for a replace or start row, without
    // touching the live slots or the cache files: `restarted` names the
    // quadrant the commit files it under, and the failure row when the
    // shadow fails. A required failure stops every shadow started so far
    // and propagates, so no replacement child outlives the rejected reload.
    let mut shadow = |spec: ServerSpec,
                      records: Vec<contract::tool::ServerRecord>,
                      restarted: bool|
     -> Result<StagedRow, contract::shapes::Failure> {
        match open_staged(
            spec.clone(),
            workspace,
            artifacts,
            images,
            cache,
            clock,
            version,
            servers.timing(),
        ) {
            Shadowed::Up(opened) => {
                shadows.push(Arc::clone(&opened.slot));
                let ready = ServerRecord::Ready(contract::events::McpServerReady {
                    server: spec.name.clone(),
                });
                Ok(StagedRow::Replaced {
                    spec,
                    slot: opened.slot,
                    listed: opened.listed,
                    used: opened.declared,
                    ready,
                    records,
                    restarted,
                })
            }
            Shadowed::Down { failure, required } => {
                if required {
                    stop_shadows(&shadows);
                    return Err(required_failed(&failure));
                }
                Ok(if restarted {
                    StagedRow::ReplaceFailed { spec, failure }
                } else {
                    StagedRow::StartFailed { spec, failure }
                })
            }
        }
    };
    for row in rows {
        match row {
            SyncRow::Keep { spec } => {
                let name = spec.name.clone();
                let Some(slot) = servers.slot(&name) else {
                    // The slot left between classify and stage: start fresh.
                    staged.push(shadow(spec, Vec::new(), true)?);
                    continue;
                };
                match slot.relist() {
                    crate::slot::Relisted::Up { listed } => {
                        let link = Arc::downgrade(&slot);
                        let used = declare(&spec, &listed.tools, &link, artifacts, images);
                        staged.push(StagedRow::Keep { spec, listed, used });
                    }
                    crate::slot::Relisted::Replace { records } => {
                        staged.push(shadow(spec, records, true)?);
                    }
                }
            }
            SyncRow::Replace { spec } => {
                // The old death the commit writes before the ready, when
                // this check is the first to see it: nothing for a live or
                // expired slot, whose stop writes no record.
                let records: Vec<contract::tool::ServerRecord> = servers
                    .slot(&spec.name)
                    .and_then(|slot| slot.idle_death())
                    .into_iter()
                    .collect();
                staged.push(shadow(spec, records, true)?);
            }
            SyncRow::Start { spec } => {
                staged.push(shadow(spec, Vec::new(), false)?);
            }
            SyncRow::Remove { name } => {
                staged.push(StagedRow::Remove { name });
            }
        }
    }
    Ok(StagedSync { rows: staged })
}

/// What the commit installs: the new MCP tool set, the records to write
/// before `reloaded`, the non-required failures for its `failed` entry,
/// and the four-quadrant grouping.
pub struct Synced {
    /// `(registered_by, tool, info)` for the post-sync MCP set, in
    /// declaration order.
    pub tools: Vec<(String, Arc<dyn Tool>, ToolInfo)>,
    /// The `mcp_server_failed`/`mcp_server_ready` lines in sync order,
    /// written before `reloaded`.
    pub records: Vec<ServerRecord>,
    /// Each non-required server failure, for `reloaded`'s `failed` entry.
    pub failed: Vec<contract::events::ReloadFailure>,
    /// Kept as they were.
    pub kept: Vec<String>,
    /// Restarted: shadow-started replacements, including non-required
    /// ones whose shadow failed (the old slot is still stopped at
    /// commit, with the server showing failed).
    pub restarted: Vec<String>,
    /// Started: fresh starts, including non-required ones that failed.
    pub started: Vec<String>,
    /// Stopped: removed declarations, never started again.
    pub stopped: Vec<String>,
}

/// Commits the staged sync: stops and removes old slots, installs the new
/// ones, writes caches, and declares. Runs only after every required serve
/// passed, so no two slots for one name ever serve at once and no stale
/// spec survives.
pub fn commit_sync(
    staged: StagedSync,
    servers: &Servers,
    cache: &Path,
    artifacts: &Path,
    images: &Arc<dyn Images>,
) -> Synced {
    let mut tools = Vec::new();
    let mut records = Vec::new();
    let mut failed = Vec::new();
    let mut kept = Vec::new();
    let mut restarted = Vec::new();
    let mut started = Vec::new();
    let mut stopped = Vec::new();
    for row in staged.rows {
        match row {
            StagedRow::Keep { spec, listed, used } => {
                let name = spec.name.clone();
                if let Some(slot) = servers.slot(&name) {
                    // The whole cached payload decides: a prompts-only
                    // change rewrites the file even when the tools stay
                    // the same.
                    if slot.listed_snapshot() != listed {
                        cache::write(cache, &name, &spec.key(), &listed);
                    }
                    slot.install_relisted(listed);
                    tools.extend(used);
                    kept.push(name);
                }
            }
            StagedRow::Replaced {
                spec,
                slot,
                listed,
                used,
                ready,
                records: carried,
                restarted: replaces,
            } => {
                let name = spec.name.clone();
                // A fresh start swaps a leftover slot out, and a
                // replacement swaps the stale spec out: a lazy
                // `NotStarted` or failed slot with the same name never
                // serves again once the commit installs the new one.
                servers.remove(&name);
                servers
                    .slots
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(Arc::clone(&slot));
                cache::write(cache, &name, &spec.key(), &listed);
                tools.extend(used);
                records.extend(carried);
                records.push(ready);
                if replaces {
                    restarted.push(name);
                } else {
                    started.push(name);
                }
            }
            StagedRow::ReplaceFailed { spec, failure } => {
                let name = spec.name.clone();
                servers.remove(&name);
                records.push(ServerRecord::Failed(failure.clone()));
                failed.push(contract::events::ReloadFailure {
                    server: name.clone(),
                    reason: failure.reason,
                    error: failure.error,
                });
                restarted.push(name);
            }
            StagedRow::StartFailed { spec, failure } => {
                let name = spec.name.clone();
                records.push(ServerRecord::Failed(failure.clone()));
                failed.push(contract::events::ReloadFailure {
                    server: name.clone(),
                    reason: failure.reason,
                    error: failure.error,
                });
                started.push(name);
            }
            StagedRow::Remove { name } => {
                servers.remove(&name);
                stopped.push(name);
            }
        }
    }
    // One `mcp_resources` when any post-sync slot offers resources, as at
    // session start: the installed hook undeclares it when none does.
    let offering = servers
        .slots
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|slot| slot.offers_resources());
    if offering {
        let definition = crate::resources_definition();
        let tool: Arc<dyn Tool> = Arc::new(crate::resources::Resources::new(
            &servers.slots,
            artifacts,
            images,
        ));
        tools.push(("builtin".to_owned(), tool, resources_info(&definition)));
    }
    Synced {
        tools,
        records,
        failed,
        kept,
        restarted,
        started,
        stopped,
    }
}
