//! Stage 1 of the #15 TUI prototype: replays a fixture session of
//! `docs/events.md` lines in a real terminal. Throwaway code: one file, the
//! fold and the drawing side by side, no tests.

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Terminal;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufWriter, Write};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

// ============================================================ theme (the mock's "atelier" palette)
const fn rgb(v: u32) -> Color {
    Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}
const RED: Color = rgb(0xff5d73);
const BLUE: Color = rgb(0x6eaafe);
const ORANGE: Color = rgb(0xff9f43);
const PURPLE: Color = rgb(0xb39ddb);
const CYAN: Color = rgb(0x7dd3fc);
const SEL: Color = rgb(0x3a3a4a);
const BP: Color = rgb(0x0c0c11); // panel
const BC: Color = rgb(0x1a1a22); // card
const BU: Color = rgb(0x343541); // the person's bubble
const BI: Color = rgb(0x1a1a22); // input box
const BW: Color = rgb(0x101017); // turn card
const BK: Color = rgb(0x181821); // code block
const HD: Color = rgb(0xff9f43);
const SX_KW: Color = rgb(0x6eaafe);
const SX_FN: Color = rgb(0x7dd3fc);
const SX_STR: Color = rgb(0xce9178);
const SX_NUM: Color = rgb(0xb5cea8);
const SX_COM: Color = rgb(0x7a7a8a);

const PANEL: u16 = 34;
const CONV_MIN: u16 = 84;
const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const GLIMMER_MS: u64 = 120;

fn fg(c: Color) -> Style {
    Style::new().fg(c)
}
fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}
fn dfg(c: Color) -> Style {
    fg(c).add_modifier(Modifier::DIM)
}
fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}
fn sp(t: impl Into<String>, s: Style) -> Span<'static> {
    Span::styled(t.into(), s)
}
fn width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

// ============================================================ rows
#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    Group(usize, usize),
    Notice(usize),
    End,
}

#[derive(Clone, Default)]
struct Row {
    spans: Vec<Span<'static>>,
    bg: Option<Color>,
    act: Option<Act>,
}
fn row(spans: Vec<Span<'static>>) -> Row {
    Row { spans, ..Default::default() }
}

/// Cuts or pads spans to exactly `w` cells; a lone "\t" span right-aligns what follows.
fn fit(spans: &[Span<'static>], w: usize) -> Vec<Span<'static>> {
    let mut spans = spans.to_vec();
    if let Some(i) = spans.iter().position(|s| s.content == "\t") {
        let right = width(&spans[i + 1..]);
        let left = width(&spans[..i]);
        if left + right + 1 > w {
            // the left side gives way to what is right-aligned
            let mut l = fit(&spans[..i], w.saturating_sub(right + 1));
            l.push(Span::raw(" "));
            l.extend(spans[i + 1..].iter().cloned());
            return fit(&l, w);
        }
        spans[i] = Span::raw(" ".repeat(w - left - right));
    }
    let mut out = vec![];
    let mut n = 0;
    for s in spans {
        let sw = s.content.width();
        if n + sw <= w {
            n += sw;
            out.push(s);
        } else {
            let mut t = String::new();
            for ch in s.content.chars() {
                let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if n + cw + 1 > w {
                    break;
                }
                t.push(ch);
                n += cw;
            }
            t.push('…');
            n += 1;
            out.push(Span::styled(t, s.style));
            break;
        }
    }
    if n < w {
        out.push(Span::raw(" ".repeat(w - n)));
    }
    out
}
/// Gives spans with no background of their own the row's background.
fn tint(spans: Vec<Span<'static>>, bg: Color) -> Vec<Span<'static>> {
    spans.into_iter().map(|s| if s.style.bg.is_none() { Span::styled(s.content, s.style.bg(bg)) } else { s }).collect()
}
fn edge(ch: &str, inner: Color, outer: Option<Color>, w: usize) -> Row {
    let mut s = fg(inner);
    if let Some(o) = outer {
        s = s.bg(o);
    }
    Row { spans: vec![sp(ch.repeat(w), s)], bg: outer, act: None }
}
/// A surface: a tinted block with half-block edges and no border.
fn slab(rows: Vec<Row>, bg: Color, outer: Option<Color>, w: usize) -> Vec<Row> {
    let mut out = vec![edge("▄", bg, outer, w)];
    for r in rows {
        let inner_bg = r.bg.unwrap_or(bg);
        out.push(Row { spans: tint(tint(fit(&r.spans, w), inner_bg), bg), bg: Some(bg), act: r.act });
    }
    out.push(edge("▀", bg, outer, w));
    out
}

/// Greedy word wrap over styled spans. `first` and `rest` prefix each line.
fn wrap(spans: Vec<Span<'static>>, w: usize, first: Vec<Span<'static>>, rest: Vec<Span<'static>>) -> Vec<Vec<Span<'static>>> {
    let mut toks: Vec<Span<'static>> = vec![];
    for s in spans {
        let mut cur = String::new();
        for ch in s.content.chars() {
            if ch == ' ' && !cur.is_empty() && !cur.ends_with(' ') {
                toks.push(Span::styled(std::mem::take(&mut cur), s.style));
            }
            cur.push(ch);
        }
        if !cur.is_empty() {
            toks.push(Span::styled(cur, s.style));
        }
    }
    let mut out = vec![];
    let mut line = first.clone();
    let mut pre = width(&first);
    let mut n = pre;
    let mut any = false;
    for t in toks {
        let tw = t.content.width();
        if any && n + tw > w {
            out.push(std::mem::replace(&mut line, rest.clone()));
            pre = width(&rest);
            n = pre;
            let tt = t.content.trim_start().to_string();
            n += tt.width();
            line.push(Span::styled(tt, t.style));
        } else {
            n += tw;
            line.push(t);
        }
        any = true;
    }
    let _ = pre;
    out.push(line);
    out
}
/// Inline markdown: `code` in cyan, **bold** bold.
fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut out = vec![];
    let mut rest = text;
    while !rest.is_empty() {
        let tick = rest.find('`');
        let star = rest.find("**");
        let (i, code) = match (tick, star) {
            (Some(a), Some(b)) if a < b => (a, true),
            (Some(a), None) => (a, true),
            (_, Some(b)) => (b, false),
            (None, None) => {
                out.push(sp(rest, base));
                break;
            }
        };
        let (open, close) = if code { ("`", "`") } else { ("**", "**") };
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(close) else {
            out.push(sp(rest, base));
            break;
        };
        if i > 0 {
            out.push(sp(&rest[..i], base));
        }
        let inner = &after[..j];
        out.push(sp(inner, if code { base.patch(fg(CYAN)) } else { base.add_modifier(Modifier::BOLD) }));
        rest = &after[j + close.len()..];
    }
    out
}
fn para(text: &str, w: usize, base: Style) -> Vec<Row> {
    text.split('\n').flat_map(|p| wrap(inline(p, base), w, vec![], vec![])).map(row).collect()
}

// ============================================================ markdown in replies
fn highlight(line: &str) -> Vec<Span<'static>> {
    const KW: &[&str] = &["pub", "fn", "let", "mut", "while", "if", "return", "use", "impl", "for", "in", "match", "struct", "const", "as", "Ok", "Err", "Some", "None"];
    let mut out = vec![];
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        let start = i;
        if line[i..].starts_with("//") {
            out.push(sp(&line[i..], fg(SX_COM).add_modifier(Modifier::ITALIC)));
            break;
        } else if c == '"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push(sp(&line[start..i], fg(SX_STR)));
        } else if c.is_ascii_digit() {
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                i += 1;
            }
            out.push(sp(&line[start..i], fg(SX_NUM)));
        } else if c.is_ascii_alphabetic() || c == '_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let w = &line[start..i];
            let s = if KW.contains(&w) {
                fg(SX_KW)
            } else if w.starts_with(|c: char| c.is_ascii_uppercase()) {
                fg(CYAN)
            } else if line[i..].starts_with('(') || line[i..].starts_with("!(") {
                fg(SX_FN)
            } else {
                Style::new()
            };
            out.push(sp(w, s));
        } else {
            let ch = line[i..].chars().next().unwrap();
            i += ch.len_utf8();
            out.push(sp(&line[start..i], Style::new()));
        }
    }
    out
}
fn code_block(code: &[&str], lang: &str, w: usize) -> Vec<Row> {
    let mut rows = vec![row(vec![sp(" ", Style::new()), sp(lang, dim()), sp("\t", Style::new()), sp("click to copy", dim()), sp(" ", Style::new())])];
    for (n, c) in code.iter().enumerate() {
        let mut s = vec![sp(format!("{:>4}  ", n + 1), fg(SEL))];
        s.extend(highlight(c));
        rows.push(row(s));
    }
    // the card's colour is the outer colour of the code block's edges
    let mut out = slab(rows, BK, Some(BW), w);
    out.first_mut().unwrap().bg = None;
    out.last_mut().unwrap().bg = None;
    out
}
fn table(lines: &[&str]) -> Vec<Row> {
    let cells: Vec<Vec<String>> = lines
        .iter()
        .filter(|l| !l.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')))
        .map(|l| l.trim().trim_matches('|').split('|').map(|c| c.trim().to_string()).collect())
        .collect();
    let cols = cells[0].len();
    let wd: Vec<usize> = (0..cols).map(|k| cells.iter().map(|r| r.get(k).map_or(0, |c| c.width())).max().unwrap_or(0)).collect();
    let num: Vec<bool> = (0..cols)
        .map(|k| cells.len() > 1 && cells[1..].iter().all(|r| r.get(k).is_some_and(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit() || ",.%+−-".contains(ch)))))
        .collect();
    let cell = |t: &str, k: usize| if num[k] { format!("{t:>w$}", w = wd[k]) } else { format!("{t:<w$}", w = wd[k]) };
    let mut out = vec![];
    for (i, r) in cells.iter().enumerate() {
        let st = if i == 0 { bold() } else { Style::new() };
        out.push(row(r.iter().enumerate().flat_map(|(k, c)| [sp(cell(c, k), st), sp("   ", Style::new())]).collect()));
        if i == 0 {
            out.push(row(wd.iter().flat_map(|n| [sp("─".repeat(*n), fg(SEL)), sp("   ", Style::new())]).collect()));
        }
    }
    out
}
fn md(text: &str, w: usize) -> Vec<Row> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = vec![];
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if let Some(lang) = l.strip_prefix("```") {
            let mut code = vec![];
            i += 1;
            while i < lines.len() && !lines[i].starts_with("```") {
                code.push(lines[i]);
                i += 1;
            }
            out.extend(code_block(&code, lang.trim(), w));
        } else if l.starts_with('|') {
            let s = i;
            while i < lines.len() && lines[i].starts_with('|') {
                i += 1;
            }
            out.extend(table(&lines[s..i]));
            continue;
        } else if l.starts_with('#') {
            out.push(row(vec![sp(l.trim_start_matches('#').trim(), fg(HD).add_modifier(Modifier::BOLD))]));
        } else if let Some(b) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
            out.extend(wrap(inline(b, Style::new()), w, vec![sp("• ", fg(BLUE))], vec![sp("  ", Style::new())]).into_iter().map(row));
        } else if l.trim().is_empty() {
            out.push(Row::default());
        } else {
            out.extend(para(l, w, Style::new()));
        }
        i += 1;
    }
    out
}

