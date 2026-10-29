//! Tests for the configuration views on the app: opening `/settings` on
//! home and attached, the keys a view takes and the ones it leaves, the
//! seam's absence, the editor's return and the theme a choice queues
//! (`docs/tui.md`, "Swapped views").

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::super::{App, Effect};
use super::ConfigView;
use crate::Configure;
use crate::configure::{Layer, Shown, WriteScope};
use crate::configure_fake::{Fake, file, row};
use crate::home::Launch;
use crate::keys::{Edit, Key};
use crate::link::Line;
use crate::mouse::TargetId;
use crate::swapped::Spot;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
fn now() -> Instant {
    contract::clock::Clock::now(fakes::clock::FakeClock::new().as_ref())
}

/// A seam over two rows.
fn fake() -> Arc<Fake> {
    let mut fake = Fake::new(vec![
        row(
            "handoff.tokens",
            Shown::Value("400000".to_owned()),
            "default",
            WriteScope::Any { repo: true },
        ),
        row(
            "tui.theme",
            Shown::Unset,
            "default",
            WriteScope::Any { repo: false },
        ),
    ]);
    fake.themes = vec!["solar".to_owned()];
    Arc::new(fake)
}

/// An app on home at 80x24 in `/w`, with `seam`.
fn home(seam: Option<Arc<Fake>>) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    app.set_configure(seam.map(|seam| seam as Arc<dyn Configure>));
    app.set_size(80, 24);
    app
}

