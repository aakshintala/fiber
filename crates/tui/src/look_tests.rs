//! Tests for the look: colour depth, the 256-colour mapping and the paint
//! pass.

use super::{Depth, Look, ThemeSetting, among, ansi256, depth};
use crate::theme::{ROLES, Role, Shade, Theme};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// An environment reader over `vars`.
fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let vars: Vec<(String, String)> = vars
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    move |name| {
        vars.iter()
            .find(|(set, _)| set == name)
            .map(|(_, value)| value.clone())
    }
}

/// The look for `setting` on a terminal described by `vars`.
fn look(setting: ThemeSetting, vars: &[(&str, &str)]) -> Look {
    Look::new(setting, &env(vars)).0
}

#[test]
fn depth_detection() {
    let cases: [(&[(&str, &str)], Depth); 13] = [
        (&[("NO_COLOR", ""), ("COLORTERM", "truecolor")], Depth::True),
        (
            &[("NO_COLOR", "1"), ("COLORTERM", "truecolor")],
            Depth::NoColour,
        ),
        (&[("COLORTERM", "truecolor")], Depth::True),
        (&[("COLORTERM", "24bit")], Depth::True),
        (
            &[("COLORTERM", "yes"), ("TERM", "xterm-256color")],
            Depth::Ansi256,
        ),
        (&[("TERM", "xterm-ghostty")], Depth::True),
        (&[("TERM", "xterm-kitty")], Depth::True),
        (&[("TERM", "wezterm")], Depth::True),
        (&[("TERM", "xterm-direct")], Depth::True),
        (&[("TERM", "xterm-direct2")], Depth::Ansi256),
        (&[("TERM", "xterm")], Depth::Ansi256),
        (&[("TERM", "xterm-256color")], Depth::Ansi256),
        (&[], Depth::Ansi256),
    ];
    for (vars, want) in cases {
        assert_eq!(depth(&env(vars)), want, "{vars:?}");
    }
}

/// Every entry but the terminal's own sixteen.
const ALL: std::ops::RangeInclusive<u8> = 16..=255;
/// The colour cube.
const CUBE: std::ops::RangeInclusive<u8> = 16..=231;
/// The grey ramp.
const GREY: std::ops::RangeInclusive<u8> = 232..=255;

#[test]
fn ansi256_known_values() {
    let cases = [
        ((0x00, 0x00, 0x00), ALL, 16),
        ((0xff, 0xff, 0xff), ALL, 231),
        ((0x80, 0x80, 0x80), ALL, 244),
        ((0x5f, 0x00, 0x00), ALL, 52),
        ((0x00, 0x5f, 0x00), ALL, 22),
        ((0x00, 0x00, 0x5f), ALL, 17),
        ((0xff, 0x00, 0x00), ALL, 196),
        ((0x80, 0x80, 0x80), CUBE, 102),
        ((0xff, 0xff, 0xff), CUBE, 231),
        ((0x28, 0x2c, 0x34), GREY, 236),
        ((0x00, 0x00, 0x00), GREY, 232),
        ((0xff, 0xff, 0xff), GREY, 255),
        // 13 sits between greys 232 (8) and 233 (18): the lower wins.
        ((0x0d, 0x0d, 0x0d), GREY, 232),
        ((0x0d, 0x0d, 0x0d), ALL, 232),
        // 115 sits between cube levels 95 (52) and 135 (88).
        ((0x73, 0x00, 0x00), CUBE, 52),
    ];
    for (rgb, among, want) in cases {
        assert_eq!(ansi256(rgb, among.clone()), want, "{rgb:?} {among:?}");
    }
}

#[test]
fn ansi256_stays_among_the_entries_it_is_given() {
    for value in (0..=255u8).step_by(5) {
        for rgb in [
            (value, 0, 0),
            (0, value, 0),
            (0, 0, value),
            (value, value, value),
        ] {
            for among in [ALL, CUBE, GREY] {
                let got = ansi256(rgb, among.clone());
                assert!(among.contains(&got), "{rgb:?} {among:?}: {got}");
            }
        }
    }
}

/// The xterm-256 entry `role` resolves to on a 256-colour terminal.
fn at_256(look: &Look, role: Role) -> u8 {
    let colour = look.colour(role);
    let Color::Indexed(index) = colour else {
        panic!("{}: {colour:?}", role.name());
    };
    index
}

#[test]
fn among_by_role() {
    // A neutral tint takes the grey ramp, any other tint the colour cube,
    // and a text role either (`docs/tui.md`, "Look").
    assert_eq!(among(Role::Surface), GREY);
    assert_eq!(among(Role::CardRetrying), CUBE);
    assert_eq!(among(Role::CardNeedsInput), CUBE);
    assert_eq!(among(Role::CardReady), GREY);
    assert_eq!(among(Role::CardCrashed), GREY);
    assert_eq!(among(Role::Rule), GREY);
    assert_eq!(among(Role::Handoff), CUBE);
    assert_eq!(among(Role::Alert), CUBE);
    assert_eq!(among(Role::Scroll), ALL);
    assert_eq!(among(Role::Info), ALL);
    assert_eq!(among(Role::Accent), ALL);
}

