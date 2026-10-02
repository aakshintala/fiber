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
//! and abandons the VM. A call still waiting behind an earlier one is judged
//! by its own timeout from when Fiber asked, and failing it leaves the VM up.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use mlua::{FromLua, Function, Lua, LuaOptions, MultiValue, StdLib, Table, Thread};
use serde_json::Value;

use crate::{Error, host};

/// Each Lua extension's memory cap. Past it, an allocation is a Lua error in
/// that extension's VM, and a file larger than it is not read.
// ponytail: one cap for every extension until #331 settles its size and
// setting; docs/extensions.md calls it optional and per extension.
pub const MEMORY_CAP: usize = 64 << 20;

/// The script Fiber runs when it creates the VM.
// ponytail: docs/configuration.md's manifest names no Lua entry script; #331.
const ENTRY: &str = "init.lua";

/// How long reading, compiling and running the entry script may take. It is
/// not a callback, so it declares no timeout of its own.
// ponytail: no doc names this bound; #331.
const LOAD_TIMEOUT: Duration = Duration::from_secs(2);

/// How long past a deadline the caller waits before it abandons the VM.
// ponytail: picked, not measured; #331.
const GRACE: Duration = Duration::from_secs(1);

/// Instructions between two looks at the clock while a deadline is ahead.
pub(super) const CHECK_EVERY: u32 = 1000;

/// The Lua half of the host's globals: `coroutine.wrap` over the armed
/// `coroutine.create`, `require`, `fiber.command` and `fiber.provider`, which
/// fill the two tables the prelude returns. Lua's own `wrap` is a
/// separate C function that would make an unarmed coroutine.
// ponytail: `timeout` is in milliseconds until #331 names its unit.
pub(super) const PRELUDE: &str = r#"
local create, load_module = ...
local commands, providers = {}, {}
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
}

return commands, providers
"#;

/// A Lua extension in a session. Creating one runs no Lua: the VM and its
/// thread start on the first call.
pub struct LuaExtension {
    name: String,
    dir: PathBuf,
    home: PathBuf,
    state: Mutex<State>,
    /// Each callback's declared timeout, published once the entry script has
    /// run. A call reads it so waiting behind an earlier item counts against
    /// its own clock (`docs/extensions.md`, "How an extension runs").
    timeouts: Arc<Mutex<BTreeMap<String, Duration>>>,
}

enum State {
    /// Never called: no VM, no thread.
    Idle,
    /// The thread's inbox.
    Running(Sender<schedule::Msg>),
    /// The VM was abandoned or its thread is gone; it takes no more calls.
    Stopped,
}

impl LuaExtension {
    /// The extension `name`, whose files are in `dir`, in the Fiber home
    /// `home` that `host.secret` reads.
    pub fn new(name: impl Into<String>, dir: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            dir: dir.into(),
            home: home.into(),
            state: Mutex::new(State::Idle),
            timeouts: Arc::new(Mutex::new(BTreeMap::new())),
        }
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
        matches!(*self.lock(), State::Running(_))
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
        let value = self.call(Target::Functions(provider.to_owned()), Value::Null)?;
        Ok(value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect())
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

