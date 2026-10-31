//! One Lua 5.4 VM (`docs/extensions.md`, "Lua extensions"): the entry
//! script, one coroutine per callback, and the host calls each yields to.
//! Only the extension's thread touches the VM.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::diag::DebugLog;
use mlua::{FromLua, Function, Lua, LuaOptions, MultiValue, StdLib, Table, Thread};
use serde_json::Value;

use crate::{Error, host};

use super::hub::CallbackTimeouts;
use super::{ENTRY, GRACE, LOAD_TIMEOUT, Target, timeout_ms};
use super::{hub, schedule, setup};
use crate::lua::{Deadline, DeclaredHooks, Hub};

pub(super) struct Vm {
    pub(super) state: Rc<host::state::VmState>,
    pub(super) lua: Lua,
    name: String,
    pub(super) deadline: Deadline,
    clock: Arc<dyn Clock>,
    /// The session's workspace, resolving an `exec` `cwd` as `host.fs`
    /// paths are.
    workspace: PathBuf,
    /// The installed diagnostic sink when the VM started: snapshotted
    /// from the hub before the entry script runs, so the entry's own
    /// `host.http` still observes when the sink arrived with the session
    /// load. `None` on test-only loads with no sink.
    diagnostics: Option<Arc<dyn DebugLog>>,
    /// The host calls the case runner scripts, absent in ordinary sessions
    /// (`docs/testing.md`, "Testing an extension").
    host_script: Option<Arc<crate::host::script::HostScript>>,
    /// The extension's memory cap in bytes, bounding each `exec` stream.
    memory_cap: usize,
    dir: PathBuf,
    loading: Rc<RefCell<Option<String>>>,
    entry: Rc<Cell<bool>>,
    violations: Table,
    /// What the host calls yield, so a callback's own yield is not a request.
    http_tag: mlua::Value,
    /// What `fiber.command` registered: each name's `timeout` and `run`.
    commands: Table,
    /// What `fiber.provider` registered: each provider's functions, each
    /// with its `timeout` and `run`.
    providers: Table,
    /// What `fiber.hook` registered: each point's hooks in order, each with
    /// its `phase`, `on_failure`, `timeout` and `run`.
    hooks: Table,
    /// What `fiber.watch` registered: each watcher's `kinds`, `timeout`
    /// and `run`, in registration order.
    watchers: Table,
    /// Why each hook, command or tool the entry script tried to register
    /// was refused.
    problems: Table,
    /// What `fiber.tool` registered.
    tools: super::tool::Registry,
    /// What `fiber.search_backend` registered.
    search: super::search::Registry,
    /// The last raised failure, consumed only if its text escapes the callback.
    failures: crate::host::failure::FailureState,
    /// The timers `host.after` and `host.every` set, by id in set order:
    /// each firing runs the function stored here.
    pub(super) timer_funcs: Table,
    /// The `host.delegate` chunk's `handles` table: each started
    /// delegate's `{ timeout, run }` under its handle, so the end's
    /// stream item can reach the stored `run`. Cleared per handle when
    /// the run's item ends.
    delegate_handles: Table,
}

/// One step of a callback: it returned, or it suspended on a host call.
#[allow(
    clippy::large_enum_variant,
    reason = "a suspend carries the call's target, which names its credential pair; boxing would allocate on every host call"
)]
pub(super) enum Step {
    Done(Value),
    Suspend {
        thread: Thread,
        deadline: Option<Instant>,
        timeout: Duration,
        target: Target,
        request: host::Request,
    },
}

