//! The rail cards, for #1632.
//!
//! A throwaway look prototype: each case draws one static frame from the
//! fixtures in this file, reusing the rail builders in `super` (the palette,
//! the `slab` surface and its roles), over a dimmed conversation backdrop,
//! and the program waits for a key; Esc, q or Ctrl+C quits. The fixture
//! replay never draws under a case. Every session here is a hard-coded
//! `Fake`, including the only `SState::Idle` one anywhere.

use super::input::{Ev, Key};
use super::{Args, Term};
use super::{BP, Fake, SState, dim, fit, lift, paint, row, sp};
use super::{panel_rows, rail_card_rows, rail_full_b, rail_spend_row_track};
use crate::cases::{Case, Surface};
use ratatui::style::Style;
use ratatui::text::Span;
use std::io::{self, Write};

/// Every `--rail-case` case, named in README.md.
const CASES: &[Case<Look>] = &[
    Case {
        name: "all-states",
        help: "the rail at 15% of 160: two projects, six sessions in every state",
        check: "two project groups, the launch project first, in start order; six cards reading WORKING, NEEDS INPUT with its orange wait reason, RETRYING, CRASHED with the red ✕, READY and IDLE; the on-screen card brighter; every row 1 keeps its state word and elapsed, and the spend row keeps spend and percentage with no bar at the 24 columns.",
        build: || Look {
            kind: Kind::AllStates,
        },
    },
    Case {
        name: "hovered",
        help: "one B card with the hover tint forced on, no mouse",
        check: "the needs-input card brightened across its whole surface, visible in SGR only; its text reads the same as the unhovered rail.",
        build: || Look {
            kind: Kind::Hovered,
        },
    },
    Case {
        name: "context-bars",
        help: "30/65/90% context bars in the panel ▆ style and the ░ track",
        check: "30%, 65% and 90% each drawn twice, once with the panel's ▆ track and once with the ░ track; the fills blue, orange and red; pick one.",
        build: || Look {
            kind: Kind::ContextBars,
        },
    },
    Case {
        name: "floor-ceiling",
        help: "the rail at its 22-column floor beside its 48-column ceiling",
        check: "the 22-column rail keeps number, glyph, state word, name, branch and spend-plus-percentage with no bar; the 48-column rail shows the full card with its bar; nothing clipped on either.",
        build: || Look {
            kind: Kind::FloorCeiling,
        },
    },
    Case {
        name: "hidden",
        help: "the hidden rail's grip with N waiting on the Session card",
        check: "a 1-column handle with the dim ⋮ grip at the screen's left edge, and the Session card ending `! 2 waiting · rail hidden …` in orange: the `· show` hint clips at the default panel width, as on screen.",
        build: || Look { kind: Kind::Hidden },
    },
];

/// `--rail-case`, for `--help` and `check/rail.md`.
pub(crate) const SURFACE: Surface = Surface {
    flag: "--rail-case",
    file: "rail",
    title: "Rail cards (#1632)",
    docs: || crate::cases::docs(CASES),
};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    AllStates,
    Hovered,
    ContextBars,
    FloorCeiling,
    Hidden,
}

struct Look {
    kind: Kind,
}

/// A fixed tick, so every still frame is stable: the spinners rest on their
/// first frame and the waiting pulse on its attention phase.
const TICK: u64 = 0;
/// The B card density every rail case draws: medium, four rows a card.
const DENSITY: u8 = 1;

/// The two sessions `six` and `pair` share: a working session and a waiting
/// one. Only the waiting session's context fill differs, so each case spans
/// two bar colours: 41 behind `six`, 64 behind `pair`.
fn first_two(wait_ctx: u8) -> Vec<Fake> {
    vec![
        Fake {
            name: "fix flaky lock test",
            project: "fiber",
            wait: String::new(),
            model: "opus-5-5 · high".into(),
            branch: "main",
            ws: "fiber",
            spend: "$1.35".into(),
            dollars: 1.35,
            ctx: 12,
            age: "16s".into(),
            st: SState::Working,
        },
        Fake {
            name: "review #688",
            project: "fiber",
            wait: "approval: shell cargo publish --dry-run".into(),
            model: "gpt-6.1-sol · medium".into(),
            branch: "review-688",
            ws: "fiber",
            spend: "$1.10".into(),
            dollars: 1.10,
            ctx: wait_ctx,
            age: "22m".into(),
            st: SState::NeedsInput,
        },
    ]
}

