//! Native process registrations (`docs/extensions.md`, "Process extensions"
//! and "Registering"): the validated registry behind the wire's JSON
//! registration calls. The checks live in `validate`, with the Lua
//! registry's exact wording, without running Lua; this owns the state,
//! mints the references, and inserts.
//!
//! Each `fiber.*` call validates exactly as its Lua twin does: a refusal
//! carries the problem's sentence back in `command_rejected` and records it
//! for the notice, and the package goes on. Only a provider or replacement
//! the manifest does not declare, or a registration outside a commands-keyed
//! package's declared commands, fails the whole package with
//! `extension_failed`. Every function a call registers is a
//! [`FunctionRef`][crate::runtime::FunctionRef]: the registering command's
//! id, the function's key and the owner's generation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use contract::CommandId;
use contract::ErrorCode;
use serde_json::Value;

use crate::Error;
use crate::lua::{DeclaredHooks, DeclaredTool, DeclaredWatchers};
use crate::runtime::{FunctionRef, FunctionTarget, ProcessInvoke, Runtime};

mod validate;

pub(crate) mod calls;
pub(crate) mod context;
pub(crate) mod requests;
#[cfg(test)]
mod tests;
pub(crate) mod transport;

/// What one registration call answers on the wire: accepted with its
/// result, rejected with the refusal's sentence, or failing the package.
#[allow(
    dead_code,
    reason = "answered by the transport once processes register in a later part; asserted by tests"
)]
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RegisterOutcome {
    /// The registration stands; the `call` answers `command_accepted`
    /// with no `value`, as a Lua registration returns nothing
    /// (`docs/extensions.md`, "Process extensions").
    Accepted,
    /// The registration does not stand; the `call` answer carries the
    /// refusal's code and sentence, and the sentence is recorded for the
    /// notice.
    Rejected {
        /// The refusal's code.
        code: ErrorCode,
        /// The refusal's sentence, in the Lua call's words.
        message: String,
    },
    /// The package is out; the `call` answer carries the failure, the
    /// sentence is recorded for the notice, and no later call registers.
    FailedPackage {
        /// The failure's code.
        code: ErrorCode,
        /// The failure's sentence.
        message: String,
    },
}

/// One tool a `fiber.tool` call registered.
#[derive(Debug, Clone)]
pub(crate) struct ToolEntry {
    /// Its declaration, as `fiber.tool` validated it.
    pub(crate) declared: DeclaredTool,
    /// Its `run` behind the runtime.
    pub(crate) run: FunctionRef,
    /// Its computed `effects` behind the runtime, when it declared a
    /// function instead of a table.
    pub(crate) effects: Option<FunctionRef>,
}

/// One command a `fiber.command` call registered.
#[allow(
    dead_code,
    reason = "timeouts are enforced when the command runs in a later part; asserted by tests"
)]
#[derive(Debug, Clone)]
pub(crate) struct CommandEntry {
    /// Its description, as the session lists it.
    pub(crate) description: String,
    /// Its timeout, enforced when the command runs.
    pub(crate) timeout: Duration,
    /// Its `run` behind the runtime.
    pub(crate) run: FunctionRef,
}

/// One provider a `fiber.provider` call registered.
#[allow(
    dead_code,
    reason = "read by provider dispatch in a later part; asserted by tests"
)]
#[derive(Debug, Clone, Default)]
pub(crate) struct ProviderEntry {
    /// Each function's timeout, by function name.
    pub(crate) timeouts: BTreeMap<String, Duration>,
    /// Each function's `run` behind the runtime, by function name.
    pub(crate) runs: BTreeMap<String, FunctionRef>,
}

/// One harness a `fiber.harness` call registered.
#[allow(
    dead_code,
    reason = "read by harness consumption in a later part; asserted by tests"
)]
#[derive(Debug, Clone)]
pub(crate) struct HarnessEntry {
    /// The timeout every harness function runs under.
    pub(crate) timeout: Duration,
    /// The harness's data, carried for the delegate runner.
    pub(crate) data: BTreeMap<String, Value>,
    /// Each function's `run` behind the runtime, by function name.
    pub(crate) runs: BTreeMap<String, FunctionRef>,
}

/// A process extension's validated registrations: declarations beside the
/// references that run them.
#[allow(
    dead_code,
    reason = "filled by the transport once processes register in a later part; views asserted by tests"
)]
pub(crate) struct Registry {
    /// The extension that registers, as `extensions_loaded` carries it.
    owner: String,
    /// The runtime minting the references.
    runtime: Runtime,
    /// The manifest's `commands` key, when it makes the package lazy: only
    /// these command names may register, and nothing else may.
    manifest_commands: Option<BTreeMap<String, config::ManifestCommand>>,
    /// The manifest's declared providers.
    manifest_providers: Vec<String>,
    /// The manifest's declared replacements.
    manifest_replaces: Vec<String>,
    /// Each tool by name, in registration order.
    tools: BTreeMap<String, ToolEntry>,
    /// Each hook by point, in registration order.
    hooks: DeclaredHooks,
    /// Each hook's `run` behind the runtime, by point and index.
    hook_runs: BTreeMap<(String, usize), FunctionRef>,
    /// Each watcher in registration order.
    watchers: DeclaredWatchers,
    /// Each watcher's `run` behind the runtime, by registration index.
    watcher_runs: BTreeMap<usize, FunctionRef>,
    /// Each command by registered name.
    commands: BTreeMap<String, CommandEntry>,
    /// Each provider by name.
    providers: BTreeMap<String, ProviderEntry>,
    /// Each search backend by name, with its timeout.
    search: BTreeMap<String, (Duration, FunctionRef)>,
    /// Each harness by name.
    harness: BTreeMap<String, HarnessEntry>,
    /// Every refusal but the watch ones, for the notice.
    problems: Vec<String>,
    /// Every watch refusal, for the notice.
    watch_problems: Vec<String>,
    /// The first package failure, latched: a failed package stays failed,
    /// and later calls fail with the first failure.
    failure: Option<(ErrorCode, String)>,
}

