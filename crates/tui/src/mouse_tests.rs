//! Tests for hit-testing and the pointer.

use super::{ClickRoute, Pointer, Target, TargetId, click_route, hit};
use crate::ext::cache::{Cache, ClickKey};
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
    for kind in [MouseKind::Motion, MouseKind::WheelUp, MouseKind::WheelDown] {
        assert_eq!(pointer.on_mouse(&at(kind, 3, 3), &targets, true), None);
    }
    // A drag on the press's own cell keeps it.
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Drag(Button::Left), 2, 3), &targets, true),
        None
    );
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 3, 3), &targets, true),
        Some(TargetId::Badge)
    );
}

#[test]
fn a_drag_to_another_cell_disarms_the_click() {
    let targets = targets();
    let mut pointer = Pointer::default();
    pointer.on_mouse(&at(LEFT, 2, 3), &targets, true);
    // Another cell of the same target: still a drag, so no click.
    pointer.on_mouse(&at(MouseKind::Drag(Button::Left), 3, 3), &targets, true);
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 3, 3), &targets, true),
        None
    );
}

#[test]
fn a_drag_that_returns_and_releases_on_its_target_does_not_click() {
    let targets = targets();
    let mut pointer = Pointer::default();
    pointer.on_mouse(&at(LEFT, 2, 3), &targets, true);
    pointer.on_mouse(&at(MouseKind::Drag(Button::Left), 0, 0), &targets, true);
    pointer.on_mouse(&at(MouseKind::Drag(Button::Left), 2, 3), &targets, true);
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 2, 3), &targets, true),
        None
    );
}

#[test]
fn a_drag_within_the_press_cell_still_clicks() {
    let targets = targets();
    let mut pointer = Pointer::default();
    pointer.on_mouse(&at(LEFT, 2, 3), &targets, true);
    pointer.on_mouse(&at(MouseKind::Drag(Button::Left), 2, 3), &targets, true);
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 2, 3), &targets, true),
        Some(TargetId::Badge)
    );
}

/// An interned click key for `id` in session `s`, extension `a`,
/// slot `notice`.
fn key(cache: &Cache, id: &str) -> ClickKey {
    cache.intern_click(&contract::SessionId("s".into()), "a", "notice", id)
}

/// A line target over the row, an extension target on its first four
/// cells, and a link target over its middle, drawn in that order.
fn clicked() -> (Cache, [Target; 3]) {
    let cache = Cache::default();
    let badge = key(&cache, "badge");
    (
        cache,
        [
            Target {
                id: TargetId::Line(crate::app::Target::Group(1)),
                rect: Rect::new(0, 3, 10, 1),
            },
            Target {
                id: TargetId::Link {
                    row: 3,
                    col: 2,
                    url: 7,
                },
                rect: Rect::new(2, 3, 4, 1),
            },
            // The extension paints over the link, so its target sits
            // after the row's other targets, as `paint_clicks` requires.
            Target {
                id: TargetId::Extension(badge),
                rect: Rect::new(0, 3, 4, 1),
            },
        ],
    )
}

#[test]
fn click_route_pairs_the_extension_hit_with_what_lies_under_it() {
    let (cache, targets) = clicked();
    let badge = key(&cache, "badge");
    // At the range's start and inside it, clear of the link: the
    // paired target resolves, with the topmost built-in hit under it.
    for col in [0, 1] {
        assert_eq!(
            click_route(&targets, badge, col, 3),
            Some(ClickRoute {
                id: badge,
                col,
                row: 3,
                fallback: Some(TargetId::Line(crate::app::Target::Group(1))),
            }),
            "col {col}"
        );
    }
    // Over the link's cells the link lies under the extension, up to
    // and including the range's last cell.
    for col in [2, 3] {
        assert_eq!(
            click_route(&targets, badge, col, 3)
                .expect("a route")
                .fallback,
            Some(TargetId::Link {
                row: 3,
                col: 2,
                url: 7,
            }),
            "col {col}"
        );
    }
}

#[test]
fn click_route_rejects_what_the_frame_never_drew() {
    let (cache, targets) = clicked();
    let badge = key(&cache, "badge");
    // Past the range's end, on another row, and with a key the frame
    // never drew: no route, so a stale pair never dispatches.
    assert_eq!(click_route(&targets, badge, 4, 3), None);
    assert_eq!(click_route(&targets, badge, 0, 4), None);
    assert_eq!(click_route(&targets, key(&cache, "other"), 0, 3), None);
    assert_eq!(click_route(&[], badge, 0, 3), None);
}

#[test]
fn click_route_skips_turns_and_prefers_the_topmost_extension() {
    let cache = Cache::default();
    let lower = key(&cache, "lower");
    let upper = key(&cache, "upper");
    let targets = [
        Target {
            id: TargetId::Extension(lower),
            rect: Rect::new(0, 3, 6, 1),
        },
        Target {
            id: TargetId::Turn(7),
            rect: Rect::new(0, 3, 6, 1),
        },
        Target {
            id: TargetId::Extension(upper),
            rect: Rect::new(0, 3, 6, 1),
        },
    ];
    // Press and release pair on the topmost extension target; a turn
    // is a focus stop, never a click fallback.
    let mut pointer = Pointer::default();
    pointer.on_mouse(&at(LEFT, 1, 3), &targets, true);
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 1, 3), &targets, true),
        Some(TargetId::Extension(upper))
    );
    assert_eq!(
        click_route(&targets, upper, 1, 3)
            .expect("a route")
            .fallback,
        None
    );
    assert_eq!(
        click_route(&targets, lower, 1, 3)
            .expect("a route")
            .fallback,
        None
    );
}

#[test]
fn click_route_candidates() {
    let (cache, targets) = clicked();
    let badge = key(&cache, "badge");
    // The saved candidate follows the release cell: clear of the link
    // the group line lies under the range, over the link the link does.
    // A press and release on its own cells still pairs: the link covers
    // the range's far cells, so the pair stays clear of it.
    let mut pointer = Pointer::default();
    pointer.on_mouse(&at(LEFT, 0, 3), &targets, true);
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 1, 3), &targets, true),
        Some(TargetId::Extension(badge))
    );
    assert_eq!(
        click_route(&targets, badge, 1, 3)
            .expect("a route")
            .fallback,
        Some(TargetId::Line(crate::app::Target::Group(1))),
        "the saved candidate under a clear release"
    );
    pointer.on_mouse(&at(LEFT, 2, 3), &targets, true);
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 2, 3), &targets, true),
        Some(TargetId::Extension(badge))
    );
    assert_eq!(
        click_route(&targets, badge, 2, 3)
            .expect("a route")
            .fallback,
        Some(TargetId::Link {
            row: 3,
            col: 2,
            url: 7,
        }),
        "the saved candidate under a link-covered release"
    );
    // A drag off the press disarms the pair, as on any target: the
    // release proves it doesn't click, not routing it.
    pointer.on_mouse(&at(LEFT, 0, 3), &targets, true);
    pointer.on_mouse(&at(MouseKind::Drag(Button::Left), 9, 3), &targets, true);
    assert_eq!(
        pointer.on_mouse(&at(MouseKind::Release, 1, 3), &targets, true),
        None
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