/// The six sessions behind `all-states` and `hovered`: two projects, every
/// state once, context fills across all three bar colours. Only this fixture
/// builds an idle session.
fn six() -> Vec<Fake> {
    let mut v = first_two(41);
    v.extend([
        Fake {
            name: "backfill embeddings",
            project: "pi-rig",
            wait: String::new(),
            model: "grok-4.7-high · high".into(),
            branch: "backfill/embed",
            ws: "pi-rig",
            spend: "$0.87".into(),
            dollars: 0.87,
            ctx: 65,
            age: "12m".into(),
            st: SState::Retrying,
        },
        Fake {
            name: "lsp probe",
            project: "fiber",
            wait: String::new(),
            model: "grok-4.7-high · low".into(),
            branch: "spike/lsp",
            ws: "fiber",
            spend: "$0.05".into(),
            dollars: 0.05,
            ctx: 91,
            age: "1h".into(),
            st: SState::Crashed,
        },
        Fake {
            name: "docs: rail spec",
            project: "fiber",
            wait: String::new(),
            model: "opus-5-5 · high".into(),
            branch: "docs/rail-692",
            ws: "fiber",
            spend: "$0.42".into(),
            dollars: 0.42,
            ctx: 27,
            age: "9m".into(),
            st: SState::Ready,
        },
        Fake {
            name: "nightly backfill",
            project: "pi-rig",
            wait: String::new(),
            model: "gpt-6.1-sol · low".into(),
            branch: "backfill/nightly",
            ws: "pi-rig",
            spend: "$0.64".into(),
            dollars: 0.64,
            ctx: 78,
            age: "3h".into(),
            st: SState::Idle,
        },
    ]);
    v
}

/// One session with a set context fill, for the bar comparison: only the
/// spend row draws, so the name never shows.
fn barred(ctx: u8, spend: &'static str) -> Fake {
    Fake {
        name: "backfill embeddings",
        project: "pi-rig",
        wait: String::new(),
        model: "grok-4.7-high · high".into(),
        branch: "backfill/embed",
        ws: "pi-rig",
        spend: spend.into(),
        dollars: 0.0,
        ctx,
        age: "12m".into(),
        st: SState::Working,
    }
}

/// Two sessions for the width comparison: waiting shows the wait reason,
/// crashed would need the dismiss column, so neither is crashed.
fn pair() -> Vec<Fake> {
    first_two(64)
}

/// The Session card's fixture behind `hidden`: enough of a session for the
/// card to read, while the waiting line is what the owner judges.
fn session_fold() -> super::Fold {
    super::Fold {
        cwd: "fiber".into(),
        mode: "agent".into(),
        model: "opus-5-5".into(),
        effort: "high".into(),
        branch: "docs/rail-692".into(),
        ctx: 120_000,
        input: 100_000,
        output: 20_000,
        cost: 1.35,
        tools: 42,
        ..Default::default()
    }
}

// ============================================================ frame
/// A row painted at its own offset and width, so the rail's background stays
/// within its columns instead of extending across the screen.
struct Placed {
    x: u16,
    w: u16,
    row: super::Row,
}

struct Frame {
    screen: Vec<Vec<Placed>>,
    /// the hovered card's cells, lifted at draw the way `--hover` lifts them
    hover: Option<(u16, u16, u16, u16)>,
}

/// The resize grip column beside a rail, as the main loop paints it at rest:
/// a dim ⋮ on 3 centred rows, blank elsewhere.
fn grip_cell(rows: usize, y: usize) -> Vec<Span<'static>> {
    let mid = rows / 2;
    if y >= mid.saturating_sub(1) && y <= mid + 1 {
        vec![sp("⋮", dim())]
    } else {
        vec![sp(" ", Style::new())]
    }
}

/// Clips a row to its columns, as `paint` does on screen: the chips rows
/// outgrow a narrow rail and the screen shows them cut.
fn clip(r: &super::Row, w: usize) -> super::Row {
    super::Row {
        spans: fit(&r.spans, w),
        ..r.clone()
    }
}

