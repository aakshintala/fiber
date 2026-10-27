//! What one process extension runs as (`docs/extensions.md`, "Process
//! extensions"): its session, its workspace, its data directories and its
//! folded state. The package builds it once and hands it to the owner, and
//! tests inject fakes without changing the public `PackageOptions`.
//!
//! The production host services dispatch native host operations through
//! this context; package tests inject a fake implementation behind the
//! same trait.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};

use contract::tool::Cancel;

use crate::host::admission::CallbackContext;

/// What one process extension runs as: the first `extension_hello` line's
/// payload, and the context every native host call reads.
#[derive(Debug, Clone)]
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) struct ProcessContext {
    /// The session's id, as `extension_hello` carries it.
    pub session_id: contract::SessionId,
    /// Why the session started, resent after a restart.
    pub reason: contract::events::ExtensionReason,
    /// The session's workspace, resolving relative paths.
    pub workspace: PathBuf,
    /// The paths `host.data_dir` returns.
    pub data_dirs: contract::events::ExtensionDataDirs,
    /// The extension's folded state, as `state.get` reads it.
    pub state: Map<String, Value>,
    /// The extension's name, attributing its calls.
    pub extension: String,
    /// Fiber home, anchoring secrets and data directories.
    pub home: PathBuf,
}

#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
impl ProcessContext {
    /// The `extension_hello` payload: the session's identity, why it
    /// started, its workspace, the extension's data directories and its
    /// folded state, with the state as it stands now.
    pub(crate) fn hello(&self) -> contract::events::ExtensionHello {
        contract::events::ExtensionHello {
            session_id: self.session_id.clone(),
            reason: self.reason,
            workspace: self.workspace.display().to_string(),
            data_dirs: self.data_dirs.clone(),
            state: self.state.clone(),
        }
    }

    /// Refolds `state`, as sent again after a restart with the state as it
    /// stands in the session.
    pub(crate) fn refold(&mut self, state: Map<String, Value>) {
        self.state = state;
    }
}

/// One decoded process host or state call: the family and its arguments
/// as native values, with the Lua half's validation already applied.
/// Byte positions carry bytes, every other string carries text.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) enum HostOperation {
    /// `host.http`: the method, URL, headers and optional body bytes.
    Http {
        /// The method, defaulting to `GET`.
        method: String,
        /// The URL.
        url: String,
        /// The headers.
        headers: Vec<(String, String)>,
        /// The body bytes, when the call passed one.
        body: Option<Vec<u8>>,
    },
    /// `host.exec`: the program, arguments, working directory, extra
    /// environment and optional input bytes.
    Exec {
        /// The program, resolved against the workspace when relative.
        program: String,
        /// Its arguments.
        args: Vec<String>,
        /// Its working directory, resolved against the workspace.
        cwd: PathBuf,
        /// Variables added to the inherited environment.
        env: BTreeMap<String, String>,
        /// Bytes piped to the child's standard input, then EOF.
        input: Option<Vec<u8>>,
    },
    /// `host.fs.read`: the path, answered with the file's bytes.
    FsRead {
        /// The path, resolved against the workspace.
        path: String,
    },
    /// `host.fs.write`: the path, the contents bytes and the lock option.
    FsWrite {
        /// The path, resolved against the workspace.
        path: String,
        /// The contents bytes.
        contents: Vec<u8>,
        /// Whether to hold the path lock while writing.
        lock: bool,
    },
    /// `host.fs.list`: the path, answered with its entry names.
    FsList {
        /// The path, resolved against the workspace.
        path: String,
    },
    /// `host.fs.stat`: the path, answered with its facts or null.
    FsStat {
        /// The path, resolved against the workspace.
        path: String,
    },
    /// `host.fs.mkdir`: the path and the lock option.
    FsMkdir {
        /// The path, resolved against the workspace.
        path: String,
        /// Whether to hold the path lock while creating.
        lock: bool,
    },
    /// `host.fs.remove`: the path and the lock option.
    FsRemove {
        /// The path, resolved against the workspace.
        path: String,
        /// Whether to hold the path lock while removing.
        lock: bool,
    },
    /// `host.fs.rename`: both paths and the lock option.
    FsRename {
        /// The source path, resolved against the workspace.
        from: String,
        /// The destination path, resolved against the workspace.
        to: String,
        /// Whether to hold the path locks while renaming.
        lock: bool,
    },
    /// `host.config.get`: the dotted key, answered with the value or null.
    ConfigGet {
        /// The dotted key.
        key: String,
    },
    /// `host.config.set`: the dotted key, value and scope.
    ConfigSet {
        /// The dotted key.
        key: String,
        /// The value.
        value: Value,
        /// The scope: `machine` or `project`.
        scope: ConfigScope,
    },
    /// `host.secret`: the declared name, answered with the secret or null.
    Secret {
        /// The secret's name.
        name: String,
    },
    /// `host.data_dir`: the scope, answered with the directory path.
    DataDir {
        /// The scope: `machine` or `project`.
        scope: DataScope,
    },
    /// `host.sha256`: the bytes, answered with the hex digest.
    Sha256 {
        /// The bytes.
        data: Vec<u8>,
    },
    /// `host.hmac_sha256`: the key and data bytes, answered with the raw
    /// digest bytes, so one result passes unchanged as the next call's key.
    HmacSha256 {
        /// The key bytes.
        key: Vec<u8>,
        /// The data bytes.
        data: Vec<u8>,
    },
    /// `host.status`: one line of status.
    Status {
        /// The line.
        text: String,
    },
    /// `host.widget`: a named block of lines.
    Widget {
        /// The widget's id.
        id: String,
        /// Its lines.
        lines: Vec<String>,
    },
    /// `host.emit`: a message to the extension's own TUI half.
    Emit {
        /// The message data.
        data: Value,
    },
    /// `host.log`: a diagnostic line.
    Log {
        /// The line, as written.
        message: String,
    },
    /// `host.tool`: the inner tool through the runner service, which the
    /// tests stub. Without a runner the call fails as a tool call with no
    /// runner does in Lua.
    Tool {
        /// The tool's name.
        name: String,
        /// Its arguments.
        arguments: Value,
    },
    /// `host.model`: the model request through the session's routing.
    Model {
        /// The model, system, messages and token limit.
        request: contract::model::HostModelRequest,
    },
    /// `host.delegate`: the spec arguments and per-call timeout. Without
    /// a delegate service the call fails as a closing session does.
    Delegate {
        /// The snapshotted spec as its argument map.
        arguments: Map<String, Value>,
        /// The per-call timeout.
        timeout: Duration,
        /// Whether the call passed its `run` function: the caller owns
        /// that end, and this step only starts the job.
        run: bool,
    },
    /// `host.oauth.callback`: serve one request on this localhost port.
    OAuthCallback {
        /// The port.
        port: u16,
        /// The path, when set.
        path: Option<String>,
    },
    /// `host.oauth.open`: show the URL in a browser.
    OAuthOpen {
        /// The URL.
        url: String,
    },
    /// `host.oauth.show`: show the device-code code to enter at the URL.
    OAuthShow {
        /// The URL.
        url: String,
        /// The code.
        code: String,
    },
    /// `host.oauth.pkcs`: a fresh PKCE pair, answered with its challenge
    /// and verifier.
    OAuthPkcs,
    /// `host.oauth.poll`: wait for the device flow to finish, polling the
    /// URL on the interval.
    OAuthPoll {
        /// The polled URL.
        url: String,
        /// The method, defaulting to `POST`.
        method: String,
        /// The headers.
        headers: Vec<(String, String)>,
        /// The body bytes, when the call passed one.
        body: Option<Vec<u8>>,
        /// The statuses that wait, defaulting to authorization-pending.
        pending: Option<Vec<u16>>,
        /// The seconds between polls.
        interval: u64,
    },
    /// `state.get`: the key, answered with the value or null.
    StateGet {
        /// The key.
        key: String,
    },
    /// `state.keys`: answered with the set keys.
    StateKeys,
    /// `state.set`: the key, value and fork rule.
    StateSet {
        /// The key.
        key: String,
        /// The value.
        value: Value,
        /// The fork rule.
        on_fork: contract::events::OnFork,
    },
    /// `state.unset`: the key.
    StateUnset {
        /// The key.
        key: String,
    },
}

