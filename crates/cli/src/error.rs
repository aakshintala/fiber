//! `fiber login`, `fiber models`, `fiber mcp` and `fiber config` failures
//! (`docs/errors.md`, "Where a code comes from"). Each case maps to its
//! stable code in one exhaustive match with no wildcard arm, so a new case
//! does not compile until it has one.

use std::io;
use std::path::PathBuf;

use config::{ConfigError, Layer};
use contract::ErrorCode;
use contract::shapes::Failure;

/// Why a login, models, model-lists, mcp or config command failed: one
/// case per sentence the command prints, with no catch-all.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Loading the configuration or a credential file failed.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Building the credential file failed: only `store_locked` maps to
    /// this, so its caller keeps its own message for an invalid label.
    #[error(transparent)]
    CredentialFile(ConfigError),
    /// An installed extension or provider lookup failed.
    #[error(transparent)]
    Extensions(#[from] extensions::Error),
    /// A failure another function made, forwarded unchanged.
    #[error("{}", .0.message)]
    Forwarded(Failure),
    /// The terminal could not be read or written.
    #[error("the terminal: {0}")]
    Terminal(#[source] io::Error),
    /// Standard error could not be written.
    #[error("standard error: {0}")]
    StandardError(#[source] io::Error),
    /// Standard input could not be read.
    #[error("standard input: {0}")]
    StandardInput(#[source] io::Error),
    /// The current directory could not be read.
    #[error("the current directory: {0}")]
    CurrentDirectory(#[source] io::Error),
    /// The interrupt handler could not be installed.
    #[error("the interrupt handler: {0}")]
    InterruptHandler(#[source] io::Error),
    /// A model row could not be encoded as JSON.
    #[error("a model row: {0}")]
    ModelRow(#[source] serde_json::Error),
    /// A configuration value could not be encoded as JSON.
    #[error("a value for `{key}`: {source}")]
    ConfigValue {
        /// The key being printed.
        key: String,
        /// Why encoding failed.
        source: serde_json::Error,
    },
    /// Another login for the provider is running under its label lock.
    #[error("another login for {name} is running")]
    LoginRunning {
        /// The provider being logged in to.
        name: String,
    },
    /// Another `fiber mcp add` of the server is running under its lock.
    #[error("another `fiber mcp add` of `{name}` is running")]
    McpAddRunning {
        /// The server being added.
        name: String,
    },
    /// The repository approval could not be recorded: the lock file, the
    /// lock, or the server item failed first.
    #[error("could not record the approval of `{name}` in {}: {why}. Run `fiber approve` to finish it.", file.display())]
    ApprovalNotRecorded {
        /// The server just declared.
        name: String,
        /// The repository's configuration file.
        file: PathBuf,
        /// Why recording failed.
        why: String,
    },
    /// The repository approval store call failed.
    #[error("could not record the approval of `{name}` in {}: {source}. Run `fiber approve` to finish it.", file.display())]
    ApprovalNotStored {
        /// The server just declared.
        name: String,
        /// The repository's configuration file.
        file: PathBuf,
        /// Why the store failed.
        source: extensions::Error,
    },
    /// The declaration write failed after its rename, so the new file is
    /// in place and the stored secrets were kept.
    #[error("{0} The secrets the declaration refers to were kept.")]
    SecretsKept(#[source] ConfigError),
    /// The repository declaration changed after it was written.
    #[error(
        "`{name}` changed after it was written; the approval was not recorded. Run `fiber approve` to approve the current declaration."
    )]
    ApprovalChanged {
        /// The server just declared.
        name: String,
    },
    /// The server's url carries a user name or password.
    #[error("`{server}`'s url carries a user name or password; remove it.")]
    UrlHasUserinfo {
        /// The server being added or logged in to.
        server: String,
    },
    /// The remote login refused with a configuration failure.
    #[error("`{server}`: {message}")]
    McpLoginRefused {
        /// The server being logged in to.
        server: String,
        /// The remote refusal's message.
        message: String,
    },
    /// The repository declares the server but nobody approved it.
    #[error(
        "`{server}` is declared by the repository but not approved; run `fiber approve` in the repository."
    )]
    McpUnapproved {
        /// The server being logged in to.
        server: String,
    },
    /// The reviewer key cannot name a provider reviewer.
    #[error("`{key}` cannot name a provider reviewer: the name is not one path segment.")]
    ReviewerNameUnrepresentable {
        /// The dotted reviewer key.
        key: String,
    },
    /// Saving the reviewer choice failed.
    #[error("could not save `{key}`: {source}")]
    ReviewerNotSaved {
        /// The dotted reviewer key.
        key: String,
        /// Why the save failed.
        source: ConfigError,
    },
    /// The login was cancelled; nothing was stored.
    #[error("the login was cancelled; nothing was stored.")]
    LoginCancelled,
    /// The provider's key comes from elsewhere, so logout cannot remove it.
    #[error("{name}'s key comes from {origin}; fiber logout cannot remove it")]
    KeyFromElsewhere {
        /// The provider being logged out of.
        name: String,
        /// Where the key comes from, described without its secret.
        origin: String,
    },
    /// No stored credential for the provider.
    #[error("no stored credential for {name}")]
    NoStoredCredential {
        /// The provider being logged out of.
        name: String,
    },
    /// No stored credential under the label.
    #[error("no stored credential {label} for {name}")]
    NoStoredLabel {
        /// The provider being logged out of.
        name: String,
        /// The label asked for.
        label: String,
    },
    /// No stored credential under the label; others are stored.
    #[error("no stored credential {label} for {name}; the stored labels are {}", labels.join(", "))]
    LabelNotAmong {
        /// The provider being logged out of.
        name: String,
        /// The label asked for.
        label: String,
        /// The labels stored.
        labels: Vec<String>,
    },
    /// No installed model matches what was typed.
    #[error("{}", model_none_message(.typed, .closest))]
    ModelNone {
        /// What was typed.
        typed: String,
        /// The closest installed references, nearest first.
        closest: Vec<String>,
    },
    /// A bare model id matches models of two or more providers.
    #[error("{0}")]
    ModelAmbiguous(#[source] extensions::Error),
    /// `fiber logout` was given no provider.
    #[error("`fiber logout` takes the provider to log out of. Run `fiber --help` for usage.")]
    LogoutNeedsProvider,
    /// The provider named is not installed.
    #[error("{}", provider_not_installed_message(.name, .installed))]
    ProviderNotInstalled {
        /// The provider named.
        name: String,
        /// The installed providers.
        installed: Vec<String>,
    },
    /// `fiber login` needs a name when there is no terminal to ask on.
    #[error(
        "`fiber login` takes a provider or a secret's name when there is no terminal to ask on. Run `fiber --help` for usage."
    )]
    LoginNeedsName,
    /// Nothing is installed to log in to.
    #[error(
        "No provider is installed, and no installed extension declares a secret. Run `fiber --help` for usage."
    )]
    NothingToLogInTo,
    /// Nothing was chosen from the login menu.
    #[error("Nothing was chosen. Run `fiber --help` for usage.")]
    NothingChosen,
    /// The login menu answer names nothing installed.
    #[error(
        "`{answer}` is neither a listed number nor an installed provider or declared secret. Run `fiber --help` for usage."
    )]
    ChoiceUnknown {
        /// The answer given.
        answer: String,
    },
    /// `--device` was given for a provider that logs in with a key.
    #[error(
        "--device applies only to a provider that logs in by browser. Run `fiber --help` for usage."
    )]
    DeviceNeedsBrowserProvider,
    /// The provider has several stored credentials and none was named.
    #[error("`{name}` has several stored credentials: {}; name one with --as <label>, or use --all. Run `fiber --help` for usage.", labels.join(", "))]
    LogoutSeveral {
        /// The provider being logged out of.
        name: String,
        /// The labels stored.
        labels: Vec<String>,
    },
    /// The label cannot be stored by `fiber login`.
    #[error(
        "`{label}` is not a label `fiber login` can store; pass one with `--as <label>`. Run `fiber --help` for usage."
    )]
    LabelNotStorable {
        /// The label the login revealed.
        label: String,
    },
    /// A usage refusal: a store that never started.
    #[error("{}", store_refused_message(.refusal, *.hint))]
    StoreRefused {
        /// Why the store refused.
        refusal: Refusal,
        /// Whether the CLI hint follows the sentence.
        hint: bool,
    },
    /// The key is not a configuration key.
    #[error("`{key}` is not a configuration key. Run `fiber --help` for usage.")]
    ConfigKeyUnknown {
        /// The key as typed.
        key: String,
    },
    /// The mcp server name is not a server name.
    #[error(
        "`{name}` is not a server name: it is non-empty and holds no `\"`, `/`, whitespace or control character. Run `fiber --help` for usage."
    )]
    McpName {
        /// The name as typed.
        name: String,
    },
    /// The env variable name is not a variable name.
    #[error(
        "`{key}` is not a variable name: it matches `[A-Za-z_][A-Za-z0-9_]*`. Run `fiber --help` for usage."
    )]
    McpEnvKey {
        /// The key as typed.
        key: String,
    },
    /// The `-e` value is not `KEY=value`.
    #[error("`{raw}` is not `KEY=value`. Run `fiber --help` for usage.")]
    McpEnvPair {
        /// The value as typed.
        raw: String,
    },
    /// Both a url and a command were given.
    #[error(
        "`fiber mcp add` takes either a url or `-- <command>`, not both. Run `fiber --help` for usage."
    )]
    McpUrlAndCommand,
    /// Neither a url nor a command was given.
    #[error("`fiber mcp add` takes a url or `-- <command>`. Run `fiber --help` for usage.")]
    McpTransportMissing,
    /// `-e` or `--secret` was given with a url.
    #[error(
        "`-e` and `--secret` apply only to `-- <command>`, not to a url. Run `fiber --help` for usage."
    )]
    McpUrlWithEnv,
    /// No program follows `--`.
    #[error("`fiber mcp add` takes a program after `--`. Run `fiber --help` for usage.")]
    McpProgramMissing,
    /// A variable is declared more than once.
    #[error(
        "`{key}` is given more than once; each variable is declared once. Run `fiber --help` for usage."
    )]
    McpKeyRepeated {
        /// The repeated key.
        key: String,
    },
    /// No value was given for `--secret`.
    #[error(
        "No value was given for `--secret {key}`; nothing was stored. Run `fiber --help` for usage."
    )]
    McpSecretEmpty {
        /// The secret key.
        key: String,
    },
    /// The server is already declared in the layer.
    #[error("`{name}` is already declared in the {} layer. Run `fiber mcp remove {name}` to remove it first. Run `fiber --help` for usage.", layer_word(*.layer))]
    McpAlreadyDeclared {
        /// The server named.
        name: String,
        /// The layer it is declared in.
        layer: Layer,
    },
    /// The server is not declared in the layer.
    #[error("`{name}` is not declared in the {} layer{}. Run `fiber --help` for usage.", layer_word(*.layer), mcp_not_declared_hint(.declared_in))]
    McpNotDeclared {
        /// The server named.
        name: String,
        /// The layer asked about.
        layer: Layer,
        /// The layers that declare it, lowest first.
        declared_in: Vec<Layer>,
    },
    /// No server with the name is declared.
    #[error("no server named `{server}` is declared. Run `fiber --help` for usage.")]
    McpServerUnknown {
        /// The server named.
        server: String,
    },
    /// The server declares a command, not a url.
    #[error(
        "`{server}` declares a command, not a url; only a remote server holds a token. Run `fiber --help` for usage."
    )]
    McpServerStdio {
        /// The server named.
        server: String,
    },
}

