//! Every key in `docs/configuration.md`, "Keys": its type, whether a
//! repository may set it, and its built-in default. Checking a layer against
//! this table is where unknown keys become notices and wrong types become
//! errors.

use std::time::Duration;

use serde_json::{Map, Value};

use contract::ErrorCode;
use contract::events::Notice;

use crate::Source;
use crate::error::ConfigError;
use crate::path::display;
use crate::secret::CredentialSource;

/// The type a key's value must have.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Str,
    Bool,
    /// A whole number of zero or more.
    Count,
    /// A whole number below the given ceiling: `cache.warm_cap`.
    CountBelow(u64),
    Number,
    OneOf(&'static [&'static str]),
    StrList,
    /// A string, or a list of strings (`[]` included): `keys.*`.
    StrOrStrList,
    /// An object whose values are strings, such as a server's `env`.
    StrMap,
    /// An object whose values are booleans, such as MCP hints.
    BoolMap,
    Credential,
    /// A list of `{path, required}` objects: `repository_extensions`.
    RepositoryExtensions,
    /// A duration string such as `"24h"`: `model_lists.refresh_after`.
    Duration,
}

impl Kind {
    pub(crate) fn accepts(self, value: &Value) -> bool {
        match self {
            Self::Str => value.is_string(),
            Self::Bool => value.is_boolean(),
            Self::Count => value.is_u64(),
            Self::CountBelow(ceiling) => value.as_u64().is_some_and(|n| n < ceiling),
            Self::Number => value.is_number(),
            Self::OneOf(allowed) => value.as_str().is_some_and(|s| allowed.contains(&s)),
            Self::StrList => value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
            Self::StrOrStrList => {
                value.is_string()
                    || value
                        .as_array()
                        .is_some_and(|items| items.iter().all(Value::is_string))
            }
            Self::StrMap => value
                .as_object()
                .is_some_and(|map| map.values().all(Value::is_string)),
            Self::BoolMap => value
                .as_object()
                .is_some_and(|map| map.values().all(Value::is_boolean)),
            Self::Credential => match serde_json::from_value(value.clone()) {
                Ok(CredentialSource::Command(argv)) => !argv.is_empty(),
                Ok(CredentialSource::Env(_) | CredentialSource::File(_)) => true,
                Err(_) => false,
            },
            Self::RepositoryExtensions => value.as_array().is_some_and(|items| {
                items.iter().all(|item| {
                    item.as_object().is_some_and(|object| {
                        object.get("path").is_some_and(Value::is_string)
                            && object.get("required").is_none_or(Value::is_boolean)
                    })
                })
            }),
            Self::Duration => value
                .as_str()
                .is_some_and(|text| parse_duration(text).is_some()),
        }
    }

    pub(crate) fn expected(self) -> String {
        match self {
            Self::Str => "a string".into(),
            Self::Bool => "true or false".into(),
            Self::Count => "a whole number of zero or more".into(),
            Self::CountBelow(ceiling) => format!("a whole number less than {ceiling}"),
            Self::Number => "a number".into(),
            Self::OneOf(allowed) => format!("one of \"{}\"", allowed.join("\", \"")),
            Self::StrList => "a list of strings".into(),
            Self::StrOrStrList => "a string or a list of strings".into(),
            Self::StrMap => "an object of strings".into(),
            Self::BoolMap => "an object of true or false values".into(),
            Self::Credential => {
                "one of {\"env\": name}, {\"file\": path} or {\"command\": [program, args]}".into()
            }
            Self::RepositoryExtensions => {
                "a list of objects, each with a string `path` and an optional true or false `required`".into()
            }
            Self::Duration => "a duration such as \"7d\"".into(),
        }
    }
}

/// Which layers may set a key ("What a repository may set"). One value,
/// so a key can never be both repository-only and global-only.
#[derive(Clone, Copy)]
pub(crate) enum Scope {
    /// Any layer may set it.
    Any,
    /// Only a repository's own file may set it: any other layer's value is
    /// ignored with a notice.
    RepoOnly,
    /// Only Fiber home's `config.json` may set it: any other layer's value
    /// is ignored with a notice.
    GlobalOnly,
    /// Only the person's own files in Fiber home may set it: the global
    /// `config.json` or the project's `config.json` in Fiber home. Any
    /// other layer's value is ignored with a notice.
    PersonFiles,
}

