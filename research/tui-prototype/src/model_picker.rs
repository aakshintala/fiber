//! The model picker (#1629): a swapped view over the conversation area, like the
//! context breakdown. Fixture data only: the stream names just the current model,
//! so the providers, roles, thinking levels and rebuild costs below are made up.

use crate::cases::{Case, Surface};
use crate::input::{Key, Mods};
use crate::{bold, dim, fg, hot_row, panel, row, sp, width, Act, Row, Ui, BLUE, ORANGE, SPIN};
use ratatui::style::{Modifier, Style};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use unicode_width::UnicodeWidthStr;

/// `--static`: the spinner stays a still glyph.
static STILL: AtomicBool = AtomicBool::new(false);

pub fn set_still(v: bool) {
    STILL.store(v, Relaxed);
}

pub struct Provider {
    pub name: &'static str,
    pub updated: &'static str,
    pub models: Vec<Model>,
}

pub struct Model {
    pub id: &'static str,
    /// the display name the filter matches alongside provider and id
    pub name: &'static str,
    pub roles: Vec<&'static str>,
    pub current: bool,
    pub levels: Vec<&'static str>,
    pub level: Option<&'static str>,
    pub rebuild: &'static str,
}

/// Three providers, twelve models. The current model is flat index 0.
pub fn fixture() -> Vec<Provider> {
    vec![
        Provider {
            name: "anthropic",
            updated: "2m ago",
            models: vec![
                Model {
                    id: "claude-opus-5-5",
                    name: "Claude Opus 5.5",
                    roles: vec!["main"],
                    current: true,
                    levels: vec!["low", "medium", "high", "xhigh"],
                    level: Some("high"),
                    rebuild: "—",
                },
                Model {
                    id: "claude-sonnet-5-5",
                    name: "Claude Sonnet 5.5",
                    roles: vec!["reviewer"],
                    current: false,
                    levels: vec!["low", "medium", "high"],
                    level: Some("medium"),
                    rebuild: "~84k tokens · $0.31",
                },
                Model {
                    id: "claude-haiku-5-5",
                    name: "Claude Haiku 5.5",
                    roles: vec!["explorer", "small"],
                    current: false,
                    levels: vec!["low", "medium"],
                    level: Some("low"),
                    rebuild: "~61k tokens · $0.12",
                },
            ],
        },
        Provider {
            name: "openai-codex",
            updated: "9m ago",
            models: vec![
                Model {
                    id: "gpt-6.1-sol",
                    name: "GPT 6.1 Sol",
                    roles: vec!["reviewer"],
                    current: false,
                    levels: vec!["low", "medium", "high", "xhigh"],
                    level: Some("high"),
                    rebuild: "~92k tokens · $0.44",
                },
                Model {
                    id: "gpt-6-luna",
                    name: "GPT 6 Luna",
                    roles: vec!["explorer"],
                    current: false,
                    levels: vec!["low", "medium", "high"],
                    level: Some("medium"),
                    rebuild: "~92k tokens · $0.28",
                },
                Model {
                    id: "gpt-6-sol-mini",
                    name: "GPT 6 Sol Mini",
                    roles: vec!["small"],
                    current: false,
                    levels: vec!["low", "medium"],
                    level: Some("low"),
                    rebuild: "~70k tokens · $0.09",
                },
                Model {
                    id: "gpt-6-nano",
                    name: "GPT 6 Nano",
                    roles: vec!["small"],
                    current: false,
                    levels: vec![],
                    level: None,
                    rebuild: "~58k tokens · $0.03",
                },
                Model {
                    id: "gpt-6-sol",
                    name: "GPT 6 Sol",
                    roles: vec!["main"],
                    current: false,
                    levels: vec!["low", "medium", "high", "xhigh"],
                    level: Some("xhigh"),
                    rebuild: "~92k tokens · $0.51",
                },
            ],
        },
        Provider {
            name: "google",
            updated: "1h ago",
            models: vec![
                Model {
                    id: "gemini-3-pro",
                    name: "Gemini 3 Pro",
                    roles: vec!["explorer"],
                    current: false,
                    levels: vec!["low", "medium", "high"],
                    level: Some("medium"),
                    rebuild: "~88k tokens · $0.22",
                },
                Model {
                    id: "gemini-3-flash",
                    name: "Gemini 3 Flash",
                    roles: vec!["small"],
                    current: false,
                    levels: vec!["low", "medium"],
                    level: Some("low"),
                    rebuild: "~64k tokens · $0.06",
                },
                Model {
                    id: "gemini-3-lite",
                    name: "Gemini 3 Lite",
                    roles: vec!["small"],
                    current: false,
                    levels: vec![],
                    level: None,
                    rebuild: "~49k tokens · $0.01",
                },
                Model {
                    id: "gemma-3-27b",
                    name: "Gemma 3 27B",
                    roles: vec!["explorer"],
                    current: false,
                    levels: vec!["low"],
                    level: Some("low"),
                    rebuild: "~77k tokens · $0.01",
                },
            ],
        },
    ]
}

/// The five models `scoped_models` holds.
const SCOPED: [usize; 5] = [0, 1, 3, 8, 9];

#[derive(Default)]
pub struct State {
    /// the focused model, as a flat index over the fixture
    pub focus: usize,
    /// the focused thinking chip of the focused model
    pub chip: Option<usize>,
    /// the model marked "this session only", by flat index
    pub session_only: Option<usize>,
    /// the filter query typed over the picker; empty means unfiltered
    pub query: String,
    /// the `scoped_models` set, as flat indices; empty means unscoped
    pub scoped: Vec<usize>,
    pub show_all: bool,
    /// providers refreshing in the background, by provider index
    pub refreshing: Vec<usize>,
    /// the frame's tick, for the spinner
    pub tick: u64,
    /// `--static`: no animation
    pub still: bool,
}

