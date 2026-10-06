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
    /// The hints a server's `annotations` object declares. Unknown JSON
    /// types are absent, never an error: a server's schema is best effort
    /// (`docs/dependencies.md`, "Written ourselves").
    pub fn from_annotations(value: &Value) -> Self {
        let object = value.as_object();
        Self {
            read_only: object.and_then(|map| hint(map, "readOnlyHint")),
            destructive: object.and_then(|map| hint(map, "destructiveHint")),
            open_world: object.and_then(|map| hint(map, "openWorldHint")),
        }
    }

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

    /// The `annotations` object these hints round-trip through: each
    /// present hint as its boolean, absent hints left out, so
    /// [`Hints::from_annotations`] reads back what this wrote.
    pub fn to_annotations(&self) -> Value {
        let mut map = Map::new();
        if let Some(read_only) = self.read_only {
            map.insert("readOnlyHint".to_owned(), Value::Bool(read_only));
        }
        if let Some(destructive) = self.destructive {
            map.insert("destructiveHint".to_owned(), Value::Bool(destructive));
        }
        if let Some(open_world) = self.open_world {
            map.insert("openWorldHint".to_owned(), Value::Bool(open_world));
        }
        Value::Object(map)
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