/// One row of "Keys". `*` in a path stands for one name, such as a role's.
pub(crate) struct Key {
    path: &'static str,
    pub(crate) kind: Kind,
    /// Whether a repository may set it ("What a repository may set").
    pub(crate) repo: bool,
    pub(crate) scope: Scope,
    /// The built-in default, as JSON text.
    pub(crate) default: Option<&'static str>,
}

const fn key(path: &'static str, kind: Kind, repo: bool, default: Option<&'static str>) -> Key {
    Key {
        path,
        kind,
        repo,
        scope: Scope::Any,
        default,
    }
}

/// A key only a repository's own file may set.
const fn repo_only(path: &'static str, kind: Kind) -> Key {
    Key {
        path,
        kind,
        repo: true,
        scope: Scope::RepoOnly,
        default: None,
    }
}

/// A key only Fiber home's `config.json` may set.
const fn global_only(path: &'static str, kind: Kind, default: Option<&'static str>) -> Key {
    Key {
        path,
        kind,
        repo: false,
        scope: Scope::GlobalOnly,
        default,
    }
}

/// A key only the person's own files in Fiber home may set: the global
/// `config.json` or the project's `config.json` in Fiber home.
const fn person_files(path: &'static str, kind: Kind) -> Key {
    Key {
        path,
        kind,
        repo: false,
        scope: Scope::PersonFiles,
        default: None,
    }
}

use Kind::{
    Bool, BoolMap, Count, CountBelow, Credential, Duration as DurationKind, Number, OneOf,
    RepositoryExtensions, Str, StrList, StrMap, StrOrStrList,
};

const YES: bool = true;
const NO: bool = false;
const LIFETIMES: &[&str] = &["5m", "1h"];
const LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];
const DIAGNOSTIC_LEVELS: &[&str] = &["info", "debug"];