#[allow(
    dead_code,
    reason = "the register pathway and its views: the transport answers through it in a later part; asserted by tests"
)]
impl Registry {
    /// An empty registry for `owner` on `runtime`, constrained by
    /// `manifest`: its `commands` key, providers and replacements. The
    /// owner registers at once, so every minted reference carries
    /// generation one until a replacement re-registers.
    pub(crate) fn new(owner: String, runtime: Runtime, manifest: &config::Manifest) -> Self {
        runtime.register_owner(&owner, None);
        Self {
            owner,
            runtime,
            manifest_commands: manifest.commands.clone(),
            manifest_providers: manifest.providers.keys().cloned().collect(),
            manifest_replaces: manifest.replaces.clone(),
            tools: BTreeMap::new(),
            hooks: DeclaredHooks {
                by_point: BTreeMap::new(),
                problems: Vec::new(),
            },
            hook_runs: BTreeMap::new(),
            watchers: DeclaredWatchers {
                watchers: Vec::new(),
                problems: Vec::new(),
            },
            watcher_runs: BTreeMap::new(),
            commands: BTreeMap::new(),
            providers: BTreeMap::new(),
            search: BTreeMap::new(),
            harness: BTreeMap::new(),
            problems: Vec::new(),
            watch_problems: Vec::new(),
            failure: None,
        }
    }

    /// The extension this registry collects.
    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    /// Each tool by name, in registration order.
    pub(crate) fn tools(&self) -> &BTreeMap<String, ToolEntry> {
        &self.tools
    }

    /// Each hook by point, in registration order.
    pub(crate) fn hooks(&self) -> &DeclaredHooks {
        &self.hooks
    }

    /// Each hook's `run` behind the runtime, by point and index.
    pub(crate) fn hook_runs(&self) -> &BTreeMap<(String, usize), FunctionRef> {
        &self.hook_runs
    }

    /// Each watcher in registration order.
    pub(crate) fn watchers(&self) -> &DeclaredWatchers {
        &self.watchers
    }

    /// Each watcher's `run` behind the runtime, by registration index.
    pub(crate) fn watcher_runs(&self) -> &BTreeMap<usize, FunctionRef> {
        &self.watcher_runs
    }

    /// Each command by registered name.
    pub(crate) fn commands(&self) -> &BTreeMap<String, CommandEntry> {
        &self.commands
    }

    /// Each provider by name.
    pub(crate) fn providers(&self) -> &BTreeMap<String, ProviderEntry> {
        &self.providers
    }

    /// Each search backend by name, with its timeout.
    pub(crate) fn search(&self) -> &BTreeMap<String, (Duration, FunctionRef)> {
        &self.search
    }

    /// Each harness by name.
    pub(crate) fn harness(&self) -> &BTreeMap<String, HarnessEntry> {
        &self.harness
    }

    /// Every refusal but the watch ones, for the notice.
    pub(crate) fn problems(&self) -> &[String] {
        &self.problems
    }

    /// Every watch refusal, for the notice.
    pub(crate) fn watch_problems(&self) -> &[String] {
        &self.watch_problems
    }

    /// The first package failure, when one call failed the package.
    pub(crate) fn failure(&self) -> Option<(ErrorCode, String)> {
        self.failure.clone()
    }

    /// Registers one `call` command `command` with `args`: the wire's
    /// answer to the call. `in_callback` marks a call made while answering
    /// another, which registers nothing, as in Lua. Unknown calls are not
    /// registrations and fail `invalid_arguments`.
    pub(crate) fn register(
        &mut self,
        command: CommandId,
        call: &str,
        args: &[Value],
        in_callback: bool,
    ) -> RegisterOutcome {
        if let Some((code, message)) = self.failure.clone() {
            return RegisterOutcome::FailedPackage { code, message };
        }
        if in_callback {
            return RegisterOutcome::Rejected {
                code: ErrorCode::ExtensionFailed,
                message: in_callback_message(call),
            };
        }
        let outcome = match call {
            "fiber.tool" => self.register_tool(command, args),
            "fiber.hook" => self.register_hook(command, args),
            "fiber.watch" => self.register_watch(command, args),
            "fiber.command" => self.register_command(command, args),
            "fiber.provider" => self.register_provider(command, args),
            "fiber.search_backend" => self.register_search(command, args),
            "fiber.harness" => self.register_harness(command, args),
            _ => RegisterOutcome::Rejected {
                code: ErrorCode::InvalidArguments,
                message: format!("unknown call `{call}`"),
            },
        };
        if let RegisterOutcome::FailedPackage { code, message } = &outcome {
            self.failure = Some((code.clone(), message.clone()));
        }
        outcome
    }

    /// Installs `invoke` for the function (`command`, `key`): the test and
    /// transport stand-in behind the reference the registration minted.
    /// Re-registering the reference swaps its target under the same
    /// generation, so the call that registered it keeps working.
    pub(crate) fn set_handler(
        &self,
        command: &CommandId,
        key: &str,
        invoke: ProcessInvoke,
    ) -> FunctionRef {
        self.runtime.register_function(
            &self.owner,
            command.clone(),
            key.to_owned(),
            FunctionTarget::Process(invoke),
        )
    }

    /// Fails the package when it declares `commands` and `call` is not a
    /// command registration: a lazy package registers only commands.
    fn reject_lazy(&mut self, call: &str) -> Option<RegisterOutcome> {
        self.manifest_commands.as_ref()?;
        let message = format!(
            "`{}` declares `commands`, so a `{call}` registration stops the load",
            self.owner
        );
        self.problems.push(message.clone());
        Some(RegisterOutcome::FailedPackage {
            code: ErrorCode::ExtensionFailed,
            message,
        })
    }
}

/// A registration made while answering another call registers nothing, as
/// in Lua: each call names where registrations run.
fn in_callback_message(call: &str) -> String {
    match call {
        "fiber.tool" => "fiber.tool: registers only while `init.lua` runs",
        "fiber.command" => "fiber.command: registers only while `init.lua` runs",
        "fiber.hook" => "fiber.hook: registers only while `init.lua` runs",
        "fiber.watch" => "fiber.watch: registers only while `init.lua` runs",
        "fiber.search_backend" => "fiber.search_backend: registers only while `init.lua` runs",
        "fiber.provider" => "fiber.provider: registers only while its provider file loads",
        "fiber.harness" => "fiber.harness: registers only while `init.lua` runs",
        _ => "registers only while `init.lua` runs",
    }
    .to_owned()
}