#[test]
fn dark_at_256_known_entries() {
    // The dark theme at 256 colours, role by role: the terminal's own
    // colours stay the default, every other role its nearest xterm entry.
    let look = look(ThemeSetting::Dark, &[("TERM", "xterm-256color")]);
    for role in [
        Role::Text,
        Role::Muted,
        Role::CodeText,
        Role::Operator,
        Role::Background,
    ] {
        assert_eq!(look.colour(role), Color::Reset, "{}", role.name());
    }
    let table = [
        (Role::Accent, 75),
        (Role::Heading, 215),
        (Role::Success, 75),
        (Role::Warning, 215),
        (Role::Error, 203),
        (Role::Attention, 215),
        (Role::Added, 78),
        (Role::Removed, 203),
        (Role::Keyword, 75),
        (Role::String, 174),
        (Role::Comment, 244),
        (Role::Number, 151),
        (Role::Function, 117),
        (Role::Type, 117),
        (Role::Constant, 117),
        (Role::Info, 117),
        (Role::Secondary, 146),
        (Role::Rule, 238),
        (Role::Scroll, 244),
        (Role::Panel, 233),
        (Role::Surface, 234),
        (Role::SurfaceRaised, 238),
        (Role::CardRetrying, 16),
        (Role::CardNeedsInput, 16),
        (Role::CardReady, 233),
        (Role::CardCrashed, 233),
        (Role::Turn, 233),
        (Role::Prompt, 237),
        (Role::Code, 234),
        (Role::Handoff, 16),
        (Role::Approval, 234),
        (Role::Alert, 52),
        (Role::Hover, 234),
        (Role::Selection, 24),
        (Role::Match, 58),
        (Role::MatchCurrent, 215),
    ];
    assert_eq!(table.len(), ROLES - 5);
    for (role, index) in table {
        assert_eq!(look.colour(role), Color::Indexed(index), "{}", role.name());
    }
}

#[test]
fn the_defaults_stay_the_default_at_every_depth() {
    // In the dark theme `text`, `code_text`, `operator` and `background`
    // are the terminal's own colours and `muted` is dim over the default
    // foreground, so they stay the default at every depth (`docs/tui.md`,
    // "Themes").
    let depths: [&[(&str, &str)]; 3] = [
        &[("COLORTERM", "truecolor")],
        &[("TERM", "xterm-256color")],
        &[("NO_COLOR", "1")],
    ];
    for (nth, vars) in depths.into_iter().enumerate() {
        let look = look(ThemeSetting::Dark, vars);
        for role in [
            Role::Text,
            Role::CodeText,
            Role::Operator,
            Role::Muted,
            Role::Background,
        ] {
            assert_eq!(look.colour(role), Color::Reset, "{} {vars:?}", role.name());
        }
        if nth < 2 {
            assert_ne!(look.colour(Role::Accent), Color::Reset, "{vars:?}");
        }
    }
}

#[test]
fn each_role_resolves_among_its_entries_at_256() {
    // Every role resolves among its documented entries, with the white
    // page's exception: the page is 231, a tint that would resolve to
    // 231 takes 255, and `muted` follows the theme's text shade
    // (`docs/tui.md`, "Look", "Themes").
    let vars = [("TERM", "xterm-256color")];
    for (setting, theme) in [
        (ThemeSetting::Dark, Theme::DARK),
        (ThemeSetting::Light, Theme::LIGHT),
    ] {
        let look = look(setting, &vars);
        let white_page = theme.shade(Role::Background) == Shade::Rgb((255, 255, 255));
        for role in Role::ALL {
            let shade = theme.shade(role);
            if matches!(shade, Shade::Terminal) {
                assert_eq!(look.colour(role), Color::Reset, "{}", role.name());
            } else if matches!(shade, Shade::Dim) {
                assert!(look.dim(role), "{}", role.name());
                match theme.shade(Role::Text) {
                    Shade::Terminal | Shade::Dim => {
                        assert_eq!(look.colour(role), Color::Reset, "{}", role.name())
                    }
                    Shade::Rgb(rgb) => assert_eq!(
                        look.colour(role),
                        Color::Indexed(ansi256(rgb, 16..=255)),
                        "{}",
                        role.name()
                    ),
                }
            } else {
                let Shade::Rgb(rgb) = shade else {
                    panic!("{}: {shade:?}", role.name());
                };
                let among = if role.grey() {
                    GREY
                } else if role.tint() {
                    CUBE
                } else {
                    ALL
                };
                let mut want = ansi256(rgb, among);
                if white_page && role == Role::Background {
                    want = 231;
                } else if white_page && role.tint() && want == 231 {
                    want = 255;
                }
                assert_eq!(look.colour(role), Color::Indexed(want), "{}", role.name());
            }
        }
    }
}