/// An app attached to [`SESSION`], with `seam`.
fn attached(seam: Option<Arc<Fake>>) -> App {
    let mut app = home(seam);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// Types `/settings` and presses Enter.
fn slash_settings(app: &mut App) -> Effect {
    for ch in "/settings".chars() {
        app.on_key(Key::Char(ch), now());
    }
    app.on_key(Key::Enter, now())
}

/// The screen's rows as text.
fn screen(app: &App) -> Vec<String> {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    (0..24)
        .map(|y| {
            (0..80)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// A session line of `kind` on [`SESSION`].
fn session_line(kind: &str, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

#[test]
fn slash_settings_opens_the_view_attached_and_on_home() {
    let seam = fake();
    let mut app = home(Some(Arc::clone(&seam)));
    assert_eq!(slash_settings(&mut app), Effect::None);
    assert!(app.config_view_open());
    assert!(app.input().is_empty());
    let rows = screen(&app);
    assert!(
        rows.first().is_some_and(|row| row.starts_with(" Settings")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.starts_with("› handoff.tokens")),
        "{rows:?}"
    );
    assert_eq!(seam.reads(), [PathBuf::from("/w")]);

    let mut app = attached(Some(seam));
    slash_settings(&mut app);
    assert!(app.config_view_open());
    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row.starts_with("  Settings")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.starts_with(" › handoff.tokens")),
        "{rows:?}"
    );
}

#[test]
fn notices_show_above_the_open_view_on_home_and_attached() {
    // Notices float above the conversation (`docs/tui.md`, "Notices"),
    // and above a configuration view swapped into its place.
    for on_home in [false, true] {
        let mut app = if on_home {
            home(Some(fake()))
        } else {
            attached(Some(fake()))
        };
        app.push_notice("config_invalid", "The theme solar floats above.".to_owned());
        slash_settings(&mut app);
        assert!(app.config_view_open());
        let rows = screen(&app);
        assert!(
            rows.iter().any(|row| row.contains("floats above")),
            "{rows:?}"
        );
    }
}

#[test]
fn esc_and_the_x_close_it_and_send_nothing() {
    let seam = fake();
    let mut app = attached(Some(Arc::clone(&seam)));
    slash_settings(&mut app);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.config_view_open());
    slash_settings(&mut app);
    assert_eq!(app.on_click(TargetId::View(Spot::Close)), Effect::None);
    assert!(!app.config_view_open());
    assert!(seam.writes().is_empty());
}

#[test]
fn every_key_but_ctrl_c_lands_in_the_view() {
    let mut app = attached(Some(fake()));
    slash_settings(&mut app);
    for key in [Key::Char('x'), Key::Tab, Key::BackTab, Key::AltP, Key::F1] {
        assert_eq!(app.on_key(key.clone(), now()), Effect::None, "{key:?}");
    }
    assert!(app.input().is_empty());
    assert!(app.keymap_top().is_none());
    assert!(app.config_view_open());
}

#[test]
fn ctrl_c_still_reaches_the_quit_flow() {
    let mut app = attached(Some(fake()));
    slash_settings(&mut app);
    let now = now();
    assert_eq!(app.on_key(Key::CtrlC, now), Effect::None);
    let later = now.checked_add(Duration::from_millis(10)).unwrap_or(now);
    assert_eq!(app.on_key(Key::CtrlC, later), Effect::Quit);
}

#[test]
fn an_approval_waits_until_the_view_closes() {
    let mut app = attached(Some(fake()));
    slash_settings(&mut app);
    app.on_line(session_line(
        "tool_call_requested",
        serde_json::json!({"name": "shell", "arguments": {"command": "ls"}}),
    ));
    app.on_line(session_line(
        "permission_requested",
        serde_json::json!({"request_id": "r_1", "effects": ["executes"],
            "reversible": true, "step": "review"}),
    ));
    assert!(app.panel().is_some());
    // The first Esc closes the view; the panel keeps its request.
    app.on_key(Key::Esc, now());
    assert!(!app.config_view_open());
    assert!(app.panel().is_some());
}

#[test]
fn opening_a_view_drops_its_unsaved_field() {
    let seam = fake();
    let mut app = attached(Some(Arc::clone(&seam)));
    slash_settings(&mut app);
    app.on_key(Key::Enter, now());
    app.on_key(Key::Char('9'), now());
    assert!(
        app.config_view_screen()
            .is_some_and(|frame| frame.field.is_some())
    );
    app.open_config_view(ConfigView::Settings);
    assert!(
        app.config_view_screen()
            .is_some_and(|frame| frame.field.is_none())
    );
    assert!(seam.writes().is_empty());
}

#[test]
fn without_a_seam_the_view_says_not_available() {
    let mut app = attached(None);
    slash_settings(&mut app);
    let frame = app.config_view_screen();
    assert_eq!(
        frame.map(|frame| frame.below),
        Some(vec!["Not available in this terminal.".to_owned()])
    );
    assert_eq!(app.on_key(Key::CtrlG, now()), Effect::None);
    assert_eq!(app.on_click(TargetId::View(Spot::Row(0))), Effect::None);
    app.on_key(Key::Esc, now());
    assert!(!app.config_view_open());
}

#[test]
fn without_a_seam_the_placeholder_carries_the_opening_command() {
    // One site's pair pins the shape: the title names the view, the
    // command the slash command that opened it (`docs/tui.md`,
    // "Swapped views").
    let mut app = attached(None);
    slash_settings(&mut app);
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.title, "Settings");
    assert_eq!(frame.command, "/settings");
}

#[test]
fn ctrl_g_returns_the_file_to_open() {
    let mut app = attached(Some(fake()));
    slash_settings(&mut app);
    assert_eq!(
        app.on_key(Key::CtrlG, now()),
        Effect::OpenFile(file(Layer::Global))
    );
}

#[test]
fn file_closed_rereads_the_rows() {
    let seam = fake();
    let mut app = attached(Some(Arc::clone(&seam)));
    slash_settings(&mut app);
    app.config_file_closed(Ok(()));
    assert_eq!(seam.reads().len(), 2);
    assert_eq!(app.notices.newest(), None);
}

#[test]
fn save_setting_writes_the_global_layer_through_the_seam() {
    // The rail queues the save; the seam write below is what the
    // loop's `save_shares` reaches.
    let seam = fake();
    let mut app = home(Some(Arc::clone(&seam)));
    app.save_setting("tui.theme", "\"solar\"");
    assert_eq!(
        seam.writes(),
        [(
            PathBuf::from("/w"),
            Layer::Global,
            "tui.theme".to_owned(),
            "\"solar\"".to_owned()
        )]
    );
    assert_eq!(app.notices.newest(), None);
}

#[test]
fn global_save_without_a_workspace_writes_at_the_hub_global_parent() {
    use serde_json::json;
    // With no local workspace the hub-global parent stands in for the
    // workspace: the global file answers once, and the set names the
    // parent at the global layer.
    no_workspace_remote_case(
        vec![
            (
                json!({"command":"configure","args":{"operation":"global_file"}}),
                json!({"file":"/hub/config.json"}),
            ),
            (
                json!({"command":"configure","args":{"operation":"set","workspace":"/hub","layer":"global","key":"model","text":"\"hub/m\""}}),
                json!({"file":"/hub/config.json","warnings":[]}),
            ),
        ],
        |mut app| {
            app.save_setting("model", "\"hub/m\"");
            assert_eq!(app.notices.newest(), None);
        },
    );
}

#[test]
fn a_file_error_is_a_notice() {
    let mut app = attached(Some(fake()));
    slash_settings(&mut app);
    app.config_file_closed(Err("The editor exited with status 1.".to_owned()));
    assert_eq!(
        app.notices.newest(),
        Some("The editor exited with status 1.")
    );
}

#[test]
fn a_usage_line_sets_the_reload_cost() {
    let mut app = attached(Some(fake()));
    app.on_line(session_line(
        "usage_recorded",
        serde_json::json!({"generation_id": "g_1", "model": "a/b",
            "tokens": {"input": 1000, "cache_read": 200000,
                "cache_write": {"1h": 34}, "output": 5000},
            "input_bytes": 1, "cost": null}),
    ));
    // A copy of another session's call, and an extension's, change
    // nothing.
    app.on_line(session_line(
        "usage_recorded",
        serde_json::json!({"generation_id": "g_2", "model": "a/b",
            "tokens": {"input": 1, "cache_read": 0, "cache_write": {}, "output": 0},
            "input_bytes": 1, "cost": null, "origin_session_id": "s_bbbbbbbbbbbbbbbb"}),
    ));
    app.on_line(session_line(
        "usage_recorded",
        serde_json::json!({"generation_id": "g_3", "model": "a/b",
            "tokens": {"input": 1, "cache_read": 0, "cache_write": {}, "output": 0},
            "input_bytes": 1, "cost": null, "extension": "x"}),
    ));
    slash_settings(&mut app);
    let below = app.config_view_screen().map(|frame| frame.below);
    assert_eq!(
        below,
        Some(vec![
            "changing handoff.tokens needs a reload (~201k tokens)".to_owned()
        ])
    );
}

#[test]
fn a_theme_choice_is_taken_once() {
    let mut app = attached(Some(fake()));
    slash_settings(&mut app);
    for key in [Key::Down, Key::Enter, Key::Up, Key::Up, Key::Enter] {
        app.on_key(key, now());
    }
    assert!(matches!(
        app.take_theme_choice(),
        Some(crate::ThemeSetting::Follow)
    ));
    assert!(app.take_theme_choice().is_none());
}

#[test]
fn the_quit_question_takes_keys_over_the_view() {
    let mut app = attached(Some(fake()));
    app.phase = super::super::Phase::Attached {
        session: contract::SessionId(SESSION.to_owned()),
        busy: true,
    };
    slash_settings(&mut app);
    let at = now();
    app.on_key(Key::CtrlC, at);
    app.on_key(Key::CtrlC, at);
    assert!(app.quit_open());
    assert_eq!(app.on_key(Key::Enter, at), Effect::Quit);
}

#[test]
fn a_new_session_stroke_does_not_leave_the_view() {
    let mut app = attached(Some(fake()));
    slash_settings(&mut app);
    let stroke = crate::stroke::Stroke::parse("ctrl+n").unwrap_or_else(|err| panic!("{err}"));
    app.on_press(stroke, now());
    assert!(app.config_view_open());
    assert!(app.session().is_some());
}

#[test]
fn the_view_hides_the_cursor_and_keeps_the_input_box_attached() {
    let mut app = attached(Some(fake()));
    slash_settings(&mut app);
    let area = Rect::new(0, 0, 80, 24);
    assert_eq!(crate::view::cursor(&app, area), None);
    // Attached, the view takes the conversation's place, not the screen.
    let rows = screen(&app);
    assert!(rows.iter().any(|row| row == " ▌ ›"), "{rows:#?}");
    app.on_key(Key::Esc, now());
    assert!(crate::view::cursor(&app, area).is_some());
}

#[test]
fn the_reload_cost_is_the_session_on_screens_only() {
    let mut app = attached(Some(fake()));
    app.on_line(session_line(
        "usage_recorded",
        serde_json::json!({"generation_id": "g_1", "model": "a/b",
            "tokens": {"input": 10, "cache_read": 0, "cache_write": {}, "output": 0},
            "input_bytes": 1, "cost": null}),
    ));
    app.go_home();
    app.open_config_view(ConfigView::Settings);
    assert_eq!(
        app.config_view_screen().map(|frame| frame.below),
        Some(vec!["changing handoff.tokens needs a reload".to_owned()])
    );
}

#[test]
fn a_save_says_the_reload_cost_of_the_session_on_screen() {
    for (on_home, said) in [
        (
            false,
            "changing handoff.tokens needs a reload (~201k tokens)",
        ),
        (true, "changing handoff.tokens needs a reload"),
    ] {
        let mut app = attached(Some(fake()));
        app.on_line(session_line(
            "usage_recorded",
            serde_json::json!({"generation_id": "g_1", "model": "a/b",
                "tokens": {"input": 1000, "cache_read": 200000,
                    "cache_write": {"1h": 34}, "output": 5000},
                "input_bytes": 1, "cost": null}),
        ));
        if on_home {
            app.go_home();
        }
        app.open_config_view(ConfigView::Settings);
        app.on_key(Key::Enter, now());
        for _ in 0..6 {
            app.on_key(Key::Backspace, now());
        }
        for ch in "200000".chars() {
            app.on_key(Key::Char(ch), now());
        }
        app.on_key(Key::Enter, now());
        let below = app
            .config_view_screen()
            .map(|frame| frame.below)
            .unwrap_or_default();
        assert!(
            below.iter().any(|line| line == said),
            "on_home {on_home}: {below:?}"
        );
    }
}

#[test]
fn an_edit_goes_to_the_open_field_not_the_input_box() {
    let mut app = attached(Some(fake()));
    slash_settings(&mut app);
    app.on_key(Key::Enter, now());
    app.on_edit(Edit::Paste("9".to_owned()));
    let text = app
        .config_view_screen()
        .and_then(|frame| frame.field)
        .map(|(text, _)| text);
    assert!(
        text.is_some_and(|text| text.ends_with('9')),
        "the field takes the paste"
    );
    assert!(app.input().is_empty());
}

#[test]
fn without_a_seam_only_esc_closes_the_view() {
    let mut app = attached(None);
    slash_settings(&mut app);
    app.on_key(Key::Char('x'), now());
    assert!(app.config_view_open(), "a key other than Esc keeps it open");
    app.on_key(Key::Esc, now());
    assert!(!app.config_view_open());
}

/// A connected app attached to [`SESSION`], with `seam`: the link is up,
/// so a command can go out.
fn connected(seam: Option<Arc<Fake>>) -> App {
    let mut app = home(seam);
    app.on_line(crate::link::Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// Types `/tools` and presses Enter.
fn slash_tools(app: &mut App) -> Effect {
    for ch in "/tools".chars() {
        app.on_key(Key::Char(ch), now());
    }
    app.on_key(Key::Enter, now())
}

/// Types `/rules` and presses Enter.
fn slash_rules(app: &mut App) -> Effect {
    for ch in "/rules".chars() {
        app.on_key(Key::Char(ch), now());
    }
    app.on_key(Key::Enter, now())
}

/// The command lines an effect sends, parsed.
fn sent(effect: Effect) -> Vec<serde_json::Value> {
    match effect {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_default())
            .collect(),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_) => Vec::new(),
    }
}

/// Opens `/tools` on a connected app and returns the sent command's id.
fn open_tools(app: &mut App) -> String {
    let lines = sent(slash_tools(app));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "tools");
    assert_eq!(lines[0]["session_id"], SESSION);
    lines[0]["id"].as_str().unwrap_or_default().to_owned()
}

