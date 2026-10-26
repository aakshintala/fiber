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
use support::clock::Wake as _;

use crate::host::admission::CallbackContext;

/// What one process extension runs as: the first `extension_hello` line's
/// payload, and the context every native host call reads.
#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
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

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
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
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
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
        /// Whether the call passed its `run` function: Part D routes that
        /// end, and Part C only starts the job.
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
    /// `host.oauth.refresh`: run the nested function the call passed and
    /// refresh the held credential with its result, releasing the lock
    /// after. The nested request is minted before the service runs, so the
    /// pump routes it while the refresh awaits it.
    OAuthRefresh {
        /// The nested function's request, already sent to the child.
        nested: NestedCall,
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
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum ConfigScope {
    /// The machine scope.
    Machine,
    /// The project scope.
    Project,
}

/// The scope a `host.data_dir` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum DataScope {
    /// The machine directory.
    Machine,
    /// The project directory.
    Project,
}

/// The nested function call a `host.oauth.refresh` runs in the child: its
/// minted request, whose answer the service reads after the pump routes
/// it. The call inherits the outer callback's `ran_by`, deadline and hook
/// transaction, and ends when the outer request ends.
#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) struct NestedCall {
    /// The nested request's id.
    pub id: contract::RequestId,
    /// What the nested callback runs as, inheriting the outer callback.
    pub ran_by: String,
    /// The nested callback's end, inheriting the outer deadline.
    pub deadline: Option<std::time::Instant>,
    /// The hook transaction the nested writes join, if any.
    pub state_call: Option<u64>,
}

/// One inner tool call: what the runner service runs, inheriting the
/// outer callback's attribution, deadline and cancellation.
#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
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
/// `host.tool` does in Lua. Tests stand a stub behind this trait; Part D
/// wires the session's runner.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
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
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
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
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
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

impl NativeHostServices {
    /// Runs `host.http`: the request with the callback's remaining
    /// deadline plus the grace as its backstop, answering the status
    /// with the body's bytes, redacted exactly as the Lua half redacts.
    /// The body reads as a bytes position. A call with no deadline
    /// waits without a backstop, as a timer's does.
    fn http(
        &self,
        method: &str,
        url: &str,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
        callback: &CallbackContext,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        // The grace the Lua scheduler adds to the declared timeout.
        const GRACE: Duration = Duration::from_secs(1);
        let timeout = callback
            .deadline
            .map(|end| end.saturating_duration_since(self.clock.now()) + GRACE);
        let (status, bytes) = crate::host::perform(&crate::host::HttpRequest {
            method: method.to_owned(),
            url: url.to_owned(),
            headers,
            body,
            timeout,
        })?;
        let mut answer = Map::new();
        answer.insert("status".to_owned(), Value::from(status));
        answer.insert(
            "body".to_owned(),
            Value::String(crate::host::admission::encode_bytes(&bytes)),
        );
        Ok(Some(Value::Object(answer)))
    }

