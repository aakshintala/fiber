use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};

use contract::tool::Cancel;
use support::clock::Wake as _;

use crate::host::admission::CallbackContext;

use super::{
    ConfigScope, DataScope, HostOperation, NativeHostServices, ProcessContext, ProcessHostServices,
    ToolCall,
};

mod oauth;

impl NativeHostServices {
    /// Runs `host.http`: the request with the callback's remaining
    /// deadline plus the grace as its backstop, answering the status
    /// with the body's bytes, redacted exactly as the Lua half redacts.
    /// The body reads as a bytes position. A call with no deadline
    /// waits without a backstop, as a timer's does. With a script the
    /// reply comes from the case alone, exactly as the Lua half consults
    /// it: a scripted miss never falls through to native I/O.
    fn http(
        &self,
        method: &str,
        url: &str,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
        callback: &CallbackContext,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let request = crate::host::HttpRequest {
            method: method.to_owned(),
            url: url.to_owned(),
            headers,
            body,
            timeout: None,
        };
        if let Some(script) = &self.host_script {
            // The script answers alone, as in Lua: no native I/O follows.
            let (status, bytes) = script.http(request.case_value())?;
            return Ok(Some(Self::http_answer(status, bytes)));
        }
        // The grace the Lua scheduler adds to the declared timeout.
        const GRACE: Duration = Duration::from_secs(1);
        let timeout = callback
            .deadline
            .map(|end| end.saturating_duration_since(self.clock.now()) + GRACE);
        let request = crate::host::HttpRequest { timeout, ..request };
        let (status, bytes) = crate::host::perform(&request)?;
        Ok(Some(Self::http_answer(status, bytes)))
    }

    /// Shapes a `host.http` answer: the status with the body's bytes, the
    /// body reading as a bytes position.
    fn http_answer(status: u16, bytes: Vec<u8>) -> Value {
        let mut answer = Map::new();
        answer.insert("status".to_owned(), Value::from(status));
        answer.insert(
            "body".to_owned(),
            Value::String(crate::host::admission::encode_bytes(&bytes)),
        );
        Value::Object(answer)
    }

