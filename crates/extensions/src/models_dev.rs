//! Fetches and merges the advisory models.dev catalog over generated provider data.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use config::ModelData;
use serde::Deserializer;
use serde::de::{DeserializeSeed, MapAccess, Visitor};
use serde_json::Value;

use crate::Providers;

/// The public models.dev catalog endpoint.
pub const URL: &str = "https://models.dev/api.json";
/// A fetch operation returning the response body or a transport error.
pub type Fetch = Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync>;

/// Advisory mapping from served provider ids to models.dev catalog entries.
#[derive(Debug, Clone, Default)]
pub struct CatalogMeta {
    /// Catalog provider for ordinary table-backed entries.
    pub source: Option<String>,
    /// Fields pinned to generated values, indexed by served model id.
    pub pins: BTreeMap<String, Vec<String>>,
    /// Prefix removed from vendor model ids before lookup.
    pub strip_prefix: Option<String>,
    /// Resolve vendor/model ids by their source and model segments.
    pub split_source: bool,
    /// Ordered catalog sources for multi-vendor entries.
    pub sources: Vec<String>,
    /// Per-served-id catalog source overrides.
    pub source_aliases: BTreeMap<String, String>,
}

/// Build the production fetch using environment proxy settings and net limits.
pub fn live_fetch() -> Fetch {
    Arc::new(|url| {
        fetch(
            &crate::prepare::agent(ureq::Proxy::try_from_env(), net::LIMITS),
            url,
        )
        .map_err(|error| error.to_string())
    })
}

/// Fetch response bytes from one URL using the provided HTTP agent.
pub fn fetch(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, ureq::Error> {
    let mut response = agent.get(url).call()?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut bytes)
        .map_err(ureq::Error::Io)?;
    Ok(bytes)
}

/// Whether the catalog is enabled and missing, unreadable or older than its age bound.
pub fn should_fetch(
    home: &Path,
    enabled: bool,
    max_age: Option<Duration>,
    now: SystemTime,
) -> bool {
    if !enabled {
        return false;
    }
    match config::models_dev_age(home, now) {
        Ok(None) | Err(_) => true,
        Ok(Some(age)) => max_age.is_none_or(|max| age > max),
    }
}

fn valid_catalog(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes).is_ok_and(|value| value.as_object().is_some())
}

fn with_lock(home: &Path, blocking: bool, action: impl FnOnce()) {
    let Ok(path) = config::models_dev_lock_file(home) else {
        return;
    };
    let Some(parent) = path.parent() else { return };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(file) = File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
    else {
        return;
    };
    let locked = if blocking {
        file.lock().is_ok()
    } else {
        file.try_lock().is_ok()
    };
    if locked {
        action();
    }
}

/// Refresh the catalog when due, skipping a lock held by another refresher.
pub fn refresh_if_due(
    home: &Path,
    enabled: bool,
    max_age: Option<Duration>,
    now: SystemTime,
    fetcher: &Fetch,
) {
    if !should_fetch(home, enabled, max_age, now) {
        return;
    }
    with_lock(home, false, || {
        if !should_fetch(home, enabled, max_age, now) {
            return;
        }
        if let Ok(bytes) = fetcher(URL)
            && valid_catalog(&bytes)
        {
            drop(config::write_models_dev(home, &bytes));
        }
    });
}

/// Refresh the catalog for a forced read, skipping when another refresh holds the lock.
pub fn refresh_forced(home: &Path, enabled: bool, now: SystemTime, fetcher: &Fetch) {
    if !enabled {
        return;
    }
    with_lock(home, false, || {
        if should_fetch(home, enabled, None, now)
            && let Ok(bytes) = fetcher(URL)
            && valid_catalog(&bytes)
        {
            drop(config::write_models_dev(home, &bytes));
        }
    });
}

/// Start a detached refresh thread only when the catalog is due.
pub fn spawn_if_due(
    home: PathBuf,
    enabled: bool,
    max_age: Option<Duration>,
    now: SystemTime,
    fetcher: Fetch,
) -> Option<JoinHandle<()>> {
    should_fetch(&home, enabled, max_age, now)
        .then(|| std::thread::spawn(move || refresh_if_due(&home, enabled, max_age, now, &fetcher)))
}