fn tools_answer() -> serde_json::Value {
    serde_json::json!({"tools": [
        {"name": "read", "source": "builtin", "state": "full", "bytes": 10},
        {"name": "mcp__m__x", "source": "mcp", "server": "m",
         "tool": "x", "state": "deferred", "bytes": 1}]})
}

#[test]
fn slash_tools_with_no_session_is_the_notice_and_opens_nothing() {
    let mut app = home(Some(fake()));
    assert_eq!(slash_tools(&mut app), Effect::None);
    assert_eq!(app.notices.newest(), Some("No session on screen."));
    assert!(!app.config_view_open());
}

#[test]
fn slash_tools_sends_tools_for_the_session_on_screen() {
    let mut app = connected(Some(fake()));
    let id = open_tools(&mut app);
    assert!(id.starts_with("c_"), "{id}");
    assert!(app.config_view_open());
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.title, "Tools");
    assert!(
        frame
            .below
            .first()
            .is_some_and(|line| line == "Reading the tools…")
    );
}

#[test]
fn the_answer_to_the_sent_id_fills_the_view() {
    let mut app = connected(Some(fake()));
    let id = open_tools(&mut app);
    app.on_line(session_line(
        "command_accepted",
        serde_json::json!({"command_id": "c_other", "result": tools_answer()}),
    ));
    let frame = app.config_view_screen().expect("the view is open");
    assert!(
        frame
            .below
            .first()
            .is_some_and(|line| line == "Reading the tools…"),
        "{frame:?}"
    );
    app.on_line(session_line(
        "command_accepted",
        serde_json::json!({"command_id": id, "result": tools_answer()}),
    ));
    let frame = app.config_view_screen().expect("the view is open");
    assert!(
        frame
            .rows
            .iter()
            .any(|row| row.first().is_some_and(|cell| cell.0.contains("read"))),
        "{frame:?}"
    );
    assert!(
        frame
            .rows
            .iter()
            .any(|row| row.first().is_some_and(|cell| cell.0.contains('x'))),
        "{frame:?}"
    );
}

#[test]
fn a_session_rejection_of_tools_shows_its_message() {
    let mut app = connected(Some(fake()));
    let id = open_tools(&mut app);
    app.on_line(session_line(
        "command_rejected",
        serde_json::json!({"command_id": id, "code": "busy", "message": "Busy."}),
    ));
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.below, vec!["Busy.".to_owned()]);
}

#[test]
fn a_hub_rejection_of_tools_shows_its_message() {
    let mut app = connected(Some(fake()));
    let id = open_tools(&mut app);
    app.on_line(crate::link::Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"command_id": id, "code": "busy", "message": "Busy."})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }));
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.below, vec!["Busy.".to_owned()]);
}

