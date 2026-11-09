//! Tests for the frame-shape vocabulary: the total width encoder, the
//! closed constructors and value preservation across the seam. The
//! section renderer that turns these shapes into report text lives in
//! `cli`; its tests pin the alphabet characters and palette wire names
//! byte for byte.

use super::{FrameShape, RING_DEPTH, ShapeCell, ShapeColor, ShapeKind, encode_cell};

/// A style with every channel reset: the palette's base entry.
fn base_style() -> super::StyleValue {
    super::StyleValue {
        fg: ShapeColor::Reset,
        bg: ShapeColor::Reset,
        ul: ShapeColor::Reset,
        mods: 0,
    }
}

#[test]
fn widths_zero_and_one_are_narrow_with_no_trailing() {
    for blank in [false, true] {
        let kind = if blank {
            ShapeKind::Blank
        } else {
            ShapeKind::Content
        };
        assert_eq!(encode_cell(0, blank, false), (kind, false, 0));
        assert_eq!(encode_cell(1, blank, false), (kind, false, 0));
    }
}

#[test]
fn width_two_is_wide_with_one_trailing() {
    assert_eq!(encode_cell(2, false, false), (ShapeKind::Content, true, 1));
    assert_eq!(encode_cell(2, true, false), (ShapeKind::Blank, true, 1));
}

#[test]
fn width_three_is_wide_with_two_trailing() {
    assert_eq!(encode_cell(3, false, false), (ShapeKind::Content, true, 2));
}

#[test]
fn max_width_is_wide_with_max_minus_one_trailing() {
    assert_eq!(
        encode_cell(65535, false, false),
        (ShapeKind::Content, true, 65534)
    );
}

#[test]
fn exempt_short_circuits_with_no_trailing() {
    // Blankness and width never survive the exempt arm: the diff omits
    // these cells, so there is no lead and no continuation.
    assert_eq!(encode_cell(3, false, true), (ShapeKind::Exempt, false, 0));
    assert_eq!(encode_cell(1, true, true), (ShapeKind::Exempt, false, 0));
    assert_eq!(encode_cell(2, false, true), (ShapeKind::Exempt, false, 0));
}

#[test]
fn blank_and_content_kinds_follow_the_blank_flag() {
    assert_eq!(encode_cell(1, true, false).0, ShapeKind::Blank);
    assert_eq!(encode_cell(1, false, false).0, ShapeKind::Content);
}

#[test]
fn ring_depth_holds_a_full_screen_of_scrolling() {
    // `docs/user/reporting.md`, "Diagnostics" (at most 20 frame shapes) and
    // `docs/invocation.md`, "Commands and flags" plus docs/tui.md `/feedback` row (last 20 painted frames).
    assert_eq!(RING_DEPTH, 20);
}

#[test]
fn red_and_blue_styles_land_on_distinct_entries() {
    // Two distinct foreground colours never share a palette entry, so
    // the report can never encode them identically; the `cli` renderer
    // pins their distinct wire names.
    let red = super::StyleValue {
        fg: ShapeColor::Red,
        ..base_style()
    };
    let blue = super::StyleValue {
        fg: ShapeColor::Blue,
        ..base_style()
    };
    assert_ne!(red, blue);
    assert_ne!(red.fg, blue.fg);
}

#[test]
fn rgb_and_indexed_styles_are_distinct_entries() {
    let rgb = super::StyleValue {
        fg: ShapeColor::Rgb(1, 2, 3),
        ..base_style()
    };
    let indexed = super::StyleValue {
        fg: ShapeColor::Indexed(1),
        ..base_style()
    };
    assert_ne!(rgb, indexed);
    assert_ne!(rgb.fg, indexed.fg);
}

#[test]
fn shape_cells_round_trip_through_the_constructor() {
    // Every reachable combination of kind, width class and style slot
    // survives construction unchanged.
    for kind in [
        ShapeKind::Blank,
        ShapeKind::Content,
        ShapeKind::Continuation,
        ShapeKind::Exempt,
    ] {
        for wide in [false, true] {
            for style in [0, 1, 7] {
                let cell = ShapeCell::new(kind, wide, style);
                assert_eq!(cell.kind(), kind);
                assert_eq!(cell.wide(), wide);
                assert_eq!(cell.style(), style);
            }
        }
    }
}

#[test]
fn shape_rows_hold_their_cells_in_order() {
    // `docs/invocation.md`, "Commands and flags" (each row lists its cells in column order,
    // left to right) plus `docs/user/reporting.md`, "Diagnostics" (per cell its width class,
    // style and whether it is blank): a row holds its cells in column order by
    // construction, so the report can place each cell in that documented order.
    let cells = vec![
        ShapeCell::new(ShapeKind::Content, false, 1),
        ShapeCell::new(ShapeKind::Continuation, false, 1),
        ShapeCell::new(ShapeKind::Blank, false, 0),
    ];
    let row = super::ShapeRow::new(cells.clone());
    assert_eq!(row.cells(), cells.as_slice());
}

#[test]
fn frame_shapes_hold_geometry_scroll_and_palette() {
    let frame = FrameShape {
        width: 80,
        height: 24,
        top: Some(12),
        has_new: true,
        palette: vec![base_style()],
        rows: vec![super::ShapeRow::new(vec![ShapeCell::new(
            ShapeKind::Content,
            false,
            0,
        )])],
    };
    assert_eq!(frame.clone(), frame);
    assert_eq!(frame.width, 80);
    assert_eq!(frame.height, 24);
    assert_eq!(frame.top, Some(12));
    assert!(frame.has_new);
    assert_eq!(frame.palette.len(), 1);
    assert_eq!(frame.rows.len(), 1);
}

#[test]
fn debug_text_of_every_reachable_combo_is_ascii() {
    // No shape value holds a glyph or string, so its debug text stays
    // ASCII: a structural pass over the closed vocabulary.
    for color in [
        ShapeColor::Reset,
        ShapeColor::Black,
        ShapeColor::Red,
        ShapeColor::Green,
        ShapeColor::Yellow,
        ShapeColor::Blue,
        ShapeColor::Magenta,
        ShapeColor::Cyan,
        ShapeColor::Gray,
        ShapeColor::DarkGray,
        ShapeColor::LightRed,
        ShapeColor::LightGreen,
        ShapeColor::LightYellow,
        ShapeColor::LightBlue,
        ShapeColor::LightMagenta,
        ShapeColor::LightCyan,
        ShapeColor::White,
        ShapeColor::Indexed(7),
        ShapeColor::Rgb(1, 2, 3),
    ] {
        let text = format!("{color:?}");
        assert!(text.is_ascii(), "{text}");
    }
    for kind in [
        ShapeKind::Blank,
        ShapeKind::Content,
        ShapeKind::Continuation,
        ShapeKind::Exempt,
    ] {
        let cell = ShapeCell::new(kind, true, 3);
        let text = format!("{cell:?} {kind:?}");
        assert!(text.is_ascii(), "{text}");
    }
    let frame = FrameShape {
        width: 80,
        height: 24,
        top: None,
        has_new: false,
        palette: vec![base_style()],
        rows: vec![],
    };
    let text = format!("{frame:?}");
    assert!(text.is_ascii(), "{text}");
}
