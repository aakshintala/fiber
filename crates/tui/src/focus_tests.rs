//! Tests for navigate mode's focus order: areas, regions and stops.

use super::{Area, Regions, area_of, next_area, order};
use crate::app::Target;
use crate::mouse::{Target as Hit, TargetId};
use ratatui::layout::Rect;

/// One target at `x`, `y`, `width` by `height`.
fn at(id: TargetId, x: u16, y: u16, width: u16, height: u16) -> Hit {
    Hit {
        id,
        rect: Rect::new(x, y, width, height),
    }
}

#[test]
fn stops_run_top_to_bottom_then_left_to_right() {
    use TargetId::{Badge, DismissNotice, Line, Notice};
    let targets = vec![
        at(Line(Target::Group(1)), 0, 5, 40, 1),
        at(Notice(0), 50, 0, 10, 2),
        at(Line(Target::Group(0)), 0, 2, 40, 1),
        at(DismissNotice(0), 59, 0, 1, 1),
        at(Badge, 0, 9, 20, 1),
    ];
    assert_eq!(
        order(&targets, &Regions::default(), Area::Conversation),
        vec![
            Notice(0),
            DismissNotice(0),
            Line(Target::Group(0)),
            Line(Target::Group(1)),
            Badge,
        ]
    );
}

#[test]
fn an_id_drawn_twice_stops_once_at_its_topmost_rect() {
    use TargetId::Line;
    let targets = vec![
        at(Line(Target::Call(3)), 0, 6, 40, 1),
        at(Line(Target::Group(0)), 0, 5, 40, 1),
        at(Line(Target::Call(3)), 0, 4, 40, 1),
    ];
    assert_eq!(
        order(&targets, &Regions::default(), Area::Conversation),
        vec![Line(Target::Call(3)), Line(Target::Group(0))]
    );
}

#[test]
fn ties_keep_draw_order() {
    use TargetId::{DropSteering, Steering};
    let rect = Rect::new(10, 3, 1, 1);
    let drawn = vec![
        Hit {
            id: Steering(0),
            rect,
        },
        Hit {
            id: DropSteering(0),
            rect,
        },
    ];
    assert_eq!(
        order(&drawn, &Regions::default(), Area::Conversation),
        vec![Steering(0), DropSteering(0)]
    );
    let swapped = vec![
        Hit {
            id: DropSteering(0),
            rect,
        },
        Hit {
            id: Steering(0),
            rect,
        },
    ];
    assert_eq!(
        order(&swapped, &Regions::default(), Area::Conversation),
        vec![DropSteering(0), Steering(0)]
    );
}

/// The regions and targets of a screen with a panel, a rail and an
/// extension widget's card: no panel, rail or widget ids exist until #669
/// and #688, so a notice and "+N more" stand in for the panel's and the
/// widget's cards, and handoff notes for the rail's.
fn screened() -> (Regions, Vec<Hit>) {
    let regions = Regions {
        panel: Some(Rect::new(60, 0, 20, 20)),
        rail: Some(Rect::new(0, 0, 12, 20)),
    };
    let targets = vec![
        at(TargetId::Line(Target::Note(1)), 0, 4, 12, 2),
        at(TargetId::Line(Target::Call(1)), 12, 8, 48, 1),
        at(TargetId::Notice(5), 60, 2, 20, 3),
        at(TargetId::Line(Target::Group(0)), 12, 3, 48, 1),
        at(TargetId::MoreNotices, 60, 6, 20, 4),
        at(TargetId::Line(Target::Note(0)), 0, 1, 12, 2),
    ];
    (regions, targets)
}

#[test]
fn a_screen_with_a_panel_a_rail_and_a_widget_orders_each_area_from_the_hit_map() {
    use TargetId::{Line, MoreNotices, Notice};
    let (regions, targets) = screened();
    assert_eq!(
        order(&targets, &regions, Area::Rail),
        vec![Line(Target::Note(0)), Line(Target::Note(1))]
    );
    assert_eq!(
        order(&targets, &regions, Area::Conversation),
        vec![Line(Target::Group(0)), Line(Target::Call(1))]
    );
    assert_eq!(
        order(&targets, &regions, Area::Panel),
        vec![Notice(5), MoreNotices]
    );
}

#[test]
fn a_targets_area_is_the_one_holding_its_top_left_cell() {
    let (regions, _) = screened();
    let area =
        |x: u16, y: u16, width: u16, height: u16| regions.area(Rect::new(x, y, width, height));
    assert_eq!(area(61, 1, 2, 1), Area::Panel);
    assert_eq!(area(1, 1, 2, 1), Area::Rail);
    // A rect starting left of the panel and reaching into it is the
    // conversation's: only its top-left cell counts.
    assert_eq!(area(59, 1, 4, 1), Area::Conversation);
    assert_eq!(area(20, 1, 2, 1), Area::Conversation);
    let area = |x: u16, y: u16, width: u16, height: u16| {
        Regions::default().area(Rect::new(x, y, width, height))
    };
    assert_eq!(area(61, 1, 2, 1), Area::Conversation);
    assert_eq!(area(1, 1, 2, 1), Area::Conversation);
}

#[test]
fn tab_skips_areas_with_no_stops() {
    let (regions, targets) = screened();
    assert_eq!(
        next_area(&targets, &regions, Area::Conversation),
        Some(Area::Panel)
    );
    assert_eq!(next_area(&targets, &regions, Area::Panel), Some(Area::Rail));
    assert_eq!(
        next_area(&targets, &regions, Area::Rail),
        Some(Area::Conversation)
    );
    // Without the panel's targets Tab runs conversation, rail, then the
    // conversation again.
    let bare: Vec<Hit> = targets
        .iter()
        .filter(|target| !matches!(target.id, TargetId::Notice(_) | TargetId::MoreNotices))
        .copied()
        .collect();
    assert_eq!(
        next_area(&bare, &regions, Area::Conversation),
        Some(Area::Rail)
    );
    assert_eq!(
        next_area(&bare, &regions, Area::Rail),
        Some(Area::Conversation)
    );
    // With conversation stops alone, Tab has nowhere to go.
    let alone: Vec<Hit> = bare
        .iter()
        .filter(|target| {
            matches!(
                target.id,
                TargetId::Line(Target::Group(_)) | TargetId::Line(Target::Call(_))
            )
        })
        .copied()
        .collect();
    assert_eq!(next_area(&alone, &regions, Area::Conversation), None);
    // With only panel stops, the rail still reaches the panel.
    let panel: Vec<Hit> = targets
        .iter()
        .filter(|target| matches!(target.id, TargetId::Notice(_) | TargetId::MoreNotices))
        .copied()
        .collect();
    assert_eq!(next_area(&panel, &regions, Area::Rail), Some(Area::Panel));
}

#[test]
fn the_area_of_a_target_not_drawn_is_none() {
    let (regions, targets) = screened();
    assert_eq!(area_of(&[], &regions, TargetId::Badge), None);
    assert_eq!(
        area_of(&targets, &regions, TargetId::MoreNotices),
        Some(Area::Panel)
    );
}