/// Every `--picker` case.
pub(crate) const CASES: &[Case<State>] = &[
    Case { name: "list", help: "providers and models, current marked", check: "three providers with a dozen models between them, roles on the rows, `● current` on claude-opus-5-5, its level chips on the row below; a centred panel with ▄ ▀ edges and the ▌ stripe, dim providers with a blank row between sections, the focused model `›` on a full-width accent bar with its rebuild cost right-aligned, and a bold-key legend foot.", build: base },
    // after a click on the thinking chip: the current level focused
    Case { name: "levels", help: "the current model's thinking chips focused", check: "the current model's thinking chips focused (`[high]`), the rest dim; same panel, bar and legend.", build: || State { chip: Some(2), ..base() } },
    Case { name: "scoped", help: "five scoped models only", check: "five models only, a `scoped · 5 of 12` chip and a `[show all]` toggle; same panel, bar and legend.", build: || State { scoped: SCOPED.to_vec(), ..base() } },
    Case { name: "scoped-all", help: "all models, scoped ones marked", check: "all twelve models, the scoped five marked `· scoped`; same panel, bar and legend.", build: || State { scoped: SCOPED.to_vec(), show_all: true, ..base() } },
    Case { name: "refreshing", help: "one provider refreshing", check: "openai-codex reads `⟳ refreshing` with a still spinner glyph, the other two `updated … ago`, and a `⟳ refresh all` button sits at the controls row's right end; same panel, bar and legend.", build: || State { refreshing: vec![1], ..base() } },
    // a non-current model focused, the `s` mark applied
    Case { name: "session-only", help: "a model picked for this session only", check: "claude-sonnet-5-5 `›` on the accent bar with `ⓢ this session only · nothing saved` under it and its rebuild cost on its row; same panel and legend.", build: || State { focus: 1, session_only: Some(1), ..base() } },
    Case { name: "filtered", help: "a typed query narrowing the list, matches underlined", check: "the query `mini` in bold after `›` with a block cursor, four models across openai-codex and google, the matched id chars underlined and bold on and off the accent bar, a `4 of 12 models` chip; same panel and legend.", build: || State { focus: 5, query: "mini".into(), ..base() } },
    Case { name: "filtered-empty", help: "a query nothing matches", check: "the query `zzz` in bold after `›` with a block cursor, one muted `No models match` line and no provider sections, a `0 of 12 models` chip; same panel and legend.", build: || State { query: "zzz".into(), ..base() } },
];

/// `--picker`, for `--help` and `check/model-picker.md`.
pub(crate) const SURFACE: Surface = Surface { flag: "--picker", file: "model-picker", title: "Model picker (#1629)", docs: || crate::cases::docs(CASES) };

fn base() -> State {
    State { still: STILL.load(Relaxed), ..Default::default() }
}

/// The picker's choice as an id and a level: the focused model's id, and
/// the focused chip's level, else the model's own level.
pub fn chosen(s: &State) -> (&'static str, &'static str) {
    let mut i = 0;
    for p in fixture() {
        for m in &p.models {
            if i == s.focus {
                let level = match s.chip {
                    Some(j) => m.levels.get(j).copied().unwrap_or(""),
                    None => m.level.unwrap_or(""),
                };
                return (m.id, level);
            }
            i += 1;
        }
    }
    ("", "")
}

/// Opens the picker on one model: `base` focused on the model's flat
/// index (0 when unknown), the chip the index of `level` in that model's
/// levels.
pub fn opened_at(model: &str, level: Option<&str>) -> State {
    let mut s = base();
    let mut i = 0;
    for p in fixture() {
        for m in &p.models {
            if m.id == model {
                s.focus = i;
                s.chip = level.and_then(|l| m.levels.iter().position(|&x| x == l));
                return s;
            }
            i += 1;
        }
    }
    s
}

pub fn for_case(case: &str) -> State {
    crate::cases::lookup(CASES, case).unwrap_or_else(|| panic!("--picker {}", crate::cases::names(CASES).replace(", ", "|")))
}

fn count() -> usize {
    fixture().iter().map(|p| p.models.len()).sum()
}

/// The text one query token must subsequence-match: provider, id and
/// display name, so a token can name any of the three.
fn haystack(provider: &str, m: &Model) -> String {
    format!("{provider} {provider}/{} {provider} {} {}", m.id, m.id, m.name)
}

/// Whether every whitespace-separated token of the query is a
/// case-insensitive subsequence of the haystack; an empty query matches all.
fn matches(query: &str, haystack: &str) -> bool {
    let hay: Vec<char> = haystack.to_lowercase().chars().collect();
    query.split_whitespace().all(|tok| {
        let mut i = 0;
        tok.to_lowercase().chars().all(|c| match hay[i..].iter().position(|&h| h == c) {
            Some(j) => {
                i += j + 1;
                true
            }
            None => false,
        })
    })
}