/// Why a store refused before anything was read or written: a usage
/// refusal, with no CLI hint of its own.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// No key was given; nothing was stored.
    #[error("No key was given; nothing was stored.")]
    NoKeyGiven,
    /// No value was given; nothing was stored.
    #[error("No value was given; nothing was stored.")]
    NoValueGiven,
    /// `--as` was given for a declared secret.
    #[error("--as applies only to a provider, and `{name}` is a declared secret.")]
    AsOnlyForProvider {
        /// The secret named.
        name: String,
    },
    /// The name is neither an installed provider nor a declared secret.
    #[error("{}", unknown_name_message(.name, .providers, .secrets))]
    UnknownName {
        /// The name as typed.
        name: String,
        /// The installed providers.
        providers: Vec<String>,
        /// The declared secrets.
        secrets: Vec<String>,
    },
    /// The label is already stored.
    #[error(
        "credentials/{stored}/{label} is already stored; log in under another label with --as <label>, or run `fiber logout {name} --as {label}` first."
    )]
    AlreadyStored {
        /// The provider being logged in to.
        name: String,
        /// The credential directory the provider reads.
        stored: String,
        /// The label already stored.
        label: String,
    },
}

/// The layer in words, as usage failures name it.
pub(crate) fn layer_word(layer: Layer) -> &'static str {
    match layer {
        Layer::Global => "global",
        Layer::Project => "project",
        Layer::Repository => "repository",
    }
}

