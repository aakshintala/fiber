//! Tests for the configuration views on the app: opening `/settings` on
//! home and attached, the keys a view takes and the ones it leaves, the
//! seam's absence, the editor's return and the theme a choice queues
//! (`docs/tui.md`, "Swapped views").

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

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
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
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
        rows.first().is_some_and(|row| row.starts_with("Settings")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.starts_with("handoff.tokens")),
        "{rows:?}"
    );
    assert_eq!(seam.reads(), [PathBuf::from("/w")]);

    let mut app = attached(Some(seam));
    slash_settings(&mut app);
    assert!(app.config_view_open());
    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row.starts_with("Settings")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.starts_with("handoff.tokens")),
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
        app.push_notice("The theme solar floats above.".to_owned());
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
            "Applies on /reload, which rebuilds the cache: about 201,034 tokens.".to_owned()
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
    assert!(rows.iter().any(|row| row == "▌ ›"), "{rows:#?}");
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
        Some(vec!["Applies on each session's next /reload.".to_owned()])
    );
}

#[test]
fn a_save_says_the_reload_cost_of_the_session_on_screen() {
    for (on_home, said) in [
        (
            false,
            "Applies on /reload, which rebuilds the cache: about 201,034 tokens.",
        ),
        (true, "Applies on each session's next /reload."),
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
        frame.rows.iter().any(|row| row[0].0.contains("read")),
        "{frame:?}"
    );
    assert!(
        frame.rows.iter().any(|row| row[0].0.contains('x')),
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
    let mut app = connected(Some(fake()));
    let id = open_tools(&mut app);
    app.on_line(session_line(
        "command_accepted",
        serde_json::json!({"command_id": id, "result": tools_answer()}),
    ));
    // Select the MCP row: heading, read, heading, x.
    for _ in 0..3 {
        app.on_key(Key::Down, now());
    }
    app.on_edit(Edit::Right);
    let frame = app.config_view_screen().expect("the view is open");
    assert!(frame.rows[3][3].0.contains('›'), "{frame:?}");
    assert!(app.input().is_empty());
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
        rows.first().is_some_and(|row| row.starts_with("Rules")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.starts_with("Global rules")),
        "{rows:?}"
    );
    assert_eq!(seam.reads(), [PathBuf::from("/w")]);

    let mut app = attached(Some(seam));
    slash_rules(&mut app);
    assert!(app.config_view_open());
    let rows = screen(&app);
    assert!(rows.iter().any(|row| row.starts_with("Rules")), "{rows:?}");
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
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
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
