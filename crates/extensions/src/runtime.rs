//! A session's backend-neutral callback owner (`docs/extensions.md`,
//! "Process extensions" and "How an extension runs"). Lua and process
//! extensions register the same things and receive the same calls; this
//! runtime owns both sides' callbacks behind one reference, so no caller
//! branches on the backend.
//!
//! A registered function is a [`FunctionRef`]: the `call` command that
//! registered it, the function's key within that call (`run`, `effects`,
//! `sign`, a provider function name, or a positional `"1"`), and the
//! owner's generation that registered it. Re-registering an owner — a
//! replacement or a restart — bumps its generation, so a reference minted
//! earlier no longer resolves and can never select the new function.
//! References that name no function, or a generation that is not current,
//! fail closed with [`Error::UnknownCallback`].
//!
//! Lua dispatch forwards to the same [`LuaExtension`] methods the session
//! uses today. Process dispatch invokes the handler installed for the
//! reference; in production the transport installs it, and in tests the
//! test installs it, so the seam is exercised without a child process.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::Instant;

use support::lock;

use contract::clock::Clock;
use contract::emit::Emit;
use contract::events::Event;
use contract::hook::{HookFailure, StateLine};
use contract::tool::{Cancel, InnerCalls};
use contract::{CommandId, RequestId};
use serde_json::{Map, Value};

use crate::lua_provider::{CredentialPair, LoginMethod};
use crate::oauth::LoginSlot;
use crate::{Error, LuaExtension};

/// A registered callback: the `call` command that registered it, the
/// function's key within that call, and the owner's generation that
/// registered it (`docs/extensions.md`, "Process extensions"). Only the
/// originating generation resolves it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct FunctionRef {
    /// The `call` command that registered the function.
    pub(crate) command: CommandId,
    /// The function's key: `run`, `effects`, a provider or harness
    /// function name, or a positional `"1"`.
    pub(crate) key: String,
    /// The owner's generation that registered it.
    pub(crate) generation: u64,
}

/// What invokes one process function: the test or transport stand-in for
/// the child's answer. Arguments arrive as the Lua function would receive
/// them, as a JSON array for callbacks taking arguments and as the decoded
/// value elsewhere; a `None` answer means the function returned nothing.
pub(crate) type ProcessInvoke =
    Arc<dyn Fn(Value, &dyn Cancel) -> Result<Option<Value>, Error> + Send + Sync>;

/// One registered function's target: a Lua function in its extension's VM,
/// or a process function behind its installed invoker.
#[derive(Clone)]
pub(crate) enum FunctionTarget {
    /// A Lua function, dispatched through its extension.
    Lua(LuaBinding),
    /// A process function, dispatched through its invoker.
    Process(ProcessInvoke),
}

/// A Lua function behind a reference: its extension and which of its
/// functions to run. The handle is weak: the session's strong handles
/// decide the VM's life, and a dispatch on a dropped VM fails instead of
/// resurrecting it.
#[derive(Clone)]
pub(crate) struct LuaBinding {
    /// The extension whose VM runs the function.
    pub(crate) extension: Weak<LuaExtension>,
    /// Which of the extension's functions to run.
    pub(crate) kind: LuaKind,
}

/// Which Lua function a reference names.
#[allow(
    dead_code,
    reason = "constructed as each family wires its dispatch through references; asserted by tests"
)]
#[derive(Clone, Debug)]
pub(crate) enum LuaKind {
    /// One hook at `point`, by its place among the extension's hooks there.
    Hook {
        /// The hook point, such as `turn_end`.
        point: String,
        /// Its place among the extension's hooks at the point.
        index: usize,
    },
    /// One watcher, by its place among the extension's watchers.
    Watcher {
        /// Its place among the extension's watchers.
        index: usize,
    },
    /// One tool's `run`.
    Tool {
        /// The tool's name.
        name: String,
    },
    /// One tool's computed `effects`.
    Effects {
        /// The tool's name.
        name: String,
    },
    /// One search backend's `run`.
    Search {
        /// The backend's name.
        name: String,
    },
    /// One command's `run`.
    Command {
        /// The registered command name.
        name: String,
    },
    /// One provider function.
    Provider {
        /// The provider's name.
        provider: String,
        /// Which of its functions to run.
        function: ProviderFunction,
    },
    /// One harness function.
    Harness {
        /// The harness's name.
        harness: String,
        /// Which of its functions to run.
        function: String,
    },
}