/// Records `message` for the notice and rejects the call with it.
fn refuse(problems: &mut Vec<String>, message: String) -> RegisterOutcome {
    problems.push(message.clone());
    RegisterOutcome::Rejected {
        code: ErrorCode::ExtensionFailed,
        message,
    }
}

/// A `fiber.tool` registration: the validated declaration, inserted with
/// its references. The checks live in `validate`, in `fiber.tool`'s order;
/// this refuses a duplicate name and mints.
impl Registry {
    /// Registers `fiber.tool` from `command` with `args`.
    fn register_tool(&mut self, command: CommandId, args: &[Value]) -> RegisterOutcome {
        if let Some(fatal) = self.reject_lazy("fiber.tool") {
            return fatal;
        }
        let valid = match validate::check_tool(args) {
            Ok(valid) => valid,
            Err(why) => return refuse(&mut self.problems, why),
        };
        if self.tools.contains_key(&valid.tool.name) {
            let message = format!("`{}` tool is already registered", valid.tool.name);
            self.problems.push(message.clone());
            return RegisterOutcome::Rejected {
                code: ErrorCode::ExtensionFailed,
                message,
            };
        }
        let run = self.mint(command.clone(), "run");
        let effects_reference = valid.effects_fn.then(|| self.mint(command, "effects"));
        self.tools.insert(
            valid.tool.name.clone(),
            ToolEntry {
                declared: valid.tool,
                run,
                effects: effects_reference,
            },
        );
        RegisterOutcome::Accepted
    }

    /// Mints the reference for (`command`, `key`): a process function that
    /// answers nothing until its handler is installed.
    fn mint(&self, command: CommandId, key: &str) -> FunctionRef {
        let owner = self.owner.clone();
        self.runtime.register_function(
            &self.owner,
            command,
            key.to_owned(),
            FunctionTarget::Process(Arc::new(move |_, _| {
                Err(Error::Stopped {
                    extension: owner.clone(),
                })
            })),
        )
    }
}

/// A `fiber.hook` registration: the validated point and declaration,
/// numbered among its point's hooks and inserted with its reference. The
/// checks live in `validate`, in `fiber.hook`'s order.
impl Registry {
    /// Registers `fiber.hook` from `command` with `args`.
    fn register_hook(&mut self, command: CommandId, args: &[Value]) -> RegisterOutcome {
        if let Some(fatal) = self.reject_lazy("fiber.hook") {
            return fatal;
        }
        let valid = match validate::check_hook(args) {
            Ok(valid) => valid,
            Err(why) => return refuse(&mut self.problems, why),
        };
        let index = self
            .hooks
            .by_point
            .get(&valid.point)
            .map(Vec::len)
            .unwrap_or(0);
        let run = self.mint(command, "run");
        self.hook_runs.insert((valid.point.clone(), index), run);
        self.hooks
            .by_point
            .entry(valid.point)
            .or_default()
            .push(valid.hook);
        RegisterOutcome::Accepted
    }

    /// Registers `fiber.watch` from `command` with `args`: the validated
    /// watcher, numbered and inserted with its reference. The checks live
    /// in `validate`; unknown kinds keep their notices and register
    /// nothing, as in Lua.
    fn register_watch(&mut self, command: CommandId, args: &[Value]) -> RegisterOutcome {
        if let Some(fatal) = self.reject_lazy("fiber.watch") {
            return fatal;
        }
        // The index counts registered watchers, as Lua's `#watchers` does:
        // a refused call registers nothing and takes no number.
        let index = self.watchers.watchers.len();
        let checked = validate::check_watch(args, index);
        self.watch_problems.extend(checked.notices);
        match checked.result {
            Err(why) => refuse(&mut self.watch_problems, why),
            Ok(None) => RegisterOutcome::Accepted,
            Ok(Some(valid)) => {
                let run = self.mint(command, "run");
                self.watcher_runs.insert(index, run);
                self.watchers.watchers.push(valid.watcher);
                RegisterOutcome::Accepted
            }
        }
    }

    /// Registers `fiber.command` from `command` with `args`: the validated
    /// entry, inserted with its reference. The checks live in `validate`,
    /// in `fiber.command`'s order; the manifest check follows them, and a
    /// later call for the same name replaces the first, as in Lua.
    fn register_command(&mut self, command: CommandId, args: &[Value]) -> RegisterOutcome {
        let valid = match validate::check_command(args) {
            Ok(valid) => valid,
            Err(why) => return refuse(&mut self.problems, why),
        };
        if let Some(declared) = &self.manifest_commands
            && !declared.contains_key(&valid.name)
        {
            let message = format!(
                "Command `{}` is not declared under `commands`; `{}` registers only the commands its manifest declares",
                valid.name, self.owner
            );
            self.problems.push(message.clone());
            return RegisterOutcome::FailedPackage {
                code: ErrorCode::ExtensionFailed,
                message,
            };
        }
        let run = self.mint(command, "run");
        self.commands.insert(
            valid.name.clone(),
            CommandEntry {
                description: valid.description,
                timeout: valid.timeout,
                run,
            },
        );
        RegisterOutcome::Accepted
    }

    /// Registers `fiber.search_backend` from `command` with `args`: the
    /// validated backend, inserted with its reference. The checks live in
    /// `validate`, in `fiber.search_backend`'s order. Unlike commands, a
    /// backend name need not be plain.
    fn register_search(&mut self, command: CommandId, args: &[Value]) -> RegisterOutcome {
        if let Some(fatal) = self.reject_lazy("fiber.search_backend") {
            return fatal;
        }
        let registered: BTreeSet<String> = self.search.keys().cloned().collect();
        let valid = match validate::check_search(args, &registered) {
            Ok(valid) => valid,
            Err(why) => return refuse(&mut self.problems, why),
        };
        let run = self.mint(command, "run");
        self.search.insert(valid.name.clone(), (valid.timeout, run));
        RegisterOutcome::Accepted
    }

