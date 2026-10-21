//! The reload's server sync (`docs/mcp.md`, "Reload"): classify each new
//! spec against the live slots, stage (re-list keeps, shadow-start
//! replacements and starts), then commit (stop and remove old slots,
//! install the new ones, write caches, declare). `main` prepare drives
//! this through the crate root and never touches slots directly.

use std::path::Path;
use std::sync::{Arc, PoisonError};

use contract::clock::Clock;
use contract::events::{
    CommandInfo, McpServerFailed, McpServerReady, ReloadedServers, ServerFailure, ToolInfo,
};
use contract::images::Images;
use contract::tool::{ServerRecord, Tool};

use crate::cache::{self, Cached};
use crate::prompt::{PromptSource, Prompts};
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
        /// The shadow's prompt sources, carried for servers with
        /// no startup rows; kept servers refresh the cache file only.
        /// The commit collects every row's sources into one listing,
        /// whose rows ride `Synced.prompts` and whose sources ride
        /// `Synced.prompt_sources`.
        sources: Vec<PromptSource>,
    },
    Failed {
        spec: ServerSpec,
        failure: McpServerFailed,
        /// The old deaths staged beside the failure: the commit writes
        /// the detected exits among them before the failure, so an
        /// optional replacement failure still records the death it
        /// replaced (`docs/mcp.md`, "Each death is recorded as
        /// `mcp_server_failed`").
        records: Vec<contract::tool::ServerRecord>,
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

/// What one shadow start staged: only a replacement or a failure, so
/// the callers' matches need no unreachable arm. The stage loop files
/// each as its commit row; the fallback passes file each as the dead
/// keep's replacement.
enum ShadowStaged {
    /// The shadow-started replacement with its staged tools, cache
    /// payload and prompt sources.
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
        /// The shadow's prompt sources, carried for servers with
        /// no startup rows.
        sources: Vec<PromptSource>,
    },
    /// The shadow start failed without `required`: the failure the
    /// commit files, with the old deaths staged beside it.
    Failed {
        spec: ServerSpec,
        failure: McpServerFailed,
        records: Vec<contract::tool::ServerRecord>,
        /// Whether the commit files the failure under `restarted`
        /// (a replacement) or `started` (a fresh start).
        replaces: bool,
    },
}

