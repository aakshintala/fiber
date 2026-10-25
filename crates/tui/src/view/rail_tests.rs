//! Tests for drawing the rail: groups, times, words, colours and the
//! card rows, read from the drawn buffer.

use super::{draw, elapsed, groups, tone, word};
use crate::app::App;
use crate::app::rail::Spot;
use crate::home::{Launch, Left, Row, State};
use crate::link::Line;
use crate::markdown::{Role, style};
use crate::mouse::{Target, TargetId};
use crate::stroke::{Code, Mods, Stroke};
use crate::view::{render, text};
use contract::SessionId;
use contract::clock::Clock;
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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/Users/you/work/fiber"),
            project: "-w".to_owned(),
            git: false,
        },
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
        "project": "-w", "state": "idle", "started": 0, "calls": 0, "since": WALL,
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

/// An accepted hub command with `result`.
fn accepted(id: &str, result: Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("result".to_owned(), result),
        ]
        .into_iter()
        .collect(),
    })
}

/// A `hub_hello` this terminal reads.
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// Accepts the feed and the empty unscoped first page, then triggers a
/// scoped refresh as `B` leaves and answers it with `total` and no rows:
/// the held total with the loaded fallback beneath it.
fn hold_total(app: &mut App, total: usize) {
    fn parse(line: &String) -> Value {
        serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}"))
    }
    let start: Vec<Value> = app.on_line(hello()).iter().map(parse).collect();
    let feed = start
        .iter()
        .find(|line| line["command"] == "feed")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a feed"));
    let recent = start
        .iter()
        .find(|line| line["command"] == "recent" && line.get("args").is_none())
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("an unscoped recent"));
    assert!(app.on_line(accepted(feed, json!({}))).is_empty());
    assert!(
        app.on_line(accepted(recent, json!({"sessions": []})))
            .is_empty()
    );
    let fanout: Vec<Value> = app.on_line(left(B, "exited")).iter().map(parse).collect();
    // `docs/tui.md:268` (a session leaving asks for its project's total).
    assert!(
        !fanout.is_empty(),
        "the session-left trigger asks for a scoped total"
    );
    for line in &fanout {
        // `docs/invocation.md:1091` (scoped `recent` for the project).
        assert_eq!(line["command"], "recent");
        let id = line["id"].as_str().unwrap_or_else(|| panic!("a scoped id"));
        assert!(
            app.on_line(accepted(id, json!({"sessions": [], "total": total})))
                .is_empty()
        );
    }
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
    let names: Vec<(String, Vec<u64>)> = groups(&cards, Some("-w"), &[])
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
    let named = groups(&[&first, &second], Some("-w"), &[]);
    assert_eq!(named.len(), 1);
    assert_eq!(named[0].name, "one");
}