#[test]
fn without_a_seam_tools_says_not_available() {
    let mut app = connected(None);
    assert_eq!(slash_tools(&mut app), Effect::None);
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.title, "Tools");
    assert_eq!(
        frame.below,
        vec!["Not available in this terminal.".to_owned()]
    );
}

#[test]
fn a_click_on_the_panels_tools_line_opens_the_tools_view() {
    let mut app = connected(Some(fake()));
    let effect = app.on_click(crate::mouse::TargetId::Panel(
        crate::app::panel::Spot::Tools,
    ));
    let lines = sent(effect);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "tools");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert!(app.config_view_open());
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.title, "Tools");
}

#[test]
fn left_and_right_reach_the_tools_view_not_the_draft() {
    let seam = fake();
    let mut app = connected(Some(seam.clone()));
    let id = open_tools(&mut app);
    app.on_line(session_line(
        "command_accepted",
        serde_json::json!({"command_id": id, "result": tools_answer()}),
    ));
    app.on_edit(Edit::Right);
    assert!(app.input().is_empty());
    app.on_key(Key::Char(' '), now());
    let switched = seam.switched.lock().unwrap().clone();
    assert_eq!(switched.len(), 1);
    assert_eq!(switched[0].2, crate::configure::SwitchScope::Everywhere);
}

/// A `preamble_built` naming `model`.
fn preamble(model: &str) -> Line {
    session_line(
        "preamble_built",
        serde_json::json!({
            "reason": "start", "model": model, "context_window": 200000,
            "trigger_at": 150000, "thinking": null,
            "tool_choice": "auto", "cache_lifetime": "5m",
            "system_prompt": "", "tools": [],
        }),
    )
}

/// A `usage_recorded` whose prompt is `input` tokens.
fn usage_of(input: u64) -> Line {
    session_line(
        "usage_recorded",
        serde_json::json!({"generation_id": "g_1", "model": "a/b",
            "tokens": {"input": input, "cache_read": 0, "cache_write": {}, "output": 0},
            "input_bytes": 1, "cost": null}),
    )
}

/// A catalogue entry for `reference`, priced at `micros` per million
/// tokens when given.
fn priced(reference: &str, micros: Option<u64>) -> crate::catalogue::ModelEntry {
    let (provider, id) = reference.split_once('/').unwrap();
    crate::catalogue::ModelEntry {
        reference: reference.to_owned(),
        provider: provider.to_owned(),
        id: id.to_owned(),
        levels: Vec::new(),
        default_level: None,
        configured: None,
        roles: Vec::new(),
        name: None,
        price: micros.map(|micros| crate::catalogue::Price {
            micros_per_mtok: micros,
            input: micros,
            output: micros,
            tiers: Vec::new(),
        }),
    }
}

/// The tools view's last row on an app that made a call of `usage` tokens
/// on model `a/b`, with `entry` in the catalogue.
fn tools_footer(usage: Option<u64>, entry: Option<crate::catalogue::ModelEntry>) -> String {
    let mut app = connected(Some(fake()));
    app.on_line(preamble("a/b"));
    if let Some(tokens) = usage {
        app.on_line(usage_of(tokens));
    }
    app.on_models(Ok(crate::catalogue::Catalogue {
        installable: Vec::new(),
        models: entry.into_iter().collect(),
        notices: Vec::new(),
        lists: Vec::new(),
        left_out: Vec::new(),
    }));
    let id = open_tools(&mut app);
    app.on_line(session_line(
        "command_accepted",
        serde_json::json!({"command_id": id, "result": tools_answer()}),
    ));
    app.config_view_screen().expect("the view is open").footer
}

#[test]
fn the_tools_last_row_says_what_the_rebuild_costs() {
    // 84,000 tokens at $3.69 per million is $0.30996.
    assert_eq!(
        tools_footer(Some(84_000), Some(priced("a/b", Some(3_690_000)))),
        "a change rebuilds the cache (~84k tokens · $0.31) on reload"
    );
    assert_eq!(
        tools_footer(Some(84_000), Some(priced("a/b", None))),
        "a change rebuilds the cache (~84k tokens) on reload"
    );
    // Another model's price is not this one's.
    assert_eq!(
        tools_footer(Some(84_000), Some(priced("a/c", Some(3_690_000)))),
        "a change rebuilds the cache (~84k tokens) on reload"
    );
    assert_eq!(
        tools_footer(None, Some(priced("a/b", Some(3_690_000)))),
        "a change rebuilds the cache on reload"
    );
}

#[test]
fn the_tools_last_row_counts_only_the_session_on_screen() {
    let mut app = connected(Some(fake()));
    app.on_line(usage_of(84_000));
    app.attach(contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()));
    let lines = sent(slash_tools(&mut app));
    let id = lines[0]["id"].as_str().unwrap_or_default().to_owned();
    app.on_line(Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"command_id": id, "result": tools_answer()})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }));
    assert_eq!(
        app.config_view_screen().expect("the view is open").footer,
        "a change rebuilds the cache on reload"
    );
}

/// A seam with one global rule on line 1.
fn rules_fake() -> (Arc<Fake>, String) {
    let fake = fake();
    let rule = contract::rules::Rule {
        decision: contract::rules::RuleDecision::Allow,
        tool: "shell".to_owned(),
        prefix: "npm test".to_owned(),
        added: Some(1791331200000),
        session_id: Some(contract::SessionId("s_01".to_owned())),
    };
    let text = serde_json::to_string(&rule).unwrap_or_default();
    if let Ok(mut rules) = fake.rules.lock() {
        *rules = Ok((
            crate::configure::RulesSection {
                file: PathBuf::from("/home/rules"),
                rows: Ok(vec![crate::configure::RuleRow {
                    line: 1,
                    text: text.clone(),
                    rule,
                }]),
            },
            crate::configure::RulesSection {
                file: PathBuf::from("/home/projects/-w/rules"),
                rows: Ok(Vec::new()),
            },
        ));
    }
    (fake, text)
}