/// The `ModelNone` sentence: what was typed, the closest installed
/// references, and where to list them.
fn model_none_message(typed: &str, closest: &[String]) -> String {
    let mut message = format!("No installed model matches `{typed}`.");
    if !closest.is_empty() {
        let listed = closest
            .iter()
            .map(|reference| format!("`{reference}`"))
            .collect::<Vec<_>>()
            .join(", ");
        message.push_str(&format!(" Closest: {listed}."));
    }
    message.push_str(" Run `fiber models` to list them.");
    message
}

/// The `ProviderNotInstalled` sentence: the empty and non-empty branches.
fn provider_not_installed_message(name: &str, installed: &[String]) -> String {
    let base = if installed.is_empty() {
        format!("`{name}` is not an installed provider, and none is installed.")
    } else {
        format!(
            "`{name}` is not an installed provider; the installed providers are {}.",
            installed.join(", ")
        )
    };
    format!("{base} Run `fiber --help` for usage.")
}

/// The `UnknownName` refusal sentence, with its two empty-list branches.
fn unknown_name_message(name: &str, providers: &[String], secrets: &[String]) -> String {
    let names = if providers.is_empty() {
        "no provider is installed".to_owned()
    } else {
        format!("the installed providers are {}", providers.join(", "))
    };
    let secrets = if secrets.is_empty() {
        "no installed extension declares a secret".to_owned()
    } else {
        format!("the declared secrets are {}", secrets.join(", "))
    };
    format!(
        "`{name}` is neither an installed provider nor a declared secret; {names}, and {secrets}."
    )
}

