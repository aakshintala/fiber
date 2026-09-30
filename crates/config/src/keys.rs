//! Every key in `docs/configuration.md`, "Keys": its type, whether a
//! repository may set it, and its built-in default. Checking a layer against
//! this table is where unknown keys become notices and wrong types become
//! errors.

use serde_json::{Map, Value};

use crate::path::display;
use crate::secret::CredentialSource;

/// The type a key's value must have.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Str,
    Bool,
    /// A whole number of zero or more.
    Count,
    Number,
    OneOf(&'static [&'static str]),
    StrList,
    /// An object whose values are strings, such as a server's `env`.
    StrMap,
    /// An object whose values are booleans, such as MCP hints.
    BoolMap,
    Credential,
}

impl Kind {
    pub(crate) fn accepts(self, value: &Value) -> bool {
        match self {
            Self::Str => value.is_string(),
            Self::Bool => value.is_boolean(),
            Self::Count => value.is_u64(),
            Self::Number => value.is_number(),
            Self::OneOf(allowed) => value.as_str().is_some_and(|s| allowed.contains(&s)),
            Self::StrList => value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
            Self::StrMap => value
                .as_object()
                .is_some_and(|map| map.values().all(Value::is_string)),
            Self::BoolMap => value
                .as_object()
                .is_some_and(|map| map.values().all(Value::is_boolean)),
            Self::Credential => serde_json::from_value::<CredentialSource>(value.clone()).is_ok(),
        }
    }

    pub(crate) fn expected(self) -> String {
        match self {
            Self::Str => "a string".into(),
            Self::Bool => "true or false".into(),
            Self::Count => "a whole number of zero or more".into(),
            Self::Number => "a number".into(),
            Self::OneOf(allowed) => format!("one of \"{}\"", allowed.join("\", \"")),
            Self::StrList => "a list of strings".into(),
            Self::StrMap => "an object of strings".into(),
            Self::BoolMap => "an object of true or false values".into(),
            Self::Credential => {
                "one of {\"env\": name}, {\"file\": path} or {\"command\": [program, args]}".into()
            }
        }
    }
}

/// One row of "Keys". `*` in a path stands for one name, such as a role's.
pub(crate) struct Key {
    path: &'static str,
    pub(crate) kind: Kind,
    /// Whether a repository may set it ("What a repository may set").
    pub(crate) repo: bool,
    /// The built-in default, as JSON text.
    pub(crate) default: Option<&'static str>,
}

const fn key(path: &'static str, kind: Kind, repo: bool, default: Option<&'static str>) -> Key {
    Key {
        path,
        kind,
        repo,
        default,
    }
}

use Kind::{Bool, BoolMap, Count, Credential, Number, OneOf, Str, StrList, StrMap};

const YES: bool = true;
const NO: bool = false;
const LIFETIMES: &[&str] = &["5m", "1h"];

