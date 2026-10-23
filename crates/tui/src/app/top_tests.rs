//! Tests for the order of the surfaces on top: the list is the declared
//! order whichever surface opened first, `between` cuts it at the edges,
//! and the context a stroke arrives in is the head's.

use std::path::PathBuf;

use serde_json::json;

use contract::clock::Clock;

use super::{Surface, between};
use crate::app::App;
use crate::app::config_views::ConfigView;
use crate::app::items::testkit::{job_started, mark_tty};
use crate::app::session_views::SessionView;
use crate::home::{Launch, Spot};
use crate::keys::Key;
use crate::keyset::Context;
use crate::link::Line;
use crate::mouse::TargetId;
use contract::events::ImportedNames;
use contract::{JobId, SessionId};

pub(in crate::app) const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The 19 surfaces, top first, as the ticket orders them.
const ORDER: [Surface; 19] = [
    Surface::Quit,
    Surface::Report,
    Surface::KeysScreen,
    Surface::Keymap,
    Surface::Picker,
    Surface::ConfigView,
    Surface::SessionView,
    Surface::DeleteQuestion,
    Surface::WorkspacePicker,
    Surface::Panel,
    Surface::Offer,
    Surface::Search,
    Surface::Results,
    Surface::Find,
    Surface::Focus,
    Surface::Completion,
    Surface::Job,
    Surface::Notice,
    Surface::Draft,
];

/// The surfaces [`open`] can open on a session.
pub(in crate::app) const ON_SESSION: [Surface; 16] = [
    Surface::Quit,
    Surface::Report,
    Surface::KeysScreen,
    Surface::Keymap,
    Surface::Picker,
    Surface::ConfigView,
    Surface::SessionView,
    Surface::Panel,
    Surface::Offer,
    Surface::Search,
    Surface::Results,
    Surface::Find,
    Surface::Focus,
    Surface::Completion,
    Surface::Job,
    Surface::Notice,
];

/// The surfaces [`open`] can open on home.
pub(in crate::app) const ON_HOME: [Surface; 9] = [
    Surface::Quit,
    Surface::Report,
    Surface::KeysScreen,
    Surface::Keymap,
    Surface::Picker,
    Surface::ConfigView,
    Surface::DeleteQuestion,
    Surface::WorkspacePicker,
    Surface::Notice,
];

/// Every ordered pair of `set` that stays open together, with a fresh
/// app from `make` holding both. Opening a swapped view or the quit
/// question can close or refuse the other, which leaves a pair that
/// never co-occurs in that order. The Ctrl+R panel is itself the
/// completion panel's rows, so the two are one surface and never a pair.
pub(in crate::app) fn stacked(set: &[Surface], make: fn() -> App) -> Vec<(Surface, Surface, App)> {
    let mut out = Vec::new();
    for first in set {
        for second in set.iter().filter(|second| {
            *second != first
                && !matches!(
                    (first.min(*second), first.max(*second)),
                    (Surface::Search, Surface::Completion)
                )
        }) {
            let mut app = make();
            launch(&mut app, *first);
            launch(&mut app, *second);
            let listed = app.top_surfaces();
            if listed.contains(first) && listed.contains(second) {
                out.push((*first, *second, app));
            }
        }
    }
    out
}

fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// A working session's status, so the quit question has someone to ask
/// about.
fn working() -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({
            "name": "fix the parser", "workspace": "/w", "project": "-w",
            "state": "streaming", "since": 0, "started": 0, "calls": 0,
            "delegates": 0, "jobs": 0, "clients": 0, "model": "test/model",
            "spend": {"tokens": {"input": 1, "output": 2, "cache_read": 0,
                "cache_write": {}}, "cost": 0.0, "subscription_cost": 0.0},
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// An app on home at 80x24 with a workspace chosen and one session
/// working.
pub(in crate::app) fn home_app() -> App {
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
        ..Default::default()
    });
    app.set_size(80, 24);
    let lines = app.on_line(hello());
    let recent: serde_json::Value = lines
        .get(1)
        .and_then(|line| serde_json::from_str(line).ok())
        .unwrap_or_default();
    app.on_line(working());
    // One exited row for the delete question to ask about.
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": recent["id"], "result": {"sessions": [{
            "session_id": "s_bbbbbbbbbbbbbbbb", "ts": 0, "project": "-w",
            "workspace": "/w", "name": "old work", "how": "exited",
        }]}})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
    app
}

