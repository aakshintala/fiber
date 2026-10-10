//! Tests for the session screen's layout: widths from the screen's width,
//! shedding, and the floor.

use super::{
    CONVERSATION_MIN, PANEL_CEILING, PANEL_FLOOR, RAIL_CEILING, RAIL_FLOOR, Shares, Want, dragged,
    edge_at, edge_rect, floor_line, past_gutter, share_for, share_of, split,
};
use ratatui::layout::Rect;

/// The default shares: the rail 15%, the panel 21%.
const DEFAULT: Shares = Shares {
    rail: 15.0,
    panel: 21.0,
};

/// The panel wanted, one live session.
const PANEL: Want = Want {
    rail: false,
    rail_by_count: false,
    panel: true,
};

/// The panel and the rail wanted, two live sessions.
const BOTH: Want = Want {
    rail: true,
    rail_by_count: true,
    panel: true,
};

/// Two live sessions, the rail hidden by the person, the panel wanted.
const GRIP: Want = Want {
    rail: false,
    rail_by_count: true,
    panel: true,
};

/// A region's columns as `(x, width)`.
fn cols(rect: Option<Rect>) -> Option<(u16, u16)> {
    rect.map(|rect| (rect.x, rect.width))
}

#[test]
fn widths_follow_the_screen() {
    // (width, shares, want, rail, grip, column, panel, narrow)
    type Case = (
        u16,
        Shares,
        Want,
        Option<(u16, u16)>,
        Option<(u16, u16)>,
        (u16, u16),
        Option<(u16, u16)>,
        bool,
    );
    let cases: Vec<Case> = vec![
        // The panel shows from 114 columns: 114 − 30 = 84.
        (80, DEFAULT, PANEL, None, None, (0, 80), None, true),
        (113, DEFAULT, PANEL, None, None, (0, 113), None, true),
        (
            114,
            DEFAULT,
            PANEL,
            None,
            None,
            (0, 84),
            Some((84, 30)),
            false,
        ),
        (
            118,
            DEFAULT,
            PANEL,
            None,
            None,
            (0, 88),
            Some((88, 30)),
            false,
        ),
        // 21% of 150 is 31.5, rounded half up; of 160, 33.6.
        (
            150,
            DEFAULT,
            PANEL,
            None,
            None,
            (0, 118),
            Some((118, 32)),
            false,
        ),
        (
            160,
            DEFAULT,
            PANEL,
            None,
            None,
            (0, 126),
            Some((126, 34)),
            false,
        ),
        (
            240,
            DEFAULT,
            PANEL,
            None,
            None,
            (0, 190),
            Some((190, 50)),
            false,
        ),
        // Ceilings: the rail 48, the panel 60.
        (
            400,
            DEFAULT,
            BOTH,
            Some((0, 48)),
            None,
            (48, 292),
            Some((340, 60)),
            false,
        ),
        // The rail shows from 22 + 84 + 30 = 136.
        (
            136,
            DEFAULT,
            BOTH,
            Some((0, 22)),
            None,
            (22, 84),
            Some((106, 30)),
            false,
        ),
        // At 135 it is hidden for width, and its grip takes column 0.
        (
            135,
            DEFAULT,
            BOTH,
            None,
            Some((0, 1)),
            (1, 104),
            Some((105, 30)),
            false,
        ),
        // With the grip, a panel that fits only without it goes.
        (114, DEFAULT, BOTH, None, Some((0, 1)), (1, 113), None, true),
        (
            115,
            DEFAULT,
            BOTH,
            None,
            Some((0, 1)),
            (1, 84),
            Some((85, 30)),
            false,
        ),
        // A rail the person hid leaves its grip however wide the screen.
        (
            400,
            DEFAULT,
            GRIP,
            None,
            Some((0, 1)),
            (1, 339),
            Some((340, 60)),
            false,
        ),
        // The panel hidden by the person: the rail shows from 22 + 84,
        // and the layout is not narrow.
        (
            106,
            DEFAULT,
            Want {
                panel: false,
                ..BOTH
            },
            Some((0, 22)),
            None,
            (22, 84),
            None,
            false,
        ),
        (
            105,
            DEFAULT,
            Want {
                panel: false,
                ..BOTH
            },
            None,
            Some((0, 1)),
            (1, 104),
            None,
            false,
        ),
        // A share from configuration: 30% of 160 is 48, the ceiling.
        (
            160,
            Shares {
                rail: 30.0,
                panel: 21.0,
            },
            Want {
                panel: false,
                ..BOTH
            },
            Some((0, 48)),
            None,
            (48, 112),
            None,
            false,
        ),
        // Floors: 1% of 400 is 4, raised to 22 and 30.
        (
            400,
            Shares {
                rail: 1.0,
                panel: 1.0,
            },
            BOTH,
            Some((0, 22)),
            None,
            (22, 348),
            Some((370, 30)),
            false,
        ),
        // One live session: no rail and no grip.
        (
            160,
            DEFAULT,
            Want {
                rail: false,
                rail_by_count: false,
                panel: false,
            },
            None,
            None,
            (0, 160),
            None,
            false,
        ),
    ];
    for (width, shares, want, rail, grip, column, panel, narrow) in cases {
        let layout = split(width, 30, &shares, &want);
        assert_eq!(
            (
                cols(layout.rail),
                cols(layout.grip),
                (layout.column.x, layout.column.width),
                cols(layout.panel),
                layout.narrow,
            ),
            (rail, grip, column, panel, narrow),
            "{width} columns, {shares:?}, {want:?}"
        );
    }
}