/// The `McpNotDeclared` hint: no layer declares it, or the layers that do.
fn mcp_not_declared_hint(declared_in: &[Layer]) -> String {
    if declared_in.is_empty() {
        "; no layer declares it".to_owned()
    } else {
        let names = declared_in
            .iter()
            .map(|layer| layer_word(*layer))
            .collect::<Vec<_>>()
            .join(", ");
        format!("; it is declared in the {names} layer(s)")
    }
}

/// The `StoreRefused` sentence: the refusal plus the CLI hint when set.
fn store_refused_message(refusal: &Refusal, hint: bool) -> String {
    let base = refusal.to_string();
    if hint {
        format!("{base} Run `fiber --help` for usage.")
    } else {
        base
    }
}

impl CliError {
    /// The stable code a caller switches on.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Config(error) => error.code(),
            Self::CredentialFile(error) => error.code(),
            Self::Extensions(error) => error.code(),
            Self::Forwarded(failure) => failure.code.clone(),
            Self::Terminal(_)
            | Self::StandardError(_)
            | Self::StandardInput(_)
            | Self::CurrentDirectory(_)
            | Self::InterruptHandler(_)
            | Self::ModelRow(_)
            | Self::ConfigValue { .. }
            | Self::LoginRunning { .. }
            | Self::McpAddRunning { .. }
            | Self::ApprovalNotRecorded { .. } => ErrorCode::IoFailed,
            Self::ApprovalNotStored { source, .. } => source.code(),
            Self::SecretsKept(error) => error.code(),
            Self::ApprovalChanged { .. }
            | Self::UrlHasUserinfo { .. }
            | Self::McpLoginRefused { .. }
            | Self::ReviewerNameUnrepresentable { .. } => ErrorCode::ConfigInvalid,
            Self::ReviewerNotSaved { source, .. } => source.code(),
            Self::LoginCancelled => ErrorCode::AuthenticationFailed,
            Self::McpUnapproved { .. } => ErrorCode::McpServerUnapproved,
            Self::KeyFromElsewhere { .. }
            | Self::NoStoredCredential { .. }
            | Self::NoStoredLabel { .. }
            | Self::LabelNotAmong { .. } => ErrorCode::CredentialMissing,
            Self::ModelNone { .. } => ErrorCode::NoModel,
            Self::ModelAmbiguous(_) => ErrorCode::ModelAmbiguous,
            Self::LogoutNeedsProvider
            | Self::ProviderNotInstalled { .. }
            | Self::LoginNeedsName
            | Self::NothingToLogInTo
            | Self::NothingChosen
            | Self::ChoiceUnknown { .. }
            | Self::DeviceNeedsBrowserProvider
            | Self::LogoutSeveral { .. }
            | Self::LabelNotStorable { .. }
            | Self::StoreRefused { .. }
            | Self::ConfigKeyUnknown { .. }
            | Self::McpName { .. }
            | Self::McpEnvKey { .. }
            | Self::McpEnvPair { .. }
            | Self::McpUrlAndCommand
            | Self::McpTransportMissing
            | Self::McpUrlWithEnv
            | Self::McpProgramMissing
            | Self::McpKeyRepeated { .. }
            | Self::McpSecretEmpty { .. }
            | Self::McpAlreadyDeclared { .. }
            | Self::McpNotDeclared { .. }
            | Self::McpServerUnknown { .. }
            | Self::McpServerStdio { .. } => ErrorCode::Usage,
        }
    }
}

