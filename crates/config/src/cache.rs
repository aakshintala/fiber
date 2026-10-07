//! A provider's discovered model list, cached at `cache/models/<name>.json`
//! in Fiber home (`docs/configuration.md`, "A provider's data", and
//! `docs/state.md`, "Cache"). The cache is always safe to delete, so a copy
//! that is missing or no longer reads as a model list is no copy at all.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

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

/// How old the cached model list of `provider` is, from its file's mtime;
/// `None` when no cached copy exists. A file newer than `now` is fresh:
/// its age is zero (`docs/model-routing.md`, "Model discovery").
pub fn model_cache_age(
    home: &Path,
    provider: &str,
    now: SystemTime,
) -> Result<Option<Duration>, ConfigError> {
    let file = cache_file(home, provider)?;
    let mtime = match fs::metadata(&file) {
        Ok(metadata) => metadata.modified().map_err(|source| ConfigError::Io {
            file: file.clone(),
            source,
        })?,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::Io {
                file: file.clone(),
                source,
            });
        }
    };
    Ok(Some(now.duration_since(mtime).unwrap_or(Duration::ZERO)))
}

/// The lock file a refresh of `provider` holds while it runs, beside the
/// cached list it replaces. The cache stays safe to delete, lock file
/// included: a crash leaves the file, and the lock it held dies with the
/// process (`docs/model-routing.md`, "Model discovery").
pub fn model_cache_lock_file(home: &Path, provider: &str) -> Result<PathBuf, ConfigError> {
    cache_file(home, provider).map(|path| path.with_extension("lock"))
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
