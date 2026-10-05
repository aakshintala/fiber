//! A Lua extension's runtime (`docs/extensions.md`, "Lua extensions", "How an
//! extension runs", "Loading, and cost when nothing is loaded" and "When an
//! extension misbehaves"): one Lua 5.4 VM on one thread per extension,
//! created on first use, with the stripped standard library, a `require`
//! held to the extension's directory, a deadline armed on every coroutine and
//! a memory cap. The host's globals are `host.rs`.
//!
//! The deadline has two guards. The instruction hook stops Lua code at the
//! deadline and leaves the VM usable. Code the hook never sees, such as a
//! `__gc` finalizer (Lua turns hooks off there) or a long C call such as a
//! backtracking `string.find`, is caught by the caller instead: once the
//! callback has started, it stops waiting a grace period past the deadline
//! and abandons the VM. What every caller waits on, and when, is `lua/hub.rs`.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use mlua::{FromLua, Function, Lua, LuaOptions, MultiValue, StdLib, Table, Thread};
use serde_json::Value;

use crate::oauth::{Browser, SystemBrowser};
use crate::{Error, host};

/// Each Lua extension's default memory cap (`docs/extensions.md`, "Loading,
/// and cost when nothing is loaded"). Past it, an allocation is a Lua error in
/// that extension's VM, and a file larger than it is not read.
pub const MEMORY_CAP: usize = 1 << 20;

/// The script Fiber runs when it creates the VM.
// docs/configuration.md, "An extension's manifest".
const ENTRY: &str = "init.lua";

/// How long reading, compiling and running the entry script may take. It is
/// not a callback, so it declares no timeout of its own.
// docs/extensions.md, "How an extension runs".
const LOAD_TIMEOUT: Duration = Duration::from_secs(2);

/// How long past a deadline the caller waits before it abandons the VM.
// docs/extensions.md, "How an extension runs".
const GRACE: Duration = Duration::from_secs(1);

/// Instructions between two looks at the clock while a deadline is ahead.
pub(super) const CHECK_EVERY: u32 = 1000;

/// The Lua half of the host's globals: `coroutine.wrap` over the armed
/// `coroutine.create`, `require`, `fiber.command`, `fiber.provider` and
/// `fiber.hook`, which fill the tables the prelude returns. Lua's own `wrap`
/// is a separate C function that would make an unarmed coroutine.
// A callback's timeout is in milliseconds (docs/extensions.md, "How an
// extension runs"). A hook's phase and `on_failure` are docs/extensions.md,
// "When several hooks share a point" and "When a hook fails".
pub(super) const PRELUDE: &str = r#"
local create, load_module = ...
local commands, providers, hooks, problems = {}, {}, {}, {}
local resume, pack, unpack = coroutine.resume, table.pack, table.unpack

coroutine.create = create
coroutine.wrap = function(f)
  local co = create(f)
  return function(...)
    local r = pack(resume(co, ...))
    if not r[1] then error(r[2], 0) end
    return unpack(r, 2, r.n)
  end
end

local loaded = {}
require = function(name)
  if loaded[name] == nil then
    local chunk, err = load_module(name)
    if not chunk then error(err, 2) end
    local value = chunk(name)
    if value == nil then value = true end
    loaded[name] = value
  end
  return loaded[name]
end

-- A callback's `{ timeout, run }`, checked; errors name the caller's line.
local function callback(what, spec)
  if type(spec) ~= "table" or math.type(spec.timeout) ~= "integer" or spec.timeout <= 0 then
    error(what .. ": `timeout` must be a whole number of milliseconds above 0", 3)
  end
  if type(spec.run) ~= "function" then
    error(what .. ": `run` must be a function", 3)
  end
  return { timeout = spec.timeout, run = spec.run }
end

local provider_functions = { models = true, quota = true, credential = true, sign = true }

local hook_points = {
  session_start = true, before_message = true, turn_start = true, before_tool = true,
  before_model_call = true, after_tool = true, turn_end = true, before_handoff = true,
}
local refusing = { before_message = true, before_tool = true, before_model_call = true }
local phases = { sanitize = true, transform = true, check = true }
local failures = { blocking = true, ["non-blocking"] = true }