impl Vm {
    /// Creates the VM and runs the entry script under the deadline
    /// `load_by`, which started when Fiber first asked, so creating the VM
    /// and reading the script count.
    pub(super) fn load(hub: &Arc<Hub>, start: &schedule::Start) -> Result<Self, Error> {
        let clock = hub.clock_handle();
        let schedule::Start {
            name,
            dir,
            home,
            run_entry,
            load_by,
            memory_cap,
            browser,
            session,
            secrets,
            deps,
        } = start;
        let deadline =
            Deadline::new(Arc::clone(&clock)).with_interrupt(Arc::clone(&hub.lock().interrupt));
        deadline.restore(*load_by);
        let fail = |message: String| Error::Lua {
            extension: name.to_owned(),
            message,
        };
        let lua_error = |e: mlua::Error| fail(setup::message(&e));
        let dir = dir.canonicalize().map_err(|source| Error::Io {
            path: dir.to_owned(),
            source,
        })?;
        let lua = Lua::new_with(
            // table, string, math, utf8 and coroutine: Lua's safe set, which
            // leaves out debug, less its I/O and module loader.
            StdLib::ALL_SAFE & !StdLib::IO & !StdLib::OS & !StdLib::PACKAGE,
            // A host function's panic is never a Lua error a `pcall` can
            // catch (`docs/code-quality.md`, "Panics").
            LuaOptions::new().catch_rust_panics(false),
        )
        .map_err(lua_error)?;
        lua.set_memory_limit(*memory_cap).map_err(lua_error)?;
        let loading = Rc::new(RefCell::new(Some(ENTRY.to_owned())));
        let (commands, providers, hooks, watchers, problems, violations, failures) =
            setup::install(
                &lua,
                &deadline,
                dir.clone(),
                *memory_cap,
                Rc::clone(&loading),
                deps.clone(),
            )
            .map_err(lua_error)?;
        let failure_state = failures.state.clone();
        let workspace = session
            .as_ref()
            .map(|session| session.config.extensions().workspace().to_path_buf())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let entry = Rc::new(Cell::new(true));
        let init = Rc::new(Cell::new(true));
        // A registration refused during a load fails it even when Lua catches the raise.
        let registration_violations = violations.clone();
        let registration_loading = Rc::clone(&loading);
        let note_violation = lua
            .create_function(move |_, message: String| {
                if let Some(file) = registration_loading.borrow().as_ref() {
                    registration_violations.raw_push(format!("{file}: {message}"))?;
                }
                Ok(())
            })
            .map_err(lua_error)?;
        let tools = super::tool::install(
            &lua,
            problems.clone(),
            Rc::clone(&init),
            note_violation.clone(),
        )
        .map_err(lua_error)?;
        let search =
            super::search::install(&lua, problems.clone(), Rc::clone(&init), note_violation)
                .map_err(lua_error)?;
        let state = Rc::new(host::state::VmState {
            store: hub.lock().state.clone(),
            scope: Cell::new(host::state::Scope::Other),
        });
        host::state::install(&lua, state.clone(), &failures.failure).map_err(lua_error)?;
        let (http_tag, timer_funcs, delegate_handles) = host::install(
            &lua,
            host::HostContext {
                home: home.to_owned(),
                workspace: workspace.clone(),
                extension: name.to_owned(),
                session: session.clone(),
                memory_cap: *memory_cap,
                secrets: secrets.clone(),
            },
            Arc::clone(browser),
            Rc::clone(&entry),
            hub,
            failures.failure,
            failures.note_failure,
        )
        .map_err(lua_error)?;
        let vm = Self {
            state,
            lua,
            name: name.to_owned(),
            deadline,
            clock,
            diagnostics: hub.diagnostics(),
            workspace,
            host_script: hub.host_script(),
            memory_cap: *memory_cap,
            dir: dir.clone(),
            loading: Rc::clone(&loading),
            entry: Rc::clone(&entry),
            violations: violations.clone(),
            http_tag,
            commands,
            providers,
            hooks,
            watchers,
            problems,
            tools,
            search,
            failures: failure_state,
            timer_funcs,
            delegate_handles,
        };

        // A provider-only VM finishes the common setup with entry
        // registration closed and skips the entry script: its provider
        // scripts still load on demand through `load_provider`.
        if !run_entry || !dir.join(ENTRY).is_file() {
            entry.set(false);
            init.set(false);
            *loading.borrow_mut() = None;
            return Ok(vm);
        }
        let entry_fn = match setup::load_file(&vm.lua, &dir, ENTRY, *memory_cap) {
            Ok(Ok(entry_fn)) => entry_fn,
            Ok(Err(message)) => return Err(fail(message)),
            Err(e) => return Err(lua_error(e)),
        };
        let ran = vm.resume(entry_fn, ENTRY, LOAD_TIMEOUT, mlua::Value::Nil);
        entry.set(false);
        init.set(false);
        *loading.borrow_mut() = None;
        ran?;
        if let Some(message) = violations.sequence_values::<String>().next() {
            return Err(fail(message.map_err(lua_error)?));
        }
        Ok(vm)
    }

