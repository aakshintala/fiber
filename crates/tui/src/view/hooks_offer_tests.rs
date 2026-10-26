//! The hooks install offer's drawing: the title, the dim sorted names
//! line, the choices with Enter's focused, and their clicks.

use super::draw;
use crate::app::{App, Effect};
use crate::configure::Configure;
use crate::configure_fake::Fake;
use crate::home::Launch;
use crate::keys::{Button, Key, Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::{Pointer, TargetId};
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use std::path::PathBuf;
use std::sync::Arc;

/// The hub's missing-hooks refusal: the only one that opens the offer.
const MISSING: &str =
    "The `hooks` extension is not installed. Run `fiber extension install hooks`.";

/// An app on home at `width` by `height` with the fake seam.
fn home(width: u16, height: u16) -> (App, Arc<Fake>) {
    let seam = Arc::new(Fake::new(Vec::new()));
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: Some("test/model".to_owned()),
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    });
    app.set_size(width, height);
    app.set_configure(Some(Arc::clone(&seam) as Arc<dyn Configure>));
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    (app, seam)
}

/// Types `text`, then Enter: the one sent start.
fn enter(app: &mut App, text: &str) {
    let now = fakes::clock::FakeClock::new().now();
    for ch in text.chars() {
        let _ = app.on_key(Key::Char(ch), now);
    }
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter sends the start");
    };
    assert_eq!(lines.len(), 1);
}

/// Refuses the sent start for missing hooks and answers the names
/// query with `names`: the drawn offer's names.
fn offer(app: &mut App, names: Vec<String>) {
    let id = app
        .outbox
        .pending
        .keys()
        .next()
        .cloned()
        .expect("the start waits");
    let refused = Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), serde_json::Value::String(id)),
            (
                "code".to_owned(),
                serde_json::Value::String("extension_missing".to_owned()),
            ),
            (
                "message".to_owned(),
                serde_json::Value::String(MISSING.to_owned()),
            ),
        ]
        .into_iter()
        .collect(),
    });
    assert!(app.on_line(refused).is_empty());
    let ticket = match app.take_hooks_job().expect("the query waits") {
        crate::install_worker::HooksJob::Query(query) => query.ticket,
        crate::install_worker::HooksJob::Install(_) => panic!("the refusal queues names"),
    };
    app.on_hooks_offer_names(ticket, Ok(names));
    assert!(app.hooks_offer_open());
}

/// The drawn rows of `buf`.
fn row_text(buf: &Buffer, y: u16, width: u16) -> String {
    (0..width)
        .map(|x| buf[(x, y)].symbol().to_owned())
        .collect()
}

/// The offer shows its title, its sorted sanitized names, both choices
/// and what the mouse does.
#[test]
fn offer_names_the_sorted_blocking_hooks() {
    let (mut app, _) = home(80, 24);
    enter(&mut app, "hi");
    offer(&mut app, vec!["zeta".to_owned(), "a\tb".to_owned()]);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    draw(&app, area, &mut buf, &mut Vec::new());
    let shown = crate::view::text(&buf);
    assert!(shown.contains("Install hooks?"), "{shown}");
    assert!(
        shown.contains("Configuration declares blocking hooks: a b, zeta."),
        "{shown}"
    );
    assert!(shown.contains("install and start"), "{shown}");
    assert!(shown.contains("cancel"), "{shown}");
    assert!(shown.contains("click a choice"), "{shown}");
    // The names line stays dim.
    let y = (0..24)
        .find(|y| row_text(&buf, *y, 80).contains("blocking hooks:"))
        .expect("the names line draws");
    assert!(
        (0..80)
            .filter(|x| buf[(*x, y)].symbol() != " ")
            .all(|x| buf[(x, y)].modifier.contains(Modifier::DIM)),
        "{}",
        row_text(&buf, y, 80)
    );
}

/// Enter is the single focused choice: barred, bold, and the only › row.
#[test]
fn enter_is_the_single_barred_focus() {
    let (mut app, _) = home(80, 24);
    enter(&mut app, "hi");
    offer(&mut app, vec!["fmt".to_owned()]);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    draw(&app, area, &mut buf, &mut Vec::new());
    let mut barred = 0;
    for y in 0..24 {
        let row = row_text(&buf, y, 80);
        if row.contains("› ") {
            barred += 1;
            assert!(row.contains("Enter"), "{row}");
            assert!(row.contains("install and start"), "{row}");
            assert!(
                (0..80)
                    .filter(|x| buf[(*x, y)].symbol() != " ")
                    .all(|x| buf[(x, y)].modifier.contains(Modifier::BOLD)),
                "{row}"
            );
        }
        if row.contains("Esc") && row.contains("cancel") {
            assert!(!row.contains("› "), "{row}");
            let at = row.find("cancel").expect("the cancel label");
            assert!(
                (u16::try_from(at).unwrap_or(u16::MAX)
                    ..u16::try_from(at + "cancel".len()).unwrap_or(0))
                    .all(|x| buf[(x, y)].modifier.contains(Modifier::DIM)),
                "{row}"
            );
        }
    }
    assert_eq!(barred, 1);
}