// ============================================================ the fold: events -> what the terminal holds
#[derive(Clone, Copy, PartialEq)]
enum St {
    Pending,
    Running,
    Completed,
    Failed,
    Denied,
    Cancelled,
}
struct Call {
    name: String,
    args: Value,
    st: St,
    start: i64,
    end: Option<i64>,
    err: Option<(String, String)>,
    lines: usize,
    exit: Option<i64>,
    changes: Vec<(String, i64, i64)>,
    content: String,
    step: usize,
}
struct Reason {
    text: String,
    start: i64,
    end: Option<i64>,
    step: usize,
}
enum Item {
    R(Reason),
    C(Call),
}
struct Group {
    items: Vec<Item>,
    steps: usize,
    step_closed: bool,
    open: bool,
    last_ts: i64,
}
enum Block {
    Text(String),
    Group(Group),
    Steer(String, i64),
    Mcp(String),
    Done(String),
}
struct Turn {
    prompt: String,
    ts: i64,
    blocks: Vec<Block>,
}
struct Job {
    id: String,
    desc: String,
    sid: Option<String>,
    running: bool,
    start: i64,
    end: i64,
    calls: usize,
    last: String,
}
#[derive(Default)]
struct Fold {
    session_id: String,
    turns: Vec<Turn>,
    at: HashMap<String, (usize, usize, usize)>,
    cwd: String,
    mode: String,
    model: String,
    effort: String,
    thinking: String,
    branch: String,
    dirty: bool,
    tools: usize,
    mcp_down: Vec<String>,
    ctx: u64,
    input: u64,
    cache_read: u64,
    cache_write: u64,
    output: u64,
    cost: f64,
    text_ms: i64,
    text_out: u64,
    text_start: HashMap<String, i64>,
    text_dur: HashMap<String, i64>,
    files: BTreeMap<String, (i64, i64)>,
    jobs: Vec<Job>,
    queue: Vec<String>,
    notices: Vec<(String, String, bool)>,
    running: bool,
    turn_start: i64,
    last_ts: i64,
}

fn heading(text: &str, last: bool) -> Option<String> {
    let mut hs = text.split("**").skip(1).step_by(2).filter(|h| !h.contains('\n'));
    if last { hs.last() } else { hs.next() }.map(str::to_string)
}
fn kind_of(c: &Call) -> &'static str {
    match c.name.as_str() {
        "shell" => match c.args["command"].as_str().unwrap_or("").split_whitespace().next() {
            Some("grep" | "rg") => "search",
            Some("find" | "ls" | "fd") => "list",
            _ => "shell",
        },
        "read" => "read",
        "edit" => "edit",
        "write" => "write",
        "delegate_spawn" | "delegate_fork" => "delegate",
        "jobs" => "jobs",
        "ask_user" => "ask",
        _ => "other",
    }
}
fn target(c: &Call) -> String {
    ["path", "command", "description"].iter().find_map(|k| c.args[k].as_str()).unwrap_or("").to_string()
}
fn dur(ms: i64) -> String {
    let s = (ms.max(0) + 500) / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{}h {:02}m", s / 3600, s / 60 % 60)
    }
}
fn clock(ts: i64) -> String {
    let s = ts / 1000 % 86400;
    format!("{:02}:{:02}", s / 3600, s / 60 % 60)
}