#[test]
fn slash_rules_opens_the_view_attached_and_on_home() {
    let seam = fake();
    let mut app = home(Some(Arc::clone(&seam)));
    assert_eq!(slash_rules(&mut app), Effect::None);
    assert!(app.config_view_open());
    assert!(app.input().is_empty());
    let rows = screen(&app);
    assert!(
        rows.first().is_some_and(|row| row.starts_with(" Rules")),
        "{rows:?}"
    );
    assert!(rows.iter().any(|row| row.trim() == "Global"), "{rows:?}");
    assert_eq!(seam.reads(), [PathBuf::from("/w")]);

    let mut app = attached(Some(seam));
    slash_rules(&mut app);
    assert!(app.config_view_open());
    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row.starts_with("  Rules")),
        "{rows:?}"
    );
}

#[test]
fn without_a_seam_rules_says_not_available_and_delete_does_nothing() {
    let mut app = attached(None);
    slash_rules(&mut app);
    let frame = app.config_view_screen();
    assert_eq!(
        frame.map(|frame| frame.below),
        Some(vec!["Not available in this terminal.".to_owned()])
    );
    assert_eq!(app.on_edit(Edit::Delete), Effect::None);
    assert!(app.config_view_open());
    assert!(app.input().is_empty());
}

#[test]
fn delete_reaches_the_rules_view_not_the_draft() {
    let (seam, text) = rules_fake();
    let mut app = attached(Some(Arc::clone(&seam)));
    slash_rules(&mut app);
    assert_eq!(app.on_edit(Edit::Delete), Effect::None);
    assert_eq!(
        seam.revokes(),
        [(
            PathBuf::from("/w"),
            crate::configure::RulesScope::Global,
            1,
            text
        )]
    );
    assert!(app.input().is_empty());
    assert!(app.config_view_open());
}

#[test]
fn the_rules_view_reads_each_age_from_the_frames_wall_time() {
    let (seam, _) = rules_fake();
    let mut app = attached(Some(seam));
    // The rule was added at 1791331200000; two days later it is "2d ago".
    app.set_now(now(), 1_791_331_200_000 + 2 * 86_400_000);
    slash_rules(&mut app);
    let rows = screen(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains("allow shell:npm test · added 2d ago in s_01 · ✕")),
        "{rows:?}"
    );
    // With no wall time set, the rule shows the minute it was added.
    let (seam, _) = rules_fake();
    let mut app = attached(Some(seam));
    slash_rules(&mut app);
    let rows = screen(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains("added 2026-10-07 00:00 UTC in s_01")),
        "{rows:?}"
    );
}

#[test]
fn a_click_on_a_rules_x_revokes() {
    let (seam, text) = rules_fake();
    let mut app = attached(Some(Arc::clone(&seam)));
    slash_rules(&mut app);
    assert_eq!(app.on_click(TargetId::View(Spot::Revoke(1))), Effect::None);
    assert_eq!(
        seam.revokes(),
        [(
            PathBuf::from("/w"),
            crate::configure::RulesScope::Global,
            1,
            text
        )]
    );
    assert!(app.config_view_open());
}

#[test]
fn file_closed_rereads_the_rules() {
    let seam = fake();
    let mut app = attached(Some(Arc::clone(&seam)));
    slash_rules(&mut app);
    app.config_file_closed(Ok(()));
    assert_eq!(seam.reads().len(), 2);
}

fn remote_settings() -> (serde_json::Value, serde_json::Value) {
    use serde_json::json;
    (
        json!({"command":"configure","args":{"operation":"settings","workspace":"/w"}}),
        json!({"rows":[{"key":"model","shown":"value","value":"\"hub/m\"","layer":"global","file":"/hub/config.json","scope":"global_only"}]}),
    )
}
fn remote_read() -> (serde_json::Value, serde_json::Value) {
    use serde_json::json;
    (
        json!({"command":"read_config","args":{"workspace":"/w","path":"/hub/config.json"}}),
        json!({"text":"original\r\n","version":"version-1"}),
    )
}

fn remote_editor_case(
    script: Vec<(serde_json::Value, serde_json::Value)>,
    test: impl FnOnce(App) + Send + 'static,
) {
    remote_case(
        script,
        |router| {
            let seam = Arc::new(Fake::new(vec![]));
            let remote = Arc::new(crate::remote::Remote::new(seam, "/client".into(), router));
            let mut app = attached(None);
            app.set_remote(remote);
            app
        },
        test,
    );
}

/// A remote-home app with no local workspace: the seam and the remote
/// are the hub, so a global save resolves the hub-global parent.
fn no_workspace_remote_case(
    script: Vec<(serde_json::Value, serde_json::Value)>,
    test: impl FnOnce(App) + Send + 'static,
) {
    remote_case(
        script,
        |router| {
            let seam = Arc::new(Fake::new(vec![]));
            let remote = Arc::new(crate::remote::Remote::new(seam, "/client".into(), router));
            let mut app = App::new(None);
            app.set_home(Launch {
                location: crate::LaunchLocation::Remote {
                    hub: "box".to_owned(),
                },
                ..Default::default()
            });
            app.set_size(80, 24);
            app.set_configure(Some(Arc::clone(&remote) as Arc<dyn Configure>));
            app.set_remote(remote);
            app
        },
        test,
    );
}

/// A `hub_hello` the remote home reads: the link comes up.
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// A hub `command_accepted` no ask waits for: the loop drops it, so it
/// proves a second pass asks the hub nothing more.
fn stray_accepted() -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            (
                "command_id".to_owned(),
                serde_json::Value::String("c_deadbeefdeadbeef".to_owned()),
            ),
            ("result".to_owned(), serde_json::Value::Null),
        ]
        .into_iter()
        .collect(),
    })
}

/// The remote offer's hub reads in order: pending first, then the
/// chips' global file and settings the home also owes.
fn offer_script(pending: bool) -> Vec<(serde_json::Value, serde_json::Value)> {
    use serde_json::json;
    vec![
        (
            json!({"command":"configure","args":{"operation":"import_pending"}}),
            json!({"pending": pending}),
        ),
        (
            json!({"command":"configure","args":{"operation":"global_file"}}),
            json!({"file": "/hub/.fiber/config.json"}),
        ),
        (
            json!({"command":"configure","args":{"operation":"settings","workspace":"/hub/.fiber"}}),
            json!({"rows": []}),
        ),
    ]
}