pub(crate) const KEYS: &[Key] = &[
    key(
        "model_lists.refresh_after",
        DurationKind,
        NO,
        Some("\"24h\""),
    ),
    key("model", Str, YES, None),
    key("scoped_models", StrList, YES, None),
    key("roles.*", Str, YES, None),
    key("hub.idle_exit_ms", Count, NO, Some("1800000")),
    key("hub.port", CountBelow(65536), NO, None),
    key("session.idle_exit_ms", Count, NO, Some("1800000")),
    key("reviewer.model", Str, NO, None),
    key("reviewer.block_limits.consecutive", Count, NO, Some("3")),
    key("reviewer.block_limits.session", Count, NO, Some("20")),
    key("handoff.enabled", Bool, YES, Some("true")),
    key("handoff.tokens", Count, YES, Some("400000")),
    key("handoff.window_fraction", Number, YES, Some("0.7")),
    key("handoff.nudge", Bool, YES, Some("true")),
    key("cache.lifetime", OneOf(LIFETIMES), YES, Some("\"1h\"")),
    key("cache.warm_idle", Bool, YES, Some("false")),
    key("cache.warm_cap", CountBelow(12), YES, Some("2")),
    key("thinking", OneOf(LEVELS), YES, None),
    key("models.*.handoff.enabled", Bool, YES, None),
    key("models.*.handoff.tokens", Count, YES, None),
    key("models.*.handoff.window_fraction", Number, YES, None),
    key("models.*.handoff.nudge", Bool, YES, None),
    key("models.*.cache.lifetime", OneOf(LIFETIMES), YES, None),
    key("models.*.thinking", OneOf(LEVELS), YES, None),
    key("retry.attempts", Count, YES, Some("3")),
    key("retry.initial_delay_ms", Count, YES, Some("2000")),
    key("retry.max_delay_ms", Count, YES, Some("60000")),
    key("tools.*.max_result_bytes", Count, YES, None),
    key("tools.*.deferred", Bool, YES, None),
    key("web_search.backend", Str, NO, None),
    key("shell.read_only.*.flags", StrList, NO, None),
    key("budget.usd", Number, NO, None),
    key("quota.notice_at", Number, YES, Some("80")),
    key("skills.disabled", StrList, NO, Some("[]")),
    key("mcp.servers.*.command", Str, YES, None),
    key("mcp.servers.*.args", StrList, YES, None),
    key("mcp.servers.*.env", StrMap, YES, None),
    key("mcp.servers.*.url", Str, YES, None),
    key("mcp.servers.*.required", Bool, YES, Some("false")),
    key("mcp.servers.*.startup_timeout_ms", Count, YES, Some("5000")),
    key("mcp.servers.*.timeout_ms", Count, YES, None),
    key("mcp.servers.*.declare_in_full", Bool, YES, Some("false")),
    key("mcp.servers.*.tools.enabled", StrList, YES, None),
    key("mcp.servers.*.tools.disabled", StrList, YES, None),
    key("mcp.servers.*.tools.*.hints", BoolMap, NO, None),
    repo_only("repository_extensions", RepositoryExtensions),
    key("extensions.*.enabled", Bool, NO, Some("true")),
    key("extensions.*.startup_timeout_ms", Count, YES, Some("5000")),
    key("extensions.*.commands.*", Str, YES, None),
    key("extensions.*.tools.enabled", StrList, YES, None),
    key("extensions.*.tools.disabled", StrList, YES, None),
    key("extensions.*.hook_timeout_ms", Count, NO, None),
    key("hooks.order.*", StrList, NO, None),
    key("providers.*.credential", Str, NO, None),
    key("providers.*.credentials.*", Credential, NO, None),
    key(
        "tui.panel.cards",
        StrList,
        NO,
        Some(r#"["session", "changed_files", "delegates", "jobs", "quota"]"#),
    ),
    key("tui.rail.width", Number, NO, Some("15")),
    key("tui.panel.width", Number, NO, Some("21")),
    key("tui.theme", Str, NO, None),
    key("tui.reduced_motion", Bool, NO, Some("false")),
    key("tui.screen_reader", Bool, NO, None),
    key("tui.attention.notification", Bool, NO, Some("true")),
    key("tui.attention.bell", Bool, NO, Some("true")),
    key("tui.attention.title", Bool, NO, Some("true")),
    key("tui.hover", Bool, NO, Some("true")),
    key("tui.inline_images", Bool, NO, Some("true")),
    key("tui.logo_glyph", OneOf(&["⌇", "≈"]), NO, Some("\"⌇\"")),
    key("keys.*", StrOrStrList, NO, None),
    global_only(
        "diagnostics.level",
        OneOf(DIAGNOSTIC_LEVELS),
        Some(r#""info""#),
    ),
    person_files("reviewer.context", Str),
];

/// The segments of a key's path; each `*` matches any one name.
fn segments(key: &Key) -> impl Iterator<Item = &'static str> {
    key.path.split('.')
}

fn prefix_matches(key: &Key, path: &[String]) -> bool {
    segments(key)
        .zip(path)
        .all(|(pattern, name)| pattern == "*" || pattern == name)
}

/// The key a full path names, if any.
pub(crate) fn leaf(path: &[String]) -> Option<&'static Key> {
    KEYS.iter()
        .find(|k| segments(k).count() == path.len() && prefix_matches(k, path))
}

/// Whether a path is an object that holds keys, such as `handoff`.
fn interior(path: &[String]) -> bool {
    KEYS.iter()
        .any(|k| segments(k).nth(path.len()).is_some() && prefix_matches(k, path))
}

/// The built-in defaults layer: every key whose default names no `*`.
pub(crate) fn defaults() -> Value {
    let mut root = Value::Object(Map::new());
    for k in KEYS {
        if let Some(text) = k.default
            && !k.path.contains('*')
            && let Ok(value) = serde_json::from_str(text)
        {
            let path: Vec<String> = k.path.split('.').map(String::from).collect();
            crate::path::set(&mut root, &path, value);
        }
    }
    root
}

