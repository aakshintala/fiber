//! `/login` (#1736): the installed providers and the secrets installed
//! extensions declare, drawn as a centred panel over a dimmed conversation.
//! Fixture data only: the prototype has no login flow, so the providers,
//! secrets and every later state below are made up. Each case draws one
//! static frame and waits for a key; Esc, q or Ctrl+C quits.

use crate::cases::{Case, Surface};
use super::input::{Ev, Key};
use super::overlays;
use super::{Args, Term};
use super::{dim, fit, paint, panel, row, sp, width};
use std::io::{self, Write};

pub struct Target {
    pub name: &'static str,
    /// OAuth opens the browser; a key provider takes a pasted API key.
    pub browser: bool,
    /// an extension credential, listed under Secrets instead of Providers.
    pub secret: bool,
}

/// Four providers, two secrets. The stream names none of them, so all six
/// are made up.
pub fn targets() -> Vec<Target> {
    vec![
        Target { name: "anthropic", browser: true, secret: false },
        Target { name: "openai-codex", browser: true, secret: false },
        Target { name: "cursor", browser: false, secret: false },
        Target { name: "google", browser: false, secret: false },
        Target { name: "linear.api_key", browser: false, secret: true },
        Target { name: "github.token", browser: false, secret: true },
    ]
}

pub struct State {
    /// the focused row, as an index over the provider rows then the secrets
    pub focus: usize,
}

/// Every `--login` case.
pub(crate) const CASES: &[Case<State>] = &[
    Case { name: "providers", help: "providers and extension credentials, OAuth told from key", check: "four providers under `Providers` with `browser` or `key` tags telling OAuth from key providers, and two extension credentials under `Secrets`; a centred panel with ▄ ▀ edges and the ▌ stripe, `Log in` bold accent with a dim ✕, the focused row `›` on a full-width accent bar, and a bold-key legend foot.", build: || State { focus: 0 } },
];

/// `--login`, for `--help` and `check/login.md`.
pub(crate) const SURFACE: Surface = Surface { flag: "--login", file: "login", title: "Login (#1736)", docs: || crate::cases::docs(CASES) };

#[cfg(test)]
pub fn for_case(case: &str) -> State {
    crate::cases::lookup(CASES, case).unwrap_or_else(|| {
        panic!(
            "--login {}",
            crate::cases::names(CASES).replace(", ", "|")
        )
    })
}

/// The foot legend: keys bold, labels muted, naming panel keys the body
/// never lists as choices.
fn footer() -> super::Row {
    panel::footer_legend(&[("↑↓", "move"), ("Enter", "log in"), ("Esc", "close")])
}

/// The panel's body at an inner width: dim group headings over the
/// provider rows and the secret rows, the focused one barred by the caller.
fn body(s: &State, inner: usize) -> Vec<super::Row> {
    let all = targets();
    let key_w = panel::key_width(&all.iter().map(|t| t.name).collect::<Vec<_>>());
    let mut out = vec![row(vec![sp("Providers", dim())])];
    let mut idx = 0;
    for t in all.iter().filter(|t| !t.secret) {
        let tag = if t.browser { "browser" } else { "key" };
        let focused = s.focus == idx;
        if focused {
            out.extend(panel::bar(panel::choice_row(true, t.name, tag, key_w, inner)));
        } else {
            out.extend(panel::choice_row(false, t.name, tag, key_w, inner));
        }
        idx += 1;
    }
    out.push(row(vec![]));
    out.push(row(vec![sp("Secrets", dim())]));
    for t in all.iter().filter(|t| t.secret) {
        let focused = s.focus == idx;
        if focused {
            out.extend(panel::bar(panel::choice_row(true, t.name, "", key_w, inner)));
        } else {
            out.extend(panel::choice_row(false, t.name, "", key_w, inner));
        }
        idx += 1;
    }
    out
}