impl Fold {
    fn turn(&mut self) -> Option<&mut Turn> {
        self.turns.last_mut()
    }
    /// The open tool group of the current turn, started if the last block is not one.
    fn group(&mut self, ts: i64) -> (usize, usize) {
        let ti = self.turns.len() - 1;
        let t = &mut self.turns[ti];
        // an MCP line lands where it happened but does not end the group, as in the mock
        if let Some(bi) = t.blocks.iter().rposition(|b| !matches!(b, Block::Mcp(_))) {
            if matches!(t.blocks[bi], Block::Group(_)) {
                return (ti, bi);
            }
        }
        t.blocks.push(Block::Group(Group { items: vec![], steps: 0, step_closed: true, open: false, last_ts: ts }));
        (ti, t.blocks.len() - 1)
    }
    fn group_at(turns: &mut [Turn], ti: usize, bi: usize) -> &mut Group {
        match &mut turns[ti].blocks[bi] {
            Block::Group(g) => g,
            _ => unreachable!(),
        }
    }
    fn apply(&mut self, e: &Value) {
        let kind = e["kind"].as_str().unwrap_or("");
        let sid = e["session_id"].as_str().unwrap_or("");
        let ts = e["ts"].as_i64().unwrap_or(0);
        let p = &e["payload"];
        let aid = e["action_id"].as_str().unwrap_or("").to_string();
        if self.session_id.is_empty() {
            self.session_id = sid.to_string();
        }
        if sid != self.session_id {
            // a delegate's own lines, relayed: the Delegates card counts its calls
            if let Some(j) = self.jobs.iter_mut().find(|j| j.sid.as_deref() == Some(sid)) {
                if kind == "tool_call_completed" {
                    j.calls += 1;
                }
                if kind == "tool_call_requested" {
                    let t = ["path", "command"].iter().find_map(|k| p["arguments"][k].as_str()).unwrap_or("");
                    j.last = format!("{} {}", p["name"].as_str().unwrap_or(""), t.rsplit('/').next().unwrap_or(""));
                }
            }
            return;
        }
        self.last_ts = ts;
        match kind {
            "session_started" => self.cwd = p["workspace"].as_str().unwrap_or("").into(),
            "opening_message" => {
                let g = &p["environment"]["git"];
                self.branch = g["branch"].as_str().unwrap_or("").into();
                self.dirty = g["dirty"].as_bool().unwrap_or(false);
            }
            "preamble_built" | "model_changed" => {
                let src = if kind == "model_changed" { &p["after"] } else { p };
                self.model = src["model"].as_str().unwrap_or("").rsplit('/').next().unwrap_or("").into();
                self.effort = src["effort"].as_str().unwrap_or("").into();
                self.thinking = src["thinking"].as_str().unwrap_or("").into();
                if let Some(t) = p["tools"].as_array() {
                    self.tools = t.len();
                }
            }
            "mode_changed" => self.mode = p["after"].as_str().unwrap_or("").into(),
            "turn_started" => {
                let text = p["input"].as_array().and_then(|a| a.iter().find_map(|i| i["text"].as_str())).unwrap_or("");
                self.turns.push(Turn { prompt: text.into(), ts, blocks: vec![] });
                self.running = true;
                self.turn_start = ts;
            }
            "turn_completed" => {
                let outcome = p["outcome"].as_str().unwrap_or("completed");
                let Some(t) = self.turns.last_mut() else { return };
                let n: usize = t.blocks.iter().map(|b| if let Block::Group(g) = b { g.items.iter().filter(|i| matches!(i, Item::C(_))).count() } else { 0 }).sum();
                let s = format!("{outcome} · {} · {n} tool calls", dur(ts - t.ts));
                t.blocks.push(Block::Done(s));
                self.running = false;
            }
            "reasoning_started" => {
                let (ti, bi) = self.group(ts);
                let g = Self::group_at(&mut self.turns, ti, bi);
                if g.step_closed {
                    g.steps += 1;
                    g.step_closed = false;
                }
                let step = g.steps;
                g.items.push(Item::R(Reason { text: String::new(), start: ts, end: None, step }));
                g.last_ts = ts;
                self.at.insert(aid, (ti, bi, g.items.len() - 1));
            }
            "reasoning_delta" | "reasoning_completed" => {
                if let Some(&(ti, bi, ii)) = self.at.get(&aid) {
                    let g = Self::group_at(&mut self.turns, ti, bi);
                    g.last_ts = ts;
                    if let Item::R(r) = &mut g.items[ii] {
                        if kind == "reasoning_delta" {
                            r.text.push_str(p["text"].as_str().unwrap_or(""));
                        } else {
                            r.text = p["text"].as_str().unwrap_or("").into();
                            r.end = Some(ts);
                        }
                    }
                }
            }
            "assistant_message_started" => {
                // every model call opens with this line, so it is the step boundary;
                // whether it is a piece of text that ends the group is known only once text arrives
                self.text_start.insert(aid, ts);
                if let Some(Turn { blocks, .. }) = self.turns.last_mut() {
                    if let Some(Block::Group(g)) = blocks.iter_mut().rev().find(|b| !matches!(b, Block::Mcp(_))) {
                        g.step_closed = true;
                    }
                }
            }
            "assistant_message_delta" | "assistant_message_completed" => {
                let text = p["text"].as_str().unwrap_or("");
                if !self.at.contains_key(&aid) && !text.is_empty() && !self.turns.is_empty() {
                    let ti = self.turns.len() - 1;
                    let t = &mut self.turns[ti];
                    t.blocks.push(Block::Text(String::new()));
                    self.at.insert(aid.clone(), (ti, t.blocks.len() - 1, 0));
                }
                if let Some(&(ti, bi, _)) = self.at.get(&aid) {
                    if let Block::Text(t) = &mut self.turns[ti].blocks[bi] {
                        if kind.ends_with("delta") {
                            t.push_str(text);
                        } else {
                            *t = text.into();
                        }
                    }
                }
                if kind.ends_with("completed") {
                    if let Some(s) = self.text_start.get(&aid) {
                        self.text_dur.insert(aid, ts - s);
                    }
                }
            }
            "tool_call_requested" => {
                let (ti, bi) = self.group(ts);
                let g = Self::group_at(&mut self.turns, ti, bi);
                if g.step_closed {
                    g.steps += 1;
                    g.step_closed = false;
                }
                let step = g.steps;
                g.items.push(Item::C(Call {
                    name: p["name"].as_str().unwrap_or("").into(),
                    args: p["arguments"].clone(),
                    st: St::Pending,
                    start: ts,
                    end: None,
                    err: None,
                    lines: 0,
                    exit: None,
                    changes: vec![],
                    content: String::new(),
                    step,
                }));
                g.last_ts = ts;
                self.at.insert(aid, (ti, bi, g.items.len() - 1));
            }
            "tool_call_started" | "tool_call_completed" => {
                let Some(&(ti, bi, ii)) = self.at.get(&aid) else { return };
                let g = Self::group_at(&mut self.turns, ti, bi);
                g.last_ts = ts;
                let Item::C(c) = &mut g.items[ii] else { return };
                if kind == "tool_call_started" {
                    c.st = St::Running;
                    return;
                }
                c.st = match p["status"].as_str() {
                    Some("completed") => St::Completed,
                    Some("failed") => St::Failed,
                    Some("denied") => St::Denied,
                    _ => St::Cancelled,
                };
                c.end = Some(ts);
                c.content = p["content"][0]["text"].as_str().unwrap_or("").into();
                c.lines = c.content.lines().count();
                c.exit = p["process"]["exit_code"].as_i64();
                if let Some(er) = p["error"].as_object() {
                    c.err = Some((er["code"].as_str().unwrap_or("").into(), er["message"].as_str().unwrap_or("").into()));
                }
                for ch in p["changes"].as_array().into_iter().flatten() {
                    let path = ch["path"].as_str().unwrap_or("").to_string();
                    let (a, r) = (ch["added"].as_i64().unwrap_or(0), ch["removed"].as_i64().unwrap_or(0));
                    c.changes.push((path.clone(), a, r));
                    let f = self.files.entry(path).or_default();
                    f.0 += a;
                    f.1 += r;
                }
            }
            "usage_recorded" => {
                let t = &p["tokens"];
                let cw: u64 = t["cache_write"].as_object().map_or(0, |m| m.values().filter_map(Value::as_u64).sum());
                let (i, r, o) = (t["input"].as_u64().unwrap_or(0), t["cache_read"].as_u64().unwrap_or(0), t["output"].as_u64().unwrap_or(0));
                self.input += i;
                self.cache_read += r;
                self.cache_write += cw;
                self.output += o;
                self.ctx = i + r + cw + o;
                self.cost += p["cost"].as_f64().unwrap_or(0.0);
                if let Some(a) = p["action_id"].as_str() {
                    if let Some(d) = self.text_dur.get(a) {
                        self.text_ms += d;
                        self.text_out += o;
                    }
                }
            }
            "steering_applied" => {
                if let Some(t) = self.turn() {
                    t.blocks.push(Block::Steer(p["text"].as_str().unwrap_or("").into(), ts));
                }
            }
            "steering_queue" => self.queue = p["messages"].as_array().into_iter().flatten().map(|m| m["text"].as_str().unwrap_or("").to_string()).collect(),
            "notice" => self.notices.push((p["code"].as_str().unwrap_or("").into(), p["message"].as_str().unwrap_or("").into(), false)),
            "mcp_server_failed" => {
                self.mcp_down.push(p["server"].as_str().unwrap_or("").into());
                let m = p["error"]["message"].as_str().unwrap_or("").to_string();
                if let Some(t) = self.turn() {
                    t.blocks.push(Block::Mcp(m));
                }
            }
            "job_started" => self.jobs.push(Job {
                id: p["job_id"].as_str().unwrap_or("").into(),
                desc: p["description"].as_str().unwrap_or("").into(),
                sid: None,
                running: true,
                start: ts,
                end: ts,
                calls: 0,
                last: String::new(),
            }),
            "delegate_started" => {
                if let Some(j) = self.jobs.iter_mut().find(|j| j.id == p["job_id"].as_str().unwrap_or("")) {
                    j.sid = p["session_id"].as_str().map(str::to_string);
                }
            }
            "job_completed" => {
                if let Some(j) = self.jobs.iter_mut().find(|j| j.id == p["job_id"].as_str().unwrap_or("")) {
                    j.running = false;
                    j.end = ts;
                }
            }
            _ => {}
        }
    }
}