#[test]
fn spend_sums_live_cards() {
    let one = row(0, "-w", "/w", 1.25, None);
    let two = row(1, "-w", "/w", 0.5, None);
    let crashed = row(2, "-w", "/w", 7.0, Some(Left::Crashed));
    let summed = groups(&[&one, &two, &crashed], Some("-w"), &[]);
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

/// A row whose latest status accepted `close` reads CLOSING, dim: the
/// fold path from a restarted terminal's first status, with no
/// closed-session list involved.
#[test]
fn a_closing_status_reads_closing_dim() {
    let Line::Session(envelope) = status(B, json!({"closing": true})) else {
        panic!("session line");
    };
    let row = crate::home::from_status(&envelope);
    assert_eq!(
        row.status.as_ref().map(|status| status.closing),
        Some(true),
        "the fold keeps closing"
    );
    assert_eq!(word(&row), "CLOSING");
    assert_eq!(tone(&row), Role::Muted);
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

/// The header row's text and click targets: the project name bold in
/// the accent colour, " +" after it starting a session in the
/// project, and the summed spend with the exited count right-aligned
/// and dim (`docs/tui.md`, "The rail").
#[test]
fn the_header_names_the_project_then_plus_then_dim_spend() {
    use ratatui::style::Modifier;
    let app = two(20.0, json!({}));
    let drawn = super::rows(&app, 40);
    let header = &drawn[0];
    let text = header.line.to_string();
    assert!(text.starts_with("  fiber +"), "{text:?}");
    assert!(text.ends_with("$0.00 · 0 ✓"), "{text:?}");
    let spans = &header.line.spans;
    assert_eq!(spans.len(), 5, "{text:?}");
    assert_eq!(spans[0].content, "  ");
    assert_eq!(spans[1].content, "fiber");
    assert_eq!(
        spans[1].style,
        style(Role::Accent).add_modifier(Modifier::BOLD)
    );
    assert_eq!(spans[2].content, " +");
    assert_eq!(spans[4].content, "$0.00 · 0 ✓");
    assert_eq!(spans[4].style, style(Role::Muted));
    // The "+" starts a session in the project's first card's
    // session and "0 ✓" opens its exited list: two margin columns,
    // the name, then a space; the count sits at the right.
    assert_eq!(
        header.spots,
        vec![
            (8, 1, 1, Spot::Start(key(&app, A))),
            (35, 3, 1, Spot::Done(key(&app, A))),
        ]
    );
}

/// A project name too long for the header keeps its "+" and its
/// count: the name is cut, never a chip.
#[test]
fn a_long_project_name_keeps_its_plus() {
    let mut app = app(20.0);
    app.on_line(status(
        A,
        json!({"workspace": "/Users/you/work/a-project-name-far-too-long-for-its-header"}),
    ));
    app.on_line(status(B, json!({})));
    let drawn = super::rows(&app, 40);
    let header = &drawn[0];
    let text = header.line.to_string();
    assert!(text.contains('+'), "{text:?}");
    assert!(text.ends_with("$0.00 · 0 ✓"), "{text:?}");
    let plus = u16::try_from(text.find('+').unwrap_or_default()).unwrap_or(u16::MAX);
    assert_eq!(
        header.spots,
        vec![
            (plus, 1, 1, Spot::Start(key(&app, A))),
            (35, 3, 1, Spot::Done(key(&app, A))),
        ]
    );
}

/// The header draws the hub's held total, not the loaded rows: one
/// loaded exit with a total of seven reads "7 ✓", dim.
#[test]
fn the_header_draws_the_held_total_beside_the_spend() {
    let mut app = two(20.0, json!({}));
    hold_total(&mut app, 7);
    let drawn = super::rows(&app, 40);
    let header = &drawn[0];
    let text = header.line.to_string();
    assert!(text.ends_with("$0.00 · 7 ✓"), "{text:?}");
    // `docs/tui.md:332-335` (the project header's start target and right-aligned exited count).
    assert_eq!(
        header.spots,
        vec![
            (8, 1, 1, Spot::Start(key(&app, A))),
            (35, 3, 1, Spot::Done(key(&app, A))),
        ]
    );
}

/// A total above nine widens the count: "12 ✓" keeps the spend whole
/// while the `Done` target stays on the first card.
#[test]
fn the_header_widens_the_count_for_a_two_digit_total() {
    let mut app = two(20.0, json!({}));
    hold_total(&mut app, 12);
    let drawn = super::rows(&app, 40);
    let header = &drawn[0];
    let text = header.line.to_string();
    assert!(text.ends_with("$0.00 · 12 ✓"), "{text:?}");
    assert!(
        header
            .spots
            .iter()
            .any(|spot| matches!(spot, (_, _, _, Spot::Done(_)))),
        "{text:?}"
    );
}

/// A held total of zero draws "0 ✓" with the loaded row beneath it.
#[test]
fn the_header_draws_zero_for_a_held_zero_total() {
    let mut app = two(20.0, json!({}));
    hold_total(&mut app, 0);
    let drawn = super::rows(&app, 40);
    let header = &drawn[0];
    let text = header.line.to_string();
    assert!(text.ends_with("$0.00 · 0 ✓"), "{text:?}");
    assert!(
        header
            .spots
            .iter()
            .any(|spot| matches!(spot, (_, _, _, Spot::Done(_)))),
        "{text:?}"
    );
}

/// An exited session leaves the rail but stays in the header's count:
/// its spend leaves the live sum while "1 ✓" stays beside it, dim.
#[test]
fn the_header_counts_one_exited_session_beside_the_spend() {
    let mut app = two(20.0, json!({}));
    app.on_line(left(B, "exited"));
    let drawn = super::rows(&app, 40);
    let header = &drawn[0];
    let text = header.line.to_string();
    assert!(text.ends_with("$0.00 · 1 ✓"), "{text:?}");
    let spans = &header.line.spans;
    assert_eq!(spans.len(), 5, "{text:?}");
    assert_eq!(spans[4].content, "$0.00 · 1 ✓");
    assert_eq!(spans[4].style, style(Role::Muted));
    assert_eq!(
        header.spots,
        vec![
            (8, 1, 1, Spot::Start(key(&app, A))),
            (35, 3, 1, Spot::Done(key(&app, A))),
        ]
    );
}

// debt: kept as the multi-exit header edge case; the one-count test does not
// pin the two-count rendering.
/// Two exits read "2 ✓": the count has no singular form.
#[test]
fn the_header_counts_two_exited_sessions() {
    let mut app = two(20.0, json!({}));
    app.on_line(status(C, json!({})));
    app.on_line(left(B, "exited"));
    app.on_line(left(C, "exited"));
    let drawn = super::rows(&app, 40);
    let header = &drawn[0];
    let text = header.line.to_string();
    assert!(text.ends_with("$0.00 · 2 ✓"), "{text:?}");
    assert_eq!(
        header.spots,
        vec![
            (8, 1, 1, Spot::Start(key(&app, A))),
            (35, 3, 1, Spot::Done(key(&app, A))),
        ]
    );
}

/// A crashed session counts in neither the spend nor the exits: its
/// card stays live on the rail but its spend left with it, and a crash
/// is not an exit.
#[test]
fn a_crashed_session_counts_in_neither_spend_nor_exits() {
    let mut app = two(20.0, json!({"spend": spend(2.0)}));
    app.on_line(left(B, "crashed"));
    let drawn = super::rows(&app, 40);
    let header = &drawn[0];
    let text = header.line.to_string();
    assert!(text.ends_with("$0.00 · 0 ✓"), "{text:?}");
}

/// Each header counts only its own project's exits.
#[test]
fn each_header_counts_only_its_own_projects_exits() {
    let mut app = app(20.0);
    app.on_line(status(A, json!({})));
    app.on_line(status(
        B,
        json!({"workspace": "/Users/you/work/hub", "project": "-hub"}),
    ));
    app.on_line(status(C, json!({})));
    app.on_line(status(
        D,
        json!({"workspace": "/Users/you/work/hub", "project": "-hub"}),
    ));
    app.on_line(left(C, "exited"));
    app.on_line(left(D, "exited"));
    let drawn = super::rows(&app, 40);
    // One header and one card per project.
    assert_eq!(drawn.len(), 14);
    let launch = &drawn[0];
    let hub = &drawn[7];
    assert!(
        launch.line.to_string().ends_with("$0.00 · 1 ✓"),
        "{:?}",
        launch.line.to_string()
    );
    assert!(
        hub.line.to_string().ends_with("$0.00 · 1 ✓"),
        "{:?}",
        hub.line.to_string()
    );
    assert_eq!(
        launch.spots,
        vec![
            (8, 1, 1, Spot::Start(key(&app, A))),
            (35, 3, 1, Spot::Done(key(&app, A))),
        ]
    );
    assert_eq!(
        hub.spots,
        vec![
            (6, 1, 1, Spot::Start(key(&app, B))),
            (35, 3, 1, Spot::Done(key(&app, B))),
        ]
    );
}

/// A narrow header cuts the name but keeps the "+" and the spend
/// with its count whole.
#[test]
fn a_narrow_header_cuts_the_name_but_keeps_plus_and_count_whole() {
    let app = two(20.0, json!({}));
    let drawn = super::rows(&app, 22);
    let header = &drawn[0];
    let text = header.line.to_string();
    // 22 columns hold 18 text columns: "$0.00 · 0 ✓" keeps its 11,
    // so the name cuts to 4 less the chip.
    assert!(text.contains('+'), "{text:?}");
    assert!(text.ends_with("$0.00 · 0 ✓"), "{text:?}");
    assert!(!text.contains("fiber"), "{text:?}");
    let plus = u16::try_from(text.find('+').unwrap_or_default()).unwrap_or(u16::MAX);
    assert_eq!(
        header.spots,
        vec![
            (plus, 1, 1, Spot::Start(key(&app, A))),
            (17, 3, 1, Spot::Done(key(&app, A))),
        ]
    );
}

/// An overflowing header starts its right side at the margin, so the
/// "N ✓" target follows the drawn count instead of the right edge:
/// 12 columns leave 8 text columns for the 11-wide "$0.00 · 0 ✓",
/// and only the count's first column stays visible.
#[test]
fn an_overflowing_header_clips_its_done_target_to_the_visible_count() {
    let app = two(20.0, json!({}));
    let drawn = super::rows(&app, 12);
    let header = &drawn[0];
    let text = header.line.to_string();
    assert!(text.starts_with("  $0.00 · 0"), "{text:?}");
    assert_eq!(
        header
            .spots
            .iter()
            .find(|(_, _, _, spot)| matches!(spot, Spot::Done(_)))
            .map(|(col, wide, high, spot)| (*col, *wide, *high, *spot)),
        Some((10, 1, 1, Spot::Done(key(&app, A)))),
        "{text:?}"
    );
}

/// A header narrower than its right side hides the count whole: with
/// nothing of "N ✓" drawn there is no target for it.
#[test]
fn a_header_narrower_than_its_right_side_draws_no_done_target() {
    let app = two(20.0, json!({}));
    let drawn = super::rows(&app, 10);
    let header = &drawn[0];
    let text = header.line.to_string();
    assert!(
        header
            .spots
            .iter()
            .all(|(_, _, _, spot)| !matches!(spot, Spot::Done(_))),
        "{text:?}"
    );
}

/// A card's third row, when it is not waiting, is a dim "⎇ " followed
/// by the branch (`docs/tui.md`, "The rail").
#[test]
fn a_branch_row_shows_a_dim_prefix() {
    let app = two(20.0, json!({"git": {"branch": "main"}}));
    let buf = rail(&app);
    assert_eq!(b_row(&buf, 2), "▌ ⎇ main");
    // B's third text row is the rail's eleventh: the stripe, a space,
    // then the prefix and the branch.
    let prefix = buf.cell((2, 10)).and_then(|cell| cell.style().fg);
    assert_eq!(prefix, Some(Role::Muted.color()));
    let space = buf.cell((3, 10)).and_then(|cell| cell.style().fg);
    assert_eq!(space, Some(Role::Muted.color()));
    for x in 4..8 {
        let fg = buf.cell((x, 10)).and_then(|cell| cell.style().fg);
        assert_eq!(fg, Some(ratatui::style::Color::Reset), "branch cell {x}");
    }
}

#[test]
fn a_detached_branch_shows_its_prefix() {
    let app = two(20.0, json!({"git": {"branch": null}}));
    assert_eq!(b_row(&rail(&app), 2), "▌ ⎇ detached");
}

#[test]
fn outside_git_the_third_row_stays_empty() {
    let app = two(20.0, json!({}));
    let row = b_row(&rail(&app), 2);
    assert!(!row.contains('⎇'), "{row:?}");
}

/// The context bar's empty cells are ░ in `rule`
/// (`docs/tui.md`, "The rail").
#[test]
fn the_bar_track_is_the_rule_colour() {
    let app = two(15.0, json!({"context": {"tokens": 50, "window": 100}}));
    let buf = rail(&app);
    // B's fourth text row is the rail's twelfth.
    let track: Vec<_> = (0..buf.area.width)
        .filter_map(|x| buf.cell((x, 11)))
        .filter(|cell| cell.symbol() == "░")
        .collect();
    assert!(!track.is_empty());
    for cell in track {
        assert_eq!(cell.style().fg, Some(Role::Rule.color()));
    }
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
    let mut app = two(
        20.0,
        json!({"started": 0, "calls": 0, "since": WALL - 16_000}),
    );
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
            "name": "fix flaky lock test", "started": 0, "calls": 0, "since": WALL - 16_000,
            "state": "waiting", "waiting": {"request_id": "r_1",
                "kind": "approval", "summary": "shell cargo mutants --in-place"},
            "spend": spend(1.35), "context": {"tokens": 700, "window": 1000},
            "git": {"branch": "lock"}}),
    ));
    app.on_line(status(
        A,
        json!({"started": 0, "calls": 0, "since": WALL - 9 * 60_000, "spend": spend(0.41),
            "git": {"branch": "main"}, "context": {"tokens": 119, "window": 1000}}),
    ));
    app.on_line(status(
        C,
        json!({"name": "write the rail", "state": "streaming",
            "started": 0, "calls": 0, "since": WALL - 2 * 3_600_000, "git": {"branch": null}}),
    ));
    app.on_line(status(
        D,
        json!({"workspace": "/Users/you/src/site", "project": "-site",
            "name": "deploy", "started": 0, "calls": 0, "since": WALL - 3 * 86_400_000,
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
        json!({"name": "fix flaky lock test", "started": 0, "calls": 0, "since": WALL - 16_000,
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
    let cards: Vec<(Spot, Rect)> = rail_targets(&targets)
        .into_iter()
        .filter(|(spot, _)| matches!(spot, Spot::Card(_)))
        .collect();
    assert_eq!(cards, expected);
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

/// `count` live sessions scrolled to the end at `height` rows.
fn scrolled_end(count: u64, height: u16) -> App {
    let mut app = app(15.0);
    app.set_size(200, height);
    app.on_line(status(A, json!({})));
    for n in 1..count {
        app.on_line(status(&format!("s_{n:016x}"), json!({})));
    }
    let down = crate::keys::Mouse {
        kind: crate::keys::MouseKind::WheelDown,
        col: 5,
        row: 10,
    };
    for _ in 0..20 {
        app.on_wheel(&down);
    }
    app
}

#[test]
fn scrolled_top_card_keeps_its_target() {
    // Ten cards take 61 rows; at 40 rows the end is scroll 21, three
    // rows into the fourth card's text.
    let app = scrolled_end(10, 40);
    assert_eq!(app.rail_state().scroll(), 21);
    let third = key(&app, "s_0000000000000003");
    let (buf, targets) = hovered(&app, None);
    let rect = Rect::new(0, 0, 29, 3);
    assert!(rail_targets(&targets).contains(&(Spot::Card(third), rect)));
    let shown = lines(&buf);
    assert!(
        shown[0].starts_with("\u{258c} fix the parser"),
        "{}",
        shown[0]
    );
    for y in 0..3 {
        assert!(shown[usize::from(y)].starts_with("\u{258c}"), "row {y}");
        assert!(rect.contains((5, y).into()), "row {y}");
    }
    assert!(!rect.contains((5, 3).into()));
    let (hovered_buf, _) = hovered(&app, Some((5, 1)));
    assert!(text(&hovered_buf).contains("fix the parser"));
}

#[test]
fn card_ending_at_scroll_has_no_target() {
    // At 37 rows the end is scroll 24, exactly past the fourth card's
    // text: its last text row is row 23.
    let app = scrolled_end(10, 37);
    assert_eq!(app.rail_state().scroll(), 24);
    let (_, targets) = hovered(&app, None);
    let cards = rail_targets(&targets);
    assert!(
        !cards
            .iter()
            .any(|(spot, _)| *spot == Spot::Card(key(&app, "s_0000000000000003")))
    );
    assert_eq!(
        cards
            .iter()
            .find(|(spot, _)| *spot == Spot::Card(key(&app, "s_0000000000000004"))),
        Some(&(
            Spot::Card(key(&app, "s_0000000000000004")),
            Rect::new(0, 2, 29, 4)
        ))
    );
}

#[test]
fn one_visible_row_keeps_a_one_row_target() {
    // At 38 rows the end is scroll 23, leaving the fourth card's last
    // text row alone.
    let app = scrolled_end(10, 38);
    assert_eq!(app.rail_state().scroll(), 23);
    let third = key(&app, "s_0000000000000003");
    let (buf, targets) = hovered(&app, None);
    let rect = Rect::new(0, 0, 29, 1);
    assert!(rail_targets(&targets).contains(&(Spot::Card(third), rect)));
    let shown = lines(&buf);
    assert!(shown[0].starts_with("\u{258c} $0.00"), "{}", shown[0]);
    assert!(rect.contains((5, 0).into()));
    assert!(!rect.contains((5, 1).into()));
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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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

#[test]
fn targets_cover_each_card_its_cross_and_plus() {
    // `A` and `C` share the launch project; `B` is alone in its own and
    // crashed. A 40-column rail's text ends on column 37.
    let mut app = app(20.0);
    app.on_line(status(A, json!({})));
    app.on_line(status(
        B,
        json!({"workspace": "/Users/you/work/hub", "project": "-hub"}),
    ));
    app.on_line(status(C, json!({})));
    app.on_line(left(B, "crashed"));
    let (a, b, c) = (key(&app, A), key(&app, B), key(&app, C));
    let (_, targets) = hovered(&app, None);
    assert_eq!(
        rail_targets(&targets),
        vec![
            (Spot::Start(a), Rect::new(8, 0, 1, 1)),
            (Spot::Done(a), Rect::new(35, 0, 3, 1)),
            (Spot::Card(a), Rect::new(0, 2, 39, 4)),
            (Spot::Card(c), Rect::new(0, 8, 39, 4)),
            (Spot::Start(b), Rect::new(6, 13, 1, 1)),
            (Spot::Done(b), Rect::new(35, 13, 3, 1)),
            (Spot::Card(b), Rect::new(0, 15, 39, 4)),
            (Spot::Dismiss(b), Rect::new(37, 15, 1, 1)),
        ]
    );
    let lines = lines(&rail(&app));
    assert!(lines[0].starts_with("  fiber +"), "{}", lines[0]);
    assert!(lines[0].ends_with("$0.00 · 0 ✓"), "{}", lines[0]);
    assert!(lines[13].starts_with("  hub +"), "{}", lines[13]);
    assert!(lines[13].ends_with("$0.00 · 0 ✓"), "{}", lines[13]);
    assert!(lines[15].ends_with('✕'), "{}", lines[15]);
    assert_eq!(lines[15].chars().count(), 38, "{}", lines[15]);
}

#[test]
fn rail_crashed_card_has_a_cross() {
    let mut app = two(20.0, json!({"name": "deploy", "spend": spend(2.0)}));
    app.on_line(status(C, json!({})));
    app.on_line(left(B, "crashed"));
    insta::assert_snapshot!("rail_crashed_card_has_a_cross", text(&rail(&app)));
}

#[test]
fn session_card_with_n_waiting() {
    // At 160 columns a 48-column rail does not fit beside the 34-column
    // panel and the conversation's 84.
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/Users/you/work/fiber"),
            project: "-w".to_owned(),
            git: false,
        },
        rail_share: 30.0,
        panel_share: 21.0,
        panel_cards: CARDS.map(str::to_owned).to_vec(),
        ..Default::default()
    });
    app.attach(SessionId(A.to_owned()));
    app.set_size(160, 40);
    app.set_wall(WALL);
    app.on_line(status(A, json!({})));
    let waiting = json!({"state": "waiting", "waiting": {"request_id": "r_1",
        "kind": "approval", "summary": "shell ls"}});
    app.on_line(status(B, waiting.clone()));
    app.on_line(status(C, waiting));
    let area = Rect::new(0, 0, 160, 40);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    assert!(
        app.chrome()
            .layout()
            .is_some_and(|layout| layout.rail.is_none())
    );
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::Panel(crate::app::panel::Spot::Waiting))
    );
    insta::assert_snapshot!("session_card_with_n_waiting", text(&buf));
}

#[test]
fn the_rail_region_is_on_the_background() {
    let app = two(20.0, json!({}));
    let buf = screen(&app);
    let rail = app
        .chrome()
        .layout()
        .and_then(|layout| layout.rail)
        .expect("a rail");
    // Below the cards the rail draws on the background (`docs/tui.md`,
    // "Themes"): no cell of its last row carries the surface tint,
    // while a card's row still does.
    let surface = Role::Surface.color();
    let bottom = rail.bottom().saturating_sub(1);
    for x in rail.left()..rail.right() {
        assert_ne!(buf[(x, bottom)].bg, surface, "column {x}");
    }
    assert!(
        (rail.y..bottom).any(|y| (rail.left()..rail.right()).any(|x| buf[(x, y)].bg == surface)),
        "a card's row keeps its tint"
    );
}

/// The NO_COLOR look: every role paints to the terminal's default.
fn no_color_look() -> crate::look::Look {
    let (look, notice) = crate::look::Look::new(crate::look::ThemeSetting::Dark, &|name: &str| {
        (name == "NO_COLOR").then(|| "1".to_owned())
    });
    assert_eq!(notice, None);
    look
}

/// The rail drawn and painted without colour: the buffer and its targets.
fn no_color_drawn(app: &App) -> (Buffer, Vec<crate::mouse::Target>) {
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.rail)
        .unwrap_or_else(|| panic!("a rail"));
    let area = Rect::new(0, 0, rect.width, rect.height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    draw(app, area, &mut buf, None, &mut targets);
    no_color_look().paint(&mut buf);
    (buf, targets)
}

#[test]
fn no_color_boxes_a_rail_card_and_narrows_its_text() {
    use ratatui::style::{Color, Modifier};
    // C's card crashed, so its ✕ target proves the spots moved with the
    // narrowed body. Two sessions stay live, so the rail draws.
    let mut app = app(15.0);
    app.on_line(status(A, json!({})));
    app.on_line(status(B, json!({})));
    app.on_line(status(C, json!({})));
    app.on_line(left(C, "crashed"));
    let (buf, targets) = no_color_drawn(&app);
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.rail)
        .expect("a rail");
    let card_w = rect.width.saturating_sub(1);
    // C's card: its top edge on row 13, four text rows, its bottom edge.
    let (top, bottom) = (13, 18);
    for x in 0..card_w {
        let corner = buf[(x, top)].symbol();
        assert_eq!(
            corner,
            if x == 0 {
                "┌"
            } else if x == card_w - 1 {
                "┐"
            } else {
                "─"
            }
        );
        let corner = buf[(x, bottom)].symbol();
        assert_eq!(
            corner,
            if x == 0 {
                "└"
            } else if x == card_w - 1 {
                "┘"
            } else {
                "─"
            }
        );
        for y in [top, bottom] {
            let cell = &buf[(x, y)];
            assert_eq!(
                (cell.fg, cell.bg),
                (Color::Reset, Color::Reset),
                "({x}, {y})"
            );
            assert_eq!(cell.modifier, Modifier::DIM, "({x}, {y})");
        }
    }
    // Each body row sits between │ sides, dim, on the default colours,
    // with the card's text readable inside.
    for y in top + 1..bottom {
        for x in [0, card_w - 1] {
            let cell = &buf[(x, y)];
            assert_eq!(cell.symbol(), "│", "({x}, {y})");
            assert_eq!(
                (cell.fg, cell.bg),
                (Color::Reset, Color::Reset),
                "({x}, {y})"
            );
            assert_eq!(cell.modifier, Modifier::DIM, "({x}, {y})");
        }
    }
    let body: String = (1..card_w - 1)
        .map(|x| buf[(x, top + 2)].symbol())
        .collect();
    assert!(body.contains("fix the parser"), "{body:?}");
    // The text room is two columns narrower than with colour: the ✕ sits
    // one column past it, and the header's bold survives the paint.
    let text = usize::from(card_w).saturating_sub(3).saturating_sub(2);
    let dismiss = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::Rail(Spot::Dismiss(_))))
        .expect("the dismiss target");
    assert_eq!(
        dismiss.rect.x,
        u16::try_from(text.saturating_add(1)).expect("a column")
    );
    assert_eq!(dismiss.rect.width, 1);
    let card = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::Rail(Spot::Card(_))))
        .expect("the card target");
    assert_eq!(card.rect.height, 4);
    // Six rows per card, header and start ranges kept.
    let rows = super::rows(&app, rect.width);
    assert_eq!(rows.len() % 6, 1, "one header row and six rows per card");
    let header = &buf[(2, 0)];
    assert!(header.modifier.contains(Modifier::BOLD));
    assert_eq!((header.fg, header.bg), (Color::Reset, Color::Reset));
}

