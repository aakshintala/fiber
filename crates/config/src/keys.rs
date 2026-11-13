//! Every key in `docs/configuration.md`, "Keys": its type, whether a
//! repository may set it, and its built-in default. Checking a layer against
//! this table is where unknown keys become notices and wrong types become
//! errors. The one exception is `providers.databricks.request_tags`, whose
//! layers pass this check unjudged: its value is validated once, after the
//! layers merge, and an invalid effective value is a notice
//! (`docs/configuration.md`, "A provider's data").

use std::time::Duration;

use serde_json::{Map, Value};

use contract::ErrorCode;
use contract::ThinkingLevel;
use contract::events::Notice;
use contract::settings::{SettingControl, SettingsMetadata, SettingsTab};

use crate::Source;
use crate::error::ConfigError;
use crate::path::display;
use crate::secret::CredentialSource;

/// The type a key's value must have.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Str,
    /// A model reference string: `model`, `reviewer.model`. Accepts what
    /// `Str` does; the settings view picks it from the model picker.
    Model,
    /// A theme name string: `tui.theme`. Accepts what `Str` does, so a
    /// theme file's name passes; the settings view steps through the
    /// themes it can find.
    Theme,
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
    /// A model reference string, or an object with `model`, the reference:
    /// `roles.*` (`docs/configuration.md`, "Keys").
    Role,
    /// An object whose values are strings or `{ "secret": name }`
    /// references, such as a server's `env`.
    EnvMap,
    /// An object whose values are booleans, such as MCP hints.
    BoolMap,
    Credential,
    /// A list of `{path, required}` objects: `repository_extensions`.
    RepositoryExtensions,
    /// A duration string such as `"24h"`: `model_lists.refresh_after`.
    Duration,
    /// Request tags: any layer value is admitted here because the merged
    /// object is what must be an object of strings, and an invalid one is a
    /// nonfatal notice rather than a wrong-type error
    /// (`Config::databricks_request_tags`). No other kind defers.
    RequestTags,
}

impl Kind {
    pub(crate) fn accepts(self, value: &Value) -> bool {
        match self {
            Self::Str | Self::Model | Self::Theme => value.is_string(),
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
            Self::Role => {
                value.is_string()
                    || value
                        .as_object()
                        .is_some_and(|object| object.get("model").is_some_and(Value::is_string))
            }
            Self::EnvMap => value.as_object().is_some_and(|map| {
                map.values().all(|item| {
                    item.is_string()
                        || item.as_object().is_some_and(|object| {
                            object.len() == 1 && object.get("secret").is_some_and(Value::is_string)
                        })
                })
            }),
            Self::BoolMap => value
                .as_object()
                .is_some_and(|map| map.values().all(Value::is_boolean)),
            Self::Credential => match serde_json::from_value(value.clone()) {
                Ok(CredentialSource::Command { argv, .. }) => !argv.is_empty(),
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
            Self::RequestTags => true,
        }
    }

    /// How `/settings` edits a value of this type. Exhaustive, so a new
    /// kind cannot reach the view without a control.
    pub(crate) fn control(self) -> SettingControl {
        match self {
            Self::Str | Self::Duration => SettingControl::Text,
            Self::Model => SettingControl::Model,
            Self::Theme => SettingControl::Theme,
            Self::Bool => SettingControl::Boolean,
            Self::Count | Self::CountBelow(_) | Self::Number => SettingControl::Number,
            Self::OneOf(allowed) => {
                SettingControl::Fixed(allowed.iter().map(|name| (*name).to_owned()).collect())
            }
            Self::StrList => SettingControl::StringList,
            Self::RepositoryExtensions => SettingControl::RepositoryExtensions,
            Self::StrOrStrList
            | Self::Role
            | Self::EnvMap
            | Self::BoolMap
            | Self::Credential
            | Self::RequestTags => SettingControl::Json,
        }
    }

    pub(crate) fn expected(self) -> String {
        match self {
            Self::Str | Self::Model | Self::Theme => "a string".into(),
            Self::Bool => "true or false".into(),
            Self::Count => "a whole number of zero or more".into(),
            Self::CountBelow(ceiling) => format!("a whole number less than {ceiling}"),
            Self::Number => "a number".into(),
            Self::OneOf(allowed) => format!("one of \"{}\"", allowed.join("\", \"")),
            Self::StrList => "a list of strings".into(),
            Self::StrOrStrList => "a string or a list of strings".into(),
            Self::Role => "a model reference, or an object with `model`, the reference".into(),
            Self::EnvMap => "an object of strings or `{ \"secret\" }` objects".into(),
            Self::BoolMap => "an object of true or false values".into(),
            Self::Credential => {
                "one of {\"env\": name}, {\"file\": path} or {\"command\": [program, args]}, and a command may add \"timeout\": a duration such as \"2m\"".into()
            }
            Self::RepositoryExtensions => {
                "a list of objects, each with a string `path` and an optional true or false `required`".into()
            }
            Self::Duration => "a duration such as \"7d\"".into(),
            Self::RequestTags => "an object of string values".into(),
        }
    }
}

/// Which layers may set a key ("What a repository may set"). One value,
/// so a key can never be both repository-only and global-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteScope {
    /// Any layer may set it, and a repository's own file when `repo`.
    Any {
        /// Whether a repository may set it.
        repo: bool,
    },
    /// Only Fiber home's `config.json` may set it: any other layer's value
    /// is ignored with a notice.
    GlobalOnly,
    /// Only a repository's own file may set it: any other layer's value is
    /// ignored with a notice.
    RepoOnly,
    /// Only the person's own files in Fiber home may set it: the global
    /// `config.json` or the project's `config.json` in Fiber home. Any
    /// other layer's value is ignored with a notice.
    PersonFiles,
}

