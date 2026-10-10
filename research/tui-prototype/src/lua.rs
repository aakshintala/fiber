//! One extension seam: a Lua renderer for the ledger row of one tool (#163).
//!
//! The script returns `{ tool = "shell", render = function(call, width) ... end }`.
//! `render` gets the call's data, all from the event stream, and the width it
//! may draw in, and returns lines: each line a list of spans
//! `{ text =, fg =, bg =, bold =, dim =, click = }`. Rust builds every cell,
//! and the plain text behind it, from the spans, so selection, copy and search
//! work on what Lua drew. A span with a control character (so any escape code)
//! is the wrong shape. A row whose render errors or has the wrong shape is drawn
//! by the built-in renderer, and the first failure becomes one notice.
use crate::{Act, Call, Row, St, dim, fit, hot_row, sp, tint};
use mlua::{Function, Lua, LuaOptions, LuaSerdeExt, StdLib, Table, Value as LV};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::time::Instant;

/// One rendered row per line: spans with their click targets.
type LuaLines = Vec<Vec<(Span<'static>, Option<Act>)>>;

/// What a Lua row was drawn from, so an uncached frame can call Lua again.
pub struct LuaSrc {
    pub input: Value,
    /// the ledger's width; Lua gets it less the gutter
    pub w: usize,
    pub gutter: String,
    pub line: usize,
}

pub struct Ext {
    lua: Lua,
    pub tool: String,
    render: Function,
    /// true: rows are cached by the call's content and the width
    pub cached: bool,
    cache: RefCell<HashMap<u64, Option<LuaLines>>>,
    /// Lua calls, their total time including building the input and reading the
    /// output, and the time inside `render` alone, in nanoseconds
    pub calls: Cell<u64>,
    pub ns: Cell<u128>,
    pub ns_in: Cell<u128>,
    failure: RefCell<Option<String>>,
    told: Cell<bool>,
    /// click ids, interned so `Act` stays `Copy`
    pub clicks: RefCell<Vec<String>>,
}

pub const GUTTER: usize = 6;

impl Ext {
    pub fn new(name: &str, src: &str, cached: bool) -> mlua::Result<Ext> {
        // the stripped standard library of docs/extensions.md
        let lua = Lua::new_with(StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8 | StdLib::COROUTINE, LuaOptions::default())?;
        let t: Table = lua.load(src).set_name(format!("@{name}")).eval()?;
        let tool: String = t.get("tool")?;
        let render: Function = t.get("render")?;
        Ok(Ext { lua, tool, render, cached, cache: Default::default(), calls: Cell::new(0), ns: Cell::new(0), ns_in: Cell::new(0), failure: Default::default(), told: Cell::new(false), clicks: Default::default() })
    }

    pub fn used_memory(&self) -> usize {
        self.lua.used_memory()
    }

    /// The first failure, once.
    pub fn take_notice(&self) -> Option<String> {
        if self.told.get() {
            return None;
        }
        let f = self.failure.borrow().clone()?;
        self.told.set(true);
        Some(format!("{} renderer failed, built-in rows shown: {f}", self.tool))
    }

    /// The ledger rows for a call, or None to use the built-in row.
    pub fn rows(&self, c: &Call, gutter: &str, w: usize) -> Option<Vec<Row>> {
        let input = input(c);
        let lines = if self.cached {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (input.to_string(), w).hash(&mut h);
            let key = h.finish();
            // debt: never evicted; a paged TUI would drop rows that leave its window
            if let Some(hit) = self.cache.borrow().get(&key) {
                hit.clone()
            } else {
                let l = self.call(&input, w);
                self.cache.borrow_mut().insert(key, l.clone());
                l
            }
        } else {
            self.call(&input, w)
        }?;
        Some(
            lines
                .into_iter()
                .enumerate()
                .map(|(i, l)| {
                    let g = if i == 0 { gutter.to_string() } else { " ".repeat(GUTTER) };
                    let mut parts = vec![(sp(g, dim()), None)];
                    parts.extend(l);
                    let mut r = hot_row(parts);
                    r.lua = Some(Rc::new(LuaSrc { input: input.clone(), w, gutter: gutter.to_string(), line: i }));
                    r
                })
                .collect(),
        )
    }

    /// An uncached frame: calls Lua again for one visible row and returns its
    /// spans as the conversation lays them out, `cw` wide, on the card `bg`.
    pub fn frame_spans(&self, s: &LuaSrc, cw: usize, bg: Color) -> Option<Vec<Span<'static>>> {
        let lines = self.call(&s.input, s.w)?;
        let l = lines.into_iter().nth(s.line)?;
        let g = if s.line == 0 { s.gutter.clone() } else { " ".repeat(GUTTER) };
        let mut body = vec![sp(g, dim())];
        body.extend(l.into_iter().map(|p| p.0));
        let mut spans = vec![sp(" ", Style::new())];
        spans.extend(fit(&body, s.w));
        spans.push(sp(" ", Style::new()));
        Some(tint(fit(&spans, cw), bg))
    }

    fn call(&self, input: &Value, w: usize) -> Option<LuaLines> {
        let t0 = Instant::now();
        let res = (|| -> Result<_, String> {
            let arg = self.lua.to_value(input).map_err(|e| e.to_string())?;
            let t1 = Instant::now();
            let out: LV = self.render.call((arg, w.saturating_sub(GUTTER))).map_err(|e| e.to_string())?;
            self.ns_in.set(self.ns_in.get() + t1.elapsed().as_nanos());
            self.lines(out)
        })();
        self.calls.set(self.calls.get() + 1);
        self.ns.set(self.ns.get() + t0.elapsed().as_nanos());
        match res {
            Ok(l) => Some(l),
            Err(e) => {
                let mut f = self.failure.borrow_mut();
                if f.is_none() {
                    *f = Some(e.lines().next().unwrap_or("").to_string());
                }
                None
            }
        }
    }

    /// Reads `render`'s return value; anything but the documented shape is an error.
    fn lines(&self, out: LV) -> Result<LuaLines, String> {
        let LV::Table(t) = out else { return Err(format!("render returned {}, not a list of lines", out.type_name())) };
        let mut lines = vec![];
        for l in t.sequence_values::<LV>() {
            let LV::Table(l) = l.map_err(|e| e.to_string())? else { return Err("a line is not a list of spans".into()) };
            let mut spans = vec![];
            for s in l.sequence_values::<LV>() {
                let LV::Table(s) = s.map_err(|e| e.to_string())? else { return Err("a span is not a table".into()) };
                spans.push(self.span(&s)?);
            }
            lines.push(spans);
        }
        if lines.is_empty() {
            return Err("render returned no lines".into());
        }
        Ok(lines)
    }

    fn span(&self, s: &Table) -> Result<(Span<'static>, Option<Act>), String> {
        let text = match s.get::<LV>("text").map_err(|e| e.to_string())? {
            LV::String(t) => t.to_str().map_err(|e| e.to_string())?.to_string(),
            v => return Err(format!("span text is {}, not a string", v.type_name())),
        };
        // no escape code, nor any other control character, reaches the terminal
        if text.chars().any(char::is_control) {
            return Err("span text holds a control character".into());
        }
        let mut st = Style::new();
        for (k, bgnd) in [("fg", false), ("bg", true)] {
            match s.get::<LV>(k).map_err(|e| e.to_string())? {
                LV::Nil => {}
                LV::String(c) => {
                    let c = colour(&c.to_string_lossy()).ok_or_else(|| format!("{k} is not a colour"))?;
                    st = if bgnd { st.bg(c) } else { st.fg(c) };
                }
                _ => return Err(format!("{k} is not a colour")),
            }
        }
        for (k, m) in [("bold", Modifier::BOLD), ("dim", Modifier::DIM)] {
            match s.get::<LV>(k).map_err(|e| e.to_string())? {
                LV::Nil | LV::Boolean(false) => {}
                LV::Boolean(true) => st = st.add_modifier(m),
                _ => return Err(format!("{k} is not a boolean")),
            }
        }
        let act = match s.get::<LV>("click").map_err(|e| e.to_string())? {
            LV::Nil => None,
            LV::String(id) => {
                let id = id.to_string_lossy();
                let mut c = self.clicks.borrow_mut();
                let i = c.iter().position(|x| *x == id).unwrap_or_else(|| {
                    c.push(id);
                    c.len() - 1
                });
                Some(Act::Ext(i))
            }
            _ => return Err("click is not a string".into()),
        };
        Ok((Span::styled(text, st), act))
    }
}

