//! A provider's discovered model list, cached at `cache/models/<name>.json`
//! in Fiber home (`docs/configuration.md`, "A provider's data", and
//! `docs/state.md`, "Cache"). The cache is always safe to delete, so a copy
//! that is missing or no longer reads as a model list is no copy at all.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::ConfigError;
use crate::extension::ModelData;
use crate::home::one_file_name;
use crate::write::write_atomic;

/// The cached model list of `provider`; `None` when there is none, or the
/// file does not hold a model list.
pub fn read_model_cache(
    home: &Path,
    provider: &str,
) -> Result<Option<Vec<ModelData>>, ConfigError> {
    let file = cache_file(home, provider)?;
    match fs::read(&file) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Io { file, source }),
    }
}

/// Replaces the cached model list of `provider` whole, by rename, so two
/// sessions writing at once leave one whole file.
pub fn write_model_cache(home: &Path, provider: &str, models: &Value) -> Result<(), ConfigError> {
    write_atomic(
        &cache_file(home, provider)?,
        models.to_string().as_bytes(),
        0o600,
    )
}

fn cache_file(home: &Path, provider: &str) -> Result<PathBuf, ConfigError> {
    if !one_file_name(provider) {
        return Err(ConfigError::WrongType {
            source_name: "a provider's name".into(),
            key: provider.into(),
            expected: "one file name in cache/models/".into(),
        });
    }
    Ok(home.join("cache/models").join(format!("{provider}.json")))
}