    /// Runs `host.exec`: the child in its group, stopped when the
    /// request ends, logging `extension_exec` outside a tool request.
    /// The run's bytes read as bytes positions. A refused start writes
    /// no line; a stop ends the group without stopping anything else.
    #[allow(clippy::too_many_arguments, reason = "one decoded call's fields")]
    fn exec(
        &self,
        context: &ProcessContext,
        callback: &CallbackContext,
        program: &str,
        args: Vec<String>,
        cwd: &std::path::Path,
        env: BTreeMap<String, String>,
        input: Option<Vec<u8>>,
        cancel: &dyn Cancel,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let _watch = self.watch_cancel(cancel, stop);
        // An absent working directory is the workspace; a relative one
        // joins it, and an absolute one is used as given, as in Lua.
        let cwd = context.workspace.join(cwd);
        let ran = crate::host::exec::run(
            &crate::host::exec::ExecRequest {
                program: program.to_owned(),
                args: args.clone(),
                cwd: cwd.to_owned(),
                env,
                input,
                cap: crate::host::fs::PROCESS_READ_CAP,
                own_group: true,
                #[cfg(test)]
                stdout_read: None,
            },
            self.clock.as_ref(),
            callback.deadline,
            stopped,
        )
        .map_err(|error: Box<crate::host::exec::ExecError>| (error.code, error.message))?;
        if callback.exec_logged() {
            self.runtime.emit(&contract::events::Event::ExtensionExec(
                contract::events::ExtensionExec {
                    extension: context.extension.clone(),
                    program: program.to_owned(),
                    args,
                    cwd: cwd.display().to_string(),
                    process: contract::shapes::Process {
                        exit_code: ran.exit_code,
                        signal: ran.signal.clone(),
                        timed_out: ran.timed_out,
                    },
                },
            ));
        }
        let mut answer = Map::new();
        if let Some(code) = ran.exit_code {
            answer.insert("exit_code".to_owned(), Value::from(code));
        }
        if let Some(signal) = ran.signal {
            answer.insert("signal".to_owned(), Value::String(signal));
        }
        answer.insert(
            "stdout".to_owned(),
            Value::String(crate::host::admission::encode_bytes(&ran.stdout)),
        );
        answer.insert(
            "stderr".to_owned(),
            Value::String(crate::host::admission::encode_bytes(&ran.stderr)),
        );
        Ok(Some(Value::Object(answer)))
    }

    /// Fires `stop` when `cancel` fires, bridging the token to the
    /// channel an `exec` run watches. An already-fired cancel stops at
    /// once, so a cancelled call never starts its child. The caller
    /// holds the guard for the run: dropping it unwatches the cancel.
    fn watch_cancel(
        &self,
        cancel: &dyn Cancel,
        stop: std::sync::mpsc::Sender<()>,
    ) -> Arc<CancelFire> {
        use std::sync::Weak;
        let fire = Arc::new(CancelFire {
            stop: std::sync::Mutex::new(Some(stop)),
        });
        if cancel.is_cancelled() {
            fire.wake();
            return fire;
        }
        cancel.subscribe(Arc::downgrade(&fire) as Weak<dyn support::clock::Wake>);
        if cancel.is_cancelled() {
            fire.wake();
        }
        fire
    }

    /// Runs `host.tool`: the inner tool through the runner service,
    /// inheriting the outer callback's attribution, deadline and
    /// cancellation. Without a runner the call fails as a runnerless
    /// tool call does in Lua, starting nothing.
    fn tool(
        &self,
        context: &ProcessContext,
        callback: &CallbackContext,
        name: &str,
        arguments: Value,
        cancel: &dyn Cancel,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let Some(runner) = &self.tool_runner else {
            return Err((
                contract::ErrorCode::InvalidArguments,
                "host.tool: called outside a running tool call".to_owned(),
            ));
        };
        let completed = runner
            .run(
                &ToolCall {
                    extension: context.extension.clone(),
                    name: name.to_owned(),
                    arguments,
                    ran_by: callback.ran_by().unwrap_or("a timer").to_owned(),
                    deadline: callback.deadline,
                },
                cancel,
            )
            .map_err(|message| (contract::ErrorCode::InvalidArguments, message))?;
        crate::host::tool::outcome(&completed)
            .map(Some)
            .map_err(|error| {
                (
                    contract::ErrorCode::InvalidArguments,
                    format!("host.tool: {error}"),
                )
            })
    }