/// A provider function a reference can name: the five `fiber.provider`
/// functions (`docs/extensions.md`, "Registering").
#[allow(
    dead_code,
    reason = "constructed as provider dispatch wires through references in a later part; asserted by tests"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderFunction {
    /// The provider's `models`.
    Models,
    /// The provider's `quota`.
    Quota,
    /// The provider's `credential`.
    Credential,
    /// The provider's `sign`.
    Sign,
    /// The provider's `cost`.
    Cost,
}

#[allow(
    dead_code,
    reason = "read once provider dispatch resolves references in a later part; asserted by tests"
)]
impl ProviderFunction {
    /// The function's name, as `fiber.provider` registers it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Models => "models",
            Self::Quota => "quota",
            Self::Credential => "credential",
            Self::Sign => "sign",
            Self::Cost => "cost",
        }
    }
}

/// A resolved reference: the owner it named and the current target.
#[derive(Clone)]
pub(crate) struct ResolvedFunction {
    /// The owner that registered the function.
    pub(crate) owner: String,
    /// The function to run.
    pub(crate) target: FunctionTarget,
}

impl std::fmt::Debug for ResolvedFunction {
    /// Names the owner and the backend, never the function itself: an
    /// `Arc` behind the target has no `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let backend = match self.target {
            FunctionTarget::Lua(_) => "lua",
            FunctionTarget::Process(_) => "process",
        };
        f.debug_struct("ResolvedFunction")
            .field("owner", &self.owner)
            .field("backend", &backend)
            .finish()
    }
}

/// One callback owner: its current generation and, for Lua, its VM.
struct Owner {
    /// The generation the next registration mints.
    generation: u64,
    /// The owner's VM, when it is a Lua extension. Weak, so a dropped
    /// provider-only VM stays dropped and `start_provider` starts fresh.
    lua: Option<Weak<LuaExtension>>,
}

struct Inner {
    /// The session clock, read for process watcher offers.
    clock: Arc<dyn Clock>,
    /// Every owner in registration order: what a reply offer walks.
    order: Vec<String>,
    /// Each owner by name.
    owners: BTreeMap<String, Owner>,
    /// Each registered function by its full reference, beside its owner.
    functions: BTreeMap<(CommandId, String, u64), (String, FunctionTarget)>,
    /// Each pending callback request by its id.
    #[allow(
        dead_code,
        reason = "request correlation retires through here once the transport holds calls in a later part"
    )]
    pending: BTreeMap<RequestId, FunctionRef>,
    /// The session's emitter, for process failures with no VM to emit them.
    emit: Option<Arc<dyn Emit>>,
}

/// The session's backend-neutral callback owner: generations, function
/// references, pending requests and dispatch, shared by every adapter.
#[derive(Clone)]
pub(crate) struct Runtime {
    /// The registry. The lock is a leaf: no callback runs under it.
    inner: Arc<Mutex<Inner>>,
}

impl Default for Runtime {
    /// An empty runtime on the system clock, for struct-update syntax: a
    /// load always replaces it with the session's clock before dispatch.
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                clock: Arc::new(support::clock::System),
                order: Vec::new(),
                owners: BTreeMap::new(),
                functions: BTreeMap::new(),
                pending: BTreeMap::new(),
                emit: None,
            })),
        }
    }
}

