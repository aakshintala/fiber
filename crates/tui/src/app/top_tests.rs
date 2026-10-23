//! Tests for the order of the surfaces on top: the list is the declared
//! order whichever surface opened first, `between` cuts it at the edges,
//! and the context a stroke arrives in is the head's.

use std::path::PathBuf;
use std::sync::Arc;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use serde_json::json;

use contract::clock::Clock;

use super::{Surface, between};
use crate::app::config_views::ConfigView;
use crate::app::items::testkit::{job_started, mark_tty};
use crate::app::session_views::SessionView;
use crate::app::{App, Effect};
use crate::home::{Launch, Spot};
use crate::keys::{Button, Key, Mouse, MouseKind};
use crate::keyset::Context;
use crate::link::Line;
use crate::mouse::{self, Pointer, TargetId};
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

/// The injected clock's now, as the input handling takes it.
fn now() -> std::time::Instant {
    fakes::clock::FakeClock::new().now()
}

/// The app drawn at its size: the buffer and the last frame's targets,
/// as the loop holds them.
fn drawn(app: &App) -> (Buffer, Vec<crate::mouse::Target>) {
    let (width, height) = app.screen_size();
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    (buf, targets)
}

/// A left press at (`col`, `row`) through the loop's mouse path, with a
/// fresh pointer.
fn press_at(app: &mut App, targets: &[crate::mouse::Target], col: u16, row: u16) -> Effect {
    let mut pointer = Pointer::default();
    app.on_mouse(
        &Mouse {
            kind: MouseKind::Press(Button::Left),
            col,
            row,
        },
        targets,
        &mut pointer,
        true,
        now(),
    )
}

/// The first screen corner outside `slab` that is no model-picker header
/// cell: the header's whole top row closes the picker outright instead.
#[track_caller]
fn outside_corner(
    label: &str,
    targets: &[crate::mouse::Target],
    slab: Rect,
    width: u16,
    height: u16,
) -> (u16, u16) {
    [
        (0, 0),
        (width - 1, 0),
        (0, height - 1),
        (width - 1, height - 1),
    ]
    .into_iter()
    .find(|&(col, row)| {
        !slab.contains(Position::new(col, row))
            && !matches!(
                mouse::hit(targets, col, row),
                Some(TargetId::View(crate::swapped::Spot::Close))
            )
    })
    .unwrap_or_else(|| panic!("{label}: no screen corner outside {slab:?}"))
}

fn open_quit(app: &mut App) {
    open(app, Surface::Quit);
}

fn open_report(app: &mut App) {
    open(app, Surface::Report);
}

fn open_keymap(app: &mut App) {
    open(app, Surface::Keymap);
}

fn open_picker(app: &mut App) {
    open(app, Surface::Picker);
}

fn open_search(app: &mut App) {
    open(app, Surface::Search);
}

fn open_completion(app: &mut App) {
    open(app, Surface::Completion);
}

fn open_notice(app: &mut App) {
    open(app, Surface::Notice);
}

fn open_delete(app: &mut App) {
    open(app, Surface::DeleteQuestion);
    assert!(
        !app.home_screen()
            .as_ref()
            .is_none_or(|screen| screen.question_import),
        "the delete question, not the import offer"
    );
}

fn open_import(app: &mut App) {
    app.set_import_offer(true);
    assert!(
        app.top_surfaces().contains(&Surface::DeleteQuestion),
        "the import offer opened"
    );
    assert!(
        app.home_screen()
            .as_ref()
            .is_some_and(|screen| screen.question_import),
        "the import offer, not the delete question"
    );
}

fn open_login(app: &mut App) {
    app.set_configure(Some(
        Arc::new(crate::configure_fake::Fake::new(Vec::new())) as Arc<dyn crate::Configure>
    ));
    let _ = app.open_config_view(ConfigView::Login);
    assert!(
        app.top_surfaces().contains(&Surface::ConfigView),
        "/login opened"
    );
}

fn open_logout(app: &mut App) {
    app.set_configure(Some(
        Arc::new(crate::configure_fake::Fake::new(Vec::new())) as Arc<dyn crate::Configure>
    ));
    let _ = app.open_config_view(ConfigView::Logout);
    assert!(
        app.top_surfaces().contains(&Surface::ConfigView),
        "/logout opened"
    );
}

fn open_picker_typed(app: &mut App) {
    open(app, Surface::WorkspacePicker);
    assert!(
        app.home_screen().as_ref().is_some_and(|screen| screen
            .picker
            .as_ref()
            .is_some_and(|picker| picker.typed.is_some())),
        "the typed picker opens typed"
    );
}