#[test]
fn grey_roles_resolve_in_the_ramp() {
    let vars = [("TERM", "xterm-256color")];
    for (setting, theme) in [
        (ThemeSetting::Dark, Theme::DARK),
        (ThemeSetting::Light, Theme::LIGHT),
    ] {
        let look = look(setting, &vars);
        let white_page = theme.shade(Role::Background) == Shade::Rgb((255, 255, 255));
        for role in Role::ALL.into_iter().filter(|role| role.grey()) {
            if matches!(theme.shade(role), Shade::Terminal | Shade::Dim) {
                assert_eq!(look.colour(role), Color::Reset, "{}", role.name());
            } else if white_page && role == Role::Background {
                // The white page is the cube's white, 231, which the
                // grey ramp lacks (`docs/tui.md`, "Themes").
                assert_eq!(look.colour(role), Color::Indexed(231), "{}", role.name());
            } else {
                let index = at_256(&look, role);
                assert!(GREY.contains(&index), "{}: {index}", role.name());
            }
        }
    }
}

#[test]
fn alert_stays_coloured_at_256() {
    let look = look(ThemeSetting::Dark, &[("TERM", "xterm-256color")]);
    // The dark alert's nearest entry of all is a grey; the cube's is red.
    assert_eq!(ansi256((0x50, 0x1c, 0x20), ALL), 236);
    assert_eq!(at_256(&look, Role::Alert), 52);
}

#[test]
fn a_text_role_may_take_a_grey_at_256() {
    let look = look(ThemeSetting::Dark, &[("TERM", "xterm-256color")]);
    // The dark scroll's nearest entry is grey 244, not the cube's 102.
    assert_eq!(at_256(&look, Role::Scroll), 244);
    assert_eq!(ansi256((0x80, 0x80, 0x80), CUBE), 102);
}

/// A one-row buffer holding `cells`, each a symbol and its style.
fn row(cells: &[(&str, Style)]) -> Buffer {
    let width = u16::try_from(cells.len()).expect("a width");
    let mut buf = Buffer::empty(Rect::new(0, 0, width, 1));
    for (x, (symbol, style)) in (0..).zip(cells) {
        buf.set_string(x, 0, symbol, *style);
    }
    buf
}

#[test]
fn paint_resolves_markers_to_the_theme() {
    let look = look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")]);
    let surface = Style::new().fg(Role::Surface.color());
    let mut buf = row(&[
        (
            "a",
            Style::new()
                .fg(Role::Accent.color())
                .bg(Role::Background.color()),
        ),
        ("▄", surface),
    ]);
    look.paint(&mut buf);
    assert_eq!(buf[(0, 0)].fg, Color::Rgb(0x6e, 0xaa, 0xfe));
    assert_eq!(buf[(0, 0)].bg, Color::Reset);
    // With colour, an edge keeps its half block.
    assert_eq!(buf[(1, 0)].symbol(), "▄");
    assert_eq!(buf[(1, 0)].fg, Color::Rgb(0x1a, 0x1a, 0x22));
}

#[test]
fn paint_at_256_writes_palette_entries() {
    let look = look(ThemeSetting::Dark, &[("TERM", "xterm-256color")]);
    let mut buf = row(&[("a", Style::new().fg(Role::Accent.color()))]);
    look.paint(&mut buf);
    assert_eq!(buf[(0, 0)].fg, Color::Indexed(75));
}

#[test]
fn paint_lifts_a_tint_marker() {
    // Dark `surface` lifted 0x14 per channel (`docs/tui.md`, palette,
    // `hover`).
    let look = look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")]);
    let mut buf = row(&[("a", Style::new().bg(Role::Surface.lifted()))]);
    look.paint(&mut buf);
    assert_eq!(buf[(0, 0)].bg, Color::Rgb(0x2e, 0x2e, 0x36));
}

#[test]
fn paint_lifts_the_light_surface() {
    // Light `surface` (#eaeef2) averages above 0x80, so the lift moves
    // 0x14 per channel away from the page: darker (`docs/tui.md`,
    // palette, `hover`).
    let look = look(ThemeSetting::Light, &[("COLORTERM", "truecolor")]);
    let mut buf = row(&[("a", Style::new().bg(Role::Surface.lifted()))]);
    look.paint(&mut buf);
    assert_eq!(buf[(0, 0)].bg, Color::Rgb(0xd6, 0xda, 0xde));
}

#[test]
fn lifted_light_surface_at_256_takes_the_grey_ramp() {
    // At 256 colours the lifted light surface takes the surface's grey
    // ramp entry: the lift, the role's ramp and the white-page rule,
    // end to end.
    let look = look(ThemeSetting::Light, &[("TERM", "xterm-256color")]);
    let mut buf = row(&[("a", Style::new().bg(Role::Surface.lifted()))]);
    look.paint(&mut buf);
    let Color::Indexed(index) = buf[(0, 0)].bg else {
        panic!("lifted surface at 256: {:?}", buf[(0, 0)].bg);
    };
    assert!(GREY.contains(&index), "grey ramp: {index}");
    assert_eq!(index, 253, "the lifted surface's entry");
}