impl Runtime {
    /// An empty runtime on `clock`.
    pub(crate) fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                clock,
                order: Vec::new(),
                owners: BTreeMap::new(),
                functions: BTreeMap::new(),
                pending: BTreeMap::new(),
                emit: None,
            })),
        }
    }

    /// Registers the Lua owner `name`, bumping its generation, and returns
    /// the new generation. A replacement or restart re-registers, so older
    /// references stop resolving. The registry keeps only a weak handle:
    /// strong handles elsewhere decide the VM's life.
    pub(crate) fn register_lua(&self, name: &str, extension: &Arc<LuaExtension>) -> u64 {
        let mut inner = lock(&self.inner);
        let generation = {
            let owner = inner.owners.entry(name.to_owned()).or_insert(Owner {
                generation: 0,
                lua: None,
            });
            owner.generation += 1;
            owner.lua = Some(Arc::downgrade(extension));
            owner.generation
        };
        if !inner.order.iter().any(|ordered| ordered == name) {
            inner.order.push(name.to_owned());
        }
        generation
    }

    /// Registers the process owner `name`, bumping its generation, and
    /// returns the new generation.
    pub(crate) fn register_process(&self, name: &str) -> u64 {
        let mut inner = lock(&self.inner);
        let generation = {
            let owner = inner.owners.entry(name.to_owned()).or_insert(Owner {
                generation: 0,
                lua: None,
            });
            owner.generation += 1;
            owner.generation
        };
        if !inner.order.iter().any(|ordered| ordered == name) {
            inner.order.push(name.to_owned());
        }
        generation
    }

    /// The owner's current generation, when it registered one.
    #[allow(
        dead_code,
        reason = "observed by tests pinning generation-keyed references"
    )]
    pub(crate) fn generation(&self, name: &str) -> Option<u64> {
        lock(&self.inner)
            .owners
            .get(name)
            .map(|owner| owner.generation)
    }

    /// Registers `target` for `owner` under the owner's current generation
    /// and returns its reference. Registering for an unknown owner records
    /// a reference that never resolves, so a missing registration fails
    /// closed instead of creating an owner.
    pub(crate) fn register_function(
        &self,
        owner: &str,
        command: CommandId,
        key: String,
        target: FunctionTarget,
    ) -> FunctionRef {
        let mut inner = lock(&self.inner);
        let generation = inner
            .owners
            .get(owner)
            .map(|known| known.generation)
            .unwrap_or(0);
        inner.functions.insert(
            (command.clone(), key.clone(), generation),
            (owner.to_owned(), target),
        );
        FunctionRef {
            command,
            key,
            generation,
        }
    }

    /// Installs the process `invoke` for (`owner`, `command`, `key`) under
    /// the owner's current generation and returns its reference: the seam
    /// the transport fills in production and tests fill directly.
    #[allow(
        dead_code,
        reason = "installs the transport's handler in a later part; the tests' stand-in"
    )]
    pub(crate) fn install_process_handler(
        &self,
        owner: &str,
        command: CommandId,
        key: &str,
        invoke: ProcessInvoke,
    ) -> FunctionRef {
        self.register_function(
            owner,
            command,
            key.to_owned(),
            FunctionTarget::Process(invoke),
        )
    }

    /// Resolves the reference against its originating generation: only the
    /// generation that registered it answers. A reference that names no
    /// function, or a generation that is not current, fails with
    /// [`Error::UnknownCallback`], and never selects another function.
    pub(crate) fn resolve(&self, func: &FunctionRef) -> Result<ResolvedFunction, Error> {
        let inner = lock(&self.inner);
        let (owner, target) = inner
            .functions
            .get(&(func.command.clone(), func.key.clone(), func.generation))
            .cloned()
            .ok_or_else(|| unknown(func, None))?;
        let current = inner.owners.get(&owner).map(|known| known.generation);
        if current != Some(func.generation) {
            return Err(unknown(func, Some(&owner)));
        }
        Ok(ResolvedFunction { owner, target })
    }

    /// The owner a reference names, whether or not its generation is
    /// current: notices can name the owner without resolving it. A
    /// reference for an owner that never registered names none.
    pub(crate) fn owner_name(&self, func: &FunctionRef) -> Option<String> {
        let inner = lock(&self.inner);
        let (owner, _) =
            inner
                .functions
                .get(&(func.command.clone(), func.key.clone(), func.generation))?;
        inner.owners.contains_key(owner).then(|| owner.clone())
    }

    /// Remembers that `id` waits on `func`. Answered, timed out and
    /// cancelled requests leave through [`Runtime::pending_take`], so one
    /// request answers once.
    #[allow(
        dead_code,
        reason = "request correlation holds here once the transport queues calls in a later part; asserted by tests"
    )]
    pub(crate) fn pending_insert(&self, id: RequestId, func: FunctionRef) {
        lock(&self.inner).pending.insert(id, func);
    }

    /// Takes the reference `id` waits on, if any. Taking removes it, so a
    /// late reply finds nothing and fails stale.
    #[allow(
        dead_code,
        reason = "request correlation retires through here once the transport queues calls in a later part; asserted by tests"
    )]
    pub(crate) fn pending_take(&self, id: &RequestId) -> Option<FunctionRef> {
        lock(&self.inner).pending.remove(id)
    }

    /// The reference `id` waits on, if any, left in place.
    #[allow(dead_code, reason = "observed by tests pinning request correlation")]
    pub(crate) fn pending_get(&self, id: &RequestId) -> Option<FunctionRef> {
        lock(&self.inner).pending.get(id).cloned()
    }

    /// Installs the session's emitter, for process failures with no VM.
    pub(crate) fn set_emit(&self, emit: Arc<dyn Emit>) {
        lock(&self.inner).emit = Some(emit);
    }

    /// Emits an ephemeral event through the installed sink, if any.
    pub(crate) fn emit(&self, event: &Event) {
        if let Some(emit) = lock(&self.inner).emit.clone() {
            emit.emit(event);
        }
    }

    /// Runs the hook behind `func` on `arg` and decodes its answer, as
    /// [`LuaExtension::hook_with`] does: the state lines a hook queued and
    /// whether the decoded answer changed anything. A process handler
    /// answering nothing fails the hook, as an empty hook answer would.
    pub(crate) fn hook_with<T>(
        &self,
        func: &FunctionRef,
        arg: Value,
        accept: impl FnOnce(Value) -> Result<(T, bool), Error>,
    ) -> Result<(T, Vec<StateLine>, bool), Error> {
        let resolved = self.resolve(func)?;
        match resolved.target {
            FunctionTarget::Lua(binding) => {
                if let LuaKind::Hook { point, index } = &binding.kind {
                    let extension = binding
                        .extension
                        .upgrade()
                        .ok_or_else(|| gone(func, &resolved.owner))?;
                    extension.hook_with(point, *index, arg, accept)
                } else {
                    Err(unknown(func, Some(resolved.owner.as_str())))
                }
            }
            FunctionTarget::Process(invoke) => {
                let cancel = support::cancel::Token::new();
                let returned =
                    invoke(arg, &cancel)?.ok_or_else(|| no_answer(func, &resolved.owner))?;
                accept(returned).map(|(value, changed)| (value, Vec::new(), changed))
            }
        }
    }

    /// Runs the tool behind `func` on `args`, as `LuaTool` does: the
    /// handler's answer, or nothing when it answered nothing.
    pub(crate) fn tool_run(
        &self,
        func: &FunctionRef,
        args: Value,
        cancel: &dyn Cancel,
    ) -> Result<Option<Value>, Error> {
        let resolved = self.resolve(func)?;
        match resolved.target {
            FunctionTarget::Lua(binding) => {
                if let LuaKind::Tool { name } = &binding.kind {
                    let extension = binding
                        .extension
                        .upgrade()
                        .ok_or_else(|| gone(func, &resolved.owner))?;
                    extension.tool_run(name, args, cancel)
                } else {
                    Err(unknown(func, Some(resolved.owner.as_str())))
                }
            }
            FunctionTarget::Process(invoke) => invoke(args, cancel),
        }
    }

    /// Runs the tool behind `func` as [`Runtime::tool_run`] does, with
    /// `runner` for the inner calls the callback makes. A process callback
    /// has no inner-call runner yet, so the runner is the model's path
    /// alone and the call runs anyway.
    pub(crate) fn tool_run_asking(
        &self,
        func: &FunctionRef,
        args: Value,
        cancel: &dyn Cancel,
        runner: Option<Arc<dyn InnerCalls>>,
    ) -> Result<Option<Value>, Error> {
        let resolved = self.resolve(func)?;
        match resolved.target {
            FunctionTarget::Lua(binding) => {
                if let LuaKind::Tool { name } = &binding.kind {
                    let extension = binding
                        .extension
                        .upgrade()
                        .ok_or_else(|| gone(func, &resolved.owner))?;
                    extension.tool_run_asking(name, args, cancel, runner)
                } else {
                    Err(unknown(func, Some(resolved.owner.as_str())))
                }
            }
            FunctionTarget::Process(invoke) => invoke(args, cancel),
        }
    }

    /// Runs the tool's computed `effects` behind `func` on `args`.
    pub(crate) fn tool_effects(&self, func: &FunctionRef, args: Value) -> Result<Value, Error> {
        let resolved = self.resolve(func)?;
        match resolved.target {
            FunctionTarget::Lua(binding) => {
                if let LuaKind::Effects { name } = &binding.kind {
                    let extension = binding
                        .extension
                        .upgrade()
                        .ok_or_else(|| gone(func, &resolved.owner))?;
                    extension.tool_effects(name, args)
                } else {
                    Err(unknown(func, Some(resolved.owner.as_str())))
                }
            }
            FunctionTarget::Process(invoke) => {
                let cancel = support::cancel::Token::new();
                invoke(args, &cancel)?.ok_or_else(|| no_answer(func, &resolved.owner))
            }
        }
    }

    /// Runs the search backend behind `func` on `args`.
    pub(crate) fn search_run(
        &self,
        func: &FunctionRef,
        args: Value,
        cancel: &dyn Cancel,
    ) -> Result<Option<Value>, Error> {
        let resolved = self.resolve(func)?;
        match resolved.target {
            FunctionTarget::Lua(binding) => {
                if let LuaKind::Search { name } = &binding.kind {
                    let extension = binding
                        .extension
                        .upgrade()
                        .ok_or_else(|| gone(func, &resolved.owner))?;
                    extension.search_run(name, args, cancel)
                } else {
                    Err(unknown(func, Some(resolved.owner.as_str())))
                }
            }
            FunctionTarget::Process(invoke) => invoke(args, cancel),
        }
    }

    /// Runs the command behind `func` with `text`, as
    /// [`LuaExtension::command`] does: its returned text, or empty when it
    /// answered nothing.
    #[allow(
        dead_code,
        reason = "the synchronous command dispatch beside the queued one; exercised by tests"
    )]
    pub(crate) fn command_run(&self, func: &FunctionRef, text: &str) -> Result<String, Error> {
        let resolved = self.resolve(func)?;
        match resolved.target {
            FunctionTarget::Lua(binding) => {
                if let LuaKind::Command { name } = &binding.kind {
                    let extension = binding
                        .extension
                        .upgrade()
                        .ok_or_else(|| gone(func, &resolved.owner))?;
                    extension.command(name, text)
                } else {
                    Err(unknown(func, Some(resolved.owner.as_str())))
                }
            }
            FunctionTarget::Process(invoke) => {
                let cancel = support::cancel::Token::new();
                let returned = invoke(Value::String(text.to_owned()), &cancel)?;
                Ok(returned
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default())
            }
        }
    }

    /// Queues the command behind `func` with `text` as a held call: it
    /// never starts until the waiter's release. A process command runs its
    /// handler on release and the wait sees its answer.
    pub(crate) fn queue_command(
        &self,
        func: &FunctionRef,
        id: CommandId,
        text: &str,
    ) -> Result<CommandWait, Error> {
        let resolved = self.resolve(func)?;
        match resolved.target {
            FunctionTarget::Lua(binding) => {
                if let LuaKind::Command { name } = &binding.kind {
                    let extension = binding
                        .extension
                        .upgrade()
                        .ok_or_else(|| gone(func, &resolved.owner))?;
                    extension
                        .queue_command(id, name, text)
                        .map(|queued| CommandWait::Lua(Arc::new(queued)))
                } else {
                    Err(unknown(func, Some(resolved.owner.as_str())))
                }
            }
            FunctionTarget::Process(invoke) => Ok(CommandWait::Process(Arc::new(
                ProcessQueued::new(resolved.owner, invoke, Value::String(text.to_owned())),
            ))),
        }
    }

    /// Whether the watcher behind `func` is busy: a process watcher never
    /// is, since its handler runs to completion on every offer.
    pub(crate) fn is_watcher_busy(&self, func: &FunctionRef) -> bool {
        self.resolve(func)
            .ok()
            .and_then(|resolved| match resolved.target {
                FunctionTarget::Lua(binding) => {
                    if let LuaKind::Watcher { index } = &binding.kind {
                        let extension = binding.extension.upgrade()?;
                        Some(extension.is_watcher_busy(*index))
                    } else {
                        None
                    }
                }
                FunctionTarget::Process(_) => Some(false),
            })
            .unwrap_or(false)
    }

    /// Offers one event to the watcher behind `func`: a queued offer
    /// advances the fan-out cursor. A process handler runs at once and the
    /// offer counts as queued.
    pub(crate) fn push_watcher(&self, func: &FunctionRef, arg: Value) -> Option<(u64, Instant)> {
        let resolved = self.resolve(func).ok()?;
        match resolved.target {
            FunctionTarget::Lua(binding) => {
                if let LuaKind::Watcher { index } = &binding.kind {
                    let extension = binding.extension.upgrade()?;
                    extension.push_watcher(*index, arg)
                } else {
                    None
                }
            }
            FunctionTarget::Process(invoke) => {
                let cancel = support::cancel::Token::new();
                invoke(arg, &cancel).ok()??;
                Some((0, lock(&self.inner).clock.now()))
            }
        }
    }

    /// Runs the watcher behind `func` on one drained log line at session
    /// end, as the fan-out does.
    pub(crate) fn drain_watcher(&self, func: &FunctionRef, arg: Value) -> Result<(), Error> {
        let resolved = self.resolve(func)?;
        match resolved.target {
            FunctionTarget::Lua(binding) => {
                if let LuaKind::Watcher { index } = &binding.kind {
                    let extension = binding
                        .extension
                        .upgrade()
                        .ok_or_else(|| gone(func, &resolved.owner))?;
                    extension.drain_watcher(*index, arg)
                } else {
                    Err(unknown(func, Some(resolved.owner.as_str())))
                }
            }
            FunctionTarget::Process(invoke) => {
                let cancel = support::cancel::Token::new();
                invoke(arg, &cancel).map(|_| ())
            }
        }
    }

    /// Waits for the owner's watchers to idle at session end, as the drain
    /// does. A process owner has no queue, so it is always idle.
    pub(crate) fn wait_for_watchers_idle(&self, owner: &str) {
        let lua = lock(&self.inner)
            .owners
            .get(owner)
            .and_then(|known| known.lua.clone())
            .and_then(|weak| weak.upgrade());
        if let Some(lua) = lua {
            lua.wait_for_watchers_idle();
        }
    }

    /// Whether the hook behind `func` would wait behind its extension's own
    /// command: only a Lua command running in its VM refuses its own hooks,
    /// and a process owner never owns a command call.
    pub(crate) fn runs_own_command(&self, func: &FunctionRef, point: &str) -> Option<HookFailure> {
        let resolved = self.resolve(func).ok()?;
        match resolved.target {
            FunctionTarget::Lua(binding) => {
                let extension = binding.extension.upgrade()?;
                if extension.runs_own_command_call() {
                    Some(HookFailure {
                        extension: resolved.owner,
                        message: format!(
                            "The `{point}` hook would wait behind its extension's command"
                        ),
                    })
                } else {
                    None
                }
            }
            FunctionTarget::Process(_) => None,
        }
    }

    /// Runs the provider function behind `func` on `arg`.
    #[allow(
        dead_code,
        reason = "provider and harness dispatch wires through references in a later part; exercised by tests"
    )]
    pub(crate) fn provider_call(&self, func: &FunctionRef, arg: Value) -> Result<Value, Error> {
        let resolved = self.resolve(func)?;
        match resolved.target {
            FunctionTarget::Lua(binding) => {
                if let LuaKind::Provider { provider, function } = &binding.kind {
                    let extension = binding
                        .extension
                        .upgrade()
                        .ok_or_else(|| gone(func, &resolved.owner))?;
                    extension.provider_call(provider, function.name(), arg)
                } else {
                    Err(unknown(func, Some(resolved.owner.as_str())))
                }
            }
            FunctionTarget::Process(invoke) => {
                let cancel = support::cancel::Token::new();
                invoke(arg, &cancel)?.ok_or_else(|| no_answer(func, &resolved.owner))
            }
        }
    }

    /// Runs the `credential` behind `func` for `pair`, passing the session's
    /// label and the stored credential's name, as `LuaExtension` does.
    #[allow(
        dead_code,
        reason = "provider and harness dispatch wires through references in a later part; exercised by tests"
    )]
    pub(crate) fn provider_credential(
        &self,
        func: &FunctionRef,
        pair: &CredentialPair,
    ) -> Result<Value, Error> {
        let resolved = self.resolve(func)?;
        match resolved.target {
            FunctionTarget::Lua(binding) => {
                if let LuaKind::Provider { provider, .. } = &binding.kind {
                    let extension = binding
                        .extension
                        .upgrade()
                        .ok_or_else(|| gone(func, &resolved.owner))?;
                    extension.provider_credential(provider, pair)
                } else {
                    Err(unknown(func, Some(resolved.owner.as_str())))
                }
            }
            FunctionTarget::Process(invoke) => {
                let cancel = support::cancel::Token::new();
                let arg = Value::Object(Map::from_iter([
                    ("label".to_owned(), Value::String(pair.label.clone())),
                    (
                        "credential".to_owned(),
                        Value::String(pair.credential.clone()),
                    ),
                ]));
                invoke(arg, &cancel)?.ok_or_else(|| no_answer(func, &resolved.owner))
            }
        }
    }

    /// Runs the `credential` behind `func` as a login, passing the stored
    /// credential's name, the `--as` label and the login method, as
    /// `LuaExtension` does.
    #[allow(
        dead_code,
        reason = "provider and harness dispatch wires through references in a later part; exercised by tests"
    )]
    pub(crate) fn provider_login(
        &self,
        func: &FunctionRef,
        credential: &str,
        label: Option<&str>,
        method: LoginMethod,
    ) -> Result<Value, Error> {
        let resolved = self.resolve(func)?;
        match resolved.target {
            FunctionTarget::Lua(binding) => {
                if let LuaKind::Provider { provider, .. } = &binding.kind {
                    let extension = binding
                        .extension
                        .upgrade()
                        .ok_or_else(|| gone(func, &resolved.owner))?;
                    extension.provider_login(
                        provider,
                        credential,
                        label,
                        method,
                        LoginSlot::default(),
                    )
                } else {
                    Err(unknown(func, Some(resolved.owner.as_str())))
                }
            }
            FunctionTarget::Process(invoke) => {
                let cancel = support::cancel::Token::new();
                let login = match method {
                    LoginMethod::Browser => "browser",
                    LoginMethod::Device => "device",
                };
                let mut arg = Map::from_iter([
                    (
                        "credential".to_owned(),
                        Value::String(credential.to_owned()),
                    ),
                    ("login".to_owned(), Value::String(login.to_owned())),
                ]);
                if let Some(label) = label {
                    arg.insert("label".to_owned(), Value::String(label.to_owned()));
                }
                invoke(Value::Object(arg), &cancel)?.ok_or_else(|| no_answer(func, &resolved.owner))
            }
        }
    }

    /// Runs the harness function behind `func` on `arg`: `command`, `line`,
    /// `models` or the optional `quota`, as the delegate runner asks.
    #[allow(
        dead_code,
        reason = "provider and harness dispatch wires through references in a later part; exercised by tests"
    )]
    pub(crate) fn harness_call(&self, func: &FunctionRef, arg: Value) -> Result<Value, Error> {
        let resolved = self.resolve(func)?;
        match resolved.target {
            FunctionTarget::Lua(binding) => {
                if let LuaKind::Harness { harness, function } = &binding.kind {
                    Err(Error::UnknownCallback {
                        extension: resolved.owner,
                        callback: format!("harness `{harness}` has no `{function}` in Lua"),
                    })
                } else {
                    Err(unknown(func, Some(resolved.owner.as_str())))
                }
            }
            FunctionTarget::Process(invoke) => {
                let cancel = support::cancel::Token::new();
                invoke(arg, &cancel)?.ok_or_else(|| no_answer(func, &resolved.owner))
            }
        }
    }

    /// Offers a reply to the owner holding its request, in registration
    /// order: the first owner that takes it answers. Process owners hold no
    /// requests yet, so the reply passes them by.
    pub(crate) fn answer(
        &self,
        mut reply: contract::commands::Reply,
        mut ack: contract::inbox::Ack,
    ) -> Option<(contract::commands::Reply, contract::inbox::Ack)> {
        let order = lock(&self.inner).order.clone();
        for owner in &order {
            let lua = lock(&self.inner)
                .owners
                .get(owner)
                .and_then(|known| known.lua.clone())
                .and_then(|weak| weak.upgrade());
            if let Some(lua) = lua {
                (reply, ack) = lua.answer(reply, ack)?;
            }
        }
        Some((reply, ack))
    }
}