/// A remote home resolves its first-run offer from the hub machine's
/// home: pending shows the question, answered receipts do not. The
/// terminal's own home never answers, so a local receipt cannot still
/// the hub's offer and a missing one cannot fire it.
#[test]
fn remote_offer_follows_the_hubs_pending_answer() {
    for pending in [true, false] {
        no_workspace_remote_case(offer_script(pending), move |mut app| {
            drop(app.on_line(hello()));
            assert_eq!(app.home_screen().unwrap().question_import, pending);
            assert_eq!(app.notices.newest(), None);
            // The read runs once: a second pass asks the hub nothing
            // more, so a receipt landing mid-run changes nothing.
            drop(app.on_line(stray_accepted()));
            assert_eq!(app.home_screen().unwrap().question_import, pending);
        });
    }
}

/// A refused pending read is one notice and the offer stays down:
/// the terminal never shows an offer it cannot ground in the hub's
/// home.
#[test]
fn remote_offer_refused_stays_down_with_a_notice() {
    use serde_json::json;
    let mut script = vec![(
        json!({"command":"configure","args":{"operation":"import_pending"}}),
        json!({"reject": "no such operation"}),
    )];
    script.extend(offer_script(false).into_iter().skip(1));
    no_workspace_remote_case(script, |mut app| {
        drop(app.on_line(hello()));
        assert!(!app.home_screen().unwrap().question_import);
        assert_eq!(app.notices.newest(), Some("no such operation"));
        drop(app.on_line(stray_accepted()));
        assert!(!app.home_screen().unwrap().question_import);
    });
}

fn remote_case(
    script: Vec<(serde_json::Value, serde_json::Value)>,
    build: impl FnOnce(std::sync::Arc<crate::remote::Router>) -> App + Send + 'static,
    test: impl FnOnce(App) + Send + 'static,
) {
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    let deadline = fakes::Deadline::start();
    let router = crate::remote::Router::new();
    let (client, hub) = std::os::unix::net::UnixStream::pair().unwrap();
    let reader = client.try_clone().unwrap();
    router.attach(client);
    let (tx, _rx) = mpsc::channel();
    let feed =
        crate::link::Feed::new(tx, crate::link::Credits::new(64)).routed(Arc::clone(&router));
    let (server, served) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(hub.try_clone().unwrap());
        for (expected, result) in script {
            let mut text = String::new();
            reader.read_line(&mut text).unwrap();
            let line: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(line["command"], expected["command"]);
            assert_eq!(line["args"], expected["args"]);
            let payload = if let Some(message) = result.get("reject") {
                json!({"command_id":line["id"],"code":"stale_request","message":message})
            } else {
                json!({"command_id":line["id"],"result":result})
            };
            let kind = if payload.get("code").is_some() {
                "command_rejected"
            } else {
                "command_accepted"
            };
            crate::link::write_line(
                &hub,
                &json!({"kind":kind,"ts":1,"schema_version":1,"payload":payload}).to_string(),
            )
            .unwrap();
        }
        server.send(()).unwrap();
    });
    std::thread::spawn(move || crate::link::read_lines(reader, &feed));
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        test(build(router));
        done.send(()).unwrap();
    });
    deadline.recv_or_fail(&finished, "remote editor behaviour");
    deadline.recv_or_fail(&served, "remote editor wire script");
}

#[test]
fn remote_ctrl_g_round_trips_and_rereads_after_success_stale_or_editor_error() {
    use serde_json::json;
    for outcome in ["ok", "stale", "editor"] {
        let mut script = vec![remote_settings(), remote_read()];
        if outcome != "editor" {
            script.push((json!({"command":"write_config","args":{"workspace":"/w","path":"/hub/config.json","text":"edited\n","version":"version-1"}}),if outcome=="stale" {json!({"reject":"file changed at hub"})} else {json!(null)}));
        }
        script.push(remote_settings());
        remote_editor_case(script, move |mut app| {
            app.open_config_view(ConfigView::Settings);
            let Effect::OpenFile(temp) = app.on_key(Key::CtrlG, now()) else {
                panic!("remote editor copy");
            };
            assert_eq!(std::fs::read(&temp).unwrap(), b"original\r\n");
            assert_ne!(temp, PathBuf::from("/hub/config.json"));
            std::fs::write(&temp, "edited\n").unwrap();
            let dir = temp.parent().unwrap().to_path_buf();
            app.config_file_closed(if outcome == "editor" {
                Err("editor failed".into())
            } else {
                Ok(())
            });
            assert!(!dir.exists());
            assert!(app.config_view_open());
            match outcome {
                "ok" => assert_eq!(app.notices.newest(), None),
                "stale" => assert_eq!(app.notices.newest(), Some("file changed at hub")),
                "editor" => assert_eq!(app.notices.newest(), Some("editor failed")),
                _ => unreachable!(),
            }
        });
    }
}

#[test]
fn pending_editor_copy_drops_on_replacement_and_shutdown() {
    remote_editor_case(
        vec![remote_settings(), remote_read(), remote_read()],
        |mut app| {
            app.open_config_view(ConfigView::Settings);
            let Effect::OpenFile(first) = app.on_key(Key::CtrlG, now()) else {
                panic!("first copy")
            };
            let Effect::OpenFile(second) = app.on_key(Key::CtrlG, now()) else {
                panic!("second copy")
            };
            assert!(!first.parent().unwrap().exists());
            assert!(second.exists());
            drop(app);
            assert!(!second.parent().unwrap().exists());
        },
    );
}

#[test]
fn failed_remote_fetch_opens_nothing_and_client_files_never_use_the_hub() {
    use serde_json::json;
    let mut read = remote_read();
    read.1 = json!({"reject":"hub read refused"});
    remote_editor_case(vec![remote_settings(), read], |mut app| {
        app.open_config_view(ConfigView::Settings);
        assert_eq!(app.on_key(Key::CtrlG, now()), Effect::None);
        assert_eq!(app.notices.newest(), Some("hub read refused"));
        assert_eq!(
            app.config_act(super::Act::OpenLocal(Some("/client/keys.json".into()))),
            Effect::OpenFile("/client/keys.json".into())
        );
        assert_eq!(
            app.config_act(super::Act::OpenLocal(None)),
            Effect::OpenFile(file(Layer::Global))
        );
    });
    let mut local = attached(Some(fake()));
    assert_eq!(
        local.config_act(super::Act::OpenLocal(None)),
        Effect::OpenFile(file(Layer::Global))
    );
}

