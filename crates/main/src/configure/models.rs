//! The hub's `models` operation: the hub's own catalogue, read the way
//! the local picker reads it (`docs/tui.md`, "Swapped views").

use std::path::Path;
use std::sync::Arc;

use contract::clock::Clock;
use contract::files::PathLock;
use contract::shapes::Failure;
use serde_json::{Map, Value, json};

/// The hub's installed models at `refresh`: the local picker's read,
/// encoded the way the picker lists it. `None` fields are absent, never
/// `null`, and a failed read keeps its code, so an invalid hub
/// configuration answers `config_invalid`.
impl super::Seam {
    pub(super) fn models(
        &self,
        workspace: &Path,
        refresh: tui::Refresh,
        clock: Arc<dyn Clock>,
    ) -> Result<Value, Failure> {
        let catalogue = crate::model_list::catalogue(
            self.home.clone(),
            workspace.to_path_buf(),
            clock,
            Arc::clone(&self.locks) as Arc<dyn PathLock>,
            Arc::clone(&self.reads),
            refresh,
        )
        .map_err(|error| Failure {
            code: error.code(),
            message: error.to_string(),
            provider: None,
            retry_after_ms: None,
        })?;
        Ok(json!({
            "models": catalogue.models.iter().map(entry).collect::<Vec<_>>(),
            "notices": catalogue.notices,
            "installable": catalogue.installable,
            "lists": catalogue.lists.iter().map(age).collect::<Vec<_>>(),
            "left_out": catalogue.left_out.iter().map(left_out).collect::<Vec<_>>(),
            "logged_out": catalogue.logged_out.iter().map(logged_out).collect::<Vec<_>>(),
        }))
    }
}

/// One provider with data but no credential, with its login capability
/// and declared source hints.
fn logged_out(provider: &tui::LoggedOutProvider) -> Value {
    json!({
        "provider": provider.provider,
        "login": provider.login,
        "hint": provider.hint,
    })
}

/// One model as the picker lists it: `name`, `default_level`,
/// `configured` and `price` only when the model names them.
fn entry(entry: &tui::ModelEntry) -> Value {
    let mut value = Map::from_iter([
        ("reference".to_owned(), json!(entry.reference)),
        ("provider".to_owned(), json!(entry.provider)),
        ("id".to_owned(), json!(entry.id)),
        ("levels".to_owned(), json!(entry.levels)),
        ("roles".to_owned(), json!(entry.roles)),
    ]);
    if let Some(name) = &entry.name {
        drop(value.insert("name".to_owned(), json!(name)));
    }
    if let Some(default_level) = &entry.default_level {
        drop(value.insert("default_level".to_owned(), json!(default_level)));
    }
    if let Some(configured) = &entry.configured {
        drop(value.insert("configured".to_owned(), json!(configured)));
    }
    if let Some(price) = &entry.price {
        drop(value.insert("price".to_owned(), cost(price)));
    }
    Value::Object(value)
}

/// One model's prices in integer micro-dollars per million tokens, tiers
/// ascending by `input_tokens_above`.
fn cost(price: &tui::Price) -> Value {
    json!({
        "micros_per_mtok": price.micros_per_mtok,
        "input": price.input,
        "output": price.output,
        "tiers": price.tiers.iter().map(|tier| json!({
            "input_tokens_above": tier.input_tokens_above,
            "micros_per_mtok": tier.micros_per_mtok,
            "input": tier.input,
            "output": tier.output,
        })).collect::<Vec<_>>(),
    })
}

/// One provider's list time: `updated_ms` only with a cached copy.
fn age(age: &tui::ListAge) -> Value {
    let mut value = Map::from_iter([
        ("provider".to_owned(), json!(age.provider)),
        ("stale".to_owned(), json!(age.stale)),
    ]);
    if let Some(updated_ms) = age.updated_ms {
        drop(value.insert("updated_ms".to_owned(), json!(updated_ms)));
    }
    Value::Object(value)
}

/// One group with no placeholder value: the raw extension name and the
/// file as read ride the wire, with each model's display name only when
/// the provider data names one.
fn left_out(group: &tui::LeftOutGroup) -> Value {
    Value::Object(Map::from_iter([
        ("provider".to_owned(), json!(group.provider)),
        ("extension".to_owned(), json!(group.extension)),
        ("file".to_owned(), json!(group.file)),
        ("setting".to_owned(), json!(group.setting)),
        ("login".to_owned(), json!(group.login)),
        (
            "models".to_owned(),
            Value::Array(group.models.iter().map(left_out_model).collect()),
        ),
    ]))
}

/// One left-out model: its id always, its name only when named.
fn left_out_model(model: &tui::LeftOutModel) -> Value {
    let mut value = Map::from_iter([("id".to_owned(), json!(model.id))]);
    if let Some(name) = &model.name {
        drop(value.insert("name".to_owned(), json!(name)));
    }
    Value::Object(value)
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
