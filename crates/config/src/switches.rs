//! The `/tools` and `/skills` switch edits (`docs/configuration.md`, "When
//! Fiber writes"): what a switch writes to a person's file, never a
//! repository's. The terminal maps its groups and scopes onto these and
//! calls through (`docs/architecture.md`, "The call rules").

use std::path::Path;

use serde_json::Value;

use crate::error::ConfigError;
use crate::home::ProjectKey;
use crate::{Config, Layer, ListChange, ListEdit, edit_list};

/// Whose tools lists a `/tools` switch writes.
pub enum ToolOwner<'a> {
    /// An MCP server's `mcp.servers."<server>".tools`.
    Mcp(&'a str),
    /// An extension's `extensions."<name>".tools`, `name` as the person
    /// typed it: [`key`](ToolOwner::key) grows it to its full name.
    Extension(&'a str),
}

impl ToolOwner<'_> {
    /// The `tools` lists' dotted prefix for this owner.
    pub fn key(&self) -> String {
        match self {
            ToolOwner::Mcp(server) => format!("mcp.servers.\"{server}\".tools"),
            ToolOwner::Extension(name) => {
                format!("extensions.\"{}\".tools", crate::names::full_name(name))
            }
        }
    }
}

/// Which person's file a switch writes: a switch never writes a
/// repository's file (`docs/configuration.md`, "When Fiber writes"), so
/// there is no repository variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchLayer {
    /// The workspace's project's file in Fiber home.
    Project,
    /// Fiber home's `config.json`.
    Global,
}

impl SwitchLayer {
    /// This switch layer as the file [`edit_list`] writes.
    fn layer(self) -> Layer {
        match self {
            SwitchLayer::Project => Layer::Project,
            SwitchLayer::Global => Layer::Global,
        }
    }
}

/// One `/tools` switch: whose tool, in which person's file, and which way.
pub struct ToolSwitch<'a> {
    /// Fiber home, holding the person's files.
    pub home: &'a Path,
    /// The workspace, whose lower files the inherited list is read from.
    pub workspace: &'a Path,
    /// The workspace's project, naming the project file.
    pub project: &'a ProjectKey,
    /// Whose tools lists to write.
    pub owner: ToolOwner<'a>,
    /// The tool's own name.
    pub tool: &'a str,
    /// Which person's file to write.
    pub layer: SwitchLayer,
    /// True switches the tool on, false switches it off.
    pub on: bool,
}

/// Switches `switch`'s tool on or off in its layer's file: one locked
/// read-modify-write of one file. On removes the name from `disabled` and
/// adds it to `enabled` when a list is in force and lacks it; off adds the
/// name to `disabled`. A file holding no list starts from the one it
/// inherits, so a write never narrows one: for the project file that is
/// the repository's list, then the global one; the global file starts from
/// none. Whether `enabled` names the tool is decided under the file's
/// lock, never from `config` here; a concurrent change to a lower file
/// between the load and the write is seen at the next switch. `true` when
/// it wrote.
pub fn switch_tool(config: &Config, switch: ToolSwitch<'_>) -> Result<bool, ConfigError> {
    let prefix = switch.owner.key();
    let enabled = format!("{prefix}.enabled");
    let disabled = format!("{prefix}.disabled");
    let below = |key: &str| match switch.layer {
        SwitchLayer::Project => config
            .in_layer(key, Layer::Repository)
            .or_else(|| config.in_layer(key, Layer::Global))
            .map(|value| strings(&value)),
        SwitchLayer::Global => None,
    };
    let below_disabled = below(&disabled);
    let below_enabled = below(&enabled);
    let edits: Vec<ListEdit<'_>> = if switch.on {
        vec![
            ListEdit {
                key: &disabled,
                name: switch.tool,
                change: ListChange::Remove,
                inherited: below_disabled.as_deref(),
            },
            ListEdit {
                key: &enabled,
                name: switch.tool,
                change: ListChange::AddIfListed,
                inherited: below_enabled.as_deref(),
            },
        ]
    } else {
        vec![ListEdit {
            key: &disabled,
            name: switch.tool,
            change: ListChange::Add,
            inherited: below_disabled.as_deref(),
        }]
    };
    edit_list(
        switch.home,
        switch.workspace,
        switch.project,
        switch.layer.layer(),
        &edits,
    )
}

/// Switches `name` off or on in `layer`'s file: one locked
/// read-modify-write of one file. Off adds the name, on removes it; the
/// other file is untouched. Each file's own list decides, since both
/// apply, so a file with no list starts from none, never an inherited
/// one. `true` when it wrote.
pub fn switch_skill(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    layer: SwitchLayer,
    name: &str,
    on: bool,
) -> Result<bool, ConfigError> {
    let change = if on {
        ListChange::Remove
    } else {
        ListChange::Add
    };
    edit_list(
        home,
        workspace,
        project,
        layer.layer(),
        &[ListEdit {
            key: "skills.disabled",
            name,
            change,
            inherited: None,
        }],
    )
}

/// A list value as strings, skipping non-strings: empty for a missing key
/// or a non-array.
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "switches_tests.rs"]
mod tests;