// ============================================================ conversation
struct View {
    all_open: bool,
    reduced: bool,
}

fn result_spans(c: &Call) -> Vec<Span<'static>> {
    match c.st {
        St::Pending | St::Running => return vec![sp("running", fg(ORANGE))],
        St::Cancelled => return vec![sp("cancelled", dim())],
        St::Denied => return vec![sp("denied", dim())],
        St::Failed => {
            let (code, msg) = c.err.clone().unwrap_or_default();
            return vec![sp(format!("{code}: {msg}"), dim())];
        }
        St::Completed => {}
    }
    let (a, r) = c.changes.iter().fold((0, 0), |(a, r), x| (a + x.1, r + x.2));
    match kind_of(c) {
        "read" => vec![sp(format!("{} lines", c.lines), dim())],
        "search" => vec![sp(format!("{} matches", c.lines), dim())],
        "list" => vec![sp(format!("{} paths", c.lines), dim())],
        "edit" | "write" if !c.changes.is_empty() => vec![sp(format!("+{a}"), fg(BLUE)), sp(" ", Style::new()), sp(format!("−{r}"), fg(RED))],
        "shell" if c.exit.is_some() => vec![sp(format!("exit {} · {} lines", c.exit.unwrap(), c.lines), dim())],
        _ => vec![sp(c.content.lines().next().unwrap_or("done").to_string(), dim())],
    }
}

fn summary(g: &Group) -> Vec<Span<'static>> {
    const ORDER: [(&str, &str, &str, &str); 9] = [
        ("read", "read", "file", "files"),
        ("search", "searched", "pattern", "patterns"),
        ("list", "listed", "directory", "directories"),
        ("edit", "edited", "file", "files"),
        ("write", "wrote", "file", "files"),
        ("shell", "ran", "command", "commands"),
        ("delegate", "started", "delegate", "delegates"),
        ("jobs", "checked", "job", "jobs"),
        ("other", "called", "tool", "tools"),
    ];
    let calls: Vec<&Call> = g.items.iter().filter_map(|i| if let Item::C(c) = i { Some(c) } else { None }).collect();
    let mut parts: Vec<String> = vec![];
    let d = dim();
    let mut out: Vec<Span<'static>> = vec![];
    for (k, verb, one, many) in ORDER {
        let cs: Vec<&&Call> = calls.iter().filter(|c| kind_of(c) == k).collect();
        if cs.is_empty() {
            continue;
        }
        let n = if matches!(k, "read" | "edit" | "write") {
            let mut t: Vec<String> = cs.iter().map(|c| target(c)).collect();
            t.sort();
            t.dedup();
            t.len()
        } else {
            cs.len()
        };
        let mut s = format!("{verb} {n} {}", if n == 1 { one } else { many });
        if parts.is_empty() {
            s = s[..1].to_uppercase() + &s[1..];
        } else {
            out.push(sp(", ", d));
        }
        parts.push(s.clone());
        out.push(sp(s, d));
        if k == "edit" {
            let (a, r) = cs.iter().filter(|c| c.st == St::Completed).flat_map(|c| &c.changes).fold((0, 0), |(a, r), x| (a + x.1, r + x.2));
            if a + r > 0 {
                out.push(sp(" ", d));
                out.push(sp(format!("+{a}"), dfg(BLUE)));
                out.push(sp(" ", d));
                out.push(sp(format!("−{r}"), dfg(RED)));
            }
        }
    }
    let thoughts = g.items.iter().filter(|i| matches!(i, Item::R(_))).count();
    if thoughts > 0 {
        let t = if thoughts == 1 { "thought once".to_string() } else { format!("thought {thoughts} times") };
        if out.is_empty() {
            out.push(sp(t[..1].to_uppercase() + &t[1..], d));
        } else {
            out.push(sp(format!(", {t}"), d));
        }
    }
    out
}

fn group_lines(g: &Group, gid: Act, w: usize, v: &View) -> Vec<Row> {
    let calls: Vec<&Call> = g.items.iter().filter_map(|i| if let Item::C(c) = i { Some(c) } else { None }).collect();
    let live_r = g.items.iter().find_map(|i| if let Item::R(r) = i { r.end.is_none().then_some(r) } else { None });
    // thinking with no tool call before the next reply is one line
    if calls.is_empty() {
        return g
            .items
            .iter()
            .filter_map(|i| if let Item::R(r) = i { Some(r) } else { None })
            .map(|r| {
                let t = if r.end.is_none() {
                    format!("Thinking{}", heading(&r.text, true).map(|h| format!(": {h}")).unwrap_or_default())
                } else {
                    format!("+ Thought{} · {}", heading(&r.text, false).map(|h| format!(": {h}")).unwrap_or_default(), dur(r.end.unwrap() - r.start))
                };
                Row { spans: vec![sp(t, dim().add_modifier(Modifier::ITALIC))], bg: None, act: Some(gid) }
            })
            .collect();
    }
    let open = v.all_open || g.open;
    let running: Vec<String> = calls.iter().filter(|c| matches!(c.st, St::Running | St::Pending)).map(|c| target(c)).collect();
    let start = g.items.iter().map(|i| match i {
        Item::R(r) => r.start,
        Item::C(c) => c.start,
    });
    let first = start.min().unwrap_or(0);
    let mut head = vec![sp("● ", dim())];
    head.extend(summary(g));
    head.push(sp(format!(" · {}", dur(g.last_ts - first)), dim()));
    if !running.is_empty() {
        head.push(sp(format!(" · {}", running.join(", ")), dim()));
    }
    if let Some(r) = live_r {
        head.push(sp(format!(" · Thinking{}", heading(&r.text, true).map(|h| format!(": {h}")).unwrap_or_default()), dim().add_modifier(Modifier::ITALIC)));
    }
    head.push(sp(if open { "  ▾" } else { "  ▸" }, dim()));
    let mut out: Vec<Row> = wrap(head, w, vec![], vec![sp("  ", Style::new())]).into_iter().map(|s| Row { spans: s, bg: None, act: Some(gid) }).collect();
    if !open {
        return out;
    }
    // the ledger: one row per call, split by step, the step's thinking first
    let mut last_step = 0;
    for it in &g.items {
        let step = match it {
            Item::R(r) => r.step,
            Item::C(c) => c.step,
        };
        let gutter = if step != last_step { format!("{step:>5} ") } else { "      ".into() };
        last_step = step;
        match it {
            Item::R(r) => {
                let t = if r.end.is_none() {
                    format!("○ Thinking{}", heading(&r.text, true).map(|h| format!(": {h}")).unwrap_or_default())
                } else {
                    format!("+ Thought{} · {}", heading(&r.text, false).map(|h| format!(": {h}")).unwrap_or_default(), dur(r.end.unwrap() - r.start))
                };
                out.push(row(vec![sp(gutter, dim()), sp(t, dim().add_modifier(Modifier::ITALIC))]));
            }
            Item::C(c) => {
                let glyph = match c.st {
                    St::Running | St::Pending => sp("○", fg(ORANGE)),
                    St::Completed => sp("✓", fg(BLUE)),
                    St::Cancelled => sp("⊘", dim()),
                    _ => sp("✗", dim()),
                };
                let res = result_spans(c);
                let tw = w.saturating_sub(6 + 2 + 9 + 2 + width(&res));
                let mut t = target(c);
                if t.width() > tw {
                    t = t.chars().take(tw.saturating_sub(1)).collect::<String>() + "…";
                }
                let mut s = vec![sp(gutter, dim()), glyph, sp(" ", Style::new()), sp(format!("{:<9}", kind_of(c)), fg(CYAN)), sp(format!("{t:<tw$}"), Style::new()), sp("  ", Style::new())];
                s.extend(res);
                out.push(row(s));
            }
        }
    }
    out
}