    pub(super) fn load_provider(
        &self,
        name: &str,
        deadline: Option<Instant>,
    ) -> Result<BTreeMap<String, Duration>, Error> {
        let file = format!("providers/{name}.lua");
        self.deadline.restore(deadline);
        *self.loading.borrow_mut() = Some(file.clone());
        self.entry.set(true);
        let result = (|| {
            let f = setup::load_file(&self.lua, &self.dir, &file, self.memory_cap)
                .map_err(|e| self.error(&e))?
                .map_err(|message| Error::Lua {
                    extension: self.name.clone(),
                    message,
                })?;
            self.resume(f, &file, LOAD_TIMEOUT, mlua::Value::Nil)?;
            if let Some(message) = self.violations.sequence_values::<String>().next() {
                return Err(Error::Lua {
                    extension: self.name.clone(),
                    message: message.map_err(|e| self.error(&e))?,
                });
            }
            if self
                .providers
                .get::<Option<Table>>(name)
                .map_err(|e| self.error(&e))?
                .is_none()
            {
                return Err(Error::Lua {
                    extension: self.name.clone(),
                    message: format!("{file}: registers no provider `{name}`"),
                });
            }
            Ok(self.declared().providers.remove(name).unwrap_or_default())
        })();
        self.entry.set(false);
        *self.loading.borrow_mut() = None;
        match self.violations.clear() {
            Ok(()) | Err(_) => {}
        }
        if result.is_err() {
            match self.providers.raw_remove(name) {
                Ok(()) | Err(_) => {}
            }
        }
        result
    }

    /// Starts `target` and runs it until it returns or suspends on
    /// `host.http`. `deadline` is its timeout from when Fiber asked, so time
    /// already spent waiting counts.
    pub(super) fn step(
        &self,
        target: &Target,
        arg: &Value,
        timeout: Duration,
        deadline: Option<Instant>,
    ) -> Result<Step, Error> {
        let fail = |e: mlua::Error| self.error(&e);
        let run = match target {
            Target::Timer { id } => self
                .timer_funcs
                .get::<Option<Function>>(i64::try_from(*id).unwrap_or(i64::MAX)),
            Target::Command(command) => self
                .commands
                .get::<Option<Table>>(command.as_str())
                .and_then(|table| {
                    table.map_or(Ok(None), |table| table.get::<Option<Function>>("run"))
                }),
            Target::Provider { name, function, .. } => self
                .providers
                .get::<Option<Table>>(name.as_str())
                .and_then(|table| {
                    table.map_or(Ok(None), |table| {
                        table.get::<Option<Table>>(*function).and_then(|table| {
                            table.map_or(Ok(None), |table| table.get::<Option<Function>>("run"))
                        })
                    })
                }),
            Target::Hook { point, index } => self
                .hooks
                .get::<Option<Table>>(point.as_str())
                .and_then(|list| {
                    list.map_or(Ok(None), |list| {
                        list.get::<Option<Table>>(index.saturating_add(1))
                            .and_then(|spec| {
                                spec.map_or(Ok(None), |spec| spec.get::<Option<Function>>("run"))
                            })
                    })
                }),
            Target::Watcher { index } => self
                .watchers
                .get::<Option<Table>>(index.saturating_add(1))
                .and_then(|spec| spec.map_or(Ok(None), |spec| spec.get::<Option<Function>>("run"))),
            Target::Tool(name) => self.tools.function(name, "run"),
            Target::Search(name) => self.search.function(name),
            Target::Effects(name) => self.tools.function(name, "effects"),
            // The end's item reaches the `run` the start stored under its
            // handle: a Lua function value crosses into Rust only through
            // the VM that owns it, so the store stays in Lua and Rust
            // carries plain data.
            Target::Delegate { handle, .. } => self
                .delegate_handles
                .get::<Option<Table>>(*handle)
                .and_then(|table| {
                    table.map_or(Ok(None), |table| table.get::<Option<Function>>("run"))
                }),
        }
        .map_err(fail)?;
        let Some(run) = run else {
            return Err(hub::not_registered(&self.name, target));
        };
        let lua_arg = match target {
            Target::Command(_) => arg
                .as_str()
                .map(|text| self.lua.create_string(text))
                .transpose()
                .map_err(fail)?
                .map_or(mlua::Value::Nil, mlua::Value::String),
            Target::Provider { .. }
            | Target::Hook { .. }
            | Target::Watcher { .. }
            | Target::Tool(_)
            | Target::Search(_)
            | Target::Delegate { .. }
            | Target::Effects(_) => host::to_lua(&self.lua, arg).map_err(fail)?,
            // A firing takes no argument.
            Target::Timer { .. } => mlua::Value::Nil,
        };
        let thread = self.lua.create_thread(run).map_err(fail)?;
        self.deadline.arm(&thread).map_err(fail)?;
        self.after(
            thread,
            MultiValue::from_vec(vec![lua_arg]),
            target,
            timeout,
            deadline,
        )
    }

