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
//! backtracking `string.find`, is caught by the caller instead: it stops
//! waiting a grace period past the deadline and abandons the VM.

use std::cell::Cell;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use mlua::{FromLua, Function, HookTriggers, Lua, LuaOptions, StdLib, Table, Thread, VmState};
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
const CHECK_EVERY: u32 = 1000;

/// The Lua half of the host's globals: `coroutine.wrap` over the armed
/// `coroutine.create`, `require`, `fiber.command` and `fiber.provider`, which
/// fill the two tables the prelude returns. Lua's own `wrap` is a
/// separate C function that would make an unarmed coroutine.
// ponytail: `timeout` is in milliseconds until #331 names its unit.
const PRELUDE: &str = r#"
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
}

enum State {
    /// Never called: no VM, no thread.
    Idle,
    /// The thread's inbox.
    Running(Sender<Job>),
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
        }
    }

    /// The extension's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The Fiber home this extension was installed into.
    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    /// Whether the extension's VM and thread exist and take calls.
    pub fn is_running(&self) -> bool {
        matches!(*self.lock(), State::Running(_))
    }

    /// Runs the command `command` the extension registered with
    /// `fiber.command`, passing `text`, and returns what its `run` returned.
    /// The first call creates the VM and runs the entry script. Calls run
    /// one at a time, in the order they take the extension's lock.
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
        let mut state = self.lock();
        let inbox = match &*state {
            State::Running(inbox) => inbox.clone(),
            State::Idle => {
                let inbox = self.start()?;
                *state = State::Running(inbox.clone());
                inbox
            }
            State::Stopped => return Err(self.stopped()),
        };
        let (reply, answers) = mpsc::channel();
        let job = Job { target, arg, reply };
        if inbox.send(job).is_err() {
            *state = State::Stopped;
            return Err(self.stopped());
        }
        // The thread is idle, since calls hold the lock, so it names its
        // first deadline at once.
        let mut until = Instant::now().checked_add(GRACE);
        loop {
            let answer = match until {
                Some(at) => answers.recv_timeout(at.saturating_duration_since(Instant::now())),
                None => answers.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match answer {
                Ok(Reply::Deadline(at)) => until = at.and_then(|at| at.checked_add(GRACE)),
                Ok(Reply::Done(result)) => return result,
                Err(RecvTimeoutError::Timeout) => {
                    // ponytail: the abandoned thread is leaked, still running,
                    // until the process exits; Rust cannot stop a thread.
                    *state = State::Stopped;
                    return Err(Error::Abandoned {
                        extension: self.name.clone(),
                        callback,
                    });
                }
                Err(RecvTimeoutError::Disconnected) => {
                    *state = State::Stopped;
                    return Err(self.stopped());
                }
            }
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

    fn start(&self) -> Result<Sender<Job>, Error> {
        let (sender, inbox) = mpsc::channel();
        let (name, dir, home) = (self.name.clone(), self.dir.clone(), self.home.clone());
        thread::Builder::new()
            .name(format!("lua {}", self.name))
            .spawn(move || serve(&name, &dir, &home, &inbox))
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
/// caller has stopped waiting and abandoned the VM, so nothing may run.
type Notify<'a> = &'a dyn Fn(Option<Instant>) -> Result<(), Error>;

/// The extension's thread: creates the VM on the first job and serves the
/// inbox in order. A VM whose entry script failed is created again on the
/// next job. It quits as soon as a caller has stopped waiting: that caller
/// abandoned the VM, so no entry script or callback may run after it.
fn serve(name: &str, dir: &Path, home: &Path, inbox: &Receiver<Job>) {
    let mut vm = None;
    for job in inbox {
        let notify = |at| {
            job.reply
                .send(Reply::Deadline(at))
                .map_err(|_| Error::Stopped {
                    extension: name.to_owned(),
                })
        };
        let result = match &vm {
            Some(vm) => Ok(vm),
            None => Vm::load(name, dir, home, &notify).map(|loaded| &*vm.insert(loaded)),
        }
        .and_then(|vm| vm.run(&job.target, &job.arg, &notify));
        if matches!(result, Err(Error::Stopped { .. }))
            || job.reply.send(Reply::Done(result)).is_err()
        {
            return;
        }
    }
}

struct Vm {
    lua: Lua,
    name: String,
    deadline: Deadline,
    /// What `fiber.command` registered: each name's `timeout` and `run`.
    commands: Table,
    /// What `fiber.provider` registered: each provider's functions, each
    /// with its `timeout` and `run`.
    providers: Table,
}

impl Vm {
    /// Creates the VM and runs the entry script. Its clock starts before
    /// anything else, so creating the VM and reading the script count.
    fn load(name: &str, dir: &Path, home: &Path, notify: Notify<'_>) -> Result<Self, Error> {
        let deadline = Deadline::default();
        notify(deadline.start(LOAD_TIMEOUT))?;
        let fail = |message: String| Error::Lua {
            extension: name.to_owned(),
            message,
        };
        let lua_error = |e: mlua::Error| fail(message(&e));
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
        let (commands, providers) = install(&lua, &deadline, dir.clone()).map_err(lua_error)?;
        host::install(&lua, &deadline, home.to_owned()).map_err(lua_error)?;
        let vm = Self {
            lua,
            name: name.to_owned(),
            deadline,
            commands,
            providers,
        };

        let entry = match load_file(&vm.lua, &dir, ENTRY) {
            Ok(Ok(entry)) => entry,
            Ok(Err(message)) => return Err(fail(message)),
            Err(e) => return Err(lua_error(e)),
        };
        vm.resume(entry, ENTRY, LOAD_TIMEOUT, mlua::Value::Nil)?;
        Ok(vm)
    }

    fn run(&self, target: &Target, arg: &Value, notify: Notify<'_>) -> Result<Value, Error> {
        let fail = |e: mlua::Error| self.error(&e);
        let spec = match target {
            Target::Command(command) => self.commands.get::<Option<Table>>(command.as_str()),
            Target::Provider { name, function } => self
                .providers
                .get::<Option<Table>>(name.as_str())
                .and_then(|p| p.map_or(Ok(None), |p| p.get::<Option<Table>>(*function))),
            Target::Functions(name) => {
                let mut names = Vec::new();
                if let Some(functions) = self
                    .providers
                    .get::<Option<Table>>(name.as_str())
                    .map_err(fail)?
                {
                    for pair in functions.pairs::<String, mlua::Value>() {
                        names.push(Value::String(pair.map_err(fail)?.0));
                    }
                }
                names.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                return Ok(Value::Array(names));
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
        let run = spec.get::<Function>("run").map_err(fail)?;
        let arg = match target {
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
        notify(self.deadline.start(timeout))?;
        let value = self.resume(run, &target.to_string(), timeout, arg)?;
        match target {
            Target::Command(_) => Option::<String>::from_lua(value, &self.lua)
                .map(|text| Value::String(text.unwrap_or_default())),
            Target::Provider { .. } | Target::Functions(_) => host::to_json(&value),
        }
        .map_err(fail)
    }

    fn error(&self, e: &mlua::Error) -> Error {
        Error::Lua {
            extension: self.name.clone(),
            message: message(e),
        }
    }

    /// Runs `f` as a coroutine armed with the deadline already started. A
    /// callback that ends past its deadline, by an error or by catching one,
    /// has timed out.
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
        let result = thread.resume::<mlua::Value>(arg);
        if self.deadline.passed() {
            return Err(Error::Timeout {
                extension: self.name.clone(),
                callback: callback.to_owned(),
                timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            });
        }
        result.map_err(fail)
    }
}

/// Removes the base library's I/O, which belongs to the host, and runs the
/// prelude. Returns the tables `fiber.command` and `fiber.provider` fill.
fn install(lua: &Lua, deadline: &Deadline, dir: PathBuf) -> mlua::Result<(Table, Table)> {
    let globals = lua.globals();
    for name in ["print", "warn", "dofile", "loadfile"] {
        globals.raw_remove(name)?;
    }
    let deadline = deadline.clone();
    let create = lua.create_function(move |lua, f: Function| {
        let thread = lua.create_thread(f)?;
        deadline.arm(&thread)?;
        Ok(thread)
    })?;
    let load_module = lua.create_function(move |lua, name: String| {
        let file = format!("{}.lua", name.replace('.', "/"));
        Ok(match load_file(lua, &dir, &file)? {
            Ok(chunk) => (Some(chunk), None),
            Err(message) => (None, Some(message)),
        })
    })?;
    lua.load(PRELUDE)
        .set_name("=prelude")
        .call((create, load_module))
}

/// Reads `file` under `dir` and compiles it, named by its path in `dir` so an
/// error names the file and line. The inner error is the reason it cannot be
/// loaded, for Lua to raise at the caller's line. A file larger than the
/// memory cap is not read.
fn load_file(lua: &Lua, dir: &Path, file: &str) -> mlua::Result<Result<Function, String>> {
    let path = match dir.join(file).canonicalize() {
        Ok(path) if path.starts_with(dir) => path,
        Ok(_) => {
            return Ok(Err(format!(
                "`{file}` is outside the extension's directory"
            )));
        }
        Err(e) => return Ok(Err(format!("`{file}`: {e}"))),
    };
    let mut source = Vec::new();
    // One byte past the cap tells a file at the cap from one over it. The
    // bound is on the read itself, so a file that grows cannot pass it.
    let limit = u64::try_from(MEMORY_CAP)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    if let Err(e) = File::open(&path).and_then(|f| f.take(limit).read_to_end(&mut source)) {
        return Ok(Err(format!("`{file}`: {e}")));
    }
    if source.len() > MEMORY_CAP {
        return Ok(Err(format!(
            "`{file}` is larger than the extension's memory cap of {MEMORY_CAP} bytes"
        )));
    }
    let name = path
        .strip_prefix(dir)
        .unwrap_or(&path)
        .display()
        .to_string();
    match lua
        .load(source)
        .set_name(format!("@{name}"))
        .into_function()
    {
        Ok(chunk) => Ok(Ok(chunk)),
        Err(mlua::Error::SyntaxError { message, .. }) => Ok(Err(message)),
        Err(e) => Err(e),
    }
}

/// The message a person reads for a Lua error: its first line, which Lua
/// starts with the file and line, without mlua's traceback.
fn message(e: &mlua::Error) -> String {
    let text = if let mlua::Error::RuntimeError(m) | mlua::Error::MemoryError(m) = e {
        m.clone()
    } else {
        e.to_string()
    };
    text.lines().next().unwrap_or_default().to_owned()
}

/// The running callback's deadline, shared by the hook on every coroutine.
///
/// The hook looks at the clock every `CHECK_EVERY` instructions. Once the
/// deadline passes it raises a timeout and re-arms its coroutine to raise one
/// on every instruction, so a `pcall` that catches the first cannot run on
/// (`research/extension-runtime/pass1/`, `interrupt_escalate`).
#[derive(Clone, Default)]
pub(crate) struct Deadline(Rc<Cell<Option<Instant>>>);

impl Deadline {
    /// The time left before the deadline; `None` when there is none.
    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.0
            .get()
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    /// Starts the clock and returns the deadline.
    fn start(&self, timeout: Duration) -> Option<Instant> {
        let at = Instant::now().checked_add(timeout);
        self.0.set(at);
        at
    }

    fn passed(&self) -> bool {
        self.0.get().is_some_and(|at| Instant::now() >= at)
    }

    fn arm(&self, thread: &Thread) -> mlua::Result<()> {
        let deadline = self.clone();
        thread.set_hook(
            HookTriggers::new().every_nth_instruction(CHECK_EVERY),
            move |lua, _| {
                if !deadline.passed() {
                    return Ok(VmState::Continue);
                }
                let escalated = deadline.clone();
                // ponytail: an escalated coroutine stays on the per-instruction
                // hook if a later callback resumes it; that callback runs slower,
                // and is still stopped at its own deadline.
                lua.current_thread().set_hook(
                    HookTriggers::new().every_nth_instruction(1),
                    move |_, _| {
                        if escalated.passed() {
                            Err(timed_out())
                        } else {
                            Ok(VmState::Continue)
                        }
                    },
                )?;
                Err(timed_out())
            },
        )
    }
}

fn timed_out() -> mlua::Error {
    mlua::Error::RuntimeError("the callback passed its timeout".to_owned())
}

#[cfg(test)]
#[path = "lua_tests.rs"]
mod tests;
