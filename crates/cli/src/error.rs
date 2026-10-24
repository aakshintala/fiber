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
    /// A host answer that is not a host: refused, nothing written.
    #[error("`{name}` is not a host; pass a host or press Ctrl-D to skip.")]
    HostNotHost {
        /// The setting the answer was for, never the answer.
        name: String,
    },
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
    /// Nobody can answer the approval prompt: no terminal and no input.
    #[error("{message}")]
    NobodyToAsk {
        /// The calling command's own sentence, naming how to answer without asking.
        message: String,
    },
    /// Approving one declared item failed.
    #[error("could not approve {kind} {name}: {source}")]
    ApproveFailed {
        /// The item's kind, as the offer names it.
        kind: &'static str,
        /// The item's name.
        name: String,
        /// Why the approval failed.
        source: extensions::Error,
    },
    /// The release to install is not this binary's own version.
    #[error(
        "This is Fiber {fiber_version}; it cannot install the docs and extensions of {version}."
    )]
    ReleaseVersionMismatch {
        /// This binary's own version.
        fiber_version: String,
        /// The version asked for.
        version: String,
    },
    /// This build records no commit, so it cannot install a release's extensions.
    #[error("This build records no commit, so it cannot install a release's extensions.")]
    ReleaseNoCommit,
    /// The binary directory cannot be written, so the staged binary
    /// cannot be renamed into place.
    #[error("update could not write to the binary directory {dir}: {source}", dir = .dir.display())]
    UpdateBinaryDirectory {
        /// The directory holding the running binary.
        dir: PathBuf,
        /// Why the probe write failed.
        source: io::Error,
    },
    /// The staged binary could not be renamed over the running one.
    #[error("update could not rename {} to {}: {source}", .staged.display(), .binary.display())]
    UpdateBinaryRename {
        /// The staged binary.
        staged: PathBuf,
        /// The running binary it replaces.
        binary: PathBuf,
        /// Why the rename failed.
        source: io::Error,
    },
    /// The working directory could not be read for an extension command.
    #[error("{0}")]
    ExtensionDirectory(#[source] io::Error),
    /// The fiber binary for an extension test could not be described.
    #[error("{0}")]
    FiberExecutable(String),
    /// The doctor report could not be written.
    #[error("doctor could not write its report: {0}")]
    DoctorReport(#[source] io::Error),
    /// The working directory could not be read for `fiber doctor`.
    #[error("doctor could not read the current directory: {0}")]
    DoctorDirectory(#[source] io::Error),
    /// No first-party package serves the provider.
    #[error("No first-party package serves the provider `{provider}`.")]
    NoFirstPartyPackage {
        /// The provider asked for.
        provider: String,
    },
    /// `--older-than` does not parse as a duration.
    #[error(
        "`--older-than` takes a duration such as `30d`: a whole number and `s`, `m`, `h` or `d`."
    )]
    OlderThanUnreadable,
    /// Some deletions failed: how many of how many, with the first failure's code.
    #[error("{failed} of {total} could not be deleted")]
    PruneFailed {
        /// The first failure's code.
        code: ErrorCode,
        /// How many deletions failed.
        failed: usize,
        /// How many deletions were attempted.
        total: usize,
    },
    /// A session lookup, resolution or export failed.
    #[error(transparent)]
    Log(#[from] log::Error),
    /// Reaching the hub failed: a local failure names the hub, any
    /// other failure keeps the doors text.
    #[error("{}", crate::error::hub_message(.0))]
    Hub(#[from] doors::Error),
    /// The hub closed the connection before answering the command.
    #[error("the hub closed the connection before answering `{command}`")]
    HubClosed {
        /// The command the hub never answered.
        command: String,
    },
    /// The hub refused the command, with the wire code and message.
    #[error("{message}")]
    Rejected {
        /// The wire code, or the fallback when it has none usable.
        code: ErrorCode,
        /// The wire message, or the refusal sentence naming the command.
        message: String,
    },
    /// A hub command could not be written as JSON.
    #[error("a command could not be written as JSON: {0}")]
    CommandEncode(#[source] serde_json::Error),
    /// The hub's answer to `sessions` did not parse.
    #[error("the hub's answer to `sessions`: {0}")]
    SessionsAnswer(#[source] serde_json::Error),
    /// A session row could not be encoded as JSON.
    #[error("a session row: {0}")]
    SessionRow(#[source] serde_json::Error),
    /// A search hit could not be encoded as JSON.
    #[error("a search hit: {0}")]
    SearchHit(#[source] serde_json::Error),
    /// A delete with nobody to ask.
    #[error("{}", crate::sessions::DELETE_NOBODY)]
    DeleteNobody,
    /// No session on the hub answers the selector.
    #[error("no session matches \"{selector}\" on hub `{hub}`")]
    SessionNotFoundOnHub {
        /// The selector that matched nothing.
        selector: String,
        /// The hub asked.
        hub: String,
    },
    /// The running binary's path could not be read.
    #[error("the running binary's path: {0}")]
    CurrentExe(#[source] io::Error),
    /// launchd still had the service loaded after `bootout` and the wait.
    #[error("launchd still had {name} loaded 10 s after `launchctl bootout`.")]
    LaunchdStillLoaded {
        /// The service's name.
        name: String,
    },
    /// The service manager's program is not installed.
    #[error("`{program}` was not found; an installed hub needs {}.", manager_needs(.program))]
    ManagerNotFound {
        /// The program that was not found.
        program: String,
    },
    /// The service manager's program could not run.
    #[error("running `{program}`: {source}")]
    ManagerRun {
        /// The program that did not run.
        program: String,
        /// Why it did not run.
        source: io::Error,
    },
    /// The service manager's program ran and failed.
    #[error("{}", manager_failed_message(.program, .args, *.exit, .said.as_deref()))]
    ManagerFailed {
        /// The program that failed.
        program: String,
        /// The arguments it ran with.
        args: Vec<String>,
        /// Its exit code, none when a signal ended it.
        exit: Option<i32>,
        /// The first line of its stderr, none when that is blank.
        said: Option<String>,
    },
    /// The unit file could not be read.
    #[error("reading {}: {source}", unit.display())]
    UnitReadFailed {
        /// The unit file.
        unit: PathBuf,
        /// Why the read failed.
        source: io::Error,
    },
    /// The unit file could not be written.
    #[error("{0}")]
    UnitWriteFailed(#[source] ConfigError),
    /// The unit file could not be deleted.
    #[error("deleting {}: {source}", unit.display())]
    UnitDeleteFailed {
        /// The unit file.
        unit: PathBuf,
        /// Why the delete failed.
        source: io::Error,
    },
    /// An install step failed and the unit file could not be put back.
    #[error("{cause} The unit file {} could not be put back ({lost}); run fiber hub uninstall.", unit.display())]
    UnitNotRestored {
        /// The step that failed.
        cause: Box<CliError>,
        /// The unit file.
        unit: PathBuf,
        /// Why the restore failed.
        lost: Box<CliError>,
    },
    /// An install step failed and the unit file was put back.
    #[error("{cause} {}", RERUN)]
    RerunInstall {
        /// The step that failed.
        cause: Box<CliError>,
    },
    /// The stop command could not be written as JSON.
    #[error("the hub: {0}")]
    HubRequestEncode(#[source] serde_json::Error),
    /// The hub closed the connection before answering the stop.
    #[error("the hub closed the connection before answering stop")]
    HubClosedBeforeAnswer,
    /// The hub did not close within the stop's deadline.
    #[error("the hub did not stop in {secs} s")]
    HubStopLate {
        /// The deadline in seconds.
        secs: u64,
    },
    /// The hub's accepted stop answer carries no session count.
    #[error("the hub answered stop without a session count")]
    StopCountMissing,
    /// The hub's pairing answer carries no code.
    #[error("the hub's pairing answer has no code")]
    PairingAnswerMissing,
    /// The address does not fit in a QR code.
    #[error("The address is too long to draw as a QR code.")]
    QrTooLong,
    /// The hub's `status` answer carries no usable variables.
    #[error("the hub's answer to `status` has no variables")]
    StatusNoVariables,
    /// The hub's `devices` answer is not a device list.
    #[error("the hub's answer to `devices` is not a device list")]
    DevicesMalformed,
    /// The Fiber home's path is too long to name the hub's login service.
    #[error("{}", hub_unit_name_too_long_message(*.bytes))]
    HubUnitNameTooLong {
        /// The overlong service name's length in bytes.
        bytes: usize,
    },
    /// `HOME` is missing or relative, so the hub's login service has no home.
    #[error("HOME must be an absolute path to find the hub's login service.")]
    HubHomeUnavailable,
    /// A path the hub's login service must name is not UTF-8 or holds a
    /// control character.
    #[error(
        "{path:?} is not UTF-8 or holds a control character, so the hub's login service cannot name it."
    )]
    HubUnitPathUnrepresentable {
        /// The path that cannot be named.
        path: PathBuf,
    },
    /// No pairing code was given for a paired hub.
    #[error("No pairing code was given. Run `fiber --help` for usage.")]
    HubPairingCodeMissing,
    /// The hub's pairing answer carries no device token.
    #[error("the hub's pairing answer has no token")]
    HubPairingTokenMissing,
    /// Another `fiber hub add` or `fiber hub remove` holds the name's lock.
    #[error("another `fiber hub add` or `fiber hub remove` of `{name}` is running")]
    HubListLocked {
        /// The hub being added or removed.
        name: String,
    },
    /// The hub is already declared in the global layer.
    #[error(
        "`{name}` is already declared in the global layer. Run `fiber hub remove {name}` to remove it first. Run `fiber --help` for usage."
    )]
    HubAlreadyDeclared {
        /// The hub already declared.
        name: String,
    },
    /// No hub with the name is in the client's list.
    #[error("No hub named `{name}` in this client's list. Run `fiber --help` for usage.")]
    HubNotFound {
        /// The hub named.
        name: String,
    },
    /// The hub-list entry write failed, keeping the device token only
    /// after a paired entry's rename.
    #[error("{}", hub_entry_failed_message(.source, *.paired))]
    HubEntryFailed {
        /// Why the entry write failed.
        source: ConfigError,
        /// Whether a device token was just stored for the entry.
        paired: bool,
    },
    /// The global `hub.port` is not a port.
    #[error("hub.port in {} is not a port: {value}", .path.display())]
    HubPortInvalid {
        /// The configured value.
        value: serde_json::Value,
        /// The global configuration file holding it.
        path: PathBuf,
    },
    /// A hub directory listing could not be read.
    #[error("reading {}: {source}", .path.display())]
    HubDevicesRead {
        /// The directory or file that could not be read.
        path: PathBuf,
        /// Why the read failed.
        source: io::Error,
    },
    /// The hub did not answer `status` before the deadline.
    #[error("the hub did not answer status in {seconds} s")]
    HubStatusLate {
        /// The deadline in seconds.
        seconds: f64,
    },
    /// The hub closed the connection before answering `status`.
    #[error("the hub closed the connection before answering status")]
    HubStatusClosed,
    /// The hub's `status` answer carries no version or client count.
    #[error("the hub's answer to status has no fiber_version or clients")]
    HubStatusAnswerMissing,
    /// The hub's status could not be written as JSON.
    #[error("the hub's status: {0}")]
    HubStatusJson(#[source] serde_json::Error),
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

/// What a failed install adds, so the person knows a rerun converges.
pub(crate) const RERUN: &str =
    "Run fiber hub install again, or fiber hub uninstall to remove what was written.";

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

/// The `HubUnitNameTooLong` sentence: the overlong name's length and the
/// service-name limit, which lives with the name.
fn hub_unit_name_too_long_message(bytes: usize) -> String {
    format!(
        "Fiber home's path is too long to name the hub's login service ({bytes} bytes, at most \
         {}); set FIBER_HOME to a shorter path.",
        crate::hub_unit::NAME_MAX
    )
}

/// The `HubEntryFailed` sentence: the source, keeping the device token
/// only after a paired entry's rename.
fn hub_entry_failed_message(source: &ConfigError, paired: bool) -> String {
    if paired && matches!(source, ConfigError::Unsynced { .. }) {
        format!("{source} The device token the entry refers to was kept.")
    } else {
        source.to_string()
    }
}

/// The `Hub` sentence: a local failure names the hub, any other failure
/// keeps the doors text.
fn hub_message(error: &doors::Error) -> String {
    if let doors::Error::Local(io) = error {
        format!("the hub: {io}")
    } else {
        error.to_string()
    }
}

/// What an installed hub needs from the manager `program` belongs to.
fn manager_needs(program: &str) -> &'static str {
    if program == "launchctl" {
        "launchd"
    } else {
        "systemd's user instance"
    }
}

/// The `ManagerFailed` sentence: the command, its exit code and the first
/// line of its stderr when it said one.
fn manager_failed_message(
    program: &str,
    args: &[String],
    exit: Option<i32>,
    said: Option<&str>,
) -> String {
    let code = exit.map_or_else(
        || "no exit code".to_owned(),
        |code| format!("exit code {code}"),
    );
    let said = said.map_or_else(String::new, |first| format!(": {first}"));
    format!("`{program} {}` failed with {code}{said}.", args.join(" "))
}

impl CliError {
    /// The stable code a caller switches on.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Config(error) => error.code(),
            Self::CredentialFile(error) => error.code(),
            Self::Extensions(error) => error.code(),
            Self::Forwarded(failure) => failure.code.clone(),
            Self::PruneFailed { code, .. } => code.clone(),
            Self::Log(error) => error.code(),
            Self::Hub(error) => error.code(),
            Self::Rejected { code, .. } => code.clone(),
            Self::UnitNotRestored { cause, .. } | Self::RerunInstall { cause } => cause.code(),
            Self::HubClosed { .. }
            | Self::CommandEncode(_)
            | Self::SessionsAnswer(_)
            | Self::SessionRow(_)
            | Self::SearchHit(_) => ErrorCode::IoFailed,
            Self::CurrentExe(_)
            | Self::LaunchdStillLoaded { .. }
            | Self::ManagerNotFound { .. }
            | Self::ManagerRun { .. }
            | Self::ManagerFailed { .. }
            | Self::UnitReadFailed { .. }
            | Self::UnitWriteFailed(_)
            | Self::UnitDeleteFailed { .. }
            | Self::HubRequestEncode(_)
            | Self::HubClosedBeforeAnswer
            | Self::HubStopLate { .. }
            | Self::StopCountMissing
            | Self::PairingAnswerMissing
            | Self::StatusNoVariables
            | Self::HubPairingTokenMissing
            | Self::HubListLocked { .. }
            | Self::HubDevicesRead { .. }
            | Self::HubStatusLate { .. }
            | Self::HubStatusClosed
            | Self::HubStatusAnswerMissing
            | Self::HubStatusJson(_)
            | Self::HubUnitPathUnrepresentable { .. }
            | Self::DevicesMalformed => ErrorCode::IoFailed,
            Self::QrTooLong => ErrorCode::Usage,
            Self::DeleteNobody => ErrorCode::Usage,
            Self::SessionNotFoundOnHub { .. } => ErrorCode::SessionNotFound,
            Self::Terminal(_)
            | Self::StandardError(_)
            | Self::StandardInput(_)
            | Self::CurrentDirectory(_)
            | Self::ExtensionDirectory(_)
            | Self::FiberExecutable(_)
            | Self::DoctorReport(_)
            | Self::DoctorDirectory(_)
            | Self::InterruptHandler(_)
            | Self::ModelRow(_)
            | Self::ConfigValue { .. }
            | Self::LoginRunning { .. }
            | Self::McpAddRunning { .. }
            | Self::ApprovalNotRecorded { .. } => ErrorCode::IoFailed,
            Self::ApprovalNotStored { source, .. } => source.code(),
            Self::ApproveFailed { source, .. } => source.code(),
            Self::ReleaseNoCommit => ErrorCode::ExtensionIncompatible,
            Self::UpdateBinaryDirectory { .. } | Self::UpdateBinaryRename { .. } => {
                ErrorCode::IoFailed
            }
            Self::NoFirstPartyPackage { .. } => ErrorCode::InvalidArguments,
            Self::SecretsKept(error) => error.code(),
            Self::HubEntryFailed { source, .. } => source.code(),
            Self::ApprovalChanged { .. }
            | Self::UrlHasUserinfo { .. }
            | Self::McpLoginRefused { .. }
            | Self::HubPortInvalid { .. }
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
            | Self::HostNotHost { .. }
            | Self::LogoutSeveral { .. }
            | Self::LabelNotStorable { .. }
            | Self::HubPairingCodeMissing
            | Self::HubAlreadyDeclared { .. }
            | Self::HubNotFound { .. }
            | Self::HubUnitNameTooLong { .. }
            | Self::HubHomeUnavailable
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
            | Self::McpServerStdio { .. }
            | Self::NobodyToAsk { .. }
            | Self::OlderThanUnreadable
            | Self::ReleaseVersionMismatch { .. } => ErrorCode::Usage,
        }
    }
}

impl From<CliError> for Failure {
    fn from(error: CliError) -> Self {
        // debt: this conversion match, a `Forwarded` passthrough plus `Failure::from(&other)` once `Failure` gains a constructor per code.
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
            | CliError::HostNotHost { .. }
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
            | CliError::McpServerStdio { .. }
            | CliError::NobodyToAsk { .. }
            | CliError::OlderThanUnreadable
            | CliError::PruneFailed { .. }
            | CliError::Log(_)
            | CliError::Hub(_)
            | CliError::HubClosed { .. }
            | CliError::Rejected { .. }
            | CliError::CommandEncode(_)
            | CliError::SessionsAnswer(_)
            | CliError::SessionRow(_)
            | CliError::SearchHit(_)
            | CliError::DeleteNobody
            | CliError::SessionNotFoundOnHub { .. }
            | CliError::ApproveFailed { .. }
            | CliError::ReleaseVersionMismatch { .. }
            | CliError::ReleaseNoCommit
            | CliError::UpdateBinaryDirectory { .. }
            | CliError::UpdateBinaryRename { .. }
            | CliError::ExtensionDirectory(_)
            | CliError::FiberExecutable(_)
            | CliError::DoctorReport(_)
            | CliError::DoctorDirectory(_)
            | CliError::NoFirstPartyPackage { .. }
            | CliError::CurrentExe(_)
            | CliError::LaunchdStillLoaded { .. }
            | CliError::ManagerNotFound { .. }
            | CliError::ManagerRun { .. }
            | CliError::ManagerFailed { .. }
            | CliError::UnitReadFailed { .. }
            | CliError::UnitWriteFailed(_)
            | CliError::UnitDeleteFailed { .. }
            | CliError::UnitNotRestored { .. }
            | CliError::RerunInstall { .. }
            | CliError::HubRequestEncode(_)
            | CliError::HubClosedBeforeAnswer
            | CliError::HubStopLate { .. }
            | CliError::StopCountMissing
            | CliError::PairingAnswerMissing
            | CliError::QrTooLong
            | CliError::StatusNoVariables
            | CliError::HubUnitNameTooLong { .. }
            | CliError::HubHomeUnavailable
            | CliError::HubUnitPathUnrepresentable { .. }
            | CliError::HubPairingCodeMissing
            | CliError::HubPairingTokenMissing
            | CliError::HubListLocked { .. }
            | CliError::HubAlreadyDeclared { .. }
            | CliError::HubNotFound { .. }
            | CliError::HubEntryFailed { .. }
            | CliError::HubPortInvalid { .. }
            | CliError::HubDevicesRead { .. }
            | CliError::HubStatusLate { .. }
            | CliError::HubStatusClosed
            | CliError::HubStatusAnswerMissing
            | CliError::HubStatusJson(_)
            | CliError::DevicesMalformed) => Failure {
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