impl WriteScope {
    /// Whether a repository's own file may set a key of this scope.
    pub(crate) fn repo(self) -> bool {
        match self {
            Self::Any { repo } => repo,
            Self::RepoOnly => true,
            Self::GlobalOnly | Self::PersonFiles => false,
        }
    }
}

/// How a key's layers combine (`docs/configuration.md`, "Layers"). One
/// value, so a key has exactly one rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Merge {
    /// Any other value replaces the one below it; two objects merge key
    /// by key.
    Replace,
    /// Every layer's names apply, lowest first, each once.
    Union,
    /// Each entry replaces the one below it as a whole: it does not merge
    /// key by key.
    EntryReplace,
}

/// One row of "Keys". `*` in a path stands for one name, such as a role's.
pub struct Key {
    /// The key's dotted path, with `*` for one name.
    pub path: &'static str,
    /// How `/settings` presents the key.
    pub metadata: SettingsMetadata,
    pub(crate) kind: Kind,
    pub(crate) scope: WriteScope,
    pub(crate) merge: Merge,
    /// The built-in default, as JSON text.
    pub(crate) default: Option<&'static str>,
}

const fn key(
    path: &'static str,
    kind: Kind,
    repo: bool,
    default: Option<&'static str>,
    metadata: SettingsMetadata,
) -> Key {
    Key {
        path,
        metadata,
        kind,
        scope: WriteScope::Any { repo },
        merge: Merge::Replace,
        default,
    }
}

/// A key only a repository's own file may set.
const fn repo_only(path: &'static str, kind: Kind, metadata: SettingsMetadata) -> Key {
    Key {
        path,
        metadata,
        kind,
        scope: WriteScope::RepoOnly,
        merge: Merge::Replace,
        default: None,
    }
}

/// A key only Fiber home's `config.json` may set.
const fn global_only(
    path: &'static str,
    kind: Kind,
    default: Option<&'static str>,
    metadata: SettingsMetadata,
) -> Key {
    Key {
        path,
        metadata,
        kind,
        scope: WriteScope::GlobalOnly,
        merge: Merge::Replace,
        default,
    }
}