/// The centred panel rows at an area this wide: a bold accent title with
/// a dim ✕, the body, and the legend foot. Rows stay area-wide, so every
/// click target keeps its coordinates.
pub fn view(s: &State, w: usize) -> Vec<super::Row> {
    let probe = body(s, 10_000);
    let legend = footer();
    let legend_w = width(&legend.spans);
    let natural = probe.iter().map(|r| width(&r.spans)).max().unwrap_or(0).max(legend_w);
    // The legend always fits: the preferred width stretches past the usual
    // cap rather than cutting the foot.
    let prefer = w.saturating_sub(4).min(96).max(legend_w);
    let panel_w = panel::fit_width(natural, prefer, w);
    let inner = panel::inner_w(panel_w);
    let rows = panel::frame(
        Some(panel::title_row("Log in", Some(sp("✕", dim())))),
        body(s, inner),
        Some(footer()),
        panel_w,
    );
    panel::centre(rows, panel_w, w)
}

// ============================================================ frame
/// A row painted at its own offset and width, so the panel's background
/// stays within its edges instead of extending across the screen.
struct Placed {
    x: u16,
    w: u16,
    row: super::Row,
}

fn frame(s: &State, cols: usize, rows: usize) -> Vec<Vec<Placed>> {
    let mut screen: Vec<Vec<Placed>> = overlays::backdrop(cols, rows)
        .into_iter()
        .map(|r| {
            vec![Placed {
                x: 0,
                w: cols as u16,
                row: r,
            }]
        })
        .collect();
    // A small panel floats centred over the dimmed conversation, which
    // reads above and below it.
    let ov = view(s, cols);
    let y0 = rows.saturating_sub(ov.len()) / 2;
    for (k, r) in ov.into_iter().enumerate() {
        if y0 + k >= screen.len() {
            break;
        }
        screen[y0 + k] = vec![Placed {
            x: 0,
            w: cols as u16,
            row: super::Row { spans: fit(&r.spans, cols), ..r },
        }];
    }
    screen
}

fn draw(term: &mut Term, s: &State) -> io::Result<()> {
    let size = term.size()?;
    let (cols, rows) = (size.width.max(1), size.height.max(1));
    term.backend_mut().write_all(b"\x1b[?2026h")?;
    term.draw(|fr| {
        let buf = fr.buffer_mut();
        for (y, placements) in frame(s, cols as usize, rows as usize).iter().enumerate() {
            for p in placements {
                paint(buf, p.x, y as u16, p.w, &p.row);
            }
        }
    })?;
    let be = term.backend_mut();
    be.write_all(b"\x1b[?2026l")?;
    be.flush()?;
    Ok(())
}