    /// Resumes `thread` until it returns or suspends on a host call again.
    pub(super) fn after(
        &self,
        thread: Thread,
        args: MultiValue,
        target: &Target,
        timeout: Duration,
        deadline: Option<Instant>,
    ) -> Result<Step, Error> {
        match self.poll(&thread, args, &target.to_string(), timeout, deadline)? {
            setup::Poll::Done(value) => Ok(Step::Done(self.returned(target, value)?)),
            setup::Poll::Host(request) => Ok(Step::Suspend {
                thread,
                deadline,
                timeout,
                target: target.clone(),
                request,
            }),
        }
    }

    /// Every callback the entry script registered.
    pub(super) fn declared(&self) -> CallbackTimeouts {
        let mut timeouts = CallbackTimeouts::default();
        for (name, spec) in self.commands.pairs::<String, Table>().flatten() {
            if let Ok(ms) = spec.get::<u64>("timeout") {
                let description = spec.get::<String>("description").unwrap_or_default();
                timeouts.commands.insert(
                    name,
                    super::declared::DeclaredCommand {
                        timeout: Duration::from_millis(ms),
                        description,
                    },
                );
            }
        }
        for (name, functions) in self.providers.pairs::<String, Table>().flatten() {
            let mut fns = BTreeMap::new();
            for (function, spec) in functions.pairs::<String, Table>().flatten() {
                if let Ok(ms) = spec.get::<u64>("timeout") {
                    fns.insert(function, Duration::from_millis(ms));
                }
            }
            timeouts.providers.insert(name, fns);
        }
        timeouts.hooks = DeclaredHooks::read(&self.hooks, &self.problems);
        timeouts.watchers = super::declared::DeclaredWatchers::read(&self.watchers, &self.problems);
        // A watch refusal is the watchers' own problem: the hook path
        // never reports it twice.
        timeouts
            .hooks
            .problems
            .retain(|problem| !super::declared::is_watch_problem(problem));
        timeouts.tools = self.tools.declared();
        timeouts.search = self.search.declared();
        timeouts
    }

    pub(super) fn returned(&self, target: &Target, value: mlua::Value) -> Result<Value, Error> {
        let fail = |e: mlua::Error| self.error(&e);
        match target {
            // A command reports through `host.status`, `host.widget`, `host.ask`
            // or `host.emit`; its return value is ignored. A string is kept,
            // anything else is `""`, so existing callers keep their returns.
            // The conversion never fails: a table, boolean or function is
            // `""`, never a failure notice for a successful command.
            Target::Command(_) => Ok(Value::String(
                Option::<String>::from_lua(value, &self.lua)
                    .unwrap_or_default()
                    .unwrap_or_default(),
            )),
            Target::Provider { function, .. } if *function == "credential" => {
                // A numeric `headers` key would not survive JSON conversion:
                // `to_json` turns `{[42] = "v"}` into `{"42": "v"}` and
                // `{[1] = "v"}` into an array, hiding the shape. Checked
                // here, before conversion (`docs/extensions.md`, "What
                // writing a provider looks like").
                if !credential_headers_are_strings(&value) {
                    return Err(Error::BadReturn {
                        extension: self.name.clone(),
                        callback: target.to_string(),
                        why: "`headers` is not a table of header names to values".to_owned(),
                    });
                }
                host::to_json(&value).map_err(fail)
            }
            Target::Provider { .. }
            | Target::Hook { .. }
            | Target::Timer { .. }
            | Target::Tool(_)
            | Target::Effects(_) => host::to_json(&value).map_err(fail),
            // A watcher changes nothing: its return value is ignored.
            Target::Watcher { .. } => Ok(Value::Null),
            // A delegate `run` changes nothing by returning: its work is
            // a side effect, so its return value is ignored, like a
            // watcher's. A value that would fail conversion never becomes
            // a spurious failure notice for a successful run.
            Target::Delegate { .. } => Ok(Value::Null),
            Target::Search(name) => {
                // A field such as `title = function() end` would fail a
                // bare JSON conversion with no backend, extension or
                // position, so the shape is checked on the Lua value first.
                match super::search::results(&value) {
                    Ok(checked) => Ok(checked),
                    Err(why) => Err(Error::BadReturn {
                        extension: self.name.clone(),
                        callback: name.clone(),
                        why,
                    }),
                }
            }
        }
    }

