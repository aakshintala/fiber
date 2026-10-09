//! Tests for the window title's bytes.

use super::{Shape, Title, notify, pointer, title};

#[test]
fn notify_wraps_in_osc_9_and_drops_controls() {
    assert_eq!(notify("a\x1bb\x07c"), b"\x1b]9;abc\x07".to_vec());
}

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

#[test]
fn pointer_shapes() {
    assert_eq!(pointer(true), b"\x1b]22;col-resize\x1b\\");
    assert_eq!(pointer(false), b"\x1b]22;default\x1b\\");
}

#[test]
fn shape_writes_only_on_change() {
    let mut shape = Shape::default();
    assert_eq!(shape.next(false), None);
    assert_eq!(shape.next(true), Some(pointer(true)));
    assert_eq!(shape.next(true), None);
    assert_eq!(shape.next(false), Some(pointer(false)));
    assert_eq!(shape.next(false), None);
}

#[test]
fn reset_returns_to_default() {
    let mut shape = Shape::default();
    assert_eq!(shape.next(true), Some(pointer(true)));
    shape.reset();
    assert_eq!(shape.next(true), Some(pointer(true)));
    shape.reset();
    assert_eq!(shape.next(false), None);
}