/// One screen from a left rail over a conversation backdrop: the rail column
/// keeps the `panel` tint throughout, the grip column marks the draggable
/// edge, and the dimmed conversation fills the rest, as the main loop
/// composes them.
fn rail_screen(rail: &[super::Row], rail_w: usize, cols: usize, rows: usize) -> Frame {
    let back = crate::overlays::backdrop(cols.saturating_sub(rail_w + 1), rows);
    let mut screen = vec![];
    for y in 0..rows {
        let r = rail.get(y).cloned().unwrap_or(super::Row {
            spans: vec![],
            bg: Some(BP),
            ..Default::default()
        });
        screen.push(vec![
            Placed {
                x: 0,
                w: rail_w as u16,
                row: clip(&r, rail_w),
            },
            Placed {
                x: rail_w as u16,
                w: 1,
                row: row(grip_cell(rows, y)),
            },
            Placed {
                x: rail_w as u16 + 1,
                w: cols.saturating_sub(rail_w + 1) as u16,
                row: back.get(y).cloned().unwrap_or_default(),
            },
        ]);
    }
    Frame {
        screen,
        hover: None,
    }
}

/// The `all-states` rail: 15% of 160 columns, the on-screen card first.
fn all_states_rows() -> (Vec<super::Row>, Vec<(usize, usize, usize)>) {
    let sess = six();
    let shown: Vec<usize> = (0..sess.len()).collect();
    rail_full_b(&sess, &shown, 0, TICK, false, 24, DENSITY)
}

fn all_states(cols: usize, rows: usize) -> Frame {
    let (rail, _) = all_states_rows();
    rail_screen(&rail, 24, cols, rows)
}

fn hovered(cols: usize, rows: usize) -> Frame {
    let (rail, map) = all_states_rows();
    let mut fr = rail_screen(&rail, 24, cols, rows);
    // The needs-input session wears the `--hover` tint, with no mouse: the
    // main loop lifts the hovered B card's whole surface, so the still frame
    // lifts the same cells at draw.
    if let Some(&(_, srow, len)) = map.iter().find(|&&(i, _, _)| i == 1) {
        fr.hover = Some((0, 24, srow as u16, (srow + len) as u16));
    }
    fr
}

/// The bar comparison: 30%, 65% and 90% each drawn twice, once with the
/// panel's ▆ track and once with the ░ track, in one edged card.
fn context_bars(cols: usize, rows: usize) -> Frame {
    const W: usize = 64;
    let iw = W.saturating_sub(2);
    let mut inner = vec![vec![sp(
        "context bars · ▆ panel track vs ░ mock track · pick one",
        dim(),
    )]];
    for (ctx, spend) in [(30u8, "$0.42"), (65u8, "$1.10"), (90u8, "$2.31")] {
        let s = barred(ctx, spend);
        inner.push(vec![
            sp(format!("ctx {ctx}% · panel"), dim()),
            sp(" ▆ track", dim()),
        ]);
        inner.push(rail_spend_row_track(&s, TICK, false, iw, true, '▆'));
        inner.push(vec![
            sp(format!("ctx {ctx}% · mock"), dim()),
            sp(" ░ track", dim()),
        ]);
        inner.push(rail_spend_row_track(&s, TICK, false, iw, true, '░'));
    }
    let card = rail_card_rows(inner, 0, super::BC, W, true);
    let centred = crate::panel::centre(card, W, cols);
    let back = crate::overlays::backdrop(cols, rows);
    let y0 = rows.saturating_sub(centred.len()) / 2;
    let mut screen = vec![];
    for y in 0..rows {
        if y >= y0
            && y - y0 < centred.len()
            && let Some(r) = centred.get(y - y0)
        {
            screen.push(vec![Placed {
                x: 0,
                w: cols as u16,
                row: r.clone(),
            }]);
        } else {
            screen.push(vec![Placed {
                x: 0,
                w: cols as u16,
                row: back.get(y).cloned().unwrap_or_default(),
            }]);
        }
    }
    Frame {
        screen,
        hover: None,
    }
}

/// The width comparison's rails: the 22-column floor and the 48-column
/// ceiling, one column apart. The tests draw each rail on its own too.
const FLOOR_W: usize = 22;
const CEIL_W: usize = 48;
const RAIL_GAP: usize = 1;

