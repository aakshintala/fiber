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

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use serde_json::{Value, json};

use crate::oauth::{Browser, SystemBrowser};
use crate::{CredentialPair, Error, host};

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
    local shown = tostring(name)
    local function refuse(why)
      problems[#problems + 1] = "`" .. shown .. "` command not registered: " .. why
    end
    if type(name) ~= "string" then
      return refuse("the name must be a string")
    end
    if name == "" or name:find("%s") or name:find("/") or name:find(":") then
      return refuse("names are plain: `" .. shown .. "` is empty or holds whitespace, `/` or `:`")
    end
    if type(spec) ~= "table" then
      return refuse("it takes a table of `timeout`, `run` and `description`")
    end
    if spec.timeout == nil then return refuse("missing `timeout`") end
    if math.type(spec.timeout) ~= "integer" or spec.timeout <= 0 then
      return refuse("`timeout` must be a whole number of milliseconds above 0")
    end
    if type(spec.run) ~= "function" then return refuse("`run` must be a function") end
    local description = spec.description
    if description == nil then description = "" end
    if type(description) ~= "string" or description:find("\n") then
      return refuse("`description` must be a single-line string")
    end
    commands[name] = { timeout = spec.timeout, run = spec.run, description = description }
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
    /// The session, when started from one: its configuration, settings
    /// keys and per-path lock.
    session: Option<host::Session>,
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
            session: None,
            browser: Arc::new(SystemBrowser::default()),
            hub: Hub::new(clock),
        }
    }

    /// Starts this extension in a session: its workspace, configuration,
    /// settings keys and per-path lock feed `host.fs`, `host.data_dir`
    /// and `host.config`. Without it, `host.fs` resolves relative paths
    /// against the process's current directory and takes no lock, and
    /// `host.data_dir` and `host.config` raise.
    pub fn with_session(mut self, session: host::Session) -> Self {
        self.session = Some(session);
        self
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

    /// The extension's package directory, which `prompt` and
    /// `prompt_addendum` paths resolve against.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Hands the session loop's inbox to this extension's deliveries,
    /// flushing what ended before any sender in order.
    pub fn deliver_to(&self, inbox: std::sync::mpsc::Sender<contract::inbox::Delivery>) {
        self.hub.set_inbox(inbox);
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

    /// Every provider `fiber.provider` registered, sorted.
    pub fn provider_names(&self) -> Result<Vec<String>, Error> {
        self.registered(|timeouts| timeouts.providers.keys().cloned().collect())
    }

    /// Every command the entry script registered: its name and description.
    /// Starts the extension.
    pub fn commands(&self) -> Result<Vec<(String, String)>, Error> {
        self.registered(|timeouts| {
            timeouts
                .commands
                .iter()
                .map(|(name, cmd)| (name.clone(), cmd.description.clone()))
                .collect()
        })
    }

    /// Queues the command `name` with `text` as a held call: it never starts
    /// until `Queued::release`. Returns the queued call; `wait` returns the
    /// run's end. Starts the extension.
    pub(crate) fn queue_command(&self, name: &str, text: &str) -> Result<Queued, Error> {
        let asked = self.hub.clock().now();
        let mut shared = self.hub.lock();
        self.start(&mut shared)?;
        // Registration must have finished before admission: the door admits
        // only listed names, so an unregistered name is refused here.
        loop {
            let now = self.hub.clock().now();
            let registered = match shared.gate(&self.name, now) {
                Gate::Ready(timeouts) => timeouts.commands.contains_key(name),
                Gate::Stopped(e) => {
                    self.hub.notify();
                    return Err(e);
                }
                Gate::Wait(until) => {
                    shared = self.hub.wait(shared, until);
                    continue;
                }
            };
            if !registered {
                return Err(Error::UnknownCommand {
                    extension: self.name.clone(),
                    command: name.to_owned(),
                });
            }
            break;
        }
        let target = Target::Command(name.to_owned());
        let id = shared.push_held(target.clone(), Value::String(text.into()), asked);
        drop(shared);
        self.hub.notify();
        Ok(Queued {
            hub: Arc::clone(&self.hub),
            name: self.name.clone(),
            id,
            target,
            asked,
        })
    }

    /// Hands the ephemeral emitter to `host.status`, `host.widget` and `host.emit`.
    pub(crate) fn set_emit(&self, emit: std::sync::Arc<dyn contract::emit::Emit>) {
        self.hub.set_emit(emit);
    }

    /// Emits an ephemeral event through the late-bound emitter.
    pub(crate) fn emit(&self, event: contract::events::Event) {
        self.hub.emit(event);
    }

    /// Drops every later emission and delivery from this extension.
    pub(crate) fn seal(&self) {
        self.hub.seal();
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
    /// returned, as JSON. These calls carry no credential pair: only
    /// `credential()` refreshes a stored credential
    /// (`docs/model-routing.md`, "Keys, tokens and OAuth").
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
                credential: None,
            },
            arg,
        )
    }

    /// Runs `credential` of the provider `provider` for `pair`, passing the
    /// session's label and the stored credential's name, and returns what it
    /// returned, as JSON (`docs/model-routing.md`, "Keys, tokens and
    /// OAuth"). The only call whose target carries a credential pair, so
    /// the only one `host.oauth.refresh` refreshes.
    pub(crate) fn provider_credential(
        &self,
        provider: &str,
        pair: &CredentialPair,
    ) -> Result<Value, Error> {
        self.call(
            Target::Provider {
                name: provider.to_owned(),
                function: "credential",
                credential: Some(pair.clone()),
            },
            json!({"label": pair.label, "credential": pair.credential}),
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
        schedule::start(self, shared)
    }
}