/// Merge the cached catalog into rows before provider admission validates them.
pub(crate) fn merge_admission(
    home: &Path,
    extension_dir: &Path,
    provider: &str,
    models: Vec<ModelData>,
    enabled: bool,
) -> Vec<ModelData> {
    if !enabled {
        return models;
    }
    let Some(file) = config::open_models_dev(home).ok().flatten() else {
        return models;
    };
    let path = extension_dir
        .join("providers")
        .join(format!("{provider}.json"));
    let Some(meta) = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|file| meta_of(&file))
    else {
        return models;
    };
    let selected = selections(&models, &meta);
    let Some(catalog) = select_catalog(file, &selected) else {
        return models;
    };
    merge(models, &meta, &catalog)
}

/// Merge cached catalog fields into installed provider rows without adding models.
pub fn apply(
    home: &Path,
    enabled: bool,
    providers: &mut Providers,
) -> Vec<contract::events::Notice> {
    if !enabled {
        return Vec::new();
    }
    let Some(file) = config::open_models_dev(home).ok().flatten() else {
        return Vec::new();
    };
    let names: Vec<String> = providers.names().map(str::to_owned).collect();
    let mut pending = Vec::new();
    let mut selected = Selection::default();
    for name in &names {
        let Ok(dir) = provider_dir(home, providers, name) else {
            continue;
        };
        let Ok(provider_bytes) = fs::read(dir) else {
            continue;
        };
        let Ok(file) = serde_json::from_slice::<Value>(&provider_bytes) else {
            continue;
        };
        let Some(meta) = meta_of(&file) else { continue };
        let data = providers.data(name);
        for (source, ids) in selections(&data.models, &meta).0 {
            for id in ids {
                selected.insert(source.clone(), id);
            }
        }
        pending.push((name.clone(), meta, data.models.clone()));
    }
    let Some(catalog) = select_catalog(file, &selected) else {
        return Vec::new();
    };
    let mut notices = Vec::new();
    for (name, meta, mut models) in pending {
        models = merge(models, &meta, &catalog);
        notices.extend(providers.set_models(&name, models));
    }
    notices
}

fn provider_dir(
    home: &Path,
    providers: &Providers,
    provider: &str,
) -> Result<PathBuf, std::io::Error> {
    let base = providers
        .provider_source_dir(provider)
        .unwrap_or_else(|| home.join("providers").join(provider));
    Ok(base.join("providers").join(format!("{provider}.json")))
}

#[derive(Default)]
struct Selection(BTreeMap<String, BTreeSet<String>>);

impl Selection {
    fn insert(&mut self, source: String, id: String) {
        self.0.entry(source).or_default().insert(id);
    }

    fn get(&self, source: &str) -> Option<&BTreeSet<String>> {
        self.0.get(source)
    }
}

fn selections(models: &[ModelData], meta: &CatalogMeta) -> Selection {
    let mut selected = Selection::default();
    for model in models {
        let (source, id) = resolve(&model.id, meta);
        selected.insert(source, id);
    }
    selected
}

fn select_catalog(reader: impl Read, selected: &Selection) -> Option<Value> {
    let mut deserializer = serde_json::Deserializer::from_reader(std::io::BufReader::new(reader));
    let catalog = SelectionSeed(selected)
        .deserialize(&mut deserializer)
        .ok()?;
    deserializer.end().ok()?;
    Some(catalog)
}

struct SelectionSeed<'a>(&'a Selection);

impl<'de> DeserializeSeed<'de> for SelectionSeed<'_> {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct CatalogVisitor<'a>(&'a Selection);
        impl<'de> Visitor<'de> for CatalogVisitor<'_> {
            type Value = Value;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a models.dev catalog object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
                let mut catalog = serde_json::Map::new();
                while let Some(source) = map.next_key::<String>()? {
                    if let Some(ids) = self.0.get(&source) {
                        let rows = map.next_value_seed(ModelRows(ids))?;
                        catalog.insert(source, rows);
                    } else {
                        map.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
                Ok(Value::Object(catalog))
            }
        }
        deserializer.deserialize_map(CatalogVisitor(self.0))
    }
}

struct ModelRows<'a>(&'a BTreeSet<String>);

impl<'de> DeserializeSeed<'de> for ModelRows<'_> {
    type Value = Value;
    fn deserialize<D>(self, deserializer: D) -> Result<Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RowsVisitor<'a>(&'a BTreeSet<String>);
        impl<'de> Visitor<'de> for RowsVisitor<'_> {
            type Value = Value;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a models.dev provider object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
                let mut rows = serde_json::Map::new();
                while let Some(id) = map.next_key::<String>()? {
                    if self.0.contains(&id) {
                        rows.insert(id, map.next_value::<Value>()?);
                    } else {
                        map.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
                Ok(Value::Object(rows))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Value, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(Value::Null)
            }
            // Non-object rows have no selectable fields, so represent them as Null.
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_string<E: serde::de::Error>(self, _: String) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
        }
        deserializer.deserialize_any(RowsVisitor(self.0))
    }
}

