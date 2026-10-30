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
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use support::lock;

use contract::CommandId;
use contract::clock::Clock;
use contract::emit::Emit;
use contract::events::Event;
use contract::hook::{HookFailure, StateLine};
use contract::tool::{Cancel, InnerCalls};
use serde_json::Value;

use crate::lua::CredentialFor;
use crate::{Error, LuaExtension};

mod queued;
mod refs;

pub(crate) use queued::{CommandWait, ProcessQueued};
pub(crate) use refs::{
    FunctionRef, FunctionTarget, LuaBinding, LuaKind, ProcessInvoke, ResolvedFunction,
};
use refs::{gone, no_answer, unknown};

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
    /// The session's emitter, for process failures with no VM to emit them.
    emit: Option<Arc<dyn Emit>>,
}

/// The session's backend-neutral callback owner: generations, function
/// references and dispatch, shared by every adapter.
#[derive(Clone)]
pub(crate) struct Runtime {
    /// The registry. The lock is a leaf: no callback runs under it.
    inner: Arc<Mutex<Inner>>,
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
                emit: None,
            })),
        }
    }

    /// Registers the owner `name`, bumping its generation, and returns
    /// the new generation. A replacement or restart re-registers, so older
    /// references stop resolving. A Lua owner passes its extension: the
    /// registry keeps only a weak handle, so strong handles elsewhere
    /// decide the VM's life, and a `None` handle leaves a stored VM in
    /// place, so re-registering a process owner never drops one.
    pub(crate) fn register_owner(&self, name: &str, lua: Option<&Arc<LuaExtension>>) -> u64 {
        let mut inner = lock(&self.inner);
        let generation = {
            let owner = inner.owners.entry(name.to_owned()).or_insert(Owner {
                generation: 0,
                lua: None,
            });
            owner.generation += 1;
            if let Some(extension) = lua {
                owner.lua = Some(Arc::downgrade(extension));
            }
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
    /// `scope` is the invocation's credential, carried to the process
    /// handler; hooks never refresh, so callers pass `None`.
    pub(crate) fn hook_with<T>(
        &self,
        func: &FunctionRef,
        arg: Value,
        scope: Option<CredentialFor>,
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
                    invoke(arg, scope, &cancel)?.ok_or_else(|| no_answer(func, &resolved.owner))?;
                accept(returned).map(|(value, changed)| (value, Vec::new(), changed))
            }
        }
    }

    /// Runs the tool behind `func` on `args`, as `LuaTool` does: the
    /// handler's answer, or nothing when it answered nothing. `scope` is
    /// the invocation's credential, carried to the process handler;
    /// ordinary tool calls pass `None`.
    pub(crate) fn tool_run(
        &self,
        func: &FunctionRef,
        args: Value,
        scope: Option<CredentialFor>,
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
            FunctionTarget::Process(invoke) => invoke(args, scope, cancel),
        }
    }

    /// Runs the tool behind `func` as [`Runtime::tool_run`] does, with
    /// `runner` for the inner calls the callback makes. A process callback
    /// has no inner-call runner yet, so the runner is the model's path
    /// alone and the call runs anyway. `scope` is the invocation's
    /// credential, carried to the process handler.
    pub(crate) fn tool_run_asking(
        &self,
        func: &FunctionRef,
        args: Value,
        scope: Option<CredentialFor>,
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
            FunctionTarget::Process(invoke) => invoke(args, scope, cancel),
        }
    }

    /// Runs the tool's computed `effects` behind `func` on `args`.
    /// `scope` is the invocation's credential, carried to the process
    /// handler; effects never refresh, so callers pass `None`.
    pub(crate) fn tool_effects(
        &self,
        func: &FunctionRef,
        args: Value,
        scope: Option<CredentialFor>,
    ) -> Result<Value, Error> {
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
                invoke(args, scope, &cancel)?.ok_or_else(|| no_answer(func, &resolved.owner))
            }
        }
    }

    /// Runs the search backend behind `func` on `args`. `scope` is the
    /// invocation's credential, carried to the process handler; search
    /// never refreshes, so callers pass `None`.
    pub(crate) fn search_run(
        &self,
        func: &FunctionRef,
        args: Value,
        scope: Option<CredentialFor>,
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
            FunctionTarget::Process(invoke) => invoke(args, scope, cancel),
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
                ProcessQueued::new(resolved.owner, invoke, Value::String(text.to_owned()), None),
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
    /// offer counts as queued. `scope` is the invocation's credential,
    /// carried to the process handler; watchers never refresh, so callers
    /// pass `None`.
    pub(crate) fn push_watcher(
        &self,
        func: &FunctionRef,
        arg: Value,
        scope: Option<CredentialFor>,
    ) -> Option<(u64, Instant)> {
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
                invoke(arg, scope, &cancel).ok()??;
                Some((0, lock(&self.inner).clock.now()))
            }
        }
    }

    /// Runs the watcher behind `func` on one drained log line at session
    /// end, as the fan-out does. `scope` is the invocation's credential,
    /// carried to the process handler; watchers never refresh, so callers
    /// pass `None`.
    pub(crate) fn drain_watcher(
        &self,
        func: &FunctionRef,
        arg: Value,
        scope: Option<CredentialFor>,
    ) -> Result<(), Error> {
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
                invoke(arg, scope, &cancel).map(|_| ())
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
