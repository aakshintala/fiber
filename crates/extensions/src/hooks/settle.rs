//! Settling a session's extension names before its tool set is fixed
//! (`docs/extensions.md`, "What a package holds", "Registering" and
//! "Commands and screens"), from what each started extension registered.
//!
//! In order: a tool named like a built-in that the manifest's `replaces`
//! does not list unloads its extension; commands settle over the rest,
//! whose own `replaces` check may unload more; then a tool name two or more
//! of the remaining extensions register goes to none of them. An unloaded
//! extension loses every registration and takes part in no later step.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use config::Config;
use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::Notice;
use contract::tool::BUILT_IN_TOOLS;

use crate::commands::{CommandSource, SessionCommands};
use crate::lua::LuaExtension;

/// What one started extension registered, by name.
pub(super) struct Meta {
    /// The extension's name, as `extensions_loaded` carries it.
    pub(super) extension: String,
    /// The manifest's `replaces`.
    pub(super) replaces: Vec<String>,
    /// Each registered command's `(name, description)`.
    pub(super) commands: Vec<(String, String)>,
    /// Each registered tool's name.
    pub(super) tools: Vec<String>,
}

/// What settling decided.
pub(super) struct Settled {
    /// The extensions not loaded for the session, by name.
    pub(super) unloaded: BTreeSet<String>,
    /// One per unloaded extension, command rename or conflict, and tool clash.
    pub(super) notices: Vec<Notice>,
    /// Each tool the session declares, as `(extension, tool)`.
    pub(super) tools: BTreeSet<(String, String)>,
}

/// Settles `metas`, given in load order. The command step builds the
/// session's commands from metadata alone; `home` and `clock` give that
/// build extensions that never start.
pub(super) fn settle(
    metas: &[Meta],
    config: &Config,
    home: &Path,
    clock: &Arc<dyn Clock>,
) -> Settled {
    let mut notices = Vec::new();
    let mut unloaded = BTreeSet::new();
    for meta in metas {
        // The first offending name, in name order.
        let undeclared = meta
            .tools
            .iter()
            .filter(|name| BUILT_IN_TOOLS.contains(&name.as_str()) && !meta.replaces.contains(name))
            .min();
        if let Some(name) = undeclared {
            unloaded.insert(meta.extension.clone());
            notices.push(Notice {
                code: ErrorCode::ExtensionFailed,
                message: format!(
                    "Tool `{name}` replaces a built-in tool its manifest does not list in `replaces`; `{}` is not loaded.",
                    meta.extension
                ),
                extension: Some(meta.extension.clone()),
            });
        }
    }
    let sources: Vec<CommandSource> = metas
        .iter()
        .filter(|meta| !unloaded.contains(&meta.extension))
        .map(|meta| CommandSource {
            extension: meta.extension.clone(),
            replaces: meta.replaces.clone(),
            commands: meta.commands.clone(),
            lua: Arc::new(LuaExtension::new(
                &meta.extension,
                home,
                home,
                Arc::clone(clock),
            )),
        })
        .collect();
    let commands = SessionCommands::build(&sources, config);
    notices.extend(commands.notices());
    unloaded.extend(commands.unloaded());
    let mut owners: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for meta in metas
        .iter()
        .filter(|meta| !unloaded.contains(&meta.extension))
    {
        for tool in &meta.tools {
            owners
                .entry(tool.as_str())
                .or_default()
                .insert(meta.extension.as_str());
        }
    }
    let mut tools = BTreeSet::new();
    for (tool, owners) in owners {
        // `BTreeSet` iterates in order, so the names are sorted.
        let named: Vec<String> = owners.iter().map(|name| format!("`{name}`")).collect();
        match named.split_last() {
            Some((last, rest)) if !rest.is_empty() => notices.push(Notice {
                code: ErrorCode::ExtensionFailed,
                message: format!(
                    "Extensions {} and {last} both register the tool `{tool}`, so neither gets it.",
                    rest.join(", ")
                ),
                extension: None,
            }),
            _ => tools.extend(
                owners
                    .iter()
                    .map(|owner| ((*owner).to_owned(), tool.to_owned())),
            ),
        }
    }
    Settled {
        unloaded,
        notices,
        tools,
    }
}

#[cfg(test)]
#[path = "settle_tests.rs"]
mod tests;