#[test]
fn no_color_keeps_coloured_half_blocks_off_the_rail() {
    // With colour the same card draws half-block edges and a stripe: the
    // box is only the colourless form.
    let app = two(15.0, json!({}));
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.rail)
        .expect("a rail");
    let area = Rect::new(0, 0, rect.width, rect.height);
    let mut buf = Buffer::empty(area);
    draw(&app, area, &mut buf, None, &mut Vec::new());
    assert_eq!(buf[(0, 7)].symbol(), "▄");
    assert_eq!(buf[(0, 8)].symbol(), "▌");
}

#[test]
fn rail_draws_at_tiny_widths_without_colour() {
    let app = two(15.0, json!({}));
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    // Widths too small for two sides plus text keep the text-first
    // fallback: no box, no panic.
    for width in [0, 1, 2, 3, 4] {
        let rows = super::rows(&app, width);
        let area = Rect::new(0, 0, width, 40);
        let mut buf = Buffer::empty(area);
        draw(&app, area, &mut buf, None, &mut Vec::new());
        if width < 3 {
            for row in &rows {
                let text: String = row
                    .line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                assert!(
                    !text.contains('┌') && !text.contains('│'),
                    "width {width}: {text:?}"
                );
            }
        }
    }
}

/// Presses ← through the bindings.
fn press_left(app: &mut App) {
    let now = fakes::clock::FakeClock::new().now();
    app.on_press(
        Stroke {
            code: Code::Left,
            mods: Mods::NONE,
        },
        now,
    );
}