    fn call(&self, target: Target, arg: Value) -> Result<Value, Error> {
        let callback = target.to_string();
        let asked = Instant::now();
        // Known once the entry script has published it. Waiting in the stream
        // counts against it. Missing means this callback was never registered.
        let budget = self.budget(&target);
        if matches!(budget, Budget::Missing) {
            return Err(self.not_registered(&target));
        }
        let inbox = {
            let mut state = self.lock();
            match &*state {
                State::Running(inbox) => inbox.clone(),
                State::Idle => {
                    let inbox = self.start()?;
                    *state = State::Running(inbox.clone());
                    inbox
                }
                State::Stopped => return Err(self.stopped()),
            }
        };
        let (reply, answers) = mpsc::channel();
        // The lock is not held while this waits. A host call suspends its
        // callback, and provider work, such as `sign()` behind a refresh,
        // has to reach the extension's thread meanwhile. The next command
        // does not: it stays queued until this one finishes.
        if inbox
            .send(schedule::Msg::Job(Job {
                target,
                arg,
                reply,
                asked,
            }))
            .is_err()
        {
            *self.lock() = State::Stopped;
            return Err(self.stopped());
        }
        let timeout = match budget {
            Budget::Known(timeout) => timeout,
            Budget::Pending | Budget::Missing => GRACE,
        };
        // Until the callback starts, its own timeout is the whole wait.
        // Failing that wait does not abandon the VM. Once it has started,
        // a grace past the deadline catches code the hook cannot stop.
        let mut started = false;
        let mut until = asked.checked_add(timeout);
        loop {
            let answer = match until {
                Some(at) => answers.recv_timeout(at.saturating_duration_since(Instant::now())),
                None => answers.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match answer {
                Ok(Reply::Deadline(at)) => {
                    started = true;
                    until = at.and_then(|at| at.checked_add(GRACE));
                }
                Ok(Reply::Done(result)) => return result,
                Err(RecvTimeoutError::Timeout) if !started => {
                    return Err(Error::Timeout {
                        extension: self.name.clone(),
                        callback,
                        timeout_ms: timeout_ms(timeout),
                    });
                }
                Err(RecvTimeoutError::Timeout) => {
                    // ponytail: the abandoned thread is leaked, still running,
                    // until the process exits; Rust cannot stop a thread.
                    *self.lock() = State::Stopped;
                    return Err(Error::Abandoned {
                        extension: self.name.clone(),
                        callback,
                    });
                }
                Err(RecvTimeoutError::Disconnected) => {
                    *self.lock() = State::Stopped;
                    return Err(self.stopped());
                }
            }
        }
    }

    /// The timeout `target` declared, if the entry script has published the
    /// table. [`Budget::Pending`] until then, including for a lookup that
    /// runs no callback.
    fn budget(&self, target: &Target) -> Budget {
        if matches!(target, Target::Functions(_)) {
            return Budget::Pending;
        }
        let map = self.timeouts.lock().unwrap_or_else(PoisonError::into_inner);
        if map.is_empty() {
            return Budget::Pending;
        }
        map.get(&target.to_string())
            .copied()
            .map_or(Budget::Missing, Budget::Known)
    }

    fn not_registered(&self, target: &Target) -> Error {
        match target {
            Target::Command(command) => Error::UnknownCommand {
                extension: self.name.clone(),
                command: command.clone(),
            },
            Target::Provider { .. } | Target::Functions(_) => Error::UnknownCallback {
                extension: self.name.clone(),
                callback: target.to_string(),
            },
        }
    }

    fn stopped(&self) -> Error {
        Error::Stopped {
            extension: self.name.clone(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic aborts the process (`docs/code-quality.md`, "Panics"), so
        // no holder can leave the lock poisoned.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn start(&self) -> Result<Sender<schedule::Msg>, Error> {
        let (sender, inbox) = mpsc::channel();
        let (name, dir, home) = (self.name.clone(), self.dir.clone(), self.home.clone());
        let http = sender.clone();
        let timeouts = Arc::clone(&self.timeouts);
        thread::Builder::new()
            .name(format!("lua {}", self.name))
            .spawn(move || schedule::serve(&name, &dir, &home, &inbox, &http, &timeouts))
            .map_err(|source| Error::Io {
                path: self.dir.clone(),
                source,
            })?;
        Ok(sender)
    }
}

struct Job {
    target: Target,
    arg: Value,
    reply: Sender<Reply>,
    /// When Fiber asked. Waiting behind an earlier stream item counts
    /// against the callback's timeout from here.
    asked: Instant,
}

/// Whether the caller already knows how long `call` may wait before the
/// callback starts.
#[derive(Clone, Copy)]
enum Budget {
    /// The entry script has not published timeouts yet.
    Pending,
    /// The callback declared this timeout.
    Known(Duration),
    /// Timeouts are published and this callback is not one of them.
    Missing,
}

/// What a job runs.
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
    /// The names of the functions a provider registered; runs no Lua.
    Functions(String),
}

/// The callback's name as an error names it: a command's name, or
/// `<provider>.<function>`.
impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Command(name) | Self::Functions(name) => f.write_str(name),
            Self::Provider { name, function } => write!(f, "{name}.{function}"),
        }
    }
}

/// What the extension's thread tells the caller about one job.
enum Reply {
    /// Lua is about to run until this deadline; none is past the end of time.
    Deadline(Option<Instant>),
    /// The job's result.
    Done(Result<Value, Error>),
}