    /// Runs `host.exec`: the child in its group, stopped when the
    /// request ends, logging `extension_exec` outside a tool request.
    /// The run's bytes read as bytes positions. A refused start writes
    /// no line; a stop ends the group without stopping anything else.
    /// With a script the reply comes from the case alone, exactly as the
    /// Lua half consults it: a scripted miss never falls through to
    /// native I/O, and a scripted hang parks on the injected clock until
    /// the callback's deadline, as a real run does.
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
        if let Some(script) = &self.host_script {
            // The script answers alone, as in Lua: no native I/O follows.
            let request = crate::host::exec::ExecRequest {
                program: program.to_owned(),
                args: args.clone(),
                cwd: cwd.clone(),
                env,
                input,
                cap: crate::host::fs::PROCESS_READ_CAP,
                own_group: true,
                #[cfg(test)]
                test_events: None,
            };
            let case_value = request.case_value();
            let (index, outcome) = script.exec(case_value.clone());
            return match outcome {
                Err(failed) => Err(failed),
                Ok(crate::host::script::ExecReply::Completed {
                    end,
                    stdout,
                    stderr,
                }) => {
                    let (exit_code, signal) = match end {
                        crate::host::script::ExecEnd::Code(code) => (Some(code), None),
                        crate::host::script::ExecEnd::Signal(number) => {
                            (None, Some(support::group::signal_name(number)))
                        }
                    };
                    Ok(Some(Self::exec_answer(
                        exit_code,
                        signal,
                        stdout.into_bytes(),
                        stderr.into_bytes(),
                    )))
                }
                Ok(crate::host::script::ExecReply::Hang) => match callback.deadline {
                    // A process that never exits: the worker parks on the
                    // case clock until the callback's deadline, as a real
                    // run parks on the injected clock, then the timeout
                    // answers. A hang without a deadline has no bound, as
                    // in Lua.
                    Some(until) => {
                        script.hang_matched(index, &case_value, until);
                        script.park_hang(self.clock.as_ref(), until, &stopped);
                        Err((
                            contract::ErrorCode::Timeout,
                            format!("host.exec: {program}: the command timed out"),
                        ))
                    }
                    None => Err((
                        contract::ErrorCode::IoFailed,
                        "host.exec: a scripted hang needs a callback deadline".to_owned(),
                    )),
                },
            };
        }
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
                test_events: None,
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
        Ok(Some(Self::exec_answer(
            ran.exit_code,
            ran.signal,
            ran.stdout,
            ran.stderr,
        )))
    }

    /// Shapes a `host.exec` answer: the exit code or signal with the run's
    /// bytes, each stream reading as a bytes position.
    fn exec_answer(
        exit_code: Option<i32>,
        signal: Option<String>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    ) -> Value {
        let mut answer = Map::new();
        if let Some(code) = exit_code {
            answer.insert("exit_code".to_owned(), Value::from(code));
        }
        if let Some(signal) = signal {
            answer.insert("signal".to_owned(), Value::String(signal));
        }
        answer.insert(
            "stdout".to_owned(),
            Value::String(crate::host::admission::encode_bytes(&stdout)),
        );
        answer.insert(
            "stderr".to_owned(),
            Value::String(crate::host::admission::encode_bytes(&stderr)),
        );
        Value::Object(answer)
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
        let runner = support::lock(&self.tool_runner).clone();
        let Some(runner) = runner else {
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
        let model = support::lock(&self.model).clone();
        let Some(model) = model else {
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
    /// caller owns the chunk-local `run` handle's end: this step starts
    /// the job and returns its id, leaving end routing to the caller.
    fn delegate(
        &self,
        context: &ProcessContext,
        callback: &CallbackContext,
        arguments: Map<String, Value>,
        timeout: Duration,
        _run: bool,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let delegate = support::lock(&self.delegate).clone();
        let Some(delegate) = delegate else {
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

    /// Reads `host.config.get`: the session's merged settings for the
    /// calling extension. Without a session there is no config to read,
    /// so the call fails naming the missing session, exactly as the Lua
    /// half fails without one.
    fn config_get(
        &self,
        context: &ProcessContext,
        key: &str,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let session = support::lock(&self.session);
        let Some(session) = session.as_ref() else {
            return Err((
                contract::ErrorCode::InvalidArguments,
                "host.config.get: this extension has no session".to_owned(),
            ));
        };
        let repo: Vec<&str> = session.repo_settings.iter().map(String::as_str).collect();
        match session
            .config
            .extensions()
            .get(&context.extension, &repo, key)
        {
            Ok(value) => Ok(value),
            Err(error) => Err(Self::coded("get", key, error)),
        }
    }

    /// Writes `host.config.set`: one key in the calling extension's
    /// settings, in the named scope. Without a session there is nothing
    /// to write, so the call fails naming the missing session.
    fn config_set(
        &self,
        context: &ProcessContext,
        key: &str,
        value: Value,
        scope: ConfigScope,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let scope = match scope {
            ConfigScope::Machine => config::Scope::Machine,
            ConfigScope::Project => config::Scope::Project,
        };
        let mut session = support::lock(&self.session);
        let Some(session) = session.as_mut() else {
            return Err((
                contract::ErrorCode::InvalidArguments,
                "host.config.set: this extension has no session".to_owned(),
            ));
        };
        match session
            .config
            .extensions_mut()
            .set(&context.extension, scope, key, value)
        {
            Ok(()) => Ok(None),
            Err(error) => Err(Self::coded("set", key, error)),
        }
    }

    /// A settings failure as the Lua half raises it: an unparseable key
    /// or one the layer may not take is an error in the calling code, and
    /// anything the files did is the registry's own coded failure.
    fn coded(op: &str, key: &str, error: config::ConfigError) -> (contract::ErrorCode, String) {
        use config::ConfigError;
        if matches!(error, ConfigError::Override { .. }) {
            (
                contract::ErrorCode::InvalidArguments,
                format!("host.config.{op}: `{key}` is not a dotted key"),
            )
        } else if matches!(error, ConfigError::Refused { .. }) {
            (
                contract::ErrorCode::InvalidArguments,
                format!("host.config.{op}: {error}"),
            )
        } else {
            (error.code(), format!("host.config.{op}: {error}"))
        }
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
    /// data directories and the process read bound, holding the session's
    /// path locks for locked calls when a session is wired.
    fn fs(&self, context: &ProcessContext) -> crate::host::fs::Fs {
        let session = support::lock(&self.session);
        let locks = session.as_ref().map(|session| Arc::clone(&session.locks));
        drop(session);
        match locks {
            Some(locks) => crate::host::fs::Fs::for_process_with_locks(
                context.workspace.clone(),
                context.data_dirs.machine.clone(),
                context.data_dirs.project.clone(),
                locks,
            ),
            None => crate::host::fs::Fs::for_process(
                context.workspace.clone(),
                context.data_dirs.machine.clone(),
                context.data_dirs.project.clone(),
            ),
        }
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
            HostOperation::ConfigGet { key } => self.config_get(context, &key),
            HostOperation::ConfigSet { key, value, scope } => {
                self.config_set(context, &key, value, scope)
            }
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
