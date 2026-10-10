//! `/login` (#1736): the installed providers and the secrets installed
//! extensions declare, drawn as a centred panel over a dimmed conversation.
//! Fixture data only: the prototype has no login flow, so the providers,
//! secrets and every later state below are made up. Each case draws one
//! static frame and waits for a key; Esc, q or Ctrl+C quits.

use crate::cases::{Case, Surface};
use super::input::{Ev, Key};
use super::overlays;
use super::{Args, Term};
use super::{BLUE, RED, dim, fg, fit, left_cut, paint, panel, row, sp, width};
use ratatui::style::Style;
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

/// A login to copy the URL from, long enough to cut at every width: the
/// panel never grows past its cap for it.
const URL: &str = "https://auth.fiber.dev/oauth/authorize?provider=anthropic&ticket=7f3a9c2e4b5d60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d&mode=ssh-fallback";
/// The dots the hidden key draws as.
const DOTS: &str = "••••••••";

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Providers,
    Waiting,
    Key,
    Done,
    Failed,
}

pub struct State {
    pub kind: Kind,
    /// the focused row, as an index over the provider rows then the secrets
    pub focus: usize,
}

/// Every `--login` case.
pub(crate) const CASES: &[Case<State>] = &[
    Case { name: "providers", help: "providers and extension credentials, OAuth told from key", check: "four providers under `Providers` with `browser` or `key` tags telling OAuth from key providers, and two extension credentials under `Secrets`; a centred panel with ▄ ▀ edges and the ▌ stripe, `Log in` bold accent with a dim ✕, the focused row `›` on a full-width accent bar, and a bold-key legend foot.", build: || State { kind: Kind::Providers, focus: 0 } },
    Case { name: "waiting", help: "the browser-path wait, URL to copy, waiting state", check: "the provider list stays with `Open this URL to log in to anthropic:` below it and the long URL cut from the left keeping its tail, `y` to copy, and a dim `Waiting for the browser…` line; a bold-key `y copy URL · Esc cancel` legend; same panel.", build: || State { kind: Kind::Waiting, focus: 0 } },
    Case { name: "key", help: "key entry with the key masked", check: "the provider list stays with `Label (--as): default.` and `Key for google:` below it, the key masked as eight dots with a block cursor; a bold-key `Tab key or label · Enter store · Esc cancel` legend; same panel.", build: || State { kind: Kind::Key, focus: 3 } },
    Case { name: "done", help: "the logged-in outcome", check: "the provider list stays with a `✓ Logged in to anthropic.` outcome line in the success colour; a bold-key `Esc close` legend; same panel.", build: || State { kind: Kind::Done, focus: 0 } },
    Case { name: "failed", help: "the failed outcome with its reason", check: "the provider list stays with a `Login to anthropic failed: token expired.` outcome line, the reason in the error colour; a bold-key `Esc close` legend; same panel.", build: || State { kind: Kind::Failed, focus: 0 } },
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

/// The foot legend for the case: keys bold, labels muted, naming panel
/// keys the body never lists as choices.
fn footer(s: &State) -> super::Row {
    match s.kind {
        Kind::Providers => panel::footer_legend(&[("↑↓", "move"), ("Enter", "log in"), ("Esc", "close")]),
        Kind::Waiting => panel::footer_legend(&[("y", "copy URL"), ("Esc", "cancel")]),
        Kind::Key => panel::footer_legend(&[("Tab", "key or label"), ("Enter", "store"), ("Esc", "cancel")]),
        Kind::Done | Kind::Failed => panel::footer_legend(&[("Esc", "close")]),
    }
}

/// The rows below the list for a waiting or key case at an inner width.
/// The list stays; the login's progress reads under it.
fn below(kind: Kind, inner: usize) -> Vec<super::Row> {
    match kind {
        Kind::Providers => vec![],
        Kind::Waiting => vec![
            row(vec![sp("Open this URL to log in to anthropic:", Style::new())]),
            row(vec![sp(left_cut(URL, inner), fg(BLUE))]),
            row(vec![sp("Waiting for the browser…", dim())]),
        ],
        Kind::Key => vec![
            row(vec![sp("Label (--as): default.", dim())]),
            row(vec![sp("Key for google:", Style::new())]),
            row(vec![sp(DOTS, Style::new()), sp("█", dim())]),
        ],
        Kind::Done => vec![row(vec![sp("✓ Logged in to anthropic.", fg(BLUE))])],
        Kind::Failed => vec![row(vec![
            sp("Login to anthropic failed: ", Style::new()),
            sp("token expired", fg(RED)),
            sp(".", Style::new()),
        ])],
    }
}

/// The panel's body at an inner width: dim group headings over the
/// provider rows and the secret rows, the focused one barred, then the
/// case's rows below the list.
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
    let extra = below(s.kind, inner);
    if !extra.is_empty() {
        out.push(row(vec![]));
        out.extend(extra);
    }
    out
}