    /// Registers `fiber.provider` from `command` with `args`: the validated
    /// provider, inserted with one reference per function. The checks live
    /// in `validate`, in `fiber.provider`'s order, reading the manifest
    /// and the standing providers as the registry holds them. The provider
    /// name stands where Lua names the provider file, which a process has
    /// none of.
    fn register_provider(&mut self, command: CommandId, args: &[Value]) -> RegisterOutcome {
        if let Some(fatal) = self.reject_lazy("fiber.provider") {
            return fatal;
        }
        let registered: BTreeSet<String> = self.providers.keys().cloned().collect();
        match validate::check_provider(args, &self.owner, &self.manifest_providers, &registered) {
            validate::Checked::Accept(valid) => {
                let mut runs = BTreeMap::new();
                for key in &valid.funcs {
                    runs.insert(key.clone(), self.mint(command.clone(), key));
                }
                self.providers.insert(
                    valid.name.clone(),
                    ProviderEntry {
                        timeouts: valid.timeouts,
                        runs,
                    },
                );
                RegisterOutcome::Accepted
            }
            validate::Checked::Refuse(message) => refuse(&mut self.problems, message),
            validate::Checked::Fail(code, message) => {
                self.problems.push(message.clone());
                RegisterOutcome::FailedPackage { code, message }
            }
        }
    }

    /// Registers `fiber.harness` from `command` with `args`: the validated
    /// harness, inserted with its references. The checks live in
    /// `validate`, with the Lua call's naming and timeout rules, since no
    /// Lua registry exists to mirror. Data keys ride along for the
    /// delegate runner; only the functions run.
    fn register_harness(&mut self, command: CommandId, args: &[Value]) -> RegisterOutcome {
        if let Some(fatal) = self.reject_lazy("fiber.harness") {
            return fatal;
        }
        let valid = match validate::check_harness(args) {
            Ok(valid) => valid,
            Err(why) => return refuse(&mut self.problems, why),
        };
        if self.harness.contains_key(&valid.name) {
            let message = format!("`{}` harness is already registered", valid.name);
            self.problems.push(message.clone());
            return RegisterOutcome::Rejected {
                code: ErrorCode::ExtensionFailed,
                message,
            };
        }
        let mut runs = BTreeMap::new();
        for key in &valid.funcs {
            runs.insert(key.clone(), self.mint(command.clone(), key));
        }
        self.harness.insert(
            valid.name.clone(),
            HarnessEntry {
                timeout: valid.timeout,
                data: valid.data,
                runs,
            },
        );
        RegisterOutcome::Accepted
    }
}

/// One process extension behind the package seam: its child on the pipes,
/// its registrations, and the requests it answers (`docs/extensions.md`,
/// "Process extensions").
///
/// The owner pumps the child's lines on one thread: registrations answer
/// through the registry before `ready` and refuse after it, supervised
/// `call`, `ready` and `reply` parse through `CommandLine`, and ordinary
/// driver commands go through the `Drive` seam. Requests ride the shared
/// request table: stream items ordered one at a time, tool calls
/// concurrent, and one retirement seam for replies, deadlines and cancels.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) struct ProcessExtension {
    shared: Arc<Shared>,
    ready: std::sync::atomic::AtomicBool,
}

/// What starting one process extension takes: the program and its
/// arguments, what the child runs as, and what answers its calls. One
/// value carries the launch so the start path stays narrow.
pub(crate) struct Launch {
    /// The extension's name.
    pub owner: String,
    /// The runtime its functions resolve through.
    pub runtime: Runtime,
    /// The registry its calls register with, shared with the package.
    pub registry: Arc<Mutex<Registry>>,
    /// The program to run.
    pub program: std::path::PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// What the child runs as: the hello's fields.
    pub context: context::ProcessContext,
    /// What answers its driver commands, absent while closing.
    pub drive: Option<Arc<dyn contract::extension::Drive>>,
    /// The clock its timeouts read.
    pub clock: Arc<dyn contract::clock::Clock>,
    /// The pipe limits; the tests inject small ones.
    pub limits: transport::Limits,
}

/// What the owner's threads share: the extension's identity, its runtime
/// and registry, its request table and pipes, and what answers its calls.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
struct Shared {
    owner: String,
    runtime: Runtime,
    registry: Arc<Mutex<Registry>>,
    table: Arc<requests::Table>,
    transport: transport::Transport,
    drive: Option<Arc<dyn contract::extension::Drive>>,
    clock: Arc<dyn contract::clock::Clock>,
    context: context::ProcessContext,
}

/// What one `pump_once` did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum Pumped {
    /// One line handled and answered.
    Handled,
    /// The pipe closed; no more lines follow.
    Closed(transport::CloseReason),
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
impl ProcessExtension {
    /// Starts the launched program for its owner: the child runs in the
    /// launch's workspace, and `extension_hello` is the first complete
    /// outbound line. The registry is shared with the package, so the
    /// session fold reads what the child registers. One value carries
    /// the launch so the start path stays narrow.
    pub(crate) fn start(launch: Launch) -> std::io::Result<Self> {
        let Launch {
            owner,
            runtime,
            registry,
            program,
            args,
            context,
            drive,
            clock,
            limits,
        } = launch;
        let transport =
            transport::Transport::spawn_with(&program, &args, &context.workspace, limits)?;
        let extension = Self {
            shared: Arc::new(Shared {
                owner,
                runtime,
                registry,
                table: Arc::new(requests::Table::new()),
                transport,
                drive,
                clock,
                context,
            }),
            ready: std::sync::atomic::AtomicBool::new(false),
        };
        let hello = extension.hello_line().map_err(std::io::Error::other)?;
        extension.shared.transport.send(hello).map_err(|error| {
            std::io::Error::other(format!("the first hello never sent: {error:?}"))
        })?;
        Ok(extension)
    }

    /// The extension's name.
    pub(crate) fn owner(&self) -> &str {
        &self.shared.owner
    }