/// A reference that names no current function: what every stale or unknown
/// dispatch fails with. The owner names the extension when the reference
/// named a known one, else the command that registered it.
fn unknown(func: &FunctionRef, owner: Option<&str>) -> Error {
    match owner {
        None => Error::UnknownCallback {
            extension: format!("command `{}`", func.command.0),
            callback: format!("`{}` (generation {})", func.key, func.generation),
        },
        Some(owner) => Error::UnknownCallback {
            extension: owner.to_owned(),
            callback: format!(
                "`{}` (generation {}): it has since re-registered",
                func.key, func.generation
            ),
        },
    }
}

/// One queued command both backends admit: it never starts until its
/// release, and the wait sees the run's answer.
#[derive(Clone)]
pub(crate) enum CommandWait {
    /// A Lua command held in its VM's queue.
    Lua(Arc<crate::lua::Queued>),
    /// A process command held for its release.
    Process(Arc<ProcessQueued>),
}

impl CommandWait {
    /// Releases the call, so the run may start.
    pub(crate) fn release(&self) {
        match self {
            Self::Lua(queued) => queued.release(),
            Self::Process(queued) => queued.release(),
        }
    }

    /// Waits until the run ends, and returns its answer.
    pub(crate) fn wait(&self) -> Result<Value, Error> {
        match self {
            Self::Lua(queued) => queued.wait(),
            Self::Process(queued) => queued.wait(),
        }
    }
}

