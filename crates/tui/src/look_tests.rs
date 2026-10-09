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
        (Role::Added, 75),
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
    let vars = [("TERM", "xterm-256color")];
    for (setting, theme) in [
        (ThemeSetting::Dark, Theme::DARK),
        (ThemeSetting::Light, Theme::LIGHT),
    ] {
        let look = look(setting, &vars);
        for role in Role::ALL {
            let shade = theme.shade(role);
            if matches!(shade, Shade::Terminal | Shade::Dim) {
                assert_eq!(look.colour(role), Color::Reset, "{}", role.name());
            } else {
                let among = if role.grey() {
                    GREY
                } else if role.tint() {
                    CUBE
                } else {
                    ALL
                };
                let Shade::Rgb(rgb) = shade else {
                    panic!("{}: {shade:?}", role.name());
                };
                let index = at_256(&look, role);
                assert_eq!(index, ansi256(rgb, among), "{}", role.name());
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
        for role in Role::ALL.into_iter().filter(|role| role.grey()) {
            if matches!(theme.shade(role), Shade::Terminal | Shade::Dim) {
                assert_eq!(look.colour(role), Color::Reset, "{}", role.name());
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
fn paint_leaves_reset_and_other_colours() {
    let look = Look::default();
    let mut buf = row(&[
        ("a", Style::new().fg(Color::Reset).bg(Color::Reset)),
        (
            "b",
            Style::new().fg(Color::Indexed(37)).bg(Color::Rgb(1, 2, 3)),
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
        ("▀", Style::new().fg(Color::Indexed(37))),
        ("x", Style::new().fg(Role::Surface.color())),
    ]);
    look.paint(&mut buf);
    let symbols: Vec<&str> = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert_eq!(symbols, [" ", " ", "▄", "▀", "x"]);
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
        Color::Rgb(0xfa, 0xfa, 0xfa)
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
        Color::Rgb(0xfa, 0xfa, 0xfa)
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