#[test]
fn every_region_spans_the_screens_rows() {
    let layout = split(400, 30, &DEFAULT, &BOTH);
    for rect in [layout.rail, Some(layout.column), layout.panel]
        .into_iter()
        .flatten()
    {
        assert_eq!((rect.y, rect.height), (0, 30));
    }
    let grip = split(135, 30, &DEFAULT, &BOTH).grip;
    assert_eq!(grip.map(|grip| (grip.y, grip.height)), Some((0, 30)));
}

#[test]
fn past_gutter_moves_the_left_edge_and_keeps_the_right() {
    let area = Rect::new(3, 1, 10, 4);
    assert_eq!(past_gutter(area, 0), Rect::new(3, 1, 10, 4));
    assert_eq!(past_gutter(area, 1), Rect::new(4, 1, 9, 4));
    assert_eq!(
        past_gutter(Rect::new(3, 1, 1, 4), 1),
        Rect::new(4, 1, 0, 4)
    );
    assert_eq!(
        past_gutter(Rect::new(3, 1, 0, 4), 1),
        Rect::new(3, 1, 0, 4)
    );
    for (area, gutter) in [
        (area, 0),
        (area, 1),
        (Rect::new(3, 1, 1, 4), 1),
        (Rect::new(3, 1, 0, 4), 1),
    ] {
        assert_eq!(past_gutter(area, gutter).right(), area.right());
    }
}

#[test]
fn the_floor_line_shows_below_40_by_10() {
    assert_eq!(
        floor_line(39, 24).as_deref(),
        Some("Fiber needs 40×10 · now 39×24")
    );
    assert_eq!(
        floor_line(40, 9).as_deref(),
        Some("Fiber needs 40×10 · now 40×9")
    );
    assert_eq!(floor_line(40, 10), None);
    assert_eq!(
        floor_line(32, 8).as_deref(),
        Some("Fiber needs 40×10 · now 32×8")
    );
}

#[test]
fn regions_tile_the_screen_and_keep_the_conversations_minimum() {
    let wants = [
        PANEL,
        BOTH,
        GRIP,
        Want::default(),
        Want {
            panel: false,
            ..BOTH
        },
    ];
    for width in 40..=400 {
        for want in wants {
            let layout = split(width, 30, &DEFAULT, &want);
            let mut regions: Vec<Rect> =
                [layout.rail, layout.grip, Some(layout.column), layout.panel]
                    .into_iter()
                    .flatten()
                    .collect();
            regions.sort_by_key(|rect| rect.x);
            let mut x = 0;
            for rect in &regions {
                assert_eq!(rect.x, x, "{width} columns, {want:?}: {regions:?}");
                x = x.saturating_add(rect.width);
            }
            assert_eq!(x, width, "{width} columns, {want:?}");
            if layout.rail.is_some() || layout.panel.is_some() {
                assert!(
                    layout.column.width >= CONVERSATION_MIN,
                    "{width} columns, {want:?}"
                );
            }
            assert!(layout.rail.is_none() || layout.grip.is_none());
        }
    }
}

/// A 200-column layout with the rail and the panel drawn.
fn drawn() -> super::Layout {
    split(200, 40, &DEFAULT, &BOTH)
}

/// A 200-column layout with the grip: the rail wanted by count, hidden.
fn gripped() -> super::Layout {
    split(200, 40, &DEFAULT, &GRIP)
}

#[test]
fn edge_at_finds_each_edge_and_nothing_beside_it() {
    let layout = drawn();
    let rail = layout.rail.expect("a rail");
    let edge = rail.right().saturating_sub(1);
    assert_eq!(edge_at(&layout, edge), Some(super::Edge::Rail));
    assert_eq!(edge_at(&layout, edge.saturating_sub(1)), None);
    assert_eq!(edge_at(&layout, edge.saturating_add(1)), None);
    let panel = layout.panel.expect("a panel");
    assert_eq!(edge_at(&layout, panel.x), Some(super::Edge::Panel));
    assert_eq!(edge_at(&layout, panel.x.saturating_sub(1)), None);
    assert_eq!(edge_at(&layout, panel.x.saturating_add(1)), None);
    let grip = gripped();
    assert_eq!(edge_at(&grip, 0), Some(super::Edge::Grip));
    assert_eq!(edge_at(&grip, 1), None);
    let bare = split(200, 40, &DEFAULT, &Want::default());
    assert_eq!(edge_at(&bare, 0), None);
    assert_eq!(edge_at(&bare, 50), None);
}

