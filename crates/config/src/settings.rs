//! Every configuration key with its effective value and layer, as the
//! terminal's `/settings` lists them (`docs/tui.md`, "Swapped views";
//! `docs/configuration.md`, "Keys", "Layers").

use std::collections::BTreeMap;

use serde_json::Value;

use crate::keys::{self, WriteScope};
use crate::path::{display, get};
use crate::{Config, Layer, Source};

/// A key's effective value as `/settings` shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    /// No layer sets it and it has no default.
    Unset,
    /// The effective value and the highest layer setting it.
    Value(Value, Source),
    /// A list whose layers all apply (`skills.disabled`): each name with
    /// the lowest layer listing it.
    Union(Vec<(String, Source)>),
    /// A value that may hold a secret, described without it: a credential
    /// source's kind and a command's program, or an `env` map's names.
    Redacted(String, Source),
}

/// One configuration key as `/settings` lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingInfo {
    /// The dotted key, a name holding a `.` quoted.
    pub key: String,
    /// Its effective value.
    pub value: SettingValue,
    /// The files it may be written to.
    pub scope: WriteScope,
}

impl Config {
    /// Every key "Keys" names with no `*` in its path, and every instance
    /// of a `*` key some layer sets, sorted by key. A credential source
    /// shows its kind and a command's program only, and a server's `env`
    /// its variable names only: their values may be secrets.
    pub fn settings(&self) -> Vec<SettingInfo> {
        let mut found: BTreeMap<String, Vec<String>> = keys::plain_paths()
            .map(|path| {
                let segments: Vec<String> = path.split('.').map(str::to_owned).collect();
                (display(&segments), segments)
            })
            .collect();
        for (_, layer) in &self.layers {
            instances(layer, &mut Vec::new(), &mut found);
        }
        found
            .into_iter()
            .filter_map(|(key, segments)| {
                let row = keys::leaf(&segments)?;
                let value = self.setting(&segments);
                Some(SettingInfo {
                    key,
                    value,
                    scope: row.scope,
                })
            })
            .collect()
    }

    /// The value `layer`'s own file sets at `key`, ignoring every other
    /// layer: what a write to that file starts from.
    pub fn in_layer(&self, key: &str, layer: Layer) -> Option<Value> {
        let segments = crate::path::parse(key)?;
        self.layers
            .iter()
            .find(|(source, _)| source.layer() == Some(layer))
            .and_then(|(_, value)| get(value, &segments).cloned())
    }

    /// One key's value as `/settings` shows it.
    fn setting(&self, segments: &[String]) -> SettingValue {
        if keys::leaf(segments).is_some_and(|row| row.merge == keys::Merge::Union) {
            let names = self.unioned(segments);
            if !names.is_empty() {
                return SettingValue::Union(names);
            }
        }
        let Some((value, from)) = self.get(&display(segments), None) else {
            return SettingValue::Unset;
        };
        match segments {
            [providers, _, credentials, _]
                if providers == "providers" && credentials == "credentials" =>
            {
                let shown = crate::credential::source(&value)
                    .map_or_else(|| "unreadable".to_owned(), |declared| declared.describe());
                SettingValue::Redacted(shown, from)
            }
            [mcp, servers, _, env] if mcp == "mcp" && servers == "servers" && env == "env" => {
                let names: Vec<&str> = value
                    .as_object()
                    .into_iter()
                    .flat_map(|map| map.keys().map(String::as_str))
                    .collect();
                SettingValue::Redacted(format!("names {}", names.join(", ")), from)
            }
            _ => SettingValue::Value(value, from),
        }
    }
}

/// Every key path under `value` that names a row of "Keys", by its dotted
/// form.
fn instances(value: &Value, path: &mut Vec<String>, found: &mut BTreeMap<String, Vec<String>>) {
    let Some(map) = value.as_object() else {
        return;
    };
    for (name, inner) in map {
        path.push(name.clone());
        if keys::leaf(path).is_some() {
            found.insert(display(path), path.clone());
        } else {
            instances(inner, path, found);
        }
        path.pop();
    }
}