/// A key only the person's own files in Fiber home may set: the global
/// `config.json` or the project's `config.json` in Fiber home.
const fn person_files(path: &'static str, kind: Kind, metadata: SettingsMetadata) -> Key {
    Key {
        path,
        metadata,
        kind,
        scope: WriteScope::PersonFiles,
        merge: Merge::Replace,
        default: None,
    }
}

/// A key whose layers all apply: every layer's names, lowest first, each
/// once (`docs/configuration.md`, "Layers").
const fn unioned(
    path: &'static str,
    kind: Kind,
    default: Option<&'static str>,
    metadata: SettingsMetadata,
) -> Key {
    Key {
        path,
        metadata,
        kind,
        scope: WriteScope::Any { repo: false },
        merge: Merge::Union,
        default,
    }
}

/// A key whose entries each replace the one below as a whole: a
/// provider's `credentials` (`docs/configuration.md`, "Layers").
const fn entry_replaced(path: &'static str, kind: Kind, metadata: SettingsMetadata) -> Key {
    Key {
        path,
        metadata,
        kind,
        scope: WriteScope::Any { repo: false },
        merge: Merge::EntryReplace,
        default: None,
    }
}

use Kind::{
    Bool, BoolMap, Count, CountBelow, Credential, Duration as DurationKind, EnvMap, Model, Number,
    OneOf, RepositoryExtensions, RequestTags, Role, Str, StrList, StrOrStrList, Theme,
};

/// Metadata for a key `/settings` lists on `tab`.
const fn tabbed(
    tab: SettingsTab,
    label: &'static str,
    description: &'static str,
) -> SettingsMetadata {
    SettingsMetadata {
        tab: Some(tab),
        label,
        description,
    }
}

/// Metadata for a key `/settings` does not list itself: one with a `*` in
/// its path, or one a screen row covers.
const fn untabbed(label: &'static str, description: &'static str) -> SettingsMetadata {
    SettingsMetadata {
        tab: None,
        label,
        description,
    }
}

const YES: bool = true;
const NO: bool = false;
const LIFETIMES: &[&str] = &["5m", "1h"];
const DIAGNOSTIC_LEVELS: &[&str] = &["info", "debug"];