/// Sends a deadline to the caller before Lua runs. An error means the
/// caller has already stopped waiting, so this job must not run.
type Notify<'a> = &'a dyn Fn(Option<Instant>) -> Result<(), Error>;

#[derive(Clone, Copy)]
enum CallbackKind {
    Command,
    Provider,
}

fn expired(at: Option<Instant>) -> bool {
    at.is_some_and(|at| Instant::now() >= at)
}

fn timeout_ms(timeout: Duration) -> u64 {
    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
}

mod schedule;
mod setup;

pub(crate) use setup::Deadline;

struct Vm {
    lua: Lua,
    name: String,
    deadline: Deadline,
    /// What `host.http` yields, so a callback's own yield is not a request.
    http_tag: mlua::Value,
    /// What `fiber.command` registered: each name's `timeout` and `run`.
    commands: Table,
    /// What `fiber.provider` registered: each provider's functions, each
    /// with its `timeout` and `run`.
    providers: Table,
}

/// One step of a callback: it returned, or it suspended on `host.http`.
enum Step {
    Done(Value),
    Suspend {
        thread: Thread,
        deadline: Option<Instant>,
        timeout: Duration,
        callback: String,
        kind: CallbackKind,
        request: host::HttpRequest,
    },
}

impl Vm {
    /// Creates the VM and runs the entry script. Its clock starts before
    /// anything else, so creating the VM and reading the script count.
    fn load(
        name: &str,
        dir: &Path,
        home: &Path,
        notify: Notify<'_>,
        timeouts: &Mutex<BTreeMap<String, Duration>>,
    ) -> Result<Self, Error> {
        let deadline = Deadline::default();
        notify(deadline.start(LOAD_TIMEOUT))?;
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
        lua.set_memory_limit(MEMORY_CAP).map_err(lua_error)?;
        let (commands, providers) =
            setup::install(&lua, &deadline, dir.clone()).map_err(lua_error)?;
        let http_tag = host::install(&lua, home.to_owned()).map_err(lua_error)?;
        let vm = Self {
            lua,
            name: name.to_owned(),
            deadline,
            http_tag,
            commands,
            providers,
        };

        let entry = match setup::load_file(&vm.lua, &dir, ENTRY) {
            Ok(Ok(entry)) => entry,
            Ok(Err(message)) => return Err(fail(message)),
            Err(e) => return Err(lua_error(e)),
        };
        vm.resume(entry, ENTRY, LOAD_TIMEOUT, mlua::Value::Nil)?;
        *timeouts.lock().unwrap_or_else(PoisonError::into_inner) = vm.declared();
        Ok(vm)
    }

    /// Starts `target` and runs it until it returns or suspends on `host.http`.
    /// `asked` is when Fiber asked, so time already spent waiting counts.
    fn step(
        &self,
        target: &Target,
        arg: &Value,
        asked: Instant,
        notify: Notify<'_>,
    ) -> Result<Step, Error> {
        let fail = |e: mlua::Error| self.error(&e);
        let spec = match target {
            Target::Command(command) => self.commands.get::<Option<Table>>(command.as_str()),
            Target::Provider { name, function } => self
                .providers
                .get::<Option<Table>>(name.as_str())
                .and_then(|p| p.map_or(Ok(None), |p| p.get::<Option<Table>>(*function))),
            Target::Functions(name) => {
                return Ok(Step::Done(self.function_names(name)?));
            }
        }
        .map_err(fail)?;
        let Some(spec) = spec else {
            return Err(match target {
                Target::Command(command) => Error::UnknownCommand {
                    extension: self.name.clone(),
                    command: command.clone(),
                },
                Target::Provider { .. } | Target::Functions(_) => Error::UnknownCallback {
                    extension: self.name.clone(),
                    callback: target.to_string(),
                },
            });
        };
        let timeout = Duration::from_millis(spec.get::<u64>("timeout").map_err(fail)?);
        let deadline = asked.checked_add(timeout);
        if expired(deadline) {
            return Err(self.timed_out(&target.to_string(), timeout));
        }
        let run = spec.get::<Function>("run").map_err(fail)?;
        let lua_arg = match target {
            Target::Command(_) => arg
                .as_str()
                .map(|text| self.lua.create_string(text))
                .transpose()
                .map_err(fail)?
                .map_or(mlua::Value::Nil, mlua::Value::String),
            Target::Provider { .. } | Target::Functions(_) => {
                host::to_lua(&self.lua, arg).map_err(fail)?
            }
        };
        self.deadline.restore(deadline);
        notify(deadline)?;
        let callback = target.to_string();
        let kind = match target {
            Target::Command(_) => CallbackKind::Command,
            Target::Provider { .. } | Target::Functions(_) => CallbackKind::Provider,
        };
        let thread = self.lua.create_thread(run).map_err(fail)?;
        self.deadline.arm(&thread).map_err(fail)?;
        self.after(
            thread,
            MultiValue::from_vec(vec![lua_arg]),
            &callback,
            timeout,
            deadline,
            kind,
        )
    }