    /// Runs `host.model`: the request through the session's routing with
    /// the callback's deadline, clock and cancel, recording usage under
    /// the extension. Without a model service the session is closing.
    fn model(
        &self,
        context: &ProcessContext,
        callback: &CallbackContext,
        request: contract::model::HostModelRequest,
        cancel: &dyn Cancel,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let Some(model) = &self.model else {
            return Err((
                contract::ErrorCode::Closing,
                "host.model: the session is closing".to_owned(),
            ));
        };
        let _ = cancel;
        let cancel: Arc<dyn Cancel> = Arc::new(callback.cancel.clone());
        let runtime = self.runtime.clone();
        let extension = context.extension.clone();
        let reply = model
            .call(
                &context.extension,
                request,
                contract::model::Bound {
                    deadline: callback.deadline,
                    clock: Arc::clone(&self.clock),
                    cancel,
                    lines: Arc::new(move |record: contract::events::UsageRecorded| {
                        let mut record = record;
                        record.extension = Some(extension.clone());
                        runtime.emit(&contract::events::Event::UsageRecorded(record));
                    }),
                },
            )
            .map_err(|failure| (failure.code, failure.message))?;
        let mut tokens = Map::new();
        tokens.insert("input".to_owned(), Value::from(reply.tokens.input));
        tokens.insert(
            "cache_read".to_owned(),
            Value::from(reply.tokens.cache_read),
        );
        let mut written = Map::new();
        for (lifetime, count) in &reply.tokens.cache_write {
            written.insert(lifetime.as_str().to_owned(), Value::from(*count));
        }
        tokens.insert("cache_write".to_owned(), Value::Object(written));
        tokens.insert("output".to_owned(), Value::from(reply.tokens.output));
        let mut usage = Map::new();
        usage.insert("tokens".to_owned(), Value::Object(tokens));
        usage.insert(
            "cost".to_owned(),
            reply.cost.map(Value::from).unwrap_or(Value::Null),
        );
        let mut answer = Map::new();
        answer.insert("text".to_owned(), Value::String(reply.text));
        answer.insert("usage".to_owned(), Value::Object(usage));
        Ok(Some(Value::Object(answer)))
    }

    /// Starts `host.delegate`: the job under the session's gate, binding
    /// its id. Without a delegate service the session is closing. The
    /// chunk-local `run` handle is the session's: Part C starts the job,
    /// and routing its end belongs to Part D.
    fn delegate(
        &self,
        context: &ProcessContext,
        callback: &CallbackContext,
        arguments: Map<String, Value>,
        timeout: Duration,
        _run: bool,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let Some(delegate) = &self.delegate else {
            return Err((
                contract::ErrorCode::Closing,
                "host.delegate: the session is closing".to_owned(),
            ));
        };
        let _ = (callback, timeout);
        let mut bound: Option<contract::JobId> = None;
        let mut bind = |id: &contract::JobId| {
            bound = Some(id.clone());
        };
        let id = delegate
            .start(&context.extension, &arguments, &mut bind)
            .map_err(|rejection| (rejection.code, rejection.message))?;
        drop(bound);
        Ok(Some(Value::String(id.0.clone())))
    }