/// Stops the extension: its thread quits the next time it looks, and a
/// run that ends after this is dropped, as the session is over.
impl Drop for LuaExtension {
    fn drop(&mut self) {
        self.hub.dispose(&self.name);
    }
}

/// A held queued command: it never starts until `release`, and `wait`
/// returns the run's end.
pub(crate) struct Queued {
    hub: Arc<Hub>,
    name: String,
    id: u64,
    target: Target,
    asked: Instant,
}

impl Queued {
    /// Releases the call, so the stream may start it.
    pub(crate) fn release(&self) {
        self.hub.release(self.id);
    }

    /// Waits until the run ends, or its own limit passes.
    pub(crate) fn wait(&self) -> Result<Value, Error> {
        let mut shared = self.hub.lock();
        loop {
            let now = self.hub.clock().now();
            match shared.judge(&self.name, self.id, &self.target, self.asked, now) {
                Next::Return(result) => {
                    self.hub.notify();
                    return result;
                }
                Next::Sleep(until) => shared = self.hub.wait(shared, until),
            }
        }
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
        /// The stored credential and label the call is for: `Some` only
        /// for `credential`, whose `host.oauth.refresh` locks and reads
        /// exactly this pair's file.
        credential: Option<CredentialPair>,
    },
    /// A hook `fiber.hook` registered.
    Hook {
        /// The hook point.
        point: String,
        /// Its place among the hooks registered there, from 0.
        index: usize,
    },
    /// A timer `host.after` or `host.every` set, firing on the hub's
    /// clock in the gaps of the session's stream.
    Timer {
        /// Assigned in set order from 0.
        id: u64,
    },
}

/// The callback's name as an error names it: a command's name,
/// `<provider>.<function>`, or a hook's point.
impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Command(name) => f.write_str(name),
            Self::Provider { name, function, .. } => write!(f, "{name}.{function}"),
            Self::Hook { point, .. } => f.write_str(point),
            Self::Timer { id } => write!(f, "timer {id}"),
        }
    }
}

fn expired(at: Option<Instant>, now: Instant) -> bool {
    at.is_some_and(|at| now >= at)
}

fn timeout_ms(timeout: Duration) -> u64 {
    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
}

mod declared;
mod errors;
mod hub;
mod schedule;
mod setup;
mod vm;

pub(crate) use hub::Hub;
use hub::{CallbackTimeouts, Gate, Next, Phase, Shared};
pub(crate) use hub::{DeclaredHooks, HookPhase};
use vm::{Step, Vm};

pub(crate) use setup::Deadline;

#[cfg(test)]
#[path = "lua_tests.rs"]
mod tests;