#[test]
fn lift_at_the_threshold_goes_darker() {
    // An input exactly at the threshold: dark `scroll` (#808080)
    // averages 0x80, so the lift moves darker, not lighter
    // (`docs/tui.md`, palette, `hover`).
    let look = look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")]);
    let mut buf = row(&[("a", Style::new().bg(Role::Scroll.lifted()))]);
    look.paint(&mut buf);
    assert_eq!(buf[(0, 0)].bg, Color::Rgb(0x6c, 0x6c, 0x6c));
}

#[test]
fn literal_rgb_resolves_by_depth() {
    let fg = Style::new()
        .fg(Color::Rgb(0, 0, 0))
        .bg(Color::Rgb(255, 255, 255));
    let mut true_buf = row(&[("a", fg)]);
    look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")]).paint(&mut true_buf);
    assert_eq!(true_buf[(0, 0)].fg, Color::Rgb(0, 0, 0));
    assert_eq!(true_buf[(0, 0)].bg, Color::Rgb(255, 255, 255));
    let mut ansi_buf = row(&[("a", fg)]);
    look(ThemeSetting::Dark, &[("TERM", "xterm-256color")]).paint(&mut ansi_buf);
    assert_eq!(
        ansi_buf[(0, 0)].fg,
        Color::Indexed(ansi256((0, 0, 0), 16..=255))
    );
    assert_eq!(
        ansi_buf[(0, 0)].bg,
        Color::Indexed(ansi256((255, 255, 255), 16..=255))
    );
    let mut none_buf = row(&[("a", fg)]);
    look(ThemeSetting::Dark, &[("NO_COLOR", "1")]).paint(&mut none_buf);
    assert_eq!(none_buf[(0, 0)].fg, Color::Reset);
    assert_eq!(none_buf[(0, 0)].bg, Color::Reset);
}

#[test]
fn paint_leaves_reset_and_other_colours() {
    let look = Look::default();
    let mut buf = row(&[
        ("a", Style::new().fg(Color::Reset).bg(Color::Reset)),
        (
            "b",
            Style::new().fg(Color::Indexed(41)).bg(Color::Rgb(1, 2, 3)),
        ),
        ("c", Style::new().fg(Color::Red).bg(Color::Indexed(255))),
    ]);
    let before = buf.clone();
    look.paint(&mut buf);
    assert_eq!(buf, before);
}

#[test]
fn no_color_resets_every_role_and_keeps_modifiers() {
    let look = look(ThemeSetting::Dark, &[("NO_COLOR", "1")]);
    for role in Role::ALL {
        assert_eq!(look.colour(role), Color::Reset, "{}", role.name());
    }
    let mut buf = row(&[(
        "a",
        Style::new()
            .fg(Role::Error.color())
            .bg(Role::Alert.color())
            .add_modifier(Modifier::BOLD | Modifier::REVERSED),
    )]);
    look.paint(&mut buf);
    let cell = &buf[(0, 0)];
    assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset));
    assert_eq!(cell.modifier, Modifier::BOLD | Modifier::REVERSED);
    assert_eq!(cell.symbol(), "a");
}

#[test]
fn no_color_blanks_edges_but_not_text_half_blocks() {
    let look = look(ThemeSetting::Dark, &[("NO_COLOR", "1")]);
    let mut buf = row(&[
        ("▄", Style::new().fg(Role::Surface.color())),
        ("▀", Style::new().fg(Role::Prompt.color())),
        ("▄", Style::new().fg(Role::Accent.color())),
        ("▀", Style::new().fg(Color::Indexed(41))),
        ("x", Style::new().fg(Role::Surface.color())),
    ]);
    look.paint(&mut buf);
    let symbols: Vec<&str> = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert_eq!(symbols, [" ", " ", "▄", "▀", "x"]);
}

#[test]
fn paint_dims_a_dim_role_only() {
    // Dark `muted` is the default foreground drawn dim: a cell it marks
    // as a foreground gains DIM and keeps its other modifiers, while a
    // background marker never adds DIM (`docs/tui.md`, "Themes").
    let look = look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")]);
    let mut buf = row(&[
        (
            "m",
            Style::new()
                .fg(Role::Muted.color())
                .add_modifier(Modifier::BOLD),
        ),
        (
            "a",
            Style::new()
                .fg(Role::Accent.color())
                .add_modifier(Modifier::BOLD),
        ),
        (
            "b",
            Style::new()
                .bg(Role::Muted.color())
                .add_modifier(Modifier::BOLD),
        ),
        ("t", Style::new().fg(Role::Text.color())),
    ]);
    look.paint(&mut buf);
    assert_eq!(buf[(0, 0)].fg, Color::Reset);
    assert_eq!(buf[(0, 0)].modifier, Modifier::BOLD | Modifier::DIM);
    assert_eq!(buf[(1, 0)].modifier, Modifier::BOLD);
    assert_eq!(buf[(2, 0)].modifier, Modifier::BOLD);
    assert_eq!(buf[(3, 0)].modifier, Modifier::empty());
}