fn bubble(text: &str, ts: i64, w: usize) -> Vec<Row> {
    let maxw = w * 72 / 100;
    let tl = (maxw - 4).min(text.width());
    let body = wrap(inline(text, Style::new()), tl + 1, vec![sp(" ", Style::new())], vec![sp(" ", Style::new())]);
    let bw = body.iter().map(|l| width(l)).max().unwrap_or(0) + 3;
    let pad = w - bw;
    let mut out = vec![];
    let e = |ch: &str| {
        let mut r = edge(ch, BU, None, bw);
        r.spans.insert(0, sp(" ".repeat(pad), Style::new()));
        r
    };
    out.push(e("▄"));
    for l in body {
        let mut s = vec![sp(" ".repeat(pad), Style::new())];
        let mut inner = fit(&l, bw - 2);
        inner.push(sp(" ", Style::new()));
        inner.push(sp("▐", fg(BLUE)));
        s.extend(tint(inner, BU));
        out.push(row(s));
    }
    out.push(e("▀"));
    let who = format!("you {}", clock(ts));
    out.push(row(vec![sp(" ".repeat(w.saturating_sub(who.width() + 1)), Style::new()), sp(who, dim())]));
    out
}

fn conversation(f: &Fold, w: usize, v: &View) -> Vec<Row> {
    let mut out = vec![];
    let iw = w - 2;
    for (ti, t) in f.turns.iter().enumerate() {
        out.push(Row::default());
        out.extend(bubble(&t.prompt, t.ts, w));
        let mut inner: Vec<Row> = vec![];
        for (bi, b) in t.blocks.iter().enumerate() {
            let r: Vec<Row> = match b {
                Block::Text(s) => md(s, iw),
                Block::Group(g) => group_lines(g, Act::Group(ti, bi), iw, v),
                Block::Steer(s, ts) => {
                    let lab = format!(" · {} ", clock(*ts));
                    let mut r = vec![row(vec![sp("steer", fg(ORANGE).add_modifier(Modifier::BOLD)), sp(lab.clone(), dim()), sp("─".repeat(iw.saturating_sub(5 + lab.width())), fg(SEL))])];
                    r.extend(para(s, iw, bold()));
                    r
                }
                Block::Mcp(m) => wrap(vec![sp(m.clone(), dim())], iw, vec![sp("⚠ ", fg(ORANGE))], vec![sp("  ", Style::new())]).into_iter().map(row).collect(),
                Block::Done(s) => vec![row(vec![sp(format!("▣ {s}"), dim())])],
            };
            if r.is_empty() {
                continue;
            }
            if !inner.is_empty() {
                inner.push(Row::default());
            }
            inner.extend(r);
        }
        if inner.is_empty() {
            continue;
        }
        out.push(Row::default());
        let rows = inner
            .into_iter()
            .map(|r| {
                let mut s = vec![sp(" ", Style::new())];
                let body = fit(&r.spans, iw);
                s.extend(if let Some(bg) = r.bg { tint(body, bg) } else { body });
                s.push(sp(" ", Style::new()));
                Row { spans: s, bg: None, act: r.act }
            })
            .collect();
        out.extend(slab(rows, BW, None, w));
    }
    out
}

// ============================================================ panel cards
fn bar(frac: f64, w: usize, c: Color) -> Vec<Span<'static>> {
    let f = ((frac * w as f64).round() as usize).clamp(if frac > 0.0 { 1 } else { 0 }, w);
    vec![sp("▆".repeat(f), fg(c)), sp("▆".repeat(w - f), fg(SEL))]
}
fn k(n: u64) -> String {
    if n >= 10_000 { format!("{}k", n / 1000) } else { format!("{:.1}k", n as f64 / 1000.0) }
}
const HANDOFF_AT: u64 = 400_000; // not in the stream: see README, findings
const WINDOW: u64 = 1_000_000; // not in the stream either

