//! The reload's server sync (`docs/mcp.md`, "Reload"): classify each new
//! spec against the live slots, stage (re-list keeps, shadow-start
//! replacements and starts), then commit (stop and remove old slots,
//! install the new ones, write caches, declare). `main` prepare drives
//! this through the crate root and never touches slots directly.

#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::{Arc, PoisonError};

use contract::clock::Clock;
use contract::events::{McpServerFailed, McpServerReady, ReloadedServers, ToolInfo};
use contract::images::Images;
use contract::tool::{ServerRecord, Tool};

use crate::cache::{self, Cached};
use crate::prompt::{PromptSource, Prompts};
use crate::slot::{Relisted, Slot};
use crate::start::{
    Declaration, ServerSpec, Servers, Shadowed, declare, open_staged, resources_declaration,
};

/// One sync's startup inputs, as at session start: the public stage and
/// commit entries thread this instead of one argument each, and `main`
/// builds it at the call site.
pub struct SyncInputs<'a> {
    /// The session workspace the servers run in.
    pub workspace: &'a Path,
    /// The artifacts directory binary content declares through.
    pub artifacts: &'a Path,
    /// The image child shadow starts load through.
    pub images: &'a Arc<dyn Images>,
    /// The cache directory session starts write through.
    pub cache: &'a Path,
    /// The clock every bound reads.
    pub clock: &'a Arc<dyn Clock>,
    /// The session version shadow slots carry.
    pub version: &'a str,
}

/// One row of the reload sync, classified from the new specs against the
/// live slots (`docs/mcp.md`, "Reload"). `Keep` re-lists in place with a
/// staged install; `Shadow` shadow-starts fresh and installs at commit;
/// `Remove` stops and drops at commit. Main drives the
/// classify/stage/commit entries and never touches slots directly.
pub enum SyncRow {
    /// The same executable with only declare-time differences, or an
    /// identical spec: re-list in place, rebuilding the declaration
    /// filters at declare time with no restart.
    Keep {
        /// The new spec to re-list and declare through.
        spec: ServerSpec,
    },
    /// A shadow-started fresh server: a replacement (a new executable
    /// or endpoint, a timeout/`required` difference, a dead slot, or a
    /// kept re-list that signalled the replace path) or a fresh start
    /// (declared now with no live slot, where a lazy `NotStarted` slot
    /// never answers a reload). Which quadrant the commit files it
    /// under rides along, so no boolean can disagree with the row that
    /// produced it.
    Shadow {
        /// The new spec to shadow-start fresh.
        spec: ServerSpec,
        /// Which quadrant the commit files the shadow under.
        disposition: Disposition,
    },
    /// Declared before with no declaration now: stop, drop, undeclare.
    /// A removed `required` server is a removal, never a start failure.
    Remove {
        /// The dropped server's name.
        name: String,
    },
}

/// Which quadrant the commit files a shadow under: a replacement lands in
/// `restarted`, a fresh start in `started`. One variant travels from the
/// classifying row through staging to the commit, so no boolean can
/// disagree with the row that produced it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Disposition {
    /// A replacement: the commit files the name under `restarted`.
    Restarted,
    /// A fresh start: the commit files the name under `started`.
    Started,
}

