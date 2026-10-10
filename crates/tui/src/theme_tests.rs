//! Tests for the colour roles and the built-in themes.

use super::{ROLES, Role, Shade, Theme};

#[test]
fn all_lists_every_role_once_in_index_order() {
    for (index, role) in Role::ALL.into_iter().enumerate() {
        let index = u8::try_from(index).expect("a role index");
        assert_eq!(Role::from_index(index), Some(role));
        assert_eq!(role.color(), ratatui::style::Color::Indexed(index));
    }
    assert_eq!(Role::from_index(36), Some(Role::MatchCurrent));
    assert_eq!(Role::from_index(37), None);
    assert_eq!(Role::from_index(255), None);
}

#[test]
fn names_round_trip() {
    let names: Vec<&str> = Role::ALL.into_iter().map(Role::name).collect();
    let mut unique = names.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ROLES, "{names:?}");
    for role in Role::ALL {
        let text = format!(
            r##"{{"base": "dark", "roles": {{"{}": "#010203"}}}}"##,
            role.name()
        );
        let theme = Theme::parse(&text).unwrap_or_else(|error| panic!("{text}: {error}"));
        assert_eq!(theme.shade(role), Shade::Rgb((1, 2, 3)), "{}", role.name());
    }
    assert_eq!(Role::CodeText.name(), "code_text");
    assert_eq!(Role::SurfaceRaised.name(), "surface_raised");
    assert_eq!(Role::MatchCurrent.name(), "match_current");
    assert_eq!(Role::Info.name(), "info");
    assert_eq!(Role::Secondary.name(), "secondary");
    assert_eq!(Role::Rule.name(), "rule");
    assert_eq!(Role::Scroll.name(), "scroll");
    assert_eq!(Role::Panel.name(), "panel");
    assert_eq!(Role::Turn.name(), "turn");
    assert_eq!(Role::Handoff.name(), "handoff");
}

/// A theme file, the built-in it names as `base`, and the roles it sets.
type Case = (&'static str, Theme, &'static [(Role, Shade)]);