    /// Whether `ready` arrived: registrations stand only before it.
    pub(crate) fn is_ready(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The request table, for attributing calls and retiring generations.
    pub(crate) fn table(&self) -> &Arc<requests::Table> {
        &self.shared.table
    }

    /// The pipes, for stopping, signalling and reaping the child.
    pub(crate) fn transport(&self) -> &transport::Transport {
        &self.shared.transport
    }

    /// The runtime, resolving generation-keyed references.
    pub(crate) fn runtime(&self) -> &Runtime {
        &self.shared.runtime
    }

    /// Handles one line or close from the child, answering it.
    pub(crate) fn pump_once(&self) -> Pumped {
        match self.shared.transport.recv() {
            transport::Incoming::Line(bytes) => {
                self.handle_line(&bytes);
                Pumped::Handled
            }
            transport::Incoming::Closed(reason) => Pumped::Closed(reason),
        }
    }

    /// Dispatches the stream item behind `func`: hooks, watcher
    /// deliveries and commands, one at a time in arrival order. Resolves
    /// the reference first, so a stale generation never runs.
    pub(crate) fn dispatch_stream(
        &self,
        func: &FunctionRef,
        args: Value,
        timeout: Duration,
        state_call: Option<u64>,
        caller: &dyn contract::tool::Cancel,
    ) -> Result<Option<Value>, Error> {
        self.shared.runtime.resolve(func)?;
        match self.family_of(func) {
            Some(Family::Hook { point }) => self.send_request(
                Dispatch::stream(func, format!("hook {point}"), args, timeout, state_call),
                caller,
            ),
            Some(Family::Watcher { index }) => self.send_request(
                Dispatch::stream(func, format!("watcher {index}"), args, timeout, state_call),
                caller,
            ),
            Some(Family::Command { name }) => self.send_request(
                Dispatch::stream(func, format!("command {name}"), args, timeout, state_call),
                caller,
            ),
            _ => Err(Error::UnknownCallback {
                extension: self.shared.owner.clone(),
                callback: format!(
                    "`{}` (generation {}): it has since re-registered",
                    func.key, func.generation
                ),
            }),
        }
    }

    /// Dispatches the tool behind `func`: tools run as they arrive, so
    /// several can be outstanding and a long tool never holds up the
    /// stream. Resolves the reference first, so a stale generation never
    /// runs.
    pub(crate) fn dispatch_tool(
        &self,
        func: &FunctionRef,
        args: Value,
        timeout: Duration,
        caller: &dyn contract::tool::Cancel,
    ) -> Result<Option<Value>, Error> {
        self.shared.runtime.resolve(func)?;
        match self.family_of(func) {
            Some(Family::Tool { name }) => self.send_request(
                Dispatch::tool(func, format!("tool {name}"), args, timeout),
                caller,
            ),
            _ => Err(Error::UnknownCallback {
                extension: self.shared.owner.clone(),
                callback: format!(
                    "`{}` (generation {}): it has since re-registered",
                    func.key, func.generation
                ),
            }),
        }
    }

    /// Sends one `dispatch` and waits for its answer, as the free
    /// `run_request` does: the stream gate first for stream items, then
    /// the line, then the reply, the deadline, the cancel or the hang
    /// guard, whichever is first.
    fn send_request(
        &self,
        dispatch: Dispatch,
        caller: &dyn contract::tool::Cancel,
    ) -> Result<Option<Value>, Error> {
        run_request(&self.shared, dispatch, caller)
    }

    /// Handles one complete child line, answering it.
    fn handle_line(&self, bytes: &[u8]) {
        match transport::parse_line(bytes) {
            transport::ParsedLine::Call {
                id,
                call,
                args,
                request_id,
            } => self.handle_call(id, call, args, request_id),
            transport::ParsedLine::Ready { id } => {
                self.ready.store(true, std::sync::atomic::Ordering::SeqCst);
                self.answer_accepted(&id, None);
            }
            transport::ParsedLine::Reply {
                id,
                request_id,
                answer,
            } => {
                let outcome = match answer {
                    transport::ReplyAnswer::Value(result) => {
                        requests::Outcome::Answered(requests::Answer::Value(result))
                    }
                    transport::ReplyAnswer::Failure { code, message } => {
                        requests::Outcome::Answered(requests::Answer::Failure { code, message })
                    }
                };
                match self.shared.table.settle(&request_id, outcome) {
                    requests::Settled::Completed => self.answer_accepted(&id, None),
                    requests::Settled::Stale => self.answer_stale(&id),
                }
            }
            transport::ParsedLine::Forward { id, command, args } => {
                self.drive_command(id, command, args);
            }
            transport::ParsedLine::Unknown { id, command } => {
                self.answer_rejected(&id, ErrorCode::UnknownCommand, not_built(&command));
            }
            transport::ParsedLine::BadReply { id } => {
                self.answer_rejected(
                    &id,
                    ErrorCode::InvalidArguments,
                    "a `reply` answers with `result` or `error`.".to_owned(),
                );
            }
            transport::ParsedLine::Unfit { id } => {
                self.answer_rejected(
                    &id,
                    ErrorCode::InvalidArguments,
                    transport::UNFIT.to_owned(),
                );
            }
            transport::ParsedLine::Malformed { id } => {
                self.answer_rejected(&id, ErrorCode::Malformed, transport::MALFORMED.to_owned());
            }
            transport::ParsedLine::Ignored => {}
        }
    }

    /// Answers one `call`: registrations through the registry before
    /// `ready` and refused after it, `host.drive` through the `Drive`
    /// seam, and host and state calls through the injected services. A
    /// call naming a request that is no longer pending is rejected
    /// `stale_request` and runs nothing. A failed package stays failed:
    /// later calls fail with the first failure, without touching the
    /// registry again.
    fn handle_call(
        &self,
        id: contract::CommandId,
        call: String,
        args: Vec<Value>,
        request_id: Option<contract::RequestId>,
    ) {
        if let Some((code, message)) = support::lock(&self.shared.registry).failure() {
            self.answer_rejected(&id, code, message);
            return;
        }
        let context = match request_id {
            Some(request) => match self.shared.table.context(&request) {
                Some(context) => context,
                None => {
                    self.answer_stale(&id);
                    return;
                }
            },
            None => self.call_context(),
        };
        match calls::route_call(&call, args) {
            Ok(calls::Route::Register { call, args }) => {
                if !context.registration_allowed() {
                    // Refused without touching the registry: no
                    // registration stands, and no notice grows.
                    self.answer_rejected(
                        &id,
                        ErrorCode::ExtensionFailed,
                        in_callback_message(&call),
                    );
                    return;
                }
                let outcome =
                    support::lock(&self.shared.registry).register(id.clone(), &call, &args, false);
                match outcome {
                    RegisterOutcome::Accepted => {
                        self.install_handlers(&id, &call, &args);
                        self.answer_accepted(&id, None);
                    }
                    RegisterOutcome::Rejected { code, message } => {
                        self.answer_rejected(&id, code, message);
                    }
                    RegisterOutcome::FailedPackage { code, message } => {
                        self.answer_rejected(&id, code, message);
                    }
                }
            }
            Ok(calls::Route::Drive { command, args }) => {
                self.drive_command(id, command, args);
            }
            Ok(calls::Route::Host { call: name, .. }) => {
                // Host and state calls dispatch through the injected
                // services, which land in a later part; until then no call
                // stands, and the registry stays untouched.
                self.answer_rejected(
                    &id,
                    ErrorCode::InvalidArguments,
                    format!("unknown call `{name}`"),
                );
            }
            Err((code, message)) => {
                self.answer_rejected(&id, code, message);
            }
        }
    }

    /// The context one `call` with no `request_id` runs under: the entry
    /// script before `ready`, a timer after it, with no timeout of its
    /// own.
    fn call_context(&self) -> crate::host::admission::CallbackContext {
        if self.is_ready() {
            crate::host::admission::CallbackContext::timer(&self.shared.owner)
        } else {
            crate::host::admission::CallbackContext::entry(&self.shared.owner)
        }
    }

    /// Installs one handler per function an accepted registration
    /// registers: every `true` the wire carries becomes a request Fiber
    /// sends. Skips a key the registry does not show, so a dispatch on it
    /// fails closed instead of running a function nothing registered.
    fn install_handlers(&self, command: &contract::CommandId, call: &str, args: &[Value]) {
        for key in calls::handler_keys(call, args) {
            let Some((kind, ran_by, timeout)) = self.handler_spec(command, call, &key, args) else {
                continue;
            };
            let invoke = self.invoke_for(command.clone(), key.clone(), kind, ran_by, timeout);
            support::lock(&self.shared.registry).set_handler(command, &key, invoke);
        }
    }

    /// What one installed key dispatches as: its kind, what the callback
    /// runs as, and the timeout it counts against. `None` when the
    /// registry does not show the key, so the handler never installs.
    fn handler_spec(
        &self,
        command: &contract::CommandId,
        call: &str,
        key: &str,
        args: &[Value],
    ) -> Option<(requests::RequestKind, String, Duration)> {
        let registry = support::lock(&self.shared.registry);
        let name = args.first().and_then(Value::as_str).unwrap_or_default();
        match call {
            "fiber.tool" => {
                let tool = registry.tools().get(name)?;
                let timeout = tool.declared.timeout;
                if key == "run" {
                    Some((requests::RequestKind::Tool, format!("tool {name}"), timeout))
                } else {
                    Some((
                        requests::RequestKind::Function,
                        format!("tool {name} effects"),
                        timeout,
                    ))
                }
            }
            "fiber.hook" => {
                let hooks = registry.hooks().by_point.get(name)?;
                let timeout = hooks.last()?.timeout;
                Some((
                    requests::RequestKind::Stream,
                    format!("hook {name}"),
                    timeout,
                ))
            }
            "fiber.watch" => {
                let watchers = &registry.watchers().watchers;
                let timeout = watchers.last()?.timeout;
                let index = registry.watcher_runs().len().saturating_sub(1);
                let _ = command;
                Some((
                    requests::RequestKind::Stream,
                    format!("watcher {index}"),
                    timeout,
                ))
            }
            "fiber.command" => {
                let timeout = registry.commands().get(name)?.timeout;
                Some((
                    requests::RequestKind::Stream,
                    format!("command {name}"),
                    timeout,
                ))
            }
            "fiber.search_backend" => {
                let (timeout, _) = registry.search().get(name)?;
                Some((
                    requests::RequestKind::Function,
                    format!("search {name}"),
                    *timeout,
                ))
            }
            "fiber.provider" => {
                let provider = registry.providers().get(name)?;
                let timeout = provider.timeouts.get(key)?.to_owned();
                Some((
                    requests::RequestKind::Function,
                    format!("provider {name}.{key}"),
                    timeout,
                ))
            }
            "fiber.harness" => {
                let harness = registry.harness().get(name)?;
                Some((
                    requests::RequestKind::Function,
                    format!("harness {name}.{key}"),
                    harness.timeout,
                ))
            }
            _ => None,
        }
    }

    /// The invoker the registry resolves one installed key through: sends
    /// the request and waits for its answer, as `run_request` does.
    fn invoke_for(
        &self,
        registration: contract::CommandId,
        key: String,
        kind: requests::RequestKind,
        ran_by: String,
        timeout: Duration,
    ) -> crate::runtime::ProcessInvoke {
        let shared = Arc::clone(&self.shared);
        Arc::new(move |args: Value, caller: &dyn contract::tool::Cancel| {
            run_request(
                &shared,
                Dispatch {
                    kind,
                    registration: registration.clone(),
                    key: key.clone(),
                    ran_by: ran_by.clone(),
                    args: wire_args(args),
                    timeout,
                    state_call: None,
                },
                caller,
            )
        })
    }

    /// The `extension_hello` line: the session's identity, why it
    /// started, its workspace, the data directories and the folded state.
    fn hello_line(&self) -> Result<Vec<u8>, serde_json::Error> {
        let payload = serde_json::to_value(self.shared.context.hello())?;
        Ok(envelope_line(
            &self.shared,
            "extension_hello",
            payload.as_object().cloned().unwrap_or_default(),
        ))
    }

    /// Answers one command accepted, carrying the call's return: `value`
    /// absent when the call returns nothing. A result that does not fit
    /// under the framing cap is answered `too_large` in its place.
    fn answer_accepted(
        &self,
        id: &contract::CommandId,
        result: Option<contract::events::CommandResult>,
    ) {
        let payload = serde_json::to_value(&contract::events::CommandAccepted {
            command_id: id.clone(),
            result,
        })
        .ok()
        .and_then(|payload| payload.as_object().cloned())
        .unwrap_or_default();
        let line = envelope_line(&self.shared, "command_accepted", payload);
        if line.len() > self.shared.transport.max_line() {
            self.answer_rejected(
                id,
                ErrorCode::TooLarge,
                "the result does not fit the 64 MiB wire cap".to_owned(),
            );
            return;
        }
        // The child may be gone: its answer dies with it, and the
        // table already settled what the answer would settle.
        match self.shared.transport.send(line) {
            Ok(()) | Err(_) => {}
        }
    }

    /// Answers one command rejected with its code and message.
    fn answer_rejected(&self, id: &contract::CommandId, code: ErrorCode, message: String) {
        let payload = serde_json::to_value(&contract::events::CommandRejected {
            command_id: Some(id.clone()),
            code,
            message,
        })
        .ok()
        .and_then(|payload| payload.as_object().cloned())
        .unwrap_or_default();
        let line = envelope_line(&self.shared, "command_rejected", payload);
        // The child may be gone: its answer dies with it, and the
        // table already settled what the answer would settle.
        match self.shared.transport.send(line) {
            Ok(()) | Err(_) => {}
        }
    }

    /// Rejects a call or reply naming a request that is no longer
    /// pending: it ran nothing and changes nothing.
    fn answer_stale(&self, id: &contract::CommandId) {
        self.answer_rejected(
            id,
            ErrorCode::StaleRequest,
            "the request is no longer pending".to_owned(),
        );
    }

    /// Sends one ordinary driver command through the `Drive` seam: the
    /// door runs it elsewhere and answers on the ack, which writes the
    /// command's answer. With no driver the session is closing, and the
    /// call raises `closing`.
    fn drive_command(
        &self,
        id: contract::CommandId,
        command: String,
        args: serde_json::Map<String, Value>,
    ) {
        let Some(driver) = self.shared.drive.clone() else {
            self.answer_rejected(
                &id,
                ErrorCode::Closing,
                "host.drive: the session is closing".to_owned(),
            );
            return;
        };
        let shared = Arc::clone(&self.shared);
        let owner = shared.owner.clone();
        let answer_id = id.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("process drive {owner}"))
            .spawn(move || {
                let ack = contract::inbox::Ack(Box::new(move |answer| match answer {
                    Ok(result) => answer_on(&shared, &answer_id, result),
                    Err(rejection) => answer_off(&shared, &answer_id, rejection),
                }));
                driver.drive(&owner, &command, args, ack);
            });
        if spawned.is_err() {
            self.answer_rejected(
                &id,
                ErrorCode::IoFailed,
                "host.drive: Fiber could not start the driver call".to_owned(),
            );
        }
    }