    /// Reads `host.config.get`: the session's config has no process half
    /// in Part C, so the call fails naming the missing session, exactly
    /// as the Lua half fails without one.
    fn config_get(&self, key: &str) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let _ = key;
        Err((
            contract::ErrorCode::InvalidArguments,
            "host.config.get: this extension has no session".to_owned(),
        ))
    }

    /// Writes `host.config.set`: without a session there is nothing to
    /// write, so the call fails naming the missing session.
    fn config_set(
        &self,
        key: &str,
        value: Value,
        scope: ConfigScope,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let _ = (key, value, scope);
        Err((
            contract::ErrorCode::InvalidArguments,
            "host.config.set: this extension has no session".to_owned(),
        ))
    }

    /// Reads `host.secret`: the manifest's declared names from the home,
    /// answering null for an absent file, as in Lua.
    fn secret(
        &self,
        context: &ProcessContext,
        name: &str,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let contents =
            crate::host::read_secret(&context.home, &self.secrets, name).map_err(|failure| {
                match failure {
                    crate::host::SecretFailure::String(message) => {
                        (contract::ErrorCode::InvalidArguments, message)
                    }
                    crate::host::SecretFailure::Coded(code, message) => (code, message),
                }
            })?;
        Ok(contents.map(Value::String))
    }

    /// Reads `host.data_dir`: the folded directory for the scope.
    fn data_dir(&self, context: &ProcessContext, scope: DataScope) -> String {
        match scope {
            DataScope::Machine => context.data_dirs.machine.clone(),
            DataScope::Project => context.data_dirs.project.clone(),
        }
    }

    /// Answers `host.sha256` with the hex digest.
    fn sha256(data: &[u8]) -> String {
        support::sha256_hex(data)
    }

    /// Answers `host.hmac_sha256` with the raw digest bytes, so one
    /// result passes unchanged as the next call's key.
    fn hmac(
        &self,
        key: &[u8],
        data: &[u8],
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
        Ok(Some(Value::String(crate::host::admission::encode_bytes(
            ring::hmac::sign(&key, data).as_ref(),
        ))))
    }

    /// Records `host.status`: one status line under the extension.
    fn status(&self, context: &ProcessContext, text: &str) {
        self.runtime.emit(&contract::events::Event::ExtensionUi(
            contract::events::ExtensionUi {
                extension: context.extension.clone(),
                ui: contract::events::Ui::Status {
                    status: text.to_owned(),
                },
            },
        ));
    }

    /// Records `host.widget`: a named block of lines under the extension.
    fn widget(&self, context: &ProcessContext, id: &str, lines: Vec<String>) {
        self.runtime.emit(&contract::events::Event::ExtensionUi(
            contract::events::ExtensionUi {
                extension: context.extension.clone(),
                ui: contract::events::Ui::Widget {
                    widget: id.to_owned(),
                    lines,
                },
            },
        ));
    }

    /// Records `host.emit`: a message to the extension's own TUI half.
    fn emit(&self, context: &ProcessContext, data: Value) {
        self.runtime
            .emit(&contract::events::Event::ExtensionMessage(
                contract::events::ExtensionMessage {
                    extension: context.extension.clone(),
                    data,
                },
            ));
    }

    /// Records `host.log`: the diagnostic line, lossy in UTF-8 as in Lua.
    fn log(&self, context: &ProcessContext, message: &str) {
        self.runtime.emit(&contract::events::Event::ExtensionLog(
            contract::events::ExtensionLog {
                extension: context.extension.clone(),
                message: message.to_owned(),
            },
        ));
    }

    /// The `host.fs` handle for `context`: the workspace, the folded
    /// data directories and the process read bound.
    fn fs(&self, context: &ProcessContext) -> crate::host::fs::Fs {
        crate::host::fs::Fs::for_process(
            context.workspace.clone(),
            context.data_dirs.machine.clone(),
            context.data_dirs.project.clone(),
        )
    }

    /// Reads `host.fs.read`: the file's bytes, answered as a bytes
    /// position.
    fn fs_read(
        &self,
        context: &ProcessContext,
        path: &str,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let bytes = self
            .fs(context)
            .read(path.as_bytes())
            .map_err(|(code, message)| (code, format!("host.fs.read: {message}")))?;
        Ok(Some(Value::String(crate::host::admission::encode_bytes(
            &bytes,
        ))))
    }

    /// Runs `host.fs.write`: the contents bytes to the path.
    fn fs_write(
        &self,
        context: &ProcessContext,
        path: &str,
        contents: Vec<u8>,
        lock: bool,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        self.fs(context)
            .write(path.as_bytes(), &contents, lock)
            .map_err(|(code, message)| (code, format!("host.fs.write: {message}")))?;
        Ok(None)
    }

    /// Runs `host.fs.list`: the entry names as text strings.
    fn fs_list(
        &self,
        context: &ProcessContext,
        path: &str,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let entries = self
            .fs(context)
            .list(path.as_bytes())
            .map_err(|(code, message)| (code, format!("host.fs.list: {message}")))?;
        let mut names = Vec::with_capacity(entries.len());
        for entry in entries {
            names.push(Value::String(String::from_utf8_lossy(&entry).into_owned()));
        }
        Ok(Some(Value::Array(names)))
    }

    /// Runs `host.fs.stat`: the facts, or null when the path is absent.
    fn fs_stat(
        &self,
        context: &ProcessContext,
        path: &str,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let facts = self
            .fs(context)
            .stat(path.as_bytes())
            .map_err(|(code, message)| (code, format!("host.fs.stat: {message}")))?;
        let Some(facts) = facts else {
            return Ok(Some(Value::Null));
        };
        let mut answer = Map::new();
        answer.insert("kind".to_owned(), Value::String(facts.kind.to_owned()));
        answer.insert("size".to_owned(), Value::from(facts.size));
        answer.insert("modified_ms".to_owned(), Value::from(facts.modified_ms));
        Ok(Some(Value::Object(answer)))
    }

    /// Runs `host.fs.mkdir`.
    fn fs_mkdir(
        &self,
        context: &ProcessContext,
        path: &str,
        lock: bool,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        self.fs(context)
            .mkdir(path.as_bytes(), lock)
            .map_err(|(code, message)| (code, format!("host.fs.mkdir: {message}")))?;
        Ok(None)
    }

    /// Runs `host.fs.remove`.
    fn fs_remove(
        &self,
        context: &ProcessContext,
        path: &str,
        lock: bool,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        self.fs(context)
            .remove(path.as_bytes(), lock)
            .map_err(|(code, message)| (code, format!("host.fs.remove: {message}")))?;
        Ok(None)
    }

    /// Runs `host.fs.rename`.
    fn fs_rename(
        &self,
        context: &ProcessContext,
        from: &str,
        to: &str,
        lock: bool,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        self.fs(context)
            .rename(from.as_bytes(), to.as_bytes(), lock)
            .map_err(|(code, message)| (code, format!("host.fs.rename: {message}")))?;
        Ok(None)
    }

    /// The state scope this callback writes under: a hook request joins
    /// its transaction, and everything else writes immediately.
    fn scope(callback: &CallbackContext) -> crate::host::state::Scope {
        match callback.state_call {
            Some(call) => crate::host::state::Scope::Hook(call),
            None => crate::host::state::Scope::Other,
        }
    }

    /// Reads `state.get`: the value, or null when the key is unset.
    fn state_get(&self, key: &str) -> Option<Value> {
        self.state.get(key)
    }

    /// Reads `state.keys`: the set keys.
    fn state_keys(&self) -> Value {
        Value::Array(self.state.keys().into_iter().map(Value::String).collect())
    }

    /// Maps a state write failure to the `state.*` sentence and code the
    /// Lua half raises: too large names the bytes against the cap, and a
    /// closed registry names the closing session.
    fn state_error(
        call: &str,
        error: crate::host::state::StateError,
    ) -> (contract::ErrorCode, String) {
        match error {
            crate::host::state::StateError::TooLarge { bytes } => (
                contract::ErrorCode::StateTooLarge,
                format!("{call}: value is {bytes} JSON bytes; at most 65536 are allowed"),
            ),
            crate::host::state::StateError::Closing => (
                contract::ErrorCode::Closing,
                format!("{call}: the session is closing"),
            ),
        }
    }

    /// Runs `state.set`: under a hook request the write joins the hook's
    /// change, committed when the hook succeeds; anywhere else it takes
    /// effect at once. The committed change is logged by the caller.
    fn state_set(
        &self,
        context: &ProcessContext,
        callback: &CallbackContext,
        key: &str,
        value: Value,
        on_fork: contract::events::OnFork,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let _ = context;
        self.state
            .set(Self::scope(callback), key, value, on_fork)
            .map_err(|error| Self::state_error("state.set", error))?;
        Ok(None)
    }

    /// Runs `state.unset`: removing the key under the same scope rules.
    fn state_unset(
        &self,
        context: &ProcessContext,
        callback: &CallbackContext,
        key: &str,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let _ = context;
        self.state
            .unset(Self::scope(callback), key)
            .map_err(|error| Self::state_error("state.unset", error))?;
        Ok(None)
    }

    /// Runs `host.oauth.callback`: serves one request on the port and
    /// answers its query pairs, unattended and unreadable exactly as in
    /// Lua. The wait ends when the callback ends, answered stale.
    fn oauth_callback(
        &self,
        context: &ProcessContext,
        callback: &CallbackContext,
        port: u16,
        path: Option<&str>,
        cancel: &dyn Cancel,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let _ = context;
        if !self.browser.attended() {
            return Err((
                contract::ErrorCode::AuthenticationFailed,
                "host.oauth.callback needs a person to log in, and nobody is attached".to_owned(),
            ));
        }
        let (answered, answer) = std::sync::mpsc::channel();
        let deliver: crate::oauth::Deliver = Arc::new(move |reply| match answered.send(reply) {
            Ok(()) | Err(_) => {}
        });
        let _serve = crate::oauth::listen(
            port,
            path.map(str::to_owned),
            &deliver,
            self.browser.as_ref(),
        )
        .map_err(|reply| match reply {
            crate::host::Reply::Query(Err(failure)) => failure,
            crate::host::Reply::Query(Ok(_))
            | crate::host::Reply::Http(_)
            | crate::host::Reply::Ask(_)
            | crate::host::Reply::Drive(_)
            | crate::host::Reply::Exec(_)
            | crate::host::Reply::Tool(_)
            | crate::host::Reply::Model(_)
            | crate::host::Reply::Delegate(_)
            | crate::host::Reply::Lock(_)
            | crate::host::Reply::Slept => (
                contract::ErrorCode::IoFailed,
                format!("host.oauth.callback: port {port}: the listener failed"),
            ),
        })?;
        loop {
            if cancel.is_cancelled() || self.ended(callback) {
                return Err((
                    contract::ErrorCode::Closing,
                    "host.oauth.callback: the callback ended".to_owned(),
                ));
            }
            match answer.try_recv() {
                Ok(crate::host::Reply::Query(Ok(pairs))) => {
                    let mut query = Map::new();
                    for (name, value) in pairs {
                        query.insert(name, Value::String(value));
                    }
                    return Ok(Some(Value::Object(query)));
                }
                Ok(crate::host::Reply::Query(Err(failure))) => return Err(failure),
                Ok(_) => {
                    return Err((
                        contract::ErrorCode::IoFailed,
                        format!("host.oauth.callback: port {port}: the listener failed"),
                    ));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => self.rest(),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Err((
                        contract::ErrorCode::IoFailed,
                        format!("host.oauth.callback: port {port}: the listener failed"),
                    ));
                }
            }
        }
    }

    /// Runs `host.oauth.open`: shows the URL in a browser.
    fn oauth_open(&self, url: &str) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        if !self.browser.attended() {
            return Err((
                contract::ErrorCode::AuthenticationFailed,
                "host.oauth.open needs a person to log in, and nobody is attached".to_owned(),
            ));
        }
        self.browser.open(url);
        Ok(None)
    }

    /// Runs `host.oauth.show`: shows the device-code to enter at the URL.
    fn oauth_show(
        &self,
        url: &str,
        code: &str,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        if !self.browser.attended() {
            return Err((
                contract::ErrorCode::AuthenticationFailed,
                "host.oauth.show needs a person to log in, and nobody is attached".to_owned(),
            ));
        }
        self.browser.show(url, code);
        Ok(None)
    }

    /// Answers `host.oauth.pkce` with a fresh challenge and verifier.
    fn pkcs() -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let (challenge, verifier) = crate::oauth::pkce_pair()?;
        let mut pair = Map::new();
        pair.insert("challenge".to_owned(), Value::String(challenge));
        pair.insert("verifier".to_owned(), Value::String(verifier));
        Ok(Some(Value::Object(pair)))
    }

    /// Runs `host.oauth.poll`: polls the URL until the flow finishes,
    /// waiting like `authorization_pending` on the pending statuses and
    /// slowing down on `slow_down`, exactly as in Lua. The wait ends
    /// when the callback ends, answered stale.
    #[allow(clippy::too_many_arguments, reason = "one decoded call's fields")]
    fn oauth_poll(
        &self,
        callback: &CallbackContext,
        url: &str,
        method: &str,
        headers: &[(String, String)],
        body: Option<Vec<u8>>,
        pending: Option<&[u16]>,
        interval: u64,
        cancel: &dyn Cancel,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        const GRACE: Duration = Duration::from_secs(1);
        let timeout = callback
            .deadline
            .map(|end| end.saturating_duration_since(self.clock.now()) + GRACE);
        let mut wait = interval;
        loop {
            if cancel.is_cancelled() || self.ended(callback) {
                return Err((
                    contract::ErrorCode::Closing,
                    "host.oauth.poll: the callback ended".to_owned(),
                ));
            }
            if !self.browser.attended() {
                return Err((
                    contract::ErrorCode::AuthenticationFailed,
                    "host.oauth.poll needs a person to log in, and nobody is attached".to_owned(),
                ));
            }
            let (status, bytes) = crate::host::perform(&crate::host::HttpRequest {
                method: method.to_owned(),
                url: url.to_owned(),
                headers: headers.to_owned(),
                body: body.clone(),
                timeout,
            })?;
            let waiting = pending.is_some_and(|pending| pending.contains(&status));
            if !waiting {
                // Only a `{` after any space opens the object the ruling
                // requires: a JSON array is not one.
                let text = String::from_utf8_lossy(&bytes);
                let body = text
                    .trim_start()
                    .strip_prefix('{')
                    .and_then(|_| serde_json::from_str::<Value>(&text).ok())
                    .filter(Value::is_object);
                let Some(body) = body else {
                    return Err((
                        contract::ErrorCode::UnreadableReply,
                        format!(
                            "host.oauth.poll: status {status} with a body that is not a JSON object"
                        ),
                    ));
                };
                let code = body.get("error").and_then(Value::as_str);
                match code {
                    None => {
                        if !(200..=299).contains(&status) {
                            return Err((
                                contract::ErrorCode::HttpError,
                                format!("host.oauth.poll: status {status}"),
                            ));
                        }
                        return Ok(Some(body));
                    }
                    Some("slow_down") => {
                        wait += 5;
                        if wait > 3600 {
                            return Err((
                                contract::ErrorCode::RateLimited,
                                "host.oauth.poll: the server asked for more than 3600 seconds between polls"
                                    .to_owned(),
                            ));
                        }
                    }
                    Some("authorization_pending") => {}
                    Some(code) => {
                        let detail = body
                            .get("error_description")
                            .and_then(Value::as_str)
                            .map(|detail| format!(": {detail}"))
                            .unwrap_or_default();
                        return Err((
                            contract::ErrorCode::AuthenticationFailed,
                            format!("host.oauth.poll: {code}{detail}"),
                        ));
                    }
                }
            }
            self.rest_for(Duration::from_secs(wait), callback, cancel);
        }
    }

    /// Runs `host.oauth.refresh`: without a provider credential the call
    /// fails naming the callback, after the nested function answers. The
    /// nested request is already sent; its answer is dropped with the
    /// refusal, and nothing is held.
    fn oauth_refresh(
        &self,
        callback: &CallbackContext,
        nested: &NestedCall,
        cancel: &dyn Cancel,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let _ = (nested, cancel);
        Err((
            contract::ErrorCode::InvalidArguments,
            crate::host::admission::lock_arg_error("host.oauth.refresh", callback.ran_by()),
        ))
    }

    /// Whether the callback ended: its deadline passed. Cancellation is
    /// the caller's own watch; a passed deadline ends every wait here.
    fn ended(&self, callback: &CallbackContext) -> bool {
        callback.deadline.is_some_and(|end| self.clock.now() >= end)
    }

    /// Rests one slice on the injected clock, re-reading the wait after.
    fn rest(&self) {
        let flag = std::sync::Mutex::new(());
        let changed = std::sync::Condvar::new();
        let guard = support::lock(&flag);
        let _ = support::clock::park(
            self.clock.as_ref(),
            None,
            Some(WAIT_POLL),
            &changed,
            guard,
            |()| false,
        );
    }

    /// Rests `wait` on the injected clock in slices, re-reading after
    /// each, so a fake clock the test moves ends the rest early.
    fn rest_for(&self, wait: Duration, callback: &CallbackContext, cancel: &dyn Cancel) {
        let start = self.clock.now();
        loop {
            if cancel.is_cancelled() || self.ended(callback) {
                return;
            }
            if self.clock.now().duration_since(start) >= wait {
                return;
            }
            self.rest();
        }
    }
}

