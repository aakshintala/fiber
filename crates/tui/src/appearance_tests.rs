//! Tests for the appearance queries and replies: OSC 11 held, read or left
//! as keys, and the theme report (`docs/tui.md`, "Themes").

use super::{Osc, osc11, scheme};
use crate::keys::{Event, Reply};
use crate::look::Appearance;

/// The appearance `scheme` reads from `params`, or nothing.
fn reported(params: &[u8]) -> Option<Appearance> {
    match scheme(params).as_slice() {
        [Event::Reply(Reply::Appearance(appearance))] => Some(*appearance),
        _ => None,
    }
}

#[test]
fn scheme_reports() {
    assert_eq!(reported(b"?997;1"), Some(Appearance::Dark));
    assert_eq!(reported(b"?997;2"), Some(Appearance::Light));
    for params in [&b"?997;3"[..], b"?997", b"?997;12", b"?996;1", b"997;1"] {
        assert!(scheme(params).is_empty(), "{params:?}");
    }
}

/// An OSC 11 reply: its bytes, and the appearance it reports.
fn reply(colour: &str, end: &str) -> Vec<u8> {
    let mut bytes = vec![0x1b, b']'];
    bytes.extend_from_slice(b"11;rgb:");
    bytes.extend_from_slice(colour.as_bytes());
    bytes.extend_from_slice(end.as_bytes());
    bytes
}

#[test]
fn osc_11_replies() {
    let cases: &[(&str, Appearance)] = &[
        ("0/0/0", Appearance::Dark),
        ("ff/ff/ff", Appearance::Light),
        ("ffff/ffff/ffff", Appearance::Light),
        ("fff/fff/fff", Appearance::Light),
        ("1e1e/1e1e/2e2e", Appearance::Dark),
        ("fafa/fafa/fafa", Appearance::Light),
        ("8/8/8", Appearance::Light),
        ("7/7/7", Appearance::Dark),
        ("7f/7f/7f", Appearance::Dark),
        // Exactly mid grey: the luma is not below it, so light.
        ("80/80/80", Appearance::Light),
        ("7f7f/7f7f/7f7f", Appearance::Dark),
        ("8080/8080/8080", Appearance::Light),
        // The luma weights the channels: red alone is dark, green light.
        ("ff/0/0", Appearance::Dark),
        ("0/ff/0", Appearance::Light),
    ];
    for (at, (colour, appearance)) in cases.iter().enumerate() {
        // BEL ends the even rows, `ESC \` the odd ones.
        let end = if at % 2 == 0 { "\x07" } else { "\x1b\\" };
        let bytes = reply(colour, end);
        let Osc::Done(events, used) = osc11(&bytes) else {
            panic!("{colour:?}: no reply");
        };
        assert_eq!(used, bytes.len(), "{colour:?}");
        assert_eq!(events, vec![Event::Reply(Reply::Appearance(*appearance))]);
    }
}

#[test]
fn osc_holds_an_incomplete_reply() {
    for prefix in [
        "\x1b]",
        "\x1b]1",
        "\x1b]11;",
        "\x1b]11;rgb:ff",
        "\x1b]11;rgb:ff/ff/ffff",
        "\x1b]11;rgb:ff/ff/ff\x1b",
    ] {
        assert!(matches!(osc11(prefix.as_bytes()), Osc::Hold), "{prefix:?}");
    }
}

#[test]
fn osc_leaves_at_the_first_byte_off_the_grammar() {
    // One row per grammar step: the byte that ends the candidate.
    for off in [
        "\x1b]2",
        "\x1b]1x",
        "\x1b]11x",
        "\x1b]11;h",
        "\x1b]11;rgba:",
        "\x1b]11;rgb;",
        "\x1b]11;rgb:/",
        "\x1b]11;rgb:fffff",
        "\x1b]11;rgb:ff:",
        "\x1b]11;rgb:ff/ff/ff x",
        "\x1b]11;rgb:ff/ff/ff\x1bx",
        "\x1b]11;rgb:zz/0/0",
        "\x1b]11;#ffffff",
    ] {
        assert!(matches!(osc11(off.as_bytes()), Osc::Off), "{off:?}");
    }
}

#[test]
fn scale_takes_one_to_four_hex_digits() {
    assert_eq!(super::scale(b""), None);
    assert_eq!(super::scale(b"f"), Some(255));
    assert_eq!(super::scale(b"ffff"), Some(255));
    // Past the clamp: five digits, and two past it, are no field.
    assert_eq!(super::scale(b"fffff"), None);
    assert_eq!(super::scale(b"ffffff"), None);
    assert_eq!(super::scale(b"zz"), None);
}