struct Card {
    title: Vec<Span<'static>>,
    lines: Vec<Vec<Span<'static>>>,
    cap: usize,
}
fn t() -> Span<'static> {
    sp("\t", Style::new())
}
fn cards(f: &Fold, now: i64) -> (Vec<Card>, Vec<Vec<Span<'static>>>) {
    let mut c = vec![];
    let mut short = vec![];
    let hit = if f.input + f.cache_read + f.cache_write > 0 { 100 * f.cache_read / (f.input + f.cache_read + f.cache_write) } else { 0 };
    let tps = if f.text_ms > 0 { f.text_out * 1000 / f.text_ms as u64 } else { 0 };
    let mode = if f.mode.is_empty() { "?".to_string() } else { f.mode.clone() };
    let down = f.mcp_down.first().map(|s| format!("✗ {s} down"));
    c.push(Card {
        title: vec![sp(f.cwd.clone(), bold()), t(), sp(mode.clone(), fg(ORANGE))],
        lines: vec![
            vec![sp("git ", dim()), sp(f.branch.clone(), fg(PURPLE)), sp(if f.dirty { "*" } else { "" }, fg(ORANGE))],
            vec![sp(f.model.clone(), fg(BLUE)), t(), sp(f.effort.clone(), fg(ORANGE)), sp(if f.thinking.is_empty() { "" } else { " ∴" }, dim())],
            [bar(f.ctx as f64 / HANDOFF_AT as f64, 17, CYAN), vec![sp("│", fg(ORANGE)), t(), sp(k(f.ctx), Style::new())]].concat(),
            vec![sp(format!("handoff {}", k(HANDOFF_AT)), dim()), t(), sp(format!("{}% of {}M", f.ctx * 100 / WINDOW, WINDOW / 1_000_000), dim())],
            vec![sp("in ", dim()), sp(k(f.input), Style::new()), sp(" out ", dim()), sp(k(f.output), Style::new()), t(), sp("cache ", dim()), sp(format!("{hit}%"), Style::new())],
            vec![sp(format!("${:.2}", f.cost), Style::new()), sp(format!(" · {tps} tok/s · {} turns", f.turns.len()), dim())],
            vec![sp("tools ", dim()), sp(f.tools.to_string(), Style::new()), t(), sp(down.clone().unwrap_or_default(), fg(RED))],
        ],
        cap: 0,
    });
    short.push(vec![sp(f.cwd.clone(), Style::new()), sp(" git ", dim()), sp(f.branch.clone(), fg(PURPLE)), sp(if f.dirty { "*" } else { "" }, fg(ORANGE))]);
    short.push(vec![sp(f.model.clone(), fg(BLUE)), sp(format!(" {}", f.effort), fg(ORANGE))]);
    short.push(vec![sp(mode, fg(ORANGE))]);
    short.push([vec![sp("ctx ", dim())], bar(f.ctx as f64 / HANDOFF_AT as f64, 8, CYAN), vec![sp(format!(" {}", k(f.ctx)), Style::new())]].concat());
    short.push(vec![sp(format!("${:.2}", f.cost), Style::new()), sp(" cache ", dim()), sp(format!("{hit}%"), Style::new())]);
    short.push(vec![sp(format!("tools {} ", f.tools), dim()), sp(down.unwrap_or_default(), fg(RED))]);

    let mut fl: Vec<(&String, &(i64, i64))> = f.files.iter().collect();
    fl.sort_by_key(|(_, (a, r))| -(a + r));
    let (ta, tr) = fl.iter().fold((0, 0), |(a, r), (_, x)| (a + x.0, r + x.1));
    let mut lines: Vec<Vec<Span<'static>>> = fl
        .iter()
        .take(5)
        .map(|(p, (a, r))| vec![sp(p.trim_start_matches("crates/").to_string(), Style::new()), t(), sp(format!("+{a}"), fg(BLUE)), sp(format!(" {:>3}", format!("−{r}")), fg(RED))])
        .collect();
    let more = if fl.len() > 5 { format!("… {} more", fl.len() - 5) } else { String::new() };
    lines.push(vec![sp(more, dim()), t(), sp(format!("+{ta}"), fg(BLUE)), sp(format!(" −{tr}"), fg(RED))]);
    c.push(Card { title: vec![sp("Changed files", bold()), t(), sp(fl.len().to_string(), dim())], lines, cap: 0 });
    short.push(vec![sp("± ", dim()), sp(format!("{} files ", fl.len()), Style::new()), sp(format!("+{ta}"), fg(BLUE)), sp(format!(" −{tr}"), fg(RED))]);

    let dels: Vec<&Job> = f.jobs.iter().filter(|j| j.sid.is_some()).collect();
    let drun = dels.iter().filter(|j| j.running).count();
    let lines = dels
        .iter()
        .flat_map(|j| {
            let (g, gc) = if j.running { ("●", ORANGE) } else { ("✓", BLUE) };
            let el = dur(if j.running { now } else { j.end } - j.start);
            let what = if j.running && !j.last.is_empty() { j.last.clone() } else { if j.running { "running" } else { "done" }.into() };
            [vec![sp(g, fg(gc)), sp(format!(" {}", j.desc), Style::new())], vec![sp(format!("  {} calls · {el} · {what}", j.calls), dim())]]
        })
        .collect();
    c.push(Card { title: vec![sp("Delegates", bold()), t(), sp(format!("{drun} running"), if drun > 0 { fg(ORANGE) } else { dim() })], lines, cap: 6 });
    short.push(vec![sp("◆ ", fg(PURPLE)), sp(format!("{drun} delegates"), Style::new())]);

    let jrun = f.jobs.iter().filter(|j| j.sid.is_none() && j.running).count();
    c.push(Card { title: vec![sp("Jobs", bold()), t(), sp(format!("{jrun} running"), if jrun > 0 { fg(ORANGE) } else { dim() }), sp(" ▸", dim())], lines: vec![], cap: 0 });
    if jrun > 0 {
        short.push(vec![sp("⚙ ", fg(ORANGE)), sp(format!("{jrun} job"), Style::new())]);
    }
    c.push(Card {
        title: vec![sp("Quota", bold()), t(), sp("extension", dim())],
        lines: vec![
            vec![sp("claude weekly", dim()), t(), sp("65%", Style::new())],
            bar(0.65, (PANEL - 7) as usize, BLUE),
            vec![sp("cursor monthly", dim()), t(), sp("1%", fg(RED))],
            bar(0.01, (PANEL - 7) as usize, RED),
        ],
        cap: 0,
    });
    short.push(vec![sp("Q ", dim()), sp("claude 65%", Style::new()), sp(" · ", dim()), sp("cursor 1%", fg(RED))]);
    (c, short)
}
fn panel_rows(f: &Fold, now: i64) -> Vec<Row> {
    let w = PANEL as usize - 2;
    let mut out = vec![];
    for card in cards(f, now).0 {
        let mut rows = vec![row([vec![sp("  ", Style::new())], fit(&card.title, w - 3), vec![sp(" ", Style::new())]].concat())];
        let body: Vec<_> = if card.cap > 0 { card.lines.into_iter().take(card.cap).collect() } else { card.lines };
        for l in body {
            rows.push(row([vec![sp("  ", Style::new())], fit(&l, w - 3), vec![sp(" ", Style::new())]].concat()));
        }
        out.extend(slab(rows, BC, Some(BP), w));
        out.push(Row::default());
    }
    out
}
fn status_rows(f: &Fold, now: i64, w: usize) -> Vec<Row> {
    let mut rows: Vec<Vec<Span<'static>>> = vec![vec![sp(" ", Style::new())]];
    for it in cards(f, now).1 {
        let cur = rows.last().unwrap();
        if cur.len() > 1 && width(cur) + 5 + width(&it) > w - 1 {
            if rows.len() == 2 {
                break;
            }
            rows.push(vec![sp(" ", Style::new())]);
        }
        let cur = rows.last_mut().unwrap();
        if cur.len() > 1 {
            cur.push(sp("  │  ", dim()));
        }
        cur.extend(it);
    }
    rows.into_iter().map(|s| Row { spans: s, bg: Some(BP), act: None }).collect()
}

// ============================================================ bottom of the conversation column
fn glimmer(word: &str, tick: u64, reduced: bool) -> Vec<Span<'static>> {
    if reduced {
        return vec![sp(word, Style::new())];
    }
    let n = word.chars().count() as i64;
    let pos = (tick as i64) % (n + 8) - 2;
    word.chars()
        .enumerate()
        .map(|(i, ch)| {
            let d = (i as i64 - pos).abs();
            sp(ch.to_string(), if d == 0 { fg(ORANGE).add_modifier(Modifier::BOLD) } else if d == 1 { fg(ORANGE) } else { dim() })
        })
        .collect()
}
fn bottom(f: &Fold, w: usize, tick: u64, now: i64, v: &View, below: usize, narrow: bool) -> Vec<Row> {
    let mut out = vec![];
    for (i, (code, msg, gone)) in f.notices.iter().enumerate() {
        if !gone {
            out.push(Row { spans: vec![sp("  ⚠ ", fg(ORANGE)), sp(code.clone(), fg(ORANGE)), sp(format!(" · {msg}"), dim()), t(), sp("✕ ", dim())], bg: None, act: Some(Act::Notice(i)) });
        }
    }
    if f.running {
        let spin = if v.reduced { "●" } else { SPIN[tick as usize % SPIN.len()] };
        let mut s = vec![sp("  ", Style::new()), sp(format!("{spin} "), fg(ORANGE))];
        s.extend(glimmer("Working", tick, v.reduced));
        s.push(sp(format!(" {} · esc to interrupt", dur(now - f.turn_start)), dim()));
        out.push(row(s));
    }
    if !f.queue.is_empty() {
        out.push(row(vec![sp("  • Steering, joins the turn at the next step", dim())]));
        for q in &f.queue {
            out.push(row(vec![sp("    ↳ ", dim()), sp(q.clone(), dim())]));
        }
        out.push(row(vec![sp("      ⌥↑ edit · ⌥↓ next · ⌥x drop · click a row", dim())]));
    }
    let mut line = vec![sp("▌", fg(BLUE)), sp(" ", Style::new()), sp("› ", fg(CYAN)), sp("█", dim())];
    if below > 0 {
        line.push(t());
        line.push(sp(format!("↓ {below} lines below · End"), fg(ORANGE)));
        line.push(sp(" ", Style::new()));
    }
    let mut ib = slab(vec![Row { spans: line, bg: None, act: if below > 0 { Some(Act::End) } else { None } }], BI, None, w);
    out.append(&mut ib);
    if narrow {
        out.extend(status_rows(f, now, w));
    }
    out
}