/// The char positions in the model id to underline: each token's greedy
/// subsequence match, unioned. A token the id alone cannot match (it named
/// the provider or display name instead) contributes no positions.
fn id_hits(query: &str, id: &str) -> Vec<usize> {
    let low: Vec<char> = id.to_lowercase().chars().collect();
    let mut hits = vec![false; low.len()];
    for tok in query.split_whitespace() {
        let mut i = 0;
        let mut local = vec![];
        let mut ok = true;
        for c in tok.to_lowercase().chars() {
            match low[i..].iter().position(|&h| h == c) {
                Some(j) => {
                    local.push(i + j);
                    i += j + 1;
                }
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            for p in local {
                hits[p] = true;
            }
        }
    }
    hits.iter().enumerate().filter_map(|(i, &h)| h.then_some(i)).collect()
}

/// The flat indices on screen: the scoped set, or everything.
pub fn visible(s: &State) -> Vec<usize> {
    let all = fixture();
    let mut flat: Vec<(&str, &Model)> = vec![];
    for p in &all {
        for m in &p.models {
            flat.push((p.name, m));
        }
    }
    let n = flat.len();
    let base: Vec<usize> = if s.scoped.is_empty() || s.show_all {
        (0..n).collect()
    } else {
        s.scoped.iter().copied().filter(|&i| i < n).collect()
    };
    // The filter keeps fixture order: it only drops rows.
    base.into_iter()
        .filter(|&i| matches(&s.query, &haystack(flat[i].0, flat[i].1)))
        .collect()
}

/// The focused model's level count and its current level's index.
fn model_levels(focus: usize) -> (usize, usize) {
    let mut i = 0;
    for p in fixture() {
        for m in &p.models {
            if i == focus {
                let cur = m
                    .level
                    .and_then(|l| m.levels.iter().position(|&x| x == l))
                    .unwrap_or(0);
                return (m.levels.len(), cur);
            }
            i += 1;
        }
    }
    (0, 0)
}

impl State {
    /// After the visible rows change the focus stays when still on screen
    /// and otherwise moves to the first visible row; the chip never survives.
    fn keep_focus(&mut self) {
        let v = visible(self);
        let first = v.first().copied().unwrap_or(self.focus);
        if !v.contains(&self.focus) {
            self.focus = first;
        }
        self.chip = None;
    }
    pub fn focus_up(&mut self) {
        let v = visible(self);
        if v.is_empty() {
            return;
        }
        let i = v.iter().position(|&x| x == self.focus).unwrap_or(0);
        self.focus = v[i.saturating_sub(1)];
        self.chip = None;
    }
    pub fn focus_down(&mut self) {
        let v = visible(self);
        if v.is_empty() {
            return;
        }
        let i = v.iter().position(|&x| x == self.focus).unwrap_or(0);
        self.focus = v[(i + 1).min(v.len().saturating_sub(1))];
        self.chip = None;
    }
    pub fn chip_left(&mut self) {
        let (n, cur) = model_levels(self.focus);
        if n == 0 {
            return;
        }
        self.chip = Some(self.chip.unwrap_or(cur).saturating_sub(1));
    }
    pub fn chip_right(&mut self) {
        let (n, cur) = model_levels(self.focus);
        if n == 0 {
            return;
        }
        self.chip = Some((self.chip.unwrap_or(cur) + 1).min(n - 1));
    }
    pub fn mark_session_only(&mut self) {
        self.session_only = Some(self.focus);
    }
    pub fn toggle_show_all(&mut self) {
        if self.scoped.is_empty() {
            return;
        }
        self.show_all = !self.show_all;
        self.keep_focus();
    }
    pub fn refresh_all(&mut self) {
        self.refreshing = (0..fixture().len()).collect();
    }
}

fn spin(s: &State) -> &'static str {
    if s.still {
        "○"
    } else {
        SPIN[(s.tick as usize) % SPIN.len()]
    }
}

/// A provider section header: dim, never bold beside the title.
fn provider_row(p: &Provider, refreshing: bool, s: &State) -> Row {
    let (what, st) = if refreshing {
        (format!("⟳ refreshing {} ", spin(s)), fg(ORANGE))
    } else {
        (format!("· updated {} ", p.updated), dim())
    };
    row(vec![sp(p.name, dim()), sp(" ", Style::new()), sp(what, st)])
}

fn model_row(f: usize, m: &Model, focused: bool, scoped_mark: bool, hits: &[usize]) -> Row {
    let mut spans = vec![
        sp(if focused { "› " } else { "  " }, if focused { bold() } else { Style::new() }),
    ];
    // The id reads as one word even underlined per matched char: matched
    // chars carry underline plus bold over the row's own colour.
    for (i, ch) in m.id.chars().enumerate() {
        let mut st = if focused {
            bold().patch(fg(BLUE))
        } else {
            fg(BLUE)
        };
        if hits.contains(&i) {
            st = st.patch(bold()).add_modifier(Modifier::UNDERLINED);
        }
        spans.push(sp(ch.to_string(), st));
    }
    spans.push(sp(" ", Style::new()));
    for r in &m.roles {
        spans.push(sp(format!("[{r}] "), dim()));
    }
    if m.current {
        spans.push(sp("● current ", bold().patch(fg(BLUE))));
    }
    if scoped_mark {
        spans.push(sp("· scoped ", dim()));
    }
    spans.extend([sp("\t", Style::new()), sp(m.rebuild, dim())]);
    // The focused model reads bold throughout, like every focused choice.
    let rest = 2 + m.id.chars().count();
    if focused {
        for s in spans.iter_mut().skip(rest) {
            s.style = s.style.patch(bold());
        }
    }
    Row { spans, act: Some(Act::Pick(f)), ..Default::default() }
}

