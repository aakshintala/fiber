//! Tests for strokes: every name and alias reads, `name` round-trips, and
//! `label` follows the bindings table's style.

use super::{Code, Mods, Stroke};

/// A stroke with no modifiers.
fn plain(code: Code) -> Stroke {
    Stroke {
        code,
        mods: Mods::NONE,
    }
}

/// A stroke with modifiers.
fn modified(code: Code, mods: Mods) -> Stroke {
    Stroke { code, mods }
}

/// Every key name and alias reads to its stroke.
#[test]
fn names_parse() {
    let cases: &[(&str, Stroke)] = &[
        ("enter", plain(Code::Enter)),
        ("return", plain(Code::Enter)),
        ("esc", plain(Code::Esc)),
        ("escape", plain(Code::Esc)),
        ("tab", plain(Code::Tab)),
        ("space", plain(Code::Space)),
        (" ", plain(Code::Space)),
        ("backspace", plain(Code::Backspace)),
        ("delete", plain(Code::Delete)),
        ("insert", plain(Code::Insert)),
        ("home", plain(Code::Home)),
        ("end", plain(Code::End)),
        ("pageup", plain(Code::PageUp)),
        ("pagedown", plain(Code::PageDown)),
        ("up", plain(Code::Up)),
        ("down", plain(Code::Down)),
        ("left", plain(Code::Left)),
        ("right", plain(Code::Right)),
        ("f1", plain(Code::F(1))),
        ("f2", plain(Code::F(2))),
        ("f3", plain(Code::F(3))),
        ("f4", plain(Code::F(4))),
        ("f5", plain(Code::F(5))),
        ("f6", plain(Code::F(6))),
        ("f7", plain(Code::F(7))),
        ("f8", plain(Code::F(8))),
        ("f9", plain(Code::F(9))),
        ("f10", plain(Code::F(10))),
        ("f11", plain(Code::F(11))),
        ("f12", plain(Code::F(12))),
        ("F12", plain(Code::F(12))),
        ("a", plain(Code::Char('a'))),
        ("z", plain(Code::Char('z'))),
        ("0", plain(Code::Char('0'))),
        ("9", plain(Code::Char('9'))),
        ("?", plain(Code::Char('?'))),
        ("+", plain(Code::Char('+'))),
        ("ctrl++", modified(Code::Char('+'), Mods::CTRL)),
        ("f", plain(Code::Char('f'))),
        ("ctrl+t", modified(Code::Char('t'), Mods::CTRL)),
        ("control+t", modified(Code::Char('t'), Mods::CTRL)),
        ("alt+p", modified(Code::Char('p'), Mods::ALT)),
        ("opt+p", modified(Code::Char('p'), Mods::ALT)),
        ("option+p", modified(Code::Char('p'), Mods::ALT)),
        ("meta+p", modified(Code::Char('p'), Mods::ALT)),
        ("super+left", modified(Code::Left, Mods::SUPER)),
        ("cmd+left", modified(Code::Left, Mods::SUPER)),
        ("command+left", modified(Code::Left, Mods::SUPER)),
        ("shift+enter", modified(Code::Enter, Mods::SHIFT)),
        ("shift+return", modified(Code::Enter, Mods::SHIFT)),
        ("SHIFT+ENTER", modified(Code::Enter, Mods::SHIFT)),
        ("Alt+Up", modified(Code::Up, Mods::ALT)),
        (
            "super+ALT+ctrl+shift+x",
            modified(
                Code::Char('x'),
                Mods::CTRL | Mods::SHIFT | Mods::ALT | Mods::SUPER,
            ),
        ),
        // A repeated modifier counts once.
        ("ctrl+ctrl+a", modified(Code::Char('a'), Mods::CTRL)),
        // An uppercase letter reads as shift with that letter, even beside
        // other modifiers.
        ("A", modified(Code::Char('a'), Mods::SHIFT)),
        (
            "CTRL+T",
            modified(Code::Char('t'), Mods::CTRL | Mods::SHIFT),
        ),
        // Shift on a character that is not a letter is dropped.
        ("shift+?", plain(Code::Char('?'))),
        ("Shift+1", plain(Code::Char('1'))),
        ("ctrl+shift+?", modified(Code::Char('?'), Mods::CTRL)),
        ("shift+space", modified(Code::Space, Mods::SHIFT)),
        ("shift+f1", modified(Code::F(1), Mods::SHIFT)),
    ];
    for (text, want) in cases {
        assert_eq!(Stroke::parse(text), Ok(*want), "{text:?}");
    }
}

