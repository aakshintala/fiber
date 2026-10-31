//! The reload's server sync (`docs/mcp.md`, "Reload"): classify each new
//! spec against the live slots, stage (re-list keeps, shadow-start
//! replacements and starts), then plan (pure) and apply (infallible:
//! stop and remove old slots, install the new ones, write caches).
//! `main` prepare drives this through the crate root and never touches
//! slots directly.

#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::{Arc, mpsc};
use std::thread;

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
/// plan and apply entries thread this instead of one argument each,
/// and `main` builds it at the call site.
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
/// staged install; `Shadow` shadow-starts fresh and installs at apply;
/// `Remove` stops and drops at apply. Main drives the
/// classify/stage/plan/apply entries and never touches slots directly.
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
/// operations first (program order on the prepare thread is the edge):
/// required new-spec rows in new-spec order, then removals in slot-list
/// order, then optional new-spec rows.
/// A key mismatch or a `startup_timeout`/`call_timeout`/`required`
/// difference takes the replace path; a running slot with a matching
/// key and timeouts keeps; a running slot that died or expired replaces;
/// a `Down` or `Dead` slot replaces beside the old one; a `NotStarted`
/// slot and a name with no slot start fresh. Any other difference rebuilds at declare
/// time with no restart.
pub fn classify_sync(new_specs: Vec<ServerSpec>, servers: &Servers) -> Vec<SyncRow> {
    let slots = support::lock(&servers.slots).clone();
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
                // A replacement files under `restarted` when the old slot
                // ran this session (it is running, or it served before),
                // and under `started` when the slot never started.
                let disposition = if slot.is_running() || slot.served_before() {
                    Disposition::Restarted
                } else {
                    Disposition::Started
                };
                // A lazy slot never answers a reload: force a real start.
                // A changed key or timeouts replace even a slot that never
                // served: serving in place would run the old spec. The
                // commit files the replacement by the disposition above.
                if old.key() != spec.key()
                    || old.startup_timeout != spec.startup_timeout
                    || old.call_timeout != spec.call_timeout
                    || old.required != spec.required
                {
                    SyncRow::Shadow { spec, disposition }
                } else if slot.is_live() {
                    SyncRow::Keep { spec }
                } else {
                    // Running but gone or expired, `Down`, `Dead` or
                    // `NotStarted`: the re-list would only fall back, so
                    // replace beside the old slot; a `NotStarted` slot
                    // never served, so the commit swaps the leftover out.
                    SyncRow::Shadow { spec, disposition }
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
/// `main`, which passes it from stage to plan to apply.
pub struct StagedSync {
    rows: Vec<StagedRow>,
}

impl Drop for StagedSync {
    /// A staged sync that never applies stops every shadow not yet
    /// applied: a rejected prepare drops its staged sync, so no
    /// replacement child outlives the rejection while the old slots keep
    /// serving. `apply_sync` takes the rows out first, so an applied
    /// sync stops nothing here.
    fn drop(&mut self) {
        for row in &self.rows {
            if let StagedRow::ShadowUp { slot, .. } = row {
                slot.stop();
            }
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

/// One deferred shadow start: the row's spec with the old deaths the
/// commit writes before the ready, and the quadrant the commit files it
/// under. Shadows run on per-server threads and join below, so the
/// assembly restores row order no matter which shadow answers first.
struct ShadowJob {
    /// The new spec to shadow-start fresh.
    spec: ServerSpec,
    /// The old deaths the commit writes before the ready.
    records: Vec<contract::tool::ServerRecord>,
    /// Which quadrant the commit files the shadow under.
    disposition: Disposition,
}

/// Stages the classified rows in row order — required rows first, as
/// `classify_sync` emits them. Keeps re-list inline: the re-list only
/// reads the live slots and the declaration builds no child, so no keep
/// disturbs another row. Shadows start on per-server threads and join
/// over the done-channel, as `start_observed` does, so the shadow phase
/// is bounded by the slowest new spec; a required failure stops every
/// shadow and drops every staged outcome, so the old slots keep serving
/// and the old caches stay byte-identical.
pub fn stage_sync(
    rows: Vec<SyncRow>,
    servers: &Servers,
    inputs: &SyncInputs,
) -> Result<StagedSync, contract::shapes::Failure> {
    enum Planned {
        Ready(Box<StagedRow>),
        Job(usize),
    }
    let mut planned = Vec::new();
    let mut jobs: Vec<ShadowJob> = Vec::new();
    for row in rows {
        match row {
            SyncRow::Keep { spec } => {
                let name = spec.name.clone();
                let Some(slot) = servers.slot(&name) else {
                    unreachable!(
                        "apply is the only remover and prepare never removes, so nothing can vanish mid-prepare"
                    );
                };
                match slot.relist() {
                    Relisted::Up { listed } => {
                        let link = Arc::downgrade(&slot);
                        let used =
                            declare(&spec, &listed.tools, &link, inputs.artifacts, inputs.images);
                        planned.push(Planned::Ready(Box::new(StagedRow::Keep {
                            spec,
                            listed,
                            used,
                        })));
                    }
                    Relisted::Replace { record } => {
                        planned.push(Planned::Job(jobs.len()));
                        jobs.push(ShadowJob {
                            spec,
                            records: record.into_iter().collect(),
                            disposition: Disposition::Restarted,
                        });
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
                planned.push(Planned::Job(jobs.len()));
                jobs.push(ShadowJob {
                    spec,
                    records,
                    disposition,
                });
            }
            SyncRow::Remove { name } => {
                planned.push(Planned::Ready(Box::new(StagedRow::Remove { name })));
            }
        }
    }
    let mut answered: std::collections::HashMap<usize, Shadowed> = std::collections::HashMap::new();
    if !jobs.is_empty() {
        let (done, results) = mpsc::channel();
        for (index, job) in jobs.iter().enumerate() {
            let done = done.clone();
            let spec = job.spec.clone();
            let workspace = inputs.workspace.to_path_buf();
            let artifacts = inputs.artifacts.to_path_buf();
            let images = Arc::clone(inputs.images);
            let cache = inputs.cache.to_path_buf();
            let clock = Arc::clone(inputs.clock);
            let version = inputs.version.to_owned();
            let timing = servers.timing();
            let debug = servers.debug_sink();
            thread::spawn(move || {
                let opened = open_staged(
                    spec, &workspace, &artifacts, &images, &cache, &clock, &version, timing, debug,
                );
                done.send((index, opened)).unwrap_or(());
            });
        }
        drop(done);
        for (index, opened) in results {
            answered.insert(index, opened);
        }
    }
    debug_assert_eq!(answered.len(), jobs.len(), "every shadow answers");
    // A required failure stops every shadow and propagates with the
    // start's code, so no replacement child outlives the rejected
    // reload; every staged outcome is dropped with the return.
    if let Some(failure) =
        jobs.iter()
            .enumerate()
            .find_map(|(index, job)| match answered.get(&index) {
                Some(Shadowed::Down { failure }) if job.spec.required => Some(failure),
                _ => None,
            })
    {
        for index in 0..jobs.len() {
            if let Some(Shadowed::Up(opened)) = answered.get(&index) {
                opened.slot.stop();
            }
        }
        return Err(required_failed(failure));
    }
    let mut staged = Vec::new();
    for item in planned {
        match item {
            Planned::Ready(row) => staged.push(*row),
            Planned::Job(index) => {
                let Some(job) = jobs.get(index) else {
                    unreachable!("every planned job classifies before the join");
                };
                match answered.remove(&index) {
                    Some(Shadowed::Up(opened)) => staged.push(StagedRow::ShadowUp {
                        spec: job.spec.clone(),
                        slot: opened.slot,
                        listed: opened.listed,
                        used: opened.declared,
                        records: job.records.clone(),
                        disposition: job.disposition,
                        sources: opened.sources,
                    }),
                    Some(Shadowed::Down { failure }) => staged.push(StagedRow::ShadowFailed {
                        spec: job.spec.clone(),
                        failure,
                        records: job.records.clone(),
                        disposition: job.disposition,
                    }),
                    None => unreachable!("every shadow answers before the assembly"),
                }
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

/// Plans a non-required shadow-start failure: the carried records ride
/// first and the failure rides its failed record and the `failed`
/// entry, filed under `restarted` (a replacement) or `started` (a
/// fresh start). The stale slot is dropped at apply time, for both
/// dispositions, so no stale slot survives.
fn install_failed(
    installing: &mut Synced,
    spec: &ServerSpec,
    carried: Vec<ServerRecord>,
    failure: McpServerFailed,
    disposition: Disposition,
) {
    let name = spec.name.clone();
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

/// Plans the staged sync without touching the live slots or the cache
/// files: the plan builds the tools, records, grouping and prompt
/// sources, and computes the resources decision from the staged rows
/// plus the keep slots. The install runs only after every required
/// shadow start has succeeded, so no two slots for one name ever serve
/// at once — and the install itself cannot fail, so a keep that died
/// racing the apply still declares its staged tools while its cache
/// stays as the stage left it.
pub fn plan_sync(staged: &StagedSync, servers: &Servers, inputs: &SyncInputs) -> Synced {
    let mut installing = Synced {
        tools: Vec::new(),
        records: Vec::new(),
        failed: Vec::new(),
        servers: empty_servers(),
        prompt_sources: Prompts::default(),
    };
    let mut all_sources: Vec<PromptSource> = Vec::new();
    // Names the apply drops: replacements swap the old slot out,
    // failures and removals leave none behind.
    let mut dropped = std::collections::HashSet::new();
    for row in &staged.rows {
        match row {
            StagedRow::ShadowUp { spec, .. } | StagedRow::ShadowFailed { spec, .. } => {
                dropped.insert(spec.name.clone());
            }
            StagedRow::Remove { name } => {
                dropped.insert(name.clone());
            }
            StagedRow::Keep { .. } => {}
        }
    }
    // Whether any post-apply slot offers resources, as at session start:
    // staged shadows read their staged flag, while every live slot the
    // apply keeps reads its live one — including slots no row covers,
    // such as repository-protected ones the caller filtered out.
    let mut offers_resources = false;
    for row in &staged.rows {
        if let StagedRow::ShadowUp { slot, .. } = row
            && slot.offers_resources()
        {
            offers_resources = true;
        }
    }
    if !offers_resources {
        let slots = support::lock(&servers.slots);
        offers_resources = slots
            .iter()
            .any(|slot| !dropped.contains(&slot.spec().name) && slot.offers_resources());
    }
    for row in &staged.rows {
        match row {
            StagedRow::Keep { spec, used, .. } => {
                let name = spec.name.clone();
                installing.tools.extend(used.iter().cloned());
                installing.servers.kept.push(name);
            }
            StagedRow::ShadowUp {
                spec,
                used,
                records: carried,
                disposition,
                sources,
                ..
            } => {
                let name = spec.name.clone();
                installing.tools.extend(used.iter().cloned());
                installing.records.extend(carried.iter().cloned());
                installing.records.push(ServerRecord::Ready(McpServerReady {
                    server: name.clone(),
                }));
                all_sources.extend(sources.iter().cloned());
                disposition.file(&mut installing.servers, name);
            }
            StagedRow::ShadowFailed {
                spec,
                failure,
                records: carried,
                disposition,
                ..
            } => {
                install_failed(
                    &mut installing,
                    spec,
                    carried.clone(),
                    failure.clone(),
                    *disposition,
                );
            }
            StagedRow::Remove { name } => {
                installing.servers.stopped.push(name.clone());
            }
        }
    }
    // Every shadow's sources join one listing: `main` reads the new rows
    // off `prompt_sources.commands()` and builds the carried fetch from
    // the sources, and `loop` routes the new tags to it, so an appended
    // row fetches through the same path as a startup row.
    installing.prompt_sources = Prompts::collect(all_sources);
    // One `mcp_resources` when any post-apply slot offers resources, as
    // at session start: the installed hook undeclares it when none does.
    // The declaration reads the live slots at call time, once applied.
    if offers_resources
        && let Some(declaration) =
            resources_declaration(&servers.slots, inputs.artifacts, inputs.images)
    {
        installing.tools.push(declaration);
    }
    installing
}

/// Applies the staged sync: the install runs only after the plan, and
/// itself cannot fail — slot swaps, `servers.remove` calls and
/// `cache::write` calls. The rows are taken out first, so the `Drop`
/// above stops nothing once applied.
pub fn apply_sync(mut staged: StagedSync, servers: &Servers, inputs: &SyncInputs) {
    let rows = std::mem::take(&mut staged.rows);
    for row in rows {
        match row {
            StagedRow::Keep { spec, listed, .. } => {
                let name = spec.name.clone();
                // The staged declaration already used the new spec, so the
                // slot carries it from here: the next classify and the call
                // timeouts read the re-declared values. The swap lands even
                // when the re-list install below declines, so a keep that
                // died racing the apply still declares its staged tools.
                if let Some(slot) = servers.slot(&name) {
                    slot.set_spec(spec.clone());
                    if slot.install_relisted(listed.clone()) {
                        cache::write(inputs.cache, &name, &spec.key(), &listed);
                    }
                }
            }
            StagedRow::ShadowUp {
                spec, slot, listed, ..
            } => {
                let name = spec.name.clone();
                // A fresh start swaps a leftover slot out, and a
                // replacement swaps the stale spec out: a lazy
                // `NotStarted` or failed slot with the same name never
                // serves again once the apply installs the new one.
                servers.remove(&name);
                support::lock(&servers.slots).push(slot);
                cache::write(inputs.cache, &name, &spec.key(), &listed);
            }
            StagedRow::ShadowFailed { spec, .. } => {
                // A non-required failure still drops the stale slot at
                // apply, for both dispositions: the configuration
                // changed (a replacement) or a leftover lazy slot never
                // served (a fresh start), so no stale slot survives,
                // with the server showing failed.
                servers.remove(&spec.name);
            }
            StagedRow::Remove { name } => {
                servers.remove(&name);
            }
        }
    }
}