/// With the names still on the worker the offer draws nothing.
#[test]
fn the_offer_draws_nothing_while_the_query_runs() {
    let (mut app, _) = home(80, 24);
    enter(&mut app, "hi");
    let id = app
        .outbox
        .pending
        .keys()
        .next()
        .cloned()
        .expect("the start waits");
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), serde_json::Value::String(id)),
            (
                "code".to_owned(),
                serde_json::Value::String("extension_missing".to_owned()),
            ),
            (
                "message".to_owned(),
                serde_json::Value::String(MISSING.to_owned()),
            ),
        ]
        .into_iter()
        .collect(),
    }));
    assert!(app.hooks_offer_open(), "the query runs");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let slab = draw(&app, area, &mut buf, &mut Vec::new());
    assert!(slab.is_empty());
    assert!(!crate::view::text(&buf).contains("Install hooks?"));
}

/// Every frame cell outside the choices and the ✕ swallows its click:
/// the draw lays one slab-wide question target under the frame's own
/// targets, so background clicks never reach the rows beneath.
#[test]
fn the_frame_background_swallows_clicks() {
    let (mut app, _) = home(80, 24);
    enter(&mut app, "hi");
    offer(&mut app, vec!["fmt".to_owned()]);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    let slab = draw(&app, area, &mut buf, &mut targets);
    assert!(!slab.is_empty(), "the offer draws its frame");
    let swallowed = targets
        .iter()
        .find(|target| target.id == TargetId::Home(crate::home::Spot::Question))
        .copied()
        .unwrap_or_else(|| panic!("the frame background swallows clicks"));
    assert_eq!(swallowed.rect, slab);
    // A background cell of the frame hits the swallower, not a choice.
    let over = |col: u16, row: u16| {
        targets.iter().any(|target| {
            target
                .rect
                .contains(ratatui::layout::Position::new(col, row))
                && !matches!(target.id, TargetId::Home(crate::home::Spot::Question))
        })
    };
    let background = (slab.y..slab.bottom())
        .flat_map(|row| (slab.x..slab.right()).map(move |col| (col, row)))
        .find(|(col, row)| !over(*col, *row))
        .expect("the frame has a background cell");
    assert_eq!(
        crate::mouse::hit(&targets, background.0, background.1),
        Some(TargetId::Home(crate::home::Spot::Question))
    );
}

/// Clicking the drawn install choice commits the install; clicking the
/// drawn cancel choice cancels.
#[test]
fn offer_choice_clicks_use_the_drawn_screen_targets() {
    for install in [true, false] {
        let (mut app, _) = home(80, 24);
        enter(&mut app, "hi");
        offer(&mut app, vec!["fmt".to_owned()]);
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        let mut targets = Vec::new();
        draw(&app, area, &mut buf, &mut targets);
        let want = if install {
            crate::home::HooksChoice::Install
        } else {
            crate::home::HooksChoice::Cancel
        };
        let target = targets
            .iter()
            .find(|target| target.id == TargetId::Home(crate::home::Spot::HooksOffer(want)))
            .copied()
            .unwrap_or_else(|| panic!("the drawn choice has a target"));
        let mouse = Mouse {
            kind: MouseKind::Press(Button::Left),
            col: target.rect.x + target.rect.width / 2,
            row: target.rect.y + target.rect.height / 2,
        };
        let mut pointer = Pointer::default();
        let now = fakes::clock::FakeClock::new().now();
        assert_eq!(
            app.on_mouse(&mouse, &targets, &mut pointer, true, now),
            Effect::None
        );
        assert_eq!(
            app.on_mouse(
                &Mouse {
                    kind: MouseKind::Release,
                    ..mouse
                },
                &targets,
                &mut pointer,
                true,
                now,
            ),
            Effect::None
        );
        if install {
            assert!(app.take_hooks_job().is_some(), "the install commits");
            assert!(app.hooks_offer_open(), "the offer waits on the install");
        } else {
            assert!(!app.hooks_offer_open(), "the cancel closes the offer");
        }
    }
}