/// What does not read as a key.
#[test]
fn refusals() {
    for text in [
        "",
        "ctrl+",
        "+a",
        "ctrl+tt",
        "hyper+a",
        "f13",
        "f0",
        "f99",
        "fx",
        "ctrl",
        "shift",
        "alt+",
        "ctrl+shift",
        "enter+ctrl",
        "a++b",
        "++",
        "shift+\x01",
        "\x01",
        "\x7f",
    ] {
        assert!(Stroke::parse(text).is_err(), "{text:?}");
    }
}

/// The refusal names the text.
#[test]
fn refusal_names_the_text() {
    assert_eq!(
        Stroke::parse("ctrl+tt"),
        Err("\"ctrl+tt\" is not a key name".to_owned())
    );
}

/// Every stroke in the names table round-trips through its written form.
#[test]
fn names_round_trip() {
    let cases: &[(&str, Stroke)] = &[
        ("enter", plain(Code::Enter)),
        ("shift+enter", modified(Code::Enter, Mods::SHIFT)),
        ("ctrl+t", modified(Code::Char('t'), Mods::CTRL)),
        ("alt+p", modified(Code::Char('p'), Mods::ALT)),
        ("super+left", modified(Code::Left, Mods::SUPER)),
        ("shift+a", modified(Code::Char('a'), Mods::SHIFT)),
        ("?", plain(Code::Char('?'))),
        ("ctrl++", modified(Code::Char('+'), Mods::CTRL)),
        ("f12", plain(Code::F(12))),
        ("y", plain(Code::Char('y'))),
    ];
    for (written, want) in cases {
        assert_eq!(want.name(), (*written).to_owned(), "{written:?}");
        assert_eq!(Stroke::parse(&want.name()), Ok(*want), "{written:?}");
    }
    // Modifiers write in ctrl, shift, alt, super order whatever order they
    // read in.
    let all = Stroke {
        code: Code::Char('x'),
        mods: Mods::SUPER | Mods::ALT | Mods::SHIFT | Mods::CTRL,
    };
    assert_eq!(all.name(), "ctrl+shift+alt+super+x".to_owned());
    assert_eq!(Stroke::parse(&all.name()), Ok(all));
}

/// Labels follow the bindings table's style.
#[test]
fn labels() {
    let cases: &[(Stroke, &str)] = &[
        (modified(Code::Char('t'), Mods::CTRL), "Ctrl+T"),
        (modified(Code::Char('p'), Mods::ALT), "⌥P"),
        (modified(Code::Left, Mods::SUPER), "⌘←"),
        (modified(Code::Enter, Mods::SHIFT), "Shift+Enter"),
        (plain(Code::F(1)), "F1"),
        (plain(Code::Char('y')), "y"),
        (plain(Code::Enter), "Enter"),
        (plain(Code::Esc), "Esc"),
        (plain(Code::Tab), "Tab"),
        (modified(Code::Tab, Mods::SHIFT), "Shift+Tab"),
        (plain(Code::Backspace), "Backspace"),
        (plain(Code::Delete), "Delete"),
        (plain(Code::Insert), "Insert"),
        (plain(Code::Home), "Home"),
        (plain(Code::End), "End"),
        (plain(Code::PageUp), "PageUp"),
        (plain(Code::PageDown), "PageDown"),
        (plain(Code::Up), "↑"),
        (plain(Code::Down), "↓"),
        (plain(Code::Left), "←"),
        (plain(Code::Right), "→"),
        (plain(Code::Space), "Space"),
        (plain(Code::F(12)), "F12"),
        (modified(Code::Char('a'), Mods::SHIFT), "A"),
        (modified(Code::Space, Mods::CTRL), "Ctrl+Space"),
        (modified(Code::Backspace, Mods::ALT), "⌥Backspace"),
        (modified(Code::Left, Mods::CTRL), "Ctrl+←"),
        (modified(Code::Left, Mods::ALT), "⌥←"),
        (modified(Code::Char('c'), Mods::CTRL), "Ctrl+C"),
        (modified(Code::Up, Mods::ALT), "⌥↑"),
        (modified(Code::Right, Mods::SUPER), "⌘→"),
        (modified(Code::Char('1'), Mods::ALT), "⌥1"),
        (modified(Code::Char('?'), Mods::CTRL), "Ctrl+?"),
        (
            modified(Code::Char('t'), Mods::CTRL | Mods::SHIFT),
            "Ctrl+Shift+T",
        ),
        (
            modified(Code::Char('x'), Mods::CTRL | Mods::ALT | Mods::SUPER),
            "Ctrl+⌥⌘X",
        ),
    ];
    for (stroke, want) in cases {
        assert_eq!(stroke.label(), (*want).to_owned(), "{stroke:?}");
    }
}