/// Fires one channel send when the watched cancel fires, bridging a
/// cancel token to the channel an `exec` run watches.
struct CancelFire {
    /// The send, taken by the first wake.
    stop: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl support::clock::Wake for CancelFire {
    fn wake(&self) {
        if let Some(stop) = support::lock(&self.stop).take() {
            match stop.send(()) {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

impl ProcessHostServices for NativeHostServices {
    fn perform(
        &self,
        context: &ProcessContext,
        callback: &CallbackContext,
        operation: HostOperation,
        cancel: &dyn Cancel,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        match operation {
            HostOperation::Http {
                method,
                url,
                headers,
                body,
            } => self.http(&method, &url, headers, body, callback),
            HostOperation::Exec {
                program,
                args,
                cwd,
                env,
                input,
            } => self.exec(context, callback, &program, args, &cwd, env, input, cancel),
            HostOperation::FsRead { path } => self.fs_read(context, &path),
            HostOperation::FsWrite {
                path,
                contents,
                lock,
            } => self.fs_write(context, &path, contents, lock),
            HostOperation::FsList { path } => self.fs_list(context, &path),
            HostOperation::FsStat { path } => self.fs_stat(context, &path),
            HostOperation::FsMkdir { path, lock } => self.fs_mkdir(context, &path, lock),
            HostOperation::FsRemove { path, lock } => self.fs_remove(context, &path, lock),
            HostOperation::FsRename { from, to, lock } => self.fs_rename(context, &from, &to, lock),
            HostOperation::ConfigGet { key } => self.config_get(&key),
            HostOperation::ConfigSet { key, value, scope } => self.config_set(&key, value, scope),
            HostOperation::Secret { name } => self.secret(context, &name),
            HostOperation::DataDir { scope } => {
                Ok(Some(Value::String(self.data_dir(context, scope))))
            }
            HostOperation::Sha256 { data } => Ok(Some(Value::String(Self::sha256(&data)))),
            HostOperation::HmacSha256 { key, data } => self.hmac(&key, &data),
            HostOperation::Status { text } => {
                self.status(context, &text);
                Ok(None)
            }
            HostOperation::Widget { id, lines } => {
                self.widget(context, &id, lines);
                Ok(None)
            }
            HostOperation::Emit { data } => {
                self.emit(context, data);
                Ok(None)
            }
            HostOperation::Log { message } => {
                self.log(context, &message);
                Ok(None)
            }
            HostOperation::Tool { name, arguments } => {
                self.tool(context, callback, &name, arguments, cancel)
            }
            HostOperation::Model { request } => self.model(context, callback, request, cancel),
            HostOperation::Delegate {
                arguments,
                timeout,
                run,
            } => self.delegate(context, callback, arguments, timeout, run),
            HostOperation::OAuthCallback { port, path } => {
                self.oauth_callback(context, callback, port, path.as_deref(), cancel)
            }
            HostOperation::OAuthOpen { url } => self.oauth_open(&url),
            HostOperation::OAuthShow { url, code } => self.oauth_show(&url, &code),
            HostOperation::OAuthPkcs => Self::pkcs(),
            HostOperation::OAuthPoll {
                url,
                method,
                headers,
                body,
                pending,
                interval,
            } => self.oauth_poll(
                callback,
                &url,
                &method,
                &headers,
                body,
                pending.as_deref(),
                interval,
                cancel,
            ),
            HostOperation::OAuthRefresh { nested } => self.oauth_refresh(callback, &nested, cancel),
            HostOperation::StateGet { key } => Ok(self.state_get(&key)),
            HostOperation::StateKeys => Ok(Some(self.state_keys())),
            HostOperation::StateSet {
                key,
                value,
                on_fork,
            } => self.state_set(context, callback, &key, value, on_fork),
            HostOperation::StateUnset { key } => self.state_unset(context, callback, &key),
        }
    }
}
