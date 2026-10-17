//! The argument `models()` receives (`docs/model-routing.md`, "Model
//! discovery"): the provider's key as a session reads it, and the host each
//! declared placeholder holds. A keyed argument reaches Lua only beside a
//! host the person set: `Providers::models_arg` builds none while a
//! declared placeholder has no host, so the token goes only to a host the
//! person set.

use std::collections::BTreeMap;

use contract::Secret;
use serde_json::{Map, Value};

/// The argument `models()` receives: the session's key for the provider,
/// absent when the provider registers `credential()` or no key is found,
/// and the host each declared placeholder holds. `Debug` prints the key
/// as `Secret(redacted)`.
#[derive(Debug, Default)]
pub struct ModelsArg {
    /// The session's key for the provider, read as a session reads it.
    pub key: Option<Secret>,
    /// Each declared placeholder's host.
    pub placeholders: BTreeMap<String, String>,
}

impl ModelsArg {
    /// The argument as Lua receives it: `key` omitted when there is none.
    pub(crate) fn to_value(&self) -> Value {
        let mut arg = Map::new();
        if let Some(key) = &self.key {
            arg.insert("key".into(), Value::String(key.expose().to_owned()));
        }
        let placeholders: Map<String, Value> = self
            .placeholders
            .iter()
            .map(|(name, host)| (name.clone(), Value::String(host.clone())))
            .collect();
        arg.insert("placeholders".into(), Value::Object(placeholders));
        Value::Object(arg)
    }
}
