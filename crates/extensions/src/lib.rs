//! Installs extensions into Fiber home and registers what they provide
//! (`docs/extensions.md`). It holds only what a data-only provider needs:
//! installing from a local path or from git by name, reading each installed provider's data
//! through `config`, and choosing the session's model from them
//! (`docs/model-routing.md`, "Naming a model" and "Choosing the model"). It
//! also hosts the Lua runtime a Lua extension runs in (`docs/extensions.md`,
//! "Lua extensions"), and a provider's Lua: `models()`, `credential()` and
//! `sign()` (`docs/model-routing.md`, "Model discovery", "Signing a request"
//! and "Credentials").

mod commands;
mod extension_tools;
mod first_party;
mod git;
mod hooks;
mod host;
mod install;
mod installed;
mod lua;
mod lua_cost;
mod lua_provider;
mod manage;
mod oauth;
mod prepare;
mod providers;
mod release;
mod repository;
mod resolve;

use std::io;
use std::path::PathBuf;

use config::ConfigError;
use contract::ErrorCode;

pub use extension_tools::LuaTool;
pub use git::{Origin, SHORT_NAMES, full_name, is_path};
pub use hooks::SessionExtensions;
pub use host::Session;
pub use host::exec::kill_every_group;
pub use host::script::{ExecEntry, ExecReply, HostScript, HttpEntry, json_matches};
pub use install::Provenance;
pub use installed::{
    Damaged, Installed, Listing, Removal, is_enabled, list, package_names, removal,
};
pub use lua::{LuaExtension, MEMORY_CAP};
pub use lua_provider::{CredentialPair, LuaProvider, REFRESH_BEFORE};
pub use manage::{Item, Plan, Request, plan};
pub use oauth::{Browser, SystemBrowser};
pub use prepare::platform;
pub use providers::{Model, Providers, StartedRefresh, leave_out_invalid, refresh_lists};
pub use release::{Release, install_release};
pub use repository::{
    Decision, Index, Pending, RepoItem, SessionOffer, Store, declared_items, hash, kind_name,
    pending,
};

/// The extension API's major version this Fiber speaks
/// (`docs/extensions.md`, "The extension API version").
pub const API: u64 = 1;

