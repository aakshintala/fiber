//! Native process registrations (`docs/extensions.md`, "Process extensions"
//! and "Registering"): validating the wire's JSON registration calls with
//! the Lua registry's exact wording, without running Lua.
//!
//! Each `fiber.*` call validates exactly as its Lua twin does: a refusal
//! carries the problem's sentence back in `command_rejected` and records it
//! for the notice, and the package goes on. Only a provider or replacement
//! the manifest does not declare, or a registration outside a commands-keyed
//! package's declared commands, fails the whole package with
//! `extension_failed`. Every function a call registers is a
//! [`FunctionRef`][crate::runtime::FunctionRef]: the registering command's
//! id, the function's key and the owner's generation.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use contract::CommandId;
use contract::ErrorCode;
use serde_json::{Map, Value};

use crate::Error;
use crate::lua::{
    DeclaredHook, DeclaredHooks, DeclaredTool, DeclaredWatcher, DeclaredWatchers, HookPhase,
    StaticEffects, effects_from, is_known_kind,
};
use crate::runtime::{FunctionRef, FunctionTarget, ProcessInvoke, Runtime};
use contract::tool::Bound;

/// What one registration call answers on the wire: accepted with its
/// result, rejected with the refusal's sentence, or failing the package.
#[allow(
    dead_code,
    reason = "answered by the transport once processes register in a later part; asserted by tests"
)]
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RegisterOutcome {
    /// The registration stands; the `call` answer carries `value`.
    Accepted {
        /// The accepted registration's result: empty, as for Lua.
        value: Value,
    },
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
        runtime.register_process(&owner);
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

/// The `args` value at `index`, when the call passed one.
fn arg(args: &[Value], index: usize) -> Option<&Value> {
    args.get(index)
}

/// How Lua shows a registration name that is not a string: the value, or
/// `nil` for nothing and `table` for what has no short rendering. JSON
/// `null` reads as Lua's `nil`, which is what the wire decodes it to.
fn shown(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "nil".to_owned(),
        Some(Value::String(name)) => name.clone(),
        Some(Value::Bool(held)) => held.to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(_) => "table".to_owned(),
    }
}

/// A whole number of milliseconds above 0, as every callback declares.
fn millis(value: Option<&Value>) -> Option<u64> {
    value?.as_u64().filter(|millis| *millis > 0)
}

/// Whether `name` is 1 to 128 of `A-Z`, `a-z`, `0-9`, `_` and `-`: what a
/// tool may be called.
fn is_tool_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whether `name` is plain: non-empty with no whitespace, `/` or `:`.
fn is_plain(name: &str) -> bool {
    !name.is_empty()
        && !name.chars().any(char::is_whitespace)
        && !name.contains('/')
        && !name.contains(':')
}

/// Whether `point` is a hook point.
fn is_hook_point(point: &str) -> bool {
    matches!(
        point,
        "session_start"
            | "before_message"
            | "turn_start"
            | "before_tool"
            | "before_model_call"
            | "after_tool"
            | "turn_end"
            | "before_handoff"
    )
}

/// Records `message` for the notice and rejects the call with it.
fn refuse(problems: &mut Vec<String>, message: String) -> RegisterOutcome {
    problems.push(message.clone());
    RegisterOutcome::Rejected {
        code: ErrorCode::ExtensionFailed,
        message,
    }
}

