//! The reload's staging: re-list keeps, shadow-start replacements and
//! starts, without touching the live slots or the cache files. Required
//! rows already come first from [`classify`](super::classify_sync), so a
//! required failure returns before any non-required row runs.

use std::sync::Arc;

use contract::clock::Clock;
use contract::events::McpServerFailed;
use contract::images::Images;
use contract::tool::ServerRecord;

use crate::cache::Cached;
use crate::prompt::PromptSource;
use crate::slot::{Relisted, Slot};
use crate::start::{Declaration, ServerSpec, Servers, Shadowed, declare, open_staged};

use super::SyncInputs;
use super::rows::SyncRow;

/// What one staged row holds: the outcome with the cache payload, without
/// touching the live slots or the cache files until every required start
/// succeeds.
pub(super) enum StagedRow {
    Keep {
        spec: ServerSpec,
        listed: Cached,
        used: Vec<Declaration>,
    },
    /// A staged shadow start: a replacement or a fresh start, or the
    /// failure a non-required shadow filed.
    Shadow(ShadowStaged),
    Remove {
        name: String,
    },
}

/// The staged sync: every kept re-list outcome and every shadow-started
/// replacement, with staged tools and staged cache writes. Opaque to
/// `main`, which passes it from stage to commit.
pub struct StagedSync {
    pub(super) rows: Vec<StagedRow>,
}

/// What one shadow start staged: only a replacement or a failure, so
/// the callers' matches need no unreachable arm. The stage loop files
/// each as its commit row; the fallback pass files each as the dead
/// keep's replacement.
pub(super) enum ShadowStaged {
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
/// replacement child outlives the rejected reload. The stage loop and the
/// fallback pass stage through here, so the paths cannot drift apart.
fn stage_shadow(
    shadows: &mut Vec<Arc<Slot>>,
    servers: &Servers,
    spec: ServerSpec,
    records: Vec<contract::tool::ServerRecord>,
    restarted: bool,
    inputs: &SyncInputs,
) -> Result<ShadowStaged, contract::shapes::Failure> {
    match open_staged(
        spec.clone(),
        inputs.workspace,
        inputs.artifacts,
        inputs.images,
        inputs.cache,
        inputs.clock,
        inputs.version,
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
/// [`classify`](super::classify_sync), so a required failure returns before
/// any non-required row runs. On a required shadow-start failure the shadows
/// are stopped, the staged outcomes dropped, and the start's failure
/// propagates: the old slots keep serving and the old caches stay
/// byte-identical.
///
/// The startup inputs stay flat here: `main` drives this entry with the
/// same arguments as the session start, so its shape matches.
#[allow(
    clippy::too_many_arguments,
    reason = "the public stage shape matches the session start"
)]
pub fn stage_sync(
    rows: Vec<SyncRow>,
    servers: &Servers,
    workspace: &std::path::Path,
    artifacts: &std::path::Path,
    images: &Arc<dyn Images>,
    cache: &std::path::Path,
    clock: &Arc<dyn Clock>,
    version: &str,
) -> Result<StagedSync, contract::shapes::Failure> {
    let inputs = SyncInputs {
        workspace,
        artifacts,
        images,
        cache,
        clock,
        version,
    };
    let mut staged = Vec::new();
    let mut shadows: Vec<Arc<Slot>> = Vec::new();
    for row in rows {
        match row {
            SyncRow::Keep { spec } => {
                let name = spec.name.clone();
                let Some(slot) = servers.slot(&name) else {
                    // The slot left between classify and stage: start fresh.
                    staged.push(StagedRow::Shadow(stage_shadow(
                        &mut shadows,
                        servers,
                        spec,
                        Vec::new(),
                        true,
                        &inputs,
                    )?));
                    continue;
                };
                match slot.relist() {
                    Relisted::Up { listed } => {
                        let link = Arc::downgrade(&slot);
                        let used =
                            declare(&spec, &listed.tools, &link, inputs.artifacts, inputs.images);
                        staged.push(StagedRow::Keep { spec, listed, used });
                    }
                    Relisted::Replace { records } => {
                        staged.push(StagedRow::Shadow(stage_shadow(
                            &mut shadows,
                            servers,
                            spec,
                            records,
                            true,
                            &inputs,
                        )?));
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
                staged.push(StagedRow::Shadow(stage_shadow(
                    &mut shadows,
                    servers,
                    spec,
                    records,
                    true,
                    &inputs,
                )?));
            }
            SyncRow::Start { spec } => {
                staged.push(StagedRow::Shadow(stage_shadow(
                    &mut shadows,
                    servers,
                    spec,
                    Vec::new(),
                    false,
                    &inputs,
                )?));
            }
            SyncRow::Remove { name } => {
                staged.push(StagedRow::Remove { name });
            }
        }
    }
    Ok(StagedSync { rows: staged })
}

/// One kept row's fallback, staged before the commit installs anything:
/// a slot that died (or left) after its re-list shadow-starts its
/// replacement through the same helper the stage loop uses, so the
/// commit installs no stale lists as kept.
pub(super) enum KeepFallback {
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
/// the ready.
fn stage_keep_fallback(
    extra_shadows: &mut Vec<Arc<Slot>>,
    servers: &Servers,
    spec: ServerSpec,
    inputs: &SyncInputs,
) -> Result<KeepFallback, contract::shapes::Failure> {
    // The old death the commit writes before the ready, when this check
    // is the first to see it: nothing for a live or expired slot, whose
    // stop writes no record.
    let carried: Vec<ServerRecord> = servers
        .slot(&spec.name)
        .and_then(|slot| slot.idle_death())
        .into_iter()
        .collect();
    match stage_shadow(extra_shadows, servers, spec, carried, true, inputs)? {
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
pub(super) fn stop_shadows(extra_shadows: &[Arc<Slot>], rows: &[StagedRow]) {
    for shadow in extra_shadows {
        shadow.stop();
    }
    for row in rows {
        if let StagedRow::Shadow(ShadowStaged::Replaced { slot, .. }) = row {
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
pub(super) fn fallback_dead_keeps(
    staged: &StagedSync,
    fallbacks: &mut std::collections::HashMap<String, KeepFallback>,
    extra_shadows: &mut Vec<Arc<Slot>>,
    servers: &Servers,
    inputs: &SyncInputs,
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
        match stage_keep_fallback(extra_shadows, servers, spec.clone(), inputs) {
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