#[test]
fn dim_follows_the_theme() {
    // DIM follows the theme's shade for the role: light `muted` is the
    // theme's text shade drawn dim, and a file that sets `muted` to a
    // hex gets that colour with no dim. Under NO_COLOR `muted` keeps
    // DIM over the default.
    let truecolour = [("COLORTERM", "truecolor")];
    let light = look(ThemeSetting::Light, &truecolour);
    assert!(light.dim(Role::Muted));
    assert_eq!(light.colour(Role::Muted), Color::Rgb(0x1f, 0x23, 0x28));
    let mut buf = row(&[("m", Style::new().fg(Role::Muted.color()))]);
    light.paint(&mut buf);
    assert_eq!(buf[(0, 0)].fg, Color::Rgb(0x1f, 0x23, 0x28));
    assert_eq!(buf[(0, 0)].modifier, Modifier::DIM);
    let (solar, notice) = Look::new(
        ThemeSetting::File {
            name: "solar".to_owned(),
            text: Ok(r##"{"base": "dark", "roles": {"muted": "#123456"}}"##.to_owned()),
        },
        &env(&truecolour),
    );
    assert_eq!(notice, None);
    assert!(!solar.dim(Role::Muted));
    let mut buf = row(&[("m", Style::new().fg(Role::Muted.color()))]);
    solar.paint(&mut buf);
    assert_eq!(buf[(0, 0)].fg, Color::Rgb(0x12, 0x34, 0x56));
    assert_eq!(buf[(0, 0)].modifier, Modifier::empty());
    let dark = look(ThemeSetting::Dark, &[("NO_COLOR", "1")]);
    assert!(dark.dim(Role::Muted));
    let mut buf = row(&[("m", Style::new().fg(Role::Muted.color()))]);
    dark.paint(&mut buf);
    assert_eq!(buf[(0, 0)].fg, Color::Reset);
    assert_eq!(buf[(0, 0)].modifier, Modifier::DIM);
}

#[test]
fn a_report_re_resolves_dim() {
    use super::Appearance;
    // `muted` stays dim in both themes; only its colour re-resolves:
    // the default foreground while dark, the theme's text shade light.
    let mut followed = following(&[("COLORTERM", "truecolor")]);
    assert!(followed.dim(Role::Muted));
    assert_eq!(followed.colour(Role::Muted), Color::Reset);
    assert!(followed.appearance(Appearance::Light));
    assert!(followed.dim(Role::Muted));
    assert_eq!(followed.colour(Role::Muted), Color::Rgb(0x1f, 0x23, 0x28));
    assert!(followed.appearance(Appearance::Dark));
    assert!(followed.dim(Role::Muted));
    assert_eq!(followed.colour(Role::Muted), Color::Reset);
}

#[test]
fn new_with_a_bad_file_follows_and_says_why() {
    let vars = [("COLORTERM", "truecolor")];
    let cases = [
        (
            Err("No such file or directory (os error 2)".to_owned()),
            "Theme \"solar\": No such file or directory (os error 2); following the terminal's appearance.",
        ),
        (
            Ok(r##"{"base": "light", "roles": {"acent": "#0b7285"}}"##.to_owned()),
            "Theme \"solar\": unknown role \"acent\"; following the terminal's appearance.",
        ),
    ];
    for (text, notice) in cases {
        let setting = ThemeSetting::File {
            name: "solar".to_owned(),
            text,
        };
        let (got, said) = Look::new(setting, &env(&vars));
        assert_eq!(said.as_deref(), Some(notice));
        assert_eq!(got, look(ThemeSetting::Follow, &vars));
    }
}

#[test]
fn new_with_dark_light_follow_and_a_file() {
    let vars = [("COLORTERM", "truecolor")];
    let colour = |look: &Look, role| look.colour(role);
    let dark = look(ThemeSetting::Dark, &vars);
    let light = look(ThemeSetting::Light, &vars);
    assert_eq!(colour(&dark, Role::Background), Color::Reset);
    assert_eq!(
        colour(&light, Role::Background),
        Color::Rgb(0xff, 0xff, 0xff)
    );
    // Before the terminal reports its appearance, following is dark: the
    // same colours, while only the follower re-resolves on a report.
    let follow = look(ThemeSetting::Follow, &vars);
    assert_eq!(follow.colour(Role::Text), dark.colour(Role::Text));
    assert_eq!(Look::default().colour(Role::Text), dark.colour(Role::Text));
    let (solar, notice) = Look::new(
        ThemeSetting::File {
            name: "solar".to_owned(),
            text: Ok(r##"{"base": "light", "roles": {"accent": "#0b7285"}}"##.to_owned()),
        },
        &env(&vars),
    );
    assert_eq!(notice, None);
    assert_eq!(colour(&solar, Role::Accent), Color::Rgb(0x0b, 0x72, 0x85));
    assert_eq!(
        colour(&solar, Role::Background),
        Color::Rgb(0xff, 0xff, 0xff)
    );
}

/// A look that follows the terminal on `vars`.
fn following(vars: &[(&str, &str)]) -> Look {
    look(ThemeSetting::Follow, vars)
}

#[test]
fn follow_switches_on_a_report() {
    use super::Appearance;
    let vars = [("COLORTERM", "truecolor")];
    let mut followed = following(&vars);
    let dark = look(ThemeSetting::Dark, &vars);
    let light = look(ThemeSetting::Light, &vars);
    assert_eq!(followed.colour(Role::Text), dark.colour(Role::Text));
    assert!(followed.appearance(Appearance::Light));
    assert_eq!(followed.colour(Role::Text), light.colour(Role::Text));
    assert!(followed.appearance(Appearance::Dark));
    assert_eq!(followed.colour(Role::Text), dark.colour(Role::Text));
}

#[test]
fn a_fixed_theme_ignores_reports_but_records_them() {
    use super::Appearance;
    let vars = [("COLORTERM", "truecolor")];
    for setting in [
        ThemeSetting::Dark,
        ThemeSetting::Light,
        ThemeSetting::File {
            name: "solar".to_owned(),
            text: Ok(r##"{"base": "light", "roles": {"accent": "#0b7285"}}"##.to_owned()),
        },
    ] {
        let mut fixed = look(setting, &vars);
        let before = fixed.colour(Role::Text);
        assert!(!fixed.appearance(Appearance::Light));
        assert_eq!(fixed.reported(), Appearance::Light);
        assert_eq!(fixed.colour(Role::Text), before);
        assert!(!fixed.appearance(Appearance::Dark));
        assert_eq!(fixed.reported(), Appearance::Dark);
        assert_eq!(fixed.colour(Role::Text), before);
    }
}

#[test]
fn the_same_appearance_is_no_change() {
    use super::Appearance;
    let vars = [("COLORTERM", "truecolor")];
    let mut followed = following(&vars);
    assert!(!followed.appearance(Appearance::Dark));
    assert!(followed.appearance(Appearance::Light));
    assert!(!followed.appearance(Appearance::Light));
}

#[test]
fn a_refused_file_follows() {
    use super::Appearance;
    let vars = [("COLORTERM", "truecolor")];
    let mut refused = look(
        ThemeSetting::File {
            name: "solar".to_owned(),
            text: Err("gone".to_owned()),
        },
        &vars,
    );
    let light = look(ThemeSetting::Light, &vars);
    assert!(refused.appearance(Appearance::Light));
    assert_eq!(refused.colour(Role::Text), light.colour(Role::Text));
}

/// A file theme on `base` with `roles` set to `rgb` each.
fn file(base: &str, roles: &[(&str, &str)]) -> ThemeSetting {
    let set: Vec<String> = roles
        .iter()
        .map(|(role, rgb)| format!("\"{role}\": \"{rgb}\""))
        .collect();
    ThemeSetting::File {
        name: "solar".to_owned(),
        text: Ok(format!(
            "{{\"base\": \"{base}\", \"roles\": {{{}}}}}",
            set.join(", ")
        )),
    }
}

#[test]
fn the_light_page_is_231_and_every_tint_stays_visible() {
    // The light theme paints its own page: at 256 colours it is the
    // colour cube's white, 231, which the grey ramp lacks, and no other
    // tint resolves to it (`docs/tui.md`, "Themes").
    let light = look(ThemeSetting::Light, &[("TERM", "xterm-256color")]);
    assert_eq!(light.colour(Role::Background), Color::Indexed(231));
    for role in Role::ALL
        .into_iter()
        .filter(|role| role.tint() && *role != Role::Background)
    {
        assert_ne!(light.colour(role), Color::Indexed(231), "{}", role.name());
    }
    // The dark page stays the terminal's own background at 256 colours.
    let dark = look(ThemeSetting::Dark, &[("TERM", "xterm-256color")]);
    assert_eq!(dark.colour(Role::Background), Color::Reset);
}

#[test]
fn only_a_tint_at_231_remaps_on_a_white_page() {
    let ansi = [("TERM", "xterm-256color")];
    // A cube tint of exactly white would vanish on the page: it takes
    // the grey ramp's lightest instead (`docs/tui.md`, "Themes").
    let white_alert = look(file("light", &[("alert", "#ffffff")]), &ansi);
    assert_eq!(white_alert.colour(Role::Alert), Color::Indexed(255));
    assert_eq!(white_alert.colour(Role::Background), Color::Indexed(231));
    // A nearby tint whose nearest entry is not 231 keeps it.
    let alert = ansi256((0xff, 0xd7, 0xd9), CUBE);
    assert_ne!(alert, 231);
    let near = look(file("light", &[("alert", "#ffd7d9")]), &ansi);
    assert_eq!(near.colour(Role::Alert), Color::Indexed(alert));
    // A text role may still take 231: only tints remap.
    let white_text = look(file("light", &[("text", "#ffffff")]), &ansi);
    assert_eq!(white_text.colour(Role::Text), Color::Indexed(231));
}

#[test]
fn a_nonwhite_or_default_page_keeps_the_old_mapping() {
    let ansi = [("TERM", "xterm-256color")];
    // `#fefefe` is not exactly white: the page takes its grey nearest,
    // and a cube tint at 231 is left alone without the exception.
    let near_white = look(
        file("light", &[("background", "#fefefe"), ("alert", "#ffffff")]),
        &ansi,
    );
    assert_eq!(
        near_white.colour(Role::Background),
        Color::Indexed(ansi256((0xfe, 0xfe, 0xfe), GREY))
    );
    assert_ne!(near_white.colour(Role::Background), Color::Indexed(231));
    assert_eq!(near_white.colour(Role::Alert), Color::Indexed(231));
    // A coloured page keeps the current role and ramp mapping.
    let coloured = look(file("dark", &[("background", "#101017")]), &ansi);
    assert_eq!(
        coloured.colour(Role::Background),
        Color::Indexed(ansi256((0x10, 0x10, 0x17), GREY))
    );
    // A terminal-default page stays the default.
    let dark = look(ThemeSetting::Dark, &ansi);
    assert_eq!(dark.colour(Role::Background), Color::Reset);
    let terminal = look(file("dark", &[("accent", "#ffffff")]), &ansi);
    assert_eq!(terminal.colour(Role::Background), Color::Reset);
}

#[test]
fn muted_never_vanishes_on_a_white_page() {
    // `muted` is the text colour drawn dim in both columns: on a white
    // page a white terminal foreground would make Reset-muted text
    // vanish, so it resolves through the theme's text shade with DIM
    // kept (`docs/tui.md`, "Themes").
    let truecolour = [("COLORTERM", "truecolor")];
    let ansi = [("TERM", "xterm-256color")];
    let light = look(ThemeSetting::Light, &truecolour);
    assert_eq!(light.colour(Role::Muted), Color::Rgb(0x1f, 0x23, 0x28));
    assert!(light.dim(Role::Muted));
    let light_256 = look(ThemeSetting::Light, &ansi);
    assert_eq!(
        light_256.colour(Role::Muted),
        Color::Indexed(ansi256((0x1f, 0x23, 0x28), 16..=255))
    );
    assert!(light_256.dim(Role::Muted));
    assert_ne!(light_256.colour(Role::Muted), Color::Reset);
    // The dark theme's text is the terminal's own foreground, so dark
    // muted stays pinned to the default with DIM.
    for vars in [&truecolour[..], &ansi[..]] {
        let dark = look(ThemeSetting::Dark, vars);
        assert_eq!(dark.colour(Role::Muted), Color::Reset);
        assert!(dark.dim(Role::Muted));
    }
}

#[test]
fn a_rendered_light_frame_keeps_the_page_distinct() {
    // Every surfaced role paints distinguishable from the page it sits
    // on (`docs/tui.md`, "Themes").
    let look = look(ThemeSetting::Light, &[("TERM", "xterm-256color")]);
    let tints: Vec<Role> = Role::ALL
        .into_iter()
        .filter(|role| role.tint() && *role != Role::Background)
        .collect();
    let width = u16::try_from(tints.len()).expect("a width");
    let mut buf = Buffer::empty(Rect::new(0, 0, width, 1));
    for (x, role) in (0..).zip(&tints) {
        buf.set_string(x, 0, "x", Style::new().fg(role.color()));
    }
    look.paint(&mut buf);
    assert_eq!(look.colour(Role::Background), Color::Indexed(231));
    for (x, role) in (0..).zip(&tints) {
        assert_ne!(buf[(x, 0)].fg, Color::Indexed(231), "{}", role.name());
    }
}

#[test]
fn no_color_paint_keeps_box_cells_and_their_modifiers() {
    // Box glyphs carry no role marker: the paint pass keeps their
    // symbols, default colours and modifiers (`docs/tui.md`, "Look").
    let look = look(ThemeSetting::Dark, &[("NO_COLOR", "1")]);
    let mut buf = row(&[
        ("┌", Style::new().add_modifier(Modifier::DIM)),
        ("─", Style::new().add_modifier(Modifier::DIM)),
        (
            "│",
            Style::new().add_modifier(Modifier::DIM | Modifier::BOLD),
        ),
        ("x", Style::new().fg(Role::Text.color())),
    ]);
    look.paint(&mut buf);
    let symbols: Vec<&str> = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert_eq!(symbols, ["┌", "─", "│", "x"]);
    for x in 0..4 {
        assert_eq!(
            (buf.content[x].fg, buf.content[x].bg),
            (Color::Reset, Color::Reset)
        );
    }
    assert_eq!(buf.content[2].modifier, Modifier::DIM | Modifier::BOLD);
}

#[test]
fn placeholder_style_carries_the_id() {
    use super::placeholder_style;
    assert_eq!(placeholder_style(7), Style::new().fg(Color::Indexed(7)));
    // Past 255 saturates: ids never truncate into another image.
    assert_eq!(
        placeholder_style(300),
        Style::new().fg(Color::Indexed(u8::MAX))
    );
}

/// A placeholder cell with `id` in its foreground: what the inline grid
/// draws (`docs/tui.md`, "Images").
fn placeholder(id: u8) -> (&'static str, Style) {
    (
        "\u{10EEEE}\u{305}\u{305}",
        Style::new().fg(Color::Indexed(id)),
    )
}

#[test]
fn paint_keeps_placeholder_colours_without_colour() {
    let look = look(ThemeSetting::Dark, &[("NO_COLOR", "1")]);
    let mut buf = row(&[placeholder(1), ("a", Style::new().fg(Role::Accent.color()))]);
    look.paint(&mut buf);
    // The id-encoding foreground survives, while the role marker
    // resolves away.
    assert_eq!(buf[(0, 0)].fg, Color::Indexed(1));
    assert_ne!(buf[(1, 0)].fg, Role::Accent.color());
    assert!(!buf[(0, 0)].modifier.contains(Modifier::DIM));
}

#[test]
fn paint_keeps_placeholder_colours_at_256() {
    let look = look(ThemeSetting::Dark, &[("TERM", "xterm-256color")]);
    let mut buf = row(&[placeholder(1), ("a", Style::new().fg(Role::Accent.color()))]);
    look.paint(&mut buf);
    // No quantizing the id to the nearest text entry, while the accent
    // takes its palette entry.
    assert_eq!(buf[(0, 0)].fg, Color::Indexed(1));
    assert_eq!(buf[(1, 0)].fg, Color::Indexed(75));
    assert!(!buf[(0, 0)].modifier.contains(Modifier::DIM));
}

#[test]
fn paint_keeps_placeholder_colours_in_truecolour() {
    let look = look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")]);
    let mut buf = row(&[placeholder(7), ("a", Style::new().fg(Role::Accent.color()))]);
    look.paint(&mut buf);
    assert_eq!(buf[(0, 0)].fg, Color::Indexed(7));
    assert_eq!(buf[(1, 0)].fg, Color::Rgb(0x6e, 0xaa, 0xfe));
}

// The logo tint: the wave in the accent, the name stops in heading,
// accent, string, type and keyword, over the page's background
// (`docs/tui.md`, "The logo").
#[test]
fn logo_tint_reads_the_roles_over_the_page_background() {
    let (tint, background) = look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")])
        .logo_tint()
        .expect("dark truecolour tints the logo");
    assert_eq!(tint.wave, (0x6e, 0xaa, 0xfe));
    assert_eq!(
        tint.stops,
        [
            (0xff, 0x9f, 0x43),
            (0x6e, 0xaa, 0xfe),
            (0xce, 0x91, 0x78),
            (0x7d, 0xd3, 0xfc),
            (0x6e, 0xaa, 0xfe),
        ]
    );
    // The dark background is the terminal's: the reported dark
    // appearance backs the image with black.
    assert_eq!(background, (0, 0, 0));
    let (tint, background) = look(ThemeSetting::Light, &[("COLORTERM", "truecolor")])
        .logo_tint()
        .expect("light truecolour tints the logo");
    assert_eq!(tint.wave, (0x09, 0x69, 0xda));
    assert_eq!(background, (0xff, 0xff, 0xff));
}

// 256 colours keep their RGB: the tint reads shades, never the
// indexed paint.
#[test]
fn logo_tint_keeps_rgb_at_256_colours() {
    let (tint, background) = look(ThemeSetting::Dark, &[("TERM", "xterm-256color")])
        .logo_tint()
        .expect("dark 256-colour tints the logo");
    assert_eq!(tint.wave, (0x6e, 0xaa, 0xfe));
    assert_eq!(background, (0, 0, 0));
}

// Without colour there is no tint: the pixel logo stays.
#[test]
fn logo_tint_is_none_without_colour() {
    assert_eq!(
        look(ThemeSetting::Dark, &[("NO_COLOR", "1")]).logo_tint(),
        None
    );
    assert_eq!(
        look(ThemeSetting::Follow, &[("NO_COLOR", "1")]).logo_tint(),
        None
    );
}

// Following the terminal re-tints on its reported appearance: dark
// backs black, light backs white.
#[test]
fn logo_tint_follows_the_reported_appearance() {
    let mut followed = look(ThemeSetting::Follow, &[("COLORTERM", "truecolor")]);
    assert_eq!(
        followed.logo_tint().map(|(_, background)| background),
        Some((0, 0, 0))
    );
    assert!(followed.appearance(crate::look::Appearance::Light));
    assert_eq!(
        followed.logo_tint().map(|(_, background)| background),
        Some((0xff, 0xff, 0xff))
    );
}
