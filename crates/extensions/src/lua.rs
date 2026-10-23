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

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use contract::JobId;
use contract::clock::Clock;
use contract::diag::{DebugLog, Purpose};
use serde_json::{Value, json};

use crate::oauth::{Browser, LoginSlot, SystemBrowser};
use crate::{CredentialPair, Error, host, lua_provider::LoginMethod};
#[cfg(any(test, feature = "test-hooks"))]
pub use setup::observe_load;

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
/// `coroutine.create`, `require`, `fiber.command`, `fiber.provider`,
/// `fiber.hook`, `fiber.watch`, `fiber.tool` and `fiber.search_backend`, which fill the tables the prelude returns. Lua's own `wrap`
/// is a separate C function that would make an unarmed coroutine.
// A callback's timeout is in milliseconds (docs/extensions.md, "How an
// extension runs"). A hook's phase and `on_failure` are docs/extensions.md,
// "When several hooks share a point" and "When a hook fails".
pub(super) const PRELUDE: &str = r#"
local create, load_module, rethrow_panic, loading = ...
local commands, providers, hooks, watchers, problems, violations = {}, {}, {}, {}, {}, {}
local registered_files = {}

local function refuse_registration(message, level)
  local file = loading()
  if file then violations[#violations + 1] = file .. ': ' .. message end
  error(message, level or 3)
end

local function init_registration(kind)
  if loading() ~= 'init.lua' then
    refuse_registration('fiber.' .. kind .. ': registers only while init.lua runs', 4)
  end
end
local resume, status, yield, pack, unpack =
  coroutine.resume, coroutine.status, coroutine.yield, table.pack, table.unpack

coroutine.create = create
coroutine.wrap = function(f)
  local co = create(f)
  return function(...)
    local r = pack(resume(co, ...))
    if not r[1] then error(r[2], 0) end
    return unpack(r, 2, r.n)
  end
end

-- Like `pcall(f, ...)`, but `f` may suspend on a host call. The VM's `pcall`
-- cannot be yielded across, so `f` runs in a coroutine of its own and each
-- yield it makes is passed up to the host, and the answer back down. Every
-- error passes through unchanged (a host failure is already its
-- `{ code, message }` table), and every return survives. `rethrow_panic`
-- reads the error so a Rust panic resumes in Rust instead of being caught.
pcall = function(f, ...)
  local co = create(f)
  local args = pack(...)
  while true do
    local r = pack(resume(co, unpack(args, 1, args.n)))
    if not r[1] then
      rethrow_panic(r[2])
      return false, r[2]
    end
    if status(co) == "dead" then return true, unpack(r, 2, r.n) end
    args = pack(yield(unpack(r, 2, r.n)))
  end
end

xpcall = function(f, errfunc, ...)
  local r = pack(pcall(f, ...))
  if r[1] then return true, unpack(r, 2, r.n) end
  local h = pack(pcall(errfunc, r[2]))
  if h[1] then return false, unpack(h, 2, h.n) end
  return false, h[2]
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

local provider_functions = { models = true, quota = true, credential = true, sign = true, cost = true }

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
  init_registration('hook')
  local name = tostring(point)
  -- A declared entry names the problem, so a notice names the entry and
  -- the missing field. Without one the string is what it always was, so
  -- every existing pin holds.
  local entry = type(spec) == "table" and spec.entry or nil
  local function refuse(why)
    if type(entry) == "string" then
      problems[#problems + 1] = "`" .. name .. "` hook not registered for entry `" .. entry .. "`: " .. why
    else
      problems[#problems + 1] = "`" .. name .. "` hook not registered: " .. why
    end
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

-- A watcher that does not register leaves a problem for Fiber's notice, and the entry
-- script goes on. The id in the problem is the watcher's place among the
-- extension's registered watchers, from 0.
local function watch(kinds, spec)
  init_registration('watch')
  local id = #watchers
  local function refuse(why)
    problems[#problems + 1] = "`watcher:" .. id .. "` watcher not registered: " .. why
  end
  if type(kinds) ~= "table" then
    return refuse("`kinds` must be a list of event kinds")
  end
  local wanted = {}
  for i = 1, #kinds do
    local kind = kinds[i]
    if type(kind) ~= "string" or kind == "" then
      return refuse("`kinds` must be a list of event kinds")
    end
    wanted[#wanted + 1] = kind
  end
  if #wanted == 0 then
    return refuse("`kinds` must be a list of event kinds")
  end
  if type(spec) ~= "table" then
    return refuse("it takes a table of `timeout` and `run`")
  end
  if spec.timeout == nil then return refuse("missing `timeout`") end
  if math.type(spec.timeout) ~= "integer" or spec.timeout <= 0 then
    return refuse("`timeout` must be a whole number of milliseconds above 0")
  end
  if type(spec.run) ~= "function" then return refuse("`run` must be a function") end
  watchers[#watchers + 1] = { kinds = wanted, timeout = spec.timeout, run = spec.run }
end

fiber = {
  command = function(name, spec)
    init_registration('command')
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
    local file = loading()
    if file == 'init.lua' then
      refuse_registration('fiber.provider: `' .. tostring(name) .. '` belongs in `providers/' .. tostring(name) .. '.lua`, not `init.lua`')
    end
    if not file then
      refuse_registration('fiber.provider: registers only while its provider file loads')
    end
    local own = file:match('^providers/(.*)%.lua$')
    if name ~= own then
      refuse_registration('fiber.provider: `' .. file .. '` registers only `' .. tostring(own) .. '`, not `' .. tostring(name) .. '`')
    end
    if registered_files[file] then
      refuse_registration('fiber.provider: `' .. file .. '` registers its provider exactly once')
    end
    registered_files[file] = true
    if type(name) ~= "string" or type(spec) ~= "table" then
      error("fiber.provider: takes a name and a table of functions", 2)
    end
    local registered = {}
    for key, f in pairs(spec) do
      if not provider_functions[key] then
        error("fiber.provider: `" .. tostring(key) .. "` is not models, quota, credential, sign or cost", 2)
      end
      registered[key] = callback("fiber.provider: `" .. key .. "`", f)
    end
    providers[name] = registered
  end,
  hook = hook,
  watch = watch,
}

return commands, providers, hooks, watchers, problems, violations
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
    /// The names `host.secret` reads: the manifest's `secrets`.
    secrets: Vec<String>,
    /// Where each manifest `depends` entry lives: the directories a
    /// qualified `require` loads from, empty until the session attaches
    /// them.
    deps: BTreeMap<String, PathBuf>,
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
            secrets: Vec::new(),
            deps: BTreeMap::new(),
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

    pub(crate) fn with_state(self, state: Arc<host::state::StateRegistry>) -> Self {
        self.hub.lock().state = state.store(&self.name);
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

    /// Sets the names `host.secret` reads. Without it, `host.secret` reads
    /// none.
    pub fn with_secrets(mut self, names: Vec<String>) -> Self {
        self.secrets = names;
        self
    }

    /// Sets where each manifest `depends` entry lives, so a qualified
    /// `require` loads from the dependency's directory. Session start,
    /// login and the case runner attach it; without it only unqualified
    /// requires resolve.
    pub fn with_dependencies(mut self, deps: BTreeMap<String, PathBuf>) -> Self {
        self.deps = deps;
        self
    }

    /// Sets the host calls the case runner scripts for this extension
    /// (`docs/testing.md`, "Testing an extension").
    pub fn with_host_script(self, script: Arc<host::script::HostScript>) -> Self {
        self.hub.set_host_script(script);
        self
    }

    /// Installs the session's diagnostic sink on this VM, once: the first
    /// installation wins and a later one changes nothing. Runs no
    /// callback: use before provider work, not as live reconfiguration.
    pub fn with_diagnostics(self, debug: Arc<dyn DebugLog>) -> Self {
        self.hub.set_diagnostics(debug);
        self
    }

    /// Installs the sink on an already-shared VM, as
    /// [`SessionExtensions::with_diagnostics`](crate::SessionExtensions::with_diagnostics)
    /// does for live handles. The first installation wins.
    pub(crate) fn set_diagnostics(&self, debug: Arc<dyn DebugLog>) {
        self.hub.set_diagnostics(debug);
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

    /// Stops admission and waits for this VM's admitted exec deliveries.
    pub(crate) fn wait_for_runs(&self) {
        self.hub.wait_for_execs();
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
        if !self.ensure_provider(provider)? {
            return Ok(Vec::new());
        }
        self.registered(|timeouts| {
            timeouts
                .providers
                .get(provider)
                .map(|fns| fns.keys().cloned().collect())
                .unwrap_or_default()
        })
    }

    pub(crate) fn ensure_provider(&self, provider: &str) -> Result<bool, Error> {
        let asked = self.hub.clock().now();
        let mut shared = self.hub.lock();
        if !shared.loads.contains_key(provider) {
            if !self
                .dir
                .join("providers")
                .join(format!("{provider}.lua"))
                .is_file()
            {
                shared
                    .loads
                    .insert(provider.to_owned(), hub::ProviderLoad::Absent);
                return Ok(false);
            }
            self.start(&mut shared)?;
            shared
                .loads
                .insert(provider.to_owned(), hub::ProviderLoad::Queued { asked });
            shared.pending_loads.push_back(provider.to_owned());
            self.hub.notify();
        }
        loop {
            match shared.judge_load(&self.name, provider, asked, self.hub.clock().now()) {
                (Next::Return(result), unsent) => {
                    self.hub.notify();
                    drop(shared);
                    drop(unsent);
                    return result.map(|value| value == Value::Bool(true));
                }
                (Next::Sleep(until), unsent) => {
                    drop(unsent);
                    shared = self.hub.wait(shared, until);
                }
            }
        }
    }

    pub(crate) fn misplaced_provider(&self) -> bool {
        self.hub.lock().misplaced
    }

    /// Whether the VM's thread has been spawned, by a command, a provider's
    /// first use or a registration read, and so whether it has begun the entry
    /// script, whether it succeeded or failed.
    pub(crate) fn started(&self) -> bool {
        !matches!(self.hub.lock().phase, Phase::Idle)
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
    pub(crate) fn queue_command(
        &self,
        command: contract::CommandId,
        name: &str,
        text: &str,
    ) -> Result<Queued, Error> {
        let asked = self.hub.clock().now();
        let mut shared = self.hub.lock();
        self.start(&mut shared)?;
        // Registration must have finished before admission: the door admits
        // only listed names, so an unregistered name is refused here.
        loop {
            let now = self.hub.clock().now();
            // `unsent` is empty unless this call abandoned the extension,
            // which ends in `Stopped` below: stopping declines nothing on
            // any other arm.
            let (gate, unsent) = shared.gate(&self.name, now);
            let registered = match gate {
                Gate::Ready(timeouts) => timeouts.commands.contains_key(name),
                Gate::Stopped(e) => {
                    self.hub.notify();
                    drop(shared);
                    drop(unsent);
                    return Err(e);
                }
                Gate::Wait(until) => {
                    drop(unsent);
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
        let ended = Arc::new(command_calls::Ended::default());
        shared.runners.insert(
            id,
            Arc::new(command_calls::CommandCalls {
                hub: Arc::downgrade(&self.hub),
                command,
                call: id,
                ended: ended.clone(),
                #[cfg(test)]
                pause: std::sync::Mutex::default(),
            }),
        );
        drop(shared);
        self.hub.notify();
        Ok(Queued {
            hub: Arc::clone(&self.hub),
            name: self.name.clone(),
            id,
            target,
            asked,
            ended,
        })
    }

    /// Whether the loop is serving this extension's command's inner-call chain.
    pub(crate) fn runs_own_command_call(&self) -> bool {
        self.hub.lock().serving.is_some()
    }

    /// Hands the ephemeral emitter to `host.status`, `host.widget` and `host.emit`.
    pub(crate) fn set_emit(&self, emit: std::sync::Arc<dyn contract::emit::Emit>) {
        self.hub.set_emit(emit);
    }

    /// Hands the in-process driver to this extension's `host.drive`.
    pub(crate) fn set_driver(&self, drive: std::sync::Arc<dyn contract::extension::Drive>) {
        self.hub.set_driver(drive);
    }

    /// Hands the session's model to this extension's `host.model`.
    pub(crate) fn set_model(&self, model: std::sync::Arc<dyn contract::model::HostModel>) {
        self.hub.set_model(model);
    }

    /// Hands the session's delegate starter to this extension's `host.delegate`.
    pub(crate) fn set_delegate(
        &self,
        delegate: std::sync::Arc<dyn contract::extension::HostDelegate>,
    ) {
        self.hub.set_delegate(delegate);
    }

    /// Sets whether a client can answer this extension's `host.ask`.
    pub(crate) fn set_answerable(&self, yes: bool) {
        self.hub.lock().answerable = yes;
    }

    /// Answers a driver's `reply` from the ask it names, if this extension
    /// holds it; hands the reply back otherwise.
    pub(crate) fn answer(
        &self,
        reply: contract::commands::Reply,
        ack: contract::inbox::Ack,
    ) -> Option<(contract::commands::Reply, contract::inbox::Ack)> {
        self.hub.answer(reply, ack)
    }

    /// Emits an ephemeral event through the late-bound emitter.
    pub(crate) fn emit(&self, event: contract::events::Event) {
        self.hub.emit(event);
    }

    /// Drops every later emission and delivery from this extension.
    pub(crate) fn seal(&self) {
        self.hub.seal();
    }

    /// Records the extension's drop: anything routed after this is dropped.
    pub(crate) fn dispose(&self) {
        self.hub.dispose(&self.name);
    }

    /// Every hook the entry script registered, and why each one it tried
    /// and could not register was refused. Starts the extension.
    pub(crate) fn hooks(&self) -> Result<DeclaredHooks, Error> {
        self.registered(|timeouts| timeouts.hooks.clone())
    }

    /// Every watcher the entry script registered, and why each one it
    /// tried and could not register was refused. Starts the extension.
    pub(crate) fn watchers(&self) -> Result<DeclaredWatchers, Error> {
        self.registered(|timeouts| timeouts.watchers.clone())
    }

    /// Queues a watcher delivery as `Target::Watcher`, in the ordered
    /// stream behind whatever runs or waits. Refused while the extension
    /// is not running: nothing is queued, so the cursor must not advance
    /// for it.
    /// Pushes the watcher `index` for a session-end redelivery. A `None`
    /// push means the hub already stopped, which the drain answers as
    /// `Stopped`. Mid-session offers ignore the id and ask time.
    pub(crate) fn push_watcher(&self, index: usize, arg: Value) -> Option<(u64, Instant)> {
        let mut shared = self.hub.lock();
        if !matches!(shared.phase, Phase::Ready(_)) {
            return None;
        }
        let asked = self.hub.clock().now();
        let id = shared.push(Target::Watcher { index }, arg, asked);
        drop(shared);
        self.hub.notify();
        Some((id, asked))
    }

    /// Runs the watcher `index` for the session-end drain and waits for
    /// its end within its declared timeout: the drain's per-line wait.
    /// The failure comes back instead of a notice, so the drain's caller
    /// can print it under `fiber_exited` (`docs/extensions.md`, "When a
    /// session ends"). A push the stop refused answers `Stopped`, which
    /// ends the whole drain, and so does a wait the stop wakes.
    pub(crate) fn drain_watcher(&self, index: usize, arg: Value) -> Result<(), Error> {
        // The keep and the push join under one lock hold: a live delivery
        // finishing between them would otherwise be retained under its own
        // id with no waiter to collect it, losing its notice.
        let pushed = {
            let mut shared = self.hub.lock();
            if !matches!(shared.phase, Phase::Ready(_)) {
                None
            } else {
                let asked = self.hub.clock().now();
                let id = shared.push(Target::Watcher { index }, arg, asked);
                shared.keep_watcher_done = Some(id);
                drop(shared);
                self.hub.notify();
                Some((id, asked))
            }
        };
        let result = match pushed {
            None => Err(errors::stopped(&self.name)),
            Some((id, asked)) => {
                let target = Target::Watcher { index };
                let mut shared = self.hub.lock();
                loop {
                    let now = self.hub.clock().now();
                    match shared.judge(&self.name, id, &target, asked, now) {
                        (Next::Return(result), unsent) => {
                            self.hub.notify();
                            drop(shared);
                            drop(unsent);
                            break result.map(|_| ());
                        }
                        (Next::Sleep(until), unsent) => {
                            drop(unsent);
                            shared = self.hub.wait(shared, until);
                        }
                    }
                }
            }
        };
        self.hub.lock().keep_watcher_done = None;
        result
    }

    /// Waits until none of this extension's watcher deliveries is queued
    /// or running: the drain's pre-replay wait, so a durable delivery the
    /// live offer already covered has run before the drain replays past
    /// it (see `Hub::wait_for_watchers_idle`).
    pub(crate) fn wait_for_watchers_idle(&self) {
        self.hub.wait_for_watchers_idle();
    }

    /// The hub every waiter and the extension's thread share: what
    /// `SessionExtensions` reaches the delegate bindings through.
    pub(crate) fn hub(&self) -> &Hub {
        &self.hub
    }

    /// Whether the watcher has a delivery queued or running: the busy bit
    /// the thread sets on start and clears on finish, or a queued job the
    /// thread has not started. An ephemeral offer skips while this holds.
    pub(crate) fn is_watcher_busy(&self, index: usize) -> bool {
        let shared = self.hub.lock();
        shared.watcher_busy.contains(&index)
            || shared.queue.iter().any(
                |job| matches!(&job.target, Target::Watcher { index: queued } if *queued == index),
            )
    }

    pub(crate) fn hook_with<T>(
        &self,
        point: &str,
        index: usize,
        arg: Value,
        accept: impl FnOnce(Value) -> Result<(T, bool), Error>,
    ) -> Result<(T, Vec<contract::hook::StateLine>, bool), Error> {
        let (id, returned) = self.call_with_id(
            Target::Hook {
                point: point.to_owned(),
                index,
            },
            arg,
        );
        let state = self.hub.lock().state.clone();
        match returned.and_then(accept) {
            Ok((value, changed)) => Ok((value, state.take_call(id), changed)),
            Err(error) => {
                state.discard_call(id);
                Err(error)
            }
        }
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
            // As above: `unsent` is empty unless this call abandoned.
            let (gate, unsent) = shared.gate(&self.name, now);
            let until = match gate {
                Gate::Ready(timeouts) => return Ok(read(timeouts)),
                Gate::Stopped(e) => {
                    // This may be the waiter that abandoned registration.
                    self.hub.notify();
                    drop(shared);
                    drop(unsent);
                    return Err(e);
                }
                Gate::Wait(until) => until,
            };
            drop(unsent);
            shared = self.hub.wait(shared, until);
        }
    }

    /// Runs `function` (`models`, `quota`, `credential`, `sign` or `cost`) of the
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
        self.ensure_provider(provider)?;
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
    /// OAuth"). The only call whose target carries a stored credential, so
    /// the only one `host.oauth.refresh` refreshes against a file.
    pub(crate) fn provider_credential(
        &self,
        provider: &str,
        pair: &CredentialPair,
    ) -> Result<Value, Error> {
        self.ensure_provider(provider)?;
        self.call(
            Target::Provider {
                name: provider.to_owned(),
                function: "credential",
                credential: Some(CredentialFor::Stored(pair.clone())),
            },
            json!({"label": pair.label, "credential": pair.credential}),
        )
    }

    /// Runs `credential` of the provider `provider` as a login, passing the
    /// stored credential's name, the `--as` label when one was given, and
    /// whether the person chose the browser or a device code, and returns
    /// what it returned, as JSON (`docs/model-routing.md`, "Logging in").
    /// During the call `host.oauth.refresh` holds no file: what its function
    /// returns is kept in `slot` for `fiber login`, which stores it after
    /// the flow.
    pub(crate) fn provider_login(
        &self,
        provider: &str,
        credential: &str,
        label: Option<&str>,
        method: LoginMethod,
        slot: LoginSlot,
    ) -> Result<Value, Error> {
        self.ensure_provider(provider)?;
        let login = match method {
            LoginMethod::Browser => "browser",
            LoginMethod::Device => "device",
        };
        let mut arg = serde_json::Map::from_iter([
            (
                "credential".to_owned(),
                Value::String(credential.to_owned()),
            ),
            ("login".to_owned(), Value::String(login.to_owned())),
        ]);
        if let Some(label) = label {
            arg.insert("label".to_owned(), Value::String(label.to_owned()));
        }
        let arg = Value::Object(arg);
        self.call(
            Target::Provider {
                name: provider.to_owned(),
                function: "credential",
                credential: Some(CredentialFor::Login(slot)),
            },
            arg,
        )
    }

    /// Queues the call and waits on the hub until it is answered, or until
    /// its own limit passes ("Extension lifecycle" in `hub.rs`).
    fn call(&self, target: Target, arg: Value) -> Result<Value, Error> {
        self.call_with_id(target, arg).1
    }

    fn call_with_id(&self, target: Target, arg: Value) -> (u64, Result<Value, Error>) {
        // When Fiber asks, before it waits for the hub (`docs/extensions.md`).
        let asked = self.hub.clock().now();
        let mut shared = self.hub.lock();
        if let Err(error) = self.start(&mut shared) {
            return (0, Err(error));
        }
        let id = shared.push(target.clone(), arg, asked);
        // Only a change wakes the others: a waiter that notified on every
        // wake would keep every other waiter spinning.
        self.hub.notify();
        loop {
            // Under the hub lock, so a clock move cannot land between the
            // judgement and the park. `unsent` is empty unless this call
            // abandoned the extension, which returns.
            let now = self.hub.clock().now();
            match shared.judge(&self.name, id, &target, asked, now) {
                (Next::Return(result), unsent) => {
                    self.hub.notify();
                    drop(shared);
                    drop(unsent);
                    return (id, result);
                }
                (Next::Sleep(until), unsent) => {
                    drop(unsent);
                    shared = self.hub.wait(shared, until);
                }
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
    ended: Arc<command_calls::Ended>,
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
                (Next::Return(result), unsent) => {
                    self.clear(&mut shared);
                    self.hub.notify();
                    drop(shared);
                    self.ended.end();
                    drop(unsent);
                    return result;
                }
                (Next::Sleep(until), unsent) => {
                    drop(unsent);
                    shared = self.hub.wait(shared, until);
                }
            }
        }
    }
    /// Removes only this command's runner and serving window under the hub lock.
    fn clear(&self, shared: &mut Shared) {
        shared.runners.remove(&self.id);
        if shared.serving == Some(self.id) {
            shared.serving = None;
        }
    }
}

impl Drop for Queued {
    fn drop(&mut self) {
        {
            let mut shared = self.hub.lock();
            self.clear(&mut shared);
        }
        self.ended.end();
    }
}

/// What a `credential` call refreshes: a stored pair's file, or a login's
/// result slot, which holds no file (`docs/model-routing.md`, "Logging
/// in").
#[derive(Clone)]
pub(crate) enum CredentialFor {
    /// The session's stored credential and label.
    Stored(CredentialPair),
    /// A login's result slot.
    Login(LoginSlot),
}

/// What a call runs.
#[derive(Clone)]
pub(crate) enum Target {
    /// A command `fiber.command` registered.
    Command(String),
    /// A function `fiber.provider` registered.
    Provider {
        /// The provider.
        name: String,
        /// `models`, `quota`, `credential`, `sign` or `cost`.
        function: &'static str,
        /// What the call refreshes: `Some` only for `credential`, whose
        /// `host.oauth.refresh` locks and reads exactly this pair's file,
        /// or fills its login slot.
        credential: Option<CredentialFor>,
    },
    /// A hook `fiber.hook` registered.
    Hook {
        /// The hook point.
        point: String,
        /// Its place among the hooks registered there, from 0.
        index: usize,
    },
    /// A watcher `fiber.watch` registered.
    Watcher {
        /// Its place among the extension's watchers, from 0.
        index: usize,
    },
    /// A timer `host.after` or `host.every` set, firing on the hub's
    /// clock in the gaps of the session's stream.
    Timer {
        /// Assigned in set order from 0.
        id: u64,
    },
    /// The `run` of a tool `fiber.tool` registered, in the gaps of the
    /// stream.
    Tool(String),
    /// The `run` of a search backend `fiber.search_backend` registered,
    /// in the gaps of the stream.
    Search(String),
    /// The `effects` function of a tool `fiber.tool` registered, in the gaps
    /// of the stream.
    Effects(String),
    /// One extension-owned delegate's end: its `run` with the completion's
    /// table, in the ordered stream. The per-call `timeout` rides along
    /// from the `host.delegate` yield, judged from the push instant.
    Delegate {
        /// The extension whose `run` runs.
        extension: String,
        /// The delegate job that ended.
        job_id: JobId,
        /// The chunk-local `run` handle this start stored.
        handle: u64,
        /// The per-call timeout the yield carried.
        timeout: Duration,
    },
}

/// The eligible provider purpose of `target`: the provider's registered
/// name and why an HTTP request it makes is recorded. `models`, `quota`,
/// `credential` (a stored credential and a login alike) and `cost` are
/// recorded; `sign` and every other target never are, whatever HTTP they
/// send. A pure mapping of the value, as the seam holds no behaviour.
pub(crate) fn provider_purpose(target: &Target) -> Option<(&str, Purpose)> {
    let Target::Provider { name, function, .. } = target else {
        return None;
    };
    let purpose = match *function {
        "models" => Purpose::ModelList,
        "quota" => Purpose::Quota,
        "credential" => Purpose::TokenRefresh,
        "cost" => Purpose::Cost,
        _ => return None,
    };
    Some((name, purpose))
}

/// Snapshots the owned observation context for an HTTP request about to
/// run as `target`: the provider's registered name and the eligible
/// purpose, with the installed sink and clock. None unless the target is
/// an eligible provider purpose, a sink is installed and it records debug
/// lines. No timer starts and nothing is written here: the admitted
/// worker observes once it executes.
pub(crate) fn http_observation(hub: &Hub, target: &Target) -> Option<host::HttpObservation> {
    let (provider, purpose) = provider_purpose(target)?;
    let debug = hub.diagnostics()?;
    if !debug.debugging() {
        return None;
    }
    Some(host::HttpObservation {
        provider: provider.to_owned(),
        purpose,
        debug,
        clock: hub.clock_handle(),
    })
}
/// The callback's name as an error names it: a command's name,
/// `<provider>.<function>`, a hook's point, `watcher:<index>`, a tool's name, a search
/// backend's name, or
/// `<tool>.effects`.
impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Command(name) => f.write_str(name),
            Self::Provider { name, function, .. } => write!(f, "{name}.{function}"),
            Self::Hook { point, .. } => f.write_str(point),
            Self::Watcher { index } => write!(f, "watcher:{index}"),
            Self::Timer { id } => write!(f, "timer {id}"),
            Self::Tool(name) => f.write_str(name),
            Self::Search(name) => f.write_str(name),
            Self::Effects(name) => write!(f, "{name}.effects"),
            Self::Delegate {
                extension, job_id, ..
            } => write!(f, "{extension}.run {}", job_id.0),
        }
    }
}

fn expired(at: Option<Instant>, now: Instant) -> bool {
    at.is_some_and(|at| now >= at)
}

fn timeout_ms(timeout: Duration) -> u64 {
    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
}

pub(crate) mod asks;
mod command_calls;
mod declared;
mod delegates;
mod errors;
mod host_calls;
pub(crate) mod hub;
mod router;
mod schedule;
mod search;
mod setup;
mod timers;
mod tool;
mod vm;

#[cfg(test)]
pub(crate) use host_calls::{
    Pause, pause_observed_execution, pause_settle, unpause_observed_execution, unpause_settle,
};
pub(crate) use hub::Hub;
use hub::{CallbackTimeouts, Gate, Next, Phase, Shared};
pub(crate) use hub::{DeclaredHooks, DeclaredWatchers, HookPhase};
#[cfg(test)]
pub(crate) use schedule::fail_next_spawn_for;
pub(crate) use tool::{DeclaredTool, effects_from, urls_from};
use vm::{Step, Vm};

pub(crate) use setup::Deadline;
#[cfg(test)]
pub(crate) use setup::install_prelude;

/// Observes a completed timer callback in tests without waiting on the wall clock.
#[cfg(test)]
pub(crate) fn on_timer_done(hook: impl Fn() + Send + Sync + 'static) {
    host_calls::pause_settle(Arc::new(move |_, pause| {
        if pause == host_calls::Pause::TimerDone {
            hook();
        }
    }));
}

#[cfg(test)]
pub(crate) fn clear_timer_done_hook() {
    host_calls::unpause_settle();
}

#[cfg(test)]
#[path = "lua_tests.rs"]
mod tests;
