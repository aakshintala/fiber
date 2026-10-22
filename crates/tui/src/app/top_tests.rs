//! Tests for the order of the surfaces on top: the list is the declared
//! order whichever surface opened first, `between` cuts it at the edges,
//! and the context a stroke arrives in is the head's.

use std::path::PathBuf;

use serde_json::json;

use super::{Surface, between};
use crate::app::App;
use crate::home::{Launch, Spot};
use crate::keys::Key;
use crate::keyset::Context;
use crate::link::Line;
use contract::SessionId;
use contract::events::ImportedNames;

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
pub(in crate::app) const ON_SESSION: [Surface; 9] = [
    Surface::Quit,
    Surface::Report,
    Surface::Keymap,
    Surface::Panel,
    Surface::Offer,
    Surface::Search,
    Surface::Results,
    Surface::Find,
    Surface::Notice,
];

/// The surfaces [`open`] can open on home.
pub(in crate::app) const ON_HOME: [Surface; 5] = [
    Surface::Quit,
    Surface::Report,
    Surface::DeleteQuestion,
    Surface::WorkspacePicker,
    Surface::Notice,
];

/// Whether `first` and `second` can both be open: home holds one question
/// at a time, so the quit question and the delete question replace each
/// other.
pub(in crate::app) fn coexist(first: Surface, second: Surface) -> bool {
    !matches!(
        (first.min(second), first.max(second)),
        (Surface::Quit, Surface::DeleteQuestion)
    )
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

/// Opens `surface` on `app`, the way its own entry point does.
#[track_caller]
pub(in crate::app) fn open(app: &mut App, surface: Surface) {
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
            let key = app
                .home_screen()
                .and_then(|screen| {
                    crate::home::home_test_rows(&screen)
                        .into_iter()
                        .find(|(_, text)| text.contains("old work"))
                        .map(|row| row.0)
                })
                .expect("the exited row");
            app.home_click(Spot::Stop(key));
        }
        Surface::KeysScreen
        | Surface::Picker
        | Surface::ConfigView
        | Surface::SessionView
        | Surface::Focus
        | Surface::Completion
        | Surface::Job
        | Surface::Draft => panic!("{surface:?} has no opener here"),
    }
    assert!(app.top_surfaces().contains(&surface), "{surface:?} opened");
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
        for first in set {
            for second in set
                .iter()
                .filter(|second| *second != first && coexist(*first, **second))
            {
                let mut app = make();
                open(&mut app, *first);
                open(&mut app, *second);
                let listed = app.top_surfaces();
                assert!(
                    listed.contains(first) && listed.contains(second),
                    "{first:?} then {second:?}: {listed:?}"
                );
                assert_eq!(
                    app.top_surface(),
                    *first.min(second),
                    "{first:?} {second:?}"
                );
                let mut sorted = listed.clone();
                sorted.sort();
                assert_eq!(listed, sorted, "{first:?} then {second:?}");
                assert_eq!(listed.last(), Some(&Surface::Draft));
            }
        }
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
fn the_keys_screen_gives_the_context_below_it() {
    let mut app = session_app();
    app.open_keys();
    assert_eq!(app.top_surface(), Surface::KeysScreen);
    assert_eq!(app.key_context(), Context::Input);
    open(&mut app, Surface::Notice);
    assert_eq!(app.top_surface(), Surface::KeysScreen);
    assert_eq!(app.key_context(), Context::Overlay);
}

#[test]
fn the_rail_is_covered_by_the_notice_overlay_and_by_an_item_view() {
    let mut app = session_app();
    assert!(!app.rail_covered());
    open(&mut app, Surface::Notice);
    assert!(app.rail_covered());
    // Another surface on top leaves the overlay covering the rail.
    open(&mut app, Surface::Keymap);
    assert!(app.rail_covered());
}