/// A failure of the `extensions` crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading a manifest or a provider's data failed.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Copying or moving an extension's files failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The path.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// A source that holds Fiber home's `extensions/`, or lies inside it.
    #[error("{} holds Fiber home's extensions or lies inside them; install from a directory outside Fiber home.", path.display())]
    Overlaps {
        /// The source, resolved.
        path: PathBuf,
    },
    /// The manifest's `fiber` is newer than the running Fiber.
    #[error(
        "`{name}` needs Fiber {needs} or later, and this is Fiber {running}. Run `fiber upgrade`."
    )]
    NeedsNewerFiber {
        /// The extension.
        name: String,
        /// Its lowest Fiber version.
        needs: String,
        /// The running Fiber's version.
        running: String,
    },
    /// The manifest's `api` is not this Fiber's.
    #[error("`{name}` was written for extension API {api}, and this Fiber speaks API {API}.")]
    ApiVersion {
        /// The extension.
        name: String,
        /// Its `api`.
        api: u64,
    },
    /// A version that is not three numbers, such as `0.3.0` or `v1.4.0`.
    #[error("`{text}` is not a version such as `0.3.0`.")]
    BadVersion {
        /// The text.
        text: String,
    },
    /// An extension name that does not slug to one file name.
    #[error("`{name}` is not an extension name such as `github.com/acme/fiber-acme`.")]
    BadName {
        /// The name.
        name: String,
    },
    /// `git` is not on the `PATH`.
    #[error("`git` is not installed. Install git, then run the command again.")]
    GitMissing,
    /// A `git` command failed.
    #[error("`git {command}` failed: {why}")]
    Git {
        /// The arguments.
        command: String,
        /// What `git` said.
        why: String,
    },
    /// `git ls-remote` reported that the repository does not exist.
    #[error("`{name}` was not found: {why}")]
    NoRepository {
        /// The repository `git` was asked for.
        name: String,
        /// What `git` said.
        why: String,
    },
    /// Dependents that need different major versions of one extension.
    #[error("{a} `{name}` and {b} `{name}`: two major versions cannot both be installed.")]
    MajorConflict {
        /// The dependency.
        name: String,
        /// A dependent and the minimum it states, as "`x` needs 1.2".
        a: String,
        /// A dependent of another major version, likewise.
        b: String,
    },
    /// No tag meets the minimums.
    #[error("No version of `{name}` is {needs} or later.")]
    NoVersion {
        /// The dependency.
        name: String,
        /// The highest minimum asked.
        needs: String,
    },
    /// The dependencies' versions kept changing one another.
    #[error("The dependencies' versions could not be settled.")]
    Unresolved,
    /// An extension that is not installed.
    #[error("`{name}` is not installed.")]
    NotInstalled {
        /// The name.
        name: String,
    },
    /// A fetched manifest names another extension than the one asked for.
    #[error("Fetched `{asked}`, but its manifest names `{found}`.")]
    WrongName {
        /// The name asked for.
        asked: String,
        /// The name in the manifest.
        found: String,
    },
    /// Another install, update or remove holds the lock over `extensions/`.
    #[error(
        "Another `fiber extension install`, `update` or `remove` is running. Run the command again when it ends."
    )]
    Busy,
    /// Two names whose directory is the same.
    #[error(
        "`{name}` and `{other}` would both be installed at `extensions/{}`.",
        config::dir_name(name)
    )]
    SlugTaken {
        /// The name being installed.
        name: String,
        /// The name that has the directory.
        other: String,
    },
    /// A repository with no version tag.
    #[error("`{name}` has no version tag, such as `v1.0.0`. A version is a git tag.")]
    NoTag {
        /// The name.
        name: String,
    },
    /// An extension whose install record is missing or unreadable: remove
    /// it, then install it again (`docs/extensions.md`, "Installing").
    #[error("{0}")]
    Damaged(Damaged),
    /// An install record that is missing or does not read.
    #[error("{}: {why}", path.display())]
    BadRecord {
        /// The record file.
        path: PathBuf,
        /// Why.
        why: String,
    },
    /// An extension's install step could not be started.
    #[error("`{name}`: its install step failed: {why}")]
    InstallStep {
        /// The extension.
        name: String,
        /// Why.
        why: String,
    },
    /// An extension's install step exited nonzero.
    #[error("`{name}`: its install step failed: {why}")]
    InstallExited {
        /// The extension.
        name: String,
        /// Why, including what the step wrote.
        why: String,
    },
    /// A download failed: an extension's binary, or a release file.
    #[error("`{name}`: a download failed: {why}")]
    Download {
        /// The extension, or the release file.
        name: String,
        /// Why.
        why: String,
    },
    /// A binary whose SHA-256 is not the manifest's.
    #[error("`{name}`: the binary at {url} does not match the sha256 in its manifest.")]
    BinaryChecksum {
        /// The extension.
        name: String,
        /// Where it was downloaded from.
        url: String,
    },
    /// A release archive whose SHA-256 is not the one its `.sha256` file
    /// holds.
    #[error("`{archive}` from {url} does not match its .sha256 file.")]
    ArchiveChecksum {
        /// The archive's file name.
        archive: String,
        /// Where it was downloaded from.
        url: String,
    },
    /// A release archive Fiber will not unpack: not gzip, not plain ustar, a
    /// member that is not a file, a directory or a relative symlink inside
    /// its directory, or a layout that is not the release's.
    #[error("`{archive}`: {why}")]
    BadArchive {
        /// The archive's file name.
        archive: String,
        /// Why, naming the member.
        why: String,
    },
    /// A move failed and some extensions could not be put back.
    #[error("The install failed ({why}) and these could not be put back: {}", stuck.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))]
    Rollback {
        /// What failed first.
        why: String,
        /// The directories left as they are.
        stuck: Vec<PathBuf>,
    },
    /// A model reference whose provider is not installed.
    #[error(
        "The provider `{provider}` is not installed. {}",
        first_party::install_hint(provider)
    )]
    ProviderMissing {
        /// The provider.
        provider: String,
    },
    /// A bare model id no installed provider has.
    #[error("No installed model matches `{id}`. Run `fiber models` to list them.")]
    ModelMissing {
        /// The id as typed.
        id: String,
    },
    /// An installed provider that does not list the model.
    #[error(
        "The provider `{provider}` has no model `{model}`. Run `fiber models` and name one it lists."
    )]
    UnknownModel {
        /// The provider.
        provider: String,
        /// The model id.
        model: String,
    },
    /// The session's model, or one named, is left out for its per-account host.
    #[error("{message}")]
    Unconfigured {
        /// Its `model_unconfigured` message.
        message: String,
    },
    /// A bare model id that more than one installed provider has.
    #[error("The model `{id}` is offered by more than one provider: {}. Name one as `provider/model`.", matches.join(", "))]
    Ambiguous {
        /// The id as typed.
        id: String,
        /// Each match, as `provider/model`.
        matches: Vec<String>,
    },
    /// A Lua extension's code failed: a Lua error, its memory cap, or its
    /// entry script. Lua starts the message with the file and line.
    #[error("`{extension}`: {message}")]
    Lua {
        /// The extension.
        extension: String,
        /// Lua's message.
        message: String,
    },
    /// The token endpoint rejected a `host.oauth.refresh`, so the function
    /// passed to it raised after a reply.
    #[error("`{extension}`: the OAuth refresh was rejected: {message}. Log in again.")]
    RefreshRejected {
        /// The extension.
        extension: String,
        /// What the refresh function raised.
        message: String,
    },
    /// A `host.oauth.refresh` never reached the token endpoint.
    #[error("`{extension}`: the OAuth refresh could not reach the token endpoint: {message}")]
    RefreshUnreachable {
        /// The extension.
        extension: String,
        /// What the refresh function raised.
        message: String,
    },
    /// An interactive `host.oauth` helper needed a person to log in, and
    /// nobody was attached (`docs/model-routing.md`, "Keys, tokens and
    /// OAuth").
    #[error(
        "`{extension}`: host.oauth.{call} needs a person to log in, and nobody is attached. Log in again."
    )]
    Unattended {
        /// The extension.
        extension: String,
        /// `open`, `callback` or `poll`.
        call: String,
    },
    /// A callback ran past the timeout it declared, and was stopped.
    #[error("`{extension}`: `{callback}` passed its {timeout_ms} ms timeout and was stopped.")]
    Timeout {
        /// The extension.
        extension: String,
        /// The callback: a command's name, `<provider>.<function>`, or the
        /// entry script.
        callback: String,
        /// The timeout it declared.
        timeout_ms: u64,
    },
    /// A command the extension never registered.
    #[error("`{extension}` has no command `{command}`.")]
    UnknownCommand {
        /// The extension.
        extension: String,
        /// The command asked for.
        command: String,
    },
    /// A provider function the extension never registered.
    #[error("`{extension}` registered no `{callback}`.")]
    UnknownCallback {
        /// The extension.
        extension: String,
        /// `<provider>.<function>`.
        callback: String,
    },
    /// A provider function returned something other than what Fiber asked
    /// for, such as a `credential()` with no token.
    #[error("`{extension}`: `{callback}` returned {why}.")]
    BadReturn {
        /// The extension.
        extension: String,
        /// `<provider>.<function>`.
        callback: String,
        /// What was wrong.
        why: String,
    },
    /// A callback the runtime could not stop at its timeout, such as a loop
    /// in a `__gc` finalizer or a long C call. Its VM is abandoned.
    #[error(
        "`{extension}`: `{callback}` could not be stopped at its timeout, so the extension is stopped for the rest of the session."
    )]
    Abandoned {
        /// The extension.
        extension: String,
        /// The command that was called.
        callback: String,
    },
    /// The extension was stopped, or its thread is gone, and takes no more
    /// calls.
    #[error("`{extension}` is stopped and takes no more calls.")]
    Stopped {
        /// The extension.
        extension: String,
    },
    /// Neither a resumed session nor configuration chose a model.
    #[error("No model was chosen. Pass `--model provider/model`, or set `model` in configuration.")]
    NoModel,
    /// A `credential()` call failed. Only from [`LuaProvider::fetch_token`].
    #[error(transparent)]
    Credential(Box<Error>),
    /// A `repository_extensions` path that is not a package directory inside
    /// the repository.
    #[error("`repository_extensions` path `{path}` {why}.")]
    BadRepositoryPath {
        /// The path as the repository wrote it.
        path: String,
        /// What is wrong with it.
        why: &'static str,
    },
    /// Reading, hashing or copying what a repository ships failed.
    #[error("`{item}`: {why}")]
    Pin {
        /// The extension, hook or MCP server.
        item: String,
        /// Why.
        why: String,
    },
    /// A file that is not the one hashed was found when copying it.
    #[error("`{item}` changed while it was being copied. Run `fiber approve` again.")]
    ChangedWhileCopying {
        /// The extension, hook or MCP server.
        item: String,
    },
}

