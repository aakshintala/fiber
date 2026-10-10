//! The model picker (#1629): a swapped view over the conversation area, like the
//! context breakdown. Fixture data only: the stream names just the current model,
//! so the providers, roles, thinking levels and rebuild costs below are made up.

use crate::cases::{Case, Surface};
use crate::input::{Key, Mods};
use crate::{bold, dim, fg, hot_row, panel, row, sp, width, Act, Row, Ui, BLUE, ORANGE, SPIN};
use ratatui::style::Style;
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
                    roles: vec!["main"],
                    current: true,
                    levels: vec!["low", "medium", "high", "xhigh"],
                    level: Some("high"),
                    rebuild: "—",
                },
                Model {
                    id: "claude-sonnet-5-5",
                    roles: vec!["reviewer"],
                    current: false,
                    levels: vec!["low", "medium", "high"],
                    level: Some("medium"),
                    rebuild: "~84k tokens · $0.31",
                },
                Model {
                    id: "claude-haiku-5-5",
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
                    roles: vec!["reviewer"],
                    current: false,
                    levels: vec!["low", "medium", "high", "xhigh"],
                    level: Some("high"),
                    rebuild: "~92k tokens · $0.44",
                },
                Model {
                    id: "gpt-6-luna",
                    roles: vec!["explorer"],
                    current: false,
                    levels: vec!["low", "medium", "high"],
                    level: Some("medium"),
                    rebuild: "~92k tokens · $0.28",
                },
                Model {
                    id: "gpt-6-sol-mini",
                    roles: vec!["small"],
                    current: false,
                    levels: vec!["low", "medium"],
                    level: Some("low"),
                    rebuild: "~70k tokens · $0.09",
                },
                Model {
                    id: "gpt-6-nano",
                    roles: vec!["small"],
                    current: false,
                    levels: vec![],
                    level: None,
                    rebuild: "~58k tokens · $0.03",
                },
                Model {
                    id: "gpt-6-sol",
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
                    roles: vec!["explorer"],
                    current: false,
                    levels: vec!["low", "medium", "high"],
                    level: Some("medium"),
                    rebuild: "~88k tokens · $0.22",
                },
                Model {
                    id: "gemini-3-flash",
                    roles: vec!["small"],
                    current: false,
                    levels: vec!["low", "medium"],
                    level: Some("low"),
                    rebuild: "~64k tokens · $0.06",
                },
                Model {
                    id: "gemini-3-lite",
                    roles: vec!["small"],
                    current: false,
                    levels: vec![],
                    level: None,
                    rebuild: "~49k tokens · $0.01",
                },
                Model {
                    id: "gemma-3-27b",
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
];

/// `--picker`, for `--help` and `check/model-picker.md`.
pub(crate) const SURFACE: Surface = Surface { flag: "--picker", file: "model-picker", title: "Model picker (#1629)", docs: || crate::cases::docs(CASES) };

fn base() -> State {
    State { still: STILL.load(Relaxed), ..Default::default() }
}

pub fn for_case(case: &str) -> State {
    crate::cases::lookup(CASES, case).unwrap_or_else(|| panic!("--picker {}", crate::cases::names(CASES).replace(", ", "|")))
}

fn count() -> usize {
    fixture().iter().map(|p| p.models.len()).sum()
}

/// The flat indices on screen: the scoped set, or everything.
pub fn visible(s: &State) -> Vec<usize> {
    let n = count();
    if s.scoped.is_empty() || s.show_all {
        (0..n).collect()
    } else {
        s.scoped.iter().copied().filter(|&i| i < n).collect()
    }
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
    pub fn focus_up(&mut self) {
        let v = visible(self);
        let i = v.iter().position(|&x| x == self.focus).unwrap_or(0);
        self.focus = v[i.saturating_sub(1)];
        self.chip = None;
    }
    pub fn focus_down(&mut self) {
        let v = visible(self);
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
        if !self.scoped.is_empty() {
            self.show_all = !self.show_all;
        }
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

fn model_row(f: usize, m: &Model, focused: bool, scoped_mark: bool) -> Row {
    let mut spans = vec![
        sp(if focused { "› " } else { "  " }, if focused { bold() } else { Style::new() }),
        sp(
            m.id,
            if focused {
                bold().patch(fg(BLUE))
            } else {
                fg(BLUE)
            },
        ),
        sp(" ", Style::new()),
    ];
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

/// The controls: the scope chip, the show-all toggle, and the refresh button.
fn controls(s: &State, w: usize) -> Row {
    let scope = if s.scoped.is_empty() {
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

/// The foot legend's pairs: keys bold, labels muted, naming panel keys the
/// body never lists as choices.
fn footer_pairs() -> Vec<(&'static str, &'static str)> {
    vec![
        ("↑↓", "move"),
        ("←→", "levels"),
        ("enter", "choose"),
        ("s", "session only"),
        ("a", "show all"),
        ("r", "refresh"),
        ("esc", "close"),
    ]
}

/// The foot legend: keys bold, labels muted.
fn footer() -> Row {
    panel::footer_legend(&footer_pairs())
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
    let pos = vis.iter().position(|&i| i == s.focus).unwrap_or(0);
    let mut out = vec![controls(s, inner)];
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
            if focused {
                out.extend(panel::bar(vec![model_row(f, m, focused, scoped_mark)]));
            } else {
                out.push(model_row(f, m, focused, scoped_mark));
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
    // The controls row pads to its width, so it never sizes the panel.
    // The legend sizes unfitted now, straight from its row.
    let natural = probe.iter().skip(1).map(|r| width(&r.spans)).max().unwrap_or(0);
    let natural = natural.max(width(&footer().spans));
    let panel_w = panel::fit_width(natural, w.saturating_sub(4).min(96), w);
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
        Key::Char('s') if plain => p.mark_session_only(),
        Key::Char('a') if plain => p.toggle_show_all(),
        Key::Char('r') if plain => p.refresh_all(),
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
        let rows = view(&for_case("list"), 100);
        let t = text(&rows);
        assert!(t.contains("\u{2584}"), "no top edge");
        assert!(t.contains("\u{2580}"), "no bottom edge");
        assert!(t.contains("\u{258c}"), "no stripe");
        assert!(t.contains("\u{203a} "), "no gutter marker");
        // Centred: the edge run neither starts at the margin nor fills the row.
        let edge = t.split('\n').find(|l| l.contains("\u{2584}")).unwrap();
        let run = edge.chars().filter(|&c| c == '\u{2584}').count();
        assert!(edge.starts_with(' '), "panel flush left");
        assert!(run < 100, "panel fills the area");
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
        assert!(t.contains("\u{2191}\u{2193} move · \u{2190}\u{2192} levels · enter choose · s session only · a show all · r refresh · esc close"));
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