/// A held queued process command: the handler runs on release, and the
/// wait sees its answer. Release runs the handler on the releasing thread,
/// so the door protocol holds: admit, wait on one thread, release on the
/// other.
pub(crate) struct ProcessQueued {
    /// The run, guarded: release runs it once, and the wait sees the one
    /// answer.
    state: Mutex<ProcessQueuedState>,
    /// Wakes the wait when release answers.
    done: Condvar,
}

struct ProcessQueuedState {
    /// The owner that registered the command, for the failure message.
    owner: String,
    /// The command's handler, taken by the release that runs it.
    invoke: Option<ProcessInvoke>,
    /// The command's text, as its handler receives it.
    arg: Value,
    /// The run's answer, once release ran it.
    answer: Option<Result<Value, Error>>,
}

impl ProcessQueued {
    /// A held call of `owner` running `invoke` on `arg` at release.
    pub(crate) fn new(owner: String, invoke: ProcessInvoke, arg: Value) -> Self {
        Self {
            state: Mutex::new(ProcessQueuedState {
                owner,
                invoke: Some(invoke),
                arg,
                answer: None,
            }),
            done: Condvar::new(),
        }
    }

    /// Runs the handler, so the wait sees its answer. A second release
    /// changes nothing: the first answer stands.
    pub(crate) fn release(&self) {
        let mut state = lock(&self.state);
        if state.answer.is_some() {
            return;
        }
        let invoke = state.invoke.take();
        let arg = state.arg.clone();
        let owner = state.owner.clone();
        if let Some(invoke) = invoke {
            let cancel = support::cancel::Token::new();
            state.answer = Some(invoke(arg, &cancel).and_then(|returned| {
                returned.ok_or_else(|| Error::BadReturn {
                    extension: owner,
                    callback: "command".to_owned(),
                    why: "returned nothing".to_owned(),
                })
            }));
        }
        self.done.notify_all();
    }

    /// Waits until a release answers the run, and takes the one answer: a
    /// second wait finds no answer and fails stopped instead of hanging.
    pub(crate) fn wait(&self) -> Result<Value, Error> {
        let mut state = lock(&self.state);
        while state.answer.is_none() && state.invoke.is_some() {
            state = self
                .done
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state.answer.take().unwrap_or_else(|| {
            Err(Error::Stopped {
                extension: state.owner.clone(),
            })
        })
    }
}

/// A function whose extension is gone: the session dropped its VM, so no
/// call runs. A dropped provider-only VM reads this way, and a later
/// `start_provider` starts fresh.
fn gone(func: &FunctionRef, owner: &str) -> Error {
    Error::UnknownCallback {
        extension: owner.to_owned(),
        callback: format!(
            "`{}` (generation {}): its extension is no longer running",
            func.key, func.generation
        ),
    }
}

/// A function that answered nothing where an answer was owed.
fn no_answer(func: &FunctionRef, owner: &str) -> Error {
    Error::BadReturn {
        extension: owner.to_owned(),
        callback: format!("`{}` (generation {})", func.key, func.generation),
        why: "returned nothing".to_owned(),
    }
}
