//! The hub's commands (`docs/invocation.md`, "What the hub speaks").
//!
//! One line per command, built from [`HubCommandLine`]: the id the hub
//! answers as `command_id`, the command under `command` and its keys under
//! `args`. A command without `args` writes an empty object, as the hub
//! reads a missing `args` and `{}` the same. An absent optional key is
//! never written as `null`.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use super::SentPart;
use crate::{CommandId, Secret, SessionId};

/// One hub command line: one JSON object on the hub connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubCommandLine {
    /// Minted by the client from random bytes. Acknowledgements name the
    /// command by it, as `command_id`.
    pub id: CommandId,
    /// The command, as `command` and its `args`.
    #[serde(flatten)]
    pub command: HubCommand,
}

/// A hub command, keyed by `command`, with its keys under `args`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", content = "args", rename_all = "snake_case")]
pub enum HubCommand {
    /// Presents a device token, the first command on the port.
    Authenticate(Authenticate),
    /// Exchanges a pairing code for a device token, on the port.
    Pair(Pair),
    /// Subscribes the connection to the feed.
    Feed(NoArgs),
    /// Drops a crashed session from the feed.
    Dismiss(Dismiss),
    /// Answers with a page of exited sessions.
    Recent(Recent),
    /// Answers with the running sessions and the exited rows.
    Sessions(SessionsArgs),
    /// Starts a session in the workspace.
    Start(Start),
    /// Deletes an exited session.
    Delete(Delete),
    /// Prunes the hub's machine.
    Prune(Prune),
    /// Answers with a page of the project's prompt history.
    PromptHistory(PromptHistory),
    /// Answers with one file from the session's `artifacts/`.
    ReadFile(ReadFile),
    /// Answers with the subdirectories `path` holds.
    Directories(Directories),
    /// Searches the hub's files.
    SearchFiles(SearchFiles),
    /// Runs the provider's device-code login on the hub.
    DeviceLogin(DeviceLogin),
    /// Answers once that connection's login has stored its credential.
    DeviceLoginResult(NoArgs),
    /// Cancels that connection's login.
    DeviceLoginCancel(NoArgs),
    /// Reads or writes configuration on the hub's machine.
    Configure(Configure),
    /// Reads a configuration view's file.
    ReadConfig(ReadConfig),
    /// Writes a configuration view's file.
    WriteConfig(WriteConfig),
    /// Answers with the hub's version, clients and environment.
    Status(NoArgs),
    /// Rebuilds the hub's environment.
    Refresh(NoArgs),
    /// Ends the hub as SIGTERM does. With `sessions` true, first shuts
    /// down every running session the hub knows of, each through its own
    /// shutdown, bounded, then exits. Any authenticated connection may
    /// send it.
    Stop(StopArgs),
    /// Answers with the hub's captured values for the named placeholders.
    PlaceholderEnv(PlaceholderEnvArgs),
    /// Answers with a new pairing code for the device.
    PairingCode(Device),
    /// Answers with the paired devices.
    Devices(NoArgs),
    /// Revokes the device's token.
    Revoke(Device),
}

/// The `args` of a command without arguments: written as `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

/// The `args` of `authenticate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authenticate {
    /// The device token. It never prints: [`Secret`]'s `Debug` redacts it,
    /// and it serialises only through the private pair below, so no other
    /// type can write a secret by deriving.
    #[serde(
        serialize_with = "serialize_secret",
        deserialize_with = "deserialize_secret"
    )]
    pub token: Secret,
}

/// The `args` of `pair`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pair {
    /// The pairing code, redacted as `token` is.
    #[serde(
        serialize_with = "serialize_secret",
        deserialize_with = "deserialize_secret"
    )]
    pub code: Secret,
}

/// The `args` of `dismiss`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dismiss {
    /// The crashed session to drop from the feed.
    pub session: SessionId,
}

/// The `args` of `recent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recent {
    /// Skips to the page after this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// The project's key; absent means every project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// The `args` of `sessions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionsArgs {
    /// Keeps only that project's sessions; absent means every project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// With true, each entry carries `active`; absent or false lists
    /// without opening any session log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

/// The `args` of `start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Start {
    /// The workspace, any absolute path.
    pub workspace: String,
    /// A model reference as a person types one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Each one a `key=value` passed to the session as `-c`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides: Option<Vec<String>>,
    /// With true, the session runs in a new worktree of the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,
    /// The session whose worktree this session continues: its id. Set from `--continues`, beside `--rewound-from`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues: Option<SessionId>,
    /// With true, a program drives the session (`docs/permissions.md`,
    /// "Supervised sessions").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervised: Option<bool>,
    /// The session's first prompt, sent after the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<SentPart>>,
}

/// The `args` of `delete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delete {
    /// The exited session to delete.
    pub session: SessionId,
    /// With true, its dependents go too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cascade: Option<bool>,
    /// The sessions the person confirmed; sent only with `cascade`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Vec<SessionId>>,
    /// With true, answers the sessions it would remove and deletes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
}

/// The `args` of `prune`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prune {
    /// Deletes exited sessions older than the duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_than: Option<String>,
    /// With true, dependents of a removed session go too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cascade: Option<bool>,
    /// With true, removes without asking about anything kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
    /// With true, answers the preview and deletes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
    /// The previewed items to remove; absent reads as empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<PruneExpect>,
}

