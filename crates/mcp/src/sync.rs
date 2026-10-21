//! The reload's server sync (`docs/mcp.md`, "Reload"): classify each new
//! spec against the live slots, stage (re-list keeps, shadow-start
//! replacements and starts), then commit (stop and remove old slots,
//! install the new ones, write caches, declare). `main` prepare drives
//! this through the crate root and never touches slots directly.

use std::path::Path;
use std::sync::{Arc, PoisonError};

use contract::clock::Clock;
use contract::events::{CommandInfo, McpServerFailed, McpServerReady, ReloadedServers, ToolInfo};
use contract::images::Images;
use contract::tool::{ServerRecord, Tool};

use crate::cache::{self, Cached};
use crate::prompt::Prompts;
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
        /// The old death the commit writes before the ready.
        records: Vec<contract::tool::ServerRecord>,
        /// Whether the commit files the shadow under `restarted` (a
        /// replacement, or a fallback for a slot that left) or
        /// `started` (a fresh start).
        restarted: bool,
        /// The shadow's runnable prompt rows, carried for servers with
        /// no startup rows; kept servers refresh the cache file only.
        prompts: Vec<CommandInfo>,
    },
    Failed {
        spec: ServerSpec,
        failure: McpServerFailed,
        /// Whether the commit files the failure under `restarted`
        /// (a replacement) or `started` (a fresh start).
        replaces: bool,
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
                let prompts = Prompts::collect(opened.sources).commands();
                Ok(StagedRow::Replaced {
                    spec,
                    slot: opened.slot,
                    listed: opened.listed,
                    used: opened.declared,
                    records,
                    restarted,
                    prompts,
                })
            }
            Shadowed::Down { failure } => {
                if spec.required {
                    for shadow in &shadows {
                        shadow.stop();
                    }
                    return Err(required_failed(&failure));
                }
                Ok(StagedRow::Failed {
                    spec,
                    failure,
                    replaces: restarted,
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
    /// Kept, restarted, started and stopped, by name.
    pub servers: ReloadedServers,
    /// The shadow-started servers' runnable prompt rows, for the
    /// servers with no startup rows; kept servers refresh the cache
    /// file only.
    pub prompts: Vec<CommandInfo>,
}

/// Installs a freshly shadow-started slot at commit: swaps the stale
/// slot out, writes its cache, declares its tools and files it under
/// `restarted` (a replacement) or `started` (a fresh start). Both the
/// staged replacements and the kept slots that died after their re-list
/// install through here, so the two paths cannot drift apart.
#[allow(clippy::too_many_arguments, reason = "one commit's install inputs")]
fn install_opened(
    installing: &mut Synced,
    servers: &Servers,
    cache: &Path,
    spec: &ServerSpec,
    slot: &Arc<Slot>,
    listed: &Cached,
    used: Vec<Declaration>,
    carried: Vec<ServerRecord>,
    carried_prompts: Vec<CommandInfo>,
    restarted: bool,
) {
    let name = spec.name.clone();
    // A fresh start swaps a leftover slot out, and a replacement swaps
    // the stale spec out: a lazy `NotStarted` or failed slot with the
    // same name never serves again once the commit installs the new one.
    servers.remove(&name);
    servers
        .slots
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Arc::clone(slot));
    cache::write(cache, &name, &spec.key(), listed);
    installing.tools.extend(used);
    installing.records.extend(carried);
    installing.records.push(ServerRecord::Ready(McpServerReady {
        server: name.clone(),
    }));
    installing.prompts.extend(carried_prompts);
    if restarted {
        installing.servers.restarted.push(name);
    } else {
        installing.servers.started.push(name);
    }
}

/// Installs a non-required shadow-start failure at commit: the old slot
/// is already gone for a replacement, the failure rides a failed record
/// and the `failed` entry, filed under `restarted` (a replacement) or
/// `started` (a fresh start).
fn install_failed(
    installing: &mut Synced,
    servers: &Servers,
    spec: &ServerSpec,
    failure: McpServerFailed,
    replaces: bool,
) {
    let name = spec.name.clone();
    if replaces {
        // A non-required replacement still stops the old slot at
        // commit: the configuration changed, so no stale slot survives,
        // with the server showing failed.
        servers.remove(&name);
    }
    installing
        .records
        .push(ServerRecord::Failed(failure.clone()));
    installing.failed.push(contract::events::ReloadFailure {
        server: name.clone(),
        reason: failure.reason,
        error: failure.error,
    });
    if replaces {
        installing.servers.restarted.push(name);
    } else {
        installing.servers.started.push(name);
    }
}

/// Commits the staged sync: stops and removes old slots, installs the new
/// ones, writes caches, and declares. Runs only after every required serve
/// passed, so no two slots for one name ever serve at once and no stale
/// spec survives. A kept slot that died (or left) after its re-list takes
/// the replacement path here instead of declaring its stale tools as kept
/// (`docs/mcp.md`, "Reload" step 3): its old death rides the carried
/// records and the fresh shadow installs as `restarted`. A required
/// replacement that fails rejects with the start's code.
#[allow(
    clippy::too_many_arguments,
    reason = "one commit's startup inputs, as at session start"
)]
pub fn commit_sync(
    staged: StagedSync,
    servers: &Servers,
    cache: &Path,
    artifacts: &Path,
    images: &Arc<dyn Images>,
    workspace: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
) -> Result<Synced, contract::shapes::Failure> {
    let mut installing = Synced {
        tools: Vec::new(),
        records: Vec::new(),
        failed: Vec::new(),
        servers: ReloadedServers {
            kept: Vec::new(),
            restarted: Vec::new(),
            started: Vec::new(),
            stopped: Vec::new(),
        },
        prompts: Vec::new(),
    };
    for row in staged.rows {
        match row {
            StagedRow::Keep { spec, listed, used } => {
                let name = spec.name.clone();
                let live = servers.slot(&name).is_some_and(|slot| slot.is_live());
                if !live {
                    // The kept server died (or its slot left) after its
                    // re-list: `install_relisted` would leave the dead
                    // slot unchanged while the sync still declared its
                    // tools as kept, so the replacement path restarts it
                    // fresh instead.
                    let carried: Vec<ServerRecord> = servers
                        .slot(&name)
                        .and_then(|slot| slot.idle_death())
                        .into_iter()
                        .collect();
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
                            let carried_prompts = Prompts::collect(opened.sources).commands();
                            install_opened(
                                &mut installing,
                                servers,
                                cache,
                                &spec,
                                &opened.slot,
                                &opened.listed,
                                opened.declared,
                                carried,
                                carried_prompts,
                                true,
                            );
                        }
                        Shadowed::Down { failure } => {
                            if spec.required {
                                return Err(required_failed(&failure));
                            }
                            install_failed(&mut installing, servers, &spec, failure, true);
                        }
                    }
                    continue;
                }
                if let Some(slot) = servers.slot(&name) {
                    // The whole cached payload decides: a prompts-only
                    // change rewrites the file even when the tools stay
                    // the same.
                    if slot.listed_snapshot() != listed {
                        cache::write(cache, &name, &spec.key(), &listed);
                    }
                    slot.install_relisted(listed);
                    installing.tools.extend(used);
                    installing.servers.kept.push(name);
                }
            }
            StagedRow::Replaced {
                spec,
                slot,
                listed,
                used,
                records: carried,
                restarted: replaces,
                prompts: carried_prompts,
            } => {
                install_opened(
                    &mut installing,
                    servers,
                    cache,
                    &spec,
                    &slot,
                    &listed,
                    used,
                    carried,
                    carried_prompts,
                    replaces,
                );
            }
            StagedRow::Failed {
                spec,
                failure,
                replaces,
            } => {
                install_failed(&mut installing, servers, &spec, failure, replaces);
            }
            StagedRow::Remove { name } => {
                servers.remove(&name);
                installing.servers.stopped.push(name);
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
        installing
            .tools
            .push(("builtin".to_owned(), tool, resources_info(&definition)));
    }
    Ok(installing)
}