    /// Which registered family `func` names, for dispatching it as a
    /// stream item, a tool or a nested function.
    fn family_of(&self, func: &FunctionRef) -> Option<Family> {
        let registry = support::lock(&self.shared.registry);
        for ((point, _), reference) in registry.hook_runs() {
            if reference == func {
                return Some(Family::Hook {
                    point: point.clone(),
                });
            }
        }
        for (index, reference) in registry.watcher_runs() {
            if reference == func {
                return Some(Family::Watcher { index: *index });
            }
        }
        for (name, entry) in registry.commands() {
            if entry.run == *func {
                return Some(Family::Command { name: name.clone() });
            }
        }
        for (name, entry) in registry.tools() {
            if entry.run == *func {
                return Some(Family::Tool { name: name.clone() });
            }
            if entry.effects.as_ref() == Some(func) {
                return Some(Family::Effects { name: name.clone() });
            }
        }
        for (name, (timeout, reference)) in registry.search() {
            if reference == func {
                let _ = timeout;
                return Some(Family::Search { name: name.clone() });
            }
        }
        for (name, provider) in registry.providers() {
            for (key, reference) in &provider.runs {
                if reference == func {
                    return Some(Family::Provider {
                        name: name.clone(),
                        func: key.clone(),
                    });
                }
            }
        }
        for (name, harness) in registry.harness() {
            for (key, reference) in &harness.runs {
                if reference == func {
                    return Some(Family::Harness {
                        name: name.clone(),
                        func: key.clone(),
                    });
                }
            }
        }
        None
    }
}