-- A hook that does not register leaves a problem for Fiber's notice, and the
-- entry script goes on.
local function hook(point, spec)
  local name = tostring(point)
  local function refuse(why)
    problems[#problems + 1] = "`" .. name .. "` hook not registered: " .. why
  end
  if type(point) ~= "string" or not hook_points[point] then
    return refuse("`" .. name .. "` is not a hook point")
  end
  if type(spec) ~= "table" then
    return refuse("it takes a table of `timeout`, `on_failure` and `run`")
  end
  if spec.timeout == nil then return refuse("missing `timeout`") end
  if math.type(spec.timeout) ~= "integer" or spec.timeout <= 0 then
    return refuse("`timeout` must be a whole number of milliseconds above 0")
  end
  if spec.on_failure == nil then return refuse("missing `on_failure`") end
  if not failures[spec.on_failure] then
    return refuse("`on_failure` must be `blocking` or `non-blocking`")
  end
  local phase = spec.phase
  if phase == nil then phase = "transform" end
  if not phases[phase] then
    return refuse("`phase` must be `sanitize`, `transform` or `check`")
  end
  if phase == "check" and not refusing[point] then
    return refuse("a `check` hook exists only at `before_message`, `before_tool` and `before_model_call`")
  end
  if type(spec.run) ~= "function" then return refuse("`run` must be a function") end
  local list = hooks[point] or {}
  hooks[point] = list
  list[#list + 1] = { phase = phase, on_failure = spec.on_failure, timeout = spec.timeout, run = spec.run }
end

fiber = {
  command = function(name, spec)
    if type(name) ~= "string" then
      error("fiber.command: the name must be a string", 2)
    end
    commands[name] = callback("fiber.command", spec)
  end,
  provider = function(name, spec)
    if type(name) ~= "string" or type(spec) ~= "table" then
      error("fiber.provider: takes a name and a table of functions", 2)
    end
    local registered = {}
    for key, f in pairs(spec) do
      if not provider_functions[key] then
        error("fiber.provider: `" .. tostring(key) .. "` is not models, quota, credential or sign", 2)
      end
      registered[key] = callback("fiber.provider: `" .. key .. "`", f)
    end
    providers[name] = registered
  end,
  hook = hook,
}

return commands, providers, hooks, problems
"#;

/// A Lua extension in a session. Creating one runs no Lua: the VM and its
/// thread start on the first call.
pub struct LuaExtension {
    name: String,
    dir: PathBuf,
    home: PathBuf,
    memory_cap: usize,
    /// What `host.oauth.open` opens URLs with.
    browser: Arc<dyn Browser>,
    /// The state every caller and the extension's thread observe.
    hub: Arc<Hub>,
}

impl LuaExtension {
    /// The extension `name`, whose files are in `dir`, in the Fiber home
    /// `home` that `host.secret` reads. `clock` is the extension's clock:
    /// deadlines, the load bound and the grace all read it.
    pub fn new(
        name: impl Into<String>,
        dir: impl Into<PathBuf>,
        home: impl Into<PathBuf>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            name: name.into(),
            dir: dir.into(),
            home: home.into(),
            memory_cap: MEMORY_CAP,
            browser: Arc::new(SystemBrowser::default()),
            hub: Hub::new(clock),
        }
    }

    /// Sets this extension's memory cap in bytes.
    pub fn with_memory_cap(mut self, bytes: NonZeroUsize) -> Self {
        self.memory_cap = bytes.get();
        self
    }

    /// Sets the browser `host.oauth.open` uses.
    pub fn with_browser(mut self, browser: Arc<dyn Browser>) -> Self {
        self.browser = browser;
        self
    }

    /// The clock this extension's deadlines read.
    pub(crate) fn clock(&self) -> &dyn Clock {
        self.hub.clock()
    }

    /// The extension's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Fiber home, where this extension's secrets and model cache live.
    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    /// Whether the extension's VM and thread exist and take calls.
    pub fn is_running(&self) -> bool {
        matches!(
            self.hub.lock().phase,
            Phase::Registering { .. } | Phase::Ready(_)
        )
    }

    /// Runs the command `command` the extension registered with
    /// `fiber.command`, passing `text`, and returns what its `run` returned.
    /// The first call creates the VM and runs the entry script. A command
    /// runs until it returns, including a host call it waits on, before the
    /// next command starts. While it is suspended the thread runs provider
    /// work (`models`, `credential`, `sign`), not the next command.
    pub fn command(&self, command: &str, text: &str) -> Result<String, Error> {
        let value = self.call(
            Target::Command(command.to_owned()),
            Value::String(text.into()),
        )?;
        Ok(value.as_str().unwrap_or_default().to_owned())
    }

    /// The functions `fiber.provider` registered for `provider`, by name;
    /// empty when the extension registered no such provider.
    pub fn provider_functions(&self, provider: &str) -> Result<Vec<String>, Error> {
        self.registered(|timeouts| {
            timeouts
                .providers
                .get(provider)
                .map(|fns| fns.keys().cloned().collect())
                .unwrap_or_default()
        })
    }

    /// Every hook the entry script registered, and why each one it tried
    /// and could not register was refused. Starts the extension.
    pub(crate) fn hooks(&self) -> Result<DeclaredHooks, Error> {
        self.registered(|timeouts| timeouts.hooks.clone())
    }

    /// Runs the hook `index` registered at `point`, in registration order
    /// from 0, passing `arg`, and returns what its `run` returned as JSON.
    pub(crate) fn hook(&self, point: &str, index: usize, arg: Value) -> Result<Value, Error> {
        self.call(
            Target::Hook {
                point: point.to_owned(),
                index,
            },
            arg,
        )
    }

    /// Every hook this extension registers runs under `timeout` instead of
    /// the one it declared (`docs/configuration.md`,
    /// `extensions."<name>".hook_timeout_ms`). Set before the first call.
    pub(crate) fn override_hook_timeout(&self, timeout: Duration) {
        self.hub.lock().hook_timeout = Some(timeout);
    }

    /// What `read` makes of the entry script's registrations, once it has
    /// returned. Starts the extension.
    fn registered<T>(&self, read: impl Fn(&CallbackTimeouts) -> T) -> Result<T, Error> {
        let mut shared = self.hub.lock();
        self.start(&mut shared)?;
        loop {
            let now = self.hub.clock().now();
            let until = match shared.gate(&self.name, now) {
                Gate::Ready(timeouts) => return Ok(read(timeouts)),
                Gate::Stopped(e) => {
                    // This may be the waiter that abandoned registration.
                    self.hub.notify();
                    return Err(e);
                }
                Gate::Wait(until) => until,
            };
            shared = self.hub.wait(shared, until);
        }
    }

    /// Runs `function` (`models`, `quota`, `credential` or `sign`) of the
    /// provider `provider` registered, passing `arg`, and returns what it
    /// returned, as JSON.
    pub(crate) fn provider_call(
        &self,
        provider: &str,
        function: &'static str,
        arg: Value,
    ) -> Result<Value, Error> {
        self.call(
            Target::Provider {
                name: provider.to_owned(),
                function,
            },
            arg,
        )
    }

    /// Queues the call and waits on the hub until it is answered, or until
    /// its own limit passes ("Extension lifecycle" in `hub.rs`).
    fn call(&self, target: Target, arg: Value) -> Result<Value, Error> {
        // When Fiber asks, before it waits for the hub (`docs/extensions.md`).
        let asked = self.hub.clock().now();
        let mut shared = self.hub.lock();
        self.start(&mut shared)?;
        let id = shared.push(target.clone(), arg, asked);
        // Only a change wakes the others: a waiter that notified on every
        // wake would keep every other waiter spinning.
        self.hub.notify();
        loop {
            // Under the hub lock, so a clock move cannot land between the
            // judgement and the park.
            let now = self.hub.clock().now();
            match shared.judge(&self.name, id, &target, asked, now) {
                Next::Return(result) => {
                    self.hub.notify();
                    return result;
                }
                Next::Sleep(until) => shared = self.hub.wait(shared, until),
            }
        }
    }

    /// Spawns the thread on the first call. The entry script's clock starts
    /// here.
    fn start(&self, shared: &mut Shared) -> Result<(), Error> {
        if !matches!(shared.phase, Phase::Idle) {
            return Ok(());
        }
        let load_by = self.hub.clock().now().checked_add(LOAD_TIMEOUT);
        let (name, dir, home) = (self.name.clone(), self.dir.clone(), self.home.clone());
        let memory_cap = self.memory_cap;
        let browser = Arc::clone(&self.browser);
        let hub = Arc::clone(&self.hub);
        thread::Builder::new()
            .name(format!("lua {}", self.name))
            .spawn(move || {
                schedule::serve(&name, &dir, &home, &hub, load_by, memory_cap, browser);
            })
            .map_err(|source| Error::Io {
                path: self.dir.clone(),
                source,
            })?;
        shared.phase = Phase::Registering {
            abandon_at: load_by.and_then(|at| at.checked_add(GRACE)),
        };
        Ok(())
    }
}

/// Stops the extension: its thread quits the next time it looks.
impl Drop for LuaExtension {
    fn drop(&mut self) {
        let mut shared = self.hub.lock();
        if !matches!(shared.phase, Phase::Stopped(_)) {
            shared.stop(hub::stopped(&self.name));
        }
        drop(shared);
        self.hub.notify();
    }
}

/// What a call runs.
#[derive(Clone)]
enum Target {
    /// A command `fiber.command` registered.
    Command(String),
    /// A function `fiber.provider` registered.
    Provider {
        /// The provider.
        name: String,
        /// `models`, `quota`, `credential` or `sign`.
        function: &'static str,
    },
    /// A hook `fiber.hook` registered.
    Hook {
        /// The hook point.
        point: String,
        /// Its place among the hooks registered there, from 0.
        index: usize,
    },
}

/// The callback's name as an error names it: a command's name,
/// `<provider>.<function>`, or a hook's point.
impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Command(name) => f.write_str(name),
            Self::Provider { name, function } => write!(f, "{name}.{function}"),
            Self::Hook { point, .. } => f.write_str(point),
        }
    }
}