/// The `expect` of `prune`: the dry run's items, each named as the dry
/// run names it. Each list reads absent as empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PruneExpect {
    /// The previewed sessions, each with its confirmed set.
    #[serde(default)]
    pub sessions: Vec<PruneSession>,
    /// The previewed worktree directory paths.
    #[serde(default)]
    pub worktrees: Vec<String>,
    /// The previewed pinned-copy hashes.
    #[serde(default)]
    pub pinned: Vec<String>,
    /// The previewed diagnostic paths.
    #[serde(default)]
    pub diagnostics: Vec<String>,
}

/// One previewed session: its id with the full confirmed set the delete
/// removes, matching `delete`'s `expect` semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PruneSession {
    /// The session to delete.
    pub id: SessionId,
    /// The full set the delete removes.
    pub cascade: Vec<SessionId>,
}

/// The `args` of `prompt_history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptHistory {
    /// The project's key.
    pub project: String,
    /// Skips to the page before this point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<u64>,
}

/// The `args` of `read_file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadFile {
    /// The session whose `artifacts/` holds the file.
    pub session: SessionId,
    /// The path relative to the session directory.
    pub path: String,
}

/// The `args` of `directories`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Directories {
    /// The path to list, absolute or starting with `~/`.
    pub path: String,
}

/// The `args` of `search_files`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchFiles {
    /// The workspace to search, an absolute path.
    pub workspace: String,
    /// The text to search for.
    pub text: String,
    /// With true, skips the cached index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh: Option<bool>,
}

/// The `args` of `device_login`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceLogin {
    /// The device's name.
    pub name: String,
}

/// The `args` of `configure`: the operation and its own arguments. The
/// operations' arguments stay a JSON map: no caller builds them,
/// and typing each of the twenty operations with no consumer would add
/// unverified structs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Configure {
    /// The operation, such as `settings` or `store_key`.
    pub operation: String,
    /// That operation's arguments.
    #[serde(flatten)]
    pub arguments: Map<String, Value>,
}

impl fmt::Debug for Configure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The arguments of `store_key` hold a key, which is never logged
        // (`docs/invocation.md`, "What the hub speaks"): only the
        // operation prints.
        f.debug_struct("Configure")
            .field("operation", &self.operation)
            .finish()
    }
}

/// The `args` of `read_config`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadConfig {
    /// The workspace, an absolute path.
    pub workspace: String,
    /// The configuration view's file.
    pub path: String,
}

/// The `args` of `write_config`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteConfig {
    /// The workspace, an absolute path.
    pub workspace: String,
    /// The configuration view's file.
    pub path: String,
    /// The new text.
    pub text: String,
    /// The version the file still has for the write to land.
    pub version: String,
}

/// The `args` of `stop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopArgs {
    /// With true, first shuts down every running session the hub knows
    /// of, each through its own shutdown, bounded, then the hub exits.
    /// Absent means the hub exits at once. Explicit `null` is refused,
    /// like any other wrong type.
    #[serde(
        default,
        deserialize_with = "deserialize_sessions",
        skip_serializing_if = "Option::is_none"
    )]
    pub sessions: Option<bool>,
}

/// Reads `sessions`: absent means the hub exits at once, while explicit
/// `null` is refused. An ordinary `Option<bool>` would read explicit
/// `null` as absent, against `docs/invocation.md` ("What the hub
/// speaks"): unknown arguments, wrong types and explicit null are
/// `invalid_arguments`.
fn deserialize_sessions<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<bool>, D::Error> {
    struct Sessions;
    impl<'de> serde::de::Visitor<'de> for Sessions {
        type Value = Option<bool>;
        // Exempt: serde_json never calls `expecting` here. Explicit null
        // routes to `visit_unit`, and a wrong type fails inside `bool`'s
        // own deserializer, so no JSON input observes its text.
        #[cfg_attr(false, mutants::skip)]
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a boolean `sessions`")
        }
        // Exempt: serde_json routes explicit null to `visit_unit`, and a
        // missing field skips this deserializer, so `visit_none` never
        // runs: its mutants change nothing any test can see.
        #[cfg_attr(false, mutants::skip)]
        fn visit_none<E: serde::de::Error>(self) -> Result<Option<bool>, E> {
            Err(E::custom("`sessions` is null, which stop refuses"))
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Option<bool>, E> {
            Err(E::custom("`sessions` is null, which stop refuses"))
        }
        fn visit_some<D2: Deserializer<'de>>(
            self,
            deserializer: D2,
        ) -> Result<Option<bool>, D2::Error> {
            bool::deserialize(deserializer).map(Some)
        }
    }
    deserializer.deserialize_option(Sessions)
}

/// The `args` of `placeholder_env`: the placeholder variable names the
/// client wants the hub's captured values for. Absent names are omitted
/// from the answer, never written as `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaceholderEnvArgs {
    /// The placeholder variable names, each once.
    pub names: Vec<String>,
}

/// The `args` of `pairing_code` and `revoke`: one device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    /// The device's name.
    pub device: String,
}

/// Writes a secret as its string, for the one place that sends it. The
/// pair is private to this module, and [`Secret`] has no blanket
/// `Serialize` impl, so no other type can write a secret by deriving.
fn serialize_secret<S: Serializer>(secret: &Secret, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(secret.expose())
}

/// Reads a secret from its string.
fn deserialize_secret<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Secret, D::Error> {
    String::deserialize(deserializer).map(Secret::new)
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