/// A `fiber.tool` registration: exactly the checks `fiber.tool` runs, in
/// its order, over JSON.
impl Registry {
    /// Registers `fiber.tool` from `command` with `args`.
    fn register_tool(&mut self, command: CommandId, args: &[Value]) -> RegisterOutcome {
        if let Some(fatal) = self.reject_lazy("fiber.tool") {
            return fatal;
        }
        let name = arg(args, 0);
        let spec = arg(args, 1);
        let fail = |problems: &mut Vec<String>, why: &str| {
            refuse(
                problems,
                format!("`{}` tool not registered: {why}", shown(name)),
            )
        };
        let Some(Value::String(name)) = name else {
            return fail(&mut self.problems, "the name must be a string");
        };
        if !is_tool_name(name) {
            return fail(
                &mut self.problems,
                "a name is 1 to 128 of `A-Z`, `a-z`, `0-9`, `_` and `-`",
            );
        }
        let Some(Value::Object(spec)) = spec else {
            return fail(
                &mut self.problems,
                "it takes a table of `description`, `input_schema`, `effects`, `timeout`, `run` and optionally `keep`",
            );
        };
        let description = match spec.get("description") {
            Some(Value::String(description)) if !description.is_empty() => description.clone(),
            _ => {
                return fail(
                    &mut self.problems,
                    "`description` must be a non-empty string",
                );
            }
        };
        let input_schema = match spec.get("input_schema") {
            Some(Value::Object(schema))
                if schema.get("type") == Some(&Value::String("object".to_owned())) =>
            {
                Value::Object(schema.clone())
            }
            _ => {
                return fail(
                    &mut self.problems,
                    "`input_schema` must be a JSON object whose `type` is `object`",
                );
            }
        };
        let (effects, effects_fn) = match spec.get("effects") {
            None | Some(Value::Null) => return fail(&mut self.problems, "missing `effects`"),
            Some(Value::Bool(true)) => (None, true),
            Some(Value::Object(effects)) => match effects_from(&Value::Object(effects.clone())) {
                Ok((declared, hosts)) => (Some(StaticEffects { declared, hosts }), false),
                Err(why) => return fail(&mut self.problems, &format!("`effects`: {why}")),
            },
            _ => {
                return fail(
                    &mut self.problems,
                    "`effects` must be a table or a function",
                );
            }
        };
        let timeout = match spec.get("timeout") {
            None | Some(Value::Null) => return fail(&mut self.problems, "missing `timeout`"),
            Some(value) => match millis(Some(value)) {
                Some(millis) => Duration::from_millis(millis),
                None => {
                    return fail(
                        &mut self.problems,
                        "`timeout` must be a whole number of milliseconds above 0",
                    );
                }
            },
        };
        if spec.get("run") != Some(&Value::Bool(true)) {
            return fail(&mut self.problems, "`run` must be a function");
        }
        let keep = match spec.get("keep") {
            None | Some(Value::Null) => Bound::DEFAULT,
            Some(Value::Object(keep)) => match keep_of(keep) {
                Ok(keep) => keep,
                Err(why) => return fail(&mut self.problems, &why),
            },
            _ => {
                return fail(
                    &mut self.problems,
                    "`keep` must be a table of `head` and `tail`",
                );
            }
        };
        if self.tools.contains_key(name) {
            let message = format!("`{name}` tool is already registered");
            self.problems.push(message.clone());
            return RegisterOutcome::Rejected {
                code: ErrorCode::ExtensionFailed,
                message,
            };
        }
        let run = self.mint(command.clone(), "run");
        let effects_reference = effects_fn.then(|| self.mint(command, "effects"));
        self.tools.insert(
            name.clone(),
            ToolEntry {
                declared: DeclaredTool {
                    name: name.clone(),
                    description,
                    input_schema,
                    effects,
                    timeout,
                    keep,
                },
                run,
                effects: effects_reference,
            },
        );
        RegisterOutcome::Accepted {
            value: Value::Object(Map::new()),
        }
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

/// A tool's `keep`: the byte counts of the start and of the end of a long
/// result, exactly as `fiber.tool` reads them.
fn keep_of(keep: &Map<String, Value>) -> Result<Bound, String> {
    for key in keep.keys() {
        if key != "head" && key != "tail" {
            return Err(format!("`keep`: `{key}` is not `head` or `tail`"));
        }
    }
    let pair = |key: &str| -> Result<usize, String> {
        match keep.get(key) {
            None | Some(Value::Null) => Ok(0),
            Some(Value::Number(number)) => number
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| format!("`keep`: `{key}` must be a whole number of bytes")),
            _ => Err(format!("`keep`: `{key}` must be a whole number of bytes")),
        }
    };
    let head = pair("head")?;
    let tail = pair("tail")?;
    if head == 0 && tail == 0 {
        return Err("`keep`: `head` and `tail` cannot both be 0".to_owned());
    }
    head.checked_add(tail)
        .map(|_| Bound {
            start: head,
            end: tail,
        })
        .ok_or_else(|| "`keep`: `head` plus `tail` is too large".to_owned())
}

/// A `fiber.hook` registration: exactly the checks `fiber.hook` runs, in
/// its order, over JSON.
impl Registry {
    /// Registers `fiber.hook` from `command` with `args`.
    fn register_hook(&mut self, command: CommandId, args: &[Value]) -> RegisterOutcome {
        if let Some(fatal) = self.reject_lazy("fiber.hook") {
            return fatal;
        }
        let point = arg(args, 0);
        let spec = arg(args, 1);
        let entry = match spec {
            Some(Value::Object(spec)) => match spec.get("entry") {
                Some(Value::String(entry)) => Some(entry.clone()),
                _ => None,
            },
            _ => None,
        };
        let fail = |problems: &mut Vec<String>, shown: &str, why: &str| {
            refuse(
                problems,
                match &entry {
                    Some(entry) => {
                        format!("`{shown}` hook not registered for entry `{entry}`: {why}")
                    }
                    None => format!("`{shown}` hook not registered: {why}"),
                },
            )
        };
        let point_text = shown(point);
        let Some(Value::String(point)) = point else {
            return fail(
                &mut self.problems,
                &point_text,
                &format!("`{point_text}` is not a hook point"),
            );
        };
        if !is_hook_point(point) {
            return fail(
                &mut self.problems,
                point,
                &format!("`{point}` is not a hook point"),
            );
        }
        let Some(Value::Object(spec)) = spec else {
            return fail(
                &mut self.problems,
                point,
                "it takes a table of `timeout`, `on_failure` and `run`",
            );
        };
        let timeout = match spec.get("timeout") {
            None | Some(Value::Null) => {
                return fail(&mut self.problems, point, "missing `timeout`");
            }
            Some(value) => match millis(Some(value)) {
                Some(millis) => Duration::from_millis(millis),
                None => {
                    return fail(
                        &mut self.problems,
                        point,
                        "`timeout` must be a whole number of milliseconds above 0",
                    );
                }
            },
        };
        let blocking = match spec.get("on_failure") {
            None | Some(Value::Null) => {
                return fail(&mut self.problems, point, "missing `on_failure`");
            }
            Some(Value::String(failure)) if failure == "blocking" => true,
            Some(Value::String(failure)) if failure == "non-blocking" => false,
            _ => {
                return fail(
                    &mut self.problems,
                    point,
                    "`on_failure` must be `blocking` or `non-blocking`",
                );
            }
        };
        let phase = match spec.get("phase") {
            None | Some(Value::Null) => HookPhase::Transform,
            Some(Value::String(phase)) if phase == "sanitize" => HookPhase::Sanitize,
            Some(Value::String(phase)) if phase == "transform" => HookPhase::Transform,
            Some(Value::String(phase)) if phase == "check" => HookPhase::Check,
            _ => {
                return fail(
                    &mut self.problems,
                    point,
                    "`phase` must be `sanitize`, `transform` or `check`",
                );
            }
        };
        if phase == HookPhase::Check
            && !matches!(
                point.as_str(),
                "before_message" | "before_tool" | "before_model_call"
            )
        {
            return fail(
                &mut self.problems,
                point,
                "a `check` hook exists only at `before_message`, `before_tool` and `before_model_call`",
            );
        }
        if spec.get("run") != Some(&Value::Bool(true)) {
            return fail(&mut self.problems, point, "`run` must be a function");
        }
        let index = self.hooks.by_point.get(point).map(Vec::len).unwrap_or(0);
        let run = self.mint(command, "run");
        self.hook_runs.insert((point.clone(), index), run);
        self.hooks
            .by_point
            .entry(point.clone())
            .or_default()
            .push(DeclaredHook {
                phase,
                timeout,
                blocking,
            });
        RegisterOutcome::Accepted {
            value: Value::Object(Map::new()),
        }
    }

