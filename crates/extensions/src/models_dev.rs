//! Fetches and merges the advisory models.dev catalog over generated provider data.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use config::ModelData;
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
pub fn merge_admission(
    home: &Path,
    extension_dir: &Path,
    provider: &str,
    models: Vec<ModelData>,
    enabled: bool,
) -> Vec<ModelData> {
    let catalog = if enabled {
        config::read_models_dev(home)
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .unwrap_or_else(|| Value::Object(Default::default()))
    } else {
        Value::Object(Default::default())
    };
    let path = extension_dir
        .join("providers")
        .join(format!("{provider}.json"));
    let Ok(bytes) = fs::read(path) else {
        return cautious(models);
    };
    let Ok(file) = serde_json::from_slice::<Value>(&bytes) else {
        return cautious(models);
    };
    let meta = meta_of(&file).unwrap_or_default();
    let generated: Vec<ModelData> = file
        .get("models")
        .cloned()
        .and_then(|models| serde_json::from_value(models).ok())
        .unwrap_or_default();
    enrich(models, &generated, &meta, &catalog)
}

fn cautious(models: Vec<ModelData>) -> Vec<ModelData> {
    models.into_iter().map(cautious_model).collect()
}

fn cautious_model(mut model: ModelData) -> ModelData {
    model.is_new = true;
    model.input = vec!["text".to_owned()];
    model.thinking_levels.clear();
    model.thinking_default = None;
    model.context_window = Some(128_000);
    model.max_output_tokens = Some(8_192);
    model.cost = None;
    model
}

fn enrich(
    models: Vec<ModelData>,
    generated: &[ModelData],
    meta: &CatalogMeta,
    catalog: &Value,
) -> Vec<ModelData> {
    models
        .into_iter()
        .map(|mut model| {
            let table = generated.iter().find(|known| known.id == model.id);
            let (source, id) = resolve(&model.id, meta);
            let fetched = catalog.get(&source).and_then(|value| value.get(&id));
            if table.is_none() && fetched.is_none() {
                return cautious_model(model);
            }
            model.is_new = false;
            if let Some(table) = table {
                model.name.clone_from(&table.name);
                model.context_window = table.context_window;
                model.max_output_tokens = table.max_output_tokens;
                model.input.clone_from(&table.input);
                model.cost.clone_from(&table.cost);
                model.thinking_levels.clone_from(&table.thinking_levels);
                model.thinking_default = table.thinking_default;
            }
            if fetched.is_some() {
                if let Some(merged) = merge(vec![model.clone()], meta, catalog).into_iter().next() {
                    model = merged;
                }
            }
            model
        })
        .collect()
}

/// Merge cached catalog fields into installed provider rows without adding models.
pub fn apply(
    home: &Path,
    enabled: bool,
    providers: &mut Providers,
) -> Vec<contract::events::Notice> {
    let names: Vec<String> = providers.names().map(str::to_owned).collect();
    let mut notices = Vec::new();
    for name in names {
        let base = providers
            .provider_source_dir(&name)
            .unwrap_or_else(|| home.join("providers").join(&name));
        let mut data = providers.data(&name);
        data.models = merge_admission(home, &base, &name, data.models, enabled);
        notices.extend(providers.set_models(&name, data.models));
    }
    notices
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