    pub(super) fn error(&self, e: &mlua::Error) -> Error {
        if let Some(failed) = self.failures.take_matching(e) {
            return pending_error(&self.name, failed);
        }
        Error::Lua {
            extension: self.name.clone(),
            message: setup::message(e),
        }
    }

    fn timed_out(&self, callback: &str, timeout: Duration) -> Error {
        Error::Timeout {
            extension: self.name.clone(),
            callback: callback.to_owned(),
            timeout_ms: timeout_ms(timeout),
        }
    }

    /// Runs `f` as a coroutine armed with the deadline already started. A
    /// callback that ends past its deadline, by an error or by catching one,
    /// has timed out. `host.http` runs here, on this thread: this is the
    /// entry script, before any other callback exists.
    pub(super) fn resume(
        &self,
        f: Function,
        callback: &str,
        timeout: Duration,
        arg: mlua::Value,
    ) -> Result<mlua::Value, Error> {
        let fail = |e: mlua::Error| self.error(&e);
        let thread = self.lua.create_thread(f).map_err(fail)?;
        self.deadline.arm(&thread).map_err(fail)?;
        let mut args = MultiValue::from_vec(vec![arg]);
        loop {
            match self.poll(
                &thread,
                std::mem::take(&mut args),
                callback,
                timeout,
                self.deadline.at(),
            )? {
                setup::Poll::Done(value) => return Ok(value),
                setup::Poll::Host(host::Request::Http(request)) => {
                    let result = match &self.host_script {
                        Some(script) => script.http(request.case_value()),
                        // A real entry request outside a provider function
                        // is the extension's own: it observes with the
                        // sink the VM snapshotted before entry ran.
                        None => match &self.diagnostics {
                            Some(debug) if debug.debugging() => {
                                let observation = host::HttpObservation {
                                    attribution: host::HttpAttribution::Extension {
                                        extension: self.name.clone(),
                                    },
                                    debug: Arc::clone(debug),
                                    clock: Arc::clone(&self.clock),
                                };
                                host::perform_observed(&request, observation)
                            }
                            _ => host::perform(&request),
                        },
                    };
                    args = host::resume_values(&self.lua, &self.clock, host::Reply::Http(result))
                        .map_err(fail)?;
                }
                // The Lua half refuses these in the entry script before it yields.
                setup::Poll::Host(
                    host::Request::Callback { .. }
                    | host::Request::Lock
                    | host::Request::Sleep(_)
                    | host::Request::Drive(_)
                    | host::Request::Ask(_)
                    | host::Request::Tool(_)
                    | host::Request::Model(_)
                    | host::Request::Delegate { .. }
                    | host::Request::Exec(_),
                ) => return Err(self.yielded()),
            }
        }
    }

    /// Resumes `thread` once. A yield of [`Vm::http_tag`] is one host call.
    fn poll(
        &self,
        thread: &Thread,
        args: MultiValue,
        callback: &str,
        timeout: Duration,
        deadline: Option<Instant>,
    ) -> Result<setup::Poll, Error> {
        self.deadline.restore(deadline);
        if self.deadline.passed() {
            return Err(self.timed_out(callback, timeout));
        }
        let values = match thread.resume::<MultiValue>(args) {
            Ok(values) => values,
            Err(e) => {
                if self.deadline.passed() {
                    return Err(self.timed_out(callback, timeout));
                }
                return Err(self.error(&e));
            }
        };
        if self.deadline.passed() {
            return Err(self.timed_out(callback, timeout));
        }
        if !thread.is_resumable() {
            return Ok(setup::Poll::Done(
                values
                    .into_vec()
                    .into_iter()
                    .next()
                    .unwrap_or(mlua::Value::Nil),
            ));
        }
        // The real-time backstop is the callback's declared timeout plus the
        // grace, fixed when the request is made. The entry script's is the
        // load bound plus the grace. It reads no clock, and it is never
        // shorter than the time the caller can still wait. The parked
        // callback's deadline stays with the scheduler, on the injected clock.
        let bound = timeout.saturating_add(GRACE);
        Ok(setup::Poll::Host(self.host_request(&values, Some(bound))?))
    }

