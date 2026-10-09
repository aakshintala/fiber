//! Tests for drawing the rail: groups, times, words, colours and the
//! card rows, read from the drawn buffer.

use super::{draw, elapsed, groups, tone, word};
use crate::app::App;
use crate::app::rail::Spot;
use crate::home::{Launch, Left, Row, State};
use crate::link::Line;
use crate::markdown::Role;
use crate::mouse::{Target, TargetId};
use crate::view::{render, text};
use contract::SessionId;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};
use std::path::PathBuf;

const A: &str = "s_aaaaaaaaaaaaaaaa";
const B: &str = "s_bbbbbbbbbbbbbbbb";
const C: &str = "s_cccccccccccccccc";
const D: &str = "s_dddddddddddddddd";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];
/// The wall time every test sets: 2023-11-14T22:13:20Z.
const WALL: u64 = 1_700_000_000_000;

/// A row in `project` at `workspace` with `spend`, ended `left`.
fn row(key: u64, project: &str, workspace: &str, spend: f64, left: Option<Left>) -> Row {
    Row {
        key,
        id: SessionId(format!("s_{key:016x}")),
        name: "work".to_owned(),
        workspace: workspace.to_owned(),
        git: false,
        project: project.to_owned(),
        state: State::Idle,
        left,
        waiting: None,
        spend,
        jobs: 0,
        delegates: 0,
        clients: 0,
        note: None,
        status: None,
    }
}

/// An app with home state, attached to `A`, its rail `share` percent of
/// a 200x40 screen, with the wall time set.
fn app(share: f64) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/Users/you/work/fiber"),
        project: "-w".to_owned(),
        rail_share: share,
        panel_share: 21.0,
        panel_cards: CARDS.map(str::to_owned).to_vec(),
        ..Default::default()
    });
    app.attach(SessionId(A.to_owned()));
    app.set_size(200, 40);
    app.set_wall(WALL);
    app
}

