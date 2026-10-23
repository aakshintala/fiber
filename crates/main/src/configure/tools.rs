//! `/tools` through the seam (`docs/tui.md`, "Swapped views"): every MCP
//! server and extension whose configuration holds tool lists, and one
//! locked write of a switch to the project's or the global file, never a
//! repository's (`docs/configuration.md`, "When Fiber writes").

use std::path::Path;

use serde_json::Value;
use tui::{ConfigureError, SwitchScope, ToolGroup, ToolLists, ToolSwitches};

use super::{Seam, from_config, from_failure};

/// `group` as the owner `config` keys its `tools` lists under.
fn owner(group: &ToolGroup) -> config::ToolOwner<'_> {
    match group {
        ToolGroup::Mcp(server) => config::ToolOwner::Mcp(server),
        ToolGroup::Extension(name) => config::ToolOwner::Extension(name),
    }
}

/// A layer's lists from its `enabled` and `disabled` values.
fn lists(enabled: Option<Value>, disabled: Option<Value>) -> ToolLists {
    ToolLists {
        enabled: enabled.map(|value| crate::mcp_servers::strings(&value)),
        disabled: disabled
            .map(|value| crate::mcp_servers::strings(&value))
            .unwrap_or_default(),
    }
}

impl Seam {
    /// Every MCP server and extension whose merged configuration holds
    /// tool lists, with the workspace's effective lists and the global
    /// file's own. A name holding a quote is left out: a dotted key
    /// cannot quote it.
    pub(super) fn read_switches(
        &self,
        workspace: &Path,
    ) -> Result<Vec<ToolSwitches>, ConfigureError> {
        let config = self.load(workspace)?;
        let merged = config.merged(None);
        let mut groups = Vec::new();
        if let Some(servers) = merged
            .get("mcp")
            .and_then(|mcp| mcp.get("servers"))
            .and_then(Value::as_object)
        {
            for (name, server) in servers {
                if name.contains('"') || server.get("tools").is_none() {
                    continue;
                }
                groups.push(ToolGroup::Mcp(name.clone()));
            }
        }
        if let Some(extensions) = merged.get("extensions").and_then(Value::as_object) {
            for (name, extension) in extensions {
                if name.contains('"') || extension.get("tools").is_none() {
                    continue;
                }
                groups.push(ToolGroup::Extension(name.clone()));
            }
        }
        groups
            .into_iter()
            .map(|group| {
                let prefix = owner(&group).key();
                Ok(ToolSwitches {
                    group,
                    project: lists(
                        config
                            .get(&format!("{prefix}.enabled"), None)
                            .map(|(value, _)| value),
                        config
                            .get(&format!("{prefix}.disabled"), None)
                            .map(|(value, _)| value),
                    ),
                    everywhere: lists(
                        config.in_layer(&format!("{prefix}.enabled"), config::Layer::Global),
                        config.in_layer(&format!("{prefix}.disabled"), config::Layer::Global),
                    ),
                })
            })
            .collect()
    }

    /// Switches `tool` (its own name) of `group` on or off in `scope`'s
    /// file: one locked read-modify-write of one file. A layer whose file
    /// holds no list starts from the list it inherits, so a write never
    /// narrows one. The `enabled` half of switching on is decided under
    /// the file's lock, never from the load here; a concurrent change to a
    /// lower file between the load and the write is seen at the next
    /// switch.
    pub(super) fn write_switch(
        &self,
        workspace: &Path,
        group: &ToolGroup,
        tool: &str,
        scope: SwitchScope,
        on: bool,
    ) -> Result<(), ConfigureError> {
        let config = self.load(workspace)?;
        let project = self.project_key(workspace).map_err(from_failure)?;
        let owner = owner(group);
        let layer = match scope {
            SwitchScope::Project => config::SwitchLayer::Project,
            SwitchScope::Everywhere => config::SwitchLayer::Global,
        };
        config::switch_tool(
            &config,
            config::ToolSwitch {
                home: &self.home,
                workspace,
                project: &project,
                owner,
                tool,
                layer,
                on,
            },
        )
        .map_err(from_config)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
