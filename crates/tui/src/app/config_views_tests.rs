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
use crate::editor::OpenFile;
use crate::home::Launch;
use crate::keys::{Edit, Key, Mouse, MouseKind};
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
        Effect::OpenFile(OpenFile {
            path: file(Layer::Global),
            line: None
        })
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
        ..Default::default()
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
            let Effect::OpenFile(OpenFile { path: temp, .. }) = app.on_key(Key::CtrlG, now())
            else {
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
            let Effect::OpenFile(OpenFile { path: first, .. }) = app.on_key(Key::CtrlG, now())
            else {
                panic!("first copy")
            };
            let Effect::OpenFile(OpenFile { path: second, .. }) = app.on_key(Key::CtrlG, now())
            else {
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
            Effect::OpenFile(OpenFile {
                path: "/client/keys.json".into(),
                line: None
            })
        );
        assert_eq!(
            app.config_act(super::Act::OpenLocal(None)),
            Effect::OpenFile(OpenFile {
                path: file(Layer::Global),
                line: None
            })
        );
    });
    let mut local = attached(Some(fake()));
    assert_eq!(
        local.config_act(super::Act::OpenLocal(None)),
        Effect::OpenFile(OpenFile {
            path: file(Layer::Global),
            line: None
        })
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
            let Effect::OpenFile(OpenFile { path: temp, .. }) = app.on_key(Key::CtrlG, now())
            else {
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

/// A wheel report at (`col`, `row`): down steps toward the tail, up away
/// from it, over the conversation column the config views take.
fn wheel(col: u16, row: u16, down: bool) -> Mouse {
    Mouse {
        kind: if down {
            MouseKind::WheelDown
        } else {
            MouseKind::WheelUp
        },
        col,
        row,
    }
}

/// A seam over six settings rows, so the settings list fits three wheel
/// steps without clamping.
fn big_fake() -> Arc<Fake> {
    Arc::new(Fake::new(vec![
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
        row(
            "reviewer.model",
            Shown::Value("a/b".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "shell.approval",
            Shown::Value("auto".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "shell.executor",
            Shown::Value("worktree".to_owned()),
            "project",
            WriteScope::Any { repo: true },
        ),
        row(
            "context.reserve",
            Shown::Value("10%".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "handoff.dir",
            Shown::Value("auto".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "hub.badge",
            Shown::Value("auto".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "hub.cards",
            Shown::Value("4".to_owned()),
            "project",
            WriteScope::Any { repo: true },
        ),
        row(
            "keys.repeat",
            Shown::Value("50".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "models.recent",
            Shown::Value("5".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "panel.cards",
            Shown::Value("session".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "rail.shown",
            Shown::Value("auto".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "session.persist",
            Shown::Value("auto".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "shell.confinement",
            Shown::Value("auto".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "shell.tools",
            Shown::Value("auto".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "steering.attention",
            Shown::Value("auto".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "turns.budget",
            Shown::Value("auto".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "turns.cap",
            Shown::Value("auto".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "turns.follow",
            Shown::Value("auto".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "turns.hints",
            Shown::Value("auto".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "view.banner",
            Shown::Value("auto".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
        row(
            "view.gutter",
            Shown::Value("auto".to_owned()),
            "global",
            WriteScope::Any { repo: true },
        ),
    ]))
}

/// The wheel over the settings view scrolls the view's list, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_settings_scrolls_the_list_not_the_conversation() {
    let mut app = attached(Some(big_fake()));
    // Scroll first, while the wheel still reaches the conversation: the
    // view covers it once open. (The scroll-up must precede the open both
    // at base, where the wheel would reach the conversation, and after
    // the fix, where the view takes it.)
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    app.open_config_view(ConfigView::Settings);
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let frame = app.config_view_screen().expect("the view is open");
    // The gutter list holds a header row, so the first key row is 1.
    assert!(frame.rows.len() >= 5, "the list fits three steps");
    assert_eq!(frame.list.selected(), 4);
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        frame.list.selected(),
        4,
        "a selection still on screen stays where it was"
    );
}

/// The wheel over the theme picker moves its selection, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_theme_moves_the_picker_not_the_conversation() {
    let mut fake = Fake::new(vec![row(
        "tui.theme",
        Shown::Unset,
        "default",
        WriteScope::Any { repo: false },
    )]);
    fake.themes = vec![
        "solar".to_owned(),
        "dawn".to_owned(),
        "dusk".to_owned(),
        "noon".to_owned(),
        "night".to_owned(),
        "ember".to_owned(),
        "frost".to_owned(),
        "grove".to_owned(),
        "harbor".to_owned(),
        "ivory".to_owned(),
        "jungle".to_owned(),
        "kelp".to_owned(),
        "lagoon".to_owned(),
        "meadow".to_owned(),
        "nebula".to_owned(),
        "onyx".to_owned(),
        "prairie".to_owned(),
        "quartz".to_owned(),
        "reef".to_owned(),
        "sable".to_owned(),
    ];
    let mut app = attached(Some(Arc::new(fake)));
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    // Direct open: typing the slash command first would disturb the
    // scrolled conversation the wheel must leave alone.
    app.open_config_view(ConfigView::Theme);
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let frame = app.config_view_screen().expect("the view is open");
    assert!(frame.rows.len() >= 4, "the picker fits three steps");
    assert_eq!(frame.list.selected(), 3);
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        frame.list.selected(),
        3,
        "a selection still on screen stays where it was"
    );
}

/// A `tools` answer with one builtin and six grouped tools: the selection
/// rests on the first grouped row, and the grouped rows run consecutive
/// so three steps move it by exactly three.
fn six_tools_answer() -> serde_json::Value {
    serde_json::json!({"tools": [
        {"name": "read", "source": "builtin", "state": "full", "bytes": 10},
        {"name": "mcp__m__a", "source": "mcp", "server": "m", "tool": "a", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__b", "source": "mcp", "server": "m", "tool": "b", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__c", "source": "mcp", "server": "m", "tool": "c", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__d", "source": "mcp", "server": "m", "tool": "d", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__e", "source": "mcp", "server": "m", "tool": "e", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__f", "source": "mcp", "server": "m", "tool": "f", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__g", "source": "mcp", "server": "m", "tool": "g", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__h", "source": "mcp", "server": "m", "tool": "h", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__i", "source": "mcp", "server": "m", "tool": "i", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__j", "source": "mcp", "server": "m", "tool": "j", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__k", "source": "mcp", "server": "m", "tool": "k", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__l", "source": "mcp", "server": "m", "tool": "l", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__m", "source": "mcp", "server": "m", "tool": "m", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__n", "source": "mcp", "server": "m", "tool": "n", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__o", "source": "mcp", "server": "m", "tool": "o", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__p", "source": "mcp", "server": "m", "tool": "p", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__q", "source": "mcp", "server": "m", "tool": "q", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__r", "source": "mcp", "server": "m", "tool": "r", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__s", "source": "mcp", "server": "m", "tool": "s", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__t", "source": "mcp", "server": "m", "tool": "t", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__u", "source": "mcp", "server": "m", "tool": "u", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__v", "source": "mcp", "server": "m", "tool": "v", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__w", "source": "mcp", "server": "m", "tool": "w", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__x", "source": "mcp", "server": "m", "tool": "x", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__y", "source": "mcp", "server": "m", "tool": "y", "state": "deferred", "bytes": 1},
        {"name": "mcp__m__z", "source": "mcp", "server": "m", "tool": "z", "state": "deferred", "bytes": 1}]})
}

/// The wheel over the tools view moves its selection, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_tools_moves_the_selection_not_the_conversation() {
    // Connected: the open sends its `tools` command through the hub.
    let mut app = connected(Some(fake()));
    app.set_size(80, 24);
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    // Direct open: typing the slash command first would disturb the
    // scrolled conversation the wheel must leave alone.
    let lines = sent(app.open_config_view(ConfigView::Tools));
    assert_eq!(lines.len(), 1);
    let id = lines[0]["id"].as_str().unwrap_or_default().to_owned();
    app.on_line(session_line(
        "command_accepted",
        serde_json::json!({"command_id": id, "result": six_tools_answer()}),
    ));
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let frame = app.config_view_screen().expect("the view is open");
    let before = frame.list.selected();
    assert!(frame.rows.len() >= before + 4, "the list fits three steps");
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        frame.list.selected(),
        before,
        "a selection still on screen stays where it was"
    );
}

/// A `skills` answer with six repository skills, so three steps fit.
fn six_skills_answer() -> serde_json::Value {
    serde_json::json!({"skills": [
        {"name": "a", "description": "A.", "path": "/w/.fiber/skills/a/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "b", "description": "B.", "path": "/w/.fiber/skills/b/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "c", "description": "C.", "path": "/w/.fiber/skills/c/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "d", "description": "D.", "path": "/w/.fiber/skills/d/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "e", "description": "E.", "path": "/w/.fiber/skills/e/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "f", "description": "F.", "path": "/w/.fiber/skills/f/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "g", "description": "G.", "path": "/w/.fiber/skills/g/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "h", "description": "H.", "path": "/w/.fiber/skills/h/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "i", "description": "I.", "path": "/w/.fiber/skills/i/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "j", "description": "J.", "path": "/w/.fiber/skills/j/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "k", "description": "K.", "path": "/w/.fiber/skills/k/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "l", "description": "L.", "path": "/w/.fiber/skills/l/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "m", "description": "M.", "path": "/w/.fiber/skills/m/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "n", "description": "N.", "path": "/w/.fiber/skills/n/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "o", "description": "O.", "path": "/w/.fiber/skills/o/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "p", "description": "P.", "path": "/w/.fiber/skills/p/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "q", "description": "Q.", "path": "/w/.fiber/skills/q/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "r", "description": "R.", "path": "/w/.fiber/skills/r/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "s", "description": "S.", "path": "/w/.fiber/skills/s/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "t", "description": "T.", "path": "/w/.fiber/skills/t/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "u", "description": "U.", "path": "/w/.fiber/skills/u/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "v", "description": "V.", "path": "/w/.fiber/skills/v/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "w", "description": "W.", "path": "/w/.fiber/skills/w/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "x", "description": "X.", "path": "/w/.fiber/skills/x/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "y", "description": "Y.", "path": "/w/.fiber/skills/y/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []},
        {"name": "z", "description": "Z.", "path": "/w/.fiber/skills/z/SKILL.md",
         "source": "repository", "model_invocable": true, "disabled": false, "shadows": []}]})
}

/// The wheel over the skills view scrolls the view's list, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_skills_scrolls_the_list_not_the_conversation() {
    let mut app = connected(Some(fake()));
    app.set_size(80, 24);
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    // Direct open: typing the slash command first would disturb the
    // scrolled conversation the wheel must leave alone.
    let lines = sent(app.open_config_view(ConfigView::Skills));
    assert_eq!(lines.len(), 1);
    let id = lines[0]["id"].as_str().unwrap_or_default().to_owned();
    app.on_line(session_line(
        "command_accepted",
        serde_json::json!({"command_id": id, "result": six_skills_answer()}),
    ));
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let frame = app.config_view_screen().expect("the view is open");
    let before = frame.list.selected();
    assert!(frame.rows.len() >= before + 4, "the list fits three steps");
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        frame.list.selected(),
        before,
        "a selection still on screen stays where it was"
    );
}

/// The wheel over the rules view scrolls the view's list, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_rules_scrolls_the_list_not_the_conversation() {
    let seam = fake();
    if let Ok(mut rules) = seam.rules.lock() {
        let section = |file: &str, texts: &[&str]| crate::configure::RulesSection {
            file: std::path::PathBuf::from(file),
            rows: Ok(texts
                .iter()
                .enumerate()
                .map(|(at, text)| crate::configure::RuleRow {
                    line: at + 1,
                    text: (*text).to_owned(),
                    rule: contract::Rule {
                        decision: contract::RuleDecision::Allow,
                        tool: "shell".to_owned(),
                        prefix: "echo".to_owned(),
                        added: None,
                        session_id: None,
                    },
                })
                .collect()),
        };
        *rules = Ok((
            section(
                "/w/.fiber/rules.md",
                &[
                    "- a", "- b", "- c", "- d", "- e", "- f", "- g", "- h", "- i", "- j", "- k",
                    "- l", "- m", "- n", "- o", "- p", "- q", "- r", "- s", "- t",
                ],
            ),
            section("/global.md", &["- m", "- n", "- o", "- p", "- q"]),
        ));
    }
    let mut app = attached(Some(seam));
    app.set_size(80, 24);
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    app.open_config_view(ConfigView::Rules);
    assert!(app.config_view_open(), "the rules view opened");
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let frame = app.config_view_screen().expect("the view is open");
    let before = frame.list.selected();
    assert!(frame.rows.len() >= before + 4, "the list fits three steps");
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        frame.list.selected(),
        before,
        "a selection still on screen stays where it was"
    );
}

/// The wheel over the login view scrolls the view's list, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_login_scrolls_the_list_not_the_conversation() {
    let seam = fake();
    if let Ok(mut targets) = seam.targets.lock() {
        *targets = Ok((0..30)
            .map(|at| crate::configure::LoginTarget {
                name: format!("p{at}"),
                kind: crate::configure::LoginKind::Key,
            })
            .collect());
    }
    let mut app = attached(Some(Arc::clone(&seam)));
    app.set_size(80, 24);
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    app.open_config_view(ConfigView::Login);
    assert!(app.config_view_open(), "the login view opened");
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    // Three Downs put the focus on p3's row and the wheel keeps it
    // there while the window moves: Enter opens p3's key field, the
    // paste fills it, and a second Enter stores p3's key, not p0's.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    app.on_edit(Edit::Paste("s3\n".to_owned()));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(seam.stores(), [("p3".to_owned(), None)]);
}

/// The wheel over the logout view scrolls the view's list, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_logout_scrolls_the_list_not_the_conversation() {
    let seam = fake();
    if let Ok(mut rows) = seam.logout.lock() {
        *rows = Ok((0..30)
            .map(|at| crate::configure::LogoutRow::Provider {
                name: "acme".to_owned(),
                label: format!("l{at:02}"),
                default: at == 0,
                deletable: true,
                origin: None,
                siblings: Vec::new(),
            })
            .collect());
    }
    let mut app = attached(Some(seam));
    app.set_size(80, 24);
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    app.open_config_view(ConfigView::Logout);
    assert!(app.config_view_open(), "the logout view opened");
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    // Three Downs put the focus on l03's row and the wheel keeps it
    // there while the window moves: Enter asks to delete l03, not the
    // still-focused l00 the base wheel would keep.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let shown = screen(&app).join("\n");
    assert!(shown.contains("Delete acme/l03?"), "{shown}");
}

/// An `mcp_servers` answer with six servers, so three steps fit.
fn six_servers_answer() -> serde_json::Value {
    serde_json::json!({"servers": [
        {"name": "a", "layer": "project", "state": "failed",
         "reason": "refused a", "tools": 2, "approved": true, "auth_url": null},
        {"name": "b", "layer": "project", "state": "failed",
         "reason": "refused b", "tools": 2, "approved": true, "auth_url": null},
        {"name": "c", "layer": "project", "state": "failed",
         "reason": "refused c", "tools": 2, "approved": true, "auth_url": null},
        {"name": "d", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 d", "tools": 0, "approved": true, "auth_url": null},
        {"name": "e", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 e", "tools": 0, "approved": true, "auth_url": null},
        {"name": "f", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 f", "tools": 0, "approved": true, "auth_url": null},
        {"name": "g", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 g", "tools": 0, "approved": true, "auth_url": null},
        {"name": "h", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 h", "tools": 0, "approved": true, "auth_url": null},
        {"name": "i", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 i", "tools": 0, "approved": true, "auth_url": null},
        {"name": "j", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 j", "tools": 0, "approved": true, "auth_url": null},
        {"name": "k", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 k", "tools": 0, "approved": true, "auth_url": null},
        {"name": "l", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 l", "tools": 0, "approved": true, "auth_url": null},
        {"name": "m", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 m", "tools": 0, "approved": true, "auth_url": null},
        {"name": "n", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 n", "tools": 0, "approved": true, "auth_url": null},
        {"name": "o", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 o", "tools": 0, "approved": true, "auth_url": null},
        {"name": "p", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 p", "tools": 0, "approved": true, "auth_url": null},
        {"name": "q", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 q", "tools": 0, "approved": true, "auth_url": null},
        {"name": "r", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 r", "tools": 0, "approved": true, "auth_url": null},
        {"name": "s", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 s", "tools": 0, "approved": true, "auth_url": null},
        {"name": "t", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 t", "tools": 0, "approved": true, "auth_url": null},
        {"name": "u", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 u", "tools": 0, "approved": true, "auth_url": null},
        {"name": "v", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 v", "tools": 0, "approved": true, "auth_url": null},
        {"name": "w", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 w", "tools": 0, "approved": true, "auth_url": null},
        {"name": "x", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 x", "tools": 0, "approved": true, "auth_url": null},
        {"name": "y", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 y", "tools": 0, "approved": true, "auth_url": null},
        {"name": "z", "layer": "global", "state": "needs-login",
         "reason": "HTTP 401 z", "tools": 0, "approved": true, "auth_url": null}]})
}

/// The wheel over the MCP view scrolls the view's list, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_mcp_scrolls_the_list_not_the_conversation() {
    let mut app = connected(Some(fake()));
    app.set_size(80, 24);
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    // Direct open: typing the slash command first would disturb the
    // scrolled conversation the wheel must leave alone.
    let lines = sent(app.open_config_view(ConfigView::Mcp));
    assert_eq!(lines.len(), 1);
    let id = lines[0]["id"].as_str().unwrap_or_default().to_owned();
    app.on_line(session_line(
        "command_accepted",
        serde_json::json!({"command_id": id, "result": six_servers_answer()}),
    ));
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let frame = app.config_view_screen().expect("the view is open");
    let before = frame.list.selected();
    assert!(frame.rows.len() >= before + 4, "the list fits three steps");
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        frame.list.selected(),
        before,
        "a selection still on screen stays where it was"
    );
}

/// The wheel over the credential picker routes through the config path
/// and leaves the conversation alone (`docs/tui.md`, "Swapped views";
/// see #3578). The picker holds no rows before its answer, so there is
/// no list move to pin.
#[test]
fn wheel_over_credential_leaves_the_conversation() {
    let mut app = connected(Some(fake()));
    app.set_size(80, 24);
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    let lines = sent(app.open_config_view(ConfigView::Credential));
    assert_eq!(lines.len(), 1);
    assert!(app.config_view_open(), "the credential picker opened");
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
}

/// The wheel over the doctor view scrolls the view's list, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_doctor_scrolls_the_list_not_the_conversation() {
    let mut app = attached(Some(fake()));
    let report = (0..60).map(|n| format!("line {n:02}")).collect::<Vec<_>>();
    let report = report.join("\n");
    app.set_doctor(Some(Arc::new(move || Ok(report.clone()))));
    app.set_size(80, 24);
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    app.open_config_view(ConfigView::Doctor);
    assert!(app.config_view_open(), "the doctor view opened");
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    let frame = app.config_view_screen().expect("the view is open");
    let before = frame.list.selected();
    assert!(frame.rows.len() >= before + 4, "the list fits three steps");
    let shown_top = frame.list.top();
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(
        frame.list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        frame.list.selected(),
        before,
        "a selection still on screen stays where it was"
    );
}
