//! Fills a per-account host placeholder in a model's `base_url`
//! (`docs/model-routing.md`, "A per-account host").

use config::ConfigError;
use contract::ErrorCode;
use contract::events::Notice;

#[cfg(test)]
#[path = "placeholders_tests.rs"]
mod tests;

/// A `base_url` filled, or the name of its first placeholder with no value.
#[allow(
    dead_code,
    reason = "wired in Task 2; the red commit holds only the signature"
)]
#[derive(Debug, PartialEq)]
pub(super) enum Filled {
    /// The template with every placeholder replaced.
    Url(String),
    /// The first placeholder with no usable value, in template order.
    Missing(String),
}

/// `template` with each `{name}` replaced by `lookup(name)`; a lookup that
/// finds no usable value stops at that name.
#[allow(
    dead_code,
    reason = "wired in Task 2; the red commit holds only the signature"
)]
pub(super) fn fill(
    template: &str,
    _lookup: &dyn Fn(&str) -> Result<Option<serde_json::Value>, ConfigError>,
) -> Result<Filled, ConfigError> {
    Ok(Filled::Url(template.to_owned()))
}

/// The `model_unconfigured` notice for model `id` of `provider`, whose
/// placeholder `name` has no value.
#[allow(
    dead_code,
    reason = "wired in Task 2; the red commit holds only the signature"
)]
pub(super) fn unconfigured(
    provider: &str,
    id: &str,
    extension: &str,
    name: &str,
    env: Option<&str>,
) -> Notice {
    let message = match env {
        Some(variable) => format!(
            "The model `{provider}/{id}` needs the setting `{name}` for its base URL, \
             which has no value, and `{variable}` has none either."
        ),
        None => format!(
            "The model `{provider}/{id}` needs the setting `{name}` for its base URL, \
             which has no value."
        ),
    };
    Notice {
        code: ErrorCode::ModelUnconfigured,
        message,
        extension: Some(extension.to_owned()),
    }
}