impl From<ShadowStaged> for StagedRow {
    fn from(staged: ShadowStaged) -> StagedRow {
        match staged {
            ShadowStaged::Replaced {
                spec,
                slot,
                listed,
                used,
                records,
                restarted,
                sources,
            } => StagedRow::Replaced {
                spec,
                slot,
                listed,
                used,
                records,
                restarted,
                sources,
            },
            ShadowStaged::Failed {
                spec,
                failure,
                records,
                replaces,
            } => StagedRow::Failed {
                spec,
                failure,
                records,
                replaces,
            },
        }
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

/// Shadow-starts one fresh server for a replace or start row, without
/// touching the live slots or the cache files: `restarted` names the
/// quadrant the commit files it under, and the failure stages the old
/// deaths beside the start failure when the shadow fails. A required
/// failure stops every shadow started so far and propagates, so no
/// replacement child outlives the rejected reload. The stage loop, the
/// fallback passes and the install-time revalidation all stage through
/// here, so the paths cannot drift apart.
#[allow(
    clippy::too_many_arguments,
    reason = "one shadow's startup inputs, as at session start"
)]
fn stage_shadow(
    shadows: &mut Vec<Arc<Slot>>,
    servers: &Servers,
    spec: ServerSpec,
    records: Vec<contract::tool::ServerRecord>,
    restarted: bool,
    workspace: &Path,
    artifacts: &Path,
    images: &Arc<dyn Images>,
    cache: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
) -> Result<ShadowStaged, contract::shapes::Failure> {
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
            Ok(ShadowStaged::Replaced {
                spec,
                slot: opened.slot,
                listed: opened.listed,
                used: opened.declared,
                records,
                restarted,
                sources: opened.sources,
            })
        }
        Shadowed::Down { failure } => {
            if spec.required {
                for shadow in shadows.iter() {
                    shadow.stop();
                }
                return Err(required_failed(&failure));
            }
            Ok(ShadowStaged::Failed {
                spec,
                failure,
                records,
                replaces: restarted,
            })
        }
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
    for row in rows {
        match row {
            SyncRow::Keep { spec } => {
                let name = spec.name.clone();
                let Some(slot) = servers.slot(&name) else {
                    // The slot left between classify and stage: start fresh.
                    staged.push(
                        stage_shadow(
                            &mut shadows,
                            servers,
                            spec,
                            Vec::new(),
                            true,
                            workspace,
                            artifacts,
                            images,
                            cache,
                            clock,
                            version,
                        )?
                        .into(),
                    );
                    continue;
                };
                match slot.relist() {
                    crate::slot::Relisted::Up { listed } => {
                        let link = Arc::downgrade(&slot);
                        let used = declare(&spec, &listed.tools, &link, artifacts, images);
                        staged.push(StagedRow::Keep { spec, listed, used });
                    }
                    crate::slot::Relisted::Replace { records } => {
                        staged.push(
                            stage_shadow(
                                &mut shadows,
                                servers,
                                spec,
                                records,
                                true,
                                workspace,
                                artifacts,
                                images,
                                cache,
                                clock,
                                version,
                            )?
                            .into(),
                        );
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
                staged.push(
                    stage_shadow(
                        &mut shadows,
                        servers,
                        spec,
                        records,
                        true,
                        workspace,
                        artifacts,
                        images,
                        cache,
                        clock,
                        version,
                    )?
                    .into(),
                );
            }
            SyncRow::Start { spec } => {
                staged.push(
                    stage_shadow(
                        &mut shadows,
                        servers,
                        spec,
                        Vec::new(),
                        false,
                        workspace,
                        artifacts,
                        images,
                        cache,
                        clock,
                        version,
                    )?
                    .into(),
                );
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
    /// The shadow-started servers' prompt sources, answering through
    /// the same path as a startup row: `main` builds the carried fetch
    /// from these, and `loop` routes the new tags to it.
    pub prompt_sources: Prompts,
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
    sources: Vec<PromptSource>,
    all_sources: &mut Vec<PromptSource>,
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
    all_sources.extend(sources);
    if restarted {
        installing.servers.restarted.push(name);
    } else {
        installing.servers.started.push(name);
    }
}

/// Installs a non-required shadow-start failure at commit: the old slot
/// is already gone for a replacement, the detected exits ride first and
/// the failure rides its failed record and the `failed` entry, filed
/// under `restarted` (a replacement) or `started` (a fresh start). A
/// carried listing miss stays out: the shadow's own failure already
/// describes the unresponsive server, while an exit would otherwise
/// never be logged.
fn install_failed(
    installing: &mut Synced,
    servers: &Servers,
    spec: &ServerSpec,
    carried: Vec<ServerRecord>,
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
        .extend(carried.into_iter().filter(|record| {
            matches!(
                record,
                ServerRecord::Failed(failed) if failed.reason == ServerFailure::Died
            )
        }));
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

// The one-shot hook the commit runs between its fallback pass and its
// liveness revalidation. Test-only: a test installs the kill that must
// land in that window, so the dead-keep routing is deterministic.
// Same-thread only: the commit runs the hook on its own thread.
#[cfg(test)]
thread_local! {
    static BETWEEN_COMMIT_PHASES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Installs the one-shot between-phases hook. Test-only.
#[cfg(test)]
pub(crate) fn on_between_commit_phases(hook: Box<dyn FnOnce()>) {
    BETWEEN_COMMIT_PHASES.with(|held| *held.borrow_mut() = Some(hook));
}

/// Takes the one-shot between-phases hook, if any. The commit calls this
/// between its passes; a test calls it after the commit to prove the hook
/// ran exactly once. Test-only.
#[cfg(test)]
pub(crate) fn take_between_commit_phases() -> Option<Box<dyn FnOnce()>> {
    BETWEEN_COMMIT_PHASES.with(|held| held.borrow_mut().take())
}

/// One kept row's fallback, staged before the commit installs anything:
/// a slot that died (or left) after its re-list shadow-starts its
/// replacement through the same helper the stage loop uses, so the
/// commit installs no stale lists as kept.
enum KeepFallback {
    /// The slot died or left: install the fresh shadow as `restarted`.
    Replaced {
        slot: Arc<Slot>,
        listed: Cached,
        used: Vec<Declaration>,
        carried: Vec<ServerRecord>,
        sources: Vec<PromptSource>,
    },
    /// The slot died or left and its replacement failed without
    /// `required`: the old death rides beside the failure.
    Failed {
        carried: Vec<ServerRecord>,
        failure: McpServerFailed,
    },
}

/// Stages one kept row's fallback: a slot that died (or left) after
/// its re-list shadow-starts its replacement through the same helper
/// the stage loop uses, carrying the old death the commit writes before
/// the ready. Both fallback passes and the install-time revalidation
/// stage through here, so the paths cannot drift apart.
#[allow(
    clippy::too_many_arguments,
    reason = "one fallback's startup inputs, as at session start"
)]
fn stage_keep_fallback(
    extra_shadows: &mut Vec<Arc<Slot>>,
    servers: &Servers,
    spec: ServerSpec,
    workspace: &Path,
    artifacts: &Path,
    images: &Arc<dyn Images>,
    cache: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
) -> Result<KeepFallback, contract::shapes::Failure> {
    // The old death the commit writes before the ready, when this check
    // is the first to see it: nothing for a live or expired slot, whose
    // stop writes no record.
    let carried: Vec<ServerRecord> = servers
        .slot(&spec.name)
        .and_then(|slot| slot.idle_death())
        .into_iter()
        .collect();
    match stage_shadow(
        extra_shadows,
        servers,
        spec,
        carried,
        true,
        workspace,
        artifacts,
        images,
        cache,
        clock,
        version,
    )? {
        ShadowStaged::Replaced {
            slot,
            listed,
            used,
            records: carried,
            sources,
            ..
        } => Ok(KeepFallback::Replaced {
            slot,
            listed,
            used,
            carried,
            sources,
        }),
        ShadowStaged::Failed {
            failure,
            records: carried,
            ..
        } => Ok(KeepFallback::Failed { carried, failure }),
    }
}

/// Stops every shadow the sync started: the fallback extras with the
/// staged replacements. On a required failure nothing installs, so every
/// earlier row's slot, tools and cache stay as the stage left them.
fn stop_shadows(extra_shadows: &[Arc<Slot>], rows: &[StagedRow]) {
    for shadow in extra_shadows {
        shadow.stop();
    }
    for row in rows {
        if let StagedRow::Replaced { slot, .. } = row {
            slot.stop();
        }
    }
}

/// One fallback pass over the staged keeps: every keep whose slot died
/// (or left) shadow-starts its replacement now, while no slot or cache
/// has changed. Its old death rides the carried records and the fresh
/// shadow installs as `restarted`. A required fallback failure stops
/// every shadow and rejects with the start's code, leaving every slot,
/// tool and cache byte as the stage left them.
#[allow(
    clippy::too_many_arguments,
    reason = "one pass's startup inputs, as at session start"
)]
fn fallback_dead_keeps(
    staged: &StagedSync,
    fallbacks: &mut std::collections::HashMap<String, KeepFallback>,
    extra_shadows: &mut Vec<Arc<Slot>>,
    servers: &Servers,
    workspace: &Path,
    artifacts: &Path,
    images: &Arc<dyn Images>,
    cache: &Path,
    clock: &Arc<dyn Clock>,
    version: &str,
) -> Result<(), contract::shapes::Failure> {
    for row in &staged.rows {
        let StagedRow::Keep { spec, .. } = row else {
            continue;
        };
        if fallbacks.contains_key(&spec.name) {
            continue;
        }
        if servers.slot(&spec.name).is_some_and(|slot| slot.is_live()) {
            continue;
        }
        match stage_keep_fallback(
            extra_shadows,
            servers,
            spec.clone(),
            workspace,
            artifacts,
            images,
            cache,
            clock,
            version,
        ) {
            Ok(fallback) => {
                fallbacks.insert(spec.name.clone(), fallback);
            }
            Err(failure) => {
                stop_shadows(extra_shadows, &staged.rows);
                return Err(failure);
            }
        }
    }
    Ok(())
}

/// Commits the staged sync: stops and removes old slots, installs the new
/// ones, writes caches, and declares. The commit runs in three phases.
/// The first stages every fallible replacement before anything is
/// installed: a kept slot that died (or left) after its re-list takes the
/// replacement path here instead of declaring its stale tools as kept
/// (`docs/mcp.md`, "Reload" step 3), shadow-started through the same
/// helper the stage loop uses. The second revalidates liveness and routes
/// keeps that died in between through the same fallback. Only once every
/// required shadow start has succeeded does the third phase install
/// (stop/remove old slots, install the new ones, write caches, declare),
/// so no two slots for one name ever serve at once and no stale spec
/// survives. A required fallback failure stops every shadow and rejects
/// with the start's code, leaving every slot, tool and cache byte as the
/// stage left them.
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
        prompt_sources: Prompts::default(),
    };
    // The first phase: every kept slot that died (or left) after its
    // re-list shadow-starts its replacement now, while no slot or cache
    // has changed. Its old death rides the carried records and the fresh
    // shadow installs as `restarted`.
    let mut fallbacks: std::collections::HashMap<String, KeepFallback> =
        std::collections::HashMap::new();
    let mut extra_shadows: Vec<Arc<Slot>> = Vec::new();
    fallback_dead_keeps(
        &staged,
        &mut fallbacks,
        &mut extra_shadows,
        servers,
        workspace,
        artifacts,
        images,
        cache,
        clock,
        version,
    )?;
    // Test-only: the dead-keep test kills its keep here, after the
    // fallback pass checked it but before the commit installs it.
    #[cfg(test)]
    if let Some(hook) = take_between_commit_phases() {
        hook();
    }
    // The second phase revalidates liveness and routes keeps that died
    // in between through the same fallback, before anything installs.
    fallback_dead_keeps(
        &staged,
        &mut fallbacks,
        &mut extra_shadows,
        servers,
        workspace,
        artifacts,
        images,
        cache,
        clock,
        version,
    )?;
    // The third phase installs, now that every required shadow start
    // has succeeded: stop/remove old slots, install the new ones, write
    // caches, declare.
    let mut all_sources: Vec<PromptSource> = Vec::new();
    let mut rows = staged.rows;
    while !rows.is_empty() {
        let row = rows.remove(0);
        match row {
            StagedRow::Keep { spec, listed, used } => {
                let name = spec.name.clone();
                match fallbacks.remove(&name) {
                    Some(KeepFallback::Replaced {
                        slot,
                        listed,
                        used,
                        carried,
                        sources,
                    }) => {
                        install_opened(
                            &mut installing,
                            servers,
                            cache,
                            &spec,
                            &slot,
                            &listed,
                            used,
                            carried,
                            sources,
                            &mut all_sources,
                            true,
                        );
                    }
                    Some(KeepFallback::Failed { carried, failure }) => {
                        install_failed(&mut installing, servers, &spec, carried, failure, true);
                    }
                    // Revalidated at install: a keep whose slot is still
                    // alive installs the staged re-list, while a keep that
                    // died after the fallback passes routes through the
                    // fallback now instead of declaring its stale tools as
                    // kept. The install writes the cache unconditionally:
                    // the staged lists are what the slot holds, and a
                    // prompts-only change rewrites the file even when the
                    // tools stay the same.
                    None => match servers.slot(&name) {
                        Some(slot) if slot.is_live() => {
                            slot.install_relisted(listed.clone());
                            cache::write(cache, &name, &spec.key(), &listed);
                            installing.tools.extend(used);
                            installing.servers.kept.push(name);
                        }
                        _ => match stage_keep_fallback(
                            &mut extra_shadows,
                            servers,
                            spec.clone(),
                            workspace,
                            artifacts,
                            images,
                            cache,
                            clock,
                            version,
                        ) {
                            Ok(KeepFallback::Replaced {
                                slot,
                                listed,
                                used,
                                carried,
                                sources,
                            }) => {
                                install_opened(
                                    &mut installing,
                                    servers,
                                    cache,
                                    &spec,
                                    &slot,
                                    &listed,
                                    used,
                                    carried,
                                    sources,
                                    &mut all_sources,
                                    true,
                                );
                            }
                            Ok(KeepFallback::Failed { carried, failure }) => {
                                install_failed(
                                    &mut installing,
                                    servers,
                                    &spec,
                                    carried,
                                    failure,
                                    true,
                                );
                            }
                            Err(failure) => {
                                // Best effort: earlier rows already
                                // installed, so only the shadows that never
                                // installed stop here.
                                stop_shadows(&extra_shadows, &rows);
                                return Err(failure);
                            }
                        },
                    },
                }
            }
            StagedRow::Replaced {
                spec,
                slot,
                listed,
                used,
                records: carried,
                restarted: replaces,
                sources,
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
                    sources,
                    &mut all_sources,
                    replaces,
                );
            }
            StagedRow::Failed {
                spec,
                failure,
                records: carried,
                replaces,
            } => {
                install_failed(&mut installing, servers, &spec, carried, failure, replaces);
            }
            StagedRow::Remove { name } => {
                servers.remove(&name);
                installing.servers.stopped.push(name);
            }
        }
    }
    // Every shadow's sources join one listing, whose rows ride
    // `prompts` and whose sources ride `prompt_sources`: `main` builds
    // the carried fetch from the sources, and `loop` routes the new
    // tags to it, so an appended row fetches through the same path as
    // a startup row.
    let collected = Prompts::collect(all_sources);
    installing.prompts = collected.commands();
    installing.prompt_sources = collected;
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