/// Checks one layer against "Keys", returning the keys it may set. An unknown
/// key, or one a repository may not set, is a notice; a wrongly typed value
/// is an error.
pub(crate) fn check(
    layer: Map<String, Value>,
    source: &Source,
    notices: &mut Vec<Notice>,
) -> Result<Map<String, Value>, ConfigError> {
    walk(layer, &mut Vec::new(), source, notices)
}

fn walk(
    map: Map<String, Value>,
    path: &mut Vec<String>,
    source: &Source,
    notices: &mut Vec<Notice>,
) -> Result<Map<String, Value>, ConfigError> {
    let ignored = |path: &[String], why: &str| Notice {
        code: ErrorCode::ConfigKeyIgnored,
        message: format!("{source}: ignored `{}`, which {why}.", display(path)),
        extension: None,
    };
    let wrong = |path: &[String], expected: String| ConfigError::WrongType {
        source_name: source.to_string(),
        key: display(path),
        expected,
    };
    let mut kept = Map::new();
    for (name, value) in map {
        path.push(name.clone());
        if let Some(key) = leaf(path) {
            if matches!(source, Source::Repository(_)) && !key.repo {
                notices.push(ignored(path, "a repository may not set"));
            } else {
                match key.scope {
                    Scope::RepoOnly if !matches!(source, Source::Repository(_)) => {
                        notices.push(ignored(path, "only a repository's own file may set"));
                    }
                    Scope::GlobalOnly if !matches!(source, Source::Global(_)) => {
                        notices.push(ignored(path, "only Fiber home's `config.json` may set"));
                    }
                    Scope::PersonFiles
                        if !matches!(source, Source::Global(_) | Source::Project(_)) =>
                    {
                        notices.push(ignored(
                            path,
                            "only Fiber home's `config.json` or the project's `config.json` \
                             in Fiber home may set",
                        ));
                    }
                    Scope::Any | Scope::RepoOnly | Scope::GlobalOnly | Scope::PersonFiles => {
                        if key.kind.accepts(&value) {
                            kept.insert(name, value);
                        } else {
                            return Err(wrong(path, key.kind.expected()));
                        }
                    }
                }
            }
        } else if interior(path) {
            let Value::Object(inner) = value else {
                return Err(wrong(path, "an object".into()));
            };
            kept.insert(name, Value::Object(walk(inner, path, source, notices)?));
        } else {
            notices.push(ignored(path, "this Fiber does not know"));
        }
        path.pop();
    }
    Ok(kept)
}

/// How old a provider's cached model list must be before it refreshes in
/// the background (`docs/configuration.md`, `model_lists.refresh_after`).
/// Configuration validation holds every layer to the duration grammar, so
/// a value that does not parse falls back to the default.
pub fn refresh_after(config: &crate::Config) -> Duration {
    config
        .get("model_lists.refresh_after", None)
        .and_then(|(value, _)| value.as_str().and_then(parse_duration))
        .unwrap_or(Duration::from_secs(86_400))
}

/// Whether the diagnostic logs record at the `debug` level
/// (`docs/configuration.md`, `diagnostics.level`). Only Fiber home's
/// `config.json` may set it, so any other layer's value never reaches here.
pub fn diagnostics_debug(config: &crate::Config) -> bool {
    config
        .get("diagnostics.level", None)
        .is_some_and(|(value, _)| value == "debug")
}

/// Reads a duration string such as `"7d"`: a whole number of 1 or more and a
/// unit, `s`, `m`, `h` or `d` (`docs/configuration.md`,
/// `model_lists.refresh_after`). Anything else is `None`.
pub fn parse_duration(text: &str) -> Option<Duration> {
    if !text.is_ascii() {
        return None;
    }
    let (number, unit) = text.split_at(text.len().checked_sub(1)?);
    let count: u64 = number.parse().ok()?;
    if count == 0 || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let seconds = match unit {
        "s" => count,
        "m" => count.checked_mul(60)?,
        "h" => count.checked_mul(60 * 60)?,
        "d" => count.checked_mul(60 * 60 * 24)?,
        _ => return None,
    };
    Some(Duration::from_secs(seconds))
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
