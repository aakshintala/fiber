//! Native process registrations (`docs/extensions.md`, "Process extensions"
//! and "Registering"): validation with the Lua registry's exact wording,
//! and references for every function a call registers.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use contract::CommandId;
use contract::ErrorCode;
use serde_json::Value;

use crate::Error;
use crate::lua::{DeclaredHooks, DeclaredTool, DeclaredWatchers};
use crate::runtime::{FunctionRef, FunctionTarget, ProcessInvoke, Runtime};

use super::validate;

/// What one registration call answers on the wire: accepted with its
/// result, rejected with the refusal's sentence, or failing the package.
#[allow(
    dead_code,
    reason = "answered by the transport once processes register; asserted by tests"
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
    reason = "timeouts are enforced when the command runs; asserted by tests"
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
#[allow(dead_code, reason = "read by provider dispatch; asserted by tests")]
#[derive(Debug, Clone, Default)]
pub(crate) struct ProviderEntry {
    /// Each function's timeout, by function name.
    pub(crate) timeouts: BTreeMap<String, Duration>,
    /// Each function's `run` behind the runtime, by function name.
    pub(crate) runs: BTreeMap<String, FunctionRef>,
}

/// One harness a `fiber.harness` call registered.
#[allow(dead_code, reason = "read by harness consumption; asserted by tests")]
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
    reason = "filled by the transport once processes register; views asserted by tests"
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
    reason = "the register pathway and its views: the transport answers through it; asserted by tests"
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

    /// Resets the collected registrations for the restart's fresh child:
    /// every declared tool, hook, watcher, command, provider, search and
    /// harness registration leaves, with their notices and run
    /// references, so the new child registers the same set again under
    /// the bumped generation. Manifest declarations and a latched
    /// package failure stay: a failed package stays failed.
    pub(crate) fn restart(&mut self) {
        self.tools.clear();
        self.hooks.by_point.clear();
        self.hooks.problems.clear();
        self.hook_runs.clear();
        self.watchers.watchers.clear();
        self.watch_problems.clear();
        self.watcher_runs.clear();
        self.commands.clear();
        self.providers.clear();
        self.search.clear();
        self.harness.clear();
        self.problems.clear();
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
pub(super) fn in_callback_message(call: &str) -> String {
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