impl From<CliError> for Failure {
    fn from(error: CliError) -> Self {
        match error {
            CliError::Forwarded(failure) => failure,
            other @ (CliError::Config(_)
            | CliError::CredentialFile(_)
            | CliError::Extensions(_)
            | CliError::Terminal(_)
            | CliError::StandardError(_)
            | CliError::StandardInput(_)
            | CliError::CurrentDirectory(_)
            | CliError::InterruptHandler(_)
            | CliError::ModelRow(_)
            | CliError::ConfigValue { .. }
            | CliError::LoginRunning { .. }
            | CliError::McpAddRunning { .. }
            | CliError::ApprovalNotRecorded { .. }
            | CliError::ApprovalNotStored { .. }
            | CliError::SecretsKept(_)
            | CliError::ApprovalChanged { .. }
            | CliError::UrlHasUserinfo { .. }
            | CliError::McpLoginRefused { .. }
            | CliError::McpUnapproved { .. }
            | CliError::ReviewerNameUnrepresentable { .. }
            | CliError::ReviewerNotSaved { .. }
            | CliError::LoginCancelled
            | CliError::KeyFromElsewhere { .. }
            | CliError::NoStoredCredential { .. }
            | CliError::NoStoredLabel { .. }
            | CliError::LabelNotAmong { .. }
            | CliError::ModelNone { .. }
            | CliError::ModelAmbiguous(_)
            | CliError::LogoutNeedsProvider
            | CliError::ProviderNotInstalled { .. }
            | CliError::LoginNeedsName
            | CliError::NothingToLogInTo
            | CliError::NothingChosen
            | CliError::ChoiceUnknown { .. }
            | CliError::DeviceNeedsBrowserProvider
            | CliError::LogoutSeveral { .. }
            | CliError::LabelNotStorable { .. }
            | CliError::StoreRefused { .. }
            | CliError::ConfigKeyUnknown { .. }
            | CliError::McpName { .. }
            | CliError::McpEnvKey { .. }
            | CliError::McpEnvPair { .. }
            | CliError::McpUrlAndCommand
            | CliError::McpTransportMissing
            | CliError::McpUrlWithEnv
            | CliError::McpProgramMissing
            | CliError::McpKeyRepeated { .. }
            | CliError::McpSecretEmpty { .. }
            | CliError::McpAlreadyDeclared { .. }
            | CliError::McpNotDeclared { .. }
            | CliError::McpServerUnknown { .. }
            | CliError::McpServerStdio { .. }) => Failure {
                code: other.code(),
                message: other.to_string(),
                retry_after_ms: None,
                provider: None,
            },
        }
    }
}

impl From<&CliError> for Failure {
    fn from(error: &CliError) -> Self {
        Failure {
            code: error.code(),
            message: error.to_string(),
            retry_after_ms: None,
            provider: None,
        }
    }
}

impl From<Failure> for CliError {
    fn from(failure: Failure) -> Self {
        Self::Forwarded(failure)
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
