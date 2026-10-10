//! An MCP tool's effects from its server's hints, or the person's
//! override (`docs/mcp.md`, "Effects"). An override replaces the server's
//! hint set as a whole: keys it does not name are absent, not inherited.

use contract::shapes::{DeclaredEffects, Effect};
use serde_json::{Map, Value};

/// One tool's hints: each of MCP's `...Hint` keys, present or absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hints {
    /// `readOnlyHint`.
    pub read_only: Option<bool>,
    /// `destructiveHint`.
    pub destructive: Option<bool>,
    /// `openWorldHint`.
    pub open_world: Option<bool>,
}

impl Hints {
    /// The person's override for one tool
    /// (`docs/configuration.md`, "MCP servers"): the same three keys, each a
    /// boolean, replacing the server's set as a whole.
    pub fn from_override(map: &Map<String, Value>) -> Self {
        Self {
            read_only: hint(map, "readOnlyHint"),
            destructive: hint(map, "destructiveHint"),
            open_world: hint(map, "openWorldHint"),
        }
    }

    /// The declared effects (`docs/mcp.md`, "Effects"): `readOnlyHint`
    /// true wins over everything; else `destructiveHint` true is
    /// irreversible writes, false is reversible writes, and absent either
    /// hint is irreversible executes. `network` follows unless
    /// `openWorldHint` is false. An MCP tool declares no paths.
    pub fn declared(&self) -> DeclaredEffects {
        let (primary, reversible) = if self.read_only == Some(true) {
            (Effect::Reads, true)
        } else if self.destructive == Some(true) {
            (Effect::Writes, false)
        } else if self.destructive == Some(false) {
            (Effect::Writes, true)
        } else {
            (Effect::Executes, false)
        };
        let mut effects = vec![primary];
        if self.open_world != Some(false) {
            effects.push(Effect::Network);
        }
        DeclaredEffects {
            effects,
            reversible,
            paths: None,
        }
    }
}

fn hint(map: &Map<String, Value>, key: &str) -> Option<bool> {
    map.get(key).and_then(Value::as_bool)
}

#[cfg(test)]
#[path = "effects_tests.rs"]
mod tests;