    fn after(
        &self,
        thread: Thread,
        args: MultiValue,
        callback: &str,
        timeout: Duration,
        deadline: Option<Instant>,
        kind: CallbackKind,
    ) -> Result<Step, Error> {
        match self.poll(&thread, args, callback, timeout, deadline)? {
            setup::Poll::Done(value) => Ok(Step::Done(self.returned(kind, value)?)),
            setup::Poll::Http(request) => Ok(Step::Suspend {
                thread,
                deadline,
                timeout,
                callback: callback.to_owned(),
                kind,
                request,
            }),
        }
    }

    /// Every callback the entry script registered, by the name an error uses.
    fn declared(&self) -> BTreeMap<String, Duration> {
        let mut map = BTreeMap::new();
        for (name, spec) in self.commands.pairs::<String, Table>().flatten() {
            if let Ok(ms) = spec.get::<u64>("timeout") {
                map.insert(name, Duration::from_millis(ms));
            }
        }
        for (name, functions) in self.providers.pairs::<String, Table>().flatten() {
            for (function, spec) in functions.pairs::<String, Table>().flatten() {
                if let Ok(ms) = spec.get::<u64>("timeout") {
                    map.insert(format!("{name}.{function}"), Duration::from_millis(ms));
                }
            }
        }
        map
    }

    fn function_names(&self, name: &str) -> Result<Value, Error> {
        let fail = |e: mlua::Error| self.error(&e);
        let mut names = Vec::new();
        if let Some(functions) = self.providers.get::<Option<Table>>(name).map_err(fail)? {
            for pair in functions.pairs::<String, mlua::Value>() {
                names.push(Value::String(pair.map_err(fail)?.0));
            }
        }
        names.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        Ok(Value::Array(names))
    }

    fn returned(&self, kind: CallbackKind, value: mlua::Value) -> Result<Value, Error> {
        let fail = |e: mlua::Error| self.error(&e);
        match kind {
            CallbackKind::Command => Option::<String>::from_lua(value, &self.lua)
                .map(|text| Value::String(text.unwrap_or_default()))
                .map_err(fail),
            CallbackKind::Provider => host::to_json(&value).map_err(fail),
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
                setup::Poll::Http(request) => {
                    args = host::resume_values(&self.lua, host::perform(&request)).map_err(fail)?;
                }
            }
        }
    }

    /// Resumes `thread` once. A yield of [`Vm::http_tag`] is one `host.http`.
    fn poll(
        &self,
        thread: &Thread,
        args: MultiValue,
        callback: &str,
        timeout: Duration,
        deadline: Option<Instant>,
    ) -> Result<setup::Poll, Error> {
        self.deadline.restore(deadline);
        if expired(deadline) {
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
        let grace = deadline.map(|at| {
            at.saturating_duration_since(Instant::now())
                .saturating_add(GRACE)
        });
        Ok(setup::Poll::Http(self.http_request(&values, grace)?))
    }

    fn http_request(
        &self,
        values: &MultiValue,
        timeout: Option<Duration>,
    ) -> Result<host::HttpRequest, Error> {
        let mut yielded = values.iter();
        let tag = yielded.next();
        let opts = yielded.next();
        let (Some(tag), Some(mlua::Value::Table(opts))) = (tag, opts) else {
            return Err(self.yielded());
        };
        if !tag.equals(&self.http_tag).map_err(|e| self.error(&e))? {
            return Err(self.yielded());
        }
        host::request_from(opts, timeout).map_err(|e| self.error(&e))
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