// ============================================================ painting
fn paint(buf: &mut Buffer, x: u16, y: u16, w: u16, r: &Row) {
    if let Some(bg) = r.bg {
        buf.set_style(Rect::new(x, y, w, 1), Style::new().bg(bg));
    }
    let spans = fit(&r.spans, w as usize);
    buf.set_line(x, y, &Line::from(spans), w);
}

// ============================================================ measurement
static BYTES: AtomicU64 = AtomicU64::new(0);
static FLUSHES: AtomicU64 = AtomicU64::new(0);
struct Counting<W: Write>(W);
impl<W: Write> Write for Counting<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let n = self.0.write(b)?;
        BYTES.fetch_add(n as u64, Relaxed);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        FLUSHES.fetch_add(1, Relaxed);
        self.0.flush()
    }
}
/// CPU seconds, voluntary and involuntary context switches, and (macOS) the
/// package idle wakeups and interrupt wakeups that `top` reports as IDLEW.
fn rusage() -> (f64, i64, i64, u64, u64) {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let t = |v: libc::timeval| v.tv_sec as f64 + v.tv_usec as f64 / 1e6;
    let (idle, intr) = wakeups();
    (t(u.ru_utime) + t(u.ru_stime), u.ru_nvcsw as i64, u.ru_nivcsw as i64, idle, intr)
}
#[cfg(target_os = "macos")]
fn wakeups() -> (u64, u64) {
    let mut i: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, &mut i as *mut _ as *mut libc::rusage_info_t) };
    if rc == 0 { (i.ri_pkg_idle_wkups, i.ri_interrupt_wkups) } else { (0, 0) }
}
#[cfg(not(target_os = "macos"))]
fn wakeups() -> (u64, u64) {
    (0, 0)
}

#[derive(Default)]
struct Audit {
    frames: u64,
    cells: u64,
    rows_max: usize,
    cells_max: usize,
    rows_hist: BTreeMap<usize, u64>,
}

// ============================================================ main
struct Args {
    path: String,
    speed: f64,
    static_: bool,
    reduced: bool,
    stats: Option<String>,
    exit_after: Option<f64>,
    warmup: f64,
    audit: bool,
}
fn args() -> Args {
    let mut a = Args { path: "fixtures/session.jsonl".into(), speed: 12.0, static_: false, reduced: false, stats: None, exit_after: None, warmup: 2.0, audit: false };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--speed" => a.speed = it.next().and_then(|v| v.parse().ok()).expect("--speed N"),
            "--static" => a.static_ = true,
            "--reduced-motion" => a.reduced = true,
            "--stats" => a.stats = it.next(),
            "--exit-after" => a.exit_after = it.next().and_then(|v| v.parse().ok()),
            "--warmup" => a.warmup = it.next().and_then(|v| v.parse().ok()).expect("--warmup S"),
            "--diff-audit" => a.audit = true,
            "-h" | "--help" => {
                println!("tui-prototype [FIXTURE] [--speed N] [--static] [--reduced-motion] [--stats FILE --exit-after S [--warmup S] [--diff-audit]]");
                std::process::exit(0);
            }
            p => a.path = p.into(),
        }
    }
    if std::env::var_os("FIBER_REDUCED_MOTION").is_some() {
        a.reduced = true;
    }
    a
}

fn main() -> io::Result<()> {
    let a = args();
    let events: Vec<Value> = std::fs::read_to_string(&a.path)?.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let mut f = Fold::default();
    let mut next = 0;
    if a.static_ {
        for e in &events {
            f.apply(e);
        }
        next = events.len();
    }

    terminal::enable_raw_mode()?;
    let mut out = Counting(BufWriter::with_capacity(1 << 16, io::stdout()));
    execute!(out, terminal::EnterAlternateScreen, event::EnableMouseCapture)?;
    let mut term = Terminal::new(CrosstermBackend::new(out))?;
    let res = run(&a, &events, &mut f, &mut next, &mut term);
    execute!(term.backend_mut(), event::DisableMouseCapture, terminal::LeaveAlternateScreen, crossterm::cursor::Show)?;
    terminal::disable_raw_mode()?;
    let report = res?;
    if let (Some(path), Some(r)) = (&a.stats, report) {
        std::fs::write(path, r)?;
    }
    println!("Session {0} · resume it with fiber --resume {0}", f.session_id);
    Ok(())
}

type Term = Terminal<CrosstermBackend<Counting<BufWriter<io::Stdout>>>>;

