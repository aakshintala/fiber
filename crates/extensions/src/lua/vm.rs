//! One Lua 5.4 VM (`docs/extensions.md`, "Lua extensions"): the entry
//! script, one coroutine per callback, and the host calls each yields to.
//! Only the extension's thread touches the VM.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use mlua::{FromLua, Function, Lua, LuaOptions, MultiValue, StdLib, Table, Thread};
use serde_json::Value;

use crate::{Error, host};

use super::hub::CallbackTimeouts;
use super::{ENTRY, GRACE, LOAD_TIMEOUT, Target, timeout_ms};
use super::{hub, schedule, setup};
use crate::lua::{Deadline, DeclaredHooks, Hub};

pub(super) struct Vm {
    pub(super) lua: Lua,
    name: String,
    pub(super) deadline: Deadline,
    clock: Arc<dyn Clock>,
    /// The session's workspace, resolving an `exec` `cwd` as `host.fs`
    /// paths are.
    workspace: PathBuf,
    /// The extension's memory cap in bytes, bounding each `exec` stream.
    memory_cap: usize,
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
    /// Why each hook `fiber.hook` refused was not registered.
    problems: Table,
    /// The timers `host.after` and `host.every` set, by id in set order:
    /// each firing runs the function stored here.
    pub(super) timer_funcs: Table,
}

/// One step of a callback: it returned, or it suspended on a host call.
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
            load_by,
            memory_cap,
            browser,
            session,
        } = start;
        let deadline = Deadline::new(Arc::clone(&clock));
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
        let (commands, providers, hooks, problems) =
            setup::install(&lua, &deadline, dir.clone(), *memory_cap).map_err(lua_error)?;
        let workspace = session
            .as_ref()
            .map(|session| session.config.workspace().to_path_buf())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let entry = Rc::new(Cell::new(true));
        let (http_tag, timer_funcs) = host::install(
            &lua,
            host::HostContext {
                home: home.to_owned(),
                workspace: workspace.clone(),
                extension: name.to_owned(),
                session: session.clone(),
                memory_cap: *memory_cap,
            },
            Arc::clone(browser),
            Rc::clone(&entry),
            hub,
        )
        .map_err(lua_error)?;
        let vm = Self {
            lua,
            name: name.to_owned(),
            deadline,
            clock,
            workspace,
            memory_cap: *memory_cap,
            http_tag,
            commands,
            providers,
            hooks,
            problems,
            timer_funcs,
        };

        let entry_fn = match setup::load_file(&vm.lua, &dir, ENTRY, *memory_cap) {
            Ok(Ok(entry_fn)) => entry_fn,
            Ok(Err(message)) => return Err(fail(message)),
            Err(e) => return Err(lua_error(e)),
        };
        let ran = vm.resume(entry_fn, ENTRY, LOAD_TIMEOUT, mlua::Value::Nil);
        entry.set(false);
        ran?;
        Ok(vm)
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
            Target::Provider { name, function } => self
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
            Target::Provider { .. } | Target::Hook { .. } => {
                host::to_lua(&self.lua, arg).map_err(fail)?
            }
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
                timeouts.commands.insert(name, Duration::from_millis(ms));
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
        timeouts
    }

    pub(super) fn returned(&self, target: &Target, value: mlua::Value) -> Result<Value, Error> {
        let fail = |e: mlua::Error| self.error(&e);
        match target {
            Target::Command(_) => Option::<String>::from_lua(value, &self.lua)
                .map(|text| Value::String(text.unwrap_or_default()))
                .map_err(fail),
            Target::Provider { .. } | Target::Hook { .. } | Target::Timer { .. } => {
                host::to_json(&value).map_err(fail)
            }
        }
    }

    pub(super) fn error(&self, e: &mlua::Error) -> Error {
        if let Some(failed) = e.downcast_ref::<crate::oauth::RefreshFailed>() {
            let (extension, message) = (self.name.clone(), failed.message.clone());
            return if failed.reached {
                Error::RefreshRejected { extension, message }
            } else {
                Error::RefreshUnreachable { extension, message }
            };
        }
        if let Some(unattended) = e.downcast_ref::<crate::oauth::Unattended>() {
            return Error::Unattended {
                extension: self.name.clone(),
                call: unattended.call.clone(),
            };
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
                    let reply = host::Reply::Http(host::perform(&request));
                    args = host::resume_values(&self.lua, &self.clock, reply).map_err(fail)?;
                }
                // The Lua half refuses these in the entry script before it yields.
                setup::Poll::Host(
                    host::Request::Callback { .. }
                    | host::Request::Lock
                    | host::Request::Sleep(_)
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
        host::request_from(
            &kind,
            yielded.next(),
            timeout,
            &self.workspace,
            self.memory_cap,
        )
        .map_err(|e| self.error(&e))?
        .ok_or_else(|| self.yielded())
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
