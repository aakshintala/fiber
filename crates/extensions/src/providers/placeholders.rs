//! Fills a per-account host placeholder in a model's `base_url`
//! (`docs/model-routing.md`, "A per-account host").

use std::collections::BTreeMap;

use config::{Config, ConfigError, Placeholder};
use contract::ErrorCode;
use contract::events::Notice;
use serde_json::Value;

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

/// A placeholder's value, read as [`Providers::fill_placeholders`] reads
/// it: the extension's own setting when present and non-empty, else the
/// environment variable `placeholders.<name>.env` names, read through
/// `env` (`docs/model-routing.md`, "A per-account host"). Nothing while
/// no value is set; an extension settings file that cannot be read is
/// `ConfigError`. A present value of any other type is never a host, so
/// it fills as `Source::Setting` with a stand-in: the value never reaches
/// a URL or a notice.
pub(super) fn value(
    config: &Config,
    extension: &str,
    declared: &BTreeMap<String, Placeholder>,
    name: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<(String, Source)>, ConfigError> {
    match config.extensions().get(extension, &[], name)? {
        Some(Value::String(value)) if !value.is_empty() => Ok(Some((value, Source::Setting))),
        // Absent, JSON null, or an empty string leaves the
        // environment fallback in play. Any other present value is the
        // setting's: it is never a host, so it fills as `Source::Setting`
        // without consulting the environment. The stand-in never reaches
        // a URL or a notice: `fill` reports it as not-a-host, and that
        // notice never repeats the value.
        Some(Value::String(_)) | Some(Value::Null) | None => {
            if let Some(variable) = declared
                .get(name)
                .and_then(|placeholder| placeholder.env.as_deref())
                && let Some(value) = env(variable)
                && !value.is_empty()
            {
                return Ok(Some((value, Source::Env(variable.to_owned()))));
            }
            Ok(None)
        }
        Some(_) => Ok(Some((" ".to_owned(), Source::Setting))),
    }
}

/// Each declared placeholder's host, read through `lookup`: `None` when a
/// declared name has no value or a value that is not a host, so `models()`
/// is not called while a placeholder the person must set has none
/// (`docs/model-routing.md`, "A per-account host").
pub(super) fn hosts(
    declared: &BTreeMap<String, Placeholder>,
    lookup: &Lookup<'_>,
) -> Result<Option<BTreeMap<String, String>>, ConfigError> {
    let mut out = BTreeMap::new();
    for name in declared.keys() {
        let Some((value, _)) = lookup(name)? else {
            return Ok(None);
        };
        let Some(filled) = host(&value) else {
            return Ok(None);
        };
        out.insert(name.clone(), filled.to_owned());
    }
    Ok(Some(out))
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

/// `value` as a host, by the one rule in [`config::host_shape`]: at most
/// one leading `https://` and one trailing `/` are stripped, and what
/// remains must be letters, digits, `.` and `-`, with an optional `:` and
/// a port from 0 to 65535. The result is a sub-slice of `value`, never a
/// copy of anything else.
pub(super) fn host(value: &str) -> Option<&str> {
    config::host_shape(value)
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
/// placeholder `name` has no value. It names the setting, the login
/// that asks for it when `login` is set, and the config file that holds
/// the answer — never the value. `login` is unset for a provider whose
/// data declares `credential_label`: its `fiber login` would store a key it
/// never reads (`docs/model-routing.md`, "A per-account host"). An absent environment fallback adds the hub-side refresh:
/// the fallback is read in the hub's environment, so a newly exported
/// variable needs `fiber hub refresh` before the list sees it. A
/// present-but-invalid value reads the same as unset: the setting or
/// variable already says something, so only the login and config file
/// can replace it.
pub(super) fn unconfigured(
    provider: &str,
    id: &str,
    extension: &str,
    name: &str,
    env: Option<&str>,
    login: bool,
) -> Notice {
    let subject = format!("The model `{provider}/{id}` needs");
    Notice {
        code: ErrorCode::ModelUnconfigured,
        message: unset_message(&subject, "its", provider, extension, name, env, login),
        extension: Some(extension.to_owned()),
    }
}

/// The message for a placeholder `name` of `provider` with no value:
/// `subject`, the setting and the base URL `its` names, the hub's
/// environment when `env` names a variable, then the remedies, as
/// [`unconfigured`] describes them.
fn unset_message(
    subject: &str,
    its: &str,
    provider: &str,
    extension: &str,
    name: &str,
    env: Option<&str>,
    login: bool,
) -> String {
    let mut message =
        format!("{subject} the setting `{name}` for {its} base URL, which has no value");
    if let Some(var) = env {
        message.push_str(&format!(
            ", and `{var}` is not set in the hub's environment (your login shell)"
        ));
    }
    let dir = config::dir_name(extension);
    if login {
        message.push_str(&format!(
            ". Run `fiber login {provider}`, or set `{name}` in `config/{dir}.json` in Fiber home"
        ));
    } else {
        message.push_str(&format!(
            ". Set `{name}` in `config/{dir}.json` in Fiber home"
        ));
    }
    if env.is_some() {
        message.push_str(", or export it in your shell profile and run `fiber hub refresh`");
    }
    message.push('.');
    message
}

/// One model left out because its placeholder has no value: its id and
/// its display name, when the provider data names one.
#[derive(Debug, Clone, PartialEq)]
pub struct UnconfiguredModel {
    /// The model's id, as the vendor spells it.
    pub id: String,
    /// Its display name, when the provider data names one.
    pub name: Option<String>,
}

/// The models of one provider left out because one placeholder has no
/// value: `fiber models` prints one notice for them all
/// (`docs/model-routing.md`, "A per-account host").
#[derive(Debug, Clone, PartialEq)]
pub struct Unconfigured {
    /// The provider's name.
    pub(super) provider: String,
    /// The extension that registered it.
    pub(super) extension: String,
    /// The placeholder with no value.
    pub(super) setting: String,
    /// The environment variable the placeholder declares, if any.
    pub(super) variable: Option<String>,
    /// Whether `fiber login <provider>` asks for the setting.
    pub(super) login: bool,
    /// The models left out, in model order.
    pub(super) models: Vec<UnconfiguredModel>,
}

impl Unconfigured {
    /// The provider's name.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// The extension that registered the provider, as it is registered.
    pub fn extension(&self) -> &str {
        &self.extension
    }

    /// The placeholder with no value.
    pub fn setting(&self) -> &str {
        &self.setting
    }

    /// Whether `fiber login <provider>` asks for the setting.
    pub fn login(&self) -> bool {
        self.login
    }

    /// Every model left out for this setting, in model order.
    pub fn models(&self) -> &[UnconfiguredModel] {
        &self.models
    }

    /// The `model_unconfigured` notice for the hidden models whose
    /// `provider/id` reference `listed` admits, counting them, with the
    /// clauses and remedies of the per-model notice; `None` when it
    /// admits none.
    pub fn notice(&self, listed: &dyn Fn(&str) -> bool) -> Option<Notice> {
        let count = self
            .models
            .iter()
            .filter(|model| listed(&format!("{}/{}", self.provider, model.id)))
            .count();
        if count == 0 {
            return None;
        }
        let provider = &self.provider;
        let (subject, its) = if count == 1 {
            (format!("`{provider}`: 1 model needs"), "its")
        } else {
            (format!("`{provider}`: {count} models need"), "their")
        };
        Some(Notice {
            code: ErrorCode::ModelUnconfigured,
            message: unset_message(
                &subject,
                its,
                provider,
                &self.extension,
                &self.setting,
                self.variable.as_deref(),
                self.login,
            ),
            extension: Some(self.extension.clone()),
        })
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