fn run(a: &Args, events: &[Value], f: &mut Fold, next: &mut usize, term: &mut Term) -> io::Result<Option<String>> {
    let v0 = Instant::now();
    let mut v = View { all_open: false, reduced: a.reduced };
    // replay clock: an event is due `gap / speed` after the one before it
    let ts_of = |i: usize| events[i]["ts"].as_i64().unwrap_or(0);
    let mut due = Instant::now();
    let mut last_applied = (Instant::now(), f.last_ts);
    let schedule = |i: usize, prev_due: Instant| -> Instant {
        let gap = (ts_of(i) - ts_of(i.saturating_sub(1))).max(0) as f64 / a.speed;
        let delta = events[i]["kind"].as_str().is_some_and(|k| k.ends_with("_delta"));
        prev_due + Duration::from_secs_f64(gap / 1000.0).max(if delta { Duration::from_millis(25) } else { Duration::ZERO })
    };
    if *next < events.len() {
        due = schedule(*next, due);
    }
    let mut tick: u64 = 0;
    let mut next_tick = Instant::now();
    let mut scroll: usize = 0; // lines above the bottom
    let mut pscroll: usize = 0;
    let mut conv_cache: Option<(usize, Vec<Row>)> = None;
    let mut panel_cache: Option<Vec<Row>> = None;
    let mut hits: Vec<(u16, u16, u16, Act)> = vec![];
    let mut dirty = true;
    let mut geom = (0u16, 0u16, 0u16, 0u16); // conv x-end, conv height, panel x, rows
    let exit_at = a.exit_after.map(|s| v0 + Duration::from_secs_f64(s));
    let mut window: Option<(Instant, u64, u64, u64, (f64, i64, i64, u64, u64))> = None;
    let mut audit = Audit::default();
    let mut prev_buf: Option<Buffer> = None;
    let mut frames: u64 = 0;

    loop {
        let now_i = Instant::now();
        // apply every event that is due
        while *next < events.len() && due <= now_i {
            f.apply(&events[*next]);
            last_applied = (due, f.last_ts);
            *next += 1;
            if *next < events.len() {
                due = schedule(*next, due);
            }
            dirty = true;
            conv_cache = None;
            panel_cache = None;
        }
        let replaying = *next < events.len();
        let rate = if replaying { a.speed } else { 1.0 };
        let vnow = last_applied.1 + (now_i.saturating_duration_since(last_applied.0).as_secs_f64() * 1000.0 * rate) as i64;
        if f.running && now_i >= next_tick {
            tick += 1;
            dirty = true;
            next_tick = if a.reduced {
                // only the elapsed seconds change: wake at the next whole second of the turn's clock
                let ms = 1000 - (vnow - f.turn_start).rem_euclid(1000);
                now_i + Duration::from_millis(ms as u64 + 1)
            } else {
                now_i + Duration::from_millis(GLIMMER_MS)
            };
        }
        if window.is_none() && now_i >= v0 + Duration::from_secs_f64(a.warmup) {
            window = Some((now_i, BYTES.load(Relaxed), frames, FLUSHES.load(Relaxed), rusage()));
        }

        if dirty {
            dirty = false;
            frames += 1;
            let size = term.size()?;
            let (cols, rows) = (size.width, size.height);
            let narrow = cols < PANEL + CONV_MIN;
            let conv_w = if narrow { cols } else { cols - PANEL };
            let cw = conv_w as usize - 2;
            if conv_cache.as_ref().is_none_or(|(w, _)| *w != cw) {
                let old = conv_cache.as_ref().map_or(0, |(_, r)| r.len());
                let rows_new = conversation(f, cw, &v);
                if scroll > 0 && old > 0 {
                    scroll += rows_new.len().saturating_sub(old); // paused: keep the view where it is
                }
                conv_cache = Some((cw, rows_new));
            }
            if panel_cache.is_none() && !narrow {
                panel_cache = Some(panel_rows(f, vnow));
            }
            let conv = &conv_cache.as_ref().unwrap().1;
            let mut bot = bottom(f, conv_w as usize, tick, vnow, &v, scroll, narrow);
            let view_h = (rows as usize).saturating_sub(bot.len());
            let max_scroll = conv.len().saturating_sub(view_h);
            if scroll > max_scroll {
                scroll = max_scroll;
                bot = bottom(f, conv_w as usize, tick, vnow, &v, scroll, narrow);
            }
            let start = conv.len().saturating_sub(view_h + scroll);
            let vis = &conv[start..(start + view_h).min(conv.len())];
            let top_pad = view_h - vis.len();
            hits.clear();
            geom = (conv_w, view_h as u16, conv_w, rows);
            let panel = panel_cache.as_ref();
            let completed = term.draw(|fr| {
                let buf = fr.buffer_mut();
                // conversation
                for (k, r) in vis.iter().enumerate() {
                    let y = (top_pad + k) as u16;
                    paint(buf, 1, y, cw as u16, r);
                    if let Some(act) = r.act {
                        hits.push((y, 1, 1 + cw as u16, act));
                    }
                }
                // scroll thumb
                if conv.len() > view_h {
                    let th = (view_h * view_h / conv.len()).max(1);
                    let tt = (view_h - th) * start / (conv.len() - view_h);
                    for y in tt..tt + th {
                        buf.set_string(conv_w - 1, y as u16, "┃", fg(rgb(0x808080)));
                    }
                }
                for (k, r) in bot.iter().enumerate() {
                    let y = (view_h + k) as u16;
                    paint(buf, 0, y, conv_w, r);
                    if let Some(act) = r.act {
                        hits.push((y, 0, conv_w, act));
                    }
                }
                // side panel
                if let Some(p) = panel {
                    buf.set_style(Rect::new(conv_w, 0, PANEL, rows), Style::new().bg(BP));
                    let ps = pscroll.min(p.len().saturating_sub(rows as usize));
                    for (k, r) in p.iter().skip(ps).take(rows as usize).enumerate() {
                        paint(buf, conv_w + 1, k as u16, PANEL - 2, r);
                    }
                }
            })?;
            if a.audit && window.is_some() {
                let cur = completed.buffer.clone();
                if let Some(prev) = &prev_buf {
                    if prev.area == cur.area {
                        let d = prev.diff(&cur);
                        let mut ys: Vec<u16> = d.iter().map(|(_, y, _)| *y).collect();
                        ys.dedup();
                        ys.sort();
                        ys.dedup();
                        audit.frames += 1;
                        audit.cells += d.len() as u64;
                        audit.cells_max = audit.cells_max.max(d.len());
                        audit.rows_max = audit.rows_max.max(ys.len());
                        *audit.rows_hist.entry(ys.len()).or_default() += 1;
                    }
                }
                prev_buf = Some(cur);
            }
        }

        // sleep until the next event, the next tick, or input; with no turn running and no
        // replay left there is no deadline at all
        let mut deadline: Option<Instant> = replaying.then_some(due);
        if f.running {
            deadline = Some(deadline.map_or(next_tick, |d| d.min(next_tick)));
        }
        if window.is_none() {
            let w = v0 + Duration::from_secs_f64(a.warmup);
            deadline = Some(deadline.map_or(w, |d| d.min(w)));
        }
        if let Some(x) = exit_at {
            deadline = Some(deadline.map_or(x, |d| d.min(x)));
            if Instant::now() >= x {
                break;
            }
        }
        let timeout = deadline.map_or(Duration::from_secs(86_400), |d| d.saturating_duration_since(Instant::now()));
        if !event::poll(timeout)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Char('c') if ctrl => break,
                    KeyCode::Char('q') => break,
                    KeyCode::Char('o') if ctrl => {
                        v.all_open = !v.all_open;
                        conv_cache = None;
                    }
                    KeyCode::End => scroll = 0,
                    KeyCode::PageUp => scroll += 20,
                    KeyCode::PageDown => scroll = scroll.saturating_sub(20),
                    KeyCode::Up => scroll += 1,
                    KeyCode::Down => scroll = scroll.saturating_sub(1),
                    _ => continue,
                }
                dirty = true;
            }
            Event::Mouse(m) => {
                let over_panel = m.column >= geom.2 && geom.2 < term.size()?.width && panel_cache.is_some();
                match m.kind {
                    MouseEventKind::ScrollUp if over_panel => pscroll = pscroll.saturating_sub(3),
                    MouseEventKind::ScrollDown if over_panel => pscroll += 3,
                    MouseEventKind::ScrollUp => scroll += 3,
                    MouseEventKind::ScrollDown => scroll = scroll.saturating_sub(3),
                    MouseEventKind::Down(MouseButton::Left) => {
                        let Some(&(_, _, _, act)) = hits.iter().find(|(y, x0, x1, _)| *y == m.row && m.column >= *x0 && m.column < *x1) else { continue };
                        match act {
                            Act::Group(ti, bi) => {
                                if let Block::Group(g) = &mut f.turns[ti].blocks[bi] {
                                    g.open = !g.open;
                                }
                                conv_cache = None;
                            }
                            Act::Notice(i) => f.notices[i].2 = true,
                            Act::End => scroll = 0,
                        }
                    }
                    _ => continue,
                }
                dirty = true;
            }
            Event::Resize(..) => {
                dirty = true;
                conv_cache = None;
            }
            _ => {}
        }
    }

    let Some((t0, b0, f0, fl0, r0)) = window else { return Ok(None) };
    let secs = t0.elapsed().as_secs_f64();
    let r1 = rusage();
    let bytes = BYTES.load(Relaxed) - b0;
    let fr = frames - f0;
    let mut s = String::new();
    use std::fmt::Write as _;
    let _ = writeln!(s, "mode\t{}", if a.reduced { "reduced-motion" } else { "glimmer" });
    let _ = writeln!(s, "running\t{}", f.running);
    let _ = writeln!(s, "seconds\t{secs:.2}");
    let _ = writeln!(s, "frames\t{fr}\nfps\t{:.2}", fr as f64 / secs);
    let _ = writeln!(s, "bytes\t{bytes}\nbytes_per_s\t{:.0}\nbytes_per_frame\t{:.1}", bytes as f64 / secs, bytes as f64 / fr.max(1) as f64);
    let _ = writeln!(s, "flushes\t{}", FLUSHES.load(Relaxed) - fl0);
    let _ = writeln!(s, "cpu_s\t{:.4}\ncpu_pct\t{:.3}", r1.0 - r0.0, 100.0 * (r1.0 - r0.0) / secs);
    let _ = writeln!(s, "vol_csw\t{}\ninvol_csw\t{}", r1.1 - r0.1, r1.2 - r0.2);
    let _ = writeln!(s, "idle_wakeups\t{}\ninterrupt_wakeups\t{}", r1.3 - r0.3, r1.4 - r0.4);
    if a.audit {
        let _ = writeln!(s, "audit_frames\t{}\ncells_per_frame\t{:.2}\ncells_max\t{}\nrows_max\t{}\nrows_hist\t{:?}", audit.frames, audit.cells as f64 / audit.frames.max(1) as f64, audit.cells_max, audit.rows_max, audit.rows_hist);
    }
    Ok(Some(s))
}