    /// The host call a coroutine yielded: the tag, a kind and its argument.
    fn host_request(
        &self,
        values: &MultiValue,
        timeout: Option<Duration>,
    ) -> Result<host::Request, Error> {
        let mut yielded = values.iter();
        let (Some(tag), Some(mlua::Value::String(kind))) = (yielded.next(), yielded.next()) else {
            return Err(self.yielded());
        };
        if !tag.equals(&self.http_tag).map_err(|e| self.error(&e))? {
            return Err(self.yielded());
        }
        let kind = kind.to_str().map_err(|e| self.error(&e))?;
        let request = host::request_from(
            &kind,
            yielded.next(),
            timeout,
            &self.workspace,
            self.memory_cap,
        )
        .map_err(|e| self.error(&e))?
        .ok_or_else(|| self.yielded())?;
        // A `/`-relative program resolves against the extension's own
        // directory and must stay inside it. Under a case runner's host
        // script the request keeps the program as written, so a case
        // matches what the Lua wrote; an ordinary session runs the
        // resolved path.
        match request {
            host::Request::Exec(mut req) => {
                match host::exec::resolve_in_package(&req.program, &self.dir) {
                    Ok(path) => {
                        if self.host_script.is_none() {
                            req.program = path;
                        }
                    }
                    Err(text) => return Err(self.error(&mlua::Error::RuntimeError(text))),
                }
                Ok(host::Request::Exec(req))
            }
            host::Request::Http(_)
            | host::Request::Ask(_)
            | host::Request::Tool(_)
            | host::Request::Drive(_)
            | host::Request::Model(_)
            | host::Request::Delegate { .. }
            | host::Request::Callback { .. }
            | host::Request::Lock
            | host::Request::Sleep(_) => Ok(request),
        }
    }

    /// Clears the stored `{ timeout, run }` pair for `handle`: the end's
    /// stream item consumed it, so a later completion for the same id
    /// pushes nothing and a reloaded VM never reaches the old `run`.
    pub(super) fn clear_delegate_handle(&self, handle: u64) {
        // Setting `nil` removes the key; a missing handle is already
        // cleared, so any outcome is ignored.
        match self.delegate_handles.set(handle, mlua::Value::Nil) {
            Ok(()) | Err(_) => {}
        }
    }

    /// A full garbage collection, so a lock handle left in a coroutine the
    /// host dropped is freed now rather than at some later cycle.
    pub(super) fn collect(&self) {
        for _ in 0..2 {
            match self.lua.gc_collect() {
                Ok(()) | Err(_) => {}
            }
        }
    }

    fn yielded(&self) -> Error {
        Error::Lua {
            extension: self.name.clone(),
            message: "a callback yielded to the host".to_owned(),
        }
    }
}

/// Whether a `credential()` return's `headers`, when present, is a table
/// whose every name and value is a string. A return that is not a table is
/// another check's problem: `fetch_token` reports the missing `token`.
/// Absent and null mean no headers, as an empty table does for `sign()`
/// (`docs/extensions.md`, "What writing a provider looks like").
pub(super) fn credential_headers_are_strings(value: &mlua::Value) -> bool {
    let mlua::Value::Table(returned) = value else {
        return true;
    };
    let Ok(headers) = returned.get::<mlua::Value>("headers") else {
        return false;
    };
    match headers {
        mlua::Value::Nil => true,
        mlua::Value::Table(headers) => headers
            .pairs::<mlua::Value, mlua::Value>()
            .all(|pair| matches!(pair, Ok((mlua::Value::String(_), mlua::Value::String(_))))),
        headers if headers.is_null() => true,
        mlua::Value::Boolean(_)
        | mlua::Value::LightUserData(_)
        | mlua::Value::Integer(_)
        | mlua::Value::Number(_)
        | mlua::Value::String(_)
        | mlua::Value::Function(_)
        | mlua::Value::Thread(_)
        | mlua::Value::UserData(_)
        | mlua::Value::Error(_)
        | mlua::Value::Other(_) => false,
    }
}

/// The error a recorded failure fails its callback with.
fn pending_error(extension: &str, failure: crate::host::failure::PendingFailure) -> Error {
    use crate::host::failure::Boundary;

    let extension = extension.to_owned();
    match failure.boundary {
        Boundary::Unattended { call } => Error::Unattended { extension, call },
        Boundary::Refresh { reached: true } => Error::RefreshRejected {
            extension,
            message: failure.message,
        },
        Boundary::Refresh { reached: false } => Error::RefreshUnreachable {
            extension,
            message: failure.message,
        },
    }
}

#[cfg(test)]
#[path = "vm_tests.rs"]
mod tests;
