//! Scoped `recent` paging: the fresh-project trigger, the fan-out bound,
//! the paging guards, and the `More` count. Each test builds its state
//! purely through `on_line` messages and asserts the outgoing lines (or
//! the copied text) for a case that differs between the original and one
//! of the survived mutants in `super` (`crates/tui/src/app/home.rs`).

use super::{App, Effect};
use crate::app::testkit::hello;
use crate::home::{ListLine, Spot};
use crate::keys::Key;
use crate::link::Line;
use contract::SessionId;
use contract::clock::Clock;
use serde_json::{Value, json};
use std::path::PathBuf;

/// An app on home at 80x24.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(crate::home::Launch {
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
    app.set_size(80, 24);
    app
}

/// A hub `command_accepted` for `id` with `result`.
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

/// A hub `command_rejected` for `id` with `message`.
fn refused(id: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            (
                "code".to_owned(),
                Value::String("invalid_arguments".to_owned()),
            ),
            ("message".to_owned(), Value::String(message.to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// A `session_status` for `session`, idle and named.
fn status(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({
            "name": "fix the parser",
            "workspace": "/w",
            "project": "-w",
            "state": "idle",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// One exited recent row in `workspace` and `project`.
fn exited_in(session: &str, name: &str, workspace: &str, project: &str) -> Value {
    json!({
        "session_id": session,
        "ts": 0,
        "project": project,
        "workspace": workspace,
        "name": name,
        "how": "exited",
    })
}

/// Parses outgoing command lines.
fn commands(lines: Vec<String>) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// Answers `id` with `result`: the outgoing lines, parsed.
fn answer_value(app: &mut App, id: &str, result: Value) -> Vec<Value> {
    app.on_line(accepted(id, result))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// Links the app: the feed and recent ids, both waiting for their
/// answers. An open picker's listing goes out beside them.
fn linked(app: &mut App) -> (String, String) {
    let lines: Vec<Value> = commands(app.on_line(hello()))
        .into_iter()
        .filter(|line| line["command"] != "directories")
        .collect();
    assert_eq!(lines.len(), 2);
    (
        lines[0]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("feed id"))
            .to_owned(),
        lines[1]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("recent id"))
            .to_owned(),
    )
}

/// The `project` arg of each outgoing `recent` line, in order.
fn fanout_projects(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .map(|line| {
            // `docs/invocation.md:1102` (each fanout line is a scoped `recent`).
            assert_eq!(line["command"], "recent");
            line["args"]["project"]
                .as_str()
                .unwrap_or_else(|| panic!("a scoped ask: {line}"))
        })
        .collect()
}

/// The outgoing line's id.
fn line_id(line: &Value) -> &str {
    line["id"]
        .as_str()
        .unwrap_or_else(|| panic!("an id: {line}"))
}

/// The stored scoped total for `project`.
fn stored(app: &App, project: &str) -> Option<usize> {
    app.home
        .as_ref()
        .and_then(|home| home.sessions.total_for(project))
}

/// Every listed row's line, drawn or not: what the feed and `recent` folded.
fn listed(app: &App) -> Vec<String> {
    app.home
        .as_ref()
        .map(|home| {
            home.sessions
                .all()
                .iter()
                .map(|row| crate::home::line(row, home.launch_project()))
                .collect()
        })
        .unwrap_or_default()
}

/// The keys of the rows home draws, in order.
fn keys(app: &App) -> Vec<u64> {
    app.home_screen()
        .map(|screen| {
            screen
                .list
                .into_iter()
                .filter_map(|line| match line {
                    ListLine::Live { key, .. } | ListLine::Exited { key, .. } => Some(key),
                    ListLine::Head { .. } | ListLine::ExitedHead { .. } | ListLine::More { .. } => {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The row key of `session` on home.
fn row_key(app: &App, session: &str) -> u64 {
    app.home
        .as_ref()
        .and_then(|home| home.sessions.row(&SessionId(session.to_owned())))
        .map(|row| row.key)
        .unwrap_or_else(|| panic!("row {session}"))
}

/// The drawn `More` row's key for `project`.
fn more_key(app: &App, project: &str) -> Option<u64> {
    let home = app.home.as_ref()?;
    app.home_screen()?
        .list
        .into_iter()
        .find_map(|line| match line {
            ListLine::More { key, .. } => {
                (home.sessions.by_key(key)?.project == project).then_some(key)
            }
            ListLine::Head { .. }
            | ListLine::Live { .. }
            | ListLine::ExitedHead { .. }
            | ListLine::Exited { .. } => None,
        })
}

/// Renders home at 80x24 and takes its targets, so keys move among drawn
/// stops.
fn drawn(app: &mut App) {
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    app.drawn(&targets);
}

/// Focuses the last list stop from the entry bar.
fn focus_last(app: &mut App) {
    drawn(app);
    let last = keys(app)
        .into_iter()
        .last()
        .unwrap_or_else(|| panic!("a row"));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(last)))
    );
}

/// The `(section head, exited head, more)` counts in draw order: the
/// section's workspace tail, its exited header count, and its More
/// count, skipping rows.
fn list_shapes(app: &App) -> Vec<String> {
    app.home_screen()
        .map(|screen| {
            screen
                .list
                .into_iter()
                .map(|line| match line {
                    ListLine::Head { name, .. } => format!("section {name}"),
                    ListLine::ExitedHead { count } => format!("head {count}"),
                    ListLine::More { count, .. } => format!("more {count}"),
                    ListLine::Exited { .. } | ListLine::Live { .. } => "row".to_owned(),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_repeat_status_for_a_known_project_queues_no_scoped_ask() {
    let mut app = home();
    app.on_line(hello());
    // The first status introduces the project: one scoped ask goes out
    // (`docs/tui.md:268`: one ask in flight per project).
    let queued = commands(app.on_line(status("s_aaaaaaaaaaaaaaaa")));
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["args"]["project"], "-w");
    let first = line_id(&queued[0]).to_owned();
    // Its refusal clears the in-flight ask without storing a total, so
    // the project is known with no ask in flight.
    assert!(app.on_line(refused(&first, "no recent")).is_empty());
    assert_eq!(stored(&app, "-w"), None);
    assert_eq!(listed(&app), ["✓  fix the parser"]);
    assert_eq!(app.notice(), Some("no recent"));
    // A repeat status for the known project queues nothing: the fresh
    // test needs an unknown project. With `||` the known project reads
    // fresh and queues one; with `!=` the single section reads unknown
    // and queues one.
    assert!(app.on_line(status("s_aaaaaaaaaaaaaaaa")).is_empty());
}

#[test]
fn a_fan_out_for_a_new_project_ignores_an_in_flight_ask_for_another_project() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    // One project on home with its scoped ask still in flight.
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    assert_eq!(fanout_projects(&fanout), ["-w"]);
    // The next unscoped page introduces `-other` while `-w` is in
    // flight: the fan-out still asks the new project. With `!=` the
    // `-w` ask reads as one for `-other` and the new project is
    // skipped.
    focus_last(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(page) = app.on_key(Key::Down, now) else {
        panic!("the last row asks the next page");
    };
    let page: Value = serde_json::from_str(&page[0]).unwrap_or_else(|err| panic!("{err}"));
    let page_id = page["id"].as_str().unwrap_or_else(|| panic!("page id"));
    let lines = answer_value(
        &mut app,
        page_id,
        json!({"sessions": [exited_in("s_bbbbbbbbbbbbbbbb", "b", "/other", "-other")]}),
    );
    // `docs/tui.md:268` (one ask per project; request order is open).
    assert_eq!(fanout_projects(&lines), ["-other"]);
    // Both projects' rows folded; request order is open, so assert the
    // rows as a set.
    let mut rows = listed(&app);
    rows.sort_unstable();
    assert_eq!(rows, ["○  a", "○  b  other"]);
}

#[test]
fn more_with_the_link_down_sends_no_scoped_page() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    answer_value(
        &mut app,
        line_id(&fanout[0]),
        json!({"sessions": [], "total": 3}),
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(3));
    // `docs/tui.md:268` (More counts from the scoped total).
    let key = more_key(&app, "-w").expect("a more row");
    assert_eq!(app.home_text(Spot::More(key)).as_deref(), Some("… 2 more"));
    // The drop keeps the held total: paging now must send nothing. With
    // `&&` the down link reads as up for a named project and a page
    // goes out.
    app.disconnected();
    assert_eq!(stored(&app, "-w"), Some(3));
    assert_eq!(app.home_click(Spot::More(key)), Effect::None);
}

#[test]
fn a_second_page_while_one_is_in_flight_sends_nothing() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    answer_value(
        &mut app,
        line_id(&fanout[0]),
        json!({"sessions": [], "total": 3}),
    );
    let key = more_key(&app, "-w").expect("a more row");
    // The first page goes out past the last loaded row
    // (`docs/tui.md:268`: opening it pages the project's `recent`).
    let Effect::Send(first) = app.home_click(Spot::More(key)) else {
        panic!("More opens the next scoped page");
    };
    assert_eq!(first.len(), 1);
    let page: Value = serde_json::from_str(&first[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(page["command"], "recent");
    assert_eq!(page["args"]["project"], "-w");
    assert_eq!(page["args"]["before"], "s_aaaaaaaaaaaaaaaa");
    // While that page is in flight a second one sends nothing: paging is
    // not a re-trigger. With `!=` the `-w` ask reads as one for another
    // project and a duplicate goes out.
    assert_eq!(app.home_click(Spot::More(key)), Effect::None);
}

#[test]
fn page_recent_scopes_to_the_focused_project_not_its_neighbor() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w"),
            exited_in("s_bbbbbbbbbbbbbbbb", "b", "/other", "-other"),
        ]}),
    );
    for (project, total) in [("-w", 3), ("-other", 1)] {
        let id = fanout
            .iter()
            .find(|line| line["args"]["project"] == project)
            .map(line_id)
            .unwrap_or_else(|| panic!("the {project} scoped ask"))
            .to_owned();
        assert!(
            app.on_line(accepted(&id, json!({"sessions": [], "total": total})))
                .is_empty()
        );
    }
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(3));
    assert_eq!(stored(&app, "-other"), Some(1));
    // The focused row is `-w`'s last loaded row and `-w` holds more, so
    // the page scopes to `-w`. With `!=` the lookup resolves `-other`'s
    // section, whose last row is not focused, and nothing scoped goes
    // out. ↓ would land on the drawn `More` row first, so the test calls
    // the paging path for the last row directly; the state around it
    // still arrives purely by messages.
    let key = row_key(&app, "s_aaaaaaaaaaaaaaaa");
    let Some(Effect::Send(page)) = app.page_recent(key) else {
        panic!("the last row pages its project");
    };
    assert_eq!(page.len(), 1);
    let page: Value = serde_json::from_str(&page[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(page["command"], "recent");
    assert_eq!(page["args"]["project"], "-w");
    assert_eq!(page["args"]["before"], "s_aaaaaaaaaaaaaaaa");
}

#[test]
fn page_recent_from_the_last_row_with_more_held_scopes_the_page() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    answer_value(
        &mut app,
        line_id(&fanout[0]),
        json!({"sessions": [], "total": 2}),
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(2));
    // One row loaded of two held: the last row pages its project. With
    // `!=` the focused row reads as not-the-last and the page stays
    // unscoped; with `<` or `==` the held total reads as no more and the
    // page stays unscoped too. ↓ would land on the drawn `More` row
    // first, so the test calls the paging path directly.
    let key = row_key(&app, "s_aaaaaaaaaaaaaaaa");
    let Some(Effect::Send(page)) = app.page_recent(key) else {
        panic!("the last row pages its project");
    };
    assert_eq!(page.len(), 1);
    let page: Value = serde_json::from_str(&page[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(page["command"], "recent");
    assert_eq!(page["args"]["project"], "-w");
    assert_eq!(page["args"]["before"], "s_aaaaaaaaaaaaaaaa");
}

#[test]
fn page_recent_off_the_last_row_sends_no_scoped_page() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w"),
            exited_in("s_bbbbbbbbbbbbbbbb", "b", "/w", "-w"),
        ]}),
    );
    answer_value(
        &mut app,
        line_id(&fanout[0]),
        json!({"sessions": [], "total": 3}),
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(3));
    // The first row is not the project's last loaded row, so no scoped
    // page goes out. With `!=` the row reads as the last one, and with
    // `||` either half suffices, so both send a scoped page.
    assert_eq!(app.page_recent(row_key(&app, "s_aaaaaaaaaaaaaaaa")), None);
    // The last row still pages its project: the feature ran.
    let Some(Effect::Send(page)) = app.page_recent(row_key(&app, "s_bbbbbbbbbbbbbbbb")) else {
        panic!("the last row pages its project");
    };
    let page: Value = serde_json::from_str(&page[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(page["args"]["project"], "-w");
    assert_eq!(page["args"]["before"], "s_bbbbbbbbbbbbbbbb");
}

#[test]
fn page_recent_at_the_total_pages_the_unscoped_cursor() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    answer_value(
        &mut app,
        line_id(&fanout[0]),
        json!({"sessions": [], "total": 1}),
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(1));
    // Loaded rows meet the held total: the next page stays unscoped past
    // the unscoped cursor. With `||`, `>=` or `==` the met total reads
    // as more and a scoped page goes out instead.
    focus_last(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(page) = app.on_key(Key::Down, now) else {
        panic!("the last row asks the next page");
    };
    let page: Value = serde_json::from_str(&page[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(page["command"], "recent");
    assert_eq!(page["args"]["before"], "s_aaaaaaaaaaaaaaaa");
    assert!(page["args"].get("project").is_none());
}

#[test]
fn page_recent_below_the_total_pages_the_unscoped_cursor() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w"),
            exited_in("s_bbbbbbbbbbbbbbbb", "b", "/w", "-w"),
        ]}),
    );
    answer_value(
        &mut app,
        line_id(&fanout[0]),
        json!({"sessions": [], "total": 1}),
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(1));
    // `docs/tui.md:254-255` (newest first) and `docs/tui.md:233-234` (sections order).
    assert_eq!(listed(&app), ["○  a", "○  b"]);
    // The held total fell below the loaded rows: the next page stays
    // unscoped past the unscoped cursor. With `<` the fallen total reads
    // as more, and with `||` either half suffices, so both send a scoped
    // page instead.
    focus_last(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(page) = app.on_key(Key::Down, now) else {
        panic!("the last row asks the next page");
    };
    let page: Value = serde_json::from_str(&page[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(page["command"], "recent");
    assert_eq!(page["args"]["before"], "s_bbbbbbbbbbbbbbbb");
    assert!(page["args"].get("project").is_none());
}

#[test]
fn more_text_at_the_total_copies_nothing() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w"),
            exited_in("s_bbbbbbbbbbbbbbbb", "b", "/w", "-w"),
        ]}),
    );
    answer_value(
        &mut app,
        line_id(&fanout[0]),
        json!({"sessions": [], "total": 2}),
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(2));
    // The rows tie on recency, so no `docs/<area>.md` line promises
    // their order: assert the rows as a set.
    let mut rows = listed(&app);
    rows.sort_unstable();
    assert_eq!(rows, ["○  a", "○  b"]);
    // `docs/tui.md:254-255` (More when rows remain) and `268` (More counts from the scoped total).
    assert_eq!(list_shapes(&app), ["section w", "head 2", "row", "row"]);
    // Loaded rows meet the held total: the `More` text is nothing. With
    // `>=` the met total reads as more and copies "… 0 more".
    let key = row_key(&app, "s_bbbbbbbbbbbbbbbb");
    assert_eq!(app.home_text(Spot::More(key)), None);
    // A row's own text still copies: the feature ran.
    assert!(app.home_text(Spot::Entry(key)).is_some());
}