/// The width comparison: the 22-column floor and the 48-column ceiling side
/// by side over one backdrop, labelled, so the owner compares directly.
fn floor_ceiling(cols: usize, rows: usize) -> Frame {
    const LEFT: usize = FLOOR_W;
    const RIGHT: usize = CEIL_W;
    const GAP: usize = RAIL_GAP;
    let sess = pair();
    let shown: Vec<usize> = (0..sess.len()).collect();
    let (floor, _) = rail_full_b(&sess, &shown, 0, TICK, false, LEFT, DENSITY);
    let (ceiling, _) = rail_full_b(&sess, &shown, 0, TICK, false, RIGHT, DENSITY);
    let back = crate::overlays::backdrop(cols.saturating_sub(LEFT + GAP + RIGHT + GAP), rows);
    let x1 = LEFT + GAP;
    let x2 = x1 + RIGHT + GAP;
    let mut screen = vec![];
    for y in 0..rows {
        let mut placements = vec![];
        if y == 0 {
            placements.push(Placed {
                x: 0,
                w: LEFT as u16,
                row: row(fit(&[sp("22 · floor", dim())], LEFT)),
            });
        } else if let Some(r) = floor.get(y - 1) {
            placements.push(Placed {
                x: 0,
                w: LEFT as u16,
                row: clip(r, LEFT),
            });
        } else {
            placements.push(Placed {
                x: 0,
                w: LEFT as u16,
                row: super::Row {
                    spans: vec![],
                    bg: Some(BP),
                    ..Default::default()
                },
            });
        }
        placements.push(Placed {
            x: LEFT as u16,
            w: 1,
            row: row(grip_cell(rows, y)),
        });
        if y == 0 {
            placements.push(Placed {
                x: x1 as u16,
                w: RIGHT as u16,
                row: row(fit(&[sp("48 · ceiling", dim())], RIGHT)),
            });
        } else if let Some(r) = ceiling.get(y - 1) {
            placements.push(Placed {
                x: x1 as u16,
                w: RIGHT as u16,
                row: clip(r, RIGHT),
            });
        } else {
            placements.push(Placed {
                x: x1 as u16,
                w: RIGHT as u16,
                row: super::Row {
                    spans: vec![],
                    bg: Some(BP),
                    ..Default::default()
                },
            });
        }
        placements.push(Placed {
            x: (x1 + RIGHT) as u16,
            w: 1,
            row: row(grip_cell(rows, y)),
        });
        placements.push(Placed {
            x: x2 as u16,
            w: cols.saturating_sub(x2) as u16,
            row: back.get(y).cloned().unwrap_or_default(),
        });
        screen.push(placements);
    }
    Frame {
        screen,
        hover: None,
    }
}

/// The hidden rail: its 1-column handle at the screen's left edge, and the
/// waiting count on the panel's Session card, as the main loop draws them.
fn hidden(cols: usize, rows: usize) -> Frame {
    const PANEL: usize = 34;
    let fold = session_fold();
    let panel = panel_rows(&fold, 1_746_000_000, "kitty · 3 ms", PANEL, 2);
    let back = crate::overlays::backdrop(cols.saturating_sub(1 + PANEL), rows);
    let x1 = 1;
    let x2 = 1 + cols.saturating_sub(1 + PANEL);
    let mut screen = vec![];
    for y in 0..rows {
        screen.push(vec![
            Placed {
                x: 0,
                w: 1,
                row: row(grip_cell(rows, y)),
            },
            Placed {
                x: x1 as u16,
                w: cols.saturating_sub(1 + PANEL) as u16,
                row: back.get(y).cloned().unwrap_or_default(),
            },
            Placed {
                x: x2 as u16,
                w: PANEL as u16,
                row: panel.get(y).cloned().unwrap_or(super::Row {
                    spans: vec![],
                    bg: Some(BP),
                    ..Default::default()
                }),
            },
        ]);
    }
    Frame {
        screen,
        hover: None,
    }
}

fn frame(c: &Look, cols: usize, rows: usize) -> Frame {
    match c.kind {
        Kind::AllStates => all_states(cols, rows),
        Kind::Hovered => hovered(cols, rows),
        Kind::ContextBars => context_bars(cols, rows),
        Kind::FloorCeiling => floor_ceiling(cols, rows),
        Kind::Hidden => hidden(cols, rows),
    }
}

/// Lifts the hovered card's cells, the way the main loop's `--hover` path does.
fn lift_hover(buf: &mut ratatui::buffer::Buffer, hover: Option<(u16, u16, u16, u16)>) {
    if let Some((x, w, y0, y1)) = hover {
        let (bw, bh) = (buf.area.width, buf.area.height);
        for y in y0..y1.min(bh) {
            for xx in x..(x + w).min(bw) {
                let cell = &mut buf[(xx, y)];
                cell.set_bg(lift(cell.bg));
            }
        }
    }
}

