//! Tests for the window title's bytes.

use super::{Title, title};

#[test]
fn title_wraps_in_osc_2() {
    assert_eq!(
        title("✓ fix · fiber"),
        "\x1b]2;✓ fix · fiber\x07".as_bytes()
    );
}

#[test]
fn controls_and_del_are_dropped() {
    assert_eq!(
        title("a\x1bb\x07c\u{9b}d\u{7f}e\nf\tg"),
        b"\x1b]2;abcdefg\x07".to_vec()
    );
}

#[test]
fn an_unchanged_title_writes_nothing_until_forgotten() {
    let mut last = Title::default();
    assert_eq!(last.next("fiber".to_owned()), Some(title("fiber")));
    assert_eq!(last.next("fiber".to_owned()), None);
    assert_eq!(last.next("x · fiber".to_owned()), Some(title("x · fiber")));
    last.forget();
    assert_eq!(last.next("x · fiber".to_owned()), Some(title("x · fiber")));
}