fn open_picker_recents(app: &mut App) {
    open(app, Surface::WorkspacePicker);
    for _ in 0..16 {
        if app.home_screen().as_ref().is_some_and(|screen| {
            screen
                .picker
                .as_ref()
                .is_some_and(|picker| picker.typed.is_none())
        }) {
            break;
        }
        app.on_key(Key::Backspace, now());
    }
    assert!(
        app.home_screen().as_ref().is_some_and(|screen| screen
            .picker
            .as_ref()
            .is_some_and(|picker| picker.typed.is_none())),
        "the recents-only picker"
    );
}

/// One row of the outside-click table: how to build the app, open the
/// overlay, and which surface the press must close.
type OutsideCase = (&'static str, fn() -> App, fn(&mut App), Surface);

const OUTSIDE_CASES: &[OutsideCase] = &[
    ("session quit", session_app, open_quit, Surface::Quit),
    ("session report", session_app, open_report, Surface::Report),
    ("session keymap", session_app, open_keymap, Surface::Keymap),
    ("session picker", session_app, open_picker, Surface::Picker),
    (
        "session login",
        session_app,
        open_login,
        Surface::ConfigView,
    ),
    (
        "session logout",
        session_app,
        open_logout,
        Surface::ConfigView,
    ),
    ("session search", session_app, open_search, Surface::Search),
    (
        "session completion",
        session_app,
        open_completion,
        Surface::Completion,
    ),
    ("session notice", session_app, open_notice, Surface::Notice),
    ("home quit", home_app, open_quit, Surface::Quit),
    ("home report", home_app, open_report, Surface::Report),
    (
        "home delete",
        home_app,
        open_delete,
        Surface::DeleteQuestion,
    ),
    (
        "home import",
        home_app,
        open_import,
        Surface::DeleteQuestion,
    ),
    (
        "home picker typed",
        home_app,
        open_picker_typed,
        Surface::WorkspacePicker,
    ),
    (
        "home picker recents",
        home_app,
        open_picker_recents,
        Surface::WorkspacePicker,
    ),
    ("home picker", home_app, open_picker, Surface::Picker),
    ("home login", home_app, open_login, Surface::ConfigView),
    (
        "home completion",
        home_app,
        open_completion,
        Surface::Completion,
    ),
    ("home search", home_app, open_search, Surface::Search),
    ("home notice", home_app, open_notice, Surface::Notice),
];

#[test]
fn a_press_outside_the_top_frame_closes_it_for_each_overlay() {
    for (label, make, opener, surface) in OUTSIDE_CASES {
        let mut app = make();
        opener(&mut app);
        assert!(
            app.top_surfaces().contains(surface),
            "{label}: {surface:?} open"
        );
        let (width, height) = app.screen_size();
        let (_, targets) = drawn(&app);
        let slab = app
            .frames()
            .slab_of(*surface)
            .unwrap_or_else(|| panic!("{label}: {surface:?} drew no frame"));
        assert!(!slab.is_empty(), "{label}: the frame is not empty");
        let (col, row) = outside_corner(label, &targets, slab, width, height);
        let mut twin = make();
        opener(&mut twin);
        let esc = twin.on_key(Key::Esc, now());
        let effect = press_at(&mut app, &targets, col, row);
        assert_eq!(effect, esc, "{label}: the press closes as Esc does");
        assert!(
            !app.top_surfaces().contains(surface),
            "{label}: {surface:?} closed"
        );
    }
}

#[test]
fn a_press_outside_the_quit_question_on_the_reports_cross_closes_only_the_quit() {
    for make in [home_app as fn() -> App, session_app as fn() -> App] {
        let mut app = make();
        open(&mut app, Surface::Report);
        open(&mut app, Surface::Quit);
        let (_, targets) = drawn(&app);
        let quit = app
            .frames()
            .slab_of(Surface::Quit)
            .expect("the quit question drew");
        let (col, row) = targets
            .iter()
            .find_map(|target| {
                (target.id == TargetId::CloseOverlay
                    && !quit.contains(Position::new(target.rect.x, target.rect.y)))
                .then_some((target.rect.x, target.rect.y))
            })
            .expect("the report's cross outside the quit question");
        assert_eq!(
            mouse::hit(&targets, col, row),
            Some(TargetId::CloseOverlay),
            "the cross is on top there"
        );
        press_at(&mut app, &targets, col, row);
        let listed = app.top_surfaces();
        assert!(
            !listed.contains(&Surface::Quit),
            "the question closed: {listed:?}"
        );
        assert!(
            listed.contains(&Surface::Report),
            "the report stays: {listed:?}"
        );
    }
}

#[test]
fn the_frame_edge_is_the_boundary() {
    let slab = {
        let mut app = session_app();
        open(&mut app, Surface::Notice);
        let (_, _) = drawn(&app);
        app.frames().slab_of(Surface::Notice).expect("notice slab")
    };
    assert!(!slab.is_empty());
    let middle_row = slab.y + slab.height / 2;
    let middle_col = slab.x + slab.width / 2;
    let x0 = slab.x.checked_sub(1).expect("room left of the frame");
    let y0 = slab.y.checked_sub(1).expect("room above the frame");
    for (col, row, inside) in [
        (x0, middle_row, false),
        (slab.x, middle_row, true),
        (slab.right() - 1, middle_row, true),
        (slab.right(), middle_row, false),
        (middle_col, y0, false),
        (middle_col, slab.y, true),
        (middle_col, slab.bottom() - 1, true),
        (middle_col, slab.bottom(), false),
    ] {
        let mut app = session_app();
        open(&mut app, Surface::Notice);
        let (_, targets) = drawn(&app);
        press_at(&mut app, &targets, col, row);
        assert_eq!(
            app.top_surfaces().contains(&Surface::Notice),
            inside,
            "at ({col}, {row})"
        );
    }
}

#[test]
fn only_a_left_press_is_intercepted() {
    let mut app = session_app();
    open(&mut app, Surface::Notice);
    let (width, height) = app.screen_size();
    let (_, targets) = drawn(&app);
    let slab = app.frames().slab_of(Surface::Notice).expect("notice slab");
    let (col, row) = outside_corner("notice", &targets, slab, width, height);
    for kind in [
        MouseKind::Release,
        MouseKind::Drag(Button::Left),
        MouseKind::Motion,
        MouseKind::WheelUp,
        MouseKind::WheelDown,
        MouseKind::Press(Button::Right),
    ] {
        let mut pointer = Pointer::default();
        app.on_mouse(
            &Mouse { kind, col, row },
            &targets,
            &mut pointer,
            true,
            now(),
        );
        assert!(
            app.top_surfaces().contains(&Surface::Notice),
            "{kind:?} leaves the overlay open"
        );
    }
    press_at(&mut app, &targets, col, row);
    assert!(
        !app.top_surfaces().contains(&Surface::Notice),
        "a left press at the same cell closes it"
    );
}

#[test]
fn the_picker_clears_its_query_first_then_closes() {
    let mut app = session_app();
    open(&mut app, Surface::Picker);
    app.on_key(Key::Char('m'), now());
    app.on_key(Key::Char('o'), now());
    assert!(!app.model_picker.query().is_empty(), "a query is typed");
    let (width, height) = app.screen_size();
    let (_, targets) = drawn(&app);
    let slab = app.frames().slab_of(Surface::Picker).expect("picker slab");
    let (col, row) = outside_corner("picker", &targets, slab, width, height);
    press_at(&mut app, &targets, col, row);
    assert_eq!(
        app.model_picker.query(),
        "",
        "the first press clears the query"
    );
    assert!(
        app.top_surfaces().contains(&Surface::Picker),
        "the picker stays for the first press"
    );
    let (_, targets) = drawn(&app);
    let slab = app.frames().slab_of(Surface::Picker).expect("picker slab");
    let (col, row) = outside_corner("picker", &targets, slab, width, height);
    press_at(&mut app, &targets, col, row);
    assert!(
        !app.top_surfaces().contains(&Surface::Picker),
        "the second press closes it"
    );
}

#[test]
fn the_control_under_the_press_does_not_fire() {
    let mut app = session_app();
    open(&mut app, Surface::Panel);
    app.put_aside();
    assert!(app.panel().is_none(), "the approval waits on its badge");
    open(&mut app, Surface::Notice);
    let (_, targets) = drawn(&app);
    let slab = app.frames().slab_of(Surface::Notice).expect("notice slab");
    let badge = targets
        .iter()
        .find(|target| target.id == TargetId::Badge)
        .expect("the badge draws")
        .rect;
    assert!(
        !slab.contains(Position::new(badge.x, badge.y)),
        "the badge shows outside the notice frame"
    );
    let (col, row) = (badge.left()..badge.right())
        .flat_map(|col| (badge.top()..badge.bottom()).map(move |row| (col, row)))
        .find(|&(col, row)| mouse::hit(&targets, col, row) == Some(TargetId::Badge))
        .expect("a badge cell on top");
    let mut pointer = Pointer::default();
    app.on_mouse(
        &Mouse {
            kind: MouseKind::Press(Button::Left),
            col,
            row,
        },
        &targets,
        &mut pointer,
        true,
        now(),
    );
    app.on_mouse(
        &Mouse {
            kind: MouseKind::Release,
            col,
            row,
        },
        &targets,
        &mut pointer,
        true,
        now(),
    );
    assert!(
        !app.top_surfaces().contains(&Surface::Notice),
        "the overlay closed"
    );
    assert!(app.panel().is_none(), "the badge never reopened the panel");
}

#[test]
fn no_frame_drawn_yet_means_no_intercept() {
    let mut twin = session_app();
    open(&mut twin, Surface::Notice);
    let (width, height) = twin.screen_size();
    let (_, targets) = drawn(&twin);
    let slab = twin.frames().slab_of(Surface::Notice).expect("notice slab");
    let (col, row) = outside_corner("notice", &targets, slab, width, height);
    let mut app = session_app();
    open(&mut app, Surface::Notice);
    press_at(&mut app, &targets, col, row);
    assert!(
        app.top_surfaces().contains(&Surface::Notice),
        "with no frame drawn the press takes the old path"
    );
    let (_, targets) = drawn(&app);
    press_at(&mut app, &targets, col, row);
    assert!(
        !app.top_surfaces().contains(&Surface::Notice),
        "after the frame the same press closes"
    );
}

#[test]
fn frames_note_replace_clear_and_ignore_an_empty_slab() {
    let frames = super::Frames::default();
    assert_eq!(frames.slab_of(Surface::Quit), None);
    let slab = Rect::new(16, 5, 48, 13);
    frames.note(Surface::Quit, slab);
    assert_eq!(frames.slab_of(Surface::Quit), Some(slab));
    let moved = Rect::new(10, 4, 20, 8);
    frames.note(Surface::Quit, moved);
    assert_eq!(frames.slab_of(Surface::Quit), Some(moved));
    frames.note(Surface::Notice, Rect::default());
    assert_eq!(frames.slab_of(Surface::Notice), None);
    frames.clear();
    assert_eq!(frames.slab_of(Surface::Quit), None);
}

#[test]
fn a_press_inside_the_frame_leaves_it_open() {
    for (label, opener, surface) in [
        ("notice", open_notice as fn(&mut App), Surface::Notice),
        ("report", open_report as fn(&mut App), Surface::Report),
        ("quit", open_quit as fn(&mut App), Surface::Quit),
    ] {
        let mut app = session_app();
        opener(&mut app);
        let (_, targets) = drawn(&app);
        let slab = app
            .frames()
            .slab_of(surface)
            .unwrap_or_else(|| panic!("{label}: drew no frame"));
        let col = slab.x + slab.width / 2;
        let row = slab.y + slab.height / 2;
        assert!(slab.contains(Position::new(col, row)), "{label}: inside");
        press_at(&mut app, &targets, col, row);
        assert!(
            app.top_surfaces().contains(&surface),
            "{label}: a press inside the frame leaves it open"
        );
    }
}

#[test]
fn a_press_outside_a_surface_outside_the_set_changes_nothing() {
    for surface in [
        Surface::Panel,
        Surface::Offer,
        Surface::Results,
        Surface::KeysScreen,
        Surface::SessionView,
    ] {
        let mut app = session_app();
        open(&mut app, surface);
        assert_eq!(app.top_surface(), surface, "{surface:?} on top");
        let (_, targets) = drawn(&app);
        press_at(&mut app, &targets, 0, 0);
        assert_eq!(app.top_surface(), surface, "{surface:?} stays on top");
        if surface == Surface::Panel {
            assert!(app.panel().is_some(), "the panel is not declined");
        } else if surface == Surface::Offer {
            assert!(app.offer_open(), "the offer is not put aside");
        }
    }
}

#[test]
fn a_covered_surface_with_no_frame_takes_the_old_path() {
    // The key map draws only on the session path: on home its swapped
    // view is full-screen and notes no slab.
    let mut app = home_app();
    open(&mut app, Surface::Keymap);
    assert_eq!(app.top_surface(), Surface::Keymap);
    let (_, targets) = drawn(&app);
    assert_eq!(app.frames().slab_of(Surface::Keymap), None);
    press_at(&mut app, &targets, 0, 0);
    assert_eq!(app.top_surface(), Surface::Keymap, "the key map stays");
    // The credential picker draws full-area through its configuration
    // view and notes no slab either; it needs the link up.
    let mut app = session_app();
    let _ = app.on_line(hub_hello());
    let _ = app.open_config_view(ConfigView::Credential);
    assert!(app.credential_view_open(), "the credential picker opened");
    assert_eq!(app.top_surface(), Surface::Picker);
    let (_, targets) = drawn(&app);
    assert_eq!(app.frames().slab_of(Surface::Picker), None);
    press_at(&mut app, &targets, 0, 0);
    assert!(app.credential_view_open(), "the credential picker stays");
    // A plain configuration view draws full-area through its swapped
    // view and notes no slab either.
    let mut app = home_app();
    let _ = app.open_config_view(ConfigView::Theme);
    assert_eq!(app.top_surface(), Surface::ConfigView);
    let (_, targets) = drawn(&app);
    assert_eq!(app.frames().slab_of(Surface::ConfigView), None);
    press_at(&mut app, &targets, 0, 0);
    assert_eq!(
        app.top_surface(),
        Surface::ConfigView,
        "the theme view stays"
    );
}

/// A hub hello, bringing the link up.
fn hub_hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// A remote home with no workspace chosen: the picker opens on its own
/// and Esc cannot close it.
fn remote_home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Remote {
            hub: "box".to_owned(),
        },
        model: Some("test/model".to_owned()),
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

#[test]
fn the_workspace_picker_without_a_workspace_swallows_the_press() {
    let mut app = remote_home();
    assert!(app.home_picker_open(), "the picker opens with no workspace");
    assert!(
        app.home_screen().as_ref().is_some_and(|screen| screen
            .picker
            .as_ref()
            .is_some_and(|picker| picker.typed.is_some())),
        "the picker is the typed one"
    );
    let (width, height) = app.screen_size();
    let (_, targets) = drawn(&app);
    let slab = app
        .frames()
        .slab_of(Surface::WorkspacePicker)
        .expect("the picker drew");
    let (col, row) = outside_corner("remote picker", &targets, slab, width, height);
    let mut pointer = Pointer::default();
    let effect = app.on_mouse(
        &Mouse {
            kind: MouseKind::Press(Button::Left),
            col,
            row,
        },
        &targets,
        &mut pointer,
        true,
        now(),
    );
    assert_eq!(effect, Effect::None, "the press closes nothing");
    assert!(app.home_picker_open(), "the picker stays");
    let effect = app.on_mouse(
        &Mouse {
            kind: MouseKind::Release,
            col,
            row,
        },
        &targets,
        &mut pointer,
        true,
        now(),
    );
    assert_eq!(effect, Effect::None, "the release clicks nothing");
    assert!(app.home_picker_open(), "the picker still stays");
}

#[test]
fn a_press_on_a_picker_header_cell_closes_it_outright() {
    let mut app = session_app();
    open(&mut app, Surface::Picker);
    app.on_key(Key::Char('m'), now());
    assert!(!app.model_picker.query().is_empty(), "a query is typed");
    let (_, targets) = drawn(&app);
    assert_eq!(
        mouse::hit(&targets, 40, 0),
        Some(TargetId::View(crate::swapped::Spot::Close)),
        "the header cell closes the picker"
    );
    let mut pointer = Pointer::default();
    app.on_mouse(
        &Mouse {
            kind: MouseKind::Press(Button::Left),
            col: 40,
            row: 0,
        },
        &targets,
        &mut pointer,
        true,
        now(),
    );
    app.on_mouse(
        &Mouse {
            kind: MouseKind::Release,
            col: 40,
            row: 0,
        },
        &targets,
        &mut pointer,
        true,
        now(),
    );
    assert!(
        !app.top_surfaces().contains(&Surface::Picker),
        "the picker closed outright, where Esc would only clear the query"
    );
    // With the quit question on top over the open picker the same
    // header press is outside its frame and closes the question.
    let mut app = session_app();
    open(&mut app, Surface::Picker);
    open(&mut app, Surface::Quit);
    let (_, targets) = drawn(&app);
    press_at(&mut app, &targets, 40, 0);
    let listed = app.top_surfaces();
    assert!(
        !listed.contains(&Surface::Quit),
        "the question closed: {listed:?}"
    );
    assert!(
        listed.contains(&Surface::Picker),
        "the picker stays: {listed:?}"
    );
}

/// A session showing one reply's text, ready to select from.
fn reply_session() -> App {
    let mut app = session_app();
    for (kind, action, payload) in [
        (
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "prompt"}]}]}),
        ),
        ("step_started", None, json!({})),
        ("assistant_message_started", Some("a_m"), json!({})),
        (
            "text_completed",
            Some("a_m"),
            json!({"text": "a one-line reply"}),
        ),
        (
            "assistant_message_completed",
            Some("a_m"),
            json!({"outcome": "completed"}),
        ),
        ("turn_completed", None, json!({"outcome": "completed"})),
    ] {
        app.on_line(session_line(kind, payload, action));
    }
    app
}

