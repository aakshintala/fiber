//! Fills a per-account host placeholder in a model's `base_url`
//! (`docs/model-routing.md`, "A per-account host").

use config::ConfigError;
use contract::ErrorCode;
use contract::events::Notice;

#[cfg(test)]
#[path = "placeholders_tests.rs"]
mod tests;

/// A `base_url` filled, or the name of its first placeholder with no value.
#[derive(Debug, PartialEq)]
pub(super) enum Filled {
    /// The template with every placeholder replaced.
    Url(String),
    /// The first placeholder with no usable value, in template order.
    Missing(String),
}

/// Whether `c` may appear in a placeholder's name.
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// `template` with each `{name}` replaced by `lookup(name)`; a lookup that
/// finds no usable value stops at that name.
pub(super) fn fill(
    template: &str,
    lookup: &dyn Fn(&str) -> Result<Option<serde_json::Value>, ConfigError>,
) -> Result<Filled, ConfigError> {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '{' {
            out.push(c);
            continue;
        }
        let mut name = String::new();
        while let Some(&d) = chars.peek() {
            if is_name_char(d) {
                name.push(d);
                chars.next();
            } else {
                break;
            }
        }
        if name.is_empty() {
            out.push('{');
            continue;
        }
        if !chars.peek().is_some_and(|d| *d == '}') {
            out.push('{');
            out.push_str(&name);
            continue;
        }
        chars.next();
        match lookup(&name)? {
            Some(serde_json::Value::String(value)) if !value.is_empty() => {
                out.push_str(&value);
            }
            _ => return Ok(Filled::Missing(name)),
        }
    }
    Ok(Filled::Url(out))
}

/// The `model_unconfigured` notice for model `id` of `provider`, whose
/// placeholder `name` has no value.
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
