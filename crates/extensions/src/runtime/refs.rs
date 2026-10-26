//! What one callback reference names (`super`): the `call` command that
//! registered the function, the function's key within that call, and the
//! owner's generation that registered it. The vocabulary is shared: the
//! registry mints references, the runtime resolves them, and every adapter
//! dispatches through them without naming a backend.
//!
//! Only the originating generation resolves a reference. References that
//! name no function, or a generation that is not current, fail closed with
//! [`Error::UnknownCallback`].

use std::sync::{Arc, Weak};

use serde_json::Value;

use contract::CommandId;
use contract::tool::Cancel;

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

/// A reference that names no current function: what every stale or unknown
/// dispatch fails with. The owner names the extension when the reference
/// named a known one, else the command that registered it.
pub(super) fn unknown(func: &FunctionRef, owner: Option<&str>) -> Error {
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

/// A function whose extension is gone: the session dropped its VM, so no
/// call runs. A dropped provider-only VM reads this way, and a later
/// `start_provider` starts fresh.
pub(super) fn gone(func: &FunctionRef, owner: &str) -> Error {
    Error::UnknownCallback {
        extension: owner.to_owned(),
        callback: format!(
            "`{}` (generation {}): its extension is no longer running",
            func.key, func.generation
        ),
    }
}

/// A function that answered nothing where an answer was owed.
pub(super) fn no_answer(func: &FunctionRef, owner: &str) -> Error {
    Error::BadReturn {
        extension: owner.to_owned(),
        callback: format!("`{}` (generation {})", func.key, func.generation),
        why: "returned nothing".to_owned(),
    }
}