/// The screen cell where `needle` first shows, by column count.
fn find_text(app: &App, needle: &str) -> (u16, u16) {
    let (buf, _) = drawn(app);
    let area = buf.area;
    for y in area.top()..area.bottom() {
        let mut row = String::new();
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell((x, y)) {
                row.push_str(cell.symbol());
            }
        }
        if let Some(byte) = row.find(needle) {
            let col = row.get(..byte).map_or(0, |before| before.chars().count());
            return (col.try_into().expect("the reply column fits"), y);
        }
    }
    panic!("{needle:?} is not on screen");
}

#[test]
fn a_swallowed_press_still_clears_copied() {
    let mut app = reply_session();
    let at = find_text(&app, "one-line");
    let (_, targets) = drawn(&app);
    app.on_select(
        &Mouse {
            kind: MouseKind::Press(Button::Left),
            col: at.0,
            row: at.1,
        },
        &targets,
    );
    app.on_select(
        &Mouse {
            kind: MouseKind::Drag(Button::Left),
            col: at.0 + 4,
            row: at.1,
        },
        &targets,
    );
    app.on_select(
        &Mouse {
            kind: MouseKind::Release,
            col: at.0 + 4,
            row: at.1,
        },
        &targets,
    );
    assert!(app.copied(), "the release copied");
    open(&mut app, Surface::Notice);
    let (width, height) = app.screen_size();
    let (_, targets) = drawn(&app);
    let slab = app.frames().slab_of(Surface::Notice).expect("notice slab");
    let (col, row) = outside_corner("notice", &targets, slab, width, height);
    press_at(&mut app, &targets, col, row);
    assert!(!app.copied(), "the swallowed press still clears Copied");
    assert!(
        !app.top_surfaces().contains(&Surface::Notice),
        "the overlay closed"
    );
}