/// [`home_app`] attached to its working session.
pub(in crate::app) fn session_app() -> App {
    let mut app = home_app();
    app.attach(SessionId(SESSION.to_owned()));
    app
}

/// A report long enough to scroll.
pub(in crate::app) fn long_names() -> ImportedNames {
    let names = |stem: &str| -> Vec<String> { (0..60).map(|at| format!("{stem}{at}")).collect() };
    serde_json::from_value(json!({
        "mcp_servers": names("server"),
        "skills": names("skill"),
        "commands": names("command"),
        "instructions": names("AGENTS"),
    }))
    .unwrap_or_default()
}

/// A session line of `kind` from the attached session.
fn session_line(kind: &str, payload: serde_json::Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Opens `surface` on `app`, the way its own entry point does, and
/// asserts it shows.
#[track_caller]
pub(in crate::app) fn open(app: &mut App, surface: Surface) {
    launch(app, surface);
    assert!(app.top_surfaces().contains(&surface), "{surface:?} opened");
}

/// The entry point of `surface`, with no check that it shows.
fn launch(app: &mut App, surface: Surface) {
    match surface {
        Surface::Quit => {
            app.quit();
        }
        Surface::Report => app.open_import_report(None, long_names(), Vec::new()),
        Surface::Keymap => {
            app.open_keymap();
        }
        Surface::Panel => {
            app.on_line(session_line(
                "permission_requested",
                json!({"request_id": "r_2", "effects": ["executes"], "reversible": true,
                    "step": "standing_ask",
                    "standing_rule": {"scope": "global", "prefix": "p"}}),
                Some("a_1"),
            ));
        }
        Surface::Offer => {
            let items: Vec<serde_json::Value> = ["a", "b", "c"]
                .iter()
                .map(|name| {
                    json!({"kind": "mcp_server", "name": name, "hash": "h",
                        "required": false, "summary": format!("MCP server: {name}")})
                })
                .collect();
            app.on_line(session_line(
                "repository_code_offered",
                json!({"request_id": "r_1", "items": items}),
                None,
            ));
        }
        Surface::Search => {
            app.open_search();
        }
        Surface::Find => {
            app.find_key(&Key::CtrlF);
        }
        Surface::Results => {
            app.find_key(&Key::CtrlF);
            app.open_results();
        }
        Surface::KeysScreen => {
            app.open_keys();
        }
        Surface::Picker => {
            app.open_model_picker(crate::model_picker::Mode::Choose);
        }
        Surface::ConfigView => {
            app.open_config_view(ConfigView::Theme);
        }
        Surface::SessionView => {
            app.open_session_view(SessionView::Usage);
        }
        Surface::Focus => app.focus = Some(TargetId::Turn(0)),
        Surface::Completion => {
            app.on_key(Key::Char('/'), fakes::clock::FakeClock::new().now());
        }
        Surface::Job => {
            mark_tty(app);
            job_started(app, "j_9", "run the editor", 0, "a_1");
            app.open_item(&JobId("j_9".to_owned()));
        }
        Surface::Notice => {
            app.notices.push(
                "extension_failed",
                "A notice with its whole text.".to_owned(),
            );
            app.open_more_notices();
        }
        Surface::WorkspacePicker => {
            app.home_click(Spot::Workspace);
        }
        Surface::DeleteQuestion => {
            // A swapped view hides the rows, and the question has nothing to ask about.
            let row = app.home_screen().and_then(|screen| {
                crate::home::home_test_rows(&screen)
                    .into_iter()
                    .find(|(_, text)| text.contains("old work"))
                    .map(|row| row.0)
            });
            if let Some(key) = row {
                app.home_click(Spot::Stop(key));
            }
        }
        Surface::Draft => panic!("{surface:?} has no opener here"),
    }
}

#[test]
fn the_declared_order_is_the_tickets() {
    let mut sorted = ORDER;
    sorted.sort();
    assert_eq!(sorted, ORDER);
    for pair in ORDER.windows(2) {
        assert!(pair[0] < pair[1], "{:?} above {:?}", pair[0], pair[1]);
    }
}

#[test]
fn nothing_open_lists_only_the_input_box() {
    let app = session_app();
    assert_eq!(app.top_surfaces(), vec![Surface::Draft]);
    assert_eq!(app.top_surface(), Surface::Draft);
    assert_eq!(home_app().top_surfaces(), vec![Surface::Draft]);
}

#[test]
fn two_surfaces_list_in_the_declared_order_whichever_opened_first() {
    for (set, make) in [
        (&ON_SESSION[..], session_app as fn() -> App),
        (&ON_HOME[..], home_app as fn() -> App),
    ] {
        for (first, second, app) in stacked(set, make) {
            let listed = app.top_surfaces();
            assert_eq!(app.top_surface(), first.min(second), "{first:?} {second:?}");
            let mut sorted = listed.clone();
            sorted.sort();
            assert_eq!(listed, sorted, "{first:?} then {second:?}");
            assert_eq!(listed.last(), Some(&Surface::Draft));
        }
    }
}

/// The pairs `docs/tui.md` lets co-occur, from the ticket's pair table;
/// the table's pairs with the draft are single surfaces and the panel
/// hides a tty job, so neither is a pair here. Opening the import
/// report closes the completion panels, so the report pairs with none.
const PLAN_PAIRS: [(Surface, Surface); 33] = [
    (Surface::Quit, Surface::Report),
    (Surface::Quit, Surface::Notice),
    (Surface::Quit, Surface::Keymap),
    (Surface::Quit, Surface::Picker),
    (Surface::Quit, Surface::ConfigView),
    (Surface::Quit, Surface::SessionView),
    (Surface::Report, Surface::Notice),
    (Surface::Report, Surface::Keymap),
    (Surface::Report, Surface::Picker),
    (Surface::Report, Surface::ConfigView),
    (Surface::Report, Surface::SessionView),
    (Surface::Report, Surface::DeleteQuestion),
    (Surface::Report, Surface::WorkspacePicker),
    (Surface::Report, Surface::Panel),
    (Surface::Report, Surface::Offer),
    (Surface::Report, Surface::Search),
    (Surface::Report, Surface::Results),
    (Surface::Report, Surface::Find),
    (Surface::Keymap, Surface::Notice),
    (Surface::Keymap, Surface::Picker),
    (Surface::KeysScreen, Surface::Notice),
    (Surface::ConfigView, Surface::Notice),
    (Surface::SessionView, Surface::Notice),
    (Surface::DeleteQuestion, Surface::Notice),
    (Surface::WorkspacePicker, Surface::Notice),
    (Surface::Panel, Surface::Notice),
    (Surface::Panel, Surface::Offer),
    (Surface::Search, Surface::Notice),
    (Surface::Offer, Surface::Notice),
    (Surface::Results, Surface::Notice),
    (Surface::Find, Surface::Notice),
    (Surface::Focus, Surface::Notice),
    (Surface::Completion, Surface::Focus),
];

#[test]
fn the_matrix_holds_every_pair_the_plan_lists_in_both_orders() {
    let mut held = Vec::new();
    for (set, make) in [
        (&ON_SESSION[..], session_app as fn() -> App),
        (&ON_HOME[..], home_app as fn() -> App),
    ] {
        held.extend(
            stacked(set, make)
                .into_iter()
                .map(|(first, second, _)| (first, second)),
        );
    }
    for (a, b) in PLAN_PAIRS {
        // A pair whose second opener refuses or closes the first in one
        // order only stays in the other.
        assert!(
            held.contains(&(a, b)) || held.contains(&(b, a)),
            "{a:?} with {b:?} never stacks"
        );
    }
}

#[test]
fn between_cuts_at_both_edges() {
    let order = [
        Surface::Quit,
        Surface::Keymap,
        Surface::Notice,
        Surface::Draft,
    ];
    // Through is inclusive: the surface itself stays.
    assert_eq!(
        between(&order, None, Some(Surface::Keymap)),
        &[Surface::Quit, Surface::Keymap]
    );
    // Just above an open surface it is out; just below it is in.
    assert_eq!(
        between(&order, None, Some(Surface::Report)),
        &[Surface::Quit]
    );
    assert_eq!(
        between(&order, None, Some(Surface::Picker)),
        &[Surface::Quit, Surface::Keymap]
    );
    // Above is exclusive: the surface itself is already handled.
    assert_eq!(
        between(&order, Some(Surface::Keymap), None),
        &[Surface::Notice, Surface::Draft]
    );
    assert_eq!(
        between(&order, Some(Surface::Report), None),
        &[Surface::Keymap, Surface::Notice, Surface::Draft]
    );
    assert_eq!(
        between(&order, Some(Surface::Quit), Some(Surface::Notice)),
        &[Surface::Keymap, Surface::Notice]
    );
    assert_eq!(between(&order, None, None), &order);
    assert!(between(&order, Some(Surface::Draft), None).is_empty());
    assert!(between(&order, Some(Surface::Notice), Some(Surface::Keymap)).is_empty());
}

#[test]
fn the_context_is_the_heads() {
    let mut app = session_app();
    assert_eq!(app.key_context(), Context::Input);
    open(&mut app, Surface::Notice);
    assert_eq!(app.top_surface(), Surface::Notice);
    assert_eq!(app.key_context(), Context::Overlay);
    open(&mut app, Surface::Find);
    assert_eq!(app.key_context(), Context::Search);
    open(&mut app, Surface::Results);
    assert_eq!(app.key_context(), Context::Search);
    open(&mut app, Surface::Search);
    assert_eq!(app.key_context(), Context::Overlay);
    open(&mut app, Surface::Keymap);
    assert_eq!(app.key_context(), Context::Overlay);
    open(&mut app, Surface::Report);
    assert_eq!(app.key_context(), Context::Overlay);
    open(&mut app, Surface::Quit);
    assert_eq!(app.key_context(), Context::Overlay);
}

#[test]
fn a_focused_chip_on_home_gives_the_input_context() {
    let mut app = home_app();
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let now = fakes::clock::FakeClock::new().now();
    // A row in the list is a stop of the conversation's kind.
    app.on_key(Key::Up, now);
    assert_eq!(app.top_surface(), Surface::Focus);
    assert!(app.focused_chip().is_none());
    assert_eq!(app.key_context(), Context::Conversation);
    app.on_key(Key::Esc, now);
    app.on_key(Key::BackTab, now);
    assert!(app.focused_chip().is_some());
    assert_eq!(app.top_surface(), Surface::Focus);
    assert_eq!(app.key_context(), Context::Input);
}

#[test]
fn esc_closes_the_completion_panel_above_the_notice_overlay_first() {
    let mut app = session_app();
    open(&mut app, Surface::Notice);
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Char('/'), now);
    assert_eq!(app.top_surface(), Surface::Completion);
    assert!(app.top_surfaces().contains(&Surface::Notice));
    app.on_key(Key::Esc, now);
    assert!(app.completions().is_none());
    assert_eq!(app.top_surface(), Surface::Notice);
}

#[test]
fn the_keys_screen_gives_the_context_below_it() {
    let mut app = session_app();
    app.open_keys();
    assert_eq!(app.top_surface(), Surface::KeysScreen);
    assert_eq!(app.key_context(), Context::Input);
    open(&mut app, Surface::Notice);
    assert_eq!(app.top_surface(), Surface::KeysScreen);
    assert_eq!(app.key_context(), Context::Overlay);
}