/// Draws the `--login` case and waits for a key. Mutually exclusive with the
/// conversation view: the fixture replay never draws.
pub(crate) fn run_login(a: &Args, term: &mut Term) -> io::Result<String> {
    let name = a.login.clone().unwrap_or_default();
    let Some(c) = crate::cases::lookup(CASES, &name) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown --login case {name:?}; one of: {}", crate::cases::names(CASES)),
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
                Ev::Key(Key::Char('c'), m) if m.ctrl => return Ok(format!("login {name}\n")),
                Ev::Key(Key::Esc, _) => return Ok(format!("login {name}\n")),
                Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => {
                    return Ok(format!("login {name}\n"));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plain;

    fn text(s: &State, cols: usize, rows: usize) -> String {
        frame(s, cols, rows)
            .into_iter()
            .map(|ps| {
                ps.iter()
                    .map(|p| p.row.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(CASES.iter().all(|c| crate::cases::lookup(CASES, c.name).is_some()));
        assert!(crate::cases::lookup(CASES, "nope").is_none());
    }

    #[test]
    fn providers_tell_browser_from_key_with_secrets_below() {
        let t = text(&for_case("providers"), 160, 48);
        assert!(t.contains("Providers"), "missing the providers heading");
        assert!(t.contains("Secrets"), "missing the secrets heading");
        // OAuth rows carry the browser tag, key providers the key tag.
        for (name, tag) in [
            ("anthropic", "browser"),
            ("openai-codex", "browser"),
            ("cursor", "key"),
            ("google", "key"),
        ] {
            assert!(t.contains(name), "missing provider {name}");
            let line = t.split('\n').find(|l| l.contains(name)).unwrap();
            assert!(line.contains(tag), "{name} misses its {tag} tag");
        }
        for secret in ["linear.api_key", "github.token"] {
            assert!(t.contains(secret), "missing secret {secret}");
        }
        // The secrets sit below their own heading, after every provider.
        let lines: Vec<&str> = t.split('\n').collect();
        let prov = lines.iter().position(|l| l.contains("Providers")).unwrap();
        let secs = lines.iter().position(|l| l.contains("Secrets")).unwrap();
        assert!(prov < secs, "the Secrets heading is above Providers");
        let last_provider =
            lines.iter().rposition(|l| l.contains("openai-codex") || l.contains("cursor")).unwrap();
        assert!(last_provider < secs, "a provider row leaks below Secrets");
        assert!(t.contains("Log in"), "missing the title");
    }

    #[test]
    fn the_panel_is_centred_with_a_bar_and_legend() {
        let rows = view(&for_case("providers"), 160);
        let t = rows.iter().map(plain).collect::<Vec<_>>().join("\n");
        assert!(t.contains("\u{2584}"), "no top edge");
        assert!(t.contains("\u{2580}"), "no bottom edge");
        assert!(t.contains("\u{258c}"), "no stripe");
        assert!(t.contains("\u{203a} "), "no gutter marker");
        // Centred: the edge run neither starts at the margin nor fills the row.
        let edge = t.split('\n').find(|l| l.contains("\u{2584}")).unwrap();
        let run = edge.chars().filter(|&c| c == '\u{2584}').count();
        assert!(edge.starts_with(' '), "panel flush left");
        assert!(run < 160, "panel fills the area");
        // Only the focused row rides the bar.
        let barred = rows
            .iter()
            .filter(|r| r.spans.iter().any(|s| s.style.bg == Some(crate::BLUE)))
            .count();
        assert_eq!(barred, 1, "more than the focus is barred");
        // The foot is a bold-key legend.
        assert!(t.contains("\u{2191}\u{2193} move · Enter log in · Esc close"));
    }

    #[test]
    fn the_panel_pads_text_off_both_edges() {
        let rows = view(&for_case("providers"), 160);
        let t = rows.iter().map(plain).collect::<Vec<_>>().join("\n");
        let lines: Vec<&str> = t.split('\n').collect();
        assert!(lines[1].trim().is_empty(), "no top pad");
        assert!(lines[lines.len() - 2].trim().is_empty(), "no bottom pad");
    }

    #[test]
    fn no_row_overflows_its_screen() {
        for (cols, rows) in [(160usize, 48usize), (100, 40)] {
            for c in CASES {
                let s = for_case(c.name);
                for ps in frame(&s, cols, rows) {
                    for p in &ps {
                        assert!(crate::width(&p.row.spans) <= cols, "{} overflows at {cols}x{rows}", c.name);
                    }
                }
            }
        }
    }

    #[test]
    fn tiny_terminals_clamp_without_panicking() {
        let rows = view(&for_case("providers"), 30);
        assert!(rows.iter().all(|r| crate::width(&r.spans) <= 30));
        let t = text(&for_case("providers"), 30, 20);
        assert!(t.contains("Log in"), "the title is gone at 30x20");
    }

    #[test]
    fn the_title_and_headings_read_dim_beside_bold() {
        use ratatui::style::Modifier;
        let rows = view(&for_case("providers"), 160);
        let title = rows.iter().find(|r| plain(r).contains("Log in")).unwrap();
        let ink = title.spans.iter().find(|s| s.content.contains("Log in")).unwrap();
        assert!(ink.style.add_modifier.contains(Modifier::BOLD), "title not bold");
        let head = rows.iter().find(|r| plain(r).trim_start_matches([' ', '\u{258c}']).starts_with("Providers")).unwrap();
        let ink = head.spans.iter().find(|s| s.content.contains("Providers")).unwrap();
        assert!(ink.style.add_modifier.contains(Modifier::DIM), "heading not dim");
        assert!(!ink.style.add_modifier.contains(Modifier::BOLD), "heading stands bold beside the title");
    }
}
