//! The Lua VM of one TUI extension (`docs/tui.md`, "How a TUI extension
//! runs").

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use contract::clock::Clock;
use mlua::serde::SerializeOptions;
use mlua::{
    Function, IntoLuaMulti, Lua, LuaOptions, LuaSerdeExt, MultiValue, StdLib, Table, Thread,
};
use serde_json::Value;

use crate::Error;
use crate::lua::{Deadline, LOAD_TIMEOUT, MEMORY_CAP, message};

/// The slot names that stand alone (`docs/tui.md`, "The slots").
pub(crate) const SLOT_NAMES: [&str; 20] = [
    "layout",
    "home",
    "turn_card",
    "prompt",
    "reply",
    "group_line",
    "ledger_row",
    "thinking_line",
    "steer",
    "answers",
    "handoff_band",
    "error_line",
    "notice",
    "working_line",
    "steering_queue",
    "input_box",
    "status_rows",
    "approval",
    "question_form",
    "repository_offer",
];

/// The slot names that take a suffix after the colon.
pub(crate) const SLOT_PREFIXES: [&str; 4] = ["ledger_row:", "card:", "view:", "overlay:"];

/// Whether `name` is a slot name (`docs/tui.md`, "The slots").
fn slot_name_ok(name: &str) -> bool {
    SLOT_NAMES.contains(&name)
        || SLOT_PREFIXES.iter().any(|prefix| {
            name.strip_prefix(prefix)
                .is_some_and(|rest| !rest.is_empty())
        })
}

/// One run of text in a line a renderer returns (`docs/tui.md`, "What a
/// renderer returns").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Span {
    /// The text.
    pub text: String,
    /// A theme colour role for the foreground.
    pub fg: Option<String>,
    /// A theme colour role for the background.
    pub bg: Option<String>,
    /// Bold.
    pub bold: bool,
    /// Dim.
    pub dim: bool,
    /// Italic.
    pub italic: bool,
    /// Underlined.
    pub underline: bool,
    /// The URL the span opens on click.
    pub link: Option<String>,
    /// The id of the span's click target.
    pub click: Option<String>,
}

/// The Lua half of the host's globals: `coroutine.create` and `coroutine.wrap`
/// over the armed thread constructor, so a coroutine a callback starts is cut
/// off at the callback's deadline as the callback is, and `fiber.slot`, which
/// fills the tables the prelude returns. Lua's own `wrap` is a separate C
/// function that would make an unarmed coroutine.
// A callback's timeout is in milliseconds (docs/extensions.md, "How an
// extension runs"), and an animated slot's `animate` holds its raw
// `{ interval_ms }` table: the adapter normalizes it to the tick floor
// at registration, while the VM stores the validated raw value.
const PRELUDE: &str = r#"
local create, loading, valid = ...
local slots, violations = {}, {}
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

-- A refusal is raised and also recorded, so a script that catches the raise
-- still fails to load.
local function refuse(message)
  violations[#violations + 1] = message
  error(message, 3)
end

fiber = {
  slot = function(name, spec)
    if not loading() then
      error("fiber.slot: registers only while the extension loads", 2)
    end
    local shown = tostring(name)
    if type(name) ~= "string" or not valid(name) then
      refuse("fiber.slot: `" .. shown .. "` is not a slot name")
    end
    if slots[name] then
      refuse("fiber.slot: `" .. shown .. "` is registered twice")
    end
    if type(spec) ~= "table" then
      refuse("fiber.slot: `" .. shown .. "` takes a table of `timeout` and `render`")
    end
    if spec.timeout == nil then
      refuse("fiber.slot: `" .. shown .. "`: missing `timeout`")
    end
    if math.type(spec.timeout) ~= "integer" or spec.timeout <= 0 then
      refuse("fiber.slot: `" .. shown .. "`: `timeout` must be a whole number of milliseconds above 0")
    end
    if type(spec.render) ~= "function" then
      refuse("fiber.slot: `" .. shown .. "`: `render` must be a function")
    end
    if spec.animate ~= nil then
      if type(spec.animate) ~= "table" then
        refuse("fiber.slot: `" .. shown .. "`: `animate` must be a table with `interval_ms`")
      end
      if math.type(spec.animate.interval_ms) ~= "integer" or spec.animate.interval_ms <= 0 then
        refuse("fiber.slot: `" .. shown .. "`: `animate.interval_ms` must be a whole number of milliseconds above 0")
      end
    end
    slots[name] = { timeout = spec.timeout, render = spec.render, animate = spec.animate }
  end,
}

return slots, violations
"#;

/// A registered slot: its declared timeout, its renderer, and the raw
/// validated animation interval when the slot animates.
struct Slot {
    timeout: Duration,
    render: Function,
    animate: Option<Duration>,
}

/// What an animated call returned: its lines, and whether the item keeps
/// animating. A plain list or `{ lines }` continues at the declared
/// interval; only `{ lines, animate = false }` stops, drawing once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderOut {
    /// The drawn lines.
    pub lines: Vec<Vec<Span>>,
    /// Whether the item's animation continues.
    pub continues: bool,
}