impl Disposition {
    /// Files `name` under `restarted` (a replacement) or `started` (a
    /// fresh start).
    fn file(self, servers: &mut ReloadedServers, name: String) {
        if self == Disposition::Restarted {
            servers.restarted.push(name);
        } else {
            servers.started.push(name);
        }
    }
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
            // Declared now with no live slot: start fresh as the session
            // start does. A lazy `NotStarted` slot never answers a
            // reload.
            None => SyncRow::Shadow {
                spec,
                disposition: Disposition::Started,
            },
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
                    SyncRow::Shadow {
                        spec,
                        disposition: Disposition::Restarted,
                    }
                } else if slot.is_live() {
                    SyncRow::Keep { spec }
                } else if slot.is_running() || slot.served_before() {
                    // Running but gone or expired, `Down` or `Dead`: it
                    // served before and serves no more, so the re-list
                    // would only fall back; replace beside the old slot
                    // and file the name under `restarted`.
                    SyncRow::Shadow {
                        spec,
                        disposition: Disposition::Restarted,
                    }
                } else {
                    // `NotStarted`: nothing ever served, so the
                    // single-spec start runs fresh and the commit swaps
                    // the leftover slot out.
                    SyncRow::Shadow {
                        spec,
                        disposition: Disposition::Started,
                    }
                }
            }
        };
        let is_required = match &row {
            SyncRow::Keep { spec } | SyncRow::Shadow { spec, .. } => spec.required,
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

/// What one staged row holds: the outcome with the cache payload, without
/// touching the live slots or the cache files until every required start
/// succeeds.
enum StagedRow {
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
    /// The shadow-started servers' prompt sources, answering through
    /// the same path as a startup row: `main` builds the carried fetch
    /// from these, and `loop` routes the new tags to it.
    pub prompt_sources: Prompts,
}

/// Installs a non-required shadow-start failure at commit: the old slot
/// is already gone for a replacement, the carried records ride first and
/// the failure rides its failed record and the `failed` entry, filed
/// under `restarted` (a replacement) or `started` (a fresh start).
fn install_failed(
    installing: &mut Synced,
    servers: &Servers,
    spec: &ServerSpec,
    carried: Vec<ServerRecord>,
    failure: McpServerFailed,
    disposition: Disposition,
) {
    let name = spec.name.clone();
    if disposition == Disposition::Restarted {
        // A non-required replacement still stops the old slot at
        // commit: the configuration changed, so no stale slot survives,
        // with the server showing failed.
        servers.remove(&name);
    }
    installing.records.extend(carried);
    installing
        .records
        .push(ServerRecord::Failed(failure.clone()));
    installing.failed.push(contract::events::ReloadFailure {
        server: name.clone(),
        reason: failure.reason,
        error: failure.error,
    });
    disposition.file(&mut installing.servers, name);
}

/// The commit starts from no quadrant: every row files its own name.
fn empty_servers() -> ReloadedServers {
    ReloadedServers {
        kept: Vec::new(),
        restarted: Vec::new(),
        started: Vec::new(),
        stopped: Vec::new(),
    }
}

/// Commits the staged sync: the install runs only after every required
/// shadow start has succeeded, so no two slots for one name ever serve
/// at once — and the install itself cannot fail, so a keep that died
/// racing the commit still declares its staged tools while its cache
/// stays as the stage left it.
pub fn commit_sync(staged: StagedSync, servers: &Servers, inputs: &SyncInputs) -> Synced {
    let mut installing = Synced {
        tools: Vec::new(),
        records: Vec::new(),
        failed: Vec::new(),
        servers: empty_servers(),
        prompt_sources: Prompts::default(),
    };
    let mut all_sources: Vec<PromptSource> = Vec::new();
    for row in staged.rows {
        match row {
            StagedRow::Keep { spec, listed, used } => {
                let name = spec.name.clone();
                // One gate for the cache write: the install answers whether
                // the slot was still live under its own lock, so a keep
                // that died racing the commit keeps its old cache bytes
                // while its staged tools still declare.
                if let Some(slot) = servers.slot(&name)
                    && slot.install_relisted(listed.clone())
                {
                    cache::write(inputs.cache, &name, &spec.key(), &listed);
                }
                installing.tools.extend(used);
                installing.servers.kept.push(name);
            }
            StagedRow::ShadowUp {
                spec,
                slot,
                listed,
                used,
                records: carried,
                disposition,
                sources,
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
                    .push(slot);
                cache::write(inputs.cache, &name, &spec.key(), &listed);
                installing.tools.extend(used);
                installing.records.extend(carried);
                installing.records.push(ServerRecord::Ready(McpServerReady {
                    server: name.clone(),
                }));
                all_sources.extend(sources);
                disposition.file(&mut installing.servers, name);
            }
            StagedRow::ShadowFailed {
                spec,
                failure,
                records: carried,
                disposition,
            } => {
                install_failed(
                    &mut installing,
                    servers,
                    &spec,
                    carried,
                    failure,
                    disposition,
                );
            }
            StagedRow::Remove { name } => {
                servers.remove(&name);
                installing.servers.stopped.push(name);
            }
        }
    }
    // Every shadow's sources join one listing: `main` reads the new rows
    // off `prompt_sources.commands()` and builds the carried fetch from
    // the sources, and `loop` routes the new tags to it, so an appended
    // row fetches through the same path as a startup row.
    installing.prompt_sources = Prompts::collect(all_sources);
    // One `mcp_resources` when any post-sync slot offers resources, as at
    // session start: the installed hook undeclares it when none does.
    if let Some(declaration) =
        resources_declaration(&servers.slots, inputs.artifacts, inputs.images)
    {
        installing.tools.push(declaration);
    }
    installing
}