/// The rail drawn with colour, and its targets.
fn drawn(app: &App) -> (Buffer, Vec<Target>) {
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.rail)
        .unwrap_or_else(|| panic!("a rail"));
    let area = Rect::new(0, 0, rect.width, rect.height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    draw(app, area, &mut buf, None, &mut targets);
    (buf, targets)
}

/// The drawn text rect of the focused card `focus`.
fn focused_rect(targets: &[Target], focus: Spot) -> Rect {
    targets
        .iter()
        .find_map(|target| {
            if target.id == TargetId::Rail(focus) {
                Some(target.rect)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("a focused card target"))
}

#[test]
fn a_focused_card_lifts_with_the_hover_tint() {
    use crate::theme::hover_bg;
    let mut app = two(15.0, json!({}));
    let (plain, _) = drawn(&app);
    press_left(&mut app);
    let focus = app.rail_state().focused().expect("focus");
    let (buf, targets) = drawn(&app);
    let rect = focused_rect(&targets, focus);
    let cell = (rect.x.saturating_add(2), rect.y);
    assert_eq!(
        buf[cell].bg,
        hover_bg(plain[cell].bg),
        "the focused card's cell"
    );
    // Its blank top edge stays untinted...
    let edge = (rect.x.saturating_add(2), rect.y.saturating_sub(1));
    assert_eq!(buf[edge].bg, plain[edge].bg, "the top edge");
    // ...and so does the header above and the next card's text row.
    let header = (rect.x.saturating_add(2), rect.y.saturating_sub(2));
    assert_eq!(buf[header].bg, plain[header].bg, "the header");
    let other = targets
        .iter()
        .find_map(|target| {
            if let TargetId::Rail(Spot::Card(_)) = target.id
                && target.id != TargetId::Rail(focus)
            {
                Some(target.rect)
            } else {
                None
            }
        })
        .expect("a second card");
    let neighbour = (other.x.saturating_add(2), other.y);
    assert_eq!(buf[neighbour].bg, plain[neighbour].bg, "the next card");
}

#[test]
fn shift_tab_from_a_focused_rail_lifts_no_card() {
    let mut app = two(15.0, json!({}));
    let (plain, plain_targets) = drawn(&app);
    let card = Spot::Card(key(&app, A));
    let before = focused_rect(&plain_targets, card);
    let cell = (before.x.saturating_add(2), before.y);
    press_left(&mut app);
    assert_eq!(app.rail_state().focused(), Some(card));
    let now = fakes::clock::FakeClock::new().now();
    app.on_press(Stroke::parse("shift+tab").unwrap(), now);
    assert_eq!(app.rail_state().focused(), None);
    let (buf, targets) = drawn(&app);
    let rect = focused_rect(&targets, card);
    let at = (rect.x.saturating_add(2), rect.y);
    assert_eq!(buf[at].bg, plain[cell].bg, "no card stays lifted");
}

#[test]
fn a_focused_card_stays_lifted_while_disconnected() {
    use crate::theme::hover_bg;
    let mut app = two(15.0, json!({}));
    let (plain, _) = drawn(&app);
    press_left(&mut app);
    let focus = app.rail_state().focused().expect("focus");
    app.disconnected();
    let (buf, targets) = drawn(&app);
    let rect = focused_rect(&targets, focus);
    let cell = (rect.x.saturating_add(2), rect.y);
    assert_eq!(
        buf[cell].bg,
        hover_bg(plain[cell].bg),
        "the focused card's cell"
    );
}

#[test]
fn no_color_a_focused_cards_rows_are_bold_and_its_box_loses_dim() {
    use ratatui::style::Modifier;
    let mut app = two(15.0, json!({}));
    press_left(&mut app);
    let focus = app.rail_state().focused().expect("focus");
    let (buf, targets) = no_color_drawn(&app);
    let rect = focused_rect(&targets, focus);
    let card_w = rect.width;
    let right = rect.x.saturating_add(card_w).saturating_sub(1);
    for y in rect.y.saturating_sub(1)..rect.y.saturating_add(5) {
        for x in rect.x..rect.x.saturating_add(card_w) {
            assert!(
                buf[(x, y)].modifier.contains(Modifier::BOLD),
                "bold at ({x}, {y})"
            );
            // The │ sides and the ┌─┐/└─┘ edge rows lose their dim;
            // the text between the sides keeps whatever dim its role
            // paints.
            let edge = y == rect.y.saturating_sub(1) || y == rect.y.saturating_add(4);
            if edge || x == rect.x || x == right {
                assert!(
                    !buf[(x, y)].modifier.contains(Modifier::DIM),
                    "no dim at ({x}, {y})"
                );
            }
        }
    }
}

#[test]
fn no_color_an_unfocused_card_keeps_its_dim_box() {
    use ratatui::style::Modifier;
    let mut app = two(15.0, json!({}));
    press_left(&mut app);
    let focus = app.rail_state().focused().expect("focus");
    let (buf, targets) = no_color_drawn(&app);
    let rect = targets
        .iter()
        .find_map(|target| {
            if let TargetId::Rail(Spot::Card(_)) = target.id
                && target.id != TargetId::Rail(focus)
            {
                Some(target.rect)
            } else {
                None
            }
        })
        .expect("a second card");
    let card_w = rect.width;
    let right = rect.x.saturating_add(card_w).saturating_sub(1);
    for y in [rect.y.saturating_sub(1), rect.y.saturating_add(4)] {
        assert!(
            buf[(rect.x, y)].modifier.contains(Modifier::DIM),
            "dim edge at ({}, {y})",
            rect.x
        );
    }
    for y in rect.y..rect.y.saturating_add(4) {
        for x in [rect.x, right] {
            assert!(
                buf[(x, y)].modifier.contains(Modifier::DIM),
                "dim side at ({x}, {y})"
            );
            assert!(
                !buf[(x, y)].modifier.contains(Modifier::BOLD),
                "no bold at ({x}, {y})"
            );
        }
    }
}

/// Presses `code` through the bindings.
fn press(app: &mut App, code: Code) {
    let now = fakes::clock::FakeClock::new().now();
    app.on_press(
        Stroke {
            code,
            mods: Mods::NONE,
        },
        now,
    );
}

#[test]
fn no_color_a_focused_plus_and_cross_go_bold() {
    use ratatui::style::Modifier;
    let mut app = two(15.0, json!({}));
    app.on_line(left(B, "crashed"));
    press_left(&mut app);
    press(&mut app, Code::Up);
    assert_eq!(app.rail_state().focused(), Some(Spot::Done(key(&app, A))));
    press(&mut app, Code::Up);
    assert_eq!(app.rail_state().focused(), Some(Spot::Start(key(&app, A))));
    let (buf, targets) = no_color_drawn(&app);
    let plus = targets
        .iter()
        .find(|target| target.id == TargetId::Rail(Spot::Start(key(&app, A))))
        .map(|target| target.rect)
        .expect("the plus target");
    assert!(
        buf[(plus.x, plus.y)].modifier.contains(Modifier::BOLD),
        "the focused plus goes bold"
    );
    press(&mut app, Code::Down);
    press(&mut app, Code::Down);
    press(&mut app, Code::Down);
    press(&mut app, Code::Down);
    assert_eq!(
        app.rail_state().focused(),
        Some(Spot::Dismiss(key(&app, B)))
    );
    let (buf, targets) = no_color_drawn(&app);
    let cross = targets
        .iter()
        .find(|target| target.id == TargetId::Rail(Spot::Dismiss(key(&app, B))))
        .map(|target| target.rect)
        .expect("the cross target");
    assert!(
        buf[(cross.x, cross.y)].modifier.contains(Modifier::BOLD),
        "the focused cross goes bold"
    );
}

#[test]
fn no_color_no_cell_carries_reversed() {
    use ratatui::style::Modifier;
    let mut app = two(15.0, json!({}));
    press_left(&mut app);
    let (buf, _) = no_color_drawn(&app);
    assert!(
        buf.content
            .iter()
            .all(|cell| !cell.modifier.contains(Modifier::REVERSED)),
        "no cell reversed"
    );
}

#[test]
fn no_color_the_dim_branch_and_usage_text_stays_dim() {
    use ratatui::style::Modifier;
    let mut app = app(15.0);
    app.on_line(status(
        A,
        json!({"git": {"branch": "main"}, "context": {"tokens": 50, "window": 100}}),
    ));
    app.on_line(status(B, json!({})));
    let (plain, _) = no_color_drawn(&app);
    press_left(&mut app);
    let focus = app.rail_state().focused().expect("focus");
    assert_eq!(focus, Spot::Card(key(&app, A)));
    let (buf, targets) = no_color_drawn(&app);
    let rect = focused_rect(&targets, focus);
    let right = rect.x.saturating_add(rect.width).saturating_sub(1);
    let mut dim = 0;
    for y in rect.y..rect.y.saturating_add(4) {
        for x in rect.x.saturating_add(1)..right {
            if plain[(x, y)].modifier.contains(Modifier::DIM) {
                assert!(
                    buf[(x, y)].modifier.contains(Modifier::DIM),
                    "the text at ({x}, {y}) stays dim"
                );
                dim += 1;
            }
        }
    }
    assert!(dim > 0, "dim text in the focused card");
}

#[test]
fn no_color_a_scrolled_focused_card_lifts_on_its_own_rows() {
    use ratatui::style::Modifier;
    let mut app = many(10);
    let down = crate::keys::Mouse {
        kind: crate::keys::MouseKind::WheelDown,
        col: 5,
        row: 10,
    };
    app.on_wheel(&down);
    app.on_wheel(&down);
    assert_eq!(app.rail_state().scroll(), 7);
    press_left(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..2 {
        app.on_press(
            Stroke {
                code: Code::Down,
                mods: Mods::NONE,
            },
            now,
        );
    }
    assert!(app.rail_state().scroll() > 0, "the rail is scrolled");
    let focus = app.rail_state().focused().expect("focus");
    let (buf, targets) = no_color_drawn(&app);
    let rect = focused_rect(&targets, focus);
    assert!(rect.y >= 1, "the card's box top is drawn: {rect:?}");
    let right = rect.x.saturating_add(rect.width).saturating_sub(1);
    for y in rect.y.saturating_sub(1)..rect.y.saturating_add(5) {
        for x in rect.x..rect.x.saturating_add(rect.width) {
            assert!(
                buf[(x, y)].modifier.contains(Modifier::BOLD),
                "bold at ({x}, {y})"
            );
            let edge = y == rect.y.saturating_sub(1) || y == rect.y.saturating_add(4);
            if edge || x == rect.x || x == right {
                assert!(
                    !buf[(x, y)].modifier.contains(Modifier::DIM),
                    "no dim at ({x}, {y})"
                );
            }
        }
    }
}

/// A drag across a rail card's name row copies the name: the stripe,
/// the box side and the gap are decoration and never go into the
/// copy.
#[test]
fn a_drag_on_a_rail_card_copies_its_text_without_the_stripe() {
    use super::super::{render, text};
    use crate::app::Effect;
    use crate::keys::{Button, Mouse, MouseKind};
    use crate::mouse::Pointer;
    use contract::clock::Clock;
    let mut app = two(20.0, json!({}));
    let area = Rect::new(0, 0, 200, 40);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let shown = text(&buf);
    let byte = shown.find("fix the parser").expect("the card name shows");
    let row = shown[..byte].matches('\n').count();
    let line_start = shown[..byte].rfind('\n').map_or(0, |at| at + 1);
    let col = shown[line_start..byte].chars().count();
    let col = u16::try_from(col).expect("a column");
    let row = u16::try_from(row).expect("a row");
    let now = fakes::clock::FakeClock::new().now();
    let mut pointer = Pointer::default();
    let mut report = |kind: MouseKind, col: u16, row: u16| {
        let mut buf = Buffer::empty(area);
        let targets = render(&app, area, &mut buf, None);
        app.on_mouse(&Mouse { kind, col, row }, &targets, &mut pointer, true, now)
    };
    let _ = report(MouseKind::Press(Button::Left), col, row);
    let _ = report(MouseKind::Drag(Button::Left), col + 13, row);
    let effect = report(MouseKind::Release, col + 13, row);
    assert_eq!(effect, Effect::Copy("fix the parser".to_owned()));
}

/// A card's own trailing bar copies: without a box no side is drawn,
/// so a bar the text itself shows is text, not decoration
/// (`docs/tui.md`, "Selection and copy").
#[test]
fn an_unboxed_row_keeps_a_trailing_bar() {
    use super::super::{render, text};
    use crate::app::Effect;
    use crate::keys::{Button, Mouse, MouseKind};
    use crate::mouse::Pointer;
    use contract::clock::Clock;
    struct Unboxed;
    impl Drop for Unboxed {
        fn drop(&mut self) {
            crate::surface::init(&|_: &str| None);
        }
    }
    let _guard = Unboxed;
    crate::surface::init(&|_: &str| None);
    assert!(!crate::surface::boxed(), "no box, so no side to strip");
    let mut app = two(20.0, json!({"name": "tail│"}));
    let area = Rect::new(0, 0, 200, 40);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let shown = text(&buf);
    let byte = shown.find("tail│").expect("the card name shows");
    let row = shown[..byte].matches('\n').count();
    let line_start = shown[..byte].rfind('\n').map_or(0, |at| at + 1);
    let col = u16::try_from(crate::format::width(&shown[line_start..byte])).expect("a column");
    let row = u16::try_from(row).expect("a row");
    let now = fakes::clock::FakeClock::new().now();
    let mut pointer = Pointer::default();
    let mut report = |kind: MouseKind, col: u16, row: u16| {
        let mut buf = Buffer::empty(area);
        let targets = render(&app, area, &mut buf, None);
        app.on_mouse(&Mouse { kind, col, row }, &targets, &mut pointer, true, now)
    };
    let _ = report(MouseKind::Press(Button::Left), col, row);
    let _ = report(MouseKind::Drag(Button::Left), col + 4, row);
    let effect = report(MouseKind::Release, col + 4, row);
    assert_eq!(effect, Effect::Copy("tail│".to_owned()));
}