    /// Registers `fiber.watch` from `command` with `args`: exactly the
    /// checks `fiber.watch` runs, with kinds checked against the event
    /// kinds, as `DeclaredWatchers` does.
    fn register_watch(&mut self, command: CommandId, args: &[Value]) -> RegisterOutcome {
        if let Some(fatal) = self.reject_lazy("fiber.watch") {
            return fatal;
        }
        // The index counts registered watchers, as Lua's `#watchers` does:
        // a refused call registers nothing and takes no number.
        let index = self.watchers.watchers.len();
        let fail = |problems: &mut Vec<String>, why: &str| {
            refuse(
                problems,
                format!("`watcher:{index}` watcher not registered: {why}"),
            )
        };
        let kinds = match arg(args, 0) {
            Some(Value::Array(kinds))
                if !kinds.is_empty()
                    && kinds
                        .iter()
                        .all(|kind| matches!(kind, Value::String(kind) if !kind.is_empty())) =>
            {
                kinds
                    .iter()
                    .filter_map(|kind| kind.as_str())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            }
            _ => {
                return fail(
                    &mut self.watch_problems,
                    "`kinds` must be a list of event kinds",
                );
            }
        };
        let spec = match arg(args, 1) {
            Some(Value::Object(spec)) => spec,
            _ => {
                return fail(
                    &mut self.watch_problems,
                    "it takes a table of `timeout` and `run`",
                );
            }
        };
        let timeout = match spec.get("timeout") {
            None | Some(Value::Null) => {
                return fail(&mut self.watch_problems, "missing `timeout`");
            }
            Some(value) => match millis(Some(value)) {
                Some(millis) => Duration::from_millis(millis),
                None => {
                    return fail(
                        &mut self.watch_problems,
                        "`timeout` must be a whole number of milliseconds above 0",
                    );
                }
            },
        };
        if spec.get("run") != Some(&Value::Bool(true)) {
            return fail(&mut self.watch_problems, "`run` must be a function");
        }
        let mut wanted = Vec::new();
        for kind in &kinds {
            if is_known_kind(kind) {
                wanted.push(kind.clone());
            } else {
                self.watch_problems.push(format!(
                    "`watcher:{index}` watcher not registered: `{kind}` is not an event kind"
                ));
            }
        }
        if wanted.is_empty() {
            let message = format!(
                "`watcher:{index}` watcher not registered: `{}` is not an event kind",
                kinds.first().cloned().unwrap_or_default()
            );
            return RegisterOutcome::Rejected {
                code: ErrorCode::ExtensionFailed,
                message,
            };
        }
        let run = self.mint(command, "run");
        self.watcher_runs.insert(index, run);
        self.watchers.watchers.push(DeclaredWatcher {
            kinds: wanted,
            timeout,
        });
        RegisterOutcome::Accepted {
            value: Value::Object(Map::new()),
        }
    }