/// Every configuration key, in the order of "Keys".
pub const KEYS: &[Key] = &[
    key(
        "model_lists.refresh_after",
        DurationKind,
        NO,
        Some("\"24h\""),
        tabbed(
            SettingsTab::Models,
            "Model list refresh",
            "How old a provider's cached model list may get before Fiber refreshes it in the background, as a duration such as 7d.",
        ),
    ),
    key(
        "model_lists.models_dev",
        Bool,
        NO,
        Some("true"),
        tabbed(
            SettingsTab::Models,
            "Live model details",
            "Whether Fiber fetches models.dev to fill in model details under the model list refresh age rule.",
        ),
    ),
    key(
        "model",
        Model,
        YES,
        None,
        tabbed(
            SettingsTab::Models,
            "Default model",
            "The model a new session starts with, as provider/model.",
        ),
    ),
    key(
        "scoped_models",
        StrList,
        YES,
        None,
        tabbed(
            SettingsTab::Models,
            "Scoped models",
            "The models the model picker shows; none means every installed model.",
        ),
    ),
    key(
        "roles.*",
        Role,
        YES,
        None,
        untabbed(
            "Delegate role model",
            "A delegate role's model reference, or an object with the model and the credential label it uses.",
        ),
    ),
    key(
        "hub.idle_exit_ms",
        Count,
        NO,
        Some("1800000"),
        tabbed(
            SettingsTab::Advanced,
            "Hub idle exit",
            "Milliseconds a hub that a client started stays running with no client connected.",
        ),
    ),
    key(
        "hub.port",
        CountBelow(65536),
        NO,
        None,
        untabbed(
            "Hub port",
            "The 127.0.0.1 port an installed hub listens on besides its local socket; unset means the local socket only.",
        ),
    ),
    key(
        "hub.default",
        Str,
        NO,
        None,
        tabbed(
            SettingsTab::Advanced,
            "Default hub",
            "The name of the hub a client uses without --hub; unset means the local hub.",
        ),
    ),
    key(
        "hubs.*.address",
        Str,
        NO,
        None,
        untabbed(
            "Hub address",
            "A named hub's address: ws://, wss://, or unix: and a socket path.",
        ),
    ),
    key(
        "hub.allowed_origins",
        StrList,
        NO,
        Some("[]"),
        tabbed(
            SettingsTab::Advanced,
            "Allowed origins",
            "Web page origins whose websockets the hub accepts.",
        ),
    ),
    key(
        "session.idle_exit_ms",
        Count,
        NO,
        Some("1800000"),
        tabbed(
            SettingsTab::Advanced,
            "Session idle exit",
            "Milliseconds a session stays running with no turn, no jobs and no cache warming.",
        ),
    ),
    key(
        "session.keep_exited",
        Count,
        NO,
        Some("10"),
        tabbed(
            SettingsTab::Advanced,
            "Keep exited sessions",
            "How many exited sessions each project keeps on home before the hub archives the older ones; 0 keeps them all.",
        ),
    ),
    key(
        "reviewer.model",
        Model,
        NO,
        None,
        tabbed(
            SettingsTab::Permissions,
            "Reviewer model",
            "The reviewer's model; when set it wins over the provider's saved pick and the package default.",
        ),
    ),
    key(
        "reviewer.block_limits.consecutive",
        Count,
        NO,
        Some("3"),
        tabbed(
            SettingsTab::Permissions,
            "Consecutive blocks",
            "Consecutive reviewer blocks before a person is asked.",
        ),
    ),
    key(
        "reviewer.block_limits.session",
        Count,
        NO,
        Some("20"),
        tabbed(
            SettingsTab::Permissions,
            "Blocks per session",
            "Reviewer blocks in a session before a person is asked.",
        ),
    ),
    key(
        "compaction.enabled",
        Bool,
        YES,
        Some("true"),
        tabbed(
            SettingsTab::Sessions,
            "Automatic compaction",
            "Whether a compaction starts automatically when the context grows.",
        ),
    ),
    key(
        "compaction.tokens",
        Count,
        YES,
        Some("400000"),
        tabbed(
            SettingsTab::Sessions,
            "Compaction tokens",
            "The context size, in tokens, at which an automatic compaction starts.",
        ),
    ),
    key(
        "compaction.window_fraction",
        Number,
        YES,
        Some("0.7"),
        tabbed(
            SettingsTab::Sessions,
            "Compaction window fraction",
            "The trigger as a fraction of the model's context window, such as 0.7.",
        ),
    ),
    key(
        "compaction.nudge",
        Bool,
        YES,
        Some("true"),
        tabbed(
            SettingsTab::Sessions,
            "Compaction nudge",
            "Whether Fiber tells the model its context is filling and that a compaction keeps the work going.",
        ),
    ),
    key(
        "cache.lifetime",
        OneOf(LIFETIMES),
        YES,
        Some("\"1h\""),
        tabbed(
            SettingsTab::Advanced,
            "Cache lifetime",
            "How long the prompt cache lasts: 5m or 1h.",
        ),
    ),
    key(
        "cache.warm_idle",
        Bool,
        YES,
        Some("false"),
        tabbed(
            SettingsTab::Advanced,
            "Warm idle cache",
            "Whether an idle session keeps its prompt cache warm.",
        ),
    ),
    key(
        "cache.warm_cap",
        CountBelow(12),
        YES,
        Some("2"),
        tabbed(
            SettingsTab::Advanced,
            "Cache warming cap",
            "How many cache lifetimes after the last turn warming stops; a whole number below 12.",
        ),
    ),
    key(
        "cache.diagnostics",
        Bool,
        YES,
        Some("false"),
        tabbed(
            SettingsTab::Advanced,
            "Cache diagnostics",
            "Whether a debug model-call log line also carries redacted prefix hashes and the reported cache use.",
        ),
    ),
    key(
        "thinking",
        OneOf(&ThinkingLevel::NAMES),
        YES,
        None,
        tabbed(
            SettingsTab::Models,
            "Thinking level",
            "The thinking level a new session starts with.",
        ),
    ),
    key(
        "models.*.compaction.enabled",
        Bool,
        YES,
        None,
        untabbed(
            "Model automatic compaction",
            "Whether automatic compaction runs for one model.",
        ),
    ),
    key(
        "models.*.compaction.tokens",
        Count,
        YES,
        None,
        untabbed(
            "Model compaction tokens",
            "The context size, in tokens, at which an automatic compaction starts for one model.",
        ),
    ),
    key(
        "models.*.compaction.window_fraction",
        Number,
        YES,
        None,
        untabbed(
            "Model compaction window fraction",
            "The compaction trigger for one model, as a fraction of its context window.",
        ),
    ),
    key(
        "models.*.compaction.nudge",
        Bool,
        YES,
        None,
        untabbed(
            "Model compaction nudge",
            "Whether the compaction nudge is given for one model.",
        ),
    ),
    key(
        "models.*.cache.lifetime",
        OneOf(LIFETIMES),
        YES,
        None,
        untabbed(
            "Model cache lifetime",
            "How long the prompt cache lasts for one model: 5m or 1h.",
        ),
    ),
    key(
        "models.*.thinking",
        OneOf(&ThinkingLevel::NAMES),
        YES,
        None,
        untabbed(
            "Model thinking level",
            "The thinking level a new session starts with for one model.",
        ),
    ),
    key(
        "retry.attempts",
        Count,
        YES,
        Some("3"),
        tabbed(
            SettingsTab::Advanced,
            "Retry attempts",
            "How many times a failed model call is retried.",
        ),
    ),
    key(
        "retry.initial_delay_ms",
        Count,
        YES,
        Some("2000"),
        tabbed(
            SettingsTab::Advanced,
            "Retry first delay",
            "Milliseconds before the first retry; the delay doubles each retry.",
        ),
    ),
    key(
        "retry.max_delay_ms",
        Count,
        YES,
        Some("60000"),
        tabbed(
            SettingsTab::Advanced,
            "Retry delay cap",
            "Milliseconds that cap one retry delay, and a wait a server asks for.",
        ),
    ),
    key(
        "tools.*.max_result_bytes",
        Count,
        YES,
        None,
        untabbed(
            "Tool result cap",
            "The most bytes of one tool's result kept; the tool's own cap, or 16384, by default.",
        ),
    ),
    key(
        "tools.*.deferred",
        Bool,
        YES,
        None,
        untabbed(
            "Tool deferred",
            "Whether one tool is deferred rather than declared in full.",
        ),
    ),
    key(
        "web_search.backend",
        Str,
        NO,
        None,
        tabbed(
            SettingsTab::Models,
            "Web search backend",
            "The backend web_search uses when more than one is installed.",
        ),
    ),
    key(
        "shell.read_only.*.flags",
        StrList,
        NO,
        None,
        untabbed(
            "Read-only shell command",
            "Flags a shell command may take and still count as read-only.",
        ),
    ),
    key(
        "sandbox.enabled",
        Bool,
        NO,
        Some("false"),
        tabbed(
            SettingsTab::Permissions,
            "Sandbox",
            "Whether shell calls run inside the OS sandbox.",
        ),
    ),
    key(
        "sandbox.allow_write",
        StrList,
        NO,
        Some("[]"),
        tabbed(
            SettingsTab::Permissions,
            "Sandbox writable paths",
            "Extra paths a sandboxed call may write.",
        ),
    ),
    key(
        "sandbox.deny_read",
        StrList,
        NO,
        Some("[]"),
        tabbed(
            SettingsTab::Permissions,
            "Sandbox unreadable paths",
            "Extra paths a sandboxed call may not read.",
        ),
    ),
    key(
        "budget.usd",
        Number,
        NO,
        None,
        tabbed(
            SettingsTab::Sessions,
            "Spending budget",
            "The most a session may spend, in US dollars, its delegates included; unset means no limit.",
        ),
    ),
    key(
        "quota.notice_at",
        Number,
        YES,
        Some("80"),
        tabbed(
            SettingsTab::Notifications,
            "Quota notice",
            "The percent of a quota window used at which the model gets a notice.",
        ),
    ),
    unioned(
        "skills.disabled",
        StrList,
        Some("[]"),
        untabbed(
            "Disabled skills",
            "Names of skills switched off: left out of the listing, refused by the skill tool and not expanded by /name.",
        ),
    ),
    key(
        "mcp.servers.*.command",
        Str,
        YES,
        None,
        untabbed("MCP server command", "The program of a stdio MCP server."),
    ),
    key(
        "mcp.servers.*.args",
        StrList,
        YES,
        None,
        untabbed(
            "MCP server arguments",
            "The arguments of a stdio MCP server's program.",
        ),
    ),
    key(
        "mcp.servers.*.env",
        EnvMap,
        YES,
        None,
        untabbed(
            "MCP server environment",
            "Environment variables of a stdio MCP server; a value is a string or a stored secret.",
        ),
    ),
    key(
        "mcp.servers.*.url",
        Str,
        YES,
        None,
        untabbed("MCP server URL", "The URL of a remote MCP server."),
    ),
    key(
        "mcp.servers.*.required",
        Bool,
        YES,
        Some("false"),
        untabbed(
            "MCP server required",
            "Whether failing to start the server ends the session.",
        ),
    ),
    key(
        "mcp.servers.*.startup_timeout_ms",
        Count,
        YES,
        Some("5000"),
        untabbed(
            "MCP server startup timeout",
            "Milliseconds the server has to start.",
        ),
    ),
    key(
        "mcp.servers.*.timeout_ms",
        Count,
        YES,
        None,
        untabbed(
            "MCP server call timeout",
            "Milliseconds a call to the server may take.",
        ),
    ),
    key(
        "mcp.servers.*.declare_in_full",
        Bool,
        YES,
        Some("false"),
        untabbed(
            "MCP server declares in full",
            "Whether the server's tools are declared in full rather than deferred.",
        ),
    ),
    key(
        "mcp.servers.*.tools.enabled",
        StrList,
        YES,
        None,
        untabbed(
            "MCP server enabled tools",
            "The server's tool names to declare; others are left out.",
        ),
    ),
    key(
        "mcp.servers.*.tools.disabled",
        StrList,
        YES,
        None,
        untabbed(
            "MCP server disabled tools",
            "The server's tool names to leave out.",
        ),
    ),
    key(
        "mcp.servers.*.tools.*.hints",
        BoolMap,
        NO,
        None,
        untabbed("MCP tool hints", "Overrides of one MCP tool's hints."),
    ),
    repo_only(
        "repository_extensions",
        RepositoryExtensions,
        tabbed(
            SettingsTab::Sessions,
            "Repository extensions",
            "The extension packages the repository ships, by path inside the repository, and whether each is required.",
        ),
    ),
    key(
        "extensions.*.enabled",
        Bool,
        NO,
        Some("true"),
        untabbed(
            "Extension enabled",
            "Whether an installed extension's non-provider parts load.",
        ),
    ),
    key(
        "extensions.*.startup_timeout_ms",
        Count,
        YES,
        Some("5000"),
        untabbed(
            "Extension startup timeout",
            "Milliseconds a process extension has to start.",
        ),
    ),
    key(
        "extensions.*.commands.*",
        Str,
        YES,
        None,
        untabbed(
            "Extension command name",
            "A new name for one of the extension's commands, when two extensions clash.",
        ),
    ),
    key(
        "extensions.*.tools.enabled",
        StrList,
        YES,
        None,
        untabbed(
            "Extension enabled tools",
            "The extension's tool names to declare; others are left out.",
        ),
    ),
    key(
        "extensions.*.tools.disabled",
        StrList,
        YES,
        None,
        untabbed(
            "Extension disabled tools",
            "The extension's tool names to leave out.",
        ),
    ),
    key(
        "extensions.*.hook_timeout_ms",
        Count,
        NO,
        None,
        untabbed(
            "Extension hook timeout",
            "Milliseconds that replace the timeout of every hook and watcher the extension registers.",
        ),
    ),
    key(
        "hooks.order.*",
        StrList,
        NO,
        None,
        untabbed(
            "Hook order",
            "Extension names in the order their hooks run at one hook point.",
        ),
    ),
    key(
        "providers.*.credential",
        Str,
        NO,
        None,
        untabbed(
            "Provider credential",
            "The credential label a new session uses for one provider.",
        ),
    ),
    key(
        "providers.*.reviewer_model",
        Str,
        NO,
        None,
        untabbed(
            "Provider reviewer model",
            "One provider's saved reviewer model, written by fiber login.",
        ),
    ),
    entry_replaced(
        "providers.*.credentials.*",
        Credential,
        untabbed(
            "Provider credential source",
            "Where one credential label's key comes from, when it is not stored in Fiber home.",
        ),
    ),
    key(
        "providers.databricks.request_tags",
        RequestTags,
        NO,
        None,
        tabbed(
            SettingsTab::Models,
            "Databricks request tags",
            "Tags sent with every Databricks model request, merged with the source tag Fiber sets.",
        ),
    ),
    key(
        "tui.panel.cards",
        StrList,
        NO,
        Some(r#"["session", "changed_files", "delegates", "jobs", "quota"]"#),
        tabbed(
            SettingsTab::Appearance,
            "Panel cards",
            "The cards the panel shows, in order; a card the list does not name is left out.",
        ),
    ),
    key(
        "tui.rail.shown",
        Bool,
        NO,
        Some("true"),
        tabbed(SettingsTab::Appearance, "Rail", "Whether the rail shows."),
    ),
    key(
        "tui.rail.width",
        Number,
        NO,
        Some("15"),
        tabbed(
            SettingsTab::Appearance,
            "Rail width",
            "The rail's share of the screen's width, in percent, kept from 22 to 48 columns.",
        ),
    ),
    key(
        "tui.panel.width",
        Number,
        NO,
        Some("21"),
        tabbed(
            SettingsTab::Appearance,
            "Panel width",
            "The panel's share of the screen's width, in percent, kept from 30 to 60 columns.",
        ),
    ),
    key(
        "tui.theme",
        Theme,
        NO,
        None,
        tabbed(
            SettingsTab::Appearance,
            "Theme",
            "The theme's name: auto, dark, light, or a theme file's name.",
        ),
    ),
    key(
        "tui.slots.*",
        Str,
        NO,
        None,
        untabbed(
            "Slot owner",
            "The extension that fills a slot or key two extensions want.",
        ),
    ),
    key(
        "tui.reduced_motion",
        Bool,
        NO,
        Some("false"),
        tabbed(
            SettingsTab::Appearance,
            "Reduced motion",
            "Whether every animation takes its still form.",
        ),
    ),
    key(
        "tui.attention.notification",
        Bool,
        NO,
        Some("true"),
        tabbed(
            SettingsTab::Notifications,
            "Desktop notification",
            "Whether the terminal sends a desktop notification when a session waits on you.",
        ),
    ),
    key(
        "tui.attention.bell",
        Bool,
        NO,
        Some("true"),
        tabbed(
            SettingsTab::Notifications,
            "Bell",
            "Whether the terminal rings the bell where desktop notifications are not supported.",
        ),
    ),
    key(
        "tui.attention.title",
        Bool,
        NO,
        Some("true"),
        tabbed(
            SettingsTab::Notifications,
            "Title state",
            "Whether the terminal title shows the session's state.",
        ),
    ),
    key(
        "tui.hover",
        Bool,
        NO,
        Some("true"),
        tabbed(
            SettingsTab::Appearance,
            "Hover",
            "Whether hover highlights the click target under the pointer.",
        ),
    ),
    key(
        "tui.inline_images",
        Bool,
        NO,
        Some("true"),
        tabbed(
            SettingsTab::Appearance,
            "Inline images",
            "Whether images show inline where the terminal supports kitty's graphics protocol.",
        ),
    ),
    key(
        "tui.logo_glyph",
        OneOf(&["⌇", "≈"]),
        NO,
        Some("\"⌇\""),
        tabbed(
            SettingsTab::Appearance,
            "Logo glyph",
            "The glyph before the name in the logo, ⌇ or ≈, for a font without ⌇.",
        ),
    ),
    key(
        "keys.*",
        StrOrStrList,
        NO,
        None,
        untabbed(
            "Key binding",
            "A key, or a list of keys, for one terminal action; an empty list unbinds it.",
        ),
    ),
    global_only(
        "diagnostics.level",
        OneOf(DIAGNOSTIC_LEVELS),
        Some(r#""info""#),
        tabbed(
            SettingsTab::Advanced,
            "Diagnostics level",
            "How much the diagnostic logs record: info or debug.",
        ),
    ),
    person_files(
        "reviewer.context",
        Str,
        tabbed(
            SettingsTab::Permissions,
            "Reviewer context",
            "Notes about your environment, in prose, that the reviewer reads after its fixed instructions.",
        ),
    ),
];

/// Every row whose layers all apply, as dotted segments, in table order.
pub(crate) fn union_rows() -> impl Iterator<Item = Vec<String>> {
    KEYS.iter()
        .filter(|key| key.merge == Merge::Union)
        .map(|key| key.path.split('.').map(str::to_owned).collect())
}

/// Every key's path that names no `*`, in table order.
pub(crate) fn plain_paths() -> impl Iterator<Item = &'static str> {
    KEYS.iter()
        .map(|key| key.path)
        .filter(|path| !path.contains('*'))
}

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

/// Whether a path is an object that holds keys, such as `compaction`.
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
            if matches!(source, Source::Repository(_)) && !key.scope.repo() {
                notices.push(ignored(path, "a repository may not set"));
            } else {
                match key.scope {
                    WriteScope::RepoOnly if !matches!(source, Source::Repository(_)) => {
                        notices.push(ignored(path, "only a repository's own file may set"));
                    }
                    WriteScope::GlobalOnly if !matches!(source, Source::Global(_)) => {
                        notices.push(ignored(path, "only Fiber home's `config.json` may set"));
                    }
                    WriteScope::PersonFiles
                        if !matches!(source, Source::Global(_) | Source::Project(_)) =>
                    {
                        notices.push(ignored(
                            path,
                            "only Fiber home's `config.json` or the project's `config.json` \
                             in Fiber home may set",
                        ));
                    }
                    WriteScope::Any { .. }
                    | WriteScope::RepoOnly
                    | WriteScope::GlobalOnly
                    | WriteScope::PersonFiles => {
                        if key.kind.accepts(&value) {
                            // A repository's role cannot name a credential:
                            // the model still applies, but the credential is
                            // ignored with a notice.
                            if matches!(source, Source::Repository(_))
                                && matches!(key.kind, Kind::Role)
                                && let Value::Object(object) = &value
                                && object.contains_key("credential")
                            {
                                // The acceptance names `roles."<name>".credential`,
                                // but `path::display` quotes a segment only when it
                                // holds a dot, so the shared `ignored` closure would
                                // print `roles.<name>.credential`: this one notice
                                // hand-formats the key.
                                notices.push(Notice {
                                    code: ErrorCode::ConfigKeyIgnored,
                                    message: format!(
                                        "{source}: ignored `roles.\"{name}\".credential`, which a repository's role may not set."
                                    ),
                                    extension: None,
                                });
                            }
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

/// Whether live models.dev details are enabled; the default is true.
pub fn models_dev_enabled(config: &crate::Config) -> bool {
    config
        .get("model_lists.models_dev", None)
        .is_some_and(|(value, _)| value == true)
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
