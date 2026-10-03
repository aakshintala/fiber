//! The VM's prelude, its `require`, and the deadline hook shared by every
//! coroutine (`docs/extensions.md`, "How an extension runs").

use std::cell::Cell;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use contract::clock::Clock;
use mlua::{Function, HookTriggers, Lua, Table, Thread, VmState};

use super::{CHECK_EVERY, PRELUDE};

use crate::host;

/// One step of a resumed coroutine: it returned, or it suspended on
/// `host.http`.
pub(super) enum Poll {
    Done(mlua::Value),
    Http(host::HttpRequest),
}

/// Removes the base library's I/O, which belongs to the host, and runs the
/// prelude. Returns the tables `fiber.command` and `fiber.provider` fill.
pub(super) fn install(
    lua: &Lua,
    deadline: &Deadline,
    dir: PathBuf,
    memory_cap: usize,
) -> mlua::Result<(Table, Table)> {
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
        Ok(match load_file(lua, &dir, &file, memory_cap)? {
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
pub(super) fn load_file(
    lua: &Lua,
    dir: &Path,
    file: &str,
    memory_cap: usize,
) -> mlua::Result<Result<Function, String>> {
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
    let limit = u64::try_from(memory_cap)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    if let Err(e) = File::open(&path).and_then(|f| f.take(limit).read_to_end(&mut source)) {
        return Ok(Err(format!("`{file}`: {e}")));
    }
    if source.len() > memory_cap {
        return Ok(Err(format!(
            "`{file}` is larger than the extension's memory cap of {memory_cap} bytes"
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
pub(super) fn message(e: &mlua::Error) -> String {
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
#[derive(Clone)]
pub(crate) struct Deadline {
    at: Rc<Cell<Option<Instant>>>,
    clock: Arc<dyn Clock>,
}

impl Deadline {
    pub(super) fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            at: Rc::default(),
            clock,
        }
    }

    /// The deadline the hook stops at, if one is set.
    pub(super) fn at(&self) -> Option<Instant> {
        self.at.get()
    }

    /// Points the hook at `at`, the deadline of the callback about to run.
    pub(super) fn restore(&self, at: Option<Instant>) {
        self.at.set(at);
    }

    /// Whether the deadline has passed.
    pub(super) fn passed(&self) -> bool {
        self.at.get().is_some_and(|at| self.clock.now() >= at)
    }

    /// Arms `thread` so the hook stops it at this deadline.
    pub(super) fn arm(&self, thread: &Thread) -> mlua::Result<()> {
        let deadline = self.clone();
        thread.set_hook(
            HookTriggers::new().every_nth_instruction(CHECK_EVERY),
            move |lua, _| {
                if !deadline.passed() {
                    return Ok(VmState::Continue);
                }
                let escalated = deadline.clone();
                // debt: an escalated coroutine stays on the per-instruction
                // hook if a later callback resumes it; that callback runs slower,
                // and is still stopped at its own deadline. Reset the hook when a
                // callback arms the coroutine if a profile shows the slowdown.
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