    /// Registers `fiber.command` from `command` with `args`: exactly the
    /// checks `fiber.command` runs, in its order, over JSON. A later call
    /// for the same name replaces the first, as in Lua.
    fn register_command(&mut self, command: CommandId, args: &[Value]) -> RegisterOutcome {
        let name = arg(args, 0);
        let spec = arg(args, 1);
        let fail = |problems: &mut Vec<String>, why: &str| {
            refuse(
                problems,
                format!("`{}` command not registered: {why}", shown(name)),
            )
        };
        let Some(Value::String(name)) = name else {
            return fail(&mut self.problems, "the name must be a string");
        };
        if name.is_empty()
            || name.chars().any(char::is_whitespace)
            || name.contains('/')
            || name.contains(':')
        {
            return fail(
                &mut self.problems,
                &format!("names are plain: `{name}` is empty or holds whitespace, `/` or `:`"),
            );
        }
        let Some(Value::Object(spec)) = spec else {
            return fail(
                &mut self.problems,
                "it takes a table of `timeout`, `run` and `description`",
            );
        };
        let timeout = match spec.get("timeout") {
            None | Some(Value::Null) => return fail(&mut self.problems, "missing `timeout`"),
            Some(value) => match millis(Some(value)) {
                Some(millis) => Duration::from_millis(millis),
                None => {
                    return fail(
                        &mut self.problems,
                        "`timeout` must be a whole number of milliseconds above 0",
                    );
                }
            },
        };
        if spec.get("run") != Some(&Value::Bool(true)) {
            return fail(&mut self.problems, "`run` must be a function");
        }
        let description = match spec.get("description") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(description)) if !description.contains('\n') => description.clone(),
            _ => {
                return fail(
                    &mut self.problems,
                    "`description` must be a single-line string",
                );
            }
        };
        if let Some(declared) = &self.manifest_commands
            && !declared.contains_key(name)
        {
            let message = format!(
                "Command `{name}` is not declared under `commands`; `{}` registers only the commands its manifest declares",
                self.owner
            );
            self.problems.push(message.clone());
            return RegisterOutcome::FailedPackage {
                code: ErrorCode::ExtensionFailed,
                message,
            };
        }
        let run = self.mint(command, "run");
        self.commands.insert(
            name.clone(),
            CommandEntry {
                description,
                timeout,
                run,
            },
        );
        RegisterOutcome::Accepted {
            value: Value::Object(Map::new()),
        }
    }

    /// Registers `fiber.search_backend` from `command` with `args`: exactly
    /// the checks `fiber.search_backend` runs, in its order, over JSON.
    /// Unlike commands, a backend name need not be plain.
    fn register_search(&mut self, command: CommandId, args: &[Value]) -> RegisterOutcome {
        let name = arg(args, 0);
        let spec = arg(args, 1);
        let fail = |problems: &mut Vec<String>, why: &str| {
            refuse(
                problems,
                format!("`{}` search backend not registered: {why}", shown(name)),
            )
        };
        if let Some(fatal) = self.reject_lazy("fiber.search_backend") {
            return fatal;
        }
        let Some(Value::String(name)) = name else {
            return fail(&mut self.problems, "the name must be a string");
        };
        if self.search.contains_key(name) {
            let message = format!("`{name}` search backend is already registered");
            self.problems.push(message.clone());
            return RegisterOutcome::Rejected {
                code: ErrorCode::ExtensionFailed,
                message,
            };
        }
        let Some(Value::Object(spec)) = spec else {
            return fail(
                &mut self.problems,
                "it takes a table of `timeout` and `run`",
            );
        };
        if spec.get("run") != Some(&Value::Bool(true)) {
            return fail(&mut self.problems, "`run` must be a function");
        }
        let timeout = match spec.get("timeout") {
            None | Some(Value::Null) => return fail(&mut self.problems, "missing `timeout`"),
            Some(value) => match millis(Some(value)) {
                Some(millis) => Duration::from_millis(millis),
                None => {
                    return fail(
                        &mut self.problems,
                        "`timeout` must be a whole number of milliseconds above 0",
                    );
                }
            },
        };
        let run = self.mint(command, "run");
        self.search.insert(name.clone(), (timeout, run));
        RegisterOutcome::Accepted {
            value: Value::Object(Map::new()),
        }
    }

    /// Registers `fiber.provider` from `command` with `args`: exactly the
    /// checks `fiber.provider` runs, in its order, over JSON. The provider
    /// name stands where Lua names the provider file, which a process has
    /// none of.
    fn register_provider(&mut self, command: CommandId, args: &[Value]) -> RegisterOutcome {
        if let Some(fatal) = self.reject_lazy("fiber.provider") {
            return fatal;
        }
        let name = arg(args, 0);
        let spec = arg(args, 1);
        let violate = |problems: &mut Vec<String>, message: String| refuse(problems, message);
        let (Some(Value::String(name)), Some(Value::Object(spec))) = (name, spec) else {
            return violate(
                &mut self.problems,
                "fiber.provider: takes a name and a table of functions".to_owned(),
            );
        };
        for key in spec.keys() {
            if !matches!(
                key.as_str(),
                "models" | "quota" | "credential" | "sign" | "cost"
            ) {
                return violate(
                    &mut self.problems,
                    format!(
                        "fiber.provider: `{key}` is not models, quota, credential, sign or cost"
                    ),
                );
            }
        }
        if !self
            .manifest_providers
            .iter()
            .any(|declared| declared == name)
        {
            let error = Error::UndeclaredProvider {
                extension: self.owner.clone(),
                provider: name.clone(),
            };
            let message = error.to_string();
            let code = error.code();
            self.problems.push(message.clone());
            return RegisterOutcome::FailedPackage { code, message };
        }
        if self.providers.contains_key(name) {
            return violate(
                &mut self.problems,
                format!("fiber.provider: `{name}` registers its provider exactly once"),
            );
        }
        let mut timeouts = BTreeMap::new();
        let mut runs = BTreeMap::new();
        for (key, func) in spec {
            let what = format!("fiber.provider: `{key}`");
            let Some(Value::Object(func)) = Some(func) else {
                return violate(
                    &mut self.problems,
                    format!("{what}: `timeout` must be a whole number of milliseconds above 0"),
                );
            };
            let timeout = match func.get("timeout") {
                Some(value) => match millis(Some(value)) {
                    Some(millis) => Duration::from_millis(millis),
                    None => {
                        return violate(
                            &mut self.problems,
                            format!(
                                "{what}: `timeout` must be a whole number of milliseconds above 0"
                            ),
                        );
                    }
                },
                None => {
                    return violate(
                        &mut self.problems,
                        format!("{what}: `timeout` must be a whole number of milliseconds above 0"),
                    );
                }
            };
            if func.get("run") != Some(&Value::Bool(true)) {
                return violate(
                    &mut self.problems,
                    format!("{what}: `run` must be a function"),
                );
            }
            timeouts.insert(key.clone(), timeout);
            runs.insert(key.clone(), self.mint(command.clone(), key));
        }
        self.providers
            .insert(name.clone(), ProviderEntry { timeouts, runs });
        RegisterOutcome::Accepted {
            value: Value::Object(Map::new()),
        }
    }

    /// Registers `fiber.harness` from `command` with `args`: the documented
    /// registration (`docs/extensions.md`, "Registering"), with the Lua
    /// call's naming and timeout rules, since no Lua registry exists to
    /// mirror. Data keys ride along for the delegate runner; only the
    /// functions run.
    fn register_harness(&mut self, command: CommandId, args: &[Value]) -> RegisterOutcome {
        if let Some(fatal) = self.reject_lazy("fiber.harness") {
            return fatal;
        }
        let name = arg(args, 0);
        let spec = arg(args, 1);
        let fail = |problems: &mut Vec<String>, why: &str| {
            refuse(
                problems,
                format!("`{}` harness not registered: {why}", shown(name)),
            )
        };
        let Some(Value::String(name)) = name else {
            return fail(&mut self.problems, "the name must be a string");
        };
        if !is_plain(name) {
            return fail(
                &mut self.problems,
                &format!("names are plain: `{name}` is empty or holds whitespace, `/` or `:`"),
            );
        }
        let Some(Value::Object(spec)) = spec else {
            return fail(
                &mut self.problems,
                "it takes a table of `timeout`, `command`, `line`, `models` and optionally `quota`",
            );
        };
        let timeout = match spec.get("timeout") {
            None | Some(Value::Null) => return fail(&mut self.problems, "missing `timeout`"),
            Some(value) => match millis(Some(value)) {
                Some(millis) => Duration::from_millis(millis),
                None => {
                    return fail(
                        &mut self.problems,
                        "`timeout` must be a whole number of milliseconds above 0",
                    );
                }
            },
        };
        for key in ["command", "line", "models"] {
            if spec.get(key) != Some(&Value::Bool(true)) {
                return fail(&mut self.problems, &format!("`{key}` must be a function"));
            }
        }
        let quota = match spec.get("quota") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(true)) => Some(self.mint(command.clone(), "quota")),
            _ => return fail(&mut self.problems, "`quota` must be a function"),
        };
        if self.harness.contains_key(name) {
            let message = format!("`{name}` harness is already registered");
            self.problems.push(message.clone());
            return RegisterOutcome::Rejected {
                code: ErrorCode::ExtensionFailed,
                message,
            };
        }
        let mut runs = BTreeMap::new();
        for key in ["command", "line", "models"] {
            runs.insert(key.to_owned(), self.mint(command.clone(), key));
        }
        if let Some(quota) = quota {
            runs.insert("quota".to_owned(), quota);
        }
        let mut data = BTreeMap::new();
        for (key, value) in spec {
            if !matches!(
                key.as_str(),
                "timeout" | "command" | "line" | "models" | "quota"
            ) {
                data.insert(key.clone(), value.clone());
            }
        }
        self.harness.insert(
            name.clone(),
            HarnessEntry {
                timeout,
                data,
                runs,
            },
        );
        RegisterOutcome::Accepted {
            value: Value::Object(Map::new()),
        }
    }
}
