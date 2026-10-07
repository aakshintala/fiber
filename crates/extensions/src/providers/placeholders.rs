//! Fills a per-account host placeholder in a model's `base_url`
//! (`docs/model-routing.md`, "A per-account host").

use config::ConfigError;
use contract::ErrorCode;
use contract::events::Notice;

#[cfg(test)]
#[path = "placeholders_tests.rs"]
mod tests;

/// Where a placeholder's value came from.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Source {
    /// The extension's own setting.
    Setting,
    /// The named environment variable, read when the setting is unset.
    Env(String),
}

/// A `base_url` filled, or the name of its first placeholder with no value
/// or with a value that is not a host.
#[derive(Debug, PartialEq)]
pub(super) enum Filled {
    /// The template with every placeholder replaced.
    Url(String),
    /// The first placeholder with no usable value, in template order.
    Missing(String),
    /// The first placeholder whose value is not a host, in template order.
    NotHost {
        /// The placeholder's name.
        name: String,
        /// Where its value came from.
        source: Source,
    },
}

/// What `fill` reads for a placeholder's name: its non-empty value and
/// where it came from, or nothing when it has no usable value.
pub(super) type Lookup<'a> = dyn Fn(&str) -> Result<Option<(String, Source)>, ConfigError> + 'a;

/// Whether `c` may appear in a placeholder's name.
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// `value` as a host: at most one leading `https://` and one trailing `/`
/// are stripped, and what remains must be letters, digits, `.` and `-`,
/// with an optional `:` and a port from 0 to 65535. The result is a
/// sub-slice of `value`, never a copy of anything else.
pub(super) fn host(value: &str) -> Option<&str> {
    let rest = value.strip_prefix("https://").unwrap_or(value);
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let (host, port) = match rest.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (rest, None),
    };
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return None;
    }
    if let Some(port) = port {
        if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        if port.parse::<u16>().is_err() {
            return None;
        }
    }
    Some(rest)
}

/// `template` with each `{name}` replaced by the host `lookup(name)` gives;
/// a lookup that finds no usable value stops at that name, as does one
/// whose value is not a host. The lookup returns only non-empty values.
pub(super) fn fill(template: &str, lookup: &Lookup<'_>) -> Result<Filled, ConfigError> {
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
            Some((value, source)) => match host(&value) {
                Some(filled) => out.push_str(filled),
                None => return Ok(Filled::NotHost { name, source }),
            },
            None => return Ok(Filled::Missing(name)),
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

/// The `model_unconfigured` notice for model `id` of `provider`, whose
/// placeholder `name` has a value that is not a host. The message never
/// repeats the value.
pub(super) fn not_a_host(
    provider: &str,
    id: &str,
    extension: &str,
    name: &str,
    source: &Source,
) -> Notice {
    let message = match source {
        Source::Setting => format!(
            "The model `{provider}/{id}` needs the setting `{name}` for its base URL, \
             whose value is not a host."
        ),
        Source::Env(variable) => format!(
            "The model `{provider}/{id}` needs the setting `{name}` for its base URL, \
             which has no value, and the value of `{variable}` is not a host."
        ),
    };
    Notice {
        code: ErrorCode::ModelUnconfigured,
        message,
        extension: Some(extension.to_owned()),
    }
}
