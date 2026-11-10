//! Scoped `recent` paging: the fresh-project trigger, the fan-out bound,
//! the paging guards, and the `More` count. Each test builds its state
//! purely through `on_line` messages and asserts the outgoing lines (or
//! the copied text) for a case that differs between the original and one
//! of the survived mutants in `super` (`crates/tui/src/app/home.rs`).

use super::{App, Effect};
use crate::app::testkit::{hello, home_with};
use crate::home::{Launch, ListLine, Spot};
use crate::keys::Key;
use crate::link::Line;
use contract::SessionId;
use contract::clock::Clock;
use contract::{HubEventKind, events::EventKind};
use serde_json::{Value, json};
use std::path::PathBuf;

/// The launch home opens: local /w with the model chip set.
fn launch() -> Launch {
    crate::home::Launch {
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
    }
}

/// A hub `command_accepted` for `id` with `result`.
fn accepted(id: &str, result: Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: HubEventKind::CommandAccepted,
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
        kind: HubEventKind::CommandRejected,
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

/// A hub `session_left` for `session`, ending `how`.
fn left(session: &str, how: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: HubEventKind::SessionLeft,
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"session_id": session, "how": how})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// A `session_status` for `session`, idle and named.
fn status(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: EventKind::SessionStatus,
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

/// Whether `line` is an archived scoped `recent` ask.
fn is_archived(line: &Value) -> bool {
    line["command"] == "recent"
        && line["args"]
            .get("archived")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// The `project` arg of each outgoing scoped (not archived) `recent`
/// line, in order.
fn fanout_projects(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| !is_archived(line))
        .map(|line| {
            // `docs/invocation.md`, "What the hub speaks" (each fanout line is a scoped `recent`; the archived asks ride beside these).
            assert_eq!(line["command"], "recent");
            line["args"]["project"]
                .as_str()
                .unwrap_or_else(|| panic!("a scoped ask: {line}"))
        })
        .collect()
}

/// The `project` of each outgoing archived scoped `recent` line, in
/// order.
fn archived_projects(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| is_archived(line))
        .map(|line| {
            // `docs/invocation.md`, "The hub" (the archived `recent` carries `archived` with its project).
            line["args"]["project"]
                .as_str()
                .unwrap_or_else(|| panic!("a scoped archived ask: {line}"))
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
    app.home.sessions.total_for(project)
}

/// The stored archived scoped total for `project`.
fn archived_stored(app: &App, project: &str) -> Option<usize> {
    app.home.sessions.archived_total_for(project)
}

/// Every listed row's line, drawn or not: what the feed and `recent` folded.
fn listed(app: &App) -> Vec<String> {
    app.home
        .sessions
        .all()
        .iter()
        .map(|row| crate::home::line(row, app.home.launch_project()))
        .collect()
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
        .sessions
        .row(&SessionId(session.to_owned()))
        .map(|row| row.key)
        .unwrap_or_else(|| panic!("row {session}"))
}

/// The drawn `More` row's key for `project`.
fn more_key(app: &App, project: &str) -> Option<u64> {
    let home = &app.home;
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
    let mut app = home_with(80, 24, launch());
    app.on_line(hello());
    // The first status introduces the project: one scoped ask of each
    // kind goes out (`docs/tui.md`, "The session list": one ask of each
    // in flight per project).
    let queued = commands(app.on_line(status("s_aaaaaaaaaaaaaaaa")));
    assert_eq!(queued.len(), 2);
    assert_eq!(fanout_projects(&queued), ["-w"]);
    assert_eq!(archived_projects(&queued), ["-w"]);
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
    let mut app = home_with(80, 24, launch());
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
    // `docs/tui.md`, "The session list" (one ask per project; request order is open).
    assert_eq!(fanout_projects(&lines), ["-other"]);
    // Both projects' rows folded; request order is open, so assert the
    // rows as a set.
    let mut rows = listed(&app);
    rows.sort_unstable();
    assert_eq!(rows, ["○  a", "○  b  other"]);
}

#[test]
fn more_with_the_link_down_sends_no_scoped_page() {
    let mut app = home_with(80, 24, launch());
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
    // `docs/invocation.md`, "What the hub speaks" (recent total) and `docs/tui.md`, "The session list" (counts from total).
    assert_eq!(stored(&app, "-w"), Some(3));
    // `docs/tui.md`, "The session list" (More counts from the scoped total).
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
    let mut app = home_with(80, 24, launch());
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
    // (`docs/tui.md`, "The session list": opening it pages the project's `recent`).
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
    let mut app = home_with(80, 24, launch());
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
            .filter(|line| !is_archived(line))
            .find(|line| line["args"]["project"] == project)
            .map(line_id)
            .unwrap_or_else(|| panic!("the {project} scoped ask"))
            .to_owned();
        assert!(
            app.on_line(accepted(&id, json!({"sessions": [], "total": total})))
                .is_empty()
        );
    }
    // `docs/invocation.md`, "What the hub speaks" (recent total) and `docs/tui.md`, "The session list" (counts from total).
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
    let mut app = home_with(80, 24, launch());
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
    // `docs/invocation.md`, "What the hub speaks" (recent total) and `docs/tui.md`, "The session list" (counts from total).
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
    let mut app = home_with(80, 24, launch());
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
    // `docs/invocation.md`, "What the hub speaks" (recent total) and `docs/tui.md`, "The session list" (counts from total).
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
    let mut app = home_with(80, 24, launch());
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
    // `docs/invocation.md`, "What the hub speaks" (recent total) and `docs/tui.md`, "The session list" (counts from total).
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
    let mut app = home_with(80, 24, launch());
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
    // `docs/invocation.md`, "What the hub speaks" (recent total) and `docs/tui.md`, "The session list" (counts from total).
    assert_eq!(stored(&app, "-w"), Some(1));
    // `docs/tui.md`, "The session list" (newest first) and `docs/tui.md`, "The session list" (sections order).
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
    let mut app = home_with(80, 24, launch());
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
    // `docs/invocation.md`, "What the hub speaks" (recent total) and `docs/tui.md`, "The session list" (counts from total).
    assert_eq!(stored(&app, "-w"), Some(2));
    // The rows tie on recency, so no `docs/<area>.md` line promises
    // their order: assert the rows as a set.
    let mut rows = listed(&app);
    rows.sort_unstable();
    assert_eq!(rows, ["○  a", "○  b"]);
    // `docs/tui.md`, "The session list" (More when rows remain, More counts from the scoped total).
    assert_eq!(list_shapes(&app), ["section w", "head 2", "row", "row"]);
    // Loaded rows meet the held total: the `More` text is nothing. With
    // `>=` the met total reads as more and copies "… 0 more".
    let key = row_key(&app, "s_bbbbbbbbbbbbbbbb");
    assert_eq!(app.home_text(Spot::More(key)), None);
    // A row's own text still copies: the feature ran.
    assert!(app.home_text(Spot::Entry(key)).is_some());
}

#[test]
fn page_recent_keeps_the_unscoped_path_with_the_link_down() {
    // The scoped half: one row loaded of two held, so the last row pages
    // its project while the link is up.
    let mut scoped = home_with(80, 24, launch());
    let (_, recent) = linked(&mut scoped);
    let fanout = answer_value(
        &mut scoped,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    answer_value(
        &mut scoped,
        line_id(&fanout[0]),
        json!({"sessions": [], "total": 2}),
    );
    let key = row_key(&scoped, "s_aaaaaaaaaaaaaaaa");
    let Some(Effect::Send(while_up)) = scoped.page_recent(key) else {
        panic!("the last row pages its project while up");
    };
    assert_eq!(while_up.len(), 1);
    // The drop keeps the held total: the scoped page now sends nothing.
    // With no link check on the scoped branch a page goes out.
    scoped.disconnected();
    assert_eq!(scoped.page_recent(key), None);
    // The unscoped half: loaded rows meet the held total, so the next
    // page stays unscoped past the unscoped cursor.
    let mut unscoped = home_with(80, 24, launch());
    let (_, recent) = linked(&mut unscoped);
    let fanout = answer_value(
        &mut unscoped,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    answer_value(
        &mut unscoped,
        line_id(&fanout[0]),
        json!({"sessions": [], "total": 1}),
    );
    let key = row_key(&unscoped, "s_aaaaaaaaaaaaaaaa");
    // The drop keeps the unscoped cursor: the unscoped page still goes
    // out with the link down. With a global offline guard this is `None`.
    unscoped.disconnected();
    let Some(Effect::Send(page)) = unscoped.page_recent(key) else {
        panic!("the unscoped page still goes out while down");
    };
    assert_eq!(page.len(), 1);
    let page: Value = serde_json::from_str(&page[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(page["command"], "recent");
    assert!(page["args"].get("project").is_none());
}

#[test]
fn scoped_first_needs_a_project_and_the_link() {
    let mut app = home_with(80, 24, launch());
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
    // An empty project sends nothing while the link is up: with `&&`
    // the empty-but-up ask reads as linked and a line goes out.
    assert!(app.scoped_first("").is_empty());
    // A named project sends nothing with the link down: with `&&` the
    // named-but-down ask reads as linked and a line goes out. The
    // request waits for the reconnect instead.
    app.disconnected();
    assert!(app.scoped_first("-w").is_empty());
    // The reconnect refetches the project on home: the link-down
    // behavior held the ask instead of sending it.
    let lines = commands(app.on_line(hello()));
    let scoped: Vec<Value> = lines
        .iter()
        .filter(|line| {
            line.get("args")
                .is_some_and(|args| args.get("project").is_some() && args.get("archived").is_none())
        })
        .cloned()
        .collect();
    // The reconnect refetches the archived count beside the exited one
    // (`docs/tui.md`, "The session list": one ask of each in flight per
    // project).
    let mut archived = archived_projects(&lines);
    archived.sort_unstable();
    assert_eq!(archived, ["-w"]);
    let mut projects = fanout_projects(&scoped);
    projects.sort_unstable();
    assert_eq!(projects, ["-w"]);
}

/// The archived ask's id in `fanout`.
fn archived_id(fanout: &[Value]) -> String {
    fanout
        .iter()
        .find(|line| is_archived(line))
        .map(line_id)
        .unwrap_or_else(|| panic!("an archived ask: {fanout:?}"))
        .to_owned()
}

#[test]
fn archived_fanout_rides_beside_the_scoped_ask_with_the_flag() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    // `docs/tui.md`, "The session list" (one ask of each in flight per project).
    assert_eq!(fanout_projects(&fanout), ["-w"]);
    assert_eq!(archived_projects(&fanout), ["-w"]);
    let archived = fanout
        .iter()
        .find(|line| is_archived(line))
        .unwrap_or_else(|| panic!("an archived ask: {fanout:?}"));
    // `docs/invocation.md`, "The hub" (the archived `recent` carries `archived` with its project and no cursor).
    assert_eq!(archived["command"], "recent");
    assert_eq!(archived["args"]["project"], "-w");
    assert_eq!(archived["args"]["archived"], true);
    assert!(archived["args"].get("before").is_none());
}

#[test]
fn an_accepted_archived_answer_sets_rows_and_total() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    let id = archived_id(&fanout);
    // The archived answer folds its own rows and total, leaving the
    // exited count untouched until its own answer arrives.
    let lines = answer_value(
        &mut app,
        &id,
        json!({"sessions": [exited_in("s_bbbbbbbbbbbbbbbb", "b", "/w", "-w")], "total": 2}),
    );
    assert!(lines.is_empty());
    // `docs/invocation.md`, "What the hub speaks" (recent total) and
    // `docs/tui.md`, "The session list" (the archived count from that `total`).
    assert_eq!(archived_stored(&app, "-w"), Some(2));
    assert_eq!(stored(&app, "-w"), None);
    // The archived row resolves through the same key lookup an open uses.
    let row = app
        .home
        .sessions
        .row(&SessionId("s_bbbbbbbbbbbbbbbb".to_owned()))
        .unwrap_or_else(|| panic!("the archived row"));
    assert_eq!(row.project, "-w");
}

#[test]
fn an_archived_refusal_keeps_the_held_total_with_one_notice() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    // While both asks are in flight a leave marks both dirty and sends
    // nothing; the refusal then clears the archived dirty flag without
    // a follow-up, storing nothing.
    assert!(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")).is_empty());
    let id = archived_id(&fanout);
    assert!(app.on_line(refused(&id, "no recent")).is_empty());
    assert_eq!(archived_stored(&app, "-w"), None);
    assert_eq!(app.notice(), Some("no recent"));
    // The cleared flag leaves no stuck ask: the next trigger sends a
    // fresh archived ask, which then holds its total.
    let retrigger = commands(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")));
    assert_eq!(archived_projects(&retrigger), ["-w"]);
    let id = archived_id(&retrigger);
    assert!(
        app.on_line(accepted(
            &id,
            json!({"sessions": [exited_in("s_bbbbbbbbbbbbbbbb", "b", "/w", "-w")], "total": 2}),
        ))
        .is_empty()
    );
    assert_eq!(archived_stored(&app, "-w"), Some(2));
    // A refusal now keeps the held total with one notice and queues
    // nothing: deleting the keep-held-total branch reads `None` here.
    let retrigger = commands(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")));
    let id = archived_id(&retrigger);
    assert!(app.on_line(refused(&id, "gone")).is_empty());
    assert_eq!(archived_stored(&app, "-w"), Some(2));
    assert_eq!(app.notice(), Some("gone"));
}

#[test]
fn a_malformed_archived_answer_keeps_the_held_total() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    // No total held: a first answer without one stores nothing, with
    // one notice.
    let id = archived_id(&fanout);
    assert!(app.on_line(accepted(&id, json!({"total": 2}))).is_empty());
    assert_eq!(archived_stored(&app, "-w"), None);
    assert!(app.notice().is_some());
    // Hold a total, then an answer without rows keeps it.
    let retrigger = commands(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")));
    let id = archived_id(&retrigger);
    assert!(
        app.on_line(accepted(
            &id,
            json!({"sessions": [exited_in("s_bbbbbbbbbbbbbbbb", "b", "/w", "-w")], "total": 2}),
        ))
        .is_empty()
    );
    assert_eq!(archived_stored(&app, "-w"), Some(2));
    let retrigger = commands(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")));
    let id = archived_id(&retrigger);
    assert!(
        app.on_line(accepted(
            &id,
            json!({"sessions": [exited_in("s_cccccccccccccccc", "c", "/w", "-w")]}),
        ))
        .is_empty()
    );
    assert_eq!(archived_stored(&app, "-w"), Some(2));
    assert!(app.notice().is_some());
}

#[test]
fn an_archived_trigger_while_in_flight_sends_exactly_one_follow_up() {
    let mut app = home_with(80, 24, launch());
    app.on_line(hello());
    let queued = commands(app.on_line(status("s_aaaaaaaaaaaaaaaa")));
    assert_eq!(queued.len(), 2);
    let archived = queued
        .iter()
        .find(|line| is_archived(line))
        .map(line_id)
        .unwrap_or_else(|| panic!("an archived ask"))
        .to_owned();
    let scoped = queued
        .iter()
        .filter(|line| !is_archived(line))
        .map(line_id)
        .next()
        .unwrap_or_else(|| panic!("a scoped ask"))
        .to_owned();
    // While both asks are in flight the leave marks both dirty and
    // sends nothing.
    assert!(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")).is_empty());
    // The archived answer folds its total and sends exactly one
    // archived follow-up, leaving the exited dirty flag alone.
    let follow = answer_value(&mut app, &archived, json!({"sessions": [], "total": 1}));
    assert_eq!(follow.len(), 1);
    assert_eq!(archived_projects(&follow), ["-w"]);
    assert_eq!(archived_stored(&app, "-w"), Some(1));
    // The exited answer still owes its own follow-up: the archived
    // answer cleared no exited dirty flag, and vice versa.
    let scoped_follow = answer_value(&mut app, &scoped, json!({"sessions": [], "total": 1}));
    assert_eq!(scoped_follow.len(), 1);
    assert_eq!(fanout_projects(&scoped_follow), ["-w"]);
    // With both flags cleared the follow-ups' answers queue nothing.
    let id = line_id(&follow[0]).to_owned();
    assert!(
        app.on_line(accepted(&id, json!({"sessions": [], "total": 1})))
            .is_empty()
    );
}

#[test]
fn archived_first_sends_nothing_without_a_project_or_the_link() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    assert_eq!(archived_projects(&fanout), ["-w"]);
    // An empty project sends nothing while the link is up: with `||`
    // the empty-but-up ask reads as linked and a line goes out.
    assert!(app.archived_first("").is_empty());
    // A named project sends nothing with the link down: with `||` the
    // named-but-down ask reads as linked and a line goes out. The
    // request waits for the reconnect instead.
    app.disconnected();
    assert!(app.archived_first("-w").is_empty());
}

#[test]
fn an_archived_answer_with_the_link_down_queues_no_follow_up() {
    let mut app = home_with(80, 24, launch());
    app.on_line(hello());
    let queued = commands(app.on_line(status("s_aaaaaaaaaaaaaaaa")));
    assert_eq!(queued.len(), 2);
    let archived = archived_id(&queued);
    // The leave marks the archived ask dirty while the link is up.
    assert!(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")).is_empty());
    // The answer lands with the link down: the follow-up stays held,
    // but the rows and total still fold.
    app.disconnected();
    assert!(
        app.on_line(accepted(&archived, json!({"sessions": [], "total": 1})))
            .is_empty()
    );
    assert_eq!(archived_stored(&app, "-w"), Some(1));
}

#[test]
fn ask_recent_first_refetches_the_archived_count_with_the_project() {
    let mut app = home_with(80, 24, launch());
    app.on_line(hello());
    // An accepted delete refills the unscoped gap and refetches both
    // counts for the deleted row's project (`docs/tui.md`, "The session
    // list": one ask of each in flight per project).
    let lines = commands(app.ask_recent_first(Some("-w")));
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["command"], "recent");
    assert!(lines[0].get("args").is_none());
    assert_eq!(fanout_projects(&lines[1..]), ["-w"]);
    assert_eq!(archived_projects(&lines), ["-w"]);
}

#[test]
fn a_held_archived_total_is_not_fanned_out_again() {
    let mut app = home_with(80, 24, launch());
    app.on_line(hello());
    // No rows on home: the delete-path refill asks the unscoped page
    // plus both counts for the project.
    let lines = commands(app.ask_recent_first(Some("-w")));
    assert_eq!(lines.len(), 3);
    let first = line_id(&lines[0]).to_owned();
    let archived = archived_id(&lines);
    let scoped = lines
        .iter()
        .filter(|line| !is_archived(line) && line["args"].get("project").is_some())
        .map(line_id)
        .next()
        .unwrap_or_else(|| panic!("a scoped ask"))
        .to_owned();
    // The scoped ask is refused without storing: its count stays
    // unheld, so only the archived total will be held below.
    assert!(app.on_line(refused(&scoped, "no recent")).is_empty());
    assert_eq!(stored(&app, "-w"), None);
    assert!(
        app.on_line(accepted(&archived, json!({"sessions": [], "total": 1})))
            .is_empty()
    );
    assert_eq!(archived_stored(&app, "-w"), Some(1));
    // The refill introduces the project's rows with no project on home
    // before it: the exited count is asked again while the held
    // archived one is not. With `is_none` flipped the archived ask
    // goes out again.
    let refill = answer_value(
        &mut app,
        &first,
        json!({"sessions": [exited_in("s_bbbbbbbbbbbbbbbb", "b", "/w", "-w")]}),
    );
    assert_eq!(fanout_projects(&refill), ["-w"]);
    assert!(archived_projects(&refill).is_empty());
}