/// The centred panel rows at an area this wide: a bold accent title with
/// a dim ✕, the body, and the legend foot. Rows stay area-wide, so every
/// click target keeps its coordinates.
pub fn view(s: &State, w: usize) -> Vec<super::Row> {
    let probe = body(s, 10_000);
    let legend = footer(s);
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
        Some(footer(s)),
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
        assert_eq!(CASES.len(), 5);
        assert!(crate::cases::lookup(CASES, "nope").is_none());
    }

    #[test]
    fn waiting_keeps_the_list_under_a_cut_url() {
        let t = text(&for_case("waiting"), 160, 48);
        // The list stays: the rows and the focused bar are still there.
        for name in ["anthropic", "Providers", "Secrets"] {
            assert!(t.contains(name), "the list lost {name}");
        }
        assert!(t.contains("\u{203a} "), "the focus is gone");
        assert!(t.contains("Open this URL to log in to anthropic:"), "missing the prompt");
        assert!(t.contains("Waiting for the browser…"), "missing the waiting state");
        assert!(t.contains("y copy URL · Esc cancel"), "missing the legend");
        // The URL cuts from the left, keeping its tail, and fits its row.
        let line = t.split('\n').find(|l| l.contains("mode=ssh-fallback")).unwrap();
        let shown = line.trim_start_matches([' ', '\u{258c}']).trim_end();
        assert!(shown.starts_with('…'), "the long URL is not cut: {shown:?}");
        assert!(shown.ends_with("mode=ssh-fallback"), "the cut lost the tail: {shown:?}");
        assert!(crate::width(&[sp(shown, Style::new())]) <= panel::inner_w(101), "the cut URL overflows its panel");
        assert!(URL.chars().count() > shown.chars().count(), "the fixture URL fits uncut");
    }

    #[test]
    fn the_cut_url_fits_at_both_widths() {
        // At, just below and just above the panel's preferred width the
        // shown URL never exceeds the inner width it is cut to.
        for cols in [100, 101, 160] {
            let rows = view(&for_case("waiting"), cols);
            let w = rows
                .iter()
                .map(|r| crate::width(&r.spans))
                .max()
                .unwrap_or(0);
            let line = rows.iter().map(plain).find(|l| l.contains("mode=ssh-fallback")).unwrap();
            let shown = line.trim_start_matches([' ', '\u{2584}', '\u{2580}', '\u{258c}']).trim_end();
            assert!(shown.starts_with('…'), "uncut at {cols}");
            assert!(w <= cols, "the panel overflows at {cols}");
        }
    }

    #[test]
    fn key_masks_the_key_with_dots() {
        let t = text(&for_case("key"), 160, 48);
        // The list stays above the prompts.
        assert!(t.contains("google"), "the list is gone");
        assert!(t.contains("Label (--as): default."), "missing the label line");
        assert!(t.contains("Key for google:"), "missing the key prompt");
        assert!(t.contains(DOTS), "the key is not masked");
        assert_eq!(DOTS.chars().count(), 8, "the mask is not eight dots");
        assert!(t.contains("Tab key or label · Enter store · Esc cancel"), "missing the legend");
        // The mask reads on one row with the block cursor after it.
        let line = t.split('\n').find(|l| l.contains(DOTS)).unwrap();
        assert!(line.trim_end().ends_with('█'), "no cursor after the mask");
    }

    #[test]
    fn every_case_keeps_one_bar_and_its_legend() {
        for c in ["providers", "waiting", "key", "done", "failed"] {
            let rows = view(&for_case(c), 100);
            let barred = rows
                .iter()
                .filter(|r| r.spans.iter().any(|s| s.style.bg == Some(crate::BLUE)))
                .count();
            assert_eq!(barred, 1, "{c}: more than the focus is barred");
        }
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
    fn done_says_logged_in_and_failed_names_its_reason() {
        let t = text(&for_case("done"), 160, 48);
        assert!(t.contains("anthropic"), "the list is gone");
        assert!(t.contains("✓ Logged in to anthropic."), "missing the outcome");
        assert!(t.contains("Esc close"), "missing the legend");
        let rows = view(&for_case("done"), 160);
        let line = rows.iter().find(|r| plain(r).contains("Logged in")).unwrap();
        let ink = line.spans.iter().find(|s| s.content.contains("✓")).unwrap();
        assert_eq!(ink.style.fg, Some(crate::BLUE), "the outcome is not the success colour");
        let t = text(&for_case("failed"), 160, 48);
        assert!(t.contains("Login to anthropic failed: token expired."), "missing the outcome");
        assert!(t.contains("Esc close"), "missing the legend");
        let rows = view(&for_case("failed"), 160);
        let line = rows.iter().find(|r| plain(r).contains("failed")).unwrap();
        let ink = line.spans.iter().find(|s| s.content.contains("token expired")).unwrap();
        assert_eq!(ink.style.fg, Some(crate::RED), "the reason is not the error colour");
        let head = line.spans.iter().find(|s| s.content.contains("Login to")).unwrap();
        assert_ne!(head.style.fg, Some(crate::RED), "the whole line went red");
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
