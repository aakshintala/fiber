//! The reload's commit: stop and remove old slots, install the new ones,
//! write caches, and declare. The install itself stages nothing and cannot
//! fail: every fallible required staging ran before the first install.

use std::sync::{Arc, PoisonError};

use contract::events::{McpServerFailed, McpServerReady, ReloadedServers, ToolInfo};
use contract::tool::{ServerRecord, Tool};

use crate::cache;
use crate::prompt::{PromptSource, Prompts};
use crate::start::{ServerSpec, Servers};

use super::SyncInputs;
use super::rows::Disposition;
use super::stage::StagedRow;

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

/// Commits the staged sync: the install runs only after every required
/// shadow start has succeeded, so no two slots for one name ever serve
/// at once — and the install itself cannot fail, so a keep that died
/// racing the commit still declares its staged tools while its cache
/// stays as the stage left it.
pub fn commit_sync(
    staged: super::stage::StagedSync,
    servers: &Servers,
    inputs: &SyncInputs,
) -> Synced {
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
        crate::start::resources_declaration(&servers.slots, inputs.artifacts, inputs.images)
    {
        installing.tools.push(declaration);
    }
    installing
}