fn expired(at: Option<Instant>, now: Instant) -> bool {
    at.is_some_and(|at| now >= at)
}

fn timeout_ms(timeout: Duration) -> u64 {
    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
}

mod hub;
mod schedule;
mod setup;

use hub::{CallbackTimeouts, Gate, Hub, Next, Phase, Shared};
pub(crate) use hub::{DeclaredHooks, HookPhase};

pub(crate) use setup::Deadline;

struct Vm {
    lua: Lua,
    name: String,
    deadline: Deadline,
    clock: Arc<dyn Clock>,
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
}

/// One step of a callback: it returned, or it suspended on a host call.
enum Step {
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
    fn load(
        name: &str,
        dir: &Path,
        home: &Path,
        clock: Arc<dyn Clock>,
        load_by: Option<Instant>,
        memory_cap: usize,
        browser: Arc<dyn Browser>,
    ) -> Result<Self, Error> {
        let deadline = Deadline::new(Arc::clone(&clock));
        deadline.restore(load_by);
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
        lua.set_memory_limit(memory_cap).map_err(lua_error)?;
        let (commands, providers, hooks, problems) =
            setup::install(&lua, &deadline, dir.clone(), memory_cap).map_err(lua_error)?;
        let entry = Rc::new(Cell::new(true));
        let http_tag =
            host::install(&lua, home.to_owned(), browser, Rc::clone(&entry)).map_err(lua_error)?;
        let vm = Self {
            lua,
            name: name.to_owned(),
            deadline,
            clock,
            http_tag,
            commands,
            providers,
            hooks,
            problems,
        };

        let entry_fn = match setup::load_file(&vm.lua, &dir, ENTRY, memory_cap) {
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
    fn step(
        &self,
        target: &Target,
        arg: &Value,
        timeout: Duration,
        deadline: Option<Instant>,
    ) -> Result<Step, Error> {
        let fail = |e: mlua::Error| self.error(&e);
        let spec = match target {
            Target::Command(command) => self.commands.get::<Option<Table>>(command.as_str()),
            Target::Provider { name, function } => self
                .providers
                .get::<Option<Table>>(name.as_str())
                .and_then(|p| p.map_or(Ok(None), |p| p.get::<Option<Table>>(*function))),
            Target::Hook { point, index } => self
                .hooks
                .get::<Option<Table>>(point.as_str())
                .and_then(|list| {
                    list.map_or(Ok(None), |list| {
                        list.get::<Option<Table>>(index.saturating_add(1))
                    })
                }),
        }
        .map_err(fail)?;
        let Some(spec) = spec else {
            return Err(hub::not_registered(&self.name, target));
        };
        let run = spec.get::<Function>("run").map_err(fail)?;
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
    fn after(
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
    fn declared(&self) -> CallbackTimeouts {
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

    fn returned(&self, target: &Target, value: mlua::Value) -> Result<Value, Error> {
        let fail = |e: mlua::Error| self.error(&e);
        match target {
            Target::Command(_) => Option::<String>::from_lua(value, &self.lua)
                .map(|text| Value::String(text.unwrap_or_default()))
                .map_err(fail),
            Target::Provider { .. } | Target::Hook { .. } => host::to_json(&value).map_err(fail),
        }
    }

    fn error(&self, e: &mlua::Error) -> Error {
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
    fn resume(
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
                    host::Request::Callback { .. } | host::Request::Lock | host::Request::Sleep(_),
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
        host::request_from(&kind, yielded.next(), timeout)
            .map_err(|e| self.error(&e))?
            .ok_or_else(|| self.yielded())
    }

    /// A full garbage collection, so a lock handle left in a coroutine the
    /// host dropped is freed now rather than at some later cycle.
    fn collect(&self) {
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

#[cfg(test)]
#[path = "lua_tests.rs"]
mod tests;