/// The scope a `host.config.set` writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) enum ConfigScope {
    /// The machine scope.
    Machine,
    /// The project scope.
    Project,
}

/// The scope a `host.data_dir` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) enum DataScope {
    /// The machine directory.
    Machine,
    /// The project directory.
    Project,
}

/// One inner tool call: what the runner service runs, inheriting the
/// outer callback's attribution, deadline and cancellation.
#[derive(Debug, Clone)]
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) struct ToolCall {
    /// The extension whose tool runs.
    pub extension: String,
    /// The tool's name.
    pub name: String,
    /// Its arguments.
    pub arguments: Value,
    /// What the outer callback runs as.
    pub ran_by: String,
    /// The outer callback's end.
    pub deadline: Option<std::time::Instant>,
}

/// Runs one inner tool call to completion, as the runner behind
/// `host.tool` does in Lua. Tests stand a stub behind this trait; the
/// session wires its runner when connected.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) trait ToolRunner: Send + Sync {
    /// Runs `call`'s tool, stopped by `cancel`. The error is raised as
    /// the calling code's string, as in Lua.
    fn run(
        &self,
        call: &ToolCall,
        cancel: &dyn Cancel,
    ) -> Result<Box<contract::events::ToolCallCompleted>, String>;
}

/// The services one process host or state call runs through: the decoded
/// operation runs under `callback` in `context`, stopped by `cancel`.
/// `None` answers nothing, `{}`; `Some` answers `{ value }`.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) trait ProcessHostServices: Send + Sync {
    /// Runs `operation`, answering its value or the code and message the
    /// Lua call raises.
    fn perform(
        &self,
        context: &ProcessContext,
        callback: &CallbackContext,
        operation: HostOperation,
        cancel: &dyn Cancel,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)>;
}

/// The production host services: shared native operations behind the
/// trait, with the session's services present only when the session
/// wired them. Without a session, service-backed calls fail exactly as
/// the Lua half fails without one.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(crate) struct NativeHostServices {
    /// Emits the session-visible lines: exec, status, widget, message,
    /// log and state events.
    pub runtime: crate::runtime::Runtime,
    /// The extension's state store, reading the folded state.
    pub state: Arc<crate::host::state::ExtensionState>,
    /// The clock every wait reads.
    pub clock: Arc<dyn contract::clock::Clock>,
    /// Opens URLs and serves the OAuth callback.
    pub browser: Arc<dyn crate::oauth::Browser>,
    /// Runs model requests; absent without a session.
    pub model: Option<Arc<dyn contract::model::HostModel>>,
    /// Starts delegates; absent without a session.
    pub delegate: Option<Arc<dyn contract::extension::HostDelegate>>,
    /// Runs inner tools; absent without a session.
    pub tool_runner: Option<Arc<dyn ToolRunner>>,
    /// The manifest's declared secret names.
    pub secrets: Vec<String>,
}

/// How long one parked wait rests before re-reading its end condition.
const WAIT_POLL: Duration = Duration::from_millis(20);

mod native;