#[test]
fn failed_copy_cleanup_does_not_prevent_store_or_view_reread() {
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    remote_editor_case(
        vec![
            remote_settings(),
            remote_read(),
            (
                json!({"command":"write_config","args":{"workspace":"/w","path":"/hub/config.json","text":"original\r\n","version":"version-1"}}),
                json!(null),
            ),
            remote_settings(),
        ],
        |mut app| {
            app.open_config_view(ConfigView::Settings);
            let Effect::OpenFile(temp) = app.on_key(Key::CtrlG, now()) else {
                panic!("copy")
            };
            let dir = temp.parent().unwrap().to_path_buf();
            let inner = dir.join("inner");
            std::fs::create_dir(&inner).unwrap();
            let held = inner.join("held");
            std::fs::write(&held, "held").unwrap();
            std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o500)).unwrap();
            assert_eq!(
                std::fs::remove_file(&held).unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied,
                "fixture must deny removal; do not run as root"
            );
            app.config_file_closed(Ok(()));
            assert!(dir.exists());
            assert_eq!(app.notices.newest(), None);
            std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::remove_dir_all(dir).unwrap();
        },
    );
}

/// A seam with an unset `tui.theme` and file themes `solar` and `solar dawn`.
fn theme_fake() -> Fake {
    let mut fake = Fake::new(vec![
        row(
            "handoff.tokens",
            Shown::Value("400000".to_owned()),
            "default",
            WriteScope::Any { repo: true },
        ),
        row(
            "tui.theme",
            Shown::Unset,
            "default",
            WriteScope::Any { repo: false },
        ),
    ]);
    fake.themes = vec!["solar".to_owned(), "solar dawn".to_owned()];
    fake
}

/// Types `/theme` and presses Enter.
fn slash_theme(app: &mut App) -> Effect {
    for ch in "/theme".chars() {
        app.on_key(Key::Char(ch), now());
    }
    app.on_key(Key::Enter, now())
}

/// The open theme picker's choice names, in order.
fn theme_choices(app: &App) -> Vec<String> {
    app.config_view_screen()
        .map(|frame| {
            frame
                .rows
                .iter()
                .map(|row| {
                    row.first()
                        .map(|(text, _, _)| text.clone())
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Paints `app` whole in `setting`, through the same two steps the
/// loop's theme apply performs, on an 80x24 test screen.
fn painted(app: &mut App, setting: crate::ThemeSetting) -> crate::screen::Screen<TestBackend> {
    let vars = [("COLORTERM".to_owned(), "truecolor".to_owned())];
    let (look, _) = crate::look::Look::new(setting, &|name| {
        vars.iter()
            .find(|(set, _)| set == name)
            .map(|(_, value)| value.clone())
    });
    let mut screen =
        crate::screen::Screen::new(TestBackend::new(80, 24), 80, 24).expect("a screen");
    screen.set_look(look);
    screen
        .draw_with(app, None, crate::view::render)
        .expect("a draw");
    screen
}

/// A painted screen's text, one row per line.
fn painted_text(screen: &crate::screen::Screen<TestBackend>) -> String {
    let buf = screen.backend().buffer();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn slash_theme_opens_the_picker_with_live_preview_and_esc_restore() {
    // A non-default entry theme: `tui.theme` saved as dark, so a restore
    // of `Default::default()` (`Follow`) cannot pass for the capture.
    let mut base = theme_fake();
    base.rows = std::sync::Mutex::new(Ok(vec![
        row(
            "handoff.tokens",
            Shown::Value("400000".to_owned()),
            "default",
            WriteScope::Any { repo: true },
        ),
        row(
            "tui.theme",
            Shown::Value("\"dark\"".to_owned()),
            "global",
            WriteScope::Any { repo: false },
        ),
    ]));
    let seam = Arc::new(base);

    // The picker lists the built-ins plus the seam's file themes, marking
    // the saved entry.
    let mut app = attached(Some(Arc::clone(&seam)));
    assert_eq!(slash_theme(&mut app), Effect::None);
    assert!(app.config_view_open());
    assert_eq!(
        theme_choices(&app),
        ["auto", "dark  (current)", "light", "solar", "solar dawn"]
    );
    let open = painted(&mut app, crate::ThemeSetting::Follow);
    insta::assert_snapshot!("theme_picker_open", painted_text(&open));

    // Moving focus previews each focused theme live. The saved dark entry
    // opens focused on dark; up previews auto, then down walks back.
    app.on_key(Key::Char('x'), now());
    assert_eq!(app.take_theme_choice(), None);
    app.on_key(Key::Up, now());
    assert_eq!(app.take_theme_choice(), Some(crate::ThemeSetting::Follow));
    app.on_key(Key::Down, now());
    assert_eq!(app.take_theme_choice(), Some(crate::ThemeSetting::Dark));
    app.on_key(Key::Down, now());
    assert_eq!(app.take_theme_choice(), Some(crate::ThemeSetting::Light));
    let lit = painted(&mut app, crate::ThemeSetting::Light);
    assert!(
        lit.backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.bg == Color::Rgb(0xff, 0xff, 0xff)),
        "the light theme paints its own page"
    );
    insta::assert_snapshot!("theme_picker_light_preview", painted_text(&lit));
    app.on_key(Key::Down, now());
    assert!(
        matches!(app.take_theme_choice(), Some(crate::ThemeSetting::File { ref name, .. }) if name == "solar")
    );
    app.on_key(Key::Down, now());
    assert!(
        matches!(app.take_theme_choice(), Some(crate::ThemeSetting::File { ref name, .. }) if name == "solar dawn")
    );

    // Esc queues the non-default entry setting and returns to the rows,
    // writing nothing.
    assert!(seam.writes().is_empty());
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.config_view_open());
    assert_eq!(app.take_theme_choice(), Some(crate::ThemeSetting::Dark));
    assert!(seam.writes().is_empty());
    let restored = painted(&mut app, crate::ThemeSetting::Dark);
    assert!(
        restored
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| cell.bg != Color::Rgb(0xff, 0xff, 0xff)),
        "esc repaints the entry theme"
    );
    insta::assert_snapshot!("theme_picker_esc_restored", painted_text(&restored));

    // The picker's `x` cancels like Esc: the non-default capture is queued,
    // then closed. Queuing `Follow` here would be the default, not the capture.
    app.on_key(Key::Esc, now());
    assert_eq!(slash_theme(&mut app), Effect::None);
    app.on_key(Key::Down, now());
    assert_eq!(app.take_theme_choice(), Some(crate::ThemeSetting::Light));
    assert_eq!(app.on_click(TargetId::View(Spot::Close)), Effect::None);
    assert!(!app.config_view_open());
    assert_eq!(app.take_theme_choice(), Some(crate::ThemeSetting::Dark));
    assert!(seam.writes().is_empty());

    // A theme file changing between preview and Esc does not leak into
    // the restore: the queued setting equals the capture (dark), not the
    // re-read (light).
    let mut other = theme_fake();
    other.rows = std::sync::Mutex::new(Ok(vec![row(
        "tui.theme",
        Shown::Value("\"light\"".to_owned()),
        "global",
        WriteScope::Any { repo: false },
    )]));
    assert_eq!(slash_theme(&mut app), Effect::None);
    app.on_key(Key::Down, now());
    app.set_configure(Some(Arc::new(other) as Arc<dyn Configure>));
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(app.take_theme_choice(), Some(crate::ThemeSetting::Dark));

    // Enter saves the focused theme globally and queues it live: from the
    // saved dark entry one step down focuses light.
    let mut app = attached(Some(Arc::clone(&seam)));
    assert_eq!(slash_theme(&mut app), Effect::None);
    app.on_key(Key::Down, now());
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(
        seam.writes(),
        [(
            PathBuf::from("/w"),
            Layer::Global,
            "tui.theme".to_owned(),
            "\"light\"".to_owned()
        )]
    );
    assert_eq!(app.take_theme_choice(), Some(crate::ThemeSetting::Light));
    let saved = painted(&mut app, crate::ThemeSetting::Light);
    assert!(
        saved
            .backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.bg == Color::Rgb(0xff, 0xff, 0xff)),
        "enter leaves the saved theme painted"
    );
    insta::assert_snapshot!("theme_picker_enter_saved", painted_text(&saved));
}

#[test]
fn slash_theme_without_a_theme_row_opens_the_rows() {
    // With no `tui.theme` row the picker has nothing to choose from:
    // `/theme` opens the rows, as `/settings` does, and queues nothing.
    let seam = Arc::new(Fake::new(vec![row(
        "handoff.tokens",
        Shown::Value("400000".to_owned()),
        "default",
        WriteScope::Any { repo: true },
    )]));
    let mut app = attached(Some(Arc::clone(&seam)));
    assert_eq!(slash_theme(&mut app), Effect::None);
    assert!(app.config_view_open());
    assert_eq!(
        app.config_view_screen().map(|frame| frame.title),
        Some("Settings".to_owned())
    );
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.config_view_open());
    assert_eq!(app.take_theme_choice(), None);
    assert!(seam.writes().is_empty());
}

#[test]
fn slash_theme_restores_the_saved_entry_theme() {
    // The cancel restores the resolved entry setting, not a re-read:
    // with `tui.theme` saved as dark, Esc queues dark.
    let seam = Arc::new(Fake::new(vec![row(
        "tui.theme",
        Shown::Value("\"dark\"".to_owned()),
        "global",
        WriteScope::Any { repo: false },
    )]));
    let mut app = attached(Some(Arc::clone(&seam)));
    assert_eq!(slash_theme(&mut app), Effect::None);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(app.take_theme_choice(), Some(crate::ThemeSetting::Dark));
    assert!(seam.writes().is_empty());
}

/// Types `text` and presses Enter, returning the sent lines parsed.
fn enter_text(app: &mut App, text: &str) -> Vec<serde_json::Value> {
    for ch in text.chars() {
        app.on_key(Key::Char(ch), now());
    }
    match app.on_key(Key::Enter, now()) {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_default())
            .collect(),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_) => Vec::new(),
    }
}

