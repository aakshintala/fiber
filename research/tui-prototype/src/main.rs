//! Stages 1 and 2 of the #15 TUI prototype: replays a fixture session of
//! `docs/events.md` lines in a real terminal. Throwaway code: the fold and the
//! drawing side by side, the input parser in `input.rs`, a few tests.

mod completions;
mod home;
mod input;
mod lua;
mod model_picker;
mod paged;

use crossterm::{execute, terminal};
use input::{Ev, Key, Mouse};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Terminal;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
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
const BH: Color = rgb(0x1f1a2e); // handoff band
const HD: Color = rgb(0xff9f43);
const SX_KW: Color = rgb(0x6eaafe);
const SX_FN: Color = rgb(0x7dd3fc);
const SX_STR: Color = rgb(0xce9178);
const SX_NUM: Color = rgb(0xb5cea8);
const SX_COM: Color = rgb(0x7a7a8a);

const PANEL: u16 = 34;
const CONV_MIN: u16 = 84;
const FLOOR_COLS: u16 = 40;
const FLOOR_ROWS: u16 = 10;
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
    /// a ledger row or a thought line: (turn, block, item), where paged the turn names a page
    Item(usize, usize, usize),
    Notice(usize),
    End,
    Choice(usize),
    NextPending,
    Reopen,
    Tab(usize),
    Opt(usize),
    Submit,
    Decline,
    /// a multi-choice question's Next row: on to the next question, or the review
    Next,
    QRow(usize),
    QDrop(usize),
    /// opens the context breakdown
    Context,
    /// leaves a swapped view
    Back,
    /// the model picker's row for one model, by its flat index: focuses it
    Pick(usize),
    /// one thinking chip of one model: focuses that chip
    PickChip(usize, usize),
    /// the picker's show-all toggle
    PickAll,
    /// the picker's refresh-all button
    PickRefresh,
    /// the rail's row for one fake session, by its index: moves the on-screen marker only
    Rail(usize),
    /// the rail's scope line: shows or hides sessions in other projects
    RailAll,
    /// a rail variant chip: switches the A/B/C design
    RailVariant(u8),
    /// a rail density chip: switches the B card density (0 full, 1 medium, 2 compact)
    RailDensity(u8),
    /// toggles the rail shown/hidden (⌥R, or the Session card's waiting line)
    RailToggle,
    /// a crashed card's ✕: dismisses it, leaving its number's gap
    RailDismiss(usize),
    /// a project header's "+" chip: starts a session in that project
    RailNew(&'static str),
    /// a project header's "N done": opens that project's session list
    RailDone(&'static str),
    /// a click region an extension drew, by its interned id
    Ext(usize),
}

#[derive(Clone, Default)]
struct Row {
    spans: Vec<Span<'static>>,
    bg: Option<Color>,
    act: Option<Act>,
    /// continues the line above, which wrapping broke: copied joined with a space
    cont: bool,
    /// leading cells that are layout, not text: never copied
    pre: u16,
    /// reply text: URLs and file paths in it get OSC 8 links
    link: bool,
    /// click targets within the row, as cell ranges
    hot: Vec<(u16, u16, Act)>,
    /// drawn by a Lua renderer, from this
    lua: Option<std::rc::Rc<lua::LuaSrc>>,
}
fn row(spans: Vec<Span<'static>>) -> Row {
    Row { spans, ..Default::default() }
}
/// A row whose spans carry their own click targets.
fn hot_row(parts: Vec<(Span<'static>, Option<Act>)>) -> Row {
    let (mut x, mut hot, mut spans) = (0u16, vec![], vec![]);
    for (s, a) in parts {
        let w = s.content.width() as u16;
        if let Some(a) = a {
            hot.push((x, x + w, a));
        }
        x += w;
        spans.push(s);
    }
    Row { spans, hot, ..Default::default() }
}
/// Puts spans in front of a row, moving its click targets with it.
fn prefixed(r: Row, pre: Vec<Span<'static>>) -> Row {
    let dx = width(&pre) as u16;
    let hot = r.hot.iter().map(|&(a, b, k)| (a + dx, b + dx, k)).collect();
    Row { spans: [pre, r.spans].concat(), hot, ..r }
}
fn plain(r: &Row) -> String {
    r.spans.iter().map(|s| s.content.as_ref()).collect()
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
    Row { spans: vec![sp(ch.repeat(w), s)], bg: outer, ..Default::default() }
}
/// A surface: a tinted block with half-block edges and no border.
fn slab(rows: Vec<Row>, bg: Color, outer: Option<Color>, w: usize) -> Vec<Row> {
    let mut out = vec![edge("▄", bg, outer, w)];
    for r in rows {
        let inner_bg = r.bg.unwrap_or(bg);
        out.push(Row { spans: tint(tint(fit(&r.spans, w), inner_bg), bg), bg: Some(bg), ..r });
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
/// `wrap`, keeping which rows continue a line, so a copy can unwrap them.
fn wrap_rows(spans: Vec<Span<'static>>, w: usize, first: Vec<Span<'static>>, rest: Vec<Span<'static>>) -> Vec<Row> {
    let pre = width(&rest) as u16;
    wrap(spans, w, first, rest).into_iter().enumerate().map(|(i, s)| Row { spans: s, cont: i > 0, pre: if i > 0 { pre } else { 0 }, ..Default::default() }).collect()
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
    text.split('\n').flat_map(|p| wrap_rows(inline(p, base), w, vec![], vec![])).collect()
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
            out.extend(wrap_rows(inline(b, Style::new()), w, vec![sp("• ", fg(BLUE))], vec![sp("  ", Style::new())]));
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
    /// `ask_user`: "answered" or "declined" once its form is resolved
    asked: Option<&'static str>,
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
    /// a resolved form: (header, answer, skipped) per question, the note, when, declined
    Answer(Vec<(String, String, bool)>, Option<String>, i64, bool),
    /// breaks the turn's card with a band
    Handoff(Handoff),
}
struct Handoff {
    trigger: String,
    before: u64,
    /// the context size the first request after the handoff reports
    after: Option<u64>,
    ts: i64,
    /// none while the note is being written
    outcome: Option<String>,
}
/// An approval or a question waiting on the person.
struct Pending {
    rid: String,
    sid: String,
    aid: String,
    what: Asking,
}
enum Asking {
    Approval { tool: String, args: Value, effects: Vec<String>, reversible: bool, paths: Vec<String>, step: String, req: Box<Value> },
    Form(Vec<Value>),
}
#[derive(Default)]
struct Turn {
    prompt: String,
    ts: i64,
    blocks: Vec<Block>,
    /// tool calls of this turn folded elsewhere: a paged piece of a turn starts mid-turn
    calls_before: usize,
    /// a paged piece that starts right after a handoff band another piece drew: its card opens anew
    after_band: bool,
    /// a paged piece of a turn whose earlier pieces drew no blocks: the card opens here, after
    /// the gap under the bubble
    fresh: bool,
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
    /// the steering queue: (id of the `steer` command, text)
    queue: Vec<(String, String)>,
    pending: Vec<Pending>,
    /// a delegate's calls, by action id, so its approvals can show the call
    dcalls: HashMap<String, (String, Value)>,
    /// each form's action id and fields, by request id
    forms: HashMap<String, (String, Vec<Value>)>,
    notices: Vec<(String, String, bool)>,
    running: bool,
    turn_start: i64,
    last_ts: i64,
    /// characters of what the preamble and opening message put in context, for the context view
    sys_chars: usize,
    tooldef_chars: usize,
    instr_chars: usize,
    skills_chars: usize,
    /// the latest handoff's band, while it waits on its outcome or on the size after it
    ho: Option<(usize, usize)>,
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
    if let Some(q) = c.args["questions"].as_array() {
        return q.iter().filter_map(|q| q["header"].as_str()).collect::<Vec<_>>().join(", ");
    }
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
/// A rail card's elapsed time in coarse units only, one unit, so the name gets the space.
fn rail_age(ms: i64) -> String {
    let s = (ms.max(0) + 500) / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
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
    fn band(&mut self) -> Option<&mut Handoff> {
        let (ti, bi) = self.ho?;
        match self.turns.get_mut(ti)?.blocks.get_mut(bi)? {
            Block::Handoff(h) => Some(h),
            _ => None,
        }
    }
    /// A call's tool and arguments, from this session's turns or a delegate's relayed lines.
    fn call_of(&self, sid: &str, aid: &str) -> (String, Value) {
        if sid == self.session_id {
            if let Some(&(ti, bi, ii)) = self.at.get(aid) {
                if let Block::Group(g) = &self.turns[ti].blocks[bi] {
                    if let Item::C(c) = &g.items[ii] {
                        return (c.name.clone(), c.args.clone());
                    }
                }
            }
        }
        self.dcalls.get(aid).cloned().unwrap_or_default()
    }
    /// Action ids of this session's calls still running.
    fn running_calls(&self) -> Vec<String> {
        self.at
            .iter()
            .filter(|(_, t)| {
                let (ti, bi, ii) = **t;
                matches!(&self.turns[ti].blocks[bi], Block::Group(g) if matches!(&g.items[ii], Item::C(c) if matches!(c.st, St::Pending | St::Running)))
            })
            .map(|(a, _)| a.clone())
            .collect()
    }
    /// Who is asking: "main", or the delegate's description.
    fn asker(&self, sid: &str) -> String {
        if sid == self.session_id {
            return "main".into();
        }
        self.jobs.iter().find(|j| j.sid.as_deref() == Some(sid)).map_or(sid.to_string(), |j| format!("◆ {} ({sid})", j.desc))
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
        // approvals and questions from any session in the tree share one queue
        match kind {
            "permission_requested" | "interaction_requested" => {
                let rid = p["request_id"].as_str().unwrap_or("").to_string();
                // a tool call is the approval's envelope action; a form names its calls in `action_ids`
                let caid = if kind == "permission_requested" { aid.clone() } else { p["action_ids"][0].as_str().unwrap_or("").to_string() };
                let what = if kind == "permission_requested" {
                    let (tool, args) = self.call_of(sid, &caid);
                    let strs = |k: &str| p[k].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
                    Asking::Approval { tool, args, effects: strs("effects"), reversible: p["reversible"].as_bool().unwrap_or(false), paths: strs("paths"), step: p["step"].as_str().unwrap_or("").into(), req: Box::new(p.clone()) }
                } else {
                    let fields = p["fields"].as_array().cloned().unwrap_or_default();
                    self.forms.insert(rid.clone(), (caid.clone(), fields.clone()));
                    Asking::Form(fields)
                };
                self.pending.push(Pending { rid, sid: sid.into(), aid: caid, what });
            }
            "permission_resolved" | "interaction_resolved" => {
                let rid = p["request_id"].as_str().unwrap_or("");
                self.pending.retain(|x| x.rid != rid);
            }
            _ => {}
        }
        if sid != self.session_id {
            // a delegate's own lines, relayed: the Delegates card counts its calls
            if kind == "tool_call_requested" {
                self.dcalls.insert(aid.clone(), (p["name"].as_str().unwrap_or("").into(), p["arguments"].clone()));
            }
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
                self.instr_chars = p["instruction_files"].as_array().into_iter().flatten().map(|i| i["content"].as_str().map_or(0, str::len)).sum();
                self.skills_chars = p["skills"].as_array().filter(|a| !a.is_empty()).map_or(0, |a| Value::from(a.clone()).to_string().len());
            }
            "preamble_built" | "model_changed" => {
                let src = if kind == "model_changed" { &p["after"] } else { p };
                self.model = src["model"].as_str().unwrap_or("").rsplit('/').next().unwrap_or("").into();
                self.effort = src["effort"].as_str().unwrap_or("").into();
                self.thinking = src["thinking"].as_str().unwrap_or("").into();
                if let Some(t) = p["tools"].as_array() {
                    self.tools = t.len();
                    // deferred tools are not in context until loaded
                    self.tooldef_chars = t.iter().filter(|d| d["deferred"] != true).map(|d| d["definition"].to_string().len()).sum();
                }
                if let Some(sp) = p["system_prompt"].as_str() {
                    self.sys_chars = sp.len();
                }
            }
            "mode_changed" => self.mode = p["after"].as_str().unwrap_or("").into(),
            "turn_started" => {
                let text = p["input"].as_array().into_iter().flatten().filter(|i| i["type"] == "message").filter_map(|i| i["content"][0]["text"].as_str()).collect::<Vec<_>>().join("\n\n");
                self.turns.push(Turn { prompt: text, ts, ..Default::default() });
                self.running = true;
                self.turn_start = ts;
            }
            "turn_completed" => {
                let outcome = p["outcome"].as_str().unwrap_or("completed");
                let Some(t) = self.turns.last_mut() else { return };
                let n: usize = t.calls_before + t.blocks.iter().map(|b| if let Block::Group(g) = b { g.items.iter().filter(|i| matches!(i, Item::C(_))).count() } else { 0 }).sum::<usize>();
                let s = format!("{outcome} · {} · {n} tool calls", dur(ts - t.ts));
                t.blocks.push(Block::Done(s));
                self.running = false;
                // nothing of this session's can still wait on the person once its turn ends
                let me = self.session_id.clone();
                self.pending.retain(|x| x.sid != me);
            }
            "interaction_resolved" => {
                let Some((caid, fields)) = self.forms.get(p["request_id"].as_str().unwrap_or("")).cloned() else { return };
                let declined = p["declined"].as_bool() == Some(true);
                let rows = if declined {
                    vec![]
                } else {
                    fields
                        .iter()
                        .enumerate()
                        .map(|(k, f)| {
                            let h = f["header"].as_str().unwrap_or("").to_string();
                            let x = &p["answers"][k];
                            if x["skipped"].as_bool() == Some(true) || x.is_null() {
                                return (h, "skipped".into(), true);
                            }
                            let mut v: Vec<String> = x["labels"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
                            if let Some(t) = x["text"].as_str() {
                                v.push(format!("“{t}”"));
                            }
                            (h, v.join(", "), false)
                        })
                        .collect()
                };
                let note = p["note"].as_str().map(str::to_string);
                if let Some(&(ti, bi, ii)) = self.at.get(&caid) {
                    if let Item::C(c) = &mut Self::group_at(&mut self.turns, ti, bi).items[ii] {
                        c.asked = Some(if declined { "declined" } else { "answered" });
                    }
                }
                if let Some(t) = self.turn() {
                    t.blocks.push(Block::Answer(rows, note, ts, declined));
                }
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
                // the handoff note belongs to the band, not the card
                let noting = self.band().is_some_and(|h| h.outcome.is_none());
                if !self.at.contains_key(&aid) && !text.is_empty() && !self.turns.is_empty() && !noting {
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
                    asked: None,
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
                let ctx = self.ctx;
                if let Some(h) = self.band().filter(|h| h.outcome.as_deref() == Some("completed")) {
                    h.after = Some(ctx);
                    self.ho = None;
                }
                if !aid.is_empty() {
                    if let Some(d) = self.text_dur.get(&aid) {
                        self.text_ms += d;
                        self.text_out += o;
                    }
                }
            }
            "handoff_started" => {
                let Some(t) = self.turns.last_mut() else { return };
                t.blocks.push(Block::Handoff(Handoff { trigger: p["trigger"].as_str().unwrap_or("").into(), before: 0, after: None, ts, outcome: None }));
                let bi = t.blocks.len() - 1;
                self.ho = Some((self.turns.len() - 1, bi));
            }
            "handoff_completed" => {
                // a tool-started handoff writes no handoff_started
                if !self.band().is_some_and(|h| h.outcome.is_none()) {
                    let Some(t) = self.turns.last_mut() else { return };
                    t.blocks.push(Block::Handoff(Handoff { trigger: "tool".into(), before: 0, after: None, ts, outcome: None }));
                    let bi = t.blocks.len() - 1;
                    self.ho = Some((self.turns.len() - 1, bi));
                }
                let outcome = p["outcome"].as_str().unwrap_or("completed").to_string();
                let done = outcome == "completed";
                if let Some(h) = self.band() {
                    h.outcome = Some(outcome);
                    h.before = p["tokens_before"].as_u64().unwrap_or(0);
                    h.ts = ts;
                }
                if !done {
                    self.ho = None;
                }
            }
            "steering_applied" => {
                if let Some(t) = self.turn() {
                    t.blocks.push(Block::Steer(p["content"][0]["text"].as_str().unwrap_or("").into(), ts));
                }
            }
            "steering_queue" => self.queue = p["messages"].as_array().into_iter().flatten().map(|m| (m["command_id"].as_str().unwrap_or("").to_string(), m["content"][0]["text"].as_str().unwrap_or("").to_string())).collect(),
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
                    j.sid = p["delegate_session_id"].as_str().map(str::to_string);
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
#[derive(Default)]
struct View {
    all_open: bool,
    reduced: bool,
    /// the search query, lower case, once it is long enough to open groups
    q: String,
    /// the group whose call is asking the person, opened so the call can be read
    force: Option<Act>,
    /// the Lua renderer for one tool's ledger rows
    lua: Option<std::rc::Rc<lua::Ext>>,
    /// ledger rows and thought lines opened to their result or text, as `Act::Item` keys
    exp: HashSet<(usize, usize, usize)>,
}

/// A thought's text, opened under its line, dim.
fn thought_rows(text: &str, w: usize, ind: usize) -> Vec<Row> {
    let pad = || vec![sp(" ".repeat(ind), Style::new())];
    text.split('\n').flat_map(|p| wrap_rows(vec![sp(p, dim())], w, pad(), pad())).map(|r| Row { pre: ind as u16, ..r }).collect()
}
/// A call's result, opened under its ledger row: its error, or its output clipped to 20 lines.
fn call_rows(c: &Call) -> Vec<Row> {
    const MAX: usize = 20;
    let text = match &c.err {
        Some((code, msg)) => format!("{code}: {msg}"),
        None if c.content.is_empty() => "no output".into(),
        None => c.content.replace('\t', "    "),
    };
    let lines: Vec<&str> = text.lines().collect();
    let r = |s: Span<'static>| Row { spans: vec![sp("        ", Style::new()), s], pre: 8, ..Default::default() };
    let mut out: Vec<Row> = lines.iter().take(MAX).map(|l| r(sp(l.to_string(), Style::new()))).collect();
    if lines.len() > MAX {
        out.push(r(sp(format!("… {} more lines", lines.len() - MAX), dim())));
    }
    out
}

fn result_spans(c: &Call) -> Vec<Span<'static>> {
    if kind_of(c) == "ask" {
        return match (c.asked, c.st) {
            (Some(a), _) => vec![sp(a, dim())],
            (None, St::Pending | St::Running) => vec![sp("waiting on you", fg(ORANGE))],
            _ => vec![sp("cancelled", dim())],
        };
    }
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
    const ORDER: [(&str, &str, &str, &str); 10] = [
        ("read", "read", "file", "files"),
        ("search", "searched", "pattern", "patterns"),
        ("list", "listed", "directory", "directories"),
        ("edit", "edited", "file", "files"),
        ("write", "wrote", "file", "files"),
        ("shell", "ran", "command", "commands"),
        ("delegate", "started", "delegate", "delegates"),
        ("jobs", "checked", "job", "jobs"),
        ("ask", "asked", "question", "questions"),
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
        let n = if k == "ask" {
            cs.iter().map(|c| c.args["questions"].as_array().map_or(1, Vec::len)).sum()
        } else if matches!(k, "read" | "edit" | "write") {
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
    let Act::Group(ti, bi) = gid else { unreachable!() };
    // thinking with no tool call before the next reply is one line; a click shows its text
    if calls.is_empty() {
        return g
            .items
            .iter()
            .enumerate()
            .filter_map(|(ii, i)| if let Item::R(r) = i { Some((ii, r)) } else { None })
            .flat_map(|(ii, r)| {
                let t = if r.end.is_none() {
                    format!("Thinking{}", heading(&r.text, true).map(|h| format!(": {h}")).unwrap_or_default())
                } else {
                    format!("+ Thought{} · {}", heading(&r.text, false).map(|h| format!(": {h}")).unwrap_or_default(), dur(r.end.unwrap() - r.start))
                };
                let mut out = vec![Row { spans: vec![sp(t, dim().add_modifier(Modifier::ITALIC))], act: Some(Act::Item(ti, bi, ii)), ..Default::default() }];
                if v.exp.contains(&(ti, bi, ii)) {
                    out.extend(thought_rows(&r.text, w, 2));
                }
                out
            })
            .collect();
    }
    let running: Vec<String> = calls.iter().filter(|c| matches!(c.st, St::Running | St::Pending)).map(|c| target(c)).collect();
    let start = g.items.iter().map(|i| match i {
        Item::R(r) => r.start,
        Item::C(c) => c.start,
    });
    let first = start.min().unwrap_or(0);
    let mut head = vec![sp("● ", dim())];
    head.extend(summary(g));
    if !running.is_empty() {
        head.push(sp(format!(" · {}", running.join(", ")), dim()));
    }
    if let Some(r) = live_r {
        head.push(sp(format!(" · Thinking{}", heading(&r.text, true).map(|h| format!(": {h}")).unwrap_or_default()), dim().add_modifier(Modifier::ITALIC)));
    }
    // the ledger: one row per call, split by step, the step's thinking first
    let mut ledger: Vec<Row> = vec![];
    let mut last_step = 0;
    for (ii, it) in g.items.iter().enumerate() {
        let (key, open) = (Act::Item(ti, bi, ii), v.exp.contains(&(ti, bi, ii)));
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
                ledger.push(Row { act: Some(key), ..row(vec![sp(gutter, dim()), sp(t, dim().add_modifier(Modifier::ITALIC))]) });
                if open {
                    ledger.extend(thought_rows(&r.text, w, 8));
                }
            }
            Item::C(c) if let Some(rows) = v.lua.as_ref().filter(|x| x.tool == c.name).and_then(|x| x.rows(c, &gutter, w)) => ledger.extend(rows),
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
                ledger.push(Row { act: Some(key), ..row(s) });
                if open {
                    ledger.extend(call_rows(c));
                }
            }
        }
    }
    // a search match inside the ledger opens it, as does a call waiting on the person
    let hit = !v.q.is_empty() && ledger.iter().any(|r| plain(r).to_lowercase().contains(&v.q));
    let open = v.all_open || g.open || hit || v.force == Some(gid);
    // one row whatever runs: wrapping to two rows and back as calls start and finish
    // moved everything around it, so what is in flight is cut to fit; the duration and toggle stay right
    head.extend([t(), sp(format!("{} {}", dur(g.last_ts - first), if open { "▾" } else { "▸" }), dim())]);
    let mut out = vec![Row { spans: fit(&head, w), act: Some(gid), ..Default::default() }];
    if open {
        out.extend(ledger);
    }
    out
}

fn bubble(text: &str, ts: i64, w: usize) -> Vec<Row> {
    let maxw = w * 72 / 100;
    let tl = (maxw - 4).min(text.width());
    let body = wrap(inline(text, Style::new()), tl + 1, vec![sp(" ", Style::new())], vec![sp(" ", Style::new())]);
    let bw = body.iter().map(|l| width(l)).max().unwrap_or(0) + 3;
    let pad = w.saturating_sub(bw);
    let mut out = vec![];
    let e =|ch: &str| {
        let mut r = edge(ch, BU, None, bw);
        r.spans.insert(0, sp(" ".repeat(pad), Style::new()));
        r
    };
    out.push(e("▄"));
    for (i, l) in body.into_iter().enumerate() {
        let mut s = vec![sp(" ".repeat(pad), Style::new())];
        let mut inner = fit(&l, bw - 2);
        inner.push(sp(" ", Style::new()));
        inner.push(sp("▐", fg(BLUE)));
        s.extend(tint(inner, BU));
        out.push(Row { spans: s, cont: i > 0, pre: pad as u16 + 1, ..Default::default() });
    }
    out.push(e("▀"));
    let who = format!("you {}", clock(ts));
    out.push(row(vec![sp(" ".repeat(w.saturating_sub(who.width() + 1)), Style::new()), sp(who, dim())]));
    out
}

/// A handoff's band: the trigger, the context before and after, and the time.
fn band_rows(h: &Handoff, w: usize) -> Vec<Row> {
    let why = match h.trigger.as_str() {
        "auto" => format!("automatic at {}", k(HANDOFF_AT)),
        "person" => "you asked with /handoff".into(),
        "overflow" => "the request did not fit".into(),
        "tool" => "the model handed off".into(),
        x => x.into(),
    };
    let size = match h.outcome.as_deref() {
        None => sp("writing the note…", dim()),
        Some("completed") => sp(format!("{} → {}", k(h.before), h.after.map_or("…".into(), k)), bold()),
        Some(o) => sp(format!("{o} · the context is unchanged"), fg(ORANGE)),
    };
    let s = vec![sp(" ⇄ ", fg(PURPLE)), sp("handoff", fg(PURPLE).add_modifier(Modifier::BOLD)), sp(format!(" · {why}"), dim()), t(), size, sp(format!(" · {} ", clock(h.ts)), dim())];
    slab(vec![row(s)], BH, None, w)
}

fn conversation(f: &Fold, w: usize, v: &View) -> Vec<Row> {
    f.turns.iter().enumerate().flat_map(|(ti, t)| turn_rows(ti, t, w, v, true, true)).collect()
}

/// One turn's rows. `head`: the turn starts here, so the gap, the bubble and the card's top
/// edge; `tail`: it ends here, so the card's bottom edge. A paged TUI renders a turn in
/// pieces, which join into exactly the rows of the whole turn.
fn turn_rows(ti: usize, t: &Turn, w: usize, v: &View, head: bool, tail: bool) -> Vec<Row> {
    let mut out = vec![];
    let iw = w - 2;
    {
        if head {
            out.push(Row::default());
            out.extend(bubble(&t.prompt, t.ts, w));
        }
        // a handoff breaks the card: each band starts a new piece of it
        let mut segs: Vec<(Option<&Handoff>, Vec<Row>)> = vec![(None, vec![])];
        for (bi, b) in t.blocks.iter().enumerate() {
            let r: Vec<Row> = match b {
                Block::Handoff(h) => {
                    segs.push((Some(h), vec![]));
                    continue;
                }
                Block::Text(s) => md(s, iw).into_iter().map(|r| Row { link: true, ..r }).collect(),
                Block::Group(g) => group_lines(g, Act::Group(ti, bi), iw, v),
                Block::Steer(s, ts) => {
                    let lab = format!(" · {} ", clock(*ts));
                    let mut r = vec![row(vec![sp("steer", fg(ORANGE).add_modifier(Modifier::BOLD)), sp(lab.clone(), dim()), sp("─".repeat(iw.saturating_sub(5 + lab.width())), fg(SEL))])];
                    r.extend(para(s, iw, bold()));
                    r
                }
                Block::Mcp(m) => wrap_rows(vec![sp(m.clone(), dim())], iw, vec![sp("⚠ ", fg(ORANGE))], vec![sp("  ", Style::new())]),
                Block::Done(s) => vec![row(vec![sp(format!("▣ {s}"), dim())])],
                Block::Answer(rows, note, ts, declined) => {
                    // the person's words, so a labelled rule like a steer, not a dim tool row
                    let lab = format!(" · {} ", clock(*ts));
                    let mut r = vec![row(vec![sp("you answered", fg(BLUE).add_modifier(Modifier::BOLD)), sp(lab.clone(), dim()), sp("─".repeat(iw.saturating_sub(12 + lab.width())), fg(SEL))])];
                    if *declined {
                        r.push(row(vec![sp("declined: the turn ended so you could answer in your own words", dim())]));
                    }
                    for (h, a, skipped) in rows {
                        r.push(row(vec![sp(format!("{h:<13}"), fg(BLUE)), sp(a.clone(), if *skipped { dim() } else { bold() })]));
                    }
                    if let Some(n) = note {
                        r.push(row(vec![sp(format!("{:<13}", "note"), fg(BLUE)), sp(format!("“{n}”"), bold())]));
                    }
                    r
                }
            };
            if r.is_empty() {
                continue;
            }
            let first = segs.len() == 1;
            let inner = &mut segs.last_mut().unwrap().1;
            // a piece that starts mid-turn follows blocks another piece drew
            if !inner.is_empty() || (!head && first && !t.after_band && !t.fresh) {
                inner.push(Row::default());
            }
            inner.extend(r);
        }
        if segs.len() == 1 && segs[0].1.is_empty() {
            return out;
        }
        if head || t.fresh {
            out.push(Row::default());
        }
        let n = segs.len();
        for (k, (h, inner)) in segs.into_iter().enumerate() {
            if let Some(h) = h {
                out.extend(band_rows(h, w));
            }
            if inner.is_empty() {
                continue;
            }
            let rows = inner
                .into_iter()
                .map(|r| {
                    let mut s = vec![sp(" ", Style::new())];
                    let body = fit(&r.spans, iw);
                    s.extend(if let Some(bg) = r.bg { tint(body, bg) } else { body });
                    s.push(sp(" ", Style::new()));
                    let hot = r.hot.iter().map(|&(a, b, k)| (a + 1, b + 1, k)).collect();
                    Row { spans: s, bg: None, pre: r.pre + 1, hot, ..r }
                })
                .collect();
            let mut card = slab(rows, BW, None, w);
            if !tail && k + 1 == n {
                card.pop();
            }
            out.extend(if head || k > 0 || t.after_band || t.fresh { card } else { card.split_off(1) });
        }
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
/// Cuts a path from the left, keeping the file name: "…/tests/serve_attach.rs".
fn left_cut(p: &str, w: usize) -> String {
    if p.width() <= w {
        return p.into();
    }
    if let Some((i, _)) = p.match_indices('/').find(|(i, _)| p[*i..].width() < w) {
        return format!("…{}", &p[i..]);
    }
    let cs: Vec<char> = p.chars().collect();
    format!("…{}", cs[cs.len().saturating_sub(w.saturating_sub(1))..].iter().collect::<String>())
}
fn cards(f: &Fold, now: i64, keys: &str, pw: usize) -> (Vec<Card>, Vec<Vec<Span<'static>>>) {
    let mut c = vec![];
    let mut short = vec![];
    let hit = if f.input + f.cache_read + f.cache_write > 0 { 100 * f.cache_read / (f.input + f.cache_read + f.cache_write) } else { 0 };
    let tps = if f.text_ms > 0 { f.text_out * 1000 / f.text_ms as u64 } else { 0 };
    let mode = if f.mode.is_empty() { "?".to_string() } else { f.mode.clone() };
    let down = f.mcp_down.first().map(|s| format!("✗ {s} down"));
    c.push(Card {
        title: vec![sp(f.cwd.clone(), bold()), t(), sp(mode.clone(), fg(ORANGE))],
        lines: vec![
            vec![sp("git ", dim()), sp(f.branch.clone(), fg(PURPLE))],
            vec![sp(f.model.clone(), fg(BLUE)), t(), sp(f.effort.clone(), fg(ORANGE)), sp(if f.thinking.is_empty() { "" } else { " ∴" }, dim())],
            [bar(f.ctx as f64 / HANDOFF_AT as f64, 17, CYAN), vec![sp("│", fg(ORANGE)), t(), sp(k(f.ctx), Style::new())]].concat(),
            vec![sp(format!("handoff {}", k(HANDOFF_AT)), dim()), t(), sp(format!("{}% of {}M", f.ctx * 100 / WINDOW, WINDOW / 1_000_000), dim())],
            vec![sp("in ", dim()), sp(k(f.input), Style::new()), sp(" out ", dim()), sp(k(f.output), Style::new()), t(), sp("cache ", dim()), sp(format!("{hit}%"), Style::new())],
            vec![sp(format!("${:.2}", f.cost), Style::new()), sp(format!(" · {tps} tok/s · {} turns", f.turns.len()), dim())],
            vec![sp("tools ", dim()), sp(f.tools.to_string(), Style::new()), t(), sp(down.clone().unwrap_or_default(), fg(RED))],
            vec![sp("keys ", dim()), t(), sp(keys.to_string(), Style::new())],
        ],
        cap: 0,
    });
    short.push(vec![sp(f.cwd.clone(), Style::new()), sp(" git ", dim()), sp(f.branch.clone(), fg(PURPLE))]);
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
        .map(|(p, (a, r))| {
            let counts = vec![sp(format!("+{a}"), fg(BLUE)), sp(format!(" {:>3}", format!("−{r}")), fg(RED))];
            let room = (pw).saturating_sub(5 + width(&counts) + 1);
            [vec![sp(left_cut(p, room), Style::new()), t()], counts].concat()
        })
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
            bar(0.65, pw.saturating_sub(7), BLUE),
            vec![sp("cursor monthly", dim()), t(), sp("1%", fg(RED))],
            bar(0.01, pw.saturating_sub(7), RED),
        ],
        cap: 0,
    });
    short.push(vec![sp("Q ", dim()), sp("claude 65%", Style::new()), sp(" · ", dim()), sp("cursor 1%", fg(RED))]);
    (c, short)
}
fn panel_rows(f: &Fold, now: i64, keys: &str, pw: usize, waiting: usize) -> Vec<Row> {
    let w = pw.saturating_sub(2);
    let mut out = vec![];
    // waiting sessions off screen (the rail is hidden) join the Session card when it
    // is drawn, and clicking the line brings the rail back
    for (ci, card) in cards(f, now, keys, pw).0.into_iter().enumerate() {
        let mut rows = vec![row([vec![sp("  ", Style::new())], fit(&card.title, w - 3), vec![sp(" ", Style::new())]].concat())];
        let body: Vec<_> = if card.cap > 0 { card.lines.into_iter().take(card.cap).collect() } else { card.lines };
        for (li, l) in body.into_iter().enumerate() {
            let mut r = row([vec![sp("  ", Style::new())], fit(&l, w - 3), vec![sp(" ", Style::new())]].concat());
            // the Session card's context bar and the line under it open the context breakdown
            if ci == 0 && (li == 2 || li == 3) {
                r.act = Some(Act::Context);
            }
            rows.push(r);
        }
        if ci == 0 && waiting > 0 {
            let line = [vec![sp("  ! ", fg(ORANGE).add_modifier(Modifier::BOLD))], fit(&[sp(format!("{waiting} waiting · rail hidden · show"), fg(ORANGE))], w - 7)].concat();
            rows.push(Row { spans: line, act: Some(Act::RailToggle), ..Default::default() });
        }
        out.extend(slab(rows, BC, Some(BP), w));
        out.push(Row::default());
    }
    out
}
fn status_rows(f: &Fold, now: i64, w: usize, keys: &str, rail_note: Option<String>) -> Vec<Row> {
    let mut rows: Vec<Vec<Span<'static>>> = vec![vec![sp(" ", Style::new())]];
    // the rail is hidden: its waiting count joins the status rows
    if let Some(n) = rail_note {
        rows[0].push(sp(n, fg(ORANGE)));
    }
    let mut ctx_at: Option<(usize, u16, u16)> = None;
    for (k, it) in cards(f, now, keys, PANEL as usize).1.into_iter().enumerate() {
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
        if k == 3 {
            // the context summary opens the context breakdown
            let x0 = width(cur) as u16;
            ctx_at = Some((rows.len() - 1, x0, x0 + width(&it) as u16));
        }
        let cur = rows.last_mut().unwrap();
        cur.extend(it);
    }
    rows.into_iter()
        .enumerate()
        .map(|(i, s)| Row { spans: s, bg: Some(BP), hot: ctx_at.filter(|c| c.0 == i).map(|c| (c.1, c.2, Act::Context)).into_iter().collect(), ..Default::default() })
        .collect()
}

// ============================================================ the session rail (#692)
// Fake sessions: the replayed fixture is session 1, the rest are hard-coded so the
// owner can judge the look. Clicking a row or pressing ⌥N moves the on-screen
// marker only; the conversation stays the replayed session.
const RAIL_A: u16 = 22;
/// Below this rail width a B card drops its model row and its context bar (keeping the
/// percentage), so the card still reads: rows 1, 2, 4 and spend-plus-percentage only.
const RAIL_COMPACT: usize = 30;
#[derive(Clone, Copy, PartialEq)]
enum SState {
    Working,
    Retrying,
    NeedsInput,
    Ready,
    Crashed,
}
struct Fake {
    name: &'static str,
    project: &'static str,
    wait: String,
    /// `model · thinking level`, replaced by `wait` while waiting
    model: String,
    branch: &'static str,
    ws: &'static str,
    spend: String,
    /// live spend in dollars, summed into the project header
    dollars: f64,
    /// context fill, 0-100
    ctx: u8,
    age: String,
    st: SState,
}
fn fake_sessions(f: &Fold, now: i64) -> Vec<Fake> {
    // session 1 is the replayed one: it waits when the fixture waits on its approval or question
    let (st0, wait0) = match f.pending.first() {
        None => (SState::Working, String::new()),
        Some(p) => match &p.what {
            Asking::Approval { tool, args, .. } => (SState::NeedsInput, format!("approval: {tool} {}", primary(args))),
            Asking::Form(qs) => (SState::NeedsInput, format!("question: {} question{}", qs.len(), if qs.len() == 1 { "" } else { "s" })),
        },
    };
    let ws0: &'static str = Box::leak(f.cwd.rsplit('/').next().unwrap_or("fiber").to_string().into_boxed_str());
    let branch0: &'static str = Box::leak(f.branch.clone().into_boxed_str());
    let model0 = if f.model.is_empty() { "opus-5-5 · high".into() } else { format!("{} · {}", short_model(&f.model), f.effort) };
    vec![
        Fake { name: "fix flaky lock test", project: "fiber", wait: wait0, model: model0, branch: branch0, ws: ws0, spend: format!("${:.2}", f.cost), dollars: f.cost, ctx: (f.ctx * 100 / WINDOW.max(1)) as u8, age: rail_age((now - f.turn_start).max(0)), st: st0 },
        Fake { name: "docs: rail spec", project: "fiber", wait: String::new(), model: "opus-5-5 · high".into(), branch: "docs/rail-692", ws: "fiber", spend: "$0.42".into(), dollars: 0.42, ctx: 27, age: "9m".into(), st: SState::Working },
        Fake { name: "review #688", project: "fiber", wait: "approval: shell cargo publish --dry-run".into(), model: "gpt-6.1-sol · medium".into(), branch: "review-688", ws: "fiber", spend: "$1.10".into(), dollars: 1.10, ctx: 64, age: "22m".into(), st: SState::NeedsInput },
        Fake { name: "bump ratatui", project: "fiber", wait: "question: 2 of 3 answered".into(), model: "gpt-6.1-sol · low".into(), branch: "deps/ratatui-0.30", ws: "fiber", spend: "$0.08".into(), dollars: 0.08, ctx: 41, age: "4m".into(), st: SState::NeedsInput },
        Fake { name: "lsp probe", project: "fiber", wait: String::new(), model: "grok-4.7-high · low".into(), branch: "spike/lsp", ws: "fiber", spend: "$0.05".into(), dollars: 0.05, ctx: 78, age: "1h".into(), st: SState::Crashed },
        Fake { name: "migrate every provider adapter to the new streaming contract", project: "pi-rig", wait: String::new(), model: "grok-4.7-high · max".into(), branch: "migrate/streaming", ws: "pi-rig", spend: "$2.31".into(), dollars: 2.31, ctx: 91, age: "31m".into(), st: SState::Working },
        Fake { name: "backfill embeddings", project: "pi-rig", wait: String::new(), model: "grok-4.7-high · high".into(), branch: "backfill/embed", ws: "pi-rig", spend: "$0.87".into(), dollars: 0.87, ctx: 55, age: "12m".into(), st: SState::Retrying },
        Fake { name: "rewrite onboarding tour", project: "beacon", wait: String::new(), model: "opus-5-5 · medium".into(), branch: "tour/rewrite", ws: "beacon", spend: "$0.35".into(), dollars: 0.35, ctx: 12, age: "47m".into(), st: SState::Ready },
    ]
}
/// Shortens a model id to its last segment, so the card's model row stays short.
fn short_model(m: &str) -> String {
    m.rsplit('/').next().unwrap_or(m).to_string()
}
/// Fake "N done" counts per project, in first-seen order.
fn rail_done() -> Vec<u32> {
    vec![2, 0, 1]
}
/// The rail's sessions with the person's resume overrides applied: a resumed crash
/// works again. Dismissed sessions are filtered where the shown set is built.
fn live_sessions(f: &Fold, ui: &Ui, now: i64) -> Vec<Fake> {
    let mut sess = fake_sessions(f, now);
    for &i in &ui.resumed {
        if let Some(s) = sess.get_mut(i) {
            s.st = SState::Working;
            s.wait = String::new();
        }
    }
    sess
}
/// The sessions in display order: stable by start time, so ⌥N numbers never move.
fn rail_seq(sess: &[Fake], _variant: u8) -> Vec<usize> {
    (0..sess.len()).collect()
}
/// A `!` needing input draws attention: a slow pulse between two colours on the
/// existing tick, so it costs no redraw of its own. Still when idle or reduced.
fn wait_glyph(tick: u64, reduced: bool) -> Style {
    if reduced {
        return fg(ORANGE).add_modifier(Modifier::BOLD);
    }
    if tick / 4 % 2 == 0 { fg(ORANGE).add_modifier(Modifier::BOLD) } else { fg(CYAN) }
}
fn rail_glyph(s: &Fake, tick: u64, reduced: bool) -> Span<'static> {
    match s.st {
        // the braille spinner animates on the existing tick, the same frames as the
        // working line; `●` under reduced motion
        SState::Working if reduced => sp("●", fg(BLUE)),
        SState::Working => sp(SPIN[tick as usize % SPIN.len()], fg(BLUE)),
        SState::Retrying if reduced => sp("●", fg(ORANGE)),
        SState::Retrying => sp(SPIN[tick as usize % SPIN.len()], fg(ORANGE)),
        SState::NeedsInput => sp("!", wait_glyph(tick, reduced)),
        SState::Ready => sp("✓", dim()),
        SState::Crashed => sp("✗", fg(RED).add_modifier(Modifier::BOLD)),
    }
}
/// The state word's style: bold in the state colour, dim for idle.
fn rail_state_style(s: &Fake, tick: u64, reduced: bool) -> Style {
    match s.st {
        SState::Working => fg(BLUE).add_modifier(Modifier::BOLD),
        SState::Retrying => fg(ORANGE).add_modifier(Modifier::BOLD),
        SState::NeedsInput => wait_glyph(tick, reduced),
        SState::Ready => dim(),
        SState::Crashed => fg(RED).add_modifier(Modifier::BOLD),
    }
}
fn rail_state_word(s: &Fake) -> &'static str {
    match s.st {
        SState::Working => "WORKING",
        SState::Retrying => "RETRYING",
        SState::NeedsInput => "NEEDS INPUT",
        SState::Ready => "READY",
        SState::Crashed => "CRASHED",
    }
}
/// The card's surface tint by state; the on-screen card is brighter (SEL) than the rest.
fn rail_card_bg(s: &Fake, on: bool) -> Color {
    if on {
        return SEL;
    }
    match s.st {
        SState::Working => BC,
        SState::Retrying => rgb(0x262014),
        SState::NeedsInput => rgb(0x241a10),
        SState::Ready => rgb(0x121217),
        SState::Crashed => rgb(0x201114),
    }
}
/// The stripe colour by state: the waiting stripe pulses with the glyph.
fn rail_stripe(s: &Fake, tick: u64, reduced: bool) -> Span<'static> {
    match s.st {
        SState::Working => sp("▌", fg(BLUE)),
        SState::Retrying => sp("▌", fg(ORANGE)),
        SState::NeedsInput => sp("▌", wait_glyph(tick, reduced)),
        SState::Ready => sp("▌", dim()),
        SState::Crashed => sp("▌", fg(RED)),
    }
}
/// The context bar's colour: normal under 60%, warning 60–85%, danger above.
fn rail_ctx_color(pct: u8) -> Color {
    if pct > 85 { RED } else if pct >= 60 { ORANGE } else { BLUE }
}
/// Cuts a name to `max` characters, ending in … so truncation shows.
fn cut(s: &str, max: usize) -> String {
    let cs: Vec<char> = s.chars().collect();
    if cs.len() <= max {
        return s.into();
    }
    cs[..max.saturating_sub(1)].iter().collect::<String>() + "…"
}
/// One row of the list (A): `1 ● name…`, the on-screen session on a tint with a stripe.
fn rail_row_a(s: &Fake, n: usize, i: usize, on: bool, tick: u64, reduced: bool, w: usize) -> Vec<Row> {
    let stripe = if on {
        sp("▌", fg(BLUE))
    } else if reduced && s.st == SState::NeedsInput {
        sp("▌", fg(ORANGE))
    } else {
        sp(" ", Style::new())
    };
    let line = vec![stripe, sp(format!("{n} "), dim()), rail_glyph(s, tick, reduced), sp(" ", Style::new()), sp(s.name.to_string(), Style::new())];
    let bg = on.then_some(SEL);
    let spans = match bg {
        Some(c) => tint(fit(&line, w), c),
        None => fit(&line, w),
    };
    let mut out = vec![Row { spans, bg, act: Some(Act::Rail(i)), ..Default::default() }];
    if s.st == SState::NeedsInput && !s.wait.is_empty() {
        let ws = match bg {
            Some(c) => tint(fit(&[sp("  ", Style::new()), sp(s.wait.clone(), dim())], w), c),
            None => fit(&[sp("  ", Style::new()), sp(s.wait.clone(), dim())], w),
        };
        out.push(Row { spans: ws, bg, act: Some(Act::Rail(i)), ..Default::default() });
    }
    out
}
/// Fits card content rows to the inner width (2 cells of right margin, matching the
/// stripe + space on the left), gives them the card's tint, and optionally wraps them
/// in ▄/▀ edges. Hot regions span the whole card width.
fn rail_card_rows(inner: Vec<Vec<Span<'static>>>, i: usize, bg: Color, w: usize, edged: bool) -> Vec<Row> {
    let iw = w.saturating_sub(2);
    let hot = vec![(0, w as u16, Act::Rail(i))];
    let rows: Vec<Row> = inner
        .into_iter()
        .map(|spans| {
            let mut fitted = fit(&spans, iw);
            fitted.push(sp("  ", Style::new()));
            Row { spans: fitted, bg: Some(bg), hot: hot.clone(), act: Some(Act::Rail(i)), ..Default::default() }
        })
        .collect();
    if edged { slab(rows, bg, Some(BP), w) } else { rows }
}
/// Row 1 shared by the compact cards: number, glyph (state colour, pulsing when
/// needing input), bold name, elapsed right-aligned. No state word; the glyph, stripe
/// and tint carry the state. A crashed card shows ✕ instead of the elapsed, to dismiss it.
fn rail_dense_top(s: &Fake, n: usize, tick: u64, reduced: bool, iw: usize) -> Vec<Span<'static>> {
    vec![
        rail_stripe(s, tick, reduced),
        sp(format!("{n} "), dim()),
        rail_glyph(s, tick, reduced),
        sp(" ", Style::new()),
        sp(cut(&s.name, iw.saturating_sub(5)), bold()),
        t(),
        rail_age_or_dismiss(s),
    ]
}
/// Row 1 for the full and medium cards: number, glyph, state word, elapsed right.
/// A crashed card shows ✕ instead of the elapsed, to dismiss it.
fn rail_word_top(s: &Fake, n: usize, tick: u64, reduced: bool) -> Vec<Span<'static>> {
    vec![
        rail_stripe(s, tick, reduced),
        sp(format!("{n} "), dim()),
        rail_glyph(s, tick, reduced),
        sp(" ", Style::new()),
        sp(rail_state_word(s), rail_state_style(s, tick, reduced)),
        t(),
        rail_age_or_dismiss(s),
    ]
}
/// A card row 1's right end: the coarse elapsed, or the dismiss ✕ on a crashed card.
fn rail_age_or_dismiss(s: &Fake) -> Span<'static> {
    if s.st == SState::Crashed {
        sp("✕", fg(RED).add_modifier(Modifier::BOLD))
    } else {
        sp(s.age.clone(), dim())
    }
}
/// Points a crashed card's row 1 at its dismiss ✕ instead of the whole-row marker
/// move; clicking elsewhere still moves the marker. `x1` is the ✕ cell's right edge.
fn rail_dismiss_hot(rows: &mut [Row], r1: usize, i: usize, w: usize, x1: u16) {
    rows[r1].act = None;
    rows[r1].hot = vec![(x1.saturating_sub(1), x1, Act::RailDismiss(i)), (0, w as u16, Act::Rail(i))];
}
/// The spend row: spend, a bar filling the rest, and the percentage; without the bar
/// spend and percentage only.
fn rail_spend_row(s: &Fake, tick: u64, reduced: bool, iw: usize, barred: bool) -> Vec<Span<'static>> {
    let stripe = rail_stripe(s, tick, reduced);
    let pad = sp("  ", Style::new());
    if !barred {
        return vec![stripe, pad, sp(s.spend.clone(), Style::new()), t(), sp(format!("{}%", s.ctx), dim())];
    }
    let pre = vec![stripe, pad, sp(format!("{} ", s.spend), Style::new())];
    let suf = vec![sp(format!("  {}%", s.ctx), dim())];
    let barw = iw.saturating_sub(width(&pre) + width(&suf));
    [pre, bar(s.ctx as f64 / 100.0, barw, rail_ctx_color(s.ctx)), suf].concat()
}
/// One rich card of the cards (B): a tinted surface with half-block edges, grouped under
/// project headers by the caller. Row 1 is number, glyph, state word and elapsed;
/// row 2 the name; row 3 the model, or what it waits on while waiting; row 4 the
/// workspace and branch; row 5 spend, a context bar filling the rest, and the
/// percentage. Below RAIL_COMPACT columns row 3 and the bar go, keeping the percentage.
fn rail_card_b(s: &Fake, n: usize, i: usize, on: bool, tick: u64, reduced: bool, w: usize) -> Vec<Row> {
    // a right inner margin equal to the left (the stripe + space), so right-aligned
    // items and cut names end 2 cells short of the card's edge: rows are fitted to
    // the inner width, then given their 2 trailing cells
    let iw = w.saturating_sub(2);
    let stripe = || rail_stripe(s, tick, reduced);
    let pad = || sp("  ", Style::new());
    let r1 = rail_word_top(s, n, tick, reduced);
    let r2 = vec![stripe(), pad(), sp(cut(&s.name, iw.saturating_sub(3)), bold())];
    let r3 = if s.st == SState::NeedsInput {
        vec![stripe(), pad(), sp(cut(&s.wait, iw.saturating_sub(3)), fg(ORANGE))]
    } else {
        vec![stripe(), pad(), sp(cut(&s.model, iw.saturating_sub(3)), dim())]
    };
    let r4 = vec![stripe(), pad(), sp(format!("⌂ {}  ", s.ws), Style::new()), sp("⎇ ", dim()), sp(cut(s.branch, iw.saturating_sub(6 + s.ws.width() + 4)), fg(BLUE))];
    let pct = format!("{}%", s.ctx);
    let r5 = if w < RAIL_COMPACT {
        vec![stripe(), pad(), sp(s.spend.clone(), Style::new()), t(), sp(pct, dim())]
    } else {
        let pre = vec![stripe(), pad(), sp(format!("{}   ", s.spend), Style::new())];
        let suf = vec![sp(format!("  {pct}"), dim())];
        let barw = iw.saturating_sub(width(&pre) + width(&suf));
        [pre, bar(s.ctx as f64 / 100.0, barw, rail_ctx_color(s.ctx)), suf].concat()
    };
    let mut inner = vec![r1, r2];
    if w >= RAIL_COMPACT {
        inner.push(r3);
    }
    inner.push(r4);
    inner.push(r5);
    let mut rows = rail_card_rows(inner, i, rail_card_bg(s, on), w, true);
    if s.st == SState::Crashed {
        rail_dismiss_hot(&mut rows, 1, i, w, iw as u16);
    }
    rows
}
/// The base model without its thinking level, which only the medium card shows.
fn rail_base_model(model: &str) -> &str {
    model.split(" · ").next().unwrap_or(model)
}
/// The branch row shared by the denser cards: `⎇ branch` with the branch in accent.
fn rail_branch_row(s: &Fake, tick: u64, reduced: bool, iw: usize) -> Vec<Span<'static>> {
    vec![
        rail_stripe(s, tick, reduced),
        sp("  ", Style::new()),
        sp("⎇ ", dim()),
        sp(cut(s.branch, iw.saturating_sub(5)), fg(BLUE)),
    ]
}
/// The medium card: 4 content rows with ▄/▀ edges. Below RAIL_COMPACT the bar goes,
/// keeping spend and percentage.
fn rail_card_medium(s: &Fake, n: usize, i: usize, on: bool, tick: u64, reduced: bool, w: usize) -> Vec<Row> {
    let inner = rail_medium_inner(s, n, tick, reduced, w.saturating_sub(2), w);
    let mut rows = rail_card_rows(inner, i, rail_card_bg(s, on), w, true);
    if s.st == SState::Crashed {
        rail_dismiss_hot(&mut rows, 1, i, w, w.saturating_sub(2) as u16);
    }
    rows
}
/// The 4 content rows medium shares with three's lower rows: word-top, name, wait reason or branch,
/// spend/bar/percentage (bar dropped below RAIL_COMPACT). `iw` is the content width.
fn rail_medium_inner(s: &Fake, n: usize, tick: u64, reduced: bool, iw: usize, w: usize) -> Vec<Vec<Span<'static>>> {
    let r2 = vec![rail_stripe(s, tick, reduced), sp("  ", Style::new()), sp(cut(&s.name, iw.saturating_sub(3)), bold())];
    let r3 = if s.st == SState::NeedsInput {
        vec![rail_stripe(s, tick, reduced), sp("  ", Style::new()), sp(cut(&s.wait, iw.saturating_sub(3)), fg(ORANGE))]
    } else {
        rail_branch_row(s, tick, reduced, iw)
    };
    vec![rail_word_top(s, n, tick, reduced), r2, r3, rail_spend_row(s, tick, reduced, iw, w >= RAIL_COMPACT)]
}
/// Row 1 for the three card: number, glyph and state word on the left; elapsed,
/// spend and context percentage right-aligned with the 2-cell margin, the percentage
/// coloured as the bar was. No bar. When it does not fit the elapsed goes first,
/// then the spend; the state word is never cut. On a crashed card ✕ sits at the far
/// right after the percentage and the elapsed is dropped.
fn rail_three_top(s: &Fake, n: usize, tick: u64, reduced: bool, iw: usize) -> Vec<Span<'static>> {
    let mut left = vec![
        rail_stripe(s, tick, reduced),
        sp(format!("{n} "), dim()),
        rail_glyph(s, tick, reduced),
        sp(" ", Style::new()),
        sp(rail_state_word(s), rail_state_style(s, tick, reduced)),
    ];
    let lw = width(&left);
    let pct = format!("{}%", s.ctx);
    // the right cluster, front-pinned: elapsed, spend, percentage; crashed swaps the
    // elapsed for a trailing ✕. Entries drop from the front, never past `min_keep`.
    let mut items: Vec<(String, Style)> = if s.st == SState::Crashed {
        vec![(s.spend.clone(), Style::new()), (pct, fg(rail_ctx_color(s.ctx))), ("✕".into(), fg(RED).add_modifier(Modifier::BOLD))]
    } else {
        vec![(s.age.clone(), dim()), (s.spend.clone(), Style::new()), (pct, fg(rail_ctx_color(s.ctx)))]
    };
    let min_keep = if s.st == SState::Crashed { 2 } else { 1 };
    while items.len() > min_keep && lw + items.iter().map(|(t, _)| 2 + t.width()).sum::<usize>() > iw {
        items.remove(0);
    }
    // double spaces, falling back to single when even that overflows
    let sep = if lw + items.iter().map(|(t, _)| 2 + t.width()).sum::<usize>() > iw { " " } else { "  " };
    for (t, st) in items {
        left.push(sp(sep, Style::new()));
        left.push(sp(t, st));
    }
    left
}
/// The three card: medium's rows with cost and context moved into row 1, both edges,
/// 3 content rows. Narrow as medium: nothing further drops, the row-1 rules cover it.
fn rail_card_three(s: &Fake, n: usize, i: usize, on: bool, tick: u64, reduced: bool, w: usize) -> Vec<Row> {
    let iw = w.saturating_sub(2);
    let mid = rail_medium_inner(s, n, tick, reduced, iw, w);
    let top = rail_three_top(s, n, tick, reduced, iw);
    let tx = width(&top) as u16;
    let inner = vec![top, mid[1].clone(), mid[2].clone()];
    let mut rows = rail_card_rows(inner, i, rail_card_bg(s, on), w, true);
    if s.st == SState::Crashed {
        rail_dismiss_hot(&mut rows, 1, i, w, tx);
    }
    rows
}
/// The compact card: 3 content rows, a flat tint with no edges, and one blank
/// untinted row between cards (added by the caller). Below RAIL_COMPACT the bar goes,
/// keeping spend and percentage.
fn rail_card_compact(s: &Fake, n: usize, i: usize, on: bool, tick: u64, reduced: bool, w: usize) -> Vec<Row> {
    let iw = w.saturating_sub(2);
    let r2 = if s.st == SState::NeedsInput {
        vec![rail_stripe(s, tick, reduced), sp("  ", Style::new()), sp(cut(&s.wait, iw.saturating_sub(3)), fg(ORANGE))]
    } else {
        let base = rail_base_model(&s.model);
        // the branch gives way first, so the model survives truncation
        let room = iw.saturating_sub(3 + 2 + 3 + base.width());
        vec![
            rail_stripe(s, tick, reduced),
            sp("  ", Style::new()),
            sp("⎇ ", dim()),
            sp(cut(s.branch, room), fg(BLUE)),
            sp(" · ", dim()),
            sp(base.to_string(), dim()),
        ]
    };
    let inner = vec![rail_dense_top(s, n, tick, reduced, iw), r2, rail_spend_row(s, tick, reduced, iw, w >= RAIL_COMPACT)];
    let mut rows = rail_card_rows(inner, i, rail_card_bg(s, on), w, false);
    if s.st == SState::Crashed {
        rail_dismiss_hot(&mut rows, 0, i, w, iw as u16);
    }
    rows
}
/// A project header: the name in the accent colour, bold, with the group's live spend
/// summed over its cards and a fake "N done" right-aligned.
/// A project header: the name in the accent colour, bold, a "+" chip starting a session
/// there, and the group's live spend summed over its cards with a fake "N done" opening
/// the project's session list. Click targets are found by scanning the fitted row.
fn rail_project_head(name: &'static str, dollars: f64, done: u32, w: usize) -> Row {
    let done_text = format!("{done} done");
    let mut spans = fit(
        &[
            sp(name.to_string(), fg(BLUE).add_modifier(Modifier::BOLD)),
            sp(" ", Style::new()),
            sp("+", Style::new().add_modifier(Modifier::BOLD).bg(SEL)),
            t(),
            sp(format!("${dollars:.2} · "), dim()),
            sp(done_text.clone(), dim()),
        ],
        w.saturating_sub(2),
    );
    spans.push(sp("  ", Style::new()));
    let (mut x, mut hot) = (0u16, vec![]);
    for s in &spans {
        let sw = s.content.width() as u16;
        if s.content == "+" {
            hot.push((x, x + sw, Act::RailNew(name)));
        } else if s.content == done_text.as_str() {
            hot.push((x, x + sw, Act::RailDone(name)));
        }
        x += sw;
    }
    Row { spans, hot, ..Default::default() }
}
/// The cards (B) design: every project shows, the launch project's group first and the
/// rest after in start order; numbers follow start order across groups. `density` is
/// 0 full, 1 medium, 2 compact.
fn rail_full_b(sess: &[Fake], shown: &[usize], screen: usize, tick: u64, reduced: bool, w: usize, density: u8) -> (Vec<Row>, Vec<(usize, usize, usize)>) {
    let mut out = vec![];
    // (session index, first row, row count), so a jump can scroll the card into view
    let mut map = vec![];
    let mut projects: Vec<&str> = vec![];
    for &i in shown {
        if !projects.contains(&sess[i].project) {
            projects.push(sess[i].project);
        }
    }
    // the launch project's group first, the rest in start order
    projects.sort_by_key(|p| if **p == *"fiber" { 0 } else { 1 });
    let done = rail_done();
    for (g, p) in projects.iter().enumerate() {
        if g > 0 {
            out.push(row(vec![sp(" ", Style::new())]));
        }
        let members: Vec<usize> = shown.iter().filter(|&&i| sess[i].project == *p).copied().collect();
        let dollars: f64 = members.iter().map(|&i| sess[i].dollars).sum();
        let di = projects.iter().position(|q| q == p).unwrap_or(0);
        out.push(rail_project_head(*p, dollars, done.get(di).copied().unwrap_or(0), w));
        for &i in &members {
            // numbers are stable per session: a dismissal leaves its number's gap
            let n = i + 1;
            let start = out.len();
            out.extend(match density {
                0 => rail_card_b(&sess[i], n, i, i == screen, tick, reduced, w),
                1 => rail_card_medium(&sess[i], n, i, i == screen, tick, reduced, w),
                2 => rail_card_three(&sess[i], n, i, i == screen, tick, reduced, w),
                _ => rail_card_compact(&sess[i], n, i, i == screen, tick, reduced, w),
            });
            map.push((i, start, out.len() - start));
            // compact cards carry no edges of their own: one blank untinted row between them
            if density == 3 {
                out.push(Row::default());
            }
        }
    }
    out.push(row(vec![sp(" ", Style::new())]));
    out.push(row(fit(&[sp("click moves marker", dim())], w)));
    out.push(rail_chips(&[("A", Act::RailVariant(0)), ("B", Act::RailVariant(1)), ("C", Act::RailVariant(2))], 1));
    out.push(rail_chips(&[("full", Act::RailDensity(0)), ("medium", Act::RailDensity(1)), ("three", Act::RailDensity(2)), ("compact", Act::RailDensity(3))], density as usize));
    (out, map)
}
/// Clickable chips for the rail's bottom label: small tinted buttons, the current one
/// highlighted. Function keys do not reach every shell, so the rail switches by click.
fn rail_chips(opts: &[(&'static str, Act)], cur: usize) -> Row {
    let mut parts: Vec<(Span<'static>, Option<Act>)> = vec![];
    for (k, (name, act)) in opts.iter().enumerate() {
        if k > 0 {
            parts.push((sp(" ", Style::new()), None));
        }
        let st = if k == cur { Style::new().fg(Color::Black).bg(ORANGE).add_modifier(Modifier::BOLD) } else { dim() };
        parts.push((sp(name.to_string(), st), Some(*act)));
    }
    hot_row(parts)
}
/// Moves the rail's scroll so the session's card is on screen; a no-op for A.
fn rail_ensure(ui: &mut Ui, lay: &(usize, usize, Vec<(usize, usize, usize)>), i: usize) {
    let (len, vis, map) = lay;
    let max_top = len.saturating_sub(*vis);
    for &(si, srow, slen) in map.iter() {
        if si == i {
            if srow < ui.rail_top {
                ui.rail_top = srow;
            } else if srow + slen > ui.rail_top + vis {
                ui.rail_top = (srow + slen).saturating_sub(*vis);
            }
            ui.rail_top = ui.rail_top.min(max_top);
        }
    }
}
/// The footer's shared rows: the scope toggle, the marker hint, and the variant chips.
fn rail_foot(hidden: usize, others: bool, w: usize) -> Vec<Row> {
    let mut out = vec![];
    if hidden > 0 {
        out.push(hot_row(vec![(sp(format!("+{hidden} other · show all"), dim()), Some(Act::RailAll))]));
    } else if others {
        out.push(hot_row(vec![(sp("show less", dim()), Some(Act::RailAll))]));
    }
    out.push(row(fit(&[sp("click moves marker", dim())], w)));
    out.push(rail_chips(&[("A", Act::RailVariant(0)), ("B", Act::RailVariant(1)), ("C", Act::RailVariant(2))], 0));
    out
}
/// The full-height list (A) design, `w` columns wide; other projects hide behind the scope line.
fn rail_full(sess: &[Fake], shown: &[usize], screen: usize, tick: u64, reduced: bool, w: usize, hidden: usize) -> Vec<Row> {
    let mut out = vec![];
    let each = |out: &mut Vec<Row>, ix: &[usize]| {
        for &i in ix {
            // numbers are stable per session: a dismissal leaves its number's gap
            let n = i + 1;
            let on = i == screen;
            out.extend(rail_row_a(&sess[i], n, i, on, tick, reduced, w));
        }
    };
    // own sessions first; sessions in other projects under a divider when shown
    let own: Vec<usize> = shown.iter().filter(|&&i| sess[i].project == "fiber").copied().collect();
    let autod: Vec<usize> = shown.iter().filter(|&&i| sess[i].project != "fiber").copied().collect();
    each(&mut out, &own);
    if !autod.is_empty() {
        out.push(row(fit(&[sp("─ other projects ─", dim())], w)));
        each(&mut out, &autod);
    }
    out.extend(rail_foot(hidden, !autod.is_empty(), w));
    out
}
/// The tabs (C): a one-row strip across the top of the conversation column.
fn rail_tabs(sess: &[Fake], shown: &[usize], screen: usize, cw: usize, tick: u64, reduced: bool, hidden: usize) -> Row {
    // the variant chips are always kept, so C can be left by click; tabs take what is left
    let room = cw.saturating_sub(10);
    let mut keep;
    let mut maxn = 18usize;
    loop {
        let mut w = 0;
        keep = shown.len();
        for (k, &i) in shown.iter().enumerate() {
            // `n ◐ name`, plus ` │ ` between tabs
            let tw = 4 + cut(sess[i].name, maxn).width() + if k > 0 { 3 } else { 0 };
            if w + tw <= room {
                w += tw;
            } else {
                keep = k;
                break;
            }
        }
        if keep == shown.len() || maxn <= 6 {
            break;
        }
        maxn -= 2;
    }
    let mut parts: Vec<(Span<'static>, Option<Act>)> = vec![];
    for (k, &i) in shown.iter().enumerate().take(keep) {
        if k > 0 {
            parts.push((sp(" │ ", dim()), None));
        }
        let s = &sess[i];
        let on = i == screen;
        let mut g = rail_glyph(s, tick, reduced);
        let mut nm = if s.st == SState::NeedsInput && !on { fg(ORANGE) } else { Style::new() };
        let mut nn = dim();
        if on {
            g = Span::styled(g.content, g.style.bg(SEL));
            nm = nm.bg(SEL);
            nn = nn.bg(SEL);
        }
        parts.push((sp(format!("{} ", i + 1), nn), Some(Act::Rail(i))));
        parts.push((g, Some(Act::Rail(i))));
        parts.push((sp(format!(" {}", cut(s.name, maxn)), nm), Some(Act::Rail(i))));
    }
    if keep < shown.len() {
        parts.push((sp(" │ ", dim()), None));
        parts.push((sp(format!("+{} ›", shown.len() - keep), dim()), None));
    }
    if hidden > 0 {
        parts.push((sp(" │ ", dim()), None));
        parts.push((sp(format!("+{hidden} ›"), dim()), Some(Act::RailAll)));
    }
    parts.push((sp(" │ ", dim()), None));
    for (k, (name, v)) in [("A", 0u8), ("B", 1), ("C", 2)].iter().enumerate() {
        if k > 0 {
            parts.push((sp(" ", dim()), None));
        }
        let st = if *v == 2 { Style::new().fg(Color::Black).bg(ORANGE).add_modifier(Modifier::BOLD) } else { dim() };
        parts.push((sp(name.to_string(), st), Some(Act::RailVariant(*v))));
    }
    let r = hot_row(parts);
    Row { spans: fit(&r.spans, cw), hot: r.hot, act: None, ..Default::default() }
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
// ============================================================ what the person has open
#[derive(Default)]
struct Search {
    q: String,
    i: usize,
    /// (conversation row, first cell, width in cells)
    hits: Vec<(usize, usize, usize)>,
    jump: bool,
}
#[derive(Default)]
struct Form {
    rid: String,
    tab: usize,
    cur: usize,
    sel: Vec<Vec<String>>,
    text: Vec<String>,
    note: String,
}
#[derive(Clone, Copy)]
struct Sel {
    a: (usize, usize),
    b: (usize, usize),
    moved: bool,
    down: bool,
}
#[derive(Default)]
struct Ui {
    input: String,
    /// the draft put away while a queued steering message is being edited
    draft: String,
    qsel: Option<usize>,
    search: Option<Search>,
    sel: Option<Sel>,
    drag_edge: i8,
    aside: HashSet<String>,
    shown: usize,
    choice: usize,
    feedback: String,
    form: Form,
    flash: Vec<String>,
    flash_at: Option<Instant>,
    /// the last copy's confirmation, floated in the conversation's top-right corner so nothing moves
    copied: Option<(String, Instant)>,
    keys: String,
    cmd_n: u32,
    /// the context breakdown is swapped into the conversation area
    ctx_view: bool,
    /// the model picker is swapped into the conversation area
    picker: Option<model_picker::State>,
    /// the `/` or `@` completion panel above the input box
    completions: Option<completions::State>,
    /// the view's own scroll, rows from its top
    vscroll: usize,
    /// the rail design: 0 list, 1 cards, 2 tabs (`--rail`, or the A/B/C chips)
    rail: u8,
    /// the B card density: 0 full, 1 medium, 2 compact (`--density`, or the chips)
    density: u8,
    /// the person shut the rail (dragged shut, ⌥R): it stays shut until dragged out or toggled
    rail_off: bool,
    /// the rail shows sessions in other projects (design A; B always shows every project)
    show_all: bool,
    /// the session on screen, by its index: a click or ⌥N moves this only
    screen: usize,
    /// crashed sessions the person dismissed: their cards are gone, their numbers gap
    dismissed: HashSet<usize>,
    /// crashed sessions the person resumed: they show WORKING again
    resumed: HashSet<usize>,
    /// the rail's width as a share of the window (`--rail-share`), 15% clamped to [22, 48]
    rail_share: f64,
    /// the panel's width as a share of the window (`--panel-share`), 21% clamped to [30, 60]
    panel_share: f64,
    /// the rail's first visible row, when its cards overflow the screen
    rail_top: usize,
    /// a resize drag in progress: 0 the rail's right edge, 1 the panel's left edge
    resize: Option<u8>,
    /// the person answered an approval or a form since the last input was handled
    answered: bool,
}
impl Ui {
    /// The request on top: the first in arrival order not put aside, or the one clicked through to.
    fn top<'a>(&self, f: &'a Fold) -> Option<(usize, &'a Pending)> {
        let open: Vec<(usize, &Pending)> = f.pending.iter().enumerate().filter(|(_, p)| !self.aside.contains(&p.rid)).collect();
        (!open.is_empty()).then(|| open[self.shown % open.len()])
    }
    fn nothing_open(&self, f: &Fold) -> bool {
        self.search.is_none() && self.qsel.is_none() && !self.ctx_view && self.completions.is_none() && self.top(f).is_none()
    }
    /// Starts a fresh form state when the form on top changes.
    fn sync_form(&mut self, f: &Fold) {
        if let Some((_, p)) = self.top(f) {
            if let Asking::Form(fields) = &p.what {
                if self.form.rid != p.rid {
                    self.form = Form { rid: p.rid.clone(), sel: vec![vec![]; fields.len()], text: vec![String::new(); fields.len()], ..Default::default() };
                }
            }
        }
    }
    fn answer_of(&self, k: usize) -> Option<String> {
        let mut v = self.form.sel.get(k)?.clone();
        let t = self.form.text.get(k)?;
        if !t.is_empty() {
            v.push(format!("“{t}”"));
        }
        (!v.is_empty()).then(|| v.join(", "))
    }
}

fn primary(args: &Value) -> String {
    ["command", "path", "url"].iter().find_map(|k| args[k].as_str()).map_or_else(|| args.to_string(), str::to_string)
}

fn input_box(ui: &Ui, w: usize) -> Vec<Row> {
    let editing = ui.qsel.is_some();
    let room = w.saturating_sub(40);
    let shown: String = if ui.input.width() > room {
        let cs: Vec<char> = ui.input.chars().collect();
        format!("…{}", cs[cs.len().saturating_sub(room)..].iter().collect::<String>())
    } else {
        ui.input.clone()
    };
    // while search is open typing goes to the search box, so the draft shows without a cursor
    let cursor = if ui.search.is_none() { "█" } else { "" };
    let mut line = vec![sp("▌", fg(if editing { ORANGE } else { BLUE })), sp(" ", Style::new()), sp("› ", fg(CYAN)), sp(shown, Style::new()), sp(cursor, dim())];
    if editing {
        line.extend([t(), sp("editing a queued message · enter amends · ⌥x drops · esc stops ", dim())]);
    }
    slab(vec![row(line)], BI, None, w)
}

/// The search box's width, floating over the conversation's top-right corner.
const SBOX_W: usize = 40;
fn search_box(s: &Search) -> Vec<Span<'static>> {
    let status = if s.q.is_empty() {
        sp("type to search", dim())
    } else if s.hits.is_empty() {
        sp("no matches", dim())
    } else {
        sp(format!("{} of {}", s.i + 1, s.hits.len()), Style::new())
    };
    vec![sp(" ⌕ ", fg(ORANGE)), sp(s.q.clone(), bold()), sp("█", dim()), t(), status, sp(" ", Style::new())]
}

/// A swapped view's header row: its name, and Esc back to the conversation.
fn view_header(name: &str, cmd: &str) -> Row {
    Row { spans: vec![sp(" ", Style::new()), sp(name.to_string(), bold()), sp(format!("  {cmd}"), dim()), t(), sp("esc returns ", dim())], bg: Some(BI), act: Some(Act::Back), ..Default::default() }
}

/// The context breakdown: one bar of context by category against the handoff point, then
/// the largest tool results. The stream has each part's text but no token count per part,
/// so each part is estimated at 4 characters a token (see README, findings).
fn context_view(f: &Fold, w: usize) -> Vec<Row> {
    let tok = |chars: usize| chars as u64 / 4;
    let (mut you, mut replies, mut reasoning, mut calls, mut results) = (0, 0, 0, 0, 0);
    let mut largest: Vec<(String, u64)> = vec![];
    // only what follows the last completed handoff is in context
    let cut = f
        .turns
        .iter()
        .enumerate()
        .flat_map(|(ti, t)| t.blocks.iter().enumerate().filter(|(_, b)| matches!(b, Block::Handoff(h) if h.outcome.as_deref() == Some("completed"))).map(move |(bi, _)| (ti, bi)))
        .last();
    for (ti, t) in f.turns.iter().enumerate() {
        if cut.is_none_or(|c| ti > c.0) {
            you += t.prompt.len();
        }
        for (bi, b) in t.blocks.iter().enumerate() {
            if cut.is_some_and(|c| (ti, bi) <= c) {
                continue;
            }
            match b {
                Block::Text(x) => replies += x.len(),
                Block::Steer(x, _) => you += x.len(),
                Block::Group(g) => {
                    for it in &g.items {
                        match it {
                            Item::R(r) => reasoning += r.text.len(),
                            Item::C(c) => {
                                calls += c.name.len() + c.args.to_string().len();
                                results += c.content.len();
                                largest.push((format!("{} {}", kind_of(c), target(c)), tok(c.content.len())));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut cats: Vec<(&str, u64, Color)> = vec![
        ("system prompt", tok(f.sys_chars), BLUE),
        ("tool definitions", tok(f.tooldef_chars), CYAN),
        ("instruction files", tok(f.instr_chars), PURPLE),
        ("skills listing", tok(f.skills_chars), rgb(0xc9a8ff)),
        ("your messages", tok(you), rgb(0xd4d4d4)),
        ("replies", tok(replies), rgb(0x4a7fd0)),
        ("reasoning", tok(reasoning), rgb(0x808080)),
        ("tool calls", tok(calls), ORANGE),
        ("tool results", tok(results), RED),
    ];
    let known: u64 = cats.iter().map(|c| c.1).sum();
    // usage reports the whole context; what the text above does not account for
    cats.push(("not attributed", f.ctx.saturating_sub(known), rgb(0x5a5a6a)));
    let total = f.ctx.max(known);
    let bw = w.saturating_sub(6);
    let mut bar = vec![sp("  ", Style::new())];
    let mut used = 0;
    for &(_, n, c) in &cats {
        if n == 0 || used >= bw {
            continue;
        }
        let cells = ((n as f64 / HANDOFF_AT as f64 * bw as f64).round() as usize).max(1).min(bw - used);
        bar.push(sp("█".repeat(cells), fg(c)));
        used += cells;
    }
    bar.push(sp("▁".repeat(bw - used), fg(SEL)));
    bar.push(sp("│", fg(ORANGE)));
    let hk = format!("handoff {}", k(HANDOFF_AT));
    let mut out = vec![
        Row::default(),
        row(vec![sp("  ", Style::new()), sp(format!("{} tokens", k(total)), bold()), sp(format!(" · {}% of the {}M window · automatic handoff at {}", total * 100 / WINDOW, WINDOW / 1_000_000, k(HANDOFF_AT)), dim())]),
        Row::default(),
        row(bar),
        row(vec![sp(" ".repeat((bw + 3).saturating_sub(hk.width())), Style::new()), sp(hk, fg(ORANGE))]),
        Row::default(),
    ];
    for &(name, n, c) in &cats {
        let est = if name == "not attributed" { " " } else { "~" };
        out.push(row(vec![sp("  ■ ", fg(c)), sp(format!("{name:<20}"), Style::new()), sp(format!("{est}{:>7}", k(n)), Style::new()), sp(format!("{:>6}", format!("{}%", n * 100 / total.max(1))), dim())]));
    }
    out.extend([Row::default(), row(vec![sp("  Largest tool results", bold())])]);
    largest.sort_by_key(|x| std::cmp::Reverse(x.1));
    let tw = w.saturating_sub(14).min(60);
    for (what, n) in largest.into_iter().take(5) {
        out.push(row([vec![sp("  ", Style::new())], fit(&[sp(what, dim())], tw), vec![sp(format!("~{:>7}", k(n)), Style::new())]].concat()));
    }
    out.push(Row::default());
    for l in ["~ is an estimate at 4 characters a token from the text in the event stream; usage reports only the total.", "Deferred tools are not counted until loaded."] {
        out.extend(wrap_rows(vec![sp(l, dim())], w, vec![sp("  ", Style::new())], vec![sp("  ", Style::new())]));
    }
    out
}

const CH_ONCE: usize = 0;
const CH_SESSION: usize = 1;
const CH_PROJECT: usize = 2;
const CH_DENY: usize = 3;
/// The choices an approval panel shows: the two that remember need a `rule` on the request.
fn choices(has_rule: bool) -> Vec<usize> {
    if has_rule { vec![CH_ONCE, CH_SESSION, CH_PROJECT, CH_DENY] } else { vec![CH_ONCE, CH_DENY] }
}
fn approval_panel(f: &Fold, ui: &Ui, k: usize, p: &Pending, w: usize) -> Vec<Row> {
    let Asking::Approval { tool, args, effects, reversible, paths, step, req } = &p.what else { return vec![] };
    let arg = primary(args);
    let (rule, standing_rule, escalation) = (req.get("rule"), req.get("standing_rule"), req.get("escalation"));
    let why = if let Some(r) = standing_rule {
        format!("asked by the {} rule {}", r["scope"].as_str().unwrap_or(""), r["prefix"].as_str().unwrap_or(""))
    } else if let Some(e) = escalation {
        let reason = e["reason"].as_str().map_or(String::new(), |r| format!(": {r}"));
        match e["cause"].as_str().unwrap_or("") {
            "reviewer_failed" => "the reviewer failed".to_string(),
            "session_blocks" => format!("the reviewer has blocked too many calls{reason}"),
            _ => format!("the reviewer blocked the last calls{reason}"),
        }
    } else {
        match step.as_str() {
            "standing_ask" => "a standing ask rule matches it",
            "readonly" => "it would leave readonly",
            _ => "",
        }
        .to_string()
    };
    let mut rows = vec![hot_row(vec![
        (sp(format!("Approval {} of {}", k + 1, f.pending.len()), fg(ORANGE).add_modifier(Modifier::BOLD)), Some(Act::NextPending)),
        (sp(format!(" · {}", f.asker(&p.sid)), fg(ORANGE)), None),
        (t(), None),
        (sp("esc puts it aside", dim()), None),
    ])];
    rows.extend(wrap_rows(vec![sp(format!("{tool} "), fg(CYAN).add_modifier(Modifier::BOLD)), sp(arg.clone(), bold())], w - 4, vec![], vec![]));
    let mut facts = effects.join(", ");
    if !reversible {
        facts += " · not reversible";
    }
    if !paths.is_empty() {
        facts += &format!(" · {}", paths.join(", "));
    }
    if !why.is_empty() {
        facts += &format!(" · {why}");
    }
    rows.push(row(vec![sp(facts, dim())]));
    rows.push(Row::default());
    let choice = |i: usize, n: usize, parts: Vec<Span<'static>>| {
        let on = ui.choice == i;
        let mut v = vec![(sp(if on { "▸ " } else { "  " }, fg(ORANGE)), Some(Act::Choice(i))), (sp(format!("{} ", n + 1), dim()), Some(Act::Choice(i)))];
        v.extend(parts.into_iter().map(|s| (if on { Span::styled(s.content, s.style.add_modifier(Modifier::BOLD)) } else { s }, Some(Act::Choice(i)))));
        hot_row(v)
    };
    let prefix = rule.and_then(|r| r["prefix"].as_str()).unwrap_or("");
    for (n, i) in choices(rule.is_some()).into_iter().enumerate() {
        let parts = match i {
            CH_ONCE => vec![sp("Allow once", Style::new())],
            CH_SESSION => vec![sp("Allow for this session ", Style::new()), sp(format!("{prefix} *"), fg(CYAN))],
            CH_PROJECT => vec![sp("Always allow in this project ", Style::new()), sp(format!("{prefix} *"), fg(CYAN))],
            _ => {
                let fb = if ui.feedback.is_empty() && ui.choice != CH_DENY { sp("  type to add feedback", dim()) } else { sp(format!("  {}█", ui.feedback), Style::new()) };
                vec![sp("Deny", Style::new()), fb]
            }
        };
        rows.push(choice(i, n, parts));
    }
    rows.push(Row::default());
    rows.push(row(vec![sp("↑↓ choose · enter confirms · typing goes to the feedback · click a choice", dim())]));
    let rows = rows.into_iter().map(|r| prefixed(r, vec![sp("▌", fg(ORANGE)), sp(" ", Style::new())])).collect();
    slab(rows, BC, None, w)
}

fn form_panel(f: &Fold, ui: &Ui, k: usize, p: &Pending, fields: &[Value], w: usize) -> Vec<Row> {
    let n = fields.len();
    let fm = &ui.form;
    let iw = w - 4;
    let mut title = vec![
        (sp("Question", fg(PURPLE).add_modifier(Modifier::BOLD)), None),
        (sp(format!(" from {} · {n} question{}, any can be skipped", f.asker(&p.sid), if n == 1 { "" } else { "s" }), fg(PURPLE)), None),
    ];
    if f.pending.len() > 1 {
        title.push((sp(format!(" · {} of {}", k + 1, f.pending.len()), fg(PURPLE).add_modifier(Modifier::BOLD)), Some(Act::NextPending)));
    }
    title.extend([(t(), None), (sp("esc: chat about this", dim()), None)]);
    let mut rows = vec![hot_row(title), Row::default()];
    let mut tabs = vec![(sp("← ", dim()), None)];
    for (j, q) in fields.iter().enumerate() {
        let done = ui.answer_of(j).is_some();
        let st = if fm.tab == j { Style::new().add_modifier(Modifier::REVERSED) } else if done { fg(BLUE) } else { Style::new() };
        tabs.push((sp(format!(" {}{} ", if done { "✔ " } else { "☐ " }, q["header"].as_str().unwrap_or("")), st), Some(Act::Tab(j))));
        tabs.push((sp(" ", Style::new()), None));
    }
    tabs.push((sp(" ✔ Submit ", if fm.tab == n { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() }), Some(Act::Tab(n))));
    tabs.push((sp(" →", dim()), None));
    rows.extend([hot_row(tabs), Row::default()]);
    if let Some(q) = fields.get(fm.tab) {
        let multi = q["multiSelect"].as_bool().unwrap_or(false);
        let opts = q["options"].as_array().cloned().unwrap_or_default();
        rows.extend(wrap_rows(vec![sp(q["question"].as_str().unwrap_or("").to_string(), bold())], iw, vec![], vec![]));
        if multi {
            rows.push(row(vec![sp("choose any", dim())]));
        }
        rows.push(Row::default());
        for (j, o) in opts.iter().enumerate() {
            let label = o["label"].as_str().unwrap_or("").to_string();
            let (cur, chosen) = (fm.cur == j, fm.sel[fm.tab].contains(&label));
            let bx = match (multi, chosen) {
                (true, true) => "[x] ",
                (true, false) => "[ ] ",
                (false, true) => "(•) ",
                (false, false) => "( ) ",
            };
            let mut ls = Style::new();
            if chosen {
                ls = ls.add_modifier(Modifier::BOLD);
            }
            if cur {
                ls = ls.add_modifier(Modifier::UNDERLINED);
            }
            let mut v = vec![
                (sp(if cur { "▸ " } else { "  " }, fg(PURPLE)), Some(Act::Opt(j))),
                (sp(format!("{} ", j + 1), dim()), Some(Act::Opt(j))),
                (sp(bx, if chosen { fg(PURPLE) } else { dim() }), Some(Act::Opt(j))),
                (sp(label, ls), Some(Act::Opt(j))),
            ];
            if let Some(d) = o["description"].as_str() {
                v.push((sp(format!("  {d}"), dim()), Some(Act::Opt(j))));
            }
            rows.push(hot_row(v));
        }
        let j = opts.len();
        let cur = fm.cur == j;
        let txt = &fm.text[fm.tab];
        let body = if !txt.is_empty() {
            sp(txt.clone(), bold())
        } else {
            sp(if opts.is_empty() { "Type an answer" } else { "Type an answer, alone or with the options" }, dim())
        };
        rows.push(hot_row(vec![
            (sp(if cur { "▸ " } else { "  " }, fg(PURPLE)), Some(Act::Opt(j))),
            (sp(format!("{} ", j + 1), dim()), Some(Act::Opt(j))),
            (sp("✎ ", dim()), Some(Act::Opt(j))),
            (body, Some(Act::Opt(j))),
            (sp(if cur { "█" } else { "" }, dim()), None),
        ]));
        if multi {
            // space toggles, so moving on is a row of its own
            let on = fm.cur == j + 1;
            let lab = if fm.tab + 1 == n { "Review →" } else { "Next →" };
            rows.push(hot_row(vec![(sp(if on { "▸ " } else { "  " }, fg(PURPLE)), Some(Act::Next)), (sp(lab, if on { fg(PURPLE).add_modifier(Modifier::BOLD) } else { fg(PURPLE) }), Some(Act::Next))]));
        }
        rows.push(Row::default());
        rows.push(row(vec![sp("←→ question · ↑↓ choose · enter chooses and moves on · space toggles · type to answer in words", dim())]));
    } else {
        rows.extend([row(vec![sp("Review", bold())]), Row::default()]);
        for (j, q) in fields.iter().enumerate() {
            let a = ui.answer_of(j);
            rows.push(hot_row(vec![
                (sp("  ", Style::new()), None),
                (sp(format!("{:<13}", q["header"].as_str().unwrap_or("")), fg(BLUE)), Some(Act::Tab(j))),
                (sp(a.clone().unwrap_or("skipped".into()), if a.is_some() { bold() } else { dim() }), Some(Act::Tab(j))),
            ]));
        }
        let note = if fm.note.is_empty() { sp("Add a note on the whole form", dim()) } else { sp(fm.note.clone(), bold()) };
        rows.push(row(vec![sp("▸ ", fg(PURPLE)), sp(format!("{:<13}", "note"), fg(BLUE)), note, sp("█", dim())]));
        rows.push(Row::default());
        rows.push(hot_row(vec![
            (sp("  ", Style::new()), None),
            (sp(" Submit ", Style::new().add_modifier(Modifier::REVERSED)), Some(Act::Submit)),
            (sp("   ", Style::new()), None),
            (sp(" Chat about this ", Style::new().bg(SEL)), Some(Act::Decline)),
            (sp("  declines and ends the turn, so you can answer in your own words", dim()), None),
        ]));
        rows.push(Row::default());
        rows.push(row(vec![sp("←→ question · enter submits · type to add the note · esc: chat about this", dim())]));
    }
    let rows = rows.into_iter().map(|r| prefixed(r, vec![sp("▌", fg(PURPLE)), sp(" ", Style::new())])).collect();
    slab(rows, BC, None, w)
}

fn bottom(f: &Fold, w: usize, tick: u64, now: i64, v: &View, narrow: bool, ui: &Ui, rail_note: Option<String>) -> Vec<Row> {
    let mut out = vec![];
    let aside = f.pending.iter().filter(|p| ui.aside.contains(&p.rid)).count();
    if aside > 0 {
        let s = if aside == 1 { "" } else { "s" };
        out.push(hot_row(vec![(sp("  ⚠ ", fg(ORANGE)), None), (sp(format!("{aside} approval{s} waiting · click to reopen"), fg(ORANGE).add_modifier(Modifier::BOLD)), Some(Act::Reopen))]));
    }
    for (i, (code, msg, gone)) in f.notices.iter().enumerate() {
        if !gone {
            out.push(Row { spans: vec![sp("  ⚠ ", fg(ORANGE)), sp(code.clone(), fg(ORANGE)), sp(format!(" · {msg}"), dim()), t(), sp("✕ ", dim())], act: Some(Act::Notice(i)), ..Default::default() });
        }
    }
    for m in &ui.flash {
        out.push(row(vec![sp("  ", Style::new()), sp(m.clone(), fg(CYAN).add_modifier(Modifier::DIM))]));
    }
    if f.running {
        let spin = if v.reduced { "●" } else { SPIN[tick as usize % SPIN.len()] };
        let mut s = vec![sp("  ", Style::new()), sp(format!("{spin} "), fg(ORANGE))];
        s.extend(glimmer("Working", tick, v.reduced));
        // Esc interrupts only when nothing is open, so the hint shows only then
        let hint = if ui.nothing_open(f) { " · esc to interrupt" } else { "" };
        s.push(sp(format!(" {}{hint}", dur(now - f.turn_start)), dim()));
        out.push(row(s));
    }
    if !f.queue.is_empty() {
        out.push(row(vec![sp("  • Steering, joins the turn at the next step", dim())]));
        for (i, (_, q)) in f.queue.iter().enumerate() {
            let on = ui.qsel == Some(i);
            out.push(hot_row(vec![
                (sp(if on { "    ▸ " } else { "    ↳ " }, if on { fg(ORANGE) } else { dim() }), Some(Act::QRow(i))),
                (sp(q.clone(), if on { Style::new() } else { dim() }), Some(Act::QRow(i))),
                (sp("  ✕", dim()), Some(Act::QDrop(i))),
            ]));
        }
        out.push(row(vec![sp("      ⌥↑ edit · ⌥↓ next · ⌥x drop · click a row to edit, ✕ to drop", dim())]));
    }
    // the `/` and `@` completion panels sit above the input box, over the conversation's bottom
    if let Some(c) = ui.completions.as_ref() {
        out.extend(completions::view(c, &ui.input, w));
    }
    // search floats over the conversation, so the input box keeps its place and its draft
    let mut ib = match ui.top(f) {
        Some((k, p)) => match &p.what {
            Asking::Approval { .. } => approval_panel(f, ui, k, p, w),
            Asking::Form(fields) => form_panel(f, ui, k, p, fields, w),
        },
        None => input_box(ui, w),
    };
    out.append(&mut ib);
    if narrow {
        out.extend(status_rows(f, now, w, &ui.keys, rail_note));
    }
    out
}

// ============================================================ reading and copying
/// The text of the chars that start in cells [from, to).
fn cells(s: &str, from: usize, to: usize) -> String {
    let mut x = 0;
    let mut out = String::new();
    for ch in s.chars() {
        if x >= from && x < to {
            out.push(ch);
        }
        x += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
    }
    out
}
/// The text between two (row, cell) points, unwrapped: a row that continues a
/// wrapped line joins it with a space, layout cells and surface edges are left out.
fn selection_text(rows: &[Row], a: (usize, usize), b: (usize, usize)) -> String {
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    let mut out = String::new();
    let mut first = true;
    for r in a.0..=b.0.min(rows.len().saturating_sub(1)) {
        let row = &rows[r];
        let t = plain(row);
        let tt = t.trim();
        if !tt.is_empty() && tt.chars().all(|c| c == '▄' || c == '▀') {
            continue;
        }
        let from = if r == a.0 { a.1 } else { 0 }.max(row.pre as usize);
        let to = if r == b.0 { b.1 + 1 } else { usize::MAX };
        let piece = cells(&t, from, to);
        if !first {
            out.push(if row.cont { ' ' } else { '\n' });
        }
        first = false;
        out.push_str(piece.trim_end_matches([' ', '▐', '▌']));
    }
    out
}
fn b64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            s.push(if i <= c.len() { T[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
        }
    }
    s
}
/// Copies through OSC 52, and also through the system clipboard command when
/// not over SSH, since the terminal (or tmux) may ignore OSC 52.
fn copy(out: &mut impl Write, text: &str) -> String {
    let _ = write!(out, "\x1b]52;c;{}\x07", b64(text.as_bytes()));
    let _ = out.flush();
    if std::env::var_os("SSH_CONNECTION").is_some() {
        return "OSC 52".into();
    }
    let cmd: Option<(&str, &[&str])> = if cfg!(target_os = "macos") {
        Some(("pbcopy", &[]))
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        Some(("wl-copy", &[]))
    } else if std::env::var_os("DISPLAY").is_some() {
        Some(("xclip", &["-selection", "clipboard"]))
    } else {
        None
    };
    let Some((c, args)) = cmd else { return "OSC 52".into() };
    let ok = std::process::Command::new(c)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut ch| {
            ch.stdin.take().unwrap().write_all(text.as_bytes())?;
            ch.wait()
        })
        .is_ok_and(|s| s.success());
    if ok { format!("OSC 52 and {c}") } else { format!("OSC 52; {c} failed") }
}

/// Every match of `q` (lower case) in the rendered rows, as (row, first cell, width).
fn find_all(rows: &[Row], q: &str) -> Vec<(usize, usize, usize)> {
    let qc: Vec<char> = q.chars().collect();
    let mut out = vec![];
    if qc.is_empty() {
        return out;
    }
    let low = |c: char| c.to_lowercase().next().unwrap_or(c);
    for (ri, r) in rows.iter().enumerate() {
        let t: Vec<char> = plain(r).chars().collect();
        let wd: Vec<usize> = t.iter().map(|&c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)).collect();
        let mut i = 0;
        while i + qc.len() <= t.len() {
            if (0..qc.len()).all(|k| low(t[i + k]) == qc[k]) {
                out.push((ri, wd[..i].iter().sum(), wd[i..i + qc.len()].iter().sum()));
                i += qc.len();
            } else {
                i += 1;
            }
        }
    }
    out
}

/// URLs and file paths in a row of reply text, as (first cell, width, URL).
fn links_in(t: &str, cwd: &str, host: &str) -> Vec<(usize, usize, String)> {
    let mut out = vec![];
    let mut x = 0;
    let mut word: Vec<(usize, char)> = vec![];
    let home = std::env::var("HOME").unwrap_or_default();
    let mut flush = |word: &mut Vec<(usize, char)>| {
        let lead = "([{\"'“`";
        let trail = ".,;:)]}\"'”`";
        let mut s = 0;
        let mut e = word.len();
        while s < e && lead.contains(word[s].1) {
            s += 1;
        }
        while e > s && trail.contains(word[e - 1].1) {
            e -= 1;
        }
        let w: String = word[s..e].iter().map(|c| c.1).collect();
        let url = if w.starts_with("https://") || w.starts_with("http://") {
            Some(w.clone())
        } else if w.contains('/')
            && w.chars().any(char::is_alphanumeric)
            && w.chars().all(|c| c.is_alphanumeric() || "._-/~".contains(c))
            && (w.rsplit('/').next().is_some_and(|l| l.contains('.') && !l.starts_with('.')) || w.starts_with("~/") || w.starts_with('/'))
        {
            let abs = if let Some(r) = w.strip_prefix("~/") {
                format!("{home}/{r}")
            } else if w.starts_with('/') {
                w.clone()
            } else {
                format!("{}/{w}", cwd.replacen('~', &home, 1))
            };
            Some(format!("file://{host}{abs}"))
        } else {
            None
        };
        if let Some(u) = url {
            let c0 = word[s].0;
            let c1 = word[e - 1].0 + 1;
            out.push((c0, c1 - c0, u));
        }
        word.clear();
    };
    for ch in t.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if ch.is_whitespace() {
            if !word.is_empty() {
                flush(&mut word);
            }
        } else if cw == 1 {
            word.push((x, ch));
        }
        x += cw;
    }
    if !word.is_empty() {
        flush(&mut word);
    }
    out
}
fn hostname() -> String {
    let mut b = [0u8; 256];
    unsafe { libc::gethostname(b.as_mut_ptr().cast(), b.len()) };
    String::from_utf8_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(0)]).into_owned()
}

// ============================================================ what the TUI would send, and Fiber's side of it
/// The prototype has no Fiber to talk to: it shows each command it would send,
/// appends it to `--commands FILE`, and plays the lines Fiber would write back.
/// A command line as `docs/invocation.md` shapes it: `id`, `command` and `session_id`
/// (`steer` and `reply` naming a delegate) at the top, the command's own keys under
/// `args`, and no `args` when there are none.
fn command_line(id: &str, command: &str, session_id: Option<&str>, args: Value) -> Value {
    let mut c = json!({ "id": id, "command": command });
    if let Some(s) = session_id {
        c["session_id"] = json!(s);
    }
    if args.as_object().is_some_and(|a| !a.is_empty()) {
        c["args"] = args;
    }
    c
}
fn send(ui: &mut Ui, cmds: &mut Option<std::fs::File>, command: &str, session_id: Option<&str>, args: Value) -> String {
    ui.cmd_n += 1;
    let id = format!("c_{:04x}", 0xc000 + ui.cmd_n);
    let line = command_line(&id, command, session_id, args).to_string();
    if let Some(fh) = cmds {
        let _ = writeln!(fh, "{line}");
    }
    if ui.flash_at.is_none_or(|t| t.elapsed() > Duration::from_millis(200)) {
        ui.flash.clear();
    }
    ui.flash.push(format!("→ would send {line}"));
    ui.flash_at = Some(Instant::now());
    id
}
fn say(ui: &mut Ui, m: String) {
    ui.flash = vec![m];
    ui.flash_at = Some(Instant::now());
}
fn synth(f: &mut Fold, kind: &str, sid: &str, aid: Option<&str>, ts: i64, payload: Value) {
    let mut e = json!({ "kind": kind, "session_id": sid, "ts": ts, "schema_version": 1, "payload": payload });
    if let Some(a) = aid {
        e["action_id"] = json!(a);
    }
    f.apply(&e);
}
fn interrupt(f: &mut Fold, ui: &mut Ui, cmds: &mut Option<std::fs::File>, ts: i64) {
    send(ui, cmds, "cancel", None, json!({}));
    let me = f.session_id.clone();
    // a pending request of the turn ends with its resolved line before its call completes
    let pending: Vec<(String, String, bool)> = f.pending.iter().filter(|p| p.sid == me).map(|p| (p.rid.clone(), p.aid.clone(), matches!(p.what, Asking::Form(_)))).collect();
    for (rid, aid, form) in pending {
        if form {
            synth(f, "interaction_resolved", &me, None, ts, json!({ "request_id": rid, "by": "fiber", "declined": true }));
        } else {
            synth(f, "permission_resolved", &me, Some(&aid), ts, json!({ "request_id": rid, "decision": "deny", "decided_by": "cancel" }));
        }
    }
    for a in f.running_calls() {
        synth(f, "tool_call_completed", &me, Some(&a), ts, json!({ "status": "cancelled", "content": [] }));
    }
    synth(f, "turn_completed", &me, None, ts, json!({ "outcome": "interrupted" }));
    // queued steering messages start the next turn at once, in queue order
    if !f.queue.is_empty() {
        let input: Vec<Value> = f.queue.iter().map(|(id, t)| json!({ "type": "message", "content": [{ "type": "text", "text": t }], "source": "driver", "command_id": id })).collect();
        synth(f, "steering_queue", &me, None, ts, queue_msgs(&[]));
        let turn_id = format!("t_{:04x}", 0xd000 + ui.cmd_n);
        let e = json!({ "kind": "turn_started", "session_id": me, "turn_id": turn_id, "ts": ts, "schema_version": 1, "payload": { "input": input } });
        f.apply(&e);
    }
}
fn approve(f: &mut Fold, ui: &mut Ui, cmds: &mut Option<std::fs::File>, ts: i64, choice: usize) {
    let Some((_, p)) = ui.top(f) else { return };
    let Asking::Approval { tool, req, .. } = &p.what else { return };
    let remember = matches!(choice, CH_SESSION | CH_PROJECT);
    // the remembering choices exist only on a request that offers a rule
    let prefix = match req["rule"]["prefix"].as_str() {
        Some(x) => x.to_string(),
        None if remember => return,
        None => String::new(),
    };
    let (rid, sid, aid, tool) = (p.rid.clone(), p.sid.clone(), p.aid.clone(), tool.clone());
    let deny = choice == CH_DENY;
    let mut args = json!({ "request_id": rid, "decision": if deny { "deny" } else { "allow" } });
    let mut resolved = json!({ "request_id": rid, "decision": args["decision"], "decided_by": "person" });
    if deny && !ui.feedback.is_empty() {
        args["feedback"] = json!(ui.feedback);
        resolved["feedback"] = json!(ui.feedback);
    }
    if remember {
        let (scope, key) = if choice == CH_SESSION { ("session", "grant") } else { ("project", "rule") };
        args["remember"] = json!({ "scope": scope, "prefix": prefix });
        resolved[key] = json!({ "tool": tool, "prefix": prefix });
    }
    send(ui, cmds, "reply", (sid != f.session_id).then_some(sid.as_str()), args);
    synth(f, "permission_resolved", &sid, Some(&aid), ts, resolved);
    ui.feedback.clear();
    ui.choice = 0;
    ui.shown = 0;
    ui.answered = true;
}
fn submit_form(f: &mut Fold, ui: &mut Ui, cmds: &mut Option<std::fs::File>, ts: i64) {
    let Some((_, p)) = ui.top(f) else { return };
    let Asking::Form(fields) = &p.what else { return };
    let (rid, aid, sid) = (p.rid.clone(), p.aid.clone(), p.sid.clone());
    let mut lines = vec![];
    let answers: Vec<Value> = fields
        .iter()
        .enumerate()
        .map(|(k, q)| {
            let (sel, txt) = (&ui.form.sel[k], &ui.form.text[k]);
            let h = q["header"].as_str().unwrap_or("");
            if sel.is_empty() && txt.is_empty() {
                lines.push(format!("{h}: skipped"));
                return json!({ "skipped": true });
            }
            let mut a = json!({ "labels": sel });
            let mut l = format!("{h}: {}", sel.join(", "));
            if !txt.is_empty() {
                a["text"] = json!(txt);
                l += &format!(" \"{txt}\"");
            }
            lines.push(l);
            a
        })
        .collect();
    let mut ans = json!({ "answers": answers });
    if !ui.form.note.is_empty() {
        ans["note"] = json!(ui.form.note);
        lines.push(format!("note: {}", ui.form.note));
    }
    let mut args = ans.clone();
    args["request_id"] = json!(rid);
    send(ui, cmds, "reply", (sid != f.session_id).then_some(sid.as_str()), args.clone());
    let mut res = args;
    res["by"] = json!("person");
    synth(f, "interaction_resolved", &sid, None, ts, res);
    synth(f, "tool_call_completed", &sid, Some(&aid), ts, json!({ "status": "completed", "content": [{ "type": "text", "text": lines.join("\n") }] }));
    ui.shown = 0;
    ui.answered = true;
}
/// The fixture ends waiting on the person, so once nothing waits the turn ends as Fiber
/// would end it: the calls complete, a short reply, then `turn_completed`.
fn finish_turn(f: &mut Fold, ts: i64) {
    let me = f.session_id.clone();
    for a in f.running_calls() {
        synth(f, "tool_call_completed", &me, Some(&a), ts, json!({ "status": "completed", "content": [] }));
    }
    let aid = format!("a_end_{}", f.turns.len());
    synth(f, "assistant_message_started", &me, Some(&aid), ts, json!({}));
    let text = "Thanks — that's everything I needed. Stopping here for the demo.";
    synth(f, "assistant_message_completed", &me, Some(&aid), ts, json!({ "text": text, "outcome": "completed" }));
    synth(f, "turn_completed", &me, None, ts, json!({ "outcome": "completed" }));
}
/// "Chat about this", and Esc on a form: reply declined, then cancel.
fn decline_form(f: &mut Fold, ui: &mut Ui, cmds: &mut Option<std::fs::File>, ts: i64) {
    let Some((_, p)) = ui.top(f) else { return };
    let (rid, aid, sid) = (p.rid.clone(), p.aid.clone(), p.sid.clone());
    send(ui, cmds, "reply", (sid != f.session_id).then_some(sid.as_str()), json!({ "request_id": rid, "declined": true }));
    synth(f, "interaction_resolved", &sid, None, ts, json!({ "request_id": rid, "declined": true, "by": "person" }));
    synth(f, "tool_call_completed", &sid, Some(&aid), ts, json!({ "status": "completed", "content": [{ "type": "text", "text": "declined" }] }));
    interrupt(f, ui, cmds, ts);
    ui.shown = 0;
}
/// Space, Enter or a click on an option: single choice replaces, multi-choice toggles.
fn form_choose(ui: &mut Ui, fields: &[Value]) {
    let Some(q) = fields.get(ui.form.tab) else { return };
    let Some(o) = q["options"].get(ui.form.cur) else { return };
    let label = o["label"].as_str().unwrap_or("").to_string();
    let sel = &mut ui.form.sel[ui.form.tab];
    if q["multiSelect"].as_bool().unwrap_or(false) {
        if let Some(i) = sel.iter().position(|l| *l == label) {
            sel.remove(i);
        } else {
            sel.push(label);
        }
    } else {
        *sel = vec![label];
    }
}
fn queue_msgs(q: &[(String, String)]) -> Value {
    json!({ "messages": q.iter().map(|(id, t)| json!({ "command_id": id, "content": [{ "type": "text", "text": t }], "source": "driver" })).collect::<Vec<_>>() })
}
fn queue_pick(f: &Fold, ui: &mut Ui, i: usize) {
    if i >= f.queue.len() {
        return;
    }
    if ui.qsel.is_none() {
        ui.draft = std::mem::take(&mut ui.input);
    }
    ui.qsel = Some(i);
    ui.input = f.queue[i].1.clone();
}
fn queue_leave(ui: &mut Ui) {
    ui.qsel = None;
    ui.input = std::mem::take(&mut ui.draft);
}
fn queue_drop(f: &mut Fold, ui: &mut Ui, cmds: &mut Option<std::fs::File>, ts: i64, i: usize) {
    let Some((id, _)) = f.queue.get(i).cloned() else { return };
    send(ui, cmds, "steer_drop", None, json!({ "command_id": id }));
    let mut q = f.queue.clone();
    q.remove(i);
    let me = f.session_id.clone();
    synth(f, "steering_queue", &me, None, ts, queue_msgs(&q));
    if ui.qsel.is_some() {
        queue_leave(ui);
    }
}
/// Enter in the input box: amends the queued message being edited, steers during a turn, or prompts.
fn enter(f: &mut Fold, ui: &mut Ui, cmds: &mut Option<std::fs::File>, ts: i64) {
    let me = f.session_id.clone();
    if let Some(i) = ui.qsel {
        let Some((id, _)) = f.queue.get(i).cloned() else { return queue_leave(ui) };
        send(ui, cmds, "steer_amend", None, json!({ "command_id": id, "content": [{ "type": "text", "text": ui.input }] }));
        let mut q = f.queue.clone();
        q[i].1 = ui.input.clone();
        synth(f, "steering_queue", &me, None, ts, queue_msgs(&q));
        return queue_leave(ui);
    }
    if ui.input.trim().is_empty() {
        return;
    }
    let text = std::mem::take(&mut ui.input);
    if f.running {
        let id = send(ui, cmds, "steer", None, json!({ "content": [{ "type": "text", "text": text }] }));
        let mut q = f.queue.clone();
        q.push((id, text));
        synth(f, "steering_queue", &me, None, ts, queue_msgs(&q));
    } else {
        send(ui, cmds, "prompt", None, json!({ "content": [{ "type": "text", "text": text }] }));
    }
}

// ============================================================ scrolling
/// Moves the conversation `d` rows (negative is up). `top` is the first row shown while
/// scrolled up, or `None` while following the output; reaching the end follows again.
/// Anchoring the top row, not the distance from the end, keeps the view still while
/// rows are added or removed below it.
fn scroll_by(top: Option<usize>, max_top: usize, d: isize) -> Option<usize> {
    let t = top.unwrap_or(max_top) as isize + d;
    (t < max_top as isize).then(|| t.max(0) as usize)
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
    commands: Option<String>,
    log_input: Option<String>,
    wheel: usize,
    lua: Option<String>,
    lua_uncached: bool,
    paged: bool,
    /// screens of rows kept rendered above and below the viewport
    window: f64,
    page_lines: usize,
    verify_copy: bool,
    bench: bool,
    no_pending: bool,
    hover: bool,
    /// the home screen case (`--home`), drawn instead of the conversation
    home: Option<String>,
    /// the rail design at start: 0 list, 1 cards, 2 tabs
    rail: u8,
    /// the B card density at start: 0 full, 1 medium, 2 compact
    density: u8,
    /// the rail's width at start, as a percent of the window
    rail_share: f64,
    /// the panel's width at start, as a percent of the window
    panel_share: f64,
    /// `--picker CASE`: start with the model picker open
    picker: Option<String>,
    /// `--completions CASE`: start with the completion panel open
    completions: Option<String>,
}
fn args() -> Args {
    let mut a = Args {
        path: "fixtures/session.jsonl".into(),
        speed: 12.0,
        static_: false,
        reduced: false,
        stats: None,
        exit_after: None,
        warmup: 2.0,
        audit: false,
        commands: None,
        log_input: None,
        wheel: 1,
        lua: None,
        lua_uncached: false,
        paged: false,
        window: 1.0,
        page_lines: 64,
        verify_copy: false,
        bench: false,
        no_pending: false,
        hover: false,
        home: None,
        rail: 1,
        density: 2,
        rail_share: 15.0,
        panel_share: 21.0,
        picker: None,
        completions: None,
    };
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
            "--commands" => a.commands = it.next(),
            "--log-input" => a.log_input = it.next(),
            "--wheel-lines" => a.wheel = it.next().and_then(|v| v.parse().ok()).expect("--wheel-lines N"),
            "--lua-renderer" => a.lua = it.next(),
            "--lua-uncached" => a.lua_uncached = true,
            "--paged" => a.paged = true,
            "--window" => a.window = it.next().and_then(|v| v.parse().ok()).expect("--window SCREENS"),
            "--page-lines" => a.page_lines = it.next().and_then(|v| v.parse().ok()).expect("--page-lines N"),
            "--verify-copy" => a.verify_copy = true,
            "--paging-bench" => a.bench = true,
            "--no-pending" => a.no_pending = true,
            "--hover" => a.hover = true,
            "--home" => a.home = it.next(),
            "--rail" => {
                a.rail = match it.next().as_deref().map(|v| v.to_uppercase()).as_deref() {
                    Some("A") => 0,
                    Some("B") => 1,
                    Some("C") => 2,
                    _ => panic!("--rail A|B|C"),
                }
            }
            "--rail-share" => a.rail_share = it.next().and_then(|v| v.parse().ok()).expect("--rail-share P"),
            "--panel-share" => a.panel_share = it.next().and_then(|v| v.parse().ok()).expect("--panel-share P"),
            "--density" => {
                a.density = match it.next().as_deref().map(|v| v.to_lowercase()).as_deref() {
                    Some("full") => 0,
                    Some("medium") => 1,
                    Some("three") => 2,
                    Some("compact") => 3,
                    _ => panic!("--density full|medium|three|compact"),
                }
            }
            "--picker" => a.picker = it.next(),
            "--completions" => a.completions = it.next(),
            "-h" | "--help" => {
                println!("tui-prototype [FIXTURE] [--speed N] [--static] [--no-pending] [--reduced-motion] [--hover] [--home CASE] [--rail A|B|C] [--density full|medium|three|compact] [--rail-share P] [--panel-share P] [--picker CASE] [--completions CASE] [--commands FILE] [--log-input FILE] [--wheel-lines N] [--lua-renderer FILE.lua [--lua-uncached]] [--paged [--window SCREENS] [--page-lines N] [--verify-copy]] [--paging-bench] [--stats FILE --exit-after S [--warmup S] [--diff-audit]]");
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

// button presses, drags and releases, in SGR form; not 1003, which reports every motion
const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";
// `--hover`: every motion too, so the target under the pointer can be tinted
const HOVER_ON: &str = "\x1b[?1003h";
const HOVER_OFF: &str = "\x1b[?1003l";

/// The hover tint: the cell's own background a little lighter.
fn lift(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(r.saturating_add(0x14), g.saturating_add(0x14), b.saturating_add(0x14)),
        _ => rgb(0x1e1e26),
    }
}

/// `--no-pending`: drops the requests nobody answered and the calls that asked them, drops the
/// open turn's calls that never finished, and ends that turn, so the screen opens settled.
fn settle(mut ev: Vec<Value>) -> Vec<Value> {
    let me = ev.first().and_then(|e| e["session_id"].as_str()).unwrap_or("").to_string();
    let kind = |e: &Value| e["kind"].as_str().unwrap_or("").to_string();
    let asks = |e: &Value| matches!(e["kind"].as_str(), Some("permission_requested" | "interaction_requested"));
    let calls = |e: &Value| matches!(e["kind"].as_str(), Some("tool_call_requested" | "tool_call_started"));
    let resolved: HashSet<String> = ev.iter().filter(|e| kind(e).ends_with("_resolved")).filter_map(|e| e["payload"]["request_id"].as_str().map(str::to_string)).collect();
    let done: HashSet<String> = ev.iter().filter(|e| kind(e) == "tool_call_completed").filter_map(|e| e["action_id"].as_str().map(str::to_string)).collect();
    let unasked = |e: &Value| asks(e) && !resolved.contains(e["payload"]["request_id"].as_str().unwrap_or(""));
    let asked: HashSet<String> = ev.iter().filter(|e| unasked(e)).filter_map(|e| e["action_id"].as_str().or(e["payload"]["action_ids"][0].as_str()).map(str::to_string)).collect();
    let mine = |e: &Value, k: &str| kind(e) == k && e["session_id"] == me.as_str();
    let open = ev.iter().rposition(|e| mine(e, "turn_started")).filter(|&s| !ev[s..].iter().any(|e| mine(e, "turn_completed")));
    let ts = ev.last().map_or(0, |e| e["ts"].as_i64().unwrap_or(0));
    let mut i = 0;
    ev.retain(|e| {
        i += 1;
        let aid = e["action_id"].as_str().unwrap_or("");
        let unfinished = open.is_some_and(|s| i > s) && e["session_id"] == me.as_str() && calls(e) && !done.contains(aid);
        !(unasked(e) || (calls(e) && asked.contains(aid)) || unfinished)
    });
    if open.is_some() {
        ev.push(json!({ "kind": "turn_completed", "session_id": me, "ts": ts, "schema_version": 1, "payload": { "outcome": "completed" } }));
    }
    ev
}

fn main() -> io::Result<()> {
    let t0 = Instant::now();
    let mut a = args();
    if a.no_pending {
        // the settled lines go to a file of their own, so the paged mode reads them too
        let ev: Vec<Value> = std::fs::read_to_string(&a.path)?.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        let p = std::env::temp_dir().join(format!("tui-prototype-settled-{}.jsonl", std::process::id()));
        std::fs::write(&p, settle(ev).iter().map(|e| e.to_string() + "\n").collect::<String>())?;
        a.path = p.to_string_lossy().into_owned();
    }
    if a.bench {
        // the conversation's text width and height at 160 by 48: 160 less the panel less
        // two margins, 48 less the input box
        print!("{}", paged::bench(&a.path, a.page_lines, &[0, 1, 4], 160 - PANEL as usize - 2, 48 - 3, &["tool", "flaky"])?);
        return Ok(());
    }
    // paged: one streaming pass for the offset table and the panel; no event is kept
    let (events, mut f, pager, open_index) = if a.paged {
        let t = Instant::now();
        let (p, sum) = paged::Pager::open(&a.path, a.page_lines)?;
        (vec![], sum, Some(p), t.elapsed())
    } else {
        let events: Vec<Value> = std::fs::read_to_string(&a.path)?.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        (events, Fold::default(), None, Duration::ZERO)
    };
    let mut next = 0;
    if a.static_ {
        for e in &events {
            f.apply(e);
        }
        next = events.len();
    }

    terminal::enable_raw_mode()?;
    let mut out = Counting(BufWriter::with_capacity(1 << 16, io::stdout()));
    execute!(out, terminal::EnterAlternateScreen)?;
    out.write_all(MOUSE_ON.as_bytes())?;
    if a.hover {
        out.write_all(HOVER_ON.as_bytes())?;
    }
    let mut term = Terminal::new(CrosstermBackend::new(out))?;
    let res = if a.home.is_some() { home::run_home(&a, &mut term) } else { run(&a, &events, &mut f, &mut next, &mut term, t0, pager, open_index) };
    let b = term.backend_mut();
    if a.hover {
        b.write_all(HOVER_OFF.as_bytes())?;
    }
    b.write_all(MOUSE_OFF.as_bytes())?;
    execute!(b, terminal::LeaveAlternateScreen, crossterm::cursor::Show)?;
    terminal::disable_raw_mode()?;
    if a.no_pending {
        let _ = std::fs::remove_file(&a.path);
    }
    let report = res?;
    if let Some(path) = &a.stats {
        std::fs::write(path, report)?;
    }
    // home draws no session, so there is nothing to resume
    if a.home.is_none() {
        println!("Session {0} · resume it with fiber --resume {0}", f.session_id);
    }
    Ok(())
}

type Term = Terminal<CrosstermBackend<Counting<BufWriter<io::Stdout>>>>;

#[allow(clippy::too_many_arguments)]
fn run(a: &Args, events: &[Value], f: &mut Fold, next: &mut usize, term: &mut Term, t0: Instant, mut pager: Option<paged::Pager>, open_index: Duration) -> io::Result<String> {
    let v0 = Instant::now();
    // paged: rows in the whole conversation; the scans (count or search passes) and their
    // times; frames that loaded pages, how long the loading took, and from the input event
    // that caused the frame to its last byte; frames whose visible rows were not yet loaded
    let mut total_rows = 0usize;
    let mut scans: Vec<Duration> = vec![];
    let (mut load_ms, mut ev_lat, mut vis_miss_n): (Vec<Duration>, Vec<Duration>, usize) = (vec![], vec![], 0);
    let mut ev_at: Option<Instant> = None;
    let mut copy_check: Option<bool> = None;
    let lua = match &a.lua {
        Some(p) => Some(std::rc::Rc::new(lua::Ext::new(p, &std::fs::read_to_string(p)?, !a.lua_uncached).map_err(|e| io::Error::other(format!("{p}: {e}")))?)),
        None => None,
    };
    let mut v = View { reduced: a.reduced, lua, ..Default::default() };
    // Lua calls when the measurement window opens, and the Lua state's memory after the first frame
    let (mut lua_calls0, mut lua_mem) = (0u64, 0usize);
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
    // the first conversation row shown while scrolled up; None follows the output
    let mut top: Option<usize> = None;
    let mut pscroll: usize = 0;
    type Key3 = (bool, Option<Act>, String);
    let mut conv_cache: Option<(usize, Key3, Vec<Row>)> = None;
    let mut conv_gen: u64 = 0;
    let mut panel_cache: Option<Vec<Row>> = None;
    let mut hits: Vec<(u16, u16, u16, Act)> = vec![];
    // `--hover`: the pointer's cell, the target tinted in the last frame, the target under the
    // pointer now, and motion events and target changes in the measurement window
    let mut ptr: Option<(u16, u16)> = None;
    // whether OSC 22 last set the col-resize pointer: written only on change, restored on exit
    let mut ptr_col_resize = false;
    let mut hovered: Option<(u16, u16, u16)> = None;
    let mut under: Option<(u16, u16, u16)> = None;
    let (mut motions, mut hover_changes) = (0u64, 0u64);
    let mut dirty = true;
    let mut geom = (0u16, 0u16, 0u16, false, 0u16); // conversation width, screen width, rail width, tabs, panel width
    // the rail's row count and card map ((session, first row, rows)) from the last frame,
    // so wheel and jump keys can scroll it without rebuilding its rows
    let mut rail_lay: (usize, usize, Vec<(usize, usize, usize)>) = (0, 0, vec![]);
    // where the conversation's rows sit: first row shown, blank rows above it, rows shown,
    // height, and the first row shown at the end
    let mut lay = (0usize, 0usize, 0usize, 0usize, 0usize);
    // frames are at most one per FRAME_GAP, so a burst of wheel events is one frame
    const FRAME_GAP: Duration = Duration::from_millis(16);
    let mut last_frame = Instant::now() - FRAME_GAP;
    let mut log = match &a.log_input {
        Some(p) => Some(std::fs::File::create(p)?),
        None => None,
    };
    let exit_at = a.exit_after.map(|s| v0 + Duration::from_secs_f64(s));
    let mut window: Option<(Instant, u64, u64, u64, (f64, i64, i64, u64, u64))> = None;
    let mut audit = Audit::default();
    let mut prev_buf: Option<Buffer> = None;
    let mut frames: u64 = 0;

    let mut ui = Ui {
        // `--completions CASE` starts with its query already typed
        input: a.completions.as_deref().map_or_else(String::new, completions::input_for),
        rail: a.rail,
        density: a.density.min(2),
        rail_share: a.rail_share.clamp(1.0, 90.0),
        panel_share: a.panel_share.clamp(1.0, 90.0),
        picker: a.picker.as_deref().map(model_picker::for_case),
        completions: a.completions.as_deref().map(completions::for_case),
        ..Default::default()
    };
    model_picker::set_still(a.static_);
    let mut rd = input::Reader::new()?;
    let mut cmds = match &a.commands {
        Some(p) => Some(std::fs::OpenOptions::new().create(true).append(true).open(p)?),
        None => None,
    };
    let host = hostname();
    // keyboard protocol detection: sent after the first frame, never waited on
    let mut first_frame: Option<Duration> = None;
    let mut det_sent: Option<Duration> = None;
    let mut kitty_seen: Option<u32> = None;
    let mut det: Option<(bool, Duration)> = None;
    let mut pushed = false;
    let mut links_at = (u64::MAX, usize::MAX, false, false, false);
    let mut next_auto = Instant::now();
    const FLASH: Duration = Duration::from_secs(6);

    'main: loop {
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
            lua_calls0 = v.lua.as_ref().map_or(0, |x| x.calls.get());
        }
        // dragging past the top or bottom edge keeps scrolling while the button is held
        if ui.drag_edge != 0 && now_i >= next_auto {
            next_auto = now_i + Duration::from_millis(40);
            let (start, _, shown, _, max_top) = lay;
            if let Some(s) = ui.sel.as_mut() {
                if ui.drag_edge < 0 && start > 0 {
                    top = Some(start - 1);
                    s.b.0 = start - 1;
                } else if ui.drag_edge > 0 && start < max_top {
                    top = scroll_by(Some(start), max_top, 1);
                    s.b.0 = start + shown;
                }
                s.moved = true;
            }
            dirty = true;
        }
        if ui.flash_at.is_some_and(|t| now_i >= t + FLASH) {
            ui.flash.clear();
            ui.flash_at = None;
            dirty = true;
        }
        if ui.copied.as_ref().is_some_and(|c| now_i >= c.1 + FLASH) {
            ui.copied = None;
            dirty = true;
        }

        // below the floor: one centred line, nothing else laid out or drawn; the loop carries on
        let size = term.size()?;
        let small = size.width < FLOOR_COLS || size.height < FLOOR_ROWS;
        if small && dirty && now_i >= last_frame + FRAME_GAP {
            dirty = false;
            last_frame = now_i;
            frames += 1;
            let (cols, rows) = (size.width, size.height);
            term.backend_mut().write_all(b"\x1b[?2026h")?;
            term.draw(|fr| {
                if cols > 0 && rows > 0 {
                    let msg: String = format!("Fiber needs {FLOOR_COLS}\u{d7}{FLOOR_ROWS} \u{b7} now {cols}\u{d7}{rows}").chars().take(cols as usize).collect();
                    fr.buffer_mut().set_string((cols as usize).saturating_sub(msg.width()) as u16 / 2, rows / 2, &msg, Style::new());
                }
            })?;
            let be = term.backend_mut();
            be.write_all(b"\x1b[?2026l")?;
            be.flush()?;
        }
        if !small && dirty && now_i >= last_frame + FRAME_GAP {
            dirty = false;
            last_frame = now_i;
            frames += 1;
            let (b0, fl0) = (BYTES.load(Relaxed), FLUSHES.load(Relaxed));
            ui.sync_form(f);
            ui.keys = match (det, det_sent) {
                (Some((true, d)), _) => format!("kitty · {} ms", d.as_millis()),
                (Some((false, d)), _) => format!("legacy · {} ms", d.as_millis()),
                (None, Some(_)) => "detecting…".into(),
                (None, None) => String::new(),
            };
            let size = term.size()?;
            let (cols, rows) = (size.width, size.height);
            // the rail takes columns from the left before the conversation column; both it
            // and the panel are a share of the window, so they survive a resize as a share.
            // The rail hides completely when shut or too narrow, leaving a 1-column
            // handle; "N waiting" joins the status rows or the Session card. The
            // conversation keeps CONV_MIN throughout.
            let rail_full_w = ((cols as f64 * ui.rail_share / 100.0).round() as u16).clamp(22, 48);
            let panel_full_w = ((cols as f64 * ui.panel_share / 100.0).round() as u16).clamp(30, 60);
            let rail_cols: u16 = match ui.rail { 0 => RAIL_A, _ => rail_full_w };
            // too narrow for rail floor + conversation minimum + panel: the rail goes;
            // an auto-hidden rail returns by itself, a dragged-shut one stays shut
            let rail_auto = cols < 22 + CONV_MIN + panel_full_w;
            let rail_hidden = ui.rail != 2 && (ui.rail_off || rail_auto);
            let mut rail_w: u16 = if ui.rail == 2 { 0 } else if rail_hidden { 1 } else { rail_cols };
            if ui.rail != 2 && cols < rail_w + CONV_MIN {
                rail_w = 0;
            }
            let avail = cols.saturating_sub(rail_w);
            let narrow = avail < panel_full_w + CONV_MIN;
            let panel_w = if narrow { 0 } else { panel_full_w };
            let conv_w = avail - panel_w;
            let cw = (conv_w as usize).saturating_sub(2);
            let sess = live_sessions(f, &ui, vnow);
            let waiting = sess.iter().enumerate().filter(|(idx, s)| s.st == SState::NeedsInput && !ui.dismissed.contains(idx)).count();
            let rail_gone = rail_hidden || (ui.rail == 2 && cols < CONV_MIN);
            let rail_note = (rail_gone && waiting > 0).then(|| format!("{waiting} waiting"));
            // variant C takes one row from the top of the conversation column instead
            let show_tabs = ui.rail == 2 && cols >= CONV_MIN;
            // the call asking the person has its group open
            v.force = ui.top(f).filter(|(_, p)| p.sid == f.session_id).and_then(|(_, p)| f.at.get(&p.aid)).map(|&(ti, bi, _)| Act::Group(ti, bi));
            v.q = ui.search.as_ref().map(|s| s.q.to_lowercase()).filter(|q| q.chars().count() >= 3).unwrap_or_default();
            let key: Key3 = (v.all_open, v.force, v.q.clone());
            if let Some(pg) = pager.as_mut() {
                // no approval is folded into a page, and group ids name pages, not turns
                v.force = None;
                let find = ui.search.as_ref().map(|s| s.q.to_lowercase()).unwrap_or_default();
                scans.extend(pg.sync(cw, &v, &find));
                if let Some(s) = ui.search.as_mut() {
                    s.hits = pg.hits.iter().map(|&(p, r, c, n)| (pg.start_of(p) + r, c, n)).collect();
                    s.i = s.i.min(s.hits.len().saturating_sub(1));
                }
            } else if conv_cache.as_ref().is_none_or(|(w, k, _)| *w != cw || *k != key) {
                conv_cache = Some((cw, key, conversation(f, cw, &v)));
                conv_gen += 1;
                if let Some(n) = v.lua.as_ref().and_then(|x| x.take_notice()) {
                    f.notices.push(("extension".into(), n, false));
                }
            }
            if narrow {
                panel_cache = None;
            }
            if panel_cache.is_none() && !narrow {
                panel_cache = Some(panel_rows(f, vnow, &ui.keys, panel_w as usize, if rail_hidden { waiting } else { 0 }));
            }
            let no_rows = vec![];
            let conv: &Vec<Row> = conv_cache.as_ref().map_or(&no_rows, |c| &c.2);
            total_rows = pager.as_ref().map_or(conv.len(), |p| p.total());
            let total = total_rows;
            if let Some(s) = ui.search.as_mut().filter(|_| pager.is_none()) {
                s.hits = find_all(conv, &s.q.to_lowercase());
                s.i = s.i.min(s.hits.len().saturating_sub(1));
            }
            let mut bot = bottom(f, conv_w as usize, tick, vnow, &v, narrow, &ui, rail_note);
            // a bottom area taller than the screen keeps its last rows, the input box
            bot.drain(..bot.len().saturating_sub(rows as usize));
            let view_h = (rows as usize).saturating_sub(bot.len());
            let tabs_h = if show_tabs { 1 } else { 0 };
            let vh = view_h.saturating_sub(tabs_h);
            let max_top = total.saturating_sub(vh);
            if let Some(s) = ui.search.as_mut().filter(|s| s.jump) {
                s.jump = false;
                if let Some(&(r, _, _)) = s.hits.get(s.i) {
                    // centre the current match
                    top = Some(r.saturating_sub(vh / 2));
                }
            }
            top = top.filter(|&t| t < max_top);
            let start = top.unwrap_or(max_top);
            let end = (start + vh).min(total);
            // paged: keep the pages of the viewport and `window` screens either side rendered
            let mut loaded = (0, Duration::ZERO, false);
            if let Some(pg) = pager.as_mut().filter(|_| end > start) {
                let m = (a.window * vh as f64) as usize;
                let miss = (pg.page_at(start)..=pg.page_at(end - 1)).any(|p| !pg.resident(p));
                let t = Instant::now();
                let n = pg.ensure(start.saturating_sub(m), (end + m).min(total), &v);
                loaded = (n, t.elapsed(), miss);
            }
            let vis: Vec<&Row> = match pager.as_ref() {
                Some(pg) => (start..end).map(|r| pg.row(r)).collect(),
                None => conv[start..end].iter().collect(),
            };
            let top_pad = tabs_h + vh.saturating_sub(vis.len());
            // a swapped view: its header, then its rows from its own scroll. The picker
            // sits over the context breakdown.
            let vrows: Option<Vec<Row>> = ui
                .picker
                .as_mut()
                .map(|p| {
                    p.tick = p.tick.wrapping_add(1);
                    model_picker::view(p, cw)
                })
                .map(|body| {
                    ui.vscroll = ui.vscroll.min(body.len().saturating_sub(view_h.saturating_sub(1)));
                    let mut r = vec![view_header("Models", "/model")];
                    r.extend(body.into_iter().skip(ui.vscroll).take(view_h.saturating_sub(1)));
                    r
                })
                .or_else(|| {
                    ui.ctx_view.then(|| {
                        let body = context_view(f, cw);
                        ui.vscroll = ui.vscroll.min(body.len().saturating_sub(view_h.saturating_sub(1)));
                        let mut r = vec![view_header("Context", "/context")];
                        r.extend(body.into_iter().skip(ui.vscroll).take(view_h.saturating_sub(1)));
                        r
                    })
                });
            lay = (start, top_pad, if vrows.is_some() { 0 } else { vis.len() }, view_h, max_top);
            hits.clear();
            geom = (conv_w, cols, rail_w, show_tabs, panel_w);
            let panel = panel_cache.as_ref();
            let uncached = v.lua.as_deref().filter(|x| !x.cached);
            let (search, sel, copied) = (ui.search.as_ref(), ui.sel, ui.copied.as_ref().map(|c| c.0.as_str()));
            // the session rail's rows, built before the frame so jumps can clamp the scroll.
            // B shows every project; A and C hide other projects behind the scope line.
            let seq = rail_seq(&sess, ui.rail);
            let shown: Vec<usize> = (if ui.rail == 1 { seq } else { seq.into_iter().filter(|&i| ui.show_all || sess[i].project == "fiber").collect() }).into_iter().filter(|&i| !ui.dismissed.contains(&i)).collect();
            let hidden_n = if ui.rail == 1 { 0 } else { sess.iter().filter(|s| s.project != "fiber").count() - shown.iter().filter(|&&i| sess[i].project != "fiber").count() };
            let (rrows, rmap): (Vec<Row>, Vec<(usize, usize, usize)>) = if rail_w <= 1 || ui.rail == 2 {
                (vec![], vec![])
            } else if ui.rail == 1 {
                rail_full_b(&sess, &shown, ui.screen, tick, v.reduced, rail_w as usize, ui.density)
            } else {
                (rail_full(&sess, &shown, ui.screen, tick, v.reduced, rail_w as usize, hidden_n), vec![])
            };
            ui.rail_top = ui.rail_top.min(rrows.len().saturating_sub(rows as usize));
            // a resize drag's live share, drawn as a dim pill over the conversation
            let drag_lab: Option<String> = ui.resize.map(|r| {
                let (name, share, w) = if r == 0 { ("rail", ui.rail_share, rail_w) } else { ("panel", ui.panel_share, panel_w) };
                format!(" {name} {}% · {w} cols ", share.round() as u16)
            });
            // synchronised output (DEC mode 2026): the terminal shows the frame only once it is
            // all written, however many writes and flushes it takes
            term.backend_mut().write_all(b"\x1b[?2026h")?;
            let completed = term.draw(|fr| {
                let buf = fr.buffer_mut();
                if let Some(vr) = &vrows {
                    for (k, r) in vr.iter().enumerate() {
                        let (x, w) = if k == 0 { (rail_w, conv_w) } else { (rail_w + 1, cw as u16) };
                        paint(buf, x, k as u16, w, r);
                        if let Some(act) = r.act {
                            hits.push((k as u16, rail_w, rail_w + conv_w, act));
                        }
                    }
                }
                // conversation
                for (k, r) in vis.iter().enumerate().filter(|_| vrows.is_none()) {
                    let y = (top_pad + k) as u16;
                    // uncached: Lua is called again for every visible row it drew, every frame
                    let fresh = r.lua.as_ref().zip(uncached).and_then(|(src, x)| x.frame_spans(src, cw, BW));
                    match fresh {
                        Some(spans) => paint(buf, rail_w + 1, y, cw as u16, &Row { spans, ..Default::default() }),
                        None => paint(buf, rail_w + 1, y, cw as u16, r),
                    }
                    if let Some(act) = r.act {
                        hits.push((y, rail_w + 1, rail_w + 1 + cw as u16, act));
                    }
                    for &(x0, x1, act) in &r.hot {
                        hits.push((y, rail_w + 1 + x0, rail_w + 1 + x1, act));
                    }
                }
                let ys = |r: usize| (r >= start && r < start + vis.len()).then(|| (top_pad + r - start) as u16);
                // every search match marked, the current one brighter
                if let Some(s) = search.filter(|_| vrows.is_none()) {
                    for (i, &(r, c, w)) in s.hits.iter().enumerate() {
                        let Some(y) = ys(r) else { continue };
                        let st = if i == s.i { Style::new().bg(ORANGE).fg(Color::Black) } else { Style::new().bg(rgb(0x5a4a1a)).fg(Color::White) };
                        buf.set_style(Rect::new(rail_w + 1 + c as u16, y, (w as u16).min(cw as u16 - c as u16), 1), st);
                    }
                }
                if let Some(s) = sel.filter(|s| s.moved) {
                    let (a, b) = if s.a <= s.b { (s.a, s.b) } else { (s.b, s.a) };
                    for r in a.0..=b.0 {
                        let Some(y) = ys(r) else { continue };
                        let x0 = if r == a.0 { a.1 } else { 0 }.min(cw);
                        let x1 = if r == b.0 { b.1 + 1 } else { cw }.min(cw);
                        if x1 > x0 {
                            buf.set_style(Rect::new(rail_w + 1 + x0 as u16, y, (x1 - x0) as u16, 1), Style::new().bg(rgb(0x264f78)));
                        }
                    }
                }
                // scroll thumb
                if total > vh && vh > 0 && vrows.is_none() {
                    let th = (vh * vh / total).max(1);
                    let tt = (vh - th) * start / (total - vh);
                    for y in tt..tt + th {
                        buf.set_string(rail_w + conv_w - 1, (tabs_h + y) as u16, "┃", fg(rgb(0x808080)));
                    }
                }
                // scrolled up: a pill centred at the bottom of the conversation jumps to the end
                if start < max_top && vrows.is_none() && view_h > 0 {
                    // paged: the rows below are not all rendered, and the ruling needs no count
                    let label = if pager.is_some() { " ↓ New messages below · End ".to_string() } else { format!(" ↓ {} lines below · End ", max_top - start) };
                    let pw = label.width() as u16 + 2;
                    let (x0, y) = (rail_w + 1 + (cw as u16).saturating_sub(pw) / 2, view_h as u16 - 1);
                    for x in x0..x0 + pw {
                        let under = buf[(x, y)].bg;
                        buf[(x, y)].reset();
                        buf[(x, y)].set_bg(under);
                    }
                    // half blocks in the pill's tint round its ends
                    buf.set_string(x0, y, "▐", fg(SEL));
                    buf.set_string(x0 + 1, y, &label, fg(ORANGE).bg(SEL));
                    buf.set_string(x0 + pw - 1, y, "▌", fg(SEL));
                    hits.insert(0, (y, x0, x0 + pw, Act::End));
                }
                // search floats over the conversation's top-right corner, as an editor's find box does
                if let Some(s) = search.filter(|_| vrows.is_none() && view_h >= 3) {
                    let bw = SBOX_W.min(cw.saturating_sub(2));
                    let x0 = rail_w + 1 + (cw - bw) as u16;
                    for x in x0..x0 + bw as u16 {
                        for (y, ch) in [(0, "▄"), (1, " "), (2, "▀")] {
                            let under = buf[(x, y)].bg;
                            let c = &mut buf[(x, y)];
                            c.reset();
                            c.set_symbol(ch);
                            if y == 1 {
                                c.set_bg(BI);
                            } else {
                                c.set_fg(BI).set_bg(under);
                            }
                        }
                    }
                    buf.set_line(x0, 1, &Line::from(tint(fit(&search_box(s), bw), BI)), bw as u16);
                }
                // the copy's confirmation: the top-right corner, below the search box when it is open
                if let Some(m) = copied.filter(|_| vrows.is_none()) {
                    let y = if search.is_some() && view_h >= 3 { 3 } else { 0 };
                    let s = vec![sp(format!(" {m} "), fg(CYAN))];
                    let mw = width(&s).min(cw);
                    if y < view_h && mw > 0 {
                        buf.set_line(rail_w + 1 + (cw - mw) as u16, y as u16, &Line::from(tint(fit(&s, mw), BI)), mw as u16);
                    }
                }
                for (k, r) in bot.iter().enumerate() {
                    let y = (view_h + k) as u16;
                    paint(buf, rail_w, y, conv_w, r);
                    for &(x0, x1, act) in &r.hot {
                        hits.push((y, rail_w + x0, rail_w + x1, act));
                    }
                    if let Some(act) = r.act {
                        hits.push((y, rail_w, rail_w + conv_w, act));
                    }
                }
                // side panel
                if let Some(p) = panel.filter(|_| panel_w > 0) {
                    buf.set_style(Rect::new(rail_w + conv_w, 0, panel_w, rows), Style::new().bg(BP));
                    let ps = pscroll.min(p.len().saturating_sub(rows as usize));
                    for (k, r) in p.iter().skip(ps).take(rows as usize).enumerate() {
                        paint(buf, rail_w + conv_w + 1, k as u16, panel_w - 2, r);
                        if let Some(act) = r.act {
                            hits.push((k as u16, rail_w + conv_w, cols, act));
                        }
                    }
                }
                // the session rail: its own column on the left, or one row of tabs for variant C.
                // Numbers follow start order and never move; a click moves the on-screen marker
                // only. The cards scroll when they overflow the screen. Hidden, the rail
                // leaves a 1-column handle at the left edge.
                if rail_w > 1 {
                    buf.set_style(Rect::new(0, 0, rail_w, rows), Style::new().bg(BP));
                    for (k, r) in rrows.iter().skip(ui.rail_top).take(rows as usize).enumerate() {
                        paint(buf, 0, k as u16, rail_w, r);
                        if let Some(act) = r.act {
                            hits.push((k as u16, 0, rail_w, act));
                        }
                        for &(x0, x1, act) in &r.hot {
                            hits.push((k as u16, x0, x1.min(rail_w), act));
                        }
                    }
                }
                // the resize grips: a dim ⋮ on 3 rows centred on each draggable column —
                // the rail's right edge, the panel's left edge, or the 1-column handle at
                // the left edge while the rail is hidden. On hover or while dragging the
                // whole column tints and the grip goes bright in the accent colour.
                // Hidden, the dead margin column beside the handle is cleared too.
                let rail_hid = ui.rail != 2 && rail_w == 1;
                let rail_hx = if rail_hid { 0 } else { rail_w };
                let rail_gap_hot = ptr.is_some_and(|(x, _)| x == rail_hx) && ui.rail != 2 && rail_w > 0;
                let panel_gap_hot = ptr.is_some_and(|(x, _)| x == rail_w + conv_w) && panel_w > 0;
                for (gx, active, clear_next) in [(rail_hx, ui.resize == Some(0) || rail_gap_hot, rail_hid), (rail_w + conv_w, ui.resize == Some(1) || panel_gap_hot, false)] {
                    if gx >= cols || (gx == rail_w && (rail_w == 0 || ui.rail == 2)) || (gx == rail_w + conv_w && panel_w == 0) {
                        continue;
                    }
                    let mid = rows / 2;
                    for y in 0..rows {
                        let grip_row = y >= mid.saturating_sub(1) && y <= mid + 1;
                        let (sym, st) = match (grip_row, active) {
                            (true, true) => ("⋮", fg(BLUE).add_modifier(Modifier::BOLD)),
                            (true, false) => ("⋮", dim()),
                            _ => (" ", Style::new()),
                        };
                        // Reset clears a stale tint when the drag ends; SEL tints the column
                        paint(buf, gx, y, 1, &Row { spans: vec![sp(sym, st)], bg: Some(if active { SEL } else { Color::Reset }), ..Default::default() });
                        if clear_next {
                            paint(buf, gx + 1, y, 1, &Row { spans: vec![sp(" ", Style::new())], bg: Some(Color::Reset), ..Default::default() });
                        }
                    }
                }
                if show_tabs {
                    let tabrow = rail_tabs(&sess, &shown, ui.screen, cw, tick, v.reduced, hidden_n);
                    paint(buf, rail_w, 0, conv_w, &tabrow);
                    for &(x0, x1, act) in &tabrow.hot {
                        hits.push((0, rail_w + x0, rail_w + x1.min(cw as u16), act));
                    }
                }
                // the target under the pointer, found as a click finds it
                hovered = ptr.and_then(|(x, y)| hits.iter().find(|(hy, x0, x1, _)| *hy == y && x >= *x0 && x < *x1)).map(|h| (h.0, h.1, h.2));
                // hovering a B card brightens the whole card, not just the hovered row
                let hover_card: Option<usize> = if ui.rail == 1 && rail_w > 1 {
                    hovered.and_then(|h| hits.iter().find(|j| (j.0, j.1, j.2) == h).map(|j| j.3)).and_then(|a| match a {
                        Act::Rail(i) => Some(i),
                        _ => None,
                    })
                } else {
                    None
                };
                if let Some(i) = hover_card {
                    for &(si, srow, len) in &rmap {
                        if si == i {
                            let end = (srow + len).saturating_sub(ui.rail_top).min(rows as usize);
                            for y in srow.saturating_sub(ui.rail_top)..end {
                                for x in 0..rail_w.min(cols) {
                                    let c = &mut buf[(x, y as u16)];
                                    c.set_bg(lift(c.bg));
                                }
                            }
                        }
                    }
                } else if let Some((y, x0, x1)) = hovered.filter(|&(y, _, _)| y < rows) {
                    for x in x0..x1.min(cols) {
                        let c = &mut buf[(x, y)];
                        c.set_bg(lift(c.bg));
                    }
                }
                // `--hover` over a rail row (not a B card): one tooltip line with the full
                // name, workspace and spend
                if ui.rail != 1 {
                    if let Some((hy, _, _)) = hovered {
                        let act = hits.iter().find(|h| Some((h.0, h.1, h.2)) == hovered).map(|h| h.3);
                        if let Some(Act::Rail(i)) = act {
                            if let Some(s) = sess.get(i) {
                                let tip = format!(" {} · {} · {} ", s.name, s.ws, s.spend);
                                let tw = tip.width().min(cw).max(1);
                                paint(buf, rail_w + 1, hy, tw as u16, &Row { spans: tint(vec![sp(tip, Style::new())], BI), bg: Some(BI), ..Default::default() });
                            }
                        }
                    }
                }
                // compact hides the name behind truncation and drops the workspace and
                // thinking level: hovering a card shows its full identity in a dim footer
                // at the rail's bottom row, running over the conversation like the A
                // tooltip when it needs the room (`--hover` only, like the brightening)
                if ui.rail == 1 && (ui.density == 3) && rail_w > 1 {
                    if let Some(i) = hover_card {
                        if let Some(s) = sess.get(i) {
                            let tip = format!(" {} · {} · {} · {} ", s.name, s.ws, s.model, s.spend);
                            let tw = tip.width().min(rail_w as usize + cw).max(1);
                            paint(buf, 0, rows - 1, tw as u16, &Row { spans: tint(vec![sp(cut(&tip, tw), dim())], BI), bg: Some(BI), ..Default::default() });
                        }
                    }
                }
                // a resize drag's live share, as a dim pill over the conversation's bottom edge
                if let Some(lab) = &drag_lab {
                    if view_h > 0 {
                        let dw = lab.width() as u16 + 2;
                        let dy = if start < max_top && vrows.is_none() { (view_h as u16).saturating_sub(2) } else { view_h as u16 - 1 };
                        let dx = rail_w + 1 + (cw as u16).saturating_sub(dw) / 2;
                        buf.set_string(dx, dy, "▐", fg(SEL));
                        buf.set_string(dx + 1, dy, lab, dim().bg(SEL));
                        buf.set_string(dx + dw - 1, dy, "▌", fg(SEL));
                    }
                }
            })?;
            rail_lay = (rrows.len(), rows as usize, rmap);
            let frame_buf = (links_at != (conv_gen, start, ui.ctx_view, ui.picker.is_some(), ui.search.is_some()) || a.audit).then(|| completed.buffer.clone());
            if a.audit && window.is_some() {
                let cur = frame_buf.clone().unwrap();
                if let Some(prev) = &prev_buf {
                    if prev.area == cur.area {
                        let d = prev.diff(&cur);
                        let mut ys: Vec<u16> = d.iter().map(|(_, y, _)| *y).collect();
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
            // links: mouse capture turns off the terminal's own link detection, so replies
            // mark URLs and paths with OSC 8, rewritten only when the conversation moves
            if links_at != (conv_gen, start, ui.ctx_view, ui.picker.is_some(), ui.search.is_some()) {
                links_at = (conv_gen, start, ui.ctx_view, ui.picker.is_some(), ui.search.is_some());
                let fb = frame_buf.unwrap();
                let be = term.backend_mut();
                for (k, r) in vis.iter().enumerate().filter(|(_, r)| r.link && vrows.is_none()) {
                    let y = (top_pad + k) as u16;
                    for (c0, w, url) in links_in(&plain(r), &f.cwd, &host) {
                        let x0 = 1 + c0 as u16;
                        let cells: Vec<(u16, u16, &ratatui::buffer::Cell)> = (x0..x0 + w as u16).filter(|&x| x < conv_w).map(|x| (x, y, &fb[(x, y)])).collect();
                        write!(be, "\x1b]8;;{url}\x1b\\")?;
                        ratatui::backend::Backend::draw(be, cells.into_iter())?;
                        write!(be, "\x1b]8;;\x1b\\")?;
                    }
                }
            }
            let be = term.backend_mut();
            be.write_all(b"\x1b[?2026l")?;
            be.flush()?;
            if loaded.0 > 0 {
                load_ms.push(loaded.1);
                vis_miss_n += loaded.2 as usize;
                ev_lat.extend(ev_at.map(|t| t.elapsed()));
            }
            ev_at = None;
            if let Some(l) = log.as_mut() {
                let _ = writeln!(
                    l,
                    "{:>9.1} frame  top {top:?} start {start} end {max_top} rows {} bytes {} flushes {}",
                    v0.elapsed().as_secs_f64() * 1000.0,
                    total,
                    BYTES.load(Relaxed) - b0,
                    FLUSHES.load(Relaxed) - fl0
                );
            }
            if first_frame.is_none() {
                first_frame = Some(t0.elapsed());
                lua_mem = v.lua.as_ref().map_or(0, |x| x.used_memory());
                // kitty's flags query, then primary device attributes: a terminal that
                // answers the second but not the first does not speak the protocol
                let be = term.backend_mut();
                be.write_all(b"\x1b[?u\x1b[c")?;
                be.flush()?;
                det_sent = Some(t0.elapsed());
                panel_cache = None;
                dirty = true;
            }
        }

        // sleep until the next event, the next tick, or input; with no turn running and no
        // replay left there is no deadline at all
        let mut deadline: Option<Instant> = replaying.then_some(due);
        let mut add = |d: Option<Instant>| {
            if let Some(d) = d {
                deadline = Some(deadline.map_or(d, |x| x.min(d)));
            }
        };
        add(f.running.then_some(next_tick));
        add(window.is_none().then(|| v0 + Duration::from_secs_f64(a.warmup)));
        add(rd.deadline());
        add(ui.flash_at.map(|t| t + FLASH));
        add(ui.copied.as_ref().map(|c| c.1 + FLASH));
        add((ui.drag_edge != 0).then_some(next_auto));
        add(exit_at);
        if dirty {
            add(Some(last_frame + FRAME_GAP));
        }
        if exit_at.is_some_and(|x| Instant::now() >= x) {
            break;
        }
        let timeout = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        let (evs, resized) = rd.wait(timeout)?;
        if !evs.is_empty() && ev_at.is_none() {
            ev_at = Some(Instant::now());
        }
        if resized {
            dirty = true;
            conv_cache = None;
        }
        if let Some(l) = log.as_mut().filter(|_| !rd.raw.is_empty()) {
            let _ = writeln!(l, "{:>9.1} read   {:?}", v0.elapsed().as_secs_f64() * 1000.0, String::from_utf8_lossy(&rd.raw));
            for e in &evs {
                let _ = writeln!(l, "{:>9} event  {e:?}", "");
            }
        }
        let (conv_w, cols, rail_w, show_tabs, panel_w) = geom;
        let (start, top_pad, shown, view_h, max_top) = lay;
        // only what changes the fold or the rows throws the caches away; scrolling and selecting do not
        let mut changed = false;
        let conv_len = total_rows;
        // a screen point in the conversation, as (row, cell), clamped to what is shown
        let at = |x: u16, y: u16| -> Option<(usize, usize)> {
            if conv_len == 0 || shown == 0 {
                return None;
            }
            let r = start + (y as usize).saturating_sub(top_pad).min(shown - 1);
            Some((r, (x as usize).saturating_sub((rail_w + 1) as usize)))
        };
        let ts = vnow;
        for ev in evs {
            if let (true, Ev::Mouse(kind, x, y, _)) = (a.hover, ev) {
                ptr = Some((x, y));
                if kind == Mouse::Move {
                    // a redraw only when the target under the pointer changes
                    let h = hits.iter().find(|(hy, x0, x1, _)| *hy == y && x >= *x0 && x < *x1).map(|h| (h.0, h.1, h.2));
                    if window.is_some() {
                        motions += 1;
                        hover_changes += (h != under) as u64;
                    }
                    under = h;
                    dirty |= h != hovered;
                    continue;
                }
            }
            // sideways wheel and other buttons change nothing, so they draw no frame
            if matches!(ev, Ev::Mouse(Mouse::Other | Mouse::Move, ..)) {
                continue;
            }
            dirty = true;
            let mut click: Option<Act> = None;
            match ev {
                Ev::KittyFlags(fl) => {
                    if det.is_none() {
                        kitty_seen = Some(fl);
                    }
                    continue;
                }
                Ev::Da1 => {
                    if det.is_none() {
                        det = Some((kitty_seen.is_some(), t0.elapsed()));
                        if kitty_seen.is_some() {
                            // disambiguate escape codes: Shift+Enter, Cmd+F and Esc become unambiguous
                            let be = term.backend_mut();
                            be.write_all(b"\x1b[>1u")?;
                            be.flush()?;
                            pushed = true;
                        }
                        panel_cache = None;
                    }
                    continue;
                }
                Ev::Key(k, m) => {
                    let plain_key = !m.ctrl && !m.alt && !m.sup;
                    if k == Key::Char('c') && m.ctrl {
                        break 'main;
                    }
                    // the selection's highlight goes with any key; Esc does nothing else then
                    if ui.sel.take().is_some_and(|s| s.moved) && k == Key::Esc {
                        continue;
                    }
                    if k == Key::Char('f') && (m.ctrl || m.sup) {
                        ui.ctx_view = false;
                        ui.picker = None;
                        let s = ui.search.get_or_insert_with(Search::default);
                        if !s.hits.is_empty() {
                            s.i = (s.i + 1) % s.hits.len();
                            s.jump = true;
                        }
                        continue;
                    }
                    // the rail: function keys do not reach every shell, so the designs and
                    // densities switch by clicking the chips in the rail's bottom label.
                    // ⌥N jumps to a session, ⌥A to the oldest waiting one, ⌥R toggles the
                    // rail shown/hidden. Alt+digit arrives with alt set both as legacy
                    // ESC-prefix and kitty `CSI 49;3u`; ⌥A and ⌥R arrive as "å" and "®"
                    // on macOS where Option is not Alt, like ⌥X arrives as "≈".
                    if k == Key::Char('å') && !m.alt {
                        let sess = live_sessions(f, &ui, vnow);
                        if let Some(i) = sess.iter().position(|s| s.st == SState::NeedsInput) {
                            ui.screen = i;
                            rail_ensure(&mut ui, &rail_lay, i);
                        }
                        continue;
                    }
                    if k == Key::Char('®') && !m.alt {
                        ui.rail_off = !ui.rail_off;
                        panel_cache = None;
                        continue;
                    }
                    if m.alt {
                        if let Key::Char(c) = k {
                            if c == 'a' || c == 'A' {
                                let sess = live_sessions(f, &ui, vnow);
                                if let Some(i) = sess.iter().position(|s| s.st == SState::NeedsInput) {
                                    ui.screen = i;
                                    rail_ensure(&mut ui, &rail_lay, i);
                                }
                                continue;
                            }
                            if c == 'r' || c == 'R' {
                                ui.rail_off = !ui.rail_off;
                                panel_cache = None;
                                continue;
                            }
                            if let Some(d) = c.to_digit(10) {
                                if (1..=9).contains(&d) {
                                    let sess = live_sessions(f, &ui, vnow);
                                    let shown: Vec<usize> = rail_seq(&sess, ui.rail).into_iter().filter(|&i| !ui.dismissed.contains(&i) && (ui.rail == 1 || ui.show_all || sess[i].project == "fiber")).collect();
                                    // numbers are stable per session, so ⌥N maps the number to its index
                                    if let Some(&i) = shown.iter().find(|&&i| i + 1 == d as usize) {
                                        ui.screen = i;
                                        rail_ensure(&mut ui, &rail_lay, i);
                                    }
                                    continue;
                                }
                            }
                        }
                    }
                    if let Some(s) = ui.search.as_mut() {
                        let n = s.hits.len().max(1);
                        match k {
                            Key::Esc => ui.search = None,
                            Key::Enter if m.shift => s.i = (s.i + n - 1) % n,
                            Key::Up => s.i = (s.i + n - 1) % n,
                            Key::Enter | Key::Down => s.i = (s.i + 1) % n,
                            Key::Backspace => {
                                s.q.pop();
                                s.i = 0;
                            }
                            Key::Char(c) if plain_key => {
                                s.q.push(c);
                                s.i = 0;
                            }
                            _ => {}
                        }
                        if let Some(s) = ui.search.as_mut() {
                            s.jump = true;
                        }
                        continue;
                    }
                    // the model picker takes its keys while it is open; Ctrl+L and
                    // `/model` open it over the conversation
                    if model_picker::on_key(&mut ui, k, m) {
                        continue;
                    }
                    // the `/` and `@` panels take their keys while open
                    if completions::on_key(&mut ui, k, m) {
                        continue;
                    }
                    // Esc leaves a swapped view, back to where the conversation was
                    if k == Key::Esc && ui.ctx_view {
                        ui.ctx_view = false;
                        continue;
                    }
                    let page = view_h.saturating_sub(2).max(1) as isize;
                    let d = match k {
                        Key::PageUp => -page,
                        Key::PageDown => page,
                        Key::Up if !m.alt && ui.top(f).is_none() => -1,
                        Key::Down if !m.alt && ui.top(f).is_none() => 1,
                        _ => 0,
                    };
                    if d != 0 && ui.ctx_view {
                        ui.vscroll = (ui.vscroll as isize + d).max(0) as usize;
                        continue;
                    }
                    if d != 0 {
                        top = scroll_by(top, max_top, d);
                        continue;
                    }
                    match k {
                        Key::Char('o') if m.ctrl => {
                            v.all_open = !v.all_open;
                            continue;
                        }
                        Key::End => {
                            top = None;
                            continue;
                        }
                        _ => {}
                    }
                    changed = true;
                    match ui.top(f).map(|(_, p)| matches!(p.what, Asking::Form(_))) {
                        // the approval panel: typing goes to the feedback
                        Some(false) => match k {
                            Key::Esc => {
                                let rid = ui.top(f).unwrap().1.rid.clone();
                                ui.aside.insert(rid);
                                ui.shown = 0;
                            }
                            Key::Up | Key::Down | Key::Tab => {
                                let has_rule = ui.top(f).is_some_and(|(_, p)| matches!(&p.what, Asking::Approval { req, .. } if req.get("rule").is_some()));
                                let cs = choices(has_rule);
                                let at = cs.iter().position(|&c| c == ui.choice).unwrap_or(0);
                                ui.choice = cs[if k == Key::Up { (at + cs.len() - 1) % cs.len() } else { (at + 1) % cs.len() }];
                            }
                            Key::Enter => {
                                let c = ui.choice;
                                approve(f, &mut ui, &mut cmds, ts, c)
                            }
                            Key::Backspace => {
                                ui.feedback.pop();
                            }
                            Key::Char(c) if plain_key => {
                                ui.feedback.push(c);
                                ui.choice = CH_DENY;
                            }
                            _ => {}
                        },
                        // the question form: Esc means "Chat about this"
                        Some(true) => {
                            let Some((_, p)) = ui.top(f) else { continue };
                            let Asking::Form(fields) = &p.what else { continue };
                            let fields = fields.clone();
                            let n = fields.len();
                            let nopt = fields.get(ui.form.tab).map_or(0, |q| q["options"].as_array().map_or(0, Vec::len));
                            let multi = fields.get(ui.form.tab).is_some_and(|q| q["multiSelect"].as_bool() == Some(true));
                            let on_text = ui.form.tab < n && ui.form.cur == nopt;
                                                        match k {
                                Key::Esc => decline_form(f, &mut ui, &mut cmds, ts),
                                Key::Left | Key::BackTab => {
                                    ui.form.tab = (ui.form.tab + n) % (n + 1);
                                    ui.form.cur = 0;
                                }
                                Key::Right | Key::Tab => {
                                    ui.form.tab = (ui.form.tab + 1) % (n + 1);
                                    ui.form.cur = 0;
                                }
                                Key::Up => ui.form.cur = ui.form.cur.saturating_sub(1),
                                Key::Down => ui.form.cur = (ui.form.cur + 1).min(nopt + multi as usize),
                                Key::Char(' ') if ui.form.tab < n && !on_text => form_choose(&mut ui, &fields),
                                Key::Enter if ui.form.tab == n => submit_form(f, &mut ui, &mut cmds, ts),
                                Key::Enter if multi && ui.form.cur == nopt + 1 => {
                                    ui.form.tab += 1;
                                    ui.form.cur = 0;
                                }
                                Key::Enter => {
                                    if !on_text {
                                        form_choose(&mut ui, &fields);
                                    }
                                    if on_text || !multi {
                                        ui.form.tab += 1;
                                        ui.form.cur = 0;
                                    }
                                }
                                Key::Backspace if ui.form.tab == n => {
                                    ui.form.note.pop();
                                }
                                Key::Backspace if on_text => {
                                    ui.form.text[ui.form.tab].pop();
                                }
                                Key::Char(c) if plain_key => {
                                    let d = c.to_digit(10).unwrap_or(0) as usize;
                                    if ui.form.tab == n {
                                        ui.form.note.push(c);
                                    } else if !on_text && d >= 1 && d <= nopt + 1 {
                                        ui.form.cur = d - 1;
                                        if d <= nopt {
                                            form_choose(&mut ui, &fields);
                                        }
                                    } else {
                                        ui.form.cur = nopt;
                                        ui.form.text[ui.form.tab].push(c);
                                    }
                                }
                                _ => {}
                            }
                        }
                        // the input box, and the steering queue above it
                        None => match k {
                            Key::Esc if ui.qsel.is_some() => queue_leave(&mut ui),
                            Key::Esc if f.running => interrupt(f, &mut ui, &mut cmds, ts),
                            Key::Up if m.alt => {
                                let i = ui.qsel.map_or(f.queue.len().saturating_sub(1), |i| i.saturating_sub(1));
                                queue_pick(f, &mut ui, i);
                            }
                            Key::Down if m.alt => match ui.qsel {
                                Some(i) if i + 1 < f.queue.len() => queue_pick(f, &mut ui, i + 1),
                                Some(_) => queue_leave(&mut ui),
                                None => {}
                            },
                            // ⌥x, or the "≈" macOS types for it where Option is not Alt
                            Key::Char('x') | Key::Char('≈') if ui.qsel.is_some() && (m.alt || k == Key::Char('≈')) => {
                                let i = ui.qsel.unwrap();
                                queue_drop(f, &mut ui, &mut cmds, ts, i);
                            }
                            // the slash commands the prototype runs locally: their views
                            // open at once, anything else sends as usual
                            Key::Enter if ui.qsel.is_none() && ui.input.trim() == "/context" => {
                                ui.input.clear();
                                ui.ctx_view = true;
                                ui.vscroll = 0;
                            }
                            Key::Enter if ui.qsel.is_none() && ui.input.trim() == "/model" => {
                                ui.input.clear();
                                ui.ctx_view = false;
                                ui.picker = Some(model_picker::for_case("list"));
                                ui.vscroll = 0;
                            }
                            Key::Enter => enter(f, &mut ui, &mut cmds, ts),
                            Key::Backspace => {
                                ui.input.pop();
                            }
                            Key::Char(c) if plain_key => ui.input.push(c),
                            _ => {}
                        },
                    }
                }
                Ev::Mouse(kind, x, y, _) => {
                    let over_panel = x >= rail_w + conv_w && rail_w + conv_w < cols && panel_cache.is_some();
                    let over_rail = rail_w > 0 && ui.rail != 2 && x < rail_w;
                    let in_conv = !over_panel && (y as usize) < view_h;
                    let in_sbox = ui.search.is_some() && y < 3 && (x as usize) + SBOX_W + 2 > (rail_w + conv_w) as usize;
                    // the resize handles: the rail's right edge and the panel's left edge,
                    // the one-column gaps beside them; hidden, the rail's 1-column handle
                    // at the screen's left edge
                    let rail_hid = ui.rail != 2 && rail_w == 1;
                    let rail_edge = ui.rail != 2 && rail_w > 0 && x == if rail_hid { 0 } else { rail_w };
                    let panel_edge = panel_w > 0 && x == rail_w + conv_w;
                    let wl = a.wheel as isize;
                    match kind {
                        Mouse::WheelUp if over_panel => pscroll = pscroll.saturating_sub(a.wheel),
                        Mouse::WheelDown if over_panel => pscroll += a.wheel,
                        Mouse::WheelUp if over_rail => ui.rail_top = ui.rail_top.saturating_sub(a.wheel),
                        Mouse::WheelDown if over_rail => ui.rail_top = (ui.rail_top + a.wheel).min(rail_lay.0.saturating_sub(rail_lay.1)),
                        Mouse::WheelUp if ui.ctx_view => ui.vscroll = ui.vscroll.saturating_sub(a.wheel),
                        Mouse::WheelDown if ui.ctx_view => ui.vscroll += a.wheel,
                        Mouse::WheelUp => top = scroll_by(top, max_top, -wl),
                        Mouse::WheelDown => top = scroll_by(top, max_top, wl),
                        Mouse::Down if rail_edge || panel_edge => {
                            ui.resize = Some(if rail_edge { 0 } else { 1 });
                            ui.sel = None;
                        }
                        Mouse::Down if in_conv && !ui.ctx_view && ui.picker.is_none() && !in_sbox && !(show_tabs && y == 0) => {
                            ui.sel = at(x, y).map(|p| Sel { a: p, b: p, moved: false, down: true });
                        }
                        Mouse::Down => {
                            ui.sel = None;
                            click = hits.iter().find(|(hy, x0, x1, _)| *hy == y && x >= *x0 && x < *x1).map(|h| h.3);
                        }
                        Mouse::Drag if ui.resize.is_some() => {
                            // a resize drag: the edge column is the new width, as a share that
                            // survives terminal resizes. The conversation keeps CONV_MIN; the
                            // drag stops there. Dragged below its floor the rail shuts (the
                            // handle stays); dragged back out it restores at the dragged width.
                            if ui.resize == Some(0) {
                                if rail_hid {
                                    if x >= 22 {
                                        ui.rail_off = false;
                                        ui.rail_share = x as f64 * 100.0 / cols.max(1) as f64;
                                    }
                                } else if (x as u16) < 22 {
                                    ui.rail_off = true;
                                } else {
                                    let maxw = cols.saturating_sub(CONV_MIN + panel_w).max(22);
                                    let w = (x as u16).clamp(22, 48).min(maxw);
                                    ui.rail_share = w as f64 * 100.0 / cols.max(1) as f64;
                                }
                            } else {
                                let maxw = cols.saturating_sub(CONV_MIN + rail_w).max(30);
                                let w = (cols.saturating_sub(x)).clamp(30, 60).min(maxw);
                                ui.panel_share = w as f64 * 100.0 / cols.max(1) as f64;
                            }
                            conv_cache = None;
                            panel_cache = None;
                        }
                        Mouse::Drag => {
                            if let Some(s) = ui.sel.as_mut().filter(|s| s.down) {
                                if let Some(p) = at(x.min(rail_w + conv_w.saturating_sub(2)), y) {
                                    s.b = p;
                                    s.moved |= s.b != s.a;
                                }
                                ui.drag_edge = if y == 0 { -1 } else if y as usize >= view_h.saturating_sub(1) { 1 } else { 0 };
                            }
                        }
                        Mouse::Up => {
                            ui.drag_edge = 0;
                            ui.resize = None;
                            if let Some(s) = ui.sel.as_mut().filter(|s| s.down) {
                                s.down = false;
                                if !s.moved {
                                    // a click, not a drag: the row's target, if it has one
                                    ui.sel = None;
                                    click = hits.iter().find(|(hy, x0, x1, _)| *hy == y && x >= *x0 && x < *x1).map(|h| h.3);
                                } else {
                                    let text = match pager.as_mut() {
                                        // pages dropped since the drag began are read and rendered again
                                        Some(pg) => {
                                            let (base, rows) = pg.rows(s.a.0.min(s.b.0), s.a.0.max(s.b.0), &v);
                                            selection_text(&rows, (s.a.0 - base, s.a.1), (s.b.0 - base, s.b.1))
                                        }
                                        None => conv_cache.as_ref().map(|c| selection_text(&c.2, s.a, s.b)).unwrap_or_default(),
                                    };
                                    if a.verify_copy && pager.is_some() {
                                        // the same selection over the whole file folded and rendered at once
                                        let mut wf = Fold::default();
                                        for l in std::fs::read_to_string(&a.path)?.lines() {
                                            if let Ok(e) = serde_json::from_str::<Value>(l) {
                                                wf.apply(&e);
                                            }
                                        }
                                        let whole = conversation(&wf, conv_w as usize - 2, &v);
                                        copy_check = Some(selection_text(&whole, s.a, s.b) == text);
                                        if let Some(p) = &a.stats {
                                            let _ = std::fs::write(format!("{p}.copied"), &text);
                                        }
                                    }
                                    if !text.is_empty() {
                                        let how = copy(term.backend_mut(), &text);
                                        ui.copied = Some((format!("✓ copied {} characters, {} lines · {how}", text.chars().count(), text.lines().count()), Instant::now()));
                                    }
                                }
                            }
                        }
                        Mouse::Other | Mouse::Move => {}
                    }
                }
            }
            // typing `/` or `@` at the input's start opens the panel; anything else closes it
            {
                let top_open = ui.top(f).is_some();
                completions::sync(&mut ui, top_open);
            }
            let Some(act) = click else { continue };
            changed = true;
            match act {
                Act::Pick(_) | Act::PickChip(_, _) | Act::PickAll | Act::PickRefresh => {
                    model_picker::click(&mut ui, act);
                }
                Act::Group(p, bi) if pager.is_some() => pager.as_mut().unwrap().toggle(p, bi, &v),
                Act::Group(ti, bi) => {
                    if let Block::Group(g) = &mut f.turns[ti].blocks[bi] {
                        g.open = !g.open;
                    }
                    conv_cache = None;
                }
                Act::Item(p, bi, ii) => {
                    if !v.exp.remove(&(p, bi, ii)) {
                        v.exp.insert((p, bi, ii));
                    }
                    // paged: only a toggle renders a page again, so toggling its group twice
                    // renders it with the row opened and the group as it was
                    if let Some(pg) = pager.as_mut() {
                        pg.toggle(p, bi, &v);
                        pg.toggle(p, bi, &v);
                    }
                }
                Act::Notice(i) => f.notices[i].2 = true,
                Act::End => top = None,
                Act::Rail(i) => {
                    // clicking a crashed card resumes it: a would-send row, then WORKING
                    let live = live_sessions(f, &ui, ts);
                    if live.get(i).is_some_and(|s| s.st == SState::Crashed) {
                        send(&mut ui, &mut cmds, "resume", Some(live[i].name), json!({}));
                        ui.resumed.insert(i);
                    }
                    ui.screen = i;
                    rail_ensure(&mut ui, &rail_lay, i);
                }
                Act::RailAll => ui.show_all = !ui.show_all,
                Act::RailVariant(v) => {
                    ui.rail = v.min(2);
                }
                Act::RailDensity(d) => {
                    ui.density = d.min(3);
                }
                Act::RailToggle => {
                    ui.rail_off = !ui.rail_off;
                }
                Act::RailDismiss(i) => {
                    ui.dismissed.insert(i);
                }
                Act::RailNew(p) => {
                    say(&mut ui, format!("→ would start a session in {p}"));
                }
                Act::RailDone(p) => {
                    say(&mut ui, format!("→ would open the session list for {p}"));
                }
                Act::Context => {
                    ui.search = None;
                    ui.picker = None;
                    ui.ctx_view = true;
                    ui.vscroll = 0;
                }
                Act::Back => {
                    ui.ctx_view = false;
                    ui.picker = None;
                }
                Act::Ext(i) => {
                    let id = v.lua.as_ref().map(|x| x.clicks.borrow()[i].clone()).unwrap_or_default();
                    say(&mut ui, format!("extension click: {id}"));
                }
                Act::Choice(i) => {
                    ui.choice = i;
                    approve(f, &mut ui, &mut cmds, ts, i);
                }
                Act::NextPending => ui.shown += 1,
                Act::Reopen => {
                    ui.aside.clear();
                    ui.shown = 0;
                }
                Act::Tab(k) => {
                    ui.form.tab = k;
                    ui.form.cur = 0;
                }
                Act::Opt(j) => {
                    ui.form.cur = j;
                    if let Some((_, p)) = ui.top(f) {
                        if let Asking::Form(fields) = &p.what {
                            let fields = fields.clone();
                            form_choose(&mut ui, &fields);
                        }
                    }
                }
                Act::Next => {
                    ui.form.tab += 1;
                    ui.form.cur = 0;
                }
                Act::Submit => submit_form(f, &mut ui, &mut cmds, ts),
                Act::Decline => decline_form(f, &mut ui, &mut cmds, ts),
                Act::QRow(i) => queue_pick(f, &mut ui, i),
                Act::QDrop(i) => queue_drop(f, &mut ui, &mut cmds, ts, i),
            }
        }
        // the last answer the turn waited on, once the replay has nothing more to play
        if std::mem::take(&mut ui.answered) && f.running && f.pending.is_empty() && *next >= events.len() {
            finish_turn(f, ts);
            changed = true;
        }
        // the resize pointer (OSC 22): col-resize over either handle or while dragging,
        // default otherwise; written only when it changes, so idle frames stay silent
        let rail_hx = if ui.rail != 2 && rail_w == 1 { 0 } else { rail_w };
        let want_col = ui.resize.is_some()
            || ptr.is_some_and(|(x, _)| (rail_w > 0 && ui.rail != 2 && x == rail_hx) || (panel_w > 0 && x == rail_w + conv_w));
        if want_col != ptr_col_resize {
            ptr_col_resize = want_col;
            let be = term.backend_mut();
            be.write_all(if want_col { b"\x1b]22;col-resize\x1b\\".as_slice() } else { b"\x1b]22;default\x1b\\".as_slice() })?;
            be.flush()?;
        }
        // a key or click that reached the panels or the input box may change the fold
        if changed {
            conv_cache = None;
            panel_cache = None;
        }
    }
    if ptr_col_resize {
        term.backend_mut().write_all(b"\x1b]22;default\x1b\\")?;
    }
    if pushed {
        term.backend_mut().write_all(b"\x1b[<u")?;
    }

    use std::fmt::Write as _;
    let mut s = String::new();
    let ms = |d: Option<Duration>| d.map_or("none".into(), |d| format!("{:.2}", d.as_secs_f64() * 1000.0));
    let _ = writeln!(s, "first_frame_ms\t{}", ms(first_frame));
    let _ = writeln!(s, "query_sent_ms\t{}", ms(det_sent));
    let _ = writeln!(s, "detected_ms\t{}", ms(det.map(|d| d.1)));
    let _ = writeln!(s, "keys\t{}", match det {
        Some((true, _)) => format!("kitty (flags {})", kitty_seen.unwrap_or(0)),
        Some((false, _)) => "legacy".into(),
        None => "no reply".into(),
    });
    if let Some(x) = &v.lua {
        let n = x.calls.get();
        let _ = writeln!(s, "lua_cached\t{}\nlua_calls\t{n}\nlua_calls_window\t{}", x.cached, n - lua_calls0);
        let _ = writeln!(s, "lua_us_per_call\t{:.2}\nlua_us_in_render\t{:.2}", x.ns.get() as f64 / 1000.0 / n.max(1) as f64, x.ns_in.get() as f64 / 1000.0 / n.max(1) as f64);
        let _ = writeln!(s, "lua_mem_bytes\t{lua_mem}");
    }
    if let Some(pg) = &pager {
        let q = |v: &mut Vec<Duration>, p: f64| {
            v.sort();
            ms(v.get(((v.len().max(1) - 1) as f64 * p).round() as usize).copied())
        };
        let _ = writeln!(s, "open_index_ms\t{}", ms(Some(open_index)));
        let _ = writeln!(s, "open_count_ms\t{}", ms(scans.first().copied()));
        let _ = writeln!(s, "pages\t{}\ntotal_rows\t{total_rows}\nresident_now\t{}\nresident_max\t{}", pg.pages.len(), pg.resident_count(), pg.resident_max);
        let _ = writeln!(s, "scans\t{}\nscan_max_ms\t{}", scans.len(), q(&mut scans, 1.0));
        let _ = writeln!(s, "load_frames\t{}\nvis_miss_frames\t{vis_miss_n}", load_ms.len());
        let _ = writeln!(s, "load_median_ms\t{}\nload_max_ms\t{}", q(&mut load_ms, 0.5), q(&mut load_ms, 1.0));
        let _ = writeln!(s, "ev_to_flush_n\t{}\nev_to_flush_median_ms\t{}\nev_to_flush_max_ms\t{}", ev_lat.len(), q(&mut ev_lat, 0.5), q(&mut ev_lat, 1.0));
        let _ = writeln!(s, "copy_matches_whole\t{}", copy_check.map_or("none".into(), |c| c.to_string()));
    }
    let Some((t0w, b0, f0, fl0, r0)) = window else { return Ok(s) };
    let secs = t0w.elapsed().as_secs_f64();
    let r1 = rusage();
    let bytes = BYTES.load(Relaxed) - b0;
    let fr = frames - f0;
    let _ = writeln!(s, "mode\t{}", if a.reduced { "reduced-motion" } else { "glimmer" });
    let _ = writeln!(s, "running\t{}", f.running);
    let _ = writeln!(s, "seconds\t{secs:.2}");
    let _ = writeln!(s, "frames\t{fr}\nfps\t{:.2}", fr as f64 / secs);
    let _ = writeln!(s, "bytes\t{bytes}\nbytes_per_s\t{:.0}\nbytes_per_frame\t{:.1}", bytes as f64 / secs, bytes as f64 / fr.max(1) as f64);
    let _ = writeln!(s, "flushes\t{}", FLUSHES.load(Relaxed) - fl0);
    let _ = writeln!(s, "cpu_s\t{:.4}\ncpu_pct\t{:.3}", r1.0 - r0.0, 100.0 * (r1.0 - r0.0) / secs);
    let _ = writeln!(s, "vol_csw\t{}\ninvol_csw\t{}", r1.1 - r0.1, r1.2 - r0.2);
    let _ = writeln!(s, "idle_wakeups\t{}\ninterrupt_wakeups\t{}", r1.3 - r0.3, r1.4 - r0.4);
    let _ = writeln!(s, "motions\t{motions}\nhover_changes\t{hover_changes}\nhover_changes_per_s\t{:.1}", hover_changes as f64 / secs);
    if a.audit {
        let _ = writeln!(s, "audit_frames\t{}\ncells_per_frame\t{:.2}\ncells_max\t{}\nrows_max\t{}\nrows_hist\t{:?}", audit.frames, audit.cells as f64 / audit.frames.max(1) as f64, audit.cells_max, audit.rows_max, audit.rows_hist);
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold_with(prompt: &str, reply: &str) -> Fold {
        let mut f = Fold::default();
        f.turns.push(Turn { prompt: prompt.into(), blocks: vec![Block::Text(reply.into())], ..Default::default() });
        f
    }
    fn find(rows: &[Row], s: &str) -> usize {
        rows.iter().position(|r| plain(r).contains(s)).unwrap()
    }

    #[test]
    fn a_copy_unwraps_lines_and_leaves_out_layout() {
        let f = fold_with("short", "alpha beta gamma delta epsilon zeta\n\n- one two three four five six seven");
        let rows = conversation(&f, 24, &View::default());
        let (r0, r1) = (find(&rows, "alpha"), find(&rows, "seven"));
        assert!(r1 > r0 + 2, "the text wrapped");
        let got = selection_text(&rows, (r0, 0), (r1, 99));
        assert_eq!(got, "alpha beta gamma delta epsilon zeta\n\n• one two three four five six seven");
        // backwards, and starting mid-line
        let c = plain(&rows[r0]).find("beta").unwrap();
        assert_eq!(selection_text(&rows, (r1, 99), (r0, c)), "beta gamma delta epsilon zeta\n\n• one two three four five six seven");
    }

    #[test]
    fn a_copy_of_the_bubble_is_the_prompt() {
        let prompt = "Fix it properly: wait on the lock file's creation instead of sleeping, then sweep every test";
        let f = fold_with(prompt, "ok");
        let rows = conversation(&f, 40, &View::default());
        let (r0, r1) = (find(&rows, "Fix it"), find(&rows, "every test"));
        assert!(r1 > r0);
        assert_eq!(selection_text(&rows, (r0, 0), (r1, 99)), prompt);
    }

    #[test]
    fn base64() {
        assert_eq!(b64(b"hello"), "aGVsbG8=");
        assert_eq!(b64(b"hi"), "aGk=");
        assert_eq!(b64(b"abc"), "YWJj");
    }

    #[test]
    fn links_and_paths() {
        let l = links_in("see crates/log/tests/lock.rs; and https://x.y/z.", "/w", "h");
        assert_eq!(l.len(), 2);
        assert_eq!(l[0], (4, 24, "file://h/w/crates/log/tests/lock.rs".into()));
        assert_eq!(l[1].2, "https://x.y/z");
        assert!(links_in("fix/wait-for-path and/or third_party/ //", "/w", "h").is_empty());
    }

    #[test]
    fn left_cut_keeps_the_file_name() {
        assert_eq!(left_cut("crates/doors/tests/serve_attach.rs", 24), "…/tests/serve_attach.rs");
        assert_eq!(left_cut("a/b.rs", 24), "a/b.rs");
    }

    #[test]
    fn scrolling_anchors_the_top_row_and_follows_again_at_the_end() {
        assert_eq!(scroll_by(None, 100, -3), Some(97));
        assert_eq!(scroll_by(Some(97), 100, 2), Some(99));
        assert_eq!(scroll_by(Some(99), 100, 1), None, "the end follows the output again");
        assert_eq!(scroll_by(Some(1), 100, -5), Some(0));
        assert_eq!(scroll_by(None, 100, 3), None);
    }

    #[test]
    fn a_running_group_line_is_one_row_whatever_is_in_flight() {
        let call = |target: &str, st: St| {
            Item::C(Call {
                name: "shell".into(),
                args: json!({ "command": target }),
                st,
                start: 0,
                end: None,
                err: None,
                lines: 0,
                exit: None,
                changes: vec![],
                content: String::new(),
                step: 1,
                asked: None,
            })
        };
        let long = "cargo test -p doors --test serve_attach -- --nocapture wait_for_path_sweeps_every_waiting_test";
        for n in 1..6 {
            let items = (0..n).map(|i| call(if i % 2 == 0 { long } else { "grep -rn sleep crates" }, St::Running)).collect();
            let g = Group { items, steps: 1, step_closed: false, open: false, last_ts: 1000 };
            let rows = group_lines(&g, Act::Group(0, 0), 60, &View::default());
            assert_eq!(rows.len(), 1, "{n} calls in flight");
            assert_eq!(width(&rows[0].spans), 60);
            assert!(plain(&rows[0]).ends_with('▸'));
        }
    }

    #[test]
    fn the_context_view_accounts_for_the_whole_context() {
        let mut f = fold_with("a prompt", "a reply of some length");
        f.ctx = 120_000;
        f.sys_chars = 4_000;
        let rows = context_view(&f, 90);
        let text: Vec<String> = rows.iter().map(plain).collect();
        assert!(text.iter().any(|l| l.contains("120k tokens")));
        assert!(text.iter().any(|l| l.contains("system prompt") && l.contains("~") && l.contains("1.0k")));
        assert!(text.iter().any(|l| l.contains("not attributed")));
        assert!(rows.iter().all(|r| width(&r.spans) <= 90));
    }

    fn shell_fold(exit: i64) -> Fold {
        let call = Call { name: "shell".into(), args: json!({ "command": "cargo test -p log" }), st: St::Completed, start: 0, end: Some(2500), err: None, lines: 42, exit: Some(exit), changes: vec![], content: String::new(), step: 1, asked: None };
        let g = Group { items: vec![Item::C(call)], steps: 1, step_closed: true, open: true, last_ts: 2500 };
        Fold { turns: vec![Turn { prompt: "p".into(), blocks: vec![Block::Group(g)], ..Default::default() }], ..Default::default() }
    }
    fn lua_view(src: &str, cached: bool) -> View {
        View { lua: Some(std::rc::Rc::new(lua::Ext::new("test.lua", src, cached).unwrap())), ..Default::default() }
    }
    const SHELL_ROW: &str = include_str!("../lua/shell_row.lua");

    #[test]
    fn search_select_and_click_work_on_rows_lua_drew() {
        let v = lua_view(SHELL_ROW, true);
        let rows = conversation(&shell_fold(1), 100, &v);
        let r = find(&rows, "exit 1");
        assert!(rows[r].lua.is_some(), "Lua drew the row");
        // search matches the text Lua drew
        assert_eq!(find_all(&rows, "exit 1").len(), 1);
        assert_eq!(find_all(&rows, "2.5s").len(), 1);
        // a copy of the row is the text behind its cells
        let t = selection_text(&rows, (r, 0), (r, 999));
        assert!(t.contains("exit 1  cargo test -p log") && t.contains("▰") && t.ends_with("42 lines"), "{t}");
        // the badge's click region covers its cells
        let p = plain(&rows[r]);
        let x = p[..p.find(" exit 1").unwrap()].width() as u16;
        let &(x0, x1, act) = rows[r].hot.first().unwrap();
        assert_eq!((x0, x1 - x0, act), (x, 8, Act::Ext(0)));
        assert_eq!(v.lua.as_ref().unwrap().clicks.borrow()[0], "badge exit 1 ");
    }

    #[test]
    fn a_failing_or_misshapen_renderer_falls_back_with_one_notice() {
        for src in [
            "return { tool = 'shell', render = function() error('boom') end }",
            "return { tool = 'shell', render = function() return 'text' end }",
            "return { tool = 'shell', render = function() return {{ { text = '\\27[31mred' } }} end }",
            "return { tool = 'shell', render = function() return {{ { text = 'x', fg = 'mauve' } }} end }",
        ] {
            let v = lua_view(src, false);
            for _ in 0..2 {
                let rows = conversation(&shell_fold(0), 100, &v);
                let r = find(&rows, "cargo test");
                assert!(rows[r].lua.is_none() && plain(&rows[r]).contains("exit 0 · 42 lines"), "the built-in row: {src}");
            }
            let x = v.lua.as_ref().unwrap();
            assert!(x.take_notice().is_some_and(|n| n.contains("shell renderer failed")), "{src}");
            assert!(x.take_notice().is_none(), "one notice");
        }
    }

    #[test]
    fn a_cached_renderer_runs_once_per_row_content() {
        for (cached, calls) in [(true, 1), (false, 3)] {
            let v = lua_view(SHELL_ROW, cached);
            for _ in 0..3 {
                conversation(&shell_fold(0), 100, &v);
            }
            assert_eq!(v.lua.as_ref().unwrap().calls.get(), calls);
        }
        // a new width or new content is a new row
        let v = lua_view(SHELL_ROW, true);
        conversation(&shell_fold(0), 100, &v);
        conversation(&shell_fold(0), 90, &v);
        conversation(&shell_fold(2), 90, &v);
        assert_eq!(v.lua.as_ref().unwrap().calls.get(), 3);
    }

    #[test]
    fn a_multi_choice_question_ends_with_a_next_row() {
        let fields = vec![
            json!({ "header": "One", "question": "q1", "multiSelect": true, "options": [{ "label": "a" }, { "label": "b" }] }),
            json!({ "header": "Two", "question": "q2", "options": [{ "label": "c" }] }),
            json!({ "header": "Three", "question": "q3", "multiSelect": true, "options": [{ "label": "d" }] }),
        ];
        let f = Fold::default();
        let p = Pending { rid: "r".into(), sid: String::new(), aid: "a".into(), what: Asking::Form(fields.clone()) };
        let mut ui = Ui::default();
        ui.form = Form { rid: "r".into(), sel: vec![vec![]; 3], text: vec![String::new(); 3], ..Default::default() };
        let texts = |ui: &Ui| form_panel(&f, ui, 0, &p, &fields, 100).iter().map(plain).collect::<Vec<_>>();
        let t = texts(&ui);
        let (words, next) = (t.iter().position(|l| l.contains("Type an answer")).unwrap(), t.iter().position(|l| l.contains("Next →")).unwrap());
        assert_eq!(next, words + 1, "Next follows the answer-in-words row");
        let rows = form_panel(&f, &ui, 0, &p, &fields, 100);
        assert!(rows[next].hot.iter().any(|h| h.2 == Act::Next));
        ui.form.tab = 1;
        assert!(!texts(&ui).iter().any(|l| l.contains("Next →") || l.contains("Review →")), "single choice has no Next row");
        ui.form.tab = 2;
        assert!(texts(&ui).iter().any(|l| l.contains("Review →")), "the last question reads Review");
    }

    fn fixture(path: &str) -> Vec<Value> {
        std::fs::read_to_string(path).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }
    fn fold_of(ev: &[Value]) -> Fold {
        let mut f = Fold::default();
        for e in ev {
            f.apply(e);
        }
        f
    }

    #[test]
    fn no_pending_opens_the_demo_settled() {
        let whole = fold_of(&fixture("fixtures/session.jsonl"));
        assert!(whole.running && !whole.pending.is_empty(), "the fixture ends waiting on the person");
        let f = fold_of(&settle(fixture("fixtures/session.jsonl")));
        assert!(!f.running && f.pending.is_empty());
        let t = f.turns.last().unwrap();
        assert!(matches!(t.blocks.last(), Some(Block::Done(s)) if s.starts_with("completed")));
        assert!(f.running_calls().is_empty(), "no call of the settled turn still runs");
        // a settled log is left as it is
        let again = settle(fixture("fixtures/idle.jsonl"));
        assert_eq!(again, fixture("fixtures/idle.jsonl"));
    }

    #[test]
    fn answering_the_last_request_ends_the_turn() {
        let mut f = fold_of(&fixture("fixtures/session.jsonl"));
        let mut ui = Ui::default();
        approve(&mut f, &mut ui, &mut None, 1, 0);
        ui.sync_form(&f);
        assert!(f.running && f.pending.len() == 1, "the form still waits");
        submit_form(&mut f, &mut ui, &mut None, 2);
        assert!(ui.answered && f.pending.is_empty());
        finish_turn(&mut f, 3);
        assert!(!f.running && f.running_calls().is_empty());
        let rows: Vec<String> = conversation(&f, 100, &View::default()).iter().map(plain).collect();
        let (reply, done) = (rows.iter().position(|l| l.contains("Stopping here for the demo")).unwrap(), rows.iter().rposition(|l| l.contains("▣ completed")).unwrap());
        assert!(done > reply, "the card closes with its ▣ line after the reply");
    }

    #[test]
    fn a_click_opens_a_ledger_row_or_a_thought() {
        let out: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        let call = Call { name: "shell".into(), args: json!({ "command": "cargo test" }), st: St::Completed, start: 0, end: Some(10), err: None, lines: 30, exit: Some(0), changes: vec![], content: out, step: 1, asked: None };
        let thought = Reason { text: "**Plan**\nfirst this, then that".into(), start: 0, end: Some(5), step: 1 };
        let g = Group { items: vec![Item::R(thought), Item::C(call)], steps: 1, step_closed: true, open: true, last_ts: 10 };
        let mut v = View::default();
        let text = |v: &View| group_lines(&g, Act::Group(0, 0), 80, v).iter().map(plain).collect::<Vec<_>>();
        let rows = group_lines(&g, Act::Group(0, 0), 80, &v);
        assert_eq!(rows[1].act, Some(Act::Item(0, 0, 0)));
        assert_eq!(rows[2].act, Some(Act::Item(0, 0, 1)));
        assert_eq!(rows.len(), 3);
        v.exp.insert((0, 0, 1));
        let t = text(&v);
        assert!(t.iter().any(|l| l.trim() == "line 20") && !t.iter().any(|l| l.trim() == "line 21"));
        assert!(t.last().unwrap().contains("… 10 more lines"));
        v.exp.insert((0, 0, 0));
        assert!(text(&v).iter().any(|l| l.contains("first this, then that")));
        // a thought with no call is one line, and a click on it shows its text
        let lone = Group { items: vec![Item::R(Reason { text: "why not".into(), start: 0, end: Some(1), step: 1 })], steps: 1, step_closed: true, open: false, last_ts: 1 };
        let rows = group_lines(&lone, Act::Group(2, 3), 80, &View::default());
        assert_eq!((rows.len(), rows[0].act), (1, Some(Act::Item(2, 3, 0))));
        let v = View { exp: HashSet::from([(2, 3, 0)]), ..Default::default() };
        assert!(plain(&group_lines(&lone, Act::Group(2, 3), 80, &v)[1]).contains("why not"));
    }

    #[test]
    fn a_ledger_row_opens_in_the_paged_mode() {
        let mut v = View { all_open: true, ..Default::default() };
        let (mut pg, _) = paged::Pager::open("fixtures/session.jsonl", 8).unwrap();
        pg.sync(124, &v, "");
        pg.ensure(0, pg.total(), &v);
        let (_, rows) = pg.rows(0, pg.total() - 1, &v);
        let (r, key) = rows.iter().enumerate().find_map(|(r, x)| match x.act {
            Some(Act::Item(p, bi, ii)) if plain(x).contains("read ") => Some((r, (p, bi, ii))),
            _ => None,
        }).unwrap();
        let before = pg.total();
        v.exp.insert(key);
        pg.toggle(key.0, key.1, &v);
        pg.toggle(key.0, key.1, &v);
        assert!(pg.total() > before, "the page renders again, taller");
        let (_, after) = pg.rows(0, pg.total() - 1, &v);
        assert_eq!(after[r].act, Some(Act::Item(key.0, key.1, key.2)));
        assert_eq!(after[r + 1].pre, 9, "the call's output sits under its row");
    }

    fn handoff_fold(outcome: Option<&str>) -> Fold {
        let e = |kind: &str, aid: &str, payload: Value| json!({ "kind": kind, "session_id": "s", "ts": 1000, "action_id": aid, "payload": payload });
        let usage = |n: u64| e("usage_recorded", "", json!({ "tokens": { "input": n, "cache_read": 0, "output": 0 } }));
        let mut ev = vec![
            e("turn_started", "", json!({ "input": [{ "type": "message", "content": [{ "type": "text", "text": "go" }], "source": "driver", "command_id": "c" }] })),
            e("assistant_message_started", "a1", json!({})),
            e("assistant_message_completed", "a1", json!({ "text": "before the handoff" })),
            usage(402_000),
        ];
        match outcome {
            // a tool-started handoff writes no handoff_started and makes no note request
            None => ev.push(e("handoff_completed", "", json!({ "outcome": "completed", "tokens_before": 402_000 }))),
            Some(o) => ev.extend([
                e("handoff_started", "", json!({ "trigger": "auto" })),
                e("assistant_message_started", "a2", json!({})),
                e("assistant_message_completed", "a2", json!({ "text": "THE NOTE" })),
                usage(403_000),
                e("handoff_completed", "", json!({ "outcome": o, "tokens_before": 402_000, "note": ["a2"] })),
            ]),
        }
        let after = if outcome == Some("failed") { 404_000 } else { 32_000 };
        ev.extend([usage(after), e("assistant_message_started", "a3", json!({})), e("assistant_message_completed", "a3", json!({ "text": "after the handoff" })), e("turn_completed", "", json!({ "outcome": "completed" }))]);
        fold_of(&ev)
    }

    #[test]
    fn a_handoff_breaks_the_card_with_a_band() {
        let f = handoff_fold(Some("completed"));
        let rows: Vec<String> = conversation(&f, 90, &View::default()).iter().map(plain).collect();
        let at = |s: &str| rows.iter().position(|l| l.contains(s));
        let (b, band, a, done) = (at("before the handoff").unwrap(), at("automatic at 400k").unwrap(), at("after the handoff").unwrap(), at("▣ completed").unwrap());
        assert!(b < band && band < a && a < done);
        assert!(rows[band].contains("402k → 32k"), "{}", rows[band]);
        assert!(at("THE NOTE").is_none(), "the note is not a reply");
        assert_eq!(rows.iter().filter(|l| !l.is_empty() && l.chars().all(|c| c == '▄')).count(), 3, "two cards and the band");
        // the context follows the size after the handoff, and counts nothing before it
        assert_eq!(f.ctx, 32_000);
        assert!(context_view(&f, 90).iter().any(|r| plain(r).contains("32k tokens")));
        // before the first request after it, the size after is not known
        let mut g = handoff_fold(Some("completed"));
        if let Some(Block::Handoff(h)) = g.turns[0].blocks.iter_mut().find(|b| matches!(b, Block::Handoff(_))) {
            h.after = None;
        }
        assert!(conversation(&g, 90, &View::default()).iter().any(|r| plain(r).contains("402k → …")));
        let failed: Vec<String> = conversation(&handoff_fold(Some("failed")), 90, &View::default()).iter().map(plain).collect();
        assert!(failed.iter().any(|l| l.contains("failed · the context is unchanged")));
        let tool: Vec<String> = conversation(&handoff_fold(None), 90, &View::default()).iter().map(plain).collect();
        assert!(tool.iter().any(|l| l.contains("the model handed off") && l.contains("402k → 32k")));
    }

    #[test]
    fn search_finds_every_match() {
        let f = fold_with("find wait", "wait_for_path, and Wait again");
        let rows = conversation(&f, 60, &View::default());
        assert_eq!(find_all(&rows, "wait").len(), 3);
    }
    /// A turn running a shell call that asks for approval, with `rule` on the request when given.
    fn asking(rule: Option<Value>) -> Fold {
        let e = |kind: &str, aid: Option<&str>, payload: Value| {
            let mut e = json!({ "kind": kind, "session_id": "s_main", "ts": 1, "schema_version": 1, "payload": payload });
            if let Some(a) = aid {
                e["action_id"] = json!(a);
            }
            e
        };
        let mut req = json!({ "request_id": "r1", "effects": ["executes"], "reversible": false, "paths": [], "step": "review" });
        if let Some(r) = rule {
            req["rule"] = r;
        }
        fold_of(&[
            e("turn_started", None, json!({ "input": [{ "type": "message", "content": [{ "type": "text", "text": "go" }], "source": "driver", "command_id": "c" }] })),
            e("tool_call_requested", Some("a1"), json!({ "name": "shell", "arguments": { "command": "npm test -- --watch" }, "provider_id": "p" })),
            e("permission_requested", Some("a1"), req),
        ])
    }
    /// Runs `act` and returns the command lines it wrote, parsed.
    fn sent(act: impl FnOnce(&mut Ui, &mut Option<std::fs::File>)) -> Vec<Value> {
        let path = std::env::temp_dir().join(format!("tui-prototype-cmds-{}-{:?}", std::process::id(), std::thread::current().id()));
        let mut cmds = Some(std::fs::File::create(&path).unwrap());
        act(&mut Ui::default(), &mut cmds);
        let out = std::fs::read_to_string(&path).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let _ = std::fs::remove_file(&path);
        out
    }
    #[test]
    fn approval_replies_follow_invocation_md() {
        let rule = json!({ "subject": "npm test -- --watch", "prefix": "npm test" });
        let mut want = vec![
            (CH_ONCE, json!({ "request_id": "r1", "decision": "allow" })),
            (CH_SESSION, json!({ "request_id": "r1", "decision": "allow", "remember": { "scope": "session", "prefix": "npm test" } })),
            (CH_PROJECT, json!({ "request_id": "r1", "decision": "allow", "remember": { "scope": "project", "prefix": "npm test" } })),
            (CH_DENY, json!({ "request_id": "r1", "decision": "deny", "feedback": "not that" })),
        ];
        for (choice, args) in want.drain(..) {
            let mut f = asking(Some(rule.clone()));
            let lines = sent(|ui, cmds| {
                ui.feedback = if choice == CH_DENY { "not that".into() } else { String::new() };
                approve(&mut f, ui, cmds, 1, choice);
            });
            assert_eq!(lines, vec![json!({ "id": "c_c001", "command": "reply", "args": args })]);
            assert!(f.pending.is_empty());
        }
        // no rule on the request: the remembering choices send nothing
        let mut f = asking(None);
        assert!(sent(|ui, cmds| approve(&mut f, ui, cmds, 1, CH_SESSION)).is_empty());
        assert_eq!(f.pending.len(), 1);
        assert_eq!(choices(false), vec![CH_ONCE, CH_DENY]);
    }

    #[test]
    fn steering_commands_and_cancel_follow_the_docs() {
        let mut f = asking(None);
        f.session_id = "s_main".into();
        let lines = sent(|ui, cmds| {
            ui.input = "use the other file".into();
            enter(&mut f, ui, cmds, 1);
            ui.input = "and then run it".into();
            enter(&mut f, ui, cmds, 1);
            queue_pick(&f, ui, 0);
            ui.input = "use the third file".into();
            enter(&mut f, ui, cmds, 1);
            queue_drop(&mut f, ui, cmds, 1, 1);
            ui.input = "and then run it again".into();
            enter(&mut f, ui, cmds, 1);
            interrupt(&mut f, ui, cmds, 2);
        });
        let text = |t: &str| json!([{ "type": "text", "text": t }]);
        assert_eq!(lines[0], json!({ "id": "c_c001", "command": "steer", "args": { "content": text("use the other file") } }));
        assert_eq!(lines[2], json!({ "id": "c_c003", "command": "steer_amend", "args": { "command_id": "c_c001", "content": text("use the third file") } }));
        assert_eq!(lines[3], json!({ "id": "c_c004", "command": "steer_drop", "args": { "command_id": "c_c002" } }));
        assert_eq!(lines[5], json!({ "id": "c_c006", "command": "cancel" }));
        // the pending approval is denied by cancel, the call is cancelled, the queue starts the next turn
        assert!(f.pending.is_empty() && f.running && f.queue.is_empty());
        let t = f.turns.last().unwrap();
        assert_eq!(t.prompt, "use the third file\n\nand then run it again");
        assert!(matches!(f.turns[0].blocks.last(), Some(Block::Done(s)) if s.starts_with("interrupted")));
    }
}