fn chips_row(s: &State, f: usize, m: &Model, focused: bool) -> Row {
    let mut parts: Vec<(ratatui::text::Span<'static>, Option<Act>)> =
        vec![(sp("    ", Style::new()), None)];
    if m.levels.is_empty() {
        parts.push((sp("fixed thinking", dim()), None));
    } else {
        parts.push((sp("thinking ", dim()), None));
        for (j, l) in m.levels.iter().enumerate() {
            let hot = focused && s.chip == Some(j);
            let label = if hot {
                format!("[{l}]")
            } else {
                (*l).to_string()
            };
            let st = if !focused {
                dim()
            } else if hot || m.level == Some(*l) {
                bold().patch(fg(ORANGE))
            } else {
                fg(ORANGE)
            };
            if j > 0 {
                parts.push((sp(" ", Style::new()), None));
            }
            parts.push((sp(label, st), Some(Act::PickChip(f, j))));
        }
    }
    hot_row(parts)
}

fn note_row() -> Row {
    row(vec![sp("    ⓢ this session only · nothing saved", dim())])
}

/// The filter line above the controls: a muted placeholder until typed,
/// then the query in bold after a muted marker, always with a block cursor.
fn search_row(query: &str) -> Row {
    if query.is_empty() {
        row(vec![sp("Type to search ", dim()), sp("█", dim())])
    } else {
        row(vec![sp("› ", dim()), sp(query.to_string(), bold()), sp("█", dim())])
    }
}

/// The controls: the scope chip, the show-all toggle, and the refresh button.
fn controls(s: &State, w: usize) -> Row {
    let scope = if !s.query.is_empty() {
        format!(" {} of {} models ", visible(s).len(), count())
    } else if s.scoped.is_empty() {
        format!(" {} models ", visible(s).len())
    } else {
        format!(" scoped · {} of {} ", visible(s).len(), count())
    };
    let mut parts: Vec<(ratatui::text::Span<'static>, Option<Act>)> =
        vec![(sp(scope, dim()), None)];
    if !s.scoped.is_empty() {
        let label = if s.show_all {
            "[show scoped]"
        } else {
            "[show all]"
        };
        parts.push((sp(" ", Style::new()), None));
        parts.push((sp(label, bold()), Some(Act::PickAll)));
    }
    let right = "⟳ refresh all ";
    let pad = w.saturating_sub(
        width(&parts.iter().map(|p| p.0.clone()).collect::<Vec<_>>()) + right.width(),
    );
    parts.push((sp(" ".repeat(pad), Style::new()), None));
    parts.push((sp(right, dim()), Some(Act::PickRefresh)));
    hot_row(parts)
}

/// The foot legend: keys bold, labels muted, naming panel keys the body
/// never lists as choices.
fn footer() -> Row {
    panel::footer_legend(&[
        ("↑↓", "move"),
        ("←→", "levels"),
        ("enter", "choose"),
        ("tab", "show all"),
        ("ctrl+s", "session"),
        ("ctrl+r", "refresh"),
        ("esc", "close"),
    ])
}

/// The panel's body at an inner width: controls, dim provider sections with
/// a blank row between them, and the visible models, the focused one barred.
fn body(s: &State, inner: usize) -> Vec<Row> {
    let all = fixture();
    let mut at: Vec<(usize, usize)> = vec![];
    for (pi, p) in all.iter().enumerate() {
        for mi in 0..p.models.len() {
            at.push((pi, mi));
        }
    }
    let vis = visible(s);
    let mut out = vec![search_row(&s.query), controls(s, inner)];
    if vis.is_empty() {
        out.push(row(vec![sp("No models match", dim())]));
        return out;
    }
    let pos = vis.iter().position(|&i| i == s.focus).unwrap_or(0);
    let mut first_section = true;
    for (pi, p) in all.iter().enumerate() {
        let here: Vec<usize> = vis.iter().copied().filter(|&i| at[i].0 == pi).collect();
        if here.is_empty() {
            continue;
        }
        if !first_section {
            out.push(row(vec![]));
        }
        first_section = false;
        out.push(provider_row(p, s.refreshing.contains(&pi), s));
        for &f in &here {
            let (ppi, mi) = at[f];
            let m = &all[ppi].models[mi];
            let focused = vis[pos] == f;
            let scoped_mark = s.show_all && s.scoped.contains(&f);
            let hits = id_hits(&s.query, m.id);
            if focused {
                out.extend(panel::bar(vec![model_row(f, m, focused, scoped_mark, &hits)]));
            } else {
                out.push(model_row(f, m, focused, scoped_mark, &hits));
            }
            out.push(chips_row(s, f, m, focused));
            if s.session_only == Some(f) {
                out.push(note_row());
            }
        }
    }
    out
}

/// The rows below the header: a centred panel that never fills the area.
/// Interaction is unchanged: rows stay area-wide, so every click target
/// keeps its coordinates.
pub fn view(s: &State, w: usize) -> Vec<Row> {
    let probe = body(s, 10_000);
    // The search and controls rows pad to their width, so neither sizes the panel.
    // The legend sizes unfitted now, straight from its row.
    let legend_w = width(&footer().spans);
    let natural = probe.iter().skip(2).map(|r| width(&r.spans)).max().unwrap_or(0).max(legend_w);
    // The legend always fits: the preferred width stretches past the usual
    // cap rather than cutting the foot.
    let prefer = w.saturating_sub(4).min(96).max(legend_w);
    let panel_w = panel::fit_width(natural, prefer, w);
    let inner = panel::inner_w(panel_w);
    let rows = panel::frame(None, body(s, inner), Some(footer()), panel_w);
    panel::centre(rows, panel_w, w)
}

/// The picker's keys. True when the key was consumed.
pub fn on_key(ui: &mut Ui, k: Key, m: Mods) -> bool {
    if ui.picker.is_none() {
        if k == Key::Char('l') && m.ctrl && !m.alt && !m.sup {
            ui.ctx_view = false;
            ui.picker = Some(for_case("list"));
            ui.vscroll = 0;
            return true;
        }
        if k == Key::Enter && ui.qsel.is_none() && ui.input.trim() == "/model" {
            ui.input.clear();
            ui.ctx_view = false;
            ui.picker = Some(for_case("list"));
            ui.vscroll = 0;
            return true;
        }
        return false;
    }
    // Esc clears a typed query first and only closes on the second press.
    if k == Key::Esc && ui.picker.as_ref().is_some_and(|p| !p.query.is_empty()) {
        if let Some(p) = ui.picker.as_mut() {
            p.query.clear();
            p.keep_focus();
        }
        return true;
    }
    // Enter with no matching row keeps the picker open instead of choosing.
    if k == Key::Enter
        && !m.ctrl
        && !m.alt
        && !m.sup
        && ui.picker.as_ref().is_some_and(|p| visible(p).is_empty())
    {
        return true;
    }
    // closing keys first, so no borrow of the picker is live
    if k == Key::Esc || k == Key::Enter || (k == Key::Char('l') && m.ctrl) {
        ui.picker = None;
        return true;
    }
    let plain = !m.ctrl && !m.alt && !m.sup;
    let Some(p) = ui.picker.as_mut() else {
        return true;
    };
    match k {
        Key::Up if !m.alt && !m.ctrl => p.focus_up(),
        Key::Down if !m.alt && !m.ctrl => p.focus_down(),
        Key::Left => p.chip_left(),
        Key::Right => p.chip_right(),
        Key::Backspace if plain => {
            p.query.pop();
            p.keep_focus();
        }
        // Every bare letter filters; the old bare-letter keys moved to
        // Ctrl+S (session only), Tab (show all) and Ctrl+R (refresh).
        Key::Char('s') if m.ctrl && !m.alt && !m.sup => p.mark_session_only(),
        Key::Tab if !m.ctrl && !m.alt && !m.sup => p.toggle_show_all(),
        Key::Char('r') if m.ctrl && !m.alt && !m.sup => p.refresh_all(),
        Key::Char(c) if plain => {
            p.query.push(c);
            p.keep_focus();
        }
        _ => return false,
    }
    true
}

/// Clicks on the picker's rows. True when the click was consumed.
pub fn click(ui: &mut Ui, a: Act) -> bool {
    let Some(p) = ui.picker.as_mut() else {
        return false;
    };
    match a {
        Act::Pick(i) => {
            p.focus = i;
            p.chip = None;
        }
        Act::PickChip(i, j) => {
            p.focus = i;
            p.chip = Some(j);
        }
        Act::PickAll => p.toggle_show_all(),
        Act::PickRefresh => p.refresh_all(),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plain;

    fn text(rows: &[Row]) -> String {
        rows.iter().map(plain).collect::<Vec<_>>().join("\n")
    }
    fn picks(rows: &[Row]) -> usize {
        rows.iter()
            .filter(|r| matches!(r.act, Some(Act::Pick(_))))
            .count()
    }

    #[test]
    fn the_list_names_providers_roles_chips_and_costs() {
        let t = text(&view(&for_case("list"), 100));
        for p in ["anthropic", "openai-codex", "google"] {
            assert!(t.contains(p), "missing provider {p}");
        }
        for r in ["[main]", "[reviewer]", "[explorer]", "[small]"] {
            assert!(t.contains(r), "missing role {r}");
        }
        assert!(t.contains("● current"), "the current model is not marked");
        // the focused model's chips: the current model supports low through xhigh
        for l in ["low", "medium", "high", "xhigh"] {
            assert!(t.contains(l), "missing thinking chip {l}");
        }
        assert!(
            t.contains("~84k tokens · $0.31"),
            "missing the rebuild cost"
        );
        assert!(t.contains("—"), "the current model shows no rebuild cost");
        assert_eq!(picks(&view(&for_case("list"), 100)), 12);
    }

    #[test]
    fn the_picker_is_a_centred_panel_with_a_bar_and_legend() {
        let rows = view(&for_case("list"), 120);
        let t = text(&rows);
        assert!(t.contains("\u{2584}"), "no top edge");
        assert!(t.contains("\u{2580}"), "no bottom edge");
        assert!(t.contains("\u{258c}"), "no stripe");
        assert!(t.contains("\u{203a} "), "no gutter marker");
        // Centred: the edge run neither starts at the margin nor fills the row.
        let edge = t.split('\n').find(|l| l.contains("\u{2584}")).unwrap();
        let run = edge.chars().filter(|&c| c == '\u{2584}').count();
        assert!(edge.starts_with(' '), "panel flush left");
        assert!(run < 120, "panel fills the area");
        // The focused model rides a full-width accent bar inside blank margins.
        let picked = rows.iter().find(|r| matches!(r.act, Some(Act::Pick(0)))).unwrap();
        assert_eq!(picked.spans[0].style.bg, None, "no margin");
        let ink: Vec<_> = picked.spans.iter().filter(|s| !s.content.trim().is_empty()).collect();
        assert!(!ink.is_empty());
        assert!(ink.iter().all(|s| s.style.bg == Some(BLUE)), "bar is not full width");
        // Only the focused model rides the bar.
        let barred = rows
            .iter()
            .filter(|r| r.spans.iter().any(|s| s.style.bg == Some(BLUE)))
            .count();
        assert_eq!(barred, 1, "more than the focus is barred");
        // Provider sections split on a blank row under a dim header.
        let b = body(&for_case("list"), 90);
        let provider = b.iter().find(|r| plain(r).contains("openai-codex")).unwrap();
        assert!(
            provider.spans[0].style.add_modifier.contains(ratatui::style::Modifier::DIM),
            "section header is not dim"
        );
        let pi = b.iter().position(|r| plain(r).contains("openai-codex")).unwrap();
        assert!(plain(&b[pi - 1]).trim().is_empty(), "no blank before the section");
        // The foot is a bold-key legend.
        assert!(t.contains("\u{2191}\u{2193} move · \u{2190}\u{2192} levels · enter choose · tab show all · ctrl+s session · ctrl+r refresh · esc close"));
    }

    #[test]
    fn every_picker_case_pads_text_off_both_edges() {
        for c in CASES {
            let rows = view(&for_case(c.name), 100);
            let t = text(&rows);
            let lines: Vec<&str> = t.split('\n').collect();
            assert!(lines[1].trim().is_empty(), "{}: no top pad", c.name);
            assert!(lines[lines.len() - 2].trim().is_empty(), "{}: no bottom pad", c.name);
        }
    }

    #[test]
    fn the_focused_model_reads_bold_in_the_buffer() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::style::Modifier;
        let rows = view(&for_case("list"), 100);
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, rows.len() as u16));
        for (y, r) in rows.iter().enumerate() {
            crate::paint(&mut buf, 0, y as u16, 100, r);
        }
        let t = text(&rows);
        let y = t.split('\n').position(|l| l.contains("claude-opus-5-5")).unwrap() as u16;
        // The role reads bold on the focused row; the same role stays plain below.
        let hit = t.split('\n').nth(y as usize).unwrap().find("[main]").unwrap();
        let col = t.split('\n').nth(y as usize).unwrap()[..hit].chars().count();
        assert!(buf[(col as u16, y)].modifier.contains(Modifier::BOLD), "role not bold");
        let y2 = t.split('\n').position(|l| l.contains("gpt-6-sol [main]")).unwrap() as u16;
        assert!(
            !(0..100).any(|x| buf[(x, y2)].modifier.contains(Modifier::BOLD) && buf[(x, y2)].symbol() == "["),
            "unfocused role went bold"
        );
    }

    #[test]
    fn filter_tokens_match_provider_id_or_name_as_subsequences() {
        let all = fixture();
        let hay = |pi: usize, mi: usize| haystack(all[pi].name, &all[pi].models[mi]);
        // One row per branch: empty, blank, one token, two tokens, provider-only,
        // name-only, id-only, non-subsequence, case mix, token order irrelevant.
        // Each row is negate-checked: flipping the want breaks it.
        let table = [
            ("", hay(0, 0), true),
            ("   ", hay(1, 2), true),
            ("opus", hay(0, 0), true),
            ("opus", hay(0, 2), false),
            ("claude opus", hay(0, 0), true),
            ("opus xyz", hay(0, 0), false),
            ("anthropic", hay(0, 2), true),
            ("anthropic", hay(2, 0), false),
            ("sol mini", hay(1, 2), true),
            ("sol mini", hay(1, 4), false),
            ("haiku-5", hay(0, 2), true),
            ("haiku-5", hay(0, 0), false),
            ("zzz", hay(0, 0), false),
            ("opusz", hay(0, 0), false),
            ("oPuS", hay(0, 0), true),
            ("opus claude", hay(0, 0), true),
            ("\topus\t", hay(0, 0), true),
        ];
        for (q, h, want) in table {
            assert_eq!(matches(q, &h), want, "query {q:?} over {h:?}");
        }
    }

    #[test]
    fn highlight_hits_are_the_id_chars_each_token_consumes() {
        // Greedy per token, unioned; a token the id cannot match alone adds nothing.
        assert_eq!(id_hits("", "claude-opus-5-5"), Vec::<usize>::new());
        assert_eq!(id_hits("opus", "claude-opus-5-5"), vec![7, 8, 9, 10]);
        assert_eq!(id_hits("OPUS", "claude-opus-5-5"), vec![7, 8, 9, 10]);
        assert_eq!(id_hits("anthropic", "claude-opus-5-5"), Vec::<usize>::new());
        assert_eq!(id_hits("opus anthropic", "claude-opus-5-5"), vec![7, 8, 9, 10]);
        assert_eq!(id_hits("opusz", "claude-opus-5-5"), Vec::<usize>::new());
        assert_eq!(id_hits("so", "gpt-6-sol"), vec![6, 7]);
    }

    #[test]
    fn visible_filters_after_the_scope_rule_in_fixture_order() {
        let mut s = for_case("scoped");
        s.query = "claude".into();
        assert_eq!(visible(&s), vec![0, 1]);
        // Show-all widens before the query narrows: all three Claudes.
        s.show_all = true;
        assert_eq!(visible(&s), vec![0, 1, 2]);
        // A query over the full list keeps fixture order.
        let mut all = for_case("list");
        all.query = "sol".into();
        let ids: Vec<&str> = {
            let f = fixture();
            let mut flat = vec![];
            for p in &f {
                for m in &p.models {
                    flat.push(m.id);
                }
            }
            visible(&all).iter().map(|&i| flat[i]).collect()
        };
        assert_eq!(ids, vec!["claude-opus-5-5", "claude-sonnet-5-5", "gpt-6.1-sol", "gpt-6-sol-mini", "gpt-6-sol", "gemini-3-flash"]);
        // Nothing matches: the screen is empty.
        all.query = "zzz".into();
        assert!(visible(&all).is_empty());
    }

    #[test]
    fn typing_filters_and_never_reaches_the_draft() {
        let mut ui = Ui { input: "hello".into(), picker: Some(for_case("list")), ..Ui::default() };
        assert!(on_key(&mut ui, Key::Char('s'), Mods::default()));
        assert!(on_key(&mut ui, Key::Char('o'), Mods::default()));
        let p = ui.picker.as_ref().unwrap();
        assert_eq!(p.query, "so");
        assert_eq!(ui.input, "hello");
        assert!(visible(p).len() < 12);
    }

    #[test]
    fn bare_s_a_r_extend_the_query() {
        let mut ui = Ui { picker: Some(for_case("list")), ..Ui::default() };
        for c in ['s', 'a', 'r'] {
            assert!(on_key(&mut ui, Key::Char(c), Mods::default()));
        }
        let p = ui.picker.as_ref().unwrap();
        assert_eq!(p.query, "sar");
        assert_eq!(p.session_only, None);
        assert!(!p.show_all);
        assert!(p.refreshing.is_empty());
    }

    #[test]
    fn backspace_edits_the_query() {
        let mut ui = Ui { picker: Some(for_case("list")), ..Ui::default() };
        for c in ['s', 'o'] {
            on_key(&mut ui, Key::Char(c), Mods::default());
        }
        assert!(on_key(&mut ui, Key::Backspace, Mods::default()));
        assert_eq!(ui.picker.as_ref().unwrap().query, "s");
        assert!(ui.picker.is_some());
        on_key(&mut ui, Key::Backspace, Mods::default());
        assert_eq!(ui.picker.as_ref().unwrap().query, "");
        assert!(ui.picker.is_some());
    }

    #[test]
    fn esc_clears_before_closing() {
        let mut ui = Ui::default();
        let mut p = for_case("list");
        p.query = "so".into();
        ui.picker = Some(p);
        assert!(on_key(&mut ui, Key::Esc, Mods::default()));
        let p = ui.picker.as_ref().expect("first Esc closed the picker");
        assert_eq!(p.query, "");
        assert!(on_key(&mut ui, Key::Esc, Mods::default()));
        assert!(ui.picker.is_none());
    }

    #[test]
    fn enter_with_no_matches_stays_open() {
        let mut ui = Ui::default();
        let mut p = for_case("list");
        p.query = "zzz".into();
        ui.picker = Some(p);
        assert!(on_key(&mut ui, Key::Enter, Mods::default()));
        assert!(ui.picker.is_some());
        // With matches Enter still closes.
        let mut live = Ui { picker: Some(for_case("list")), ..Ui::default() };
        assert!(on_key(&mut live, Key::Enter, Mods::default()));
        assert!(live.picker.is_none());
    }

    #[test]
    fn session_showall_refresh_work_with_a_query() {
        let mut ui = Ui::default();
        let mut p = for_case("scoped");
        p.query = "so".into();
        ui.picker = Some(p);
        let ctrl = Mods { ctrl: true, ..Default::default() };
        assert!(on_key(&mut ui, Key::Char('s'), ctrl));
        assert_eq!(ui.picker.as_ref().unwrap().session_only, Some(0));
        assert!(on_key(&mut ui, Key::Tab, Mods::default()));
        assert!(ui.picker.as_ref().unwrap().show_all);
        assert_eq!(ui.picker.as_ref().unwrap().query, "so");
        assert!(on_key(&mut ui, Key::Char('r'), ctrl));
        assert_eq!(ui.picker.as_ref().unwrap().refreshing, vec![0, 1, 2]);
    }

    #[test]
    fn filtering_keeps_a_visible_focus_and_moves_a_hidden_one() {
        // gpt-6-sol-mini matches `mini` but is not its first hit: a visible
        // focus off the head stays put while the chip clears.
        let mut ui = Ui::default();
        let mut p = for_case("list");
        p.focus = 8;
        p.chip = Some(1);
        ui.picker = Some(p);
        for c in ['m', 'i', 'n', 'i'] {
            on_key(&mut ui, Key::Char(c), Mods::default());
        }
        let p = ui.picker.as_ref().unwrap();
        assert_eq!((p.focus, p.chip), (8, None));
        // A hidden focus jumps to the first visible row.
        let mut hidden = Ui { picker: Some(for_case("list")), ..Ui::default() };
        for c in ['m', 'i', 'n', 'i'] {
            on_key(&mut hidden, Key::Char(c), Mods::default());
        }
        let p = hidden.picker.as_ref().unwrap();
        assert_eq!((p.focus, p.chip), (5, None));
    }

    #[test]
    fn movement_on_empty_selection_does_nothing() {
        let mut ui = Ui::default();
        let mut p = for_case("list");
        p.query = "zzz".into();
        let focus = p.focus;
        ui.picker = Some(p);
        on_key(&mut ui, Key::Up, Mods::default());
        on_key(&mut ui, Key::Down, Mods::default());
        let p = ui.picker.as_ref().unwrap();
        assert_eq!((p.focus, p.chip), (focus, None));
        assert!(visible(p).is_empty());
    }

    #[test]
    fn the_search_line_reads_typed_or_placeholder() {
        let t = text(&view(&for_case("list"), 100));
        assert!(t.contains("Type to search"), "missing the placeholder");
        assert!(!t.contains("No models match"), "empty line with matches");
        let mut q = for_case("list");
        q.query = "so".into();
        let t = text(&view(&q, 100));
        assert!(t.contains("› so"), "missing the typed query");
        assert!(!t.contains("Type to search"), "placeholder behind the query");
        assert!(t.contains("6 of 12 models"), "controls miss the match count");
    }

    #[test]
    fn only_matched_id_chars_read_underlined() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        // gpt-6-sol-mini plain on screen while gemini-3-pro rides the bar.
        let mut q = for_case("list");
        q.query = "mini".into();
        q.focus = 8;
        let rows = view(&q, 100);
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, rows.len() as u16));
        for (y, r) in rows.iter().enumerate() {
            crate::paint(&mut buf, 0, y as u16, 100, r);
        }
        let lines: Vec<String> = rows.iter().map(plain).collect();
        let y = lines.iter().position(|l| l.contains("gpt-6-sol-mini")).unwrap() as u16;
        let start = lines[y as usize][..lines[y as usize].find("gpt-6-sol-mini").unwrap()].chars().count();
        let hits = id_hits("mini", "gpt-6-sol-mini");
        assert_eq!(hits, vec![10, 11, 12, 13]);
        for (i, _) in "gpt-6-sol-mini".chars().enumerate() {
            let cell = &buf[(start as u16 + i as u16, y)];
            if hits.contains(&i) {
                assert!(cell.modifier.contains(Modifier::UNDERLINED), "hit {i} not underlined");
                assert!(cell.modifier.contains(Modifier::BOLD), "hit {i} not bold");
            } else {
                assert!(!cell.modifier.contains(Modifier::UNDERLINED), "char {i} underlined");
            }
        }
        // The row is off the bar: none of its spans carry the accent behind.
        assert!(
            rows[y as usize].spans.iter().all(|s| s.style.bg != Some(BLUE)),
            "plain row picked up the bar"
        );
    }

    #[test]
    fn the_bar_row_keeps_the_underline() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut q = for_case("list");
        q.query = "mini".into();
        q.focus = 5;
        let rows = view(&q, 100);
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, rows.len() as u16));
        for (y, r) in rows.iter().enumerate() {
            crate::paint(&mut buf, 0, y as u16, 100, r);
        }
        let lines: Vec<String> = rows.iter().map(plain).collect();
        let y = lines.iter().position(|l| l.contains("gpt-6-sol-mini")).unwrap() as u16;
        let start = lines[y as usize][..lines[y as usize].find("gpt-6-sol-mini").unwrap()].chars().count();
        assert!(
            rows[y as usize].spans.iter().any(|s| s.style.bg == Some(BLUE)),
            "focus lost the bar"
        );
        for i in id_hits("mini", "gpt-6-sol-mini") {
            let cell = &buf[(start as u16 + i as u16, y)];
            assert!(cell.modifier.contains(Modifier::UNDERLINED), "bar stripped hit {i}");
            assert_eq!(cell.bg, BLUE, "hit {i} left the bar");
        }
    }

    #[test]
    fn no_matches_draws_one_muted_line() {
        let mut q = for_case("list");
        q.query = "zzz".into();
        let rows = view(&q, 100);
        let t = text(&rows);
        assert!(t.contains("No models match"), "missing the empty line");
        assert!(t.contains("0 of 12 models"), "controls miss the zero count");
        for id in ["claude-opus-5-5", "anthropic", "thinking"] {
            assert!(!t.contains(id), "empty list still draws {id}");
        }
        let line = rows.iter().find(|r| plain(r).contains("No models match")).unwrap();
        let ink = line.spans.iter().find(|s| s.content.contains("No models match")).unwrap();
        assert!(ink.style.add_modifier.contains(Modifier::DIM), "empty line is not muted");
        assert_eq!(picks(&rows), 0);
    }

    #[test]
    fn scoped_hides_the_other_seven_models() {
        let rows = view(&for_case("scoped"), 100);
        let t = text(&rows);
        assert!(t.contains("scoped · 5 of 12"), "missing the scope chip");
        assert_eq!(picks(&rows), 5);
        for id in [
            "claude-haiku-5-5",
            "gpt-6-luna",
            "gpt-6-sol-mini",
            "gpt-6-nano",
            "gpt-6-sol",
            "gemini-3-lite",
            "gemma-3-27b",
        ] {
            assert!(!t.contains(id), "scoped out but shown: {id}");
        }
    }

    #[test]
    fn show_all_lists_twelve_with_the_scoped_marked() {
        let rows = view(&for_case("scoped-all"), 100);
        assert_eq!(picks(&rows), 12);
        assert!(
            text(&rows).contains("· scoped"),
            "scoped models are not marked"
        );
    }

    #[test]
    fn the_session_only_note_appears_only_after_s() {
        let mut s = for_case("list");
        assert!(!text(&view(&s, 100)).contains("nothing saved"));
        s.focus = 1;
        s.mark_session_only();
        let t = text(&view(&s, 100));
        assert!(
            t.contains("this session only · nothing saved"),
            "missing the note"
        );
    }

    #[test]
    fn chosen_reads_focus_and_chip() {
        assert_eq!(chosen(&for_case("list")), ("claude-opus-5-5", "high"));
        let mut sonnet = for_case("list");
        sonnet.focus = 1;
        assert_eq!(chosen(&sonnet), ("claude-sonnet-5-5", "medium"));
        let mut low = for_case("list");
        low.chip = Some(0);
        assert_eq!(chosen(&low), ("claude-opus-5-5", "low"));
    }

    #[test]
    fn opened_at_focuses_model_and_level() {
        let s = opened_at("claude-sonnet-5-5", None);
        assert_eq!((s.focus, s.chip), (1, None));
        let s = opened_at("claude-opus-5-5", Some("low"));
        assert_eq!((s.focus, s.chip), (0, Some(0)));
        let s = opened_at("no-such-model", None);
        assert_eq!((s.focus, s.chip), (0, None));
    }

    #[test]
    fn movement_clamps_at_both_ends() {
        let mut s = for_case("list");
        s.focus_up();
        assert_eq!(s.focus, 0, "focus moved above the first row");
        s.focus = 11;
        s.focus_down();
        assert_eq!(s.focus, 11, "focus moved below the last row");
        // at, below and above the scoped set all land on screen
        let mut q = for_case("scoped");
        q.focus = 11;
        q.focus_down();
        assert!(
            visible(&q).contains(&q.focus),
            "focus is off screen: {}",
            q.focus
        );
        let mut r = for_case("scoped");
        r.focus = 99;
        r.focus_up();
        assert!(
            visible(&r).contains(&r.focus),
            "focus is off screen: {}",
            r.focus
        );
        // chips clamp at both ends of the focused model
        let mut c = for_case("levels");
        c.chip_right();
        c.chip_right();
        assert_eq!(c.chip, Some(3), "chip moved past the last level");
        c.chip = Some(0);
        c.chip_left();
        assert_eq!(c.chip, Some(0), "chip moved before the first level");
        // a model with no levels ignores the chip keys
        let mut n = for_case("list");
        n.focus = 6;
        n.chip_right();
        assert_eq!(n.chip, None, "a level-less model grew a chip");
    }

    #[test]
    fn the_refreshing_case_spins_one_provider() {
        let t = text(&view(&for_case("refreshing"), 100));
        assert!(t.contains("refreshing"), "no provider is refreshing");
        assert!(
            t.contains("updated 2m ago"),
            "the other providers lost their age"
        );
        assert!(t.contains("⟳ refresh all"), "missing the refresh button");
    }
}