#[test]
fn slash_theme_name_saves_applies_and_refuses() {
    let seam = Arc::new(theme_fake());

    // `/theme light` saves globally and queues the live theme, opening
    // nothing.
    let mut app = attached(Some(Arc::clone(&seam)));
    assert!(enter_text(&mut app, "/theme light").is_empty());
    assert!(!app.config_view_open());
    assert_eq!(
        seam.writes(),
        [(
            PathBuf::from("/w"),
            Layer::Global,
            "tui.theme".to_owned(),
            "\"light\"".to_owned()
        )]
    );
    assert_eq!(app.take_theme_choice(), Some(crate::ThemeSetting::Light));
    assert_eq!(app.input().expand(), "");
    let saved = painted(&mut app, crate::ThemeSetting::Light);
    assert!(
        saved
            .backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.bg == Color::Rgb(0xff, 0xff, 0xff)),
        "the named save paints the light page"
    );
    insta::assert_snapshot!("theme_named_light_saved", painted_text(&saved));

    // A spaced file theme saves whole.
    let mut app = attached(Some(Arc::clone(&seam)));
    assert!(enter_text(&mut app, "/theme solar dawn").is_empty());
    assert_eq!(
        seam.writes().last(),
        Some(&(
            PathBuf::from("/w"),
            Layer::Global,
            "tui.theme".to_owned(),
            "\"solar dawn\"".to_owned()
        ))
    );
    assert!(
        matches!(app.take_theme_choice(), Some(crate::ThemeSetting::File { ref name, .. }) if name == "solar dawn")
    );

    // An unknown name is refused with a notice naming it, changing nothing.
    let mut app = attached(Some(Arc::clone(&seam)));
    let writes = seam.writes().len();
    assert!(enter_text(&mut app, "/theme nope").is_empty());
    assert!(
        app.notices().iter().any(
            |notice| matches!(notice, crate::app::NoticeLine::Notice { code, text, .. } if code == "invalid_arguments" && text == "Unknown theme \"nope\".")
        ),
        "the refusal names the typed text"
    );
    assert_eq!(seam.writes().len(), writes);
    assert_eq!(app.take_theme_choice(), None);
    assert_eq!(app.input().expand(), "");

    // Case folds nothing: `Light` is unknown. Esc dismisses the
    // completion panel first: with it open, Enter would accept the
    // shown `light` candidate instead, as the accepted-name test proves.
    let mut app = attached(Some(Arc::clone(&seam)));
    for ch in "/theme Light".chars() {
        app.on_key(Key::Char(ch), now());
    }
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(
        app.notices().iter().any(
            |notice| matches!(notice, crate::app::NoticeLine::Notice { code, text, .. } if code == "invalid_arguments" && text == "Unknown theme \"Light\".")
        )
    );
    assert_eq!(seam.writes().len(), writes);
    assert_eq!(app.take_theme_choice(), None);
}