impl Error {
    /// The stable code a caller switches on.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Config(e) => e.code(),
            Self::Io { .. } | Self::Pin { .. } | Self::ChangedWhileCopying { .. } => {
                ErrorCode::IoFailed
            }
            Self::BadRepositoryPath { .. } => ErrorCode::ConfigInvalid,
            Self::Overlaps { .. } | Self::GitMissing | Self::SlugTaken { .. } => ErrorCode::Usage,
            Self::Git { .. } | Self::Download { .. } => ErrorCode::FetchFailed,
            Self::Busy
            | Self::BadRecord { .. }
            | Self::InstallStep { .. }
            | Self::BinaryChecksum { .. }
            | Self::ArchiveChecksum { .. }
            | Self::BadArchive { .. }
            | Self::Rollback { .. } => ErrorCode::IoFailed,
            Self::InstallExited { .. } => ErrorCode::NonzeroExit,
            Self::MajorConflict { .. } | Self::NoVersion { .. } | Self::Unresolved => {
                ErrorCode::VersionConflict
            }
            Self::NoRepository { .. } | Self::NoTag { .. } | Self::WrongName { .. } => {
                ErrorCode::ExtensionNotFound
            }
            Self::NotInstalled { .. } => ErrorCode::ExtensionMissing,
            Self::NeedsNewerFiber { .. } | Self::ApiVersion { .. } => {
                ErrorCode::ExtensionIncompatible
            }
            Self::BadVersion { .. } | Self::BadName { .. } => ErrorCode::ConfigInvalid,
            Self::ProviderMissing { .. } => ErrorCode::ExtensionMissing,
            Self::Damaged(_)
            | Self::Lua { .. }
            | Self::Timeout { .. }
            | Self::Abandoned { .. }
            | Self::UnknownCallback { .. }
            | Self::BadReturn { .. }
            | Self::Stopped { .. } => ErrorCode::ExtensionFailed,
            Self::UnknownCommand { .. } => ErrorCode::UnknownCommand,
            Self::UnknownModel { .. } | Self::ModelMissing { .. } | Self::NoModel => {
                ErrorCode::NoModel
            }
            Self::Unconfigured { .. } => ErrorCode::ModelUnconfigured,
            Self::Ambiguous { .. } => ErrorCode::ModelAmbiguous,
            Self::Credential(_) => ErrorCode::CredentialFailed,
            Self::RefreshRejected { .. } | Self::Unattended { .. } => {
                ErrorCode::AuthenticationFailed
            }
            Self::RefreshUnreachable { .. } => ErrorCode::ConnectionFailed,
        }
    }
}
