//! A Lua extension's runtime (`docs/extensions.md`, "Lua extensions", "How an
//! extension runs", "Loading, and cost when nothing is loaded" and "When an
//! extension misbehaves"): one Lua 5.4 VM on one thread per extension,
//! created on first use, with the stripped standard library, a `require`
//! held to the extension's directory, a deadline armed on every coroutine and
//! a memory cap.
//!
//! The deadline has two guards. The instruction hook stops Lua code at the
//! deadline and leaves the VM usable. Code the hook never sees, such as a
//! `__gc` finalizer (Lua turns hooks off there) or a long C call such as a
//! backtracking `string.find`, is caught by the caller instead: it stops
//! waiting a grace period past the deadline and abandons the VM.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use mlua::{Function, HookTriggers, Lua, LuaOptions, StdLib, Table, Thread, VmState};

use crate::Error;

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
/// `coroutine.create`, `require`, and `fiber.command`, which fills the table
/// the prelude returns. Lua's own `wrap` is a
/// separate C function that would make an unarmed coroutine.
// ponytail: `timeout` is in milliseconds until #331 names its unit.
const PRELUDE: &str = r#"
local create, load_module = ...
local commands = {}
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

fiber = {
  command = function(name, spec)
    if type(name) ~= "string" then
      error("fiber.command: the name must be a string", 2)
    end
    if type(spec) ~= "table" or math.type(spec.timeout) ~= "integer" or spec.timeout <= 0 then
      error("fiber.command: `timeout` must be a whole number of milliseconds above 0", 2)
    end
    if type(spec.run) ~= "function" then
      error("fiber.command: `run` must be a function", 2)
    end
    commands[name] = { timeout = spec.timeout, run = spec.run }
  end,
}

return commands
"#;

/// A Lua extension in a session. Creating one runs no Lua: the VM and its
/// thread start on the first call.
pub struct LuaExtension {
    name: String,
    dir: PathBuf,
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
    /// The extension `name`, whose files are in `dir`.
    pub fn new(name: impl Into<String>, dir: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            dir: dir.into(),
            state: Mutex::new(State::Idle),
        }
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
        let job = Job {
            command: command.to_owned(),
            text: text.to_owned(),
            reply,
        };
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
                        callback: command.to_owned(),
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
        let (name, dir) = (self.name.clone(), self.dir.clone());
        thread::Builder::new()
            .name(format!("lua {}", self.name))
            .spawn(move || serve(&name, &dir, &inbox))
            .map_err(|source| Error::Io {
                path: self.dir.clone(),
                source,
            })?;
        Ok(sender)
    }
}

struct Job {
    command: String,
    text: String,
    reply: Sender<Reply>,
}

/// What the extension's thread tells the caller about one job.
enum Reply {
    /// Lua is about to run until this deadline; none is past the end of time.
    Deadline(Option<Instant>),
    /// The job's result.
    Done(Result<String, Error>),
}

/// The extension's thread: creates the VM on the first job and serves the
/// inbox in order. A VM whose entry script failed is created again on the
/// next job.
fn serve(name: &str, dir: &Path, inbox: &Receiver<Job>) {
    let mut vm = None;
    for job in inbox {
        let notify = |at| {
            if job.reply.send(Reply::Deadline(at)).is_err() {
                // The caller stopped waiting; the job runs all the same.
            }
        };
        let result = match &vm {
            Some(vm) => Ok(vm),
            None => Vm::load(name, dir, &notify).map(|loaded| &*vm.insert(loaded)),
        }
        .and_then(|vm| vm.command(&job.command, &job.text, &notify));
        if job.reply.send(Reply::Done(result)).is_err() {
            // The caller stopped waiting; the next job is served all the same.
        }
    }
}

struct Vm {
    lua: Lua,
    name: String,
    deadline: Deadline,
    /// What `fiber.command` registered: each name's `timeout` and `run`.
    commands: Table,
}

impl Vm {
    /// Creates the VM and runs the entry script. Its clock starts before the
    /// script is read.
    fn load(name: &str, dir: &Path, notify: &dyn Fn(Option<Instant>)) -> Result<Self, Error> {
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
        let deadline = Deadline::default();
        let commands = install(&lua, &deadline, dir.clone()).map_err(lua_error)?;
        let vm = Self {
            lua,
            name: name.to_owned(),
            deadline,
            commands,
        };

        notify(vm.deadline.start(LOAD_TIMEOUT));
        let entry = match load_file(&vm.lua, &dir, ENTRY) {
            Ok(Ok(entry)) => entry,
            Ok(Err(message)) => return Err(fail(message)),
            Err(e) => return Err(lua_error(e)),
        };
        vm.resume(entry, ENTRY, LOAD_TIMEOUT, "")?;
        Ok(vm)
    }

    fn command(
        &self,
        command: &str,
        text: &str,
        notify: &dyn Fn(Option<Instant>),
    ) -> Result<String, Error> {
        let fail = |e: mlua::Error| self.error(&e);
        let Some(spec) = self.commands.get::<Option<Table>>(command).map_err(fail)? else {
            return Err(Error::UnknownCommand {
                extension: self.name.clone(),
                command: command.to_owned(),
            });
        };
        let timeout = Duration::from_millis(spec.get::<u64>("timeout").map_err(fail)?);
        let run = spec.get::<Function>("run").map_err(fail)?;
        notify(self.deadline.start(timeout));
        self.resume(run, command, timeout, text)
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
        text: &str,
    ) -> Result<String, Error> {
        let fail = |e: mlua::Error| self.error(&e);
        let thread = self.lua.create_thread(f).map_err(fail)?;
        self.deadline.arm(&thread).map_err(fail)?;
        let result = thread.resume::<Option<String>>(text);
        if self.deadline.passed() {
            return Err(Error::Timeout {
                extension: self.name.clone(),
                callback: callback.to_owned(),
                timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            });
        }
        result.map(Option::unwrap_or_default).map_err(fail)
    }
}

/// Removes the base library's I/O, which belongs to the host, and runs the
/// prelude. Returns the table `fiber.command` fills.
fn install(lua: &Lua, deadline: &Deadline, dir: PathBuf) -> mlua::Result<Table> {
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
    let too_big = |len: u64| usize::try_from(len).map_or(true, |len| len > MEMORY_CAP);
    let source = match std::fs::metadata(&path) {
        Ok(meta) if too_big(meta.len()) => {
            return Ok(Err(format!(
                "`{file}` is {} bytes, more than the extension's memory cap of {MEMORY_CAP}",
                meta.len()
            )));
        }
        Ok(_) => std::fs::read(&path),
        Err(e) => Err(e),
    };
    let source = match source {
        Ok(source) => source,
        Err(e) => return Ok(Err(format!("`{file}`: {e}"))),
    };
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
struct Deadline(Rc<Cell<Option<Instant>>>);

impl Deadline {
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