fn draw(term: &mut Term, c: &Look) -> io::Result<()> {
    let size = term.size()?;
    let (cols, rows) = (size.width.max(1), size.height.max(1));
    let fr = frame(c, cols as usize, rows as usize);
    term.backend_mut().write_all(b"\x1b[?2026h")?;
    term.draw(|f| {
        let buf = f.buffer_mut();
        for (y, placements) in fr.screen.iter().enumerate() {
            for p in placements {
                paint(buf, p.x, y as u16, p.w, &p.row);
            }
        }
        lift_hover(buf, fr.hover);
    })?;
    let be = term.backend_mut();
    be.write_all(b"\x1b[?2026l")?;
    be.flush()?;
    Ok(())
}

/// Draws the `--rail-case` case and waits for a key. Mutually exclusive with
/// the conversation view: the fixture replay never draws.
pub(crate) fn run_rail(a: &Args, term: &mut Term) -> io::Result<String> {
    let name = a.rail_case.clone().unwrap_or_default();
    let Some(c) = crate::cases::lookup(CASES, &name) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "unknown --rail-case case {name:?}; one of: {}",
                crate::cases::names(CASES)
            ),
        ));
    };
    draw(term, &c)?;
    let mut rd = super::input::Reader::new()?;
    loop {
        let (evs, resized) = rd.wait(None)?;
        if resized {
            draw(term, &c)?;
        }
        for ev in evs {
            match ev {
                Ev::Key(Key::Char('c'), m) if m.ctrl => return Ok(format!("rail {name}\n")),
                Ev::Key(Key::Esc, _) => return Ok(format!("rail {name}\n")),
                Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => {
                    return Ok(format!("rail {name}\n"));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn parse(name: &str) -> Option<Look> {
        crate::cases::lookup(CASES, name)
    }

    fn text(c: &Look, cols: usize, rows: usize) -> String {
        frame(c, cols, rows)
            .screen
            .into_iter()
            .map(|ps| {
                // Placements tile the row left to right; the text reads whole.
                let mut ps = ps;
                ps.sort_by_key(|p| p.x);
                ps.iter()
                    .map(|p| {
                        p.row
                            .spans
                            .iter()
                            .map(|s| s.content.as_ref())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Paints a case into a buffer, lifting the hovered card as `draw` does.
    fn buffer(c: &Look, cols: usize, rows: usize) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, cols as u16, rows as u16));
        let fr = frame(c, cols, rows);
        for (y, ps) in fr.screen.iter().enumerate() {
            for p in ps {
                crate::paint(&mut buf, p.x, y as u16, p.w, &p.row);
            }
        }
        lift_hover(&mut buf, fr.hover);
        buf
    }

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(
            CASES
                .iter()
                .all(|c| crate::cases::lookup(CASES, c.name).is_some())
        );
        assert_eq!(CASES.len(), 5);
        assert!(parse("nope").is_none());
    }

    use crate::{BLUE, ORANGE, RED};
    use crate::{bar_track, rail_ctx_color};

    #[test]
    fn the_track_glyph_only_changes_the_track() {
        use crate::SEL;
        // Fill rounds the same under both tracks: empty, a trace, mid fills, full.
        for (frac, w, filled) in [
            (0.0, 10, 0),
            (0.001, 100, 1),
            (0.3, 10, 3),
            (0.65, 40, 26),
            (0.9, 40, 36),
            (1.0, 10, 10),
        ] {
            for color in [BLUE, ORANGE, RED] {
                for track in ['▆', '░'] {
                    let bar = bar_track(frac, w, color, track);
                    let plain: String = bar.iter().map(|s| s.content.as_ref()).collect();
                    assert_eq!(
                        plain,
                        "▆".repeat(filled) + &track.to_string().repeat(w - filled),
                        "fill differs at {frac} under {track}"
                    );
                    // The fill keeps the state colour, the track the surface.
                    assert_eq!(bar[0].style.fg, Some(color));
                    assert_eq!(bar[1].style.fg, Some(SEL));
                }
            }
        }
    }

    #[test]
    fn the_bar_colour_turns_at_sixty_and_eighty_five() {
        assert_eq!(rail_ctx_color(59), BLUE);
        assert_eq!(rail_ctx_color(60), ORANGE);
        assert_eq!(rail_ctx_color(85), ORANGE);
        assert_eq!(rail_ctx_color(86), RED);
    }

    use crate::SEL;
    use crate::{rail_card_bg, rail_state_word, state_glyph};

    #[test]
    fn idle_reads_dim_and_still() {
        let idle = Fake {
            st: SState::Idle,
            ..barred(10, "$0.00")
        };
        // Glyph, word, style and stripe all stay dim: no spinner, no pulse.
        let g = state_glyph(SState::Idle, 7, false);
        assert_eq!(g.content, "○");
        assert_eq!(g.style, dim());
        assert_eq!(rail_state_word(&idle), "IDLE");
        // The non-screen card keeps the dim quiescent tint, like ready.
        assert_eq!(
            rail_card_bg(&idle, false),
            rail_card_bg(
                &Fake {
                    st: SState::Ready,
                    ..barred(10, "$0.00")
                },
                false,
            )
        );
        // On screen it still brightens to the raised surface.
        assert_eq!(rail_card_bg(&idle, true), SEL);
    }

    #[test]
    fn all_states_shows_two_projects_and_every_state() {
        let c = parse("all-states").unwrap();
        let t = text(&c, 160, 48);
        assert!(t.contains("fiber"));
        assert!(t.contains("pi-rig"));
        for word in [
            "WORKING",
            "NEEDS INPUT",
            "RETRYING",
            "CRASHED",
            "READY",
            "IDLE",
        ] {
            assert!(t.contains(word), "missing {word}");
        }
        // The wait reason, the dismiss ✕ and the kept percentage all read.
        // The 24-column rail cuts the wait reason, as on screen.
        assert!(t.contains("approval:"));
        assert!(t.contains("✕"));
        // Row 1 keeps the state word and the elapsed; the spend row keeps
        // the spend and the percentage, the bar giving way below 30 columns.
        assert!(t.contains("12%"));
        assert!(t.contains("16s"), "the elapsed should read at 24 columns");
        assert!(t.contains("$1.35"), "the spend should read at 24 columns");
        // Stable numbers: every session keeps its start-order number.
        for n in ["1 ", "2 ", "3 ", "4 ", "5 ", "6 "] {
            assert!(t.contains(n), "missing number {n}");
        }
    }

    #[test]
    fn the_on_screen_card_is_brighter_than_the_rest() {
        let c = parse("all-states").unwrap();
        let buf = buffer(&c, 160, 48);
        let t = text(&c, 160, 48);
        // Row 1 of the first card is the on-screen session's state row.
        let y = t.split('\n').position(|l| l.contains("WORKING")).unwrap() as u16;
        assert_eq!(buf[(3, y)].bg, SEL, "on-screen card is not raised");
        // A later card keeps its dimmer state tint.
        let y2 = t.split('\n').position(|l| l.contains("RETRYING")).unwrap() as u16;
        assert_ne!(buf[(3, y2)].bg, SEL, "off-screen card raised too");
    }

    #[test]
    fn hovered_lifts_the_whole_card_and_nothing_else() {
        let (plain, hover) = (
            buffer(&parse("all-states").unwrap(), 160, 48),
            buffer(&parse("hovered").unwrap(), 160, 48),
        );
        let t = text(&parse("hovered").unwrap(), 160, 48);
        // The text reads the same hovered or not.
        assert_eq!(t, text(&parse("all-states").unwrap(), 160, 48));
        // Every cell of the needs-input card is lighter; the card above is not.
        let y = t
            .split('\n')
            .position(|l| l.contains("NEEDS INPUT"))
            .unwrap() as u16;
        let mut lifted = 0;
        for x in 0..24u16 {
            if hover[(x, y)].bg != plain[(x, y)].bg {
                assert_eq!(hover[(x, y)].bg, lift(plain[(x, y)].bg));
                lifted += 1;
            }
        }
        assert!(lifted > 12, "hover tint covers the card's width");
        let y0 = t.split('\n').position(|l| l.contains("WORKING")).unwrap() as u16;
        for x in 0..24u16 {
            assert_eq!(hover[(x, y0)].bg, plain[(x, y0)].bg, "tint leaks up at {x}");
        }
    }

    #[test]
    fn context_bars_show_both_tracks_at_three_fills() {
        let c = parse("context-bars").unwrap();
        let t = text(&c, 160, 48);
        for pct in ["30%", "65%", "90%"] {
            assert!(t.contains(pct), "missing {pct}");
        }
        assert!(t.contains('░'), "no ░ track drawn");
        // Each fill reads in its colour on both rows.
        let buf = buffer(&c, 160, 48);
        for (pct, color) in [("30%", BLUE), ("65%", ORANGE), ("90%", RED)] {
            let rows: Vec<u16> = t
                .split('\n')
                .enumerate()
                .filter(|(_, l)| l.contains(pct) && l.contains('$') && l.contains('▆'))
                .map(|(y, _)| y as u16)
                .collect();
            assert_eq!(rows.len(), 2, "{pct} is not drawn twice");
            for y in rows {
                assert!(
                    (0..160).any(|x| buf[(x, y)].fg == color && buf[(x, y)].symbol() == "▆"),
                    "{pct} fill is not {color:?}"
                );
            }
        }
        // The two rows of a fill cover the same cells: only the glyph differs.
        let lines: Vec<&str> = t.split('\n').collect();
        for pct in ["30%", "65%", "90%"] {
            let pair: Vec<&&str> = lines
                .iter()
                .filter(|l| l.contains(pct) && l.contains('$'))
                .collect();
            assert_eq!(pair.len(), 2);
            let cells = |l: &str| l.chars().filter(|&ch| ch == '▆' || ch == '░').count();
            assert_eq!(cells(pair[0]), cells(pair[1]), "{pct} widths differ");
        }
    }

    #[test]
    fn floor_and_ceiling_keep_everything_readable() {
        let c = parse("floor-ceiling").unwrap();
        let t = text(&c, 160, 48);
        assert!(t.contains("22 · floor"));
        assert!(t.contains("48 · ceiling"));
        // Both rails name the sessions; the narrow one keeps the state word.
        // The 22-column rail cuts the long name, as on screen.
        assert_eq!(t.matches("fix flaky").count(), 2);
        assert_eq!(t.matches("review #688").count(), 2);
        assert!(t.contains("NEEDS INPUT"));
    }

    /// One rail of the width comparison on its own, so the floor test
    /// asserts on the floor frame and the ceiling test on the ceiling frame.
    fn width_rows(w: usize) -> Vec<crate::Row> {
        let sess = pair();
        let shown: Vec<usize> = (0..sess.len()).collect();
        rail_full_b(&sess, &shown, 0, TICK, false, w, DENSITY).0
    }

    /// The text of rail rows on their own, one line each.
    fn rows_text(rows: &[crate::Row]) -> String {
        rows.iter()
            .map(|r| {
                r.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn floor_keeps_spend_with_no_bar() {
        let t = rows_text(&width_rows(FLOOR_W));
        assert!(t.contains("$1.35"), "the floor lost spend");
        assert!(t.contains("12%"), "the floor lost the percentage");
        assert!(!t.contains('▆'), "the floor draws a bar it has no room for");
    }

    #[test]
    fn ceiling_keeps_its_bar() {
        let t = rows_text(&width_rows(CEIL_W));
        assert!(t.contains('▆'), "the ceiling lost its bar");
    }

    #[test]
    fn hidden_shows_the_grip_and_the_waiting_line() {
        let c = parse("hidden").unwrap();
        let t = text(&c, 160, 48);
        assert!(t.contains("2 waiting · rail hidden …"));
        // The dim ⋮ grip stands at the screen's left edge on 3 centred rows.
        let lines: Vec<&str> = t.split('\n').collect();
        let mid = 48 / 2;
        for y in [mid - 1, mid, mid + 1] {
            assert!(
                lines[y].starts_with('⋮'),
                "no grip at row {y}: {:?}",
                &lines[y][..8.min(lines[y].len())]
            );
        }
        assert!(t.contains("fiber"), "the Session card lost its session");
    }

    use crate::width;

    #[test]
    fn no_row_overflows_its_screen() {
        for name in CASES.iter().map(|c| c.name) {
            let c = crate::cases::lookup(CASES, name).unwrap();
            let fr = frame(&c, 160, 48);
            assert_eq!(fr.screen.len(), 48, "{name} is not 48 rows");
            for (y, ps) in fr.screen.iter().enumerate() {
                for p in ps {
                    assert!(
                        width(&p.row.spans) <= p.w as usize,
                        "{name} row {y} overflows its {w} columns",
                        w = p.w
                    );
                    assert!(p.x + p.w <= 160, "{name} row {y} runs past the screen edge");
                }
            }
        }
    }
}