/// Sends one request and waits for its answer: the stream gate first
/// for stream items, then the line, then the reply, the deadline, the
/// cancel or the hang guard, whichever is first. The request settles
/// exactly once on every path, releasing the stream gate. A free function
/// over the shared owner, so installed handlers own it beside the
/// dispatches.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn run_request(
    shared: &Shared,
    dispatch: Dispatch,
    caller: &dyn contract::tool::Cancel,
) -> Result<Option<Value>, Error> {
    // The callback's clock starts when Fiber asks, so time spent waiting
    // behind earlier stream items counts against it.
    let clock = shared.clock.as_ref();
    let deadline = clock.now().checked_add(dispatch.timeout);
    let minted = shared.table.mint(
        dispatch.kind,
        &shared.owner,
        dispatch.ran_by,
        deadline,
        dispatch.state_call,
    );
    let id = minted.id;
    if dispatch.kind == requests::RequestKind::Stream {
        let generation = shared.table.generation();
        if !shared.table.acquire_stream(&id, generation) {
            shared.table.cancel(&id);
            return Err(Error::Stopped {
                extension: shared.owner.clone(),
            });
        }
    }
    shared.table.watch_caller(caller, &id);
    let payload = serde_json::to_value(&contract::events::ExtensionRequest {
        request_id: id.clone(),
        registration: dispatch.registration,
        key: dispatch.key.clone(),
        args: dispatch.args,
    })
    .map_err(|error| Error::BadReturn {
        extension: shared.owner.clone(),
        callback: dispatch.key.clone(),
        why: format!("the request never serialized: {error}"),
    })?;
    let line = envelope_line(
        shared,
        "extension_request",
        payload.as_object().cloned().unwrap_or_default(),
    );
    match shared.transport.send(line) {
        Ok(()) => {}
        Err(transport::SendError::TooLarge) => {
            shared.table.cancel(&id);
            return Err(Error::BadReturn {
                extension: shared.owner.clone(),
                callback: dispatch.key.clone(),
                why: "the request does not fit the 64 MiB wire cap".to_owned(),
            });
        }
        Err(transport::SendError::Gone) => {
            shared.table.cancel(&id);
            return Err(Error::Stopped {
                extension: shared.owner.clone(),
            });
        }
    }
    let hang = hang_guard(clock, deadline);
    let outcome =
        match requests::Table::wait_for(&minted.wait, deadline, &minted.cancel, clock, hang) {
            Some(requests::Outcome::Answered(answer)) => requests::Outcome::Answered(answer),
            Some(requests::Outcome::TimedOut) => {
                shared
                    .table
                    .end_race(&id, &minted.wait, requests::Outcome::TimedOut)
            }
            Some(requests::Outcome::Cancelled) | None => {
                shared
                    .table
                    .end_race(&id, &minted.wait, requests::Outcome::Cancelled)
            }
        };
    match outcome {
        requests::Outcome::Answered(requests::Answer::Value(value)) => Ok(Some(value)),
        requests::Outcome::Answered(requests::Answer::Failure { code, message }) => {
            Err(Error::Lua {
                extension: shared.owner.clone(),
                message: format!(
                    "`{key}` failed with {code:?}: {message}",
                    key = dispatch.key
                ),
            })
        }
        requests::Outcome::TimedOut => Err(Error::Timeout {
            extension: shared.owner.clone(),
            callback: dispatch.key,
            timeout_ms: dispatch.timeout.as_millis().try_into().unwrap_or(u64::MAX),
        }),
        requests::Outcome::Cancelled => Err(Error::Stopped {
            extension: shared.owner.clone(),
        }),
    }
}