/// One extension's Lua VM, owned by the thread that calls it. It holds a
/// `Lua`, so it is not `Send`.
pub struct TuiVm {
    name: String,
    lua: Lua,
    deadline: Deadline,
    clock: Arc<dyn Clock>,
    slots: BTreeMap<String, Slot>,
    /// Set by a timeout: the VM answers nothing until the caller drops it
    /// (`docs/tui.md`, "When an extension fails").
    off: bool,
}

impl std::fmt::Debug for TuiVm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiVm")
            .field("name", &self.name)
            .field("slots", &self.slots.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl TuiVm {
    /// Runs `source`, the extension `name`'s entry script, and returns the VM
    /// holding the slots it registered. `fiber.slot` works only while the
    /// script runs.
    pub fn load(name: &str, source: &str, clock: Arc<dyn Clock>) -> Result<Self, Error> {
        Self::load_with_source(name, || Ok(source.to_owned()), clock)
    }

    /// Reads, compiles and initializes an entry script under one loading
    /// deadline. Uninterruptible reads and compilation are checked on return;
    /// timeout takes precedence over their errors.
    pub fn load_with_source(
        name: &str,
        read_source: impl FnOnce() -> Result<String, Error>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, Error> {
        let at = clock.now().checked_add(LOAD_TIMEOUT);
        let source = read_source();
        if at.is_some_and(|at| clock.now() >= at) {
            return Err(Error::TuiTimeout);
        }
        // A Lua error while loading is a failure to start, not a renderer's.
        Self::start(name, &source?, clock, at).map_err(|error| {
            if let Error::TuiLua(message) = error {
                Error::Lua {
                    extension: name.to_owned(),
                    message,
                }
            } else {
                error
            }
        })
    }

    fn start(
        name: &str,
        source: &str,
        clock: Arc<dyn Clock>,
        at: Option<std::time::Instant>,
    ) -> Result<Self, Error> {
        let lua = Lua::new_with(
            // table, string, math, utf8 and coroutine: Lua's safe set, which
            // leaves out debug, less its I/O and module loader. `require` is
            // absent with the loader: an extension is one script.
            StdLib::ALL_SAFE & !StdLib::IO & !StdLib::OS & !StdLib::PACKAGE,
            // A host function's panic is never a Lua error a `pcall` can
            // catch (`docs/code-quality.md`, "Panics").
            LuaOptions::new().catch_rust_panics(false),
        )
        .map_err(lua_error)?;
        let globals = lua.globals();
        for global in ["print", "warn", "dofile", "loadfile"] {
            globals.raw_remove(global).map_err(lua_error)?;
        }
        lua.set_memory_limit(MEMORY_CAP).map_err(lua_error)?;
        let deadline = Deadline::new(Arc::clone(&clock));
        let loading = Rc::new(Cell::new(true));
        let create = {
            let deadline = deadline.clone();
            lua.create_function(move |lua, f: Function| {
                let thread = lua.create_thread(f)?;
                deadline.arm(&thread)?;
                Ok(thread)
            })
        }
        .map_err(lua_error)?;
        let is_loading = {
            let loading = Rc::clone(&loading);
            lua.create_function(move |_, ()| Ok(loading.get()))
        }
        .map_err(lua_error)?;
        let valid = lua
            .create_function(|_, name: String| Ok(slot_name_ok(&name)))
            .map_err(lua_error)?;
        let (registered, violations): (Table, Table) = lua
            .load(PRELUDE)
            .set_name("=prelude")
            .call((create, is_loading, valid))
            .map_err(lua_error)?;

        let script = lua
            .load(source)
            .set_name(format!("={name}"))
            .into_function();
        if at.is_some_and(|at| clock.now() >= at) {
            return Err(Error::TuiTimeout);
        }
        let thread = lua
            .create_thread(script.map_err(lua_error)?)
            .map_err(lua_error)?;
        deadline.arm(&thread).map_err(lua_error)?;
        deadline.restore(at);
        let ran = resume(&deadline, &thread, (), "the script makes no host call");
        deadline.restore(None);
        loading.set(false);
        let refusal = violations.raw_get::<Option<String>>(1).map_err(lua_error)?;
        match (ran, refusal) {
            (Err(Error::TuiTimeout), _) => return Err(Error::TuiTimeout),
            (_, Some(message)) => return Err(Error::TuiRegistration(message)),
            (Err(error), None) => return Err(error),
            (Ok(_), None) => {}
        }

        let mut slots = BTreeMap::new();
        for entry in registered.pairs::<String, Table>() {
            let (slot, spec) = entry.map_err(lua_error)?;
            let timeout = Duration::from_millis(spec.raw_get("timeout").map_err(lua_error)?);
            let render = spec.raw_get("render").map_err(lua_error)?;
            let animate: Option<Table> = spec.raw_get("animate").map_err(lua_error)?;
            let animate = animate
                .map(|table| {
                    let interval_ms: mlua::Integer =
                        table.raw_get("interval_ms").map_err(lua_error)?;
                    u64::try_from(interval_ms)
                        .map(Duration::from_millis)
                        .map_err(|_| shape("`animate.interval_ms` must be a whole number above 0"))
                })
                .transpose()?;
            slots.insert(
                slot,
                Slot {
                    timeout,
                    render,
                    animate,
                },
            );
        }
        Ok(Self {
            name: name.to_owned(),
            lua,
            deadline,
            clock,
            slots,
            off: false,
        })
    }

    /// Each registered slot and its declared timeout, by name.
    pub fn slots(&self) -> BTreeMap<String, Duration> {
        self.slots
            .iter()
            .map(|(name, slot)| (name.clone(), slot.timeout))
            .collect()
    }

    /// The slot's raw validated animation interval, if it animates. Absent
    /// `animate` stores none; a present one stores its whole milliseconds
    /// above 0, unclamped: the adapter normalizes to the tick floor.
    pub fn animate(&self, slot: &str) -> Option<Duration> {
        self.slots.get(slot)?.animate
    }

    /// Calls the renderer of `slot` for `session_id` with `input`, an object,
    /// and returns its lines. The renderer receives `input` with
    /// `session_id` set to the argument, and the width. `builtin(input, width)`
    /// in Lua returns what `builtin` returns for the original `input`.
    ///
    /// A timeout switches the VM off: every later call is [`Error::TuiOff`].
    /// The caller decides the fallback.
    pub fn call(
        &mut self,
        slot: &str,
        session_id: Option<&str>,
        input: &Value,
        width: u16,
        builtin: &dyn Fn(&Value, u16) -> Vec<Vec<Span>>,
    ) -> Result<Vec<Vec<Span>>, Error> {
        self.invoke(slot, session_id, input, width, None, builtin)
            .and_then(|returned| render_out_from(&returned))
            .map(|out| out.lines)
    }

    /// Calls the renderer of `slot` for `session_id` with `input`, an object,
    /// and returns its lines with whether the item keeps animating. The
    /// renderer receives `input` with `session_id` set to the argument, the
    /// `frame` count, and the width. A plain list or `{ lines }` continues
    /// at the declared interval; only `{ lines, animate = false }` stops,
    /// drawing once. Any other shape is the existing wrong-shape failure.
    ///
    /// A timeout switches the VM off: every later call is [`Error::TuiOff`].
    /// The caller decides the fallback.
    pub fn call_animated(
        &mut self,
        slot: &str,
        session_id: Option<&str>,
        input: &Value,
        width: u16,
        frame: u64,
        builtin: &dyn Fn(&Value, u16) -> Vec<Vec<Span>>,
    ) -> Result<RenderOut, Error> {
        self.invoke(slot, session_id, input, width, Some(frame), builtin)
            .and_then(|returned| render_out_from(&returned))
    }

    /// Runs the renderer once under its declared timeout and returns what
    /// it returned. `frame` counts the frame into the input object when
    /// set; a plain call sends no `frame` key and Lua reads nil. Shares
    /// the deadline, the off latch and the `builtin` scope with every
    /// animated call.
    fn invoke(
        &mut self,
        slot: &str,
        session_id: Option<&str>,
        input: &Value,
        width: u16,
        frame: Option<u64>,
        builtin: &dyn Fn(&Value, u16) -> Vec<Vec<Span>>,
    ) -> Result<mlua::Value, Error> {
        if self.off {
            return Err(Error::TuiOff);
        }
        let Some(registered) = self.slots.get(slot) else {
            return Err(Error::TuiNoSlot(slot.to_owned()));
        };
        let Some(object) = input.as_object() else {
            return Err(Error::TuiShape("input must be an object".to_owned()));
        };
        let mut object = object.clone();
        // A sessionless call sends no `session_id` key and Lua reads
        // nil: no previous session's identity is reused and no empty or
        // synthetic id is sent.
        if let Some(session_id) = session_id {
            object.insert(
                "session_id".to_owned(),
                Value::String(session_id.to_owned()),
            );
        }
        if let Some(frame) = frame {
            object.insert("frame".to_owned(), Value::from(frame));
        }
        // JSON null is nil, so an absent field and a null one read alike.
        let options = SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false)
            .set_array_metatable(false);
        let arg = self
            .lua
            .to_value_with(&Value::Object(object), options)
            .map_err(lua_error)?;
        let render = registered.render.clone();
        // `None` when the timeout is too large to add to an `Instant`: such a
        // callback is never cut off.
        let at = self.clock.now().checked_add(registered.timeout);
        let globals = self.lua.globals();
        let result = self.lua.scope(|scope| {
            let host = scope.create_function(|lua, (_, wide): (mlua::Value, Option<u16>)| {
                lines_to_lua(lua, &builtin(input, wide.unwrap_or(width)))
            })?;
            globals.raw_set("builtin", host)?;
            let thread = self.lua.create_thread(render)?;
            self.deadline.arm(&thread)?;
            self.deadline.restore(at);
            let returned = resume(
                &self.deadline,
                &thread,
                (arg, width),
                "a renderer makes no host call",
            );
            // The conversion ran no Lua, but a finish is late when the clock
            // says so, whichever path it took.
            Ok(if self.deadline.passed() {
                Err(Error::TuiTimeout)
            } else {
                returned
            })
        });
        self.deadline.restore(None);
        // The scoped function is invalid now; the global must not keep it.
        globals
            .raw_set("builtin", mlua::Value::Nil)
            .map_err(lua_error)?;
        let result = result.map_err(lua_error)?;
        if matches!(result, Err(Error::TuiTimeout)) {
            self.off = true;
        }
        result
    }
}

/// Runs `thread` once and returns its first value. Past `deadline` the result
/// is a timeout, whatever came back, a finish after it included.
// debt: a C call Lua cannot interrupt (a `__gc` finalizer, a backtracking
// `string.find`) freezes the terminal, since no second thread can abandon
// the VM; revisit when Part 3 measures a freeze, or when a TUI extension
// may run untrusted Lua.
fn resume(
    deadline: &Deadline,
    thread: &Thread,
    args: impl IntoLuaMulti,
    yielded: &str,
) -> Result<mlua::Value, Error> {
    let returned = thread.resume::<MultiValue>(args);
    if deadline.passed() {
        return Err(Error::TuiTimeout);
    }
    let values = returned.map_err(lua_error)?;
    if thread.is_resumable() {
        return Err(Error::TuiLua(yielded.to_owned()));
    }
    Ok(values.into_iter().next().unwrap_or(mlua::Value::Nil))
}

fn lua_error(e: mlua::Error) -> Error {
    Error::TuiLua(message(&e))
}

fn shape(what: impl Into<String>) -> Error {
    Error::TuiShape(what.into())
}

/// The lines a renderer returned. It reads with raw gets and raw lengths, so
/// no metamethod runs and the conversion cannot hang or call out.
fn lines_from(returned: &mlua::Value) -> Result<Vec<Vec<Span>>, Error> {
    let mlua::Value::Table(lines) = returned else {
        return Err(shape("a renderer returns a list of lines"));
    };
    let mut out = Vec::new();
    for number in 1..=lines.raw_len() {
        let mlua::Value::Table(line) = lines.raw_get(number).map_err(lua_error)? else {
            return Err(shape(format!("line {number} is not a list of spans")));
        };
        let mut spans = Vec::new();
        for index in 1..=line.raw_len() {
            let mlua::Value::Table(span) = line.raw_get(index).map_err(lua_error)? else {
                return Err(shape(format!(
                    "span {index} of line {number} is not a table"
                )));
            };
            spans.push(span_from(&span, number, index)?);
        }
        out.push(spans);
    }
    Ok(out)
}

/// An animated return's lines and whether the item keeps animating. A
/// table with neither a `lines` nor an `animate` key is a plain list and
/// continues. A table with either key is keyed: it must hold `lines` as a
/// list, `animate` only as `false` or absent, and no other key. Only
/// `{ lines, animate = false }` stops, drawing once; every other keyed
/// shape is the existing wrong-shape failure. Reads with raw gets and raw
/// iteration, so no metamethod runs.
fn render_out_from(returned: &mlua::Value) -> Result<RenderOut, Error> {
    let mlua::Value::Table(table) = returned else {
        return Err(shape("a renderer returns a list of lines"));
    };
    let lines: mlua::Value = table.raw_get("lines").map_err(lua_error)?;
    let animate: mlua::Value = table.raw_get("animate").map_err(lua_error)?;
    if lines.is_nil() && animate.is_nil() {
        return Ok(RenderOut {
            lines: lines_from(returned)?,
            continues: true,
        });
    }
    if !lines.is_table() {
        return Err(shape("a keyed return holds `lines` as a list of lines"));
    }
    // `pairs` walks with `lua_next`, so no metamethod runs.
    for pair in table.pairs::<mlua::Value, mlua::Value>() {
        let (key, _) = pair.map_err(lua_error)?;
        if !is_key(&key, "lines") && !is_key(&key, "animate") {
            return Err(shape("a keyed return holds only `lines` and `animate`"));
        }
    }
    // Only `false` stops; any other set value, `true` included, is a
    // wrong shape.
    let continues = if animate.is_nil() {
        true
    } else if matches!(animate, mlua::Value::Boolean(false)) {
        false
    } else {
        return Err(shape(
            "`animate` is `false` to stop, or omitted to continue",
        ));
    };
    Ok(RenderOut {
        lines: lines_from(&lines)?,
        continues,
    })
}

/// Whether `key` names the string `name`.
fn is_key(key: &mlua::Value, name: &str) -> bool {
    matches!(key, mlua::Value::String(found) if found.to_str().is_ok_and(|found| found == name))
}

fn span_from(span: &Table, line: usize, index: usize) -> Result<Span, Error> {
    let at = format!("span {index} of line {line}");
    let text =
        text_field(span, "text", &at)?.ok_or_else(|| shape(format!("{at} has no `text`")))?;
    Ok(Span {
        text,
        fg: text_field(span, "fg", &at)?,
        bg: text_field(span, "bg", &at)?,
        bold: flag(span, "bold", &at)?,
        dim: flag(span, "dim", &at)?,
        italic: flag(span, "italic", &at)?,
        underline: flag(span, "underline", &at)?,
        link: text_field(span, "link", &at)?,
        click: text_field(span, "click", &at)?,
    })
}

fn text_field(span: &Table, key: &str, at: &str) -> Result<Option<String>, Error> {
    let value: mlua::Value = span.raw_get(key).map_err(lua_error)?;
    if value.is_nil() {
        return Ok(None);
    }
    let text = value
        .as_string()
        .ok_or_else(|| shape(format!("`{key}` of {at} must be a string")))?;
    text.to_str()
        .map(|text| Some(text.to_string()))
        .map_err(|_| shape(format!("`{key}` of {at} is not valid UTF-8")))
}

fn flag(span: &Table, key: &str, at: &str) -> Result<bool, Error> {
    let value: mlua::Value = span.raw_get(key).map_err(lua_error)?;
    if value.is_nil() {
        return Ok(false);
    }
    value
        .as_boolean()
        .ok_or_else(|| shape(format!("`{key}` of {at} must be a boolean")))
}

/// `lines` as the table a renderer returns, for `builtin` to hand to Lua.
fn lines_to_lua(lua: &Lua, lines: &[Vec<Span>]) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    for (number, line) in lines.iter().enumerate() {
        let spans = lua.create_table()?;
        for (index, span) in line.iter().enumerate() {
            let table = lua.create_table()?;
            table.raw_set("text", span.text.as_str())?;
            for (key, value) in [
                ("fg", &span.fg),
                ("bg", &span.bg),
                ("link", &span.link),
                ("click", &span.click),
            ] {
                if let Some(value) = value {
                    table.raw_set(key, value.as_str())?;
                }
            }
            for (key, on) in [
                ("bold", span.bold),
                ("dim", span.dim),
                ("italic", span.italic),
                ("underline", span.underline),
            ] {
                if on {
                    table.raw_set(key, true)?;
                }
            }
            spans.raw_set(index + 1, table)?;
        }
        out.raw_set(number + 1, spans)?;
    }
    Ok(out)
}
