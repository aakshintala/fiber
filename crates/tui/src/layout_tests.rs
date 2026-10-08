//! Tests for the session screen's layout: widths from the screen's width,
//! shedding, and the floor.

use super::{CONVERSATION_MIN, Shares, Want, floor_line, split};
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