pub(crate) const KEYS: &[Key] = &[
    key("model", Str, YES, None),
    key("roles.*", Str, YES, None),
    key("permissions.mode", OneOf(&["auto", "yolo"]), NO, None),
    key("reviewer.model", Str, NO, None),
    key("reviewer.block_limits.consecutive", Count, NO, Some("3")),
    key("reviewer.block_limits.session", Count, NO, Some("20")),
    key("handoff.enabled", Bool, YES, Some("true")),
    key("handoff.tokens", Count, YES, Some("400000")),
    key("handoff.window_fraction", Number, YES, Some("0.7")),
    key("handoff.nudge", Bool, YES, Some("true")),
    key("cache.lifetime", OneOf(LIFETIMES), YES, Some("\"1h\"")),
    key("models.*.handoff.enabled", Bool, YES, None),
    key("models.*.handoff.tokens", Count, YES, None),
    key("models.*.handoff.window_fraction", Number, YES, None),
    key("models.*.handoff.nudge", Bool, YES, None),
    key("models.*.cache.lifetime", OneOf(LIFETIMES), YES, None),
    key("retry.attempts", Count, YES, Some("3")),
    key("retry.initial_delay_ms", Count, YES, Some("2000")),
    key("retry.max_delay_ms", Count, YES, Some("60000")),
    key("tools.*.max_result_bytes", Count, YES, None),
    key("tools.*.deferred", Bool, YES, None),
    key("web_search.backend", Str, NO, None),
    key("shell.read_only.*.flags", StrList, NO, None),
    key("quota.notice_at", Number, YES, Some("80")),
    key("mcp.servers.*.command", Str, YES, None),
    key("mcp.servers.*.args", StrList, YES, None),
    key("mcp.servers.*.env", StrMap, YES, None),
    key("mcp.servers.*.url", Str, YES, None),
    key("mcp.servers.*.required", Bool, YES, Some("false")),
    key("mcp.servers.*.startup_timeout_ms", Count, YES, Some("5000")),
    key("mcp.servers.*.timeout_ms", Count, YES, None),
    key("mcp.servers.*.eager", Bool, YES, Some("false")),
    key("mcp.servers.*.tools.enabled", StrList, YES, None),
    key("mcp.servers.*.tools.disabled", StrList, YES, None),
    key("mcp.servers.*.tools.*.hints", BoolMap, NO, None),
    key("extensions.*.version", Str, YES, None),
    key("extensions.*.startup_timeout_ms", Count, YES, Some("5000")),
    key("extensions.*.commands.*", Str, YES, None),
    key("extensions.*.tools.enabled", StrList, YES, None),
    key("extensions.*.tools.disabled", StrList, YES, None),
    key("extensions.*.hook_timeout_ms", Count, NO, None),
    key("hooks.order.*", StrList, NO, None),
    key("providers.*.credential", Credential, NO, None),
    key(
        "tui.panel.cards",
        StrList,
        NO,
        Some(r#"["session", "changed_files", "delegates", "jobs", "quota"]"#),
    ),
    key("tui.theme", Str, NO, None),
    key("tui.reduced_motion", Bool, NO, Some("false")),
    key("tui.screen_reader", Bool, NO, None),
    key("tui.attention.notification", Bool, NO, Some("true")),
    key("tui.attention.bell", Bool, NO, Some("true")),
    key("tui.attention.title", Bool, NO, Some("true")),
    key("tui.hover", Bool, NO, Some("true")),
    key("tui.inline_images", Bool, NO, Some("true")),
    key("tui.logo_glyph", OneOf(&["⌇", "≈"]), NO, Some("\"⌇\"")),
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
        .any(|k| segments(k).count() > path.len() && prefix_matches(k, path))
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

/// A key checking a layer dropped.
pub(crate) enum Ignored {
    /// Not in "Keys".
    Unknown(String),
    /// Person-only, set by a repository.
    PersonOnly(String),
}

/// A key whose value has the wrong type, which refuses the whole layer.
pub(crate) struct WrongType {
    pub(crate) key: String,
    pub(crate) expected: String,
}

/// Checks one layer against "Keys", returning the keys it may set. Unknown and
/// refused keys are pushed onto `ignored`; a wrongly typed value is an error.
pub(crate) fn check(
    layer: Map<String, Value>,
    repo: bool,
    ignored: &mut Vec<Ignored>,
) -> Result<Map<String, Value>, WrongType> {
    walk(layer, &mut Vec::new(), repo, ignored)
}

fn walk(
    map: Map<String, Value>,
    path: &mut Vec<String>,
    repo: bool,
    ignored: &mut Vec<Ignored>,
) -> Result<Map<String, Value>, WrongType> {
    let mut kept = Map::new();
    for (name, value) in map {
        path.push(name.clone());
        if let Some(key) = leaf(path) {
            if repo && !key.repo {
                ignored.push(Ignored::PersonOnly(display(path)));
            } else if key.kind.accepts(&value) {
                kept.insert(name, value);
            } else {
                return Err(WrongType {
                    key: display(path),
                    expected: key.kind.expected(),
                });
            }
        } else if interior(path) {
            let Value::Object(inner) = value else {
                return Err(WrongType {
                    key: display(path),
                    expected: "an object".into(),
                });
            };
            kept.insert(name, Value::Object(walk(inner, path, repo, ignored)?));
        } else {
            ignored.push(Ignored::Unknown(display(path)));
        }
        path.pop();
    }
    Ok(kept)
}