/// Merge fetched fields into each generated row while retaining pins and Fiber-only fields.
pub fn merge(models: Vec<ModelData>, meta: &CatalogMeta, catalog: &Value) -> Vec<ModelData> {
    models
        .into_iter()
        .map(|mut model| {
            let (source, id) = resolve(&model.id, meta);
            let Some(fetched) = catalog.get(&source).and_then(|v| v.get(&id)) else {
                return model;
            };
            let pins = meta.pins.get(&model.id);
            if !pinned(pins, "name")
                && let Some(name) = fetched
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            {
                model.name = Some(name.to_owned());
            }
            let limits = fetched.get("limit");
            if !pinned(pins, "context_window")
                && let Some(n) = limits
                    .and_then(|v| v.get("context"))
                    .and_then(Value::as_u64)
                    .filter(|n| *n > 0)
            {
                model.context_window = Some(n);
            }
            if !pinned(pins, "max_output_tokens")
                && let Some(n) = limits
                    .and_then(|v| v.get("output"))
                    .and_then(Value::as_u64)
                    .filter(|n| *n > 0)
            {
                model.max_output_tokens = Some(n);
            }
            if !pinned(pins, "input")
                && let Some(input) = fetched
                    .get("modalities")
                    .and_then(|v| v.get("input"))
                    .and_then(Value::as_array)
            {
                let mut kinds = Vec::new();
                if input.iter().any(|v| v == "text") {
                    kinds.push("text".to_owned());
                }
                if input.iter().any(|v| v == "image") {
                    kinds.push("image".to_owned());
                }
                if !kinds.is_empty() {
                    model.input = kinds;
                }
            }
            if !pinned(pins, "cost")
                && let Some(cost) = fetched.get("cost")
            {
                model.cost = serde_json::from_value(cost.clone()).ok();
            }
            model
        })
        .collect()
}

fn pinned(pins: Option<&Vec<String>>, field: &str) -> bool {
    pins.is_some_and(|p| p.iter().any(|v| v == field))
}

fn resolve(id: &str, meta: &CatalogMeta) -> (String, String) {
    if let Some(source) = meta.source.as_ref() {
        return (source.clone(), id.to_owned());
    }
    if let Some(source) = meta.source_aliases.get(id) {
        if let Some(prefix) = meta.strip_prefix.as_deref() {
            return (
                source.clone(),
                id.strip_prefix(prefix).unwrap_or(id).to_owned(),
            );
        }
        if meta.split_source {
            return (
                source.clone(),
                id.split_once('/').map_or(id, |(_, tail)| tail).to_owned(),
            );
        }
        return (source.clone(), id.to_owned());
    }
    if meta.split_source
        && let Some((source, tail)) = id.split_once('/')
    {
        return (source.to_owned(), tail.to_owned());
    }
    let bare = meta
        .strip_prefix
        .as_deref()
        .and_then(|prefix| id.strip_prefix(prefix))
        .unwrap_or(id);
    (
        meta.sources.first().cloned().unwrap_or_default(),
        bare.to_owned(),
    )
}

/// Parse provider metadata tolerantly; malformed or absent metadata has no overlay.
pub fn meta_of(file: &Value) -> Option<CatalogMeta> {
    let value = file.get("models_dev")?.as_object()?;
    let strings = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default()
    };
    let pins = value
        .get("pins")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| {
                    v.as_array().map(|a| {
                        (
                            k.clone(),
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect(),
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(CatalogMeta {
        source: value
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned),
        pins,
        strip_prefix: value
            .get("strip_prefix")
            .and_then(Value::as_str)
            .map(str::to_owned),
        split_source: value
            .get("split_source")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        sources: value
            .get("sources")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        source_aliases: strings("source_aliases"),
    })
}

#[cfg(test)]
mod retained_catalog_tests {
    use super::{Selection, select_catalog};
    use serde_json::json;

    #[test]
    fn retains_only_selected_catalog_rows() {
        let bytes = br#"{"served":{"model":{"name":"kept"},"unserved-model":{"name":"drop"}},"unserved-source":{"model":{"name":"drop"}}}"#;
        let mut selected = Selection::default();
        selected.insert("served".to_owned(), "model".to_owned());

        let retained = select_catalog(&bytes[..], &selected).unwrap();

        assert_eq!(retained, json!({"served":{"model":{"name":"kept"}}}));
    }
}