/// A `session_status` for `session` in the launch project, idle unless
/// `fields` say otherwise.
fn status(session: &str, fields: Value) -> Line {
    let mut payload = json!({
        "name": "fix the parser", "workspace": "/Users/you/work/fiber",
        "project": "-w", "state": "idle", "since": WALL,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in fields.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A hub `session_left` for `session`, ended `how`.
fn left(session: &str, how: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"session_id": session, "how": how})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// A spend of `dollars` billed.
fn spend(dollars: f64) -> Value {
    json!({"tokens": {"input": 1, "cache_read": 0, "cache_write": {}, "output": 2},
        "cost": dollars, "subscription_cost": 0.0})
}

/// The rail rect drawn alone at its own width, 40 rows.
fn rail(app: &App) -> Buffer {
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.rail)
        .unwrap_or_else(|| panic!("a rail"));
    let area = Rect::new(0, 0, rect.width, rect.height);
    let mut buf = Buffer::empty(area);
    draw(app, area, &mut buf, None, &mut Vec::new());
    buf
}

/// The drawn rows' text.
fn lines(buf: &Buffer) -> Vec<String> {
    text(buf).lines().map(str::to_owned).collect()
}

/// The whole 200x40 screen drawn.
fn screen(app: &App) -> Buffer {
    let area = Rect::new(0, 0, 200, 40);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    buf
}

/// Two cards, `A` and `B`, with `B`'s fields: `B`'s card's text rows
/// start on row 8 of the rail.
fn two(share: f64, fields: Value) -> App {
    let mut app = app(share);
    app.on_line(status(A, json!({})));
    app.on_line(status(B, fields));
    app
}

/// The text of `B`'s card's `n`th text row, 0-based, in `two`'s rail.
fn b_row(buf: &Buffer, n: u16) -> String {
    lines(buf)
        .get(usize::from(8 + n))
        .cloned()
        .unwrap_or_default()
}

#[test]
fn groups_put_the_launch_project_first_then_first_appearance() {
    let site = row(0, "-site", "/src/site", 0.0, None);
    let fiber = row(1, "-w", "/work/fiber", 0.0, None);
    let hub = row(2, "-hub", "/work/hub", 0.0, None);
    let site2 = row(3, "-site", "/src/site", 0.0, None);
    let cards = [&site, &fiber, &hub, &site2];
    let names: Vec<(String, Vec<u64>)> = groups(&cards, "-w")
        .into_iter()
        .map(|group| {
            (
                group.name,
                group.cards.iter().map(|card| card.key).collect(),
            )
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ("fiber".to_owned(), vec![1]),
            ("site".to_owned(), vec![0, 3]),
            ("hub".to_owned(), vec![2]),
        ]
    );
}

#[test]
fn a_group_is_named_by_its_first_cards_workspace() {
    let first = row(0, "-p", "/work/one/", 0.0, None);
    let second = row(1, "-p", "/work/two", 0.0, None);
    let named = groups(&[&first, &second], "-w");
    assert_eq!(named.len(), 1);
    assert_eq!(named[0].name, "one");
}

#[test]
fn spend_sums_live_cards() {
    let one = row(0, "-w", "/w", 1.25, None);
    let two = row(1, "-w", "/w", 0.5, None);
    let crashed = row(2, "-w", "/w", 7.0, Some(Left::Crashed));
    let summed = groups(&[&one, &two, &crashed], "-w");
    assert!((summed[0].spend - 1.75).abs() < f64::EPSILON);
}

#[test]
fn elapsed_uses_one_unit() {
    for (ms, shown) in [
        (0, "0s"),
        (59_999, "59s"),
        (60_000, "1m"),
        (3_599_999, "59m"),
        (3_600_000, "1h"),
        (86_399_999, "23h"),
        (86_400_000, "1d"),
    ] {
        assert_eq!(elapsed(WALL, WALL + ms), shown, "{ms}");
    }
    assert_eq!(elapsed(WALL + 5_000, WALL), "0s");
}

#[test]
fn word_and_tone_per_state() {
    for (state, left, shown, role) in [
        (State::Working, None, "WORKING", Role::Accent),
        (State::Jobs, None, "WORKING", Role::Accent),
        (State::Retrying, None, "RETRYING", Role::Warning),
        (State::Waiting, None, "NEEDS INPUT", Role::Attention),
        (State::Idle, None, "READY", Role::Muted),
        (State::Unreadable, None, "CANNOT ATTACH", Role::Muted),
        (State::Working, Some(Left::Crashed), "CRASHED", Role::Error),
        (State::Waiting, Some(Left::Crashed), "CRASHED", Role::Error),
    ] {
        let mut card = row(0, "-w", "/w", 0.0, left);
        card.state = state;
        assert_eq!(word(&card), shown, "{state:?}");
        assert_eq!(tone(&card), role, "{state:?}");
    }
}

/// The fill colour of `B`'s bar at `pct` percent of a 100-token window.
fn bar_fill(pct: u64) -> Option<ratatui::style::Color> {
    let app = two(20.0, json!({"context": {"tokens": pct, "window": 100}}));
    let buf = rail(&app);
    (0..buf.area.width)
        .filter_map(|x| buf.cell((x, 11)))
        .find(|cell| cell.symbol() == "▆")
        .and_then(|cell| cell.style().fg)
}

#[test]
fn bar_colour_thresholds() {
    assert_eq!(bar_fill(59), Some(Role::Accent.color()));
    assert_eq!(bar_fill(60), Some(Role::Warning.color()));
    assert_eq!(bar_fill(84), Some(Role::Warning.color()));
    assert_eq!(bar_fill(85), Some(Role::Error.color()));
}

#[test]
fn a_full_context_fills_every_cell() {
    let app = two(20.0, json!({"context": {"tokens": 1000, "window": 1000}}));
    // A 40-column rail: 36 text columns less `$0.00`, `100%` and two
    // spaces leaves 25 cells.
    assert_eq!(
        b_row(&rail(&app), 3),
        format!("▌ $0.00 {} 100%", "▆".repeat(25))
    );
}

#[test]
fn no_context_draws_no_bar_and_no_percentage() {
    let app = two(20.0, json!({}));
    assert_eq!(b_row(&rail(&app), 3), "▌ $0.00");
}

#[test]
fn a_zero_window_draws_no_bar() {
    let app = two(20.0, json!({"context": {"tokens": 0, "window": 0}}));
    assert_eq!(b_row(&rail(&app), 3), "▌ $0.00");
}

#[test]
fn the_bar_goes_below_30_columns() {
    let context = json!({"context": {"tokens": 50, "window": 100}});
    // 14.5% of 200 is 29 columns; 15% is 30.
    let narrow = two(14.5, context.clone());
    assert_eq!(
        b_row(&rail(&narrow), 3),
        format!("▌ $0.00{}50%", " ".repeat(25 - 5 - 3))
    );
    let wide = two(15.0, context);
    assert_eq!(
        b_row(&rail(&wide), 3),
        format!("▌ $0.00 {}{} 50%", "▆".repeat(8), "░".repeat(8))
    );
}

#[test]
fn a_bar_with_no_room_keeps_the_percentage_whole() {
    // A 30-column rail has 26 text columns: this spend and `50%` leave
    // no cell for the bar.
    let app = two(
        15.0,
        json!({"spend": spend(1e18), "context": {"tokens": 50, "window": 100}}),
    );
    let shown = b_row(&rail(&app), 3);
    assert!(shown.ends_with(" 50%"), "{shown}");
    assert!(!shown.contains('░'), "{shown}");
}

#[test]
fn a_control_character_in_a_name_draws_as_a_space() {
    let app = two(20.0, json!({"name": "fix\tthe\nparser"}));
    assert_eq!(b_row(&rail(&app), 1), "▌ fix the parser");
}

#[test]
fn the_on_screen_card_is_raised() {
    let app = two(20.0, json!({}));
    let buf = rail(&app);
    for y in 2..6 {
        let bg = buf.cell((5, y)).and_then(|cell| cell.style().bg);
        assert_eq!(bg, Some(Role::SurfaceRaised.color()), "row {y}");
    }
    for y in 8..12 {
        let bg = buf.cell((5, y)).and_then(|cell| cell.style().bg);
        assert_eq!(bg, Some(Role::Surface.color()), "row {y}");
    }
}

#[test]
fn a_crashed_card_shows_a_cross_for_its_time() {
    let mut app = two(20.0, json!({"since": WALL - 16_000}));
    // A third live session keeps the rail drawn once `B` crashes.
    app.on_line(status(C, json!({})));
    assert!(b_row(&rail(&app), 0).ends_with("16s"));
    app.on_line(left(B, "crashed"));
    let first = b_row(&rail(&app), 0);
    assert!(first.starts_with("▌ 2 ✗ CRASHED"), "{first}");
    assert!(first.ends_with('✕'), "{first}");
    assert!(!first.contains("16s"), "{first}");
}

#[test]
fn waiting_text_is_in_the_attention_colour() {
    let app = two(
        20.0,
        json!({"state": "waiting", "waiting": {"request_id": "r_1",
            "kind": "approval", "summary": "shell cargo test"}}),
    );
    let buf = rail(&app);
    assert_eq!(b_row(&buf, 2), "▌ approval: shell cargo test");
    let fg = buf.cell((2, 10)).and_then(|cell| cell.style().fg);
    assert_eq!(fg, Some(Role::Attention.color()));
}

/// Three projects' sessions at `share`: `A` on screen and idle on
/// `main`, a waiting card in another project, a working card with no
/// context, and a crashed card in a third project.
fn three_projects(share: f64) -> App {
    let mut app = app(share);
    app.on_line(status(
        B,
        json!({"workspace": "/Users/you/work/hub", "project": "-hub",
            "name": "fix flaky lock test", "since": WALL - 16_000,
            "state": "waiting", "waiting": {"request_id": "r_1",
                "kind": "approval", "summary": "shell cargo mutants --in-place"},
            "spend": spend(1.35), "context": {"tokens": 700, "window": 1000},
            "git": {"branch": "lock"}}),
    ));
    app.on_line(status(
        A,
        json!({"since": WALL - 9 * 60_000, "spend": spend(0.41),
            "git": {"branch": "main"}, "context": {"tokens": 119, "window": 1000}}),
    ));
    app.on_line(status(
        C,
        json!({"name": "write the rail", "state": "streaming",
            "since": WALL - 2 * 3_600_000, "git": {"branch": null}}),
    ));
    app.on_line(status(
        D,
        json!({"workspace": "/Users/you/src/site", "project": "-site",
            "name": "deploy", "since": WALL - 3 * 86_400_000,
            "spend": spend(2.0), "context": {"tokens": 900, "window": 1000}}),
    ));
    app.on_line(left(D, "crashed"));
    app
}

#[test]
fn rail_three_projects() {
    for (share, name) in [
        (11.0, "rail_three_projects_at_22"),
        (14.5, "rail_three_projects_at_29"),
        (15.0, "rail_three_projects_at_30"),
        (24.0, "rail_three_projects_at_48"),
    ] {
        let app = three_projects(share);
        insta::assert_snapshot!(name, text(&screen(&app)));
    }
}

#[test]
fn rail_waiting_card_shows_its_wait() {
    let app = two(
        20.0,
        json!({"name": "fix flaky lock test", "since": WALL - 16_000,
            "state": "waiting", "waiting": {"request_id": "r_1",
                "kind": "question", "summary": "which branch do I rebase onto first"},
            "spend": spend(1.35), "context": {"tokens": 120, "window": 1000}}),
    );
    insta::assert_snapshot!("rail_waiting_card_shows_its_wait", text(&rail(&app)));
}

/// The rail rect drawn alone with `pointer`: the buffer and the targets.
fn hovered(app: &App, pointer: Option<(u16, u16)>) -> (Buffer, Vec<Target>) {
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.rail)
        .unwrap_or_else(|| panic!("a rail"));
    let area = Rect::new(0, 0, rect.width, rect.height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    draw(app, area, &mut buf, pointer, &mut targets);
    (buf, targets)
}

/// The rail's targets with their rects.
fn rail_targets(targets: &[Target]) -> Vec<(Spot, Rect)> {
    targets
        .iter()
        .filter_map(|target| {
            if let TargetId::Rail(spot) = target.id {
                Some((spot, target.rect))
            } else {
                None
            }
        })
        .collect()
}

/// `count` live sessions in the launch project, `A` first and on screen.
fn many(count: u64) -> App {
    let mut app = app(15.0);
    app.on_line(status(A, json!({})));
    for n in 1..count {
        app.on_line(status(&format!("s_{n:016x}"), json!({})));
    }
    app
}

/// The key of `session`'s card.
fn key(app: &App, session: &str) -> u64 {
    app.rail_cards()
        .and_then(|(cards, _)| {
            cards
                .iter()
                .find(|row| row.id.0 == session)
                .map(|row| row.key)
        })
        .unwrap_or_else(|| panic!("a card for {session}"))
}

#[test]
fn targets_cover_each_card() {
    // Seven cards in one group take 43 rows: the last card's text rows
    // start on row 38, and its target stops at the rail's foot.
    let app = many(7);
    let keys: Vec<u64> = app
        .rail_cards()
        .map(|(cards, _)| cards.iter().map(|card| card.key).collect())
        .unwrap_or_default();
    let (_, targets) = hovered(&app, None);
    let expected: Vec<(Spot, Rect)> = keys
        .iter()
        .zip(0u16..)
        .map(|(key, at)| {
            let top = 2 + at * 6;
            (Spot::Card(*key), Rect::new(0, top, 29, (40 - top).min(4)))
        })
        .collect();
    assert_eq!(rail_targets(&targets), expected);
}

#[test]
fn rail_hover_foot_line() {
    let app = two(
        20.0,
        json!({"name": "a name too long for the rail's card to show it whole",
            "workspace": "/Users/you/work/hub"}),
    );
    let (buf, _) = hovered(&app, Some((5, 9)));
    insta::assert_snapshot!("rail_hover_foot_line", text(&buf));
    assert_eq!(
        lines(&buf).last().map(String::as_str),
        Some("  a name too long for the rail's card")
    );
    let fg = buf.cell((2, 39)).and_then(|cell| cell.style().fg);
    assert_eq!(fg, Some(Role::Muted.color()));
}

#[test]
fn no_foot_line_without_a_pointer() {
    let app = two(20.0, json!({"workspace": "/Users/you/work/hub"}));
    for pointer in [None, Some((5, 0)), Some((5, 13))] {
        let (buf, _) = hovered(&app, pointer);
        assert!(!text(&buf).contains("work/hub"), "{pointer:?}");
    }
    let (buf, _) = hovered(&app, Some((5, 8)));
    assert!(text(&buf).contains("work/hub"));
}

#[test]
fn rail_scrolled() {
    let mut app = many(10);
    let down = crate::keys::Mouse {
        kind: crate::keys::MouseKind::WheelDown,
        col: 5,
        row: 10,
    };
    app.on_wheel(&down);
    app.on_wheel(&down);
    assert_eq!(app.rail_state().scroll(), 7);
    let (buf, targets) = hovered(&app, None);
    insta::assert_snapshot!("rail_scrolled", text(&buf));
    // The first card is past the top: the second card's target leads.
    assert_eq!(
        rail_targets(&targets).first(),
        Some(&(
            Spot::Card(key(&app, "s_0000000000000001")),
            Rect::new(0, 1, 29, 4)
        ))
    );
}

#[test]
fn on_home_no_rail_is_drawn() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: CARDS.map(str::to_owned).to_vec(),
        ..Default::default()
    });
    app.set_size(200, 40);
    for session in [A, B, C] {
        app.on_line(status(session, json!({})));
    }
    let area = Rect::new(0, 0, 200, 40);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    assert!(app.home_screen().is_some());
    assert!(rail_targets(&targets).is_empty());
}