/// "#rrggbb", or a name from the prototype's palette.
fn colour(s: &str) -> Option<Color> {
    if let Some(h) = s.strip_prefix('#').filter(|h| h.len() == 6) {
        return u32::from_str_radix(h, 16).ok().map(crate::rgb);
    }
    Some(match s {
        "red" => crate::RED,
        "blue" => crate::BLUE,
        "orange" => crate::ORANGE,
        "purple" => crate::PURPLE,
        "cyan" => crate::CYAN,
        _ => return None,
    })
}

/// The call as the renderer sees it: only what the event stream carries.
fn input(c: &Call) -> Value {
    let status = match c.st {
        St::Pending => "pending",
        St::Running => "running",
        St::Completed => "completed",
        St::Failed => "failed",
        St::Denied => "denied",
        St::Cancelled => "cancelled",
    };
    json!({
        "name": c.name,
        "arguments": c.args,
        "status": status,
        "exit_code": c.exit,
        "duration_ms": c.end.map(|e| e - c.start),
        "lines": c.lines,
        "changes": c.changes.iter().map(|(p, a, r)| json!({ "path": p, "added": a, "removed": r })).collect::<Vec<_>>(),
        "error": c.err.as_ref().map(|(code, m)| json!({ "code": code, "message": m })),
    })
}
