//! The reload's staging: re-list keeps, shadow-start replacements and
//! starts, without touching the live slots or the cache files.

use std::sync::Arc;

use contract::events::McpServerFailed;

use crate::cache::Cached;
use crate::prompt::PromptSource;
use crate::slot::{Relisted, Slot};
use crate::start::{Declaration, ServerSpec, Servers, Shadowed, declare, open_staged};

use super::SyncInputs;
use super::rows::{Disposition, SyncRow};

/// What one staged row holds: the outcome with the cache payload, without
/// touching the live slots or the cache files until every required start
/// succeeds.
pub(super) enum StagedRow {
    Keep {
        spec: ServerSpec,
        listed: Cached,
        used: Vec<Declaration>,
    },
    /// A successful shadow start — a replacement or a fresh start —
    /// with its staged tools, cache payload and prompt sources.
    ShadowUp {
        spec: ServerSpec,
        slot: Arc<Slot>,
        listed: Cached,
        used: Vec<Declaration>,
        /// The old death the commit writes before the ready.
        records: Vec<contract::tool::ServerRecord>,
        /// Which quadrant the commit files the shadow under.
        disposition: Disposition,
        /// The shadow's prompt sources, carried for servers with
        /// no startup rows.
        sources: Vec<PromptSource>,
    },
    /// A shadow start that failed without `required`: the failure the
    /// commit files, with the old deaths staged beside it.
    ShadowFailed {
        spec: ServerSpec,
        failure: McpServerFailed,
        records: Vec<contract::tool::ServerRecord>,
        /// Which quadrant the commit files the failure under.
        disposition: Disposition,
    },
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

/// Shadow-starts one fresh server for a shadow row, without touching the
/// live slots or the cache files: `disposition` names the quadrant the
/// commit files it under, and the failure stages the old deaths beside
/// the start failure when the shadow fails. A required failure stops every
/// shadow started so far and propagates, so no replacement child outlives
/// the rejected reload.
fn stage_shadow(
    shadows: &mut Vec<Arc<Slot>>,
    servers: &Servers,
    spec: ServerSpec,
    records: Vec<contract::tool::ServerRecord>,
    disposition: Disposition,
    inputs: &SyncInputs,
) -> Result<StagedRow, contract::shapes::Failure> {
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
            Ok(StagedRow::ShadowUp {
                spec,
                slot: opened.slot,
                listed: opened.listed,
                used: opened.declared,
                records,
                disposition,
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
            Ok(StagedRow::ShadowFailed {
                spec,
                failure,
                records,
                disposition,
            })
        }
    }
}

/// Stages the classified rows: required rows come first, so a required
/// failure returns before any non-required row runs — and the staged
/// outcomes are dropped, so the old slots keep serving and the old
/// caches stay byte-identical.
///
pub fn stage_sync(
    rows: Vec<SyncRow>,
    servers: &Servers,
    inputs: &SyncInputs,
) -> Result<StagedSync, contract::shapes::Failure> {
    let mut staged = Vec::new();
    let mut shadows: Vec<Arc<Slot>> = Vec::new();
    for row in rows {
        match row {
            SyncRow::Keep { spec } => {
                let name = spec.name.clone();
                let Some(slot) = servers.slot(&name) else {
                    unreachable!(
                        "commit is the only remover and runs on this thread, so nothing can vanish mid-prepare"
                    );
                };
                match slot.relist() {
                    Relisted::Up { listed } => {
                        let link = Arc::downgrade(&slot);
                        let used =
                            declare(&spec, &listed.tools, &link, inputs.artifacts, inputs.images);
                        staged.push(StagedRow::Keep { spec, listed, used });
                    }
                    Relisted::Replace { records } => {
                        staged.push(stage_shadow(
                            &mut shadows,
                            servers,
                            spec,
                            records,
                            Disposition::Restarted,
                            inputs,
                        )?);
                    }
                }
            }
            SyncRow::Shadow { spec, disposition } => {
                // The old death the commit writes before the ready, when
                // this check is the first to see it: nothing for a live
                // or expired slot, whose stop writes no record, and
                // nothing for a fresh start, which carries no old slot.
                let records: Vec<contract::tool::ServerRecord> =
                    if disposition == Disposition::Restarted {
                        servers
                            .slot(&spec.name)
                            .and_then(|slot| slot.idle_death())
                            .into_iter()
                            .collect()
                    } else {
                        Vec::new()
                    };
                staged.push(stage_shadow(
                    &mut shadows,
                    servers,
                    spec,
                    records,
                    disposition,
                    inputs,
                )?);
            }
            SyncRow::Remove { name } => {
                staged.push(StagedRow::Remove { name });
            }
        }
    }
    Ok(StagedSync { rows: staged })
}