#[test]
fn a_lost_release_cannot_click_the_target_pressed_before_the_swallow() {
    let mut app = session_app();
    open(&mut app, Surface::Panel);
    app.put_aside();
    assert!(app.panel().is_none(), "the approval waits on its badge");
    let (_, targets) = drawn(&app);
    let badge = targets
        .iter()
        .find(|target| target.id == TargetId::Badge)
        .expect("the badge draws")
        .rect;
    let (badge_col, badge_row) = (badge.left()..badge.right())
        .flat_map(|col| (badge.top()..badge.bottom()).map(move |row| (col, row)))
        .find(|&(col, row)| mouse::hit(&targets, col, row) == Some(TargetId::Badge))
        .expect("a badge cell on top");
    // A press on the badge with no release, as a lost release leaves it.
    let mut pointer = Pointer::default();
    app.on_mouse(
        &Mouse {
            kind: MouseKind::Press(Button::Left),
            col: badge_col,
            row: badge_row,
        },
        &targets,
        &mut pointer,
        true,
        now(),
    );
    open(&mut app, Surface::Notice);
    let (width, height) = app.screen_size();
    let (_, targets) = drawn(&app);
    let slab = app.frames().slab_of(Surface::Notice).expect("notice slab");
    let (col, row) = outside_corner("notice", &targets, slab, width, height);
    assert!(
        (col, row) != (badge_col, badge_row),
        "the swallowed press lands elsewhere"
    );
    app.on_mouse(
        &Mouse {
            kind: MouseKind::Press(Button::Left),
            col,
            row,
        },
        &targets,
        &mut pointer,
        true,
        now(),
    );
    app.on_mouse(
        &Mouse {
            kind: MouseKind::Release,
            col: badge_col,
            row: badge_row,
        },
        &targets,
        &mut pointer,
        true,
        now(),
    );
    assert!(app.panel().is_none(), "the badge never reopened the panel");
}
