//! Tests for the colour roles and the built-in themes.

use super::{ROLES, Role, Theme};

#[test]
fn all_lists_every_role_once_in_index_order() {
    for (index, role) in Role::ALL.into_iter().enumerate() {
        let index = u8::try_from(index).expect("a role index");
        assert_eq!(Role::from_index(index), Some(role));
        assert_eq!(role.color(), ratatui::style::Color::Indexed(index));
    }
    assert_eq!(Role::from_index(30), None);
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
        assert_eq!(theme.rgb(role), (1, 2, 3), "{}", role.name());
    }
    assert_eq!(Role::CodeText.name(), "code_text");
    assert_eq!(Role::SurfaceRaised.name(), "surface_raised");
    assert_eq!(Role::MatchCurrent.name(), "match_current");
}

/// A theme file, the built-in it names as `base`, and the roles it sets.
type Case = (&'static str, Theme, &'static [(Role, (u8, u8, u8))]);

#[test]
fn parse_accepts_base_and_roles() {
    let cases: [Case; 4] = [
        (r#"{"base": "dark"}"#, Theme::DARK, &[]),
        (r#"{"base": "light", "roles": {}}"#, Theme::LIGHT, &[]),
        (
            r##"{"base": "light", "roles": {"accent": "#0b7285", "alert": "#5f1e22"}}"##,
            Theme::LIGHT,
            &[
                (Role::Accent, (0x0b, 0x72, 0x85)),
                (Role::Alert, (0x5f, 0x1e, 0x22)),
            ],
        ),
        (
            r##"{"roles": {"text": "#ffffff"}, "base": "dark"}"##,
            Theme::DARK,
            &[(Role::Text, (255, 255, 255))],
        ),
    ];
    for (text, base, set) in cases {
        let theme = Theme::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"));
        for role in Role::ALL {
            let want = set
                .iter()
                .find(|(named, _)| *named == role)
                .map_or_else(|| base.rgb(role), |(_, rgb)| *rgb);
            assert_eq!(theme.rgb(role), want, "{text}: {}", role.name());
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
                theme.rgb(role),
                built_in.rgb(role),
                "{base}: {}",
                role.name()
            );
        }
        assert_eq!(theme.rgb(Role::Muted), (0x12, 0x34, 0x56));
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
    assert_eq!(theme.rgb(Role::Accent), (0x0b, 0x72, 0xaf));
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
        (Role::Background, true, true),
        (Role::Surface, true, true),
        (Role::SurfaceRaised, true, true),
        (Role::Prompt, true, true),
        (Role::Code, true, true),
        (Role::Approval, true, false),
        (Role::Alert, true, false),
        (Role::Hover, true, true),
        (Role::Selection, true, true),
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
    for role in Role::ALL.into_iter().filter(|role| role.tint()) {
        assert_ne!(
            Theme::DARK.rgb(role),
            Theme::LIGHT.rgb(role),
            "{}",
            role.name()
        );
    }
}

#[test]
fn the_alert_tint_is_red_in_both_built_ins() {
    for theme in [Theme::DARK, Theme::LIGHT] {
        let (r, g, b) = theme.rgb(Role::Alert);
        assert!(r > g && r > b, "{:?}", (r, g, b));
    }
}