#[test]
fn edge_rect_is_the_edge_column_full_height() {
    let layout = drawn();
    let rail = layout.rail.expect("a rail");
    assert_eq!(
        edge_rect(&layout, super::Edge::Rail),
        Some(Rect::new(
            rail.right().saturating_sub(1),
            rail.y,
            1,
            rail.height
        ))
    );
    let panel = layout.panel.expect("a panel");
    assert_eq!(
        edge_rect(&layout, super::Edge::Panel),
        Some(Rect::new(panel.x, panel.y, 1, panel.height))
    );
    let grip = gripped();
    assert_eq!(edge_rect(&grip, super::Edge::Grip), grip.grip);
    assert_eq!(edge_rect(&grip, super::Edge::Rail), None);
    let panel = grip.panel.expect("a panel beside the grip");
    assert_eq!(
        edge_rect(&grip, super::Edge::Panel),
        Some(Rect::new(panel.x, panel.y, 1, panel.height))
    );
    let bare = split(200, 40, &DEFAULT, &Want::default());
    assert_eq!(edge_rect(&bare, super::Edge::Rail), None);
    assert_eq!(edge_rect(&bare, super::Edge::Grip), None);
    assert_eq!(edge_rect(&bare, super::Edge::Panel), None);
}

#[test]
fn rail_drag_widths() {
    let plain = split(400, 40, &DEFAULT, &Want::default());
    assert_eq!(dragged(super::Edge::Rail, 60, 400, &plain), 48);
    let layout = split(160, 40, &DEFAULT, &BOTH);
    assert_eq!(layout.panel.map(|panel| panel.width), Some(34));
    assert_eq!(dragged(super::Edge::Rail, 40, 160, &layout), 41);
    assert_eq!(dragged(super::Edge::Rail, 41, 160, &layout), 42);
    assert_eq!(dragged(super::Edge::Rail, 42, 160, &layout), 42);
    assert_eq!(dragged(super::Edge::Rail, 15, 160, &layout), 16);
}

#[test]
fn grip_drag_widths() {
    let layout = split(160, 40, &DEFAULT, &GRIP);
    assert_eq!(dragged(super::Edge::Grip, 21, 160, &layout), 22);
    assert_eq!(dragged(super::Edge::Grip, 70, 160, &layout), 48);
    let narrow = split(105, 40, &DEFAULT, &GRIP);
    assert_eq!(dragged(super::Edge::Grip, 30, 105, &narrow), 21);
}

#[test]
fn panel_drag_widths() {
    let layout = split(160, 40, &DEFAULT, &BOTH);
    let rail = layout.rail.expect("a rail");
    assert_eq!(rail.width, 24);
    assert_eq!(dragged(super::Edge::Panel, 140, 160, &layout), 30);
    assert_eq!(dragged(super::Edge::Panel, 129, 160, &layout), 31);
    assert_eq!(dragged(super::Edge::Panel, 60, 160, &layout), 52);
    let wide = split(400, 40, &DEFAULT, &BOTH);
    assert_eq!(dragged(super::Edge::Panel, 300, 400, &wide), 60);
    let gripped = split(160, 40, &DEFAULT, &GRIP);
    assert_eq!(dragged(super::Edge::Panel, 60, 160, &gripped), 60);
    let tight = split(140, 40, &DEFAULT, &GRIP);
    assert_eq!(dragged(super::Edge::Panel, 70, 140, &tight), 55);
}

#[test]
fn share_for_rounds_to_a_tenth() {
    assert_eq!(share_for(25, 160), 15.6);
    assert_eq!(share_for(31, 160), 19.4);
    assert_eq!(share_for(32, 160), 20.0);
    assert_eq!(share_for(48, 400), 12.0);
}

#[test]
fn a_dragged_width_survives_the_share_round_trip() {
    for screen in 40..=999 {
        for width in 22..=48 {
            assert_eq!(
                share_of(screen, share_for(width, screen), RAIL_FLOOR, RAIL_CEILING),
                width,
                "rail {width} at {screen}"
            );
        }
        for width in 30..=60 {
            assert_eq!(
                share_of(screen, share_for(width, screen), PANEL_FLOOR, PANEL_CEILING),
                width,
                "panel {width} at {screen}"
            );
        }
    }
}