/// What one dispatch sends: the request's kind, whom it answers, what it
/// runs as, its arguments, its timeout and its hook transaction. One
/// value carries the dispatch so the send path stays narrow.
#[derive(Debug, Clone)]
pub(crate) struct Dispatch {
    /// Ordered behind the stream gate, or concurrent with everything.
    pub kind: requests::RequestKind,
    /// The registering call: the request answers under its generation.
    pub registration: contract::CommandId,
    /// The function key within the registration: `run`, `effects` or a
    /// numbered function.
    pub key: String,
    /// What the callback runs as, naming its attribution.
    pub ran_by: String,
    /// The arguments the Lua function receives, as a JSON array.
    pub args: Vec<Value>,
    /// How long the callback may run before it fails.
    pub timeout: Duration,
    /// The hook state transaction the call's writes join, when the
    /// request is a hook delivery.
    pub state_call: Option<u64>,
}

impl Dispatch {
    /// One ordered stream item: a hook, a watcher delivery or a command.
    pub(crate) fn stream(
        func: &FunctionRef,
        ran_by: String,
        args: Value,
        timeout: Duration,
        state_call: Option<u64>,
    ) -> Self {
        Self {
            kind: requests::RequestKind::Stream,
            registration: func.command.clone(),
            key: func.key.clone(),
            ran_by,
            args: wire_args(args),
            timeout,
            state_call,
        }
    }

    /// One tool call: concurrent with everything.
    pub(crate) fn tool(func: &FunctionRef, ran_by: String, args: Value, timeout: Duration) -> Self {
        Self {
            kind: requests::RequestKind::Tool,
            registration: func.command.clone(),
            key: func.key.clone(),
            ran_by,
            args: wire_args(args),
            timeout,
            state_call: None,
        }
    }
}

/// Which registered family a reference names.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
enum Family {
    Hook { point: String },
    Watcher { index: usize },
    Command { name: String },
    Tool { name: String },
    Effects { name: String },
    Search { name: String },
    Provider { name: String, func: String },
    Harness { name: String, func: String },
}

/// The arguments the Lua function receives, as a JSON array: a function
/// taking one value receives it alone, and an array value spreads, so a
/// multi-argument function receives each element.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn wire_args(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        other @ (Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Object(_)) => vec![other],
    }
}

/// The hang guard one request waits under: the callback's deadline with
/// slack for the settlement to land, or a backstop without one. This is no
/// timer call's timeout, only the bound that fails a wedged wait instead
/// of hanging it: the reply, the deadline or the cancel always ends the
/// wait first.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn hang_guard(
    clock: &dyn contract::clock::Clock,
    deadline: Option<std::time::Instant>,
) -> Duration {
    deadline
        .map(|end| end.saturating_duration_since(clock.now()) + Duration::from_secs(300))
        .unwrap_or_else(|| Duration::from_secs(3600))
}

/// The sentence a command this Fiber does not build is rejected with,
/// shared with the doors' `not_built`: it names the line's own command.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn not_built(command: &str) -> String {
    format!("`{command}` is not built in this Fiber yet.")
}

/// One wire line: the envelope around `payload`, with the session's id,
/// the clock's wall time and the build's schema version.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn envelope_line(shared: &Shared, kind: &str, payload: serde_json::Map<String, Value>) -> Vec<u8> {
    let envelope = contract::Envelope {
        kind: kind.to_owned(),
        session_id: shared.context.session_id.clone(),
        ts: contract::clock::wall_ms(shared.clock.wall()),
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    };
    serde_json::to_vec(&envelope).unwrap_or_default()
}

/// Answers a driven command accepted, on the pump's thread.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn answer_on(
    shared: &Shared,
    id: &contract::CommandId,
    result: Option<contract::events::CommandResult>,
) {
    let payload = serde_json::to_value(&contract::events::CommandAccepted {
        command_id: id.clone(),
        result,
    })
    .ok()
    .and_then(|payload| payload.as_object().cloned())
    .unwrap_or_default();
    let line = envelope_line(shared, "command_accepted", payload);
    if line.len() > shared.transport.max_line() {
        answer_off(
            shared,
            id,
            contract::inbox::Rejection {
                code: ErrorCode::TooLarge,
                message: "the result does not fit the 64 MiB wire cap".to_owned(),
            },
        );
        return;
    }
    // The child may be gone: its answer dies with it, and the
    // table already settled what the answer would settle.
    match shared.transport.send(line) {
        Ok(()) | Err(_) => {}
    }
}

/// Answers a driven command rejected, on the pump's thread.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn answer_off(shared: &Shared, id: &contract::CommandId, rejection: contract::inbox::Rejection) {
    let payload = serde_json::to_value(&contract::events::CommandRejected {
        command_id: Some(id.clone()),
        code: rejection.code,
        message: rejection.message,
    })
    .ok()
    .and_then(|payload| payload.as_object().cloned())
    .unwrap_or_default();
    // The child may be gone: its answer dies with it.
    match shared
        .transport
        .send(envelope_line(shared, "command_rejected", payload))
    {
        Ok(()) | Err(_) => {}
    }
}
