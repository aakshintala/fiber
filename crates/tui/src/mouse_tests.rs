//! Tests for hit-testing and the pointer.

use super::{Pointer, Target, TargetId, hit};
use crate::keys::{Button, Mouse, MouseKind};
use ratatui::layout::Rect;

/// The badge at columns 2..6 of rows 3..5, and "new below" over its
/// last column of row 4, drawn after it.
fn targets() -> [Target; 2] {
    [
        Target {
            id: TargetId::Badge,
            rect: Rect::new(2, 3, 4, 2),
        },
        Target {
            id: TargetId::NewBelow,
            rect: Rect::new(5, 4, 3, 1),
        },
    ]
}

#[test]
fn hit_takes_first_and_last_cells_and_nothing_past_them() {
    let targets = targets();
    assert_eq!(hit(&targets, 2, 3), Some(TargetId::Badge));
    assert_eq!(hit(&targets, 5, 3), Some(TargetId::Badge));
    assert_eq!(hit(&targets, 2, 4), Some(TargetId::Badge));
    assert_eq!(hit(&targets, 1, 3), None);
    assert_eq!(hit(&targets, 6, 3), None);
    assert_eq!(hit(&targets, 2, 2), None);
    assert_eq!(hit(&targets, 2, 5), None);
    assert_eq!(hit(&targets, 7, 4), Some(TargetId::NewBelow));
    assert_eq!(hit(&targets, 8, 4), None);
    assert_eq!(hit(&[], 0, 0), None);
}

#[test]
fn hit_prefers_the_target_drawn_last() {
    let targets = targets();
    assert_eq!(hit(&targets, 5, 4), Some(TargetId::NewBelow));
    let reversed = [targets[1], targets[0]];
    assert_eq!(hit(&reversed, 5, 4), Some(TargetId::Badge));
}

/// One report at `col`, `row`.
fn at(kind: MouseKind, col: u16, row: u16) -> Mouse {
    Mouse { kind, col, row }
}

const LEFT: MouseKind = MouseKind::Press(Button::Left);

#[test]
fn a_press_and_release_on_one_target_is_a_click() {
    let targets = targets();
    let mut pointer = Pointer::default();
    assert_eq!(pointer.on_mouse(&at(LEFT, 2, 3), &targets, true), None);
    // The release may land on another cell of the same target.
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 4, 4), &targets, true),
        Some(TargetId::Badge)
    );
    // The click is spent: a second release is none.
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 4, 4), &targets, true),
        None
    );
}

#[test]
fn a_release_elsewhere_or_without_a_press_is_no_click() {
    let targets = targets();
    let mut pointer = Pointer::default();
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 2, 3), &targets, true),
        None
    );
    pointer.on_mouse(&at(LEFT, 2, 3), &targets, true);
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 7, 4), &targets, true),
        None
    );
    pointer.on_mouse(&at(LEFT, 2, 3), &targets, true);
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 0, 0), &targets, true),
        None
    );
    // A press on no target, released on one, is none.
    pointer.on_mouse(&at(LEFT, 0, 0), &targets, true);
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 2, 3), &targets, true),
        None
    );
}

#[test]
fn other_buttons_never_click() {
    let targets = targets();
    let mut pointer = Pointer::default();
    for button in [Button::Middle, Button::Right] {
        pointer.on_mouse(&at(MouseKind::Press(button), 2, 3), &targets, true);
        assert_eq!(
            pointer.on_mouse(&at(MouseKind::Release, 2, 3), &targets, true),
            None
        );
    }
    // A right press after a left press disarms it.
    pointer.on_mouse(&at(LEFT, 2, 3), &targets, true);
    pointer.on_mouse(&at(MouseKind::Press(Button::Right), 2, 3), &targets, true);
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 2, 3), &targets, true),
        None
    );
}

#[test]
fn motion_drag_and_wheel_keep_a_press_and_click_nothing() {
    let targets = targets();
    let mut pointer = Pointer::default();
    pointer.on_mouse(&at(LEFT, 2, 3), &targets, true);
    for kind in [
        MouseKind::Motion,
        MouseKind::Drag(Button::Left),
        MouseKind::WheelUp,
        MouseKind::WheelDown,
    ] {
        assert_eq!(pointer.on_mouse(&at(kind, 3, 3), &targets, true), None);
    }
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 3, 3), &targets, true),
        Some(TargetId::Badge)
    );
}

#[test]
fn every_report_moves_the_pointer_only_with_hover() {
    let targets = targets();
    let mut pointer = Pointer::default();
    pointer.on_mouse(&at(MouseKind::Motion, 4, 1), &targets, true);
    assert_eq!(pointer.at, Some((4, 1)));
    pointer.on_mouse(&at(MouseKind::Drag(Button::Left), 6, 2), &targets, true);
    assert_eq!(pointer.at, Some((6, 2)));
    let mut off = Pointer::default();
    off.on_mouse(&at(MouseKind::Motion, 4, 1), &targets, false);
    off.on_mouse(&at(LEFT, 2, 3), &targets, false);
    assert_eq!(off.at, None);
    // Clicks still work with hover off.
    assert_eq!(
        off.on_mouse(&at(MouseKind::Release, 2, 3), &targets, false),
        Some(TargetId::Badge)
    );
}

#[test]
fn under_passes_over_a_turn() {
    let targets = vec![
        Target {
            id: TargetId::Turn(0),
            rect: Rect::new(0, 0, 60, 5),
        },
        Target {
            id: TargetId::Line(crate::app::Target::Group(0)),
            rect: Rect::new(0, 2, 60, 1),
        },
    ];
    assert_eq!(hit(&targets, 3, 1), None);
    assert_eq!(
        hit(&targets, 3, 2),
        Some(TargetId::Line(crate::app::Target::Group(0)))
    );
}