#[test]
fn parse_accepts_base_and_roles() {
    let cases: [Case; 4] = [
        (r#"{"base": "dark"}"#, Theme::DARK, &[]),
        (r#"{"base": "light", "roles": {}}"#, Theme::LIGHT, &[]),
        (
            r##"{"base": "light", "roles": {"accent": "#0b7285", "alert": "#5f1e22"}}"##,
            Theme::LIGHT,
            &[
                (Role::Accent, Shade::Rgb((0x0b, 0x72, 0x85))),
                (Role::Alert, Shade::Rgb((0x5f, 0x1e, 0x22))),
            ],
        ),
        (
            r##"{"roles": {"text": "#ffffff"}, "base": "dark"}"##,
            Theme::DARK,
            &[(Role::Text, Shade::Rgb((255, 255, 255)))],
        ),
    ];
    for (text, base, set) in cases {
        let theme = Theme::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"));
        for role in Role::ALL {
            let want = set
                .iter()
                .find(|(named, _)| *named == role)
                .map_or_else(|| base.shade(role), |(_, shade)| *shade);
            assert_eq!(theme.shade(role), want, "{text}: {}", role.name());
        }
    }
}

#[test]
fn parse_fills_missing_roles_from_base() {
    for (base, built_in) in [("dark", Theme::DARK), ("light", Theme::LIGHT)] {
        let text = format!(r##"{{"base": "{base}", "roles": {{"muted": "#123456"}}}}"##);
        let theme = Theme::parse(&text).unwrap_or_else(|error| panic!("{text}: {error}"));
        for role in Role::ALL.into_iter().filter(|role| *role != Role::Muted) {
            assert_eq!(
                theme.shade(role),
                built_in.shade(role),
                "{base}: {}",
                role.name()
            );
        }
        assert_eq!(theme.shade(Role::Muted), Shade::Rgb((0x12, 0x34, 0x56)));
    }
}

#[test]
fn parse_refuses() {
    let cases = [
        ("{", "not JSON: "),
        ("[]", "not a JSON object"),
        (r#"{"roles": {}}"#, "no \"base\""),
        (
            r#"{"base": "blue"}"#,
            "\"base\" is \"blue\", not \"dark\" or \"light\"",
        ),
        (r#"{"base": 1}"#, "\"base\" is 1, not \"dark\" or \"light\""),
        (
            r#"{"base": "dark", "roles": []}"#,
            "\"roles\" is not an object",
        ),
        (
            r##"{"base": "dark", "roles": {"acent": "#56b6c2"}}"##,
            "unknown role \"acent\"",
        ),
        (
            r##"{"base": "dark", "roles": {"accent": "#abc"}}"##,
            "role \"accent\": \"#abc\" is not #rrggbb",
        ),
        (
            r#"{"base": "dark", "roles": {"accent": "56b6c2"}}"#,
            "role \"accent\": \"56b6c2\" is not #rrggbb",
        ),
        (
            r##"{"base": "dark", "roles": {"accent": "#56b6cg"}}"##,
            "role \"accent\": \"#56b6cg\" is not #rrggbb",
        ),
        (
            r##"{"base": "dark", "roles": {"accent": "#+5b6c2"}}"##,
            "role \"accent\": \"#+5b6c2\" is not #rrggbb",
        ),
        (
            r##"{"base": "dark", "roles": {"accent": "#56b6c2ff"}}"##,
            "role \"accent\": \"#56b6c2ff\" is not #rrggbb",
        ),
        (
            r#"{"base": "dark", "roles": {"accent": 1}}"#,
            "role \"accent\": 1 is not #rrggbb",
        ),
        (
            r#"{"base": "dark", "name": "solar"}"#,
            "unknown key \"name\"",
        ),
    ];
    for (text, reason) in cases {
        let error = Theme::parse(text).expect_err(text);
        assert!(error.starts_with(reason), "{text}: {error}");
        if !reason.ends_with(": ") {
            assert_eq!(error, reason, "{text}");
        }
    }
}

#[test]
fn parse_accepts_upper_case_hex() {
    let theme = Theme::parse(r##"{"base": "dark", "roles": {"accent": "#0B72Af"}}"##)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(theme.shade(Role::Accent), Shade::Rgb((0x0b, 0x72, 0xaf)));
}

#[test]
fn tint_and_grey_per_role() {
    // (role, a background role, a neutral tint on the grey ramp)
    let table = [
        (Role::Text, false, false),
        (Role::Muted, false, false),
        (Role::Accent, false, false),
        (Role::Heading, false, false),
        (Role::Success, false, false),
        (Role::Warning, false, false),
        (Role::Error, false, false),
        (Role::Attention, false, false),
        (Role::Added, false, false),
        (Role::Removed, false, false),
        (Role::CodeText, false, false),
        (Role::Keyword, false, false),
        (Role::String, false, false),
        (Role::Comment, false, false),
        (Role::Number, false, false),
        (Role::Function, false, false),
        (Role::Type, false, false),
        (Role::Constant, false, false),
        (Role::Operator, false, false),
        (Role::Info, false, false),
        (Role::Secondary, false, false),
        (Role::Rule, false, true),
        (Role::Scroll, false, false),
        (Role::Background, true, true),
        (Role::Panel, true, true),
        (Role::Surface, true, true),
        (Role::SurfaceRaised, true, true),
        (Role::Turn, true, true),
        (Role::Prompt, true, true),
        (Role::Code, true, true),
        (Role::Handoff, true, false),
        (Role::Approval, true, true),
        (Role::Alert, true, false),
        (Role::Hover, true, true),
        (Role::Selection, true, false),
        (Role::Match, true, false),
        (Role::MatchCurrent, true, false),
    ];
    assert_eq!(table.map(|(role, _, _)| role), Role::ALL);
    for (role, tint, grey) in table {
        assert_eq!(role.tint(), tint, "{}", role.name());
        assert_eq!(role.grey(), grey, "{}", role.name());
    }
}

#[test]
fn dark_and_light_differ_in_every_background_role() {
    // `panel`, `turn` and `handoff` are new in the doc table and take the
    // dark value in the light theme until ruled (see #1634's case).
    for role in Role::ALL.into_iter().filter(|role| {
        role.tint() && *role != Role::Panel && *role != Role::Turn && *role != Role::Handoff
    }) {
        assert_ne!(
            Theme::DARK.shade(role),
            Theme::LIGHT.shade(role),
            "{}",
            role.name()
        );
    }
}

#[test]
fn the_alert_tint_is_red_in_both_built_ins() {
    for theme in [Theme::DARK, Theme::LIGHT] {
        let Shade::Rgb((r, g, b)) = theme.shade(Role::Alert) else {
            panic!("alert is {:?}", theme.shade(Role::Alert));
        };
        assert!(r > g && r > b, "{:?}", (r, g, b));
    }
}

/// Whether `line`, outside a `//` comment, names `Color` as a word.
fn names_color(line: &str) -> bool {
    let code = line.split("//").next().unwrap_or_default();
    code.match_indices("Color").any(|(at, _)| {
        let word =
            |byte: Option<&u8>| byte.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
        let before = at
            .checked_sub(1)
            .and_then(|before| code.as_bytes().get(before));
        let after = code.as_bytes().get(at + "Color".len());
        !word(before) && !word(after)
    })
}

#[test]
fn names_color_finds_the_type_and_skips_comments_and_longer_words() {
    assert!(names_color("use ratatui::style::{Color, Style};"));
    assert!(names_color("    Color,"));
    assert!(names_color("let x = Color::Reset;"));
    assert!(!names_color("// a Color in a comment"));
    assert!(!names_color("let colour = ColorTerm;"));
    assert!(!names_color("let x = MyColor;"));
    assert!(!names_color("Role::Accent.color()"));
}

/// Every `.rs` file under `dir`, recursively.
fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|err| panic!("{}: {err}", dir.display())) {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_colour_outside_the_theme() {
    // Drawing code names roles, never colours (`docs/tui.md`, "Themes"):
    // only the theme and the look hold a `Color`.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(files.len() > 50, "{}", files.len());
    let mut found = Vec::new();
    for path in files {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if ["theme.rs", "look.rs"].contains(&name) || name.ends_with("_tests.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{err}"));
        for (at, line) in text.lines().enumerate() {
            if names_color(line) {
                found.push(format!("{}:{}: {line}", path.display(), at + 1));
            }
        }
    }
    assert!(found.is_empty(), "{found:#?}");
}

/// The `index`-th `|`-separated column of the first table under
/// `### Themes` in `doc`, trimmed.
fn doc_column(doc: &str, index: usize) -> Vec<String> {
    doc.lines()
        .skip_while(|line| *line != "### Themes")
        .skip(1)
        .take_while(|line| !line.starts_with('#'))
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .skip(2)
        .filter_map(|line| line.split('|').nth(index))
        .map(|cell| cell.trim().to_owned())
        .collect()
}

/// The first column of the first table under `### Themes` in `doc`, each
/// cell with its backticks taken off.
fn doc_roles(doc: &str) -> Vec<String> {
    doc_column(doc, 1)
        .into_iter()
        .map(|cell| cell.trim_matches('`').to_owned())
        .collect()
}

#[test]
fn doc_roles_reads_the_first_column_of_the_themes_table() {
    let doc = "## Look\n\n| a | b |\n|---|---|\n| `x` | no |\n\n### Themes\n\nText.\n\n\
               | Role | Use |\n|---|---|\n| `text` | words |\n| `muted` | dim |\n\nAfter.\n\n\
               | `later` | no |\n\n### Next\n";
    assert_eq!(doc_roles(doc), ["text", "muted"]);
}

#[test]
fn the_doc_lists_every_role() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/tui.md");
    let doc =
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let names: Vec<&str> = Role::ALL.into_iter().map(Role::name).collect();
    assert_eq!(doc_roles(&doc), names);
}

/// The third column of the first table under `### Themes` in `doc`, one
/// cell per role, in order, backticks kept.
fn doc_dark(doc: &str) -> Vec<String> {
    doc_column(doc, 3)
}

#[test]
fn doc_dark_reads_the_third_column_of_the_themes_table() {
    let doc = "## Look\n\n| a | b | c |\n|---|---|---|\n| `x` | use | `1` |\n\n### Themes\n\nText.\n\n\
               | Role | Use | Dark |\n|---|---|---|\n| `text` | words | the terminal's foreground |\n| `muted` | dim | `text`, dim |\n\nAfter.\n\n\
               | `later` | no | `2` |\n\n### Next\n";
    assert_eq!(doc_dark(doc), ["the terminal's foreground", "`text`, dim"]);
}

/// The dark theme's shade the doc's dark cell `cell` names for `role`.
fn dark_cell(cell: &str, role: Role) -> Shade {
    match cell {
        "the terminal's foreground" | "the terminal's background" => Shade::Terminal,
        "`text`, dim" => Shade::Dim,
        hex => {
            let text = hex.strip_prefix('`').and_then(|hex| hex.strip_suffix('`'));
            text.and_then(super::hex)
                .map(|(r, g, b)| Shade::Rgb((r, g, b)))
                .unwrap_or_else(|| panic!("{}: unknown dark cell {cell:?}", role.name()))
        }
    }
}

#[test]
fn the_dark_theme_is_the_doc_table() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/tui.md");
    let doc =
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let names: Vec<&str> = Role::ALL.into_iter().map(Role::name).collect();
    assert_eq!(doc_roles(&doc), names);
    let dark = doc_dark(&doc);
    assert_eq!(dark.len(), ROLES, "{dark:?}");
    for (role, cell) in Role::ALL.into_iter().zip(dark.iter()) {
        assert_eq!(
            Theme::DARK.shade(role),
            dark_cell(cell, role),
            "{}",
            role.name()
        );
    }
}

#[test]
fn the_light_theme_takes_the_dark_value_for_unruled_roles() {
    for role in [
        Role::Info,
        Role::Secondary,
        Role::Rule,
        Role::Scroll,
        Role::Panel,
        Role::Turn,
        Role::Handoff,
    ] {
        assert_eq!(
            Theme::LIGHT.shade(role),
            Theme::DARK.shade(role),
            "{}",
            role.name()
        );
    }
    assert_eq!(
        Theme::LIGHT.shade(Role::Text),
        Shade::Rgb((0x38, 0x3a, 0x42))
    );
    assert_eq!(
        Theme::LIGHT.shade(Role::Background),
        Shade::Rgb((0xfa, 0xfa, 0xfa))
    );
    assert_eq!(
        Theme::LIGHT.shade(Role::Muted),
        Shade::Rgb((0xa0, 0xa1, 0xa7))
    );
}
