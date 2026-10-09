//! Tests for `/skills` on the app: sending the `skills` command for the
//! session on screen, the answer matching its command and result kind,
//! and the switches reaching the view (`docs/tui.md`, "Swapped views").

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use super::super::{App, Effect};
use crate::Configure;
use crate::configure::{Shown, WriteScope};
use crate::configure_fake::{Fake, row};
use crate::home::Launch;
use crate::keys::{Edit, Key};
use crate::link::Line;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
fn now() -> Instant {
    contract::clock::Clock::now(fakes::clock::FakeClock::new().as_ref())
}

/// A seam over two rows.
fn fake() -> Arc<Fake> {
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
    ]))
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

/// An app on home with the hub up and attached to [`SESSION`].
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

/// Types `/skills` and presses Enter.
fn slash_skills(app: &mut App) -> Effect {
    for ch in "/skills".chars() {
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

/// Opens `/skills` on a connected app and returns the sent command's id.
fn open_skills(app: &mut App) -> String {
    let lines = sent(slash_skills(app));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "skills");
    assert_eq!(lines[0]["session_id"], SESSION);
    lines[0]["id"].as_str().unwrap_or_default().to_owned()
}

/// A `skills` answer with one repository skill.
fn skills_answer() -> serde_json::Value {
    serde_json::json!({"skills": [
        {"name": "tdd", "description": "Test first.",
         "path": "/w/.fiber/skills/tdd/SKILL.md", "source": "repository",
         "model_invocable": true, "disabled": false, "shadows": []}]})
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
fn slash_skills_with_no_session_is_the_notice_and_opens_nothing() {
    let mut app = home(Some(fake()));
    assert_eq!(slash_skills(&mut app), Effect::None);
    assert_eq!(app.notices.newest(), Some("No session on screen."));
    assert!(!app.config_view_open());
}

#[test]
fn slash_skills_sends_skills_for_the_session_on_screen() {
    let mut app = connected(Some(fake()));
    let id = open_skills(&mut app);
    assert!(id.starts_with("c_"), "{id}");
    assert!(app.config_view_open());
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.title, "Skills");
    assert!(
        frame
            .below
            .first()
            .is_some_and(|line| line == "Reading the skills…")
    );
}

#[test]
fn the_answer_to_the_sent_id_fills_the_view() {
    let mut app = connected(Some(fake()));
    let id = open_skills(&mut app);
    app.on_line(session_line(
        "command_accepted",
        serde_json::json!({"command_id": id, "result": skills_answer()}),
    ));
    let frame = app.config_view_screen().expect("the view is open");
    assert!(
        frame.rows.iter().any(|row| row[0].0.contains("tdd")),
        "{frame:?}"
    );
}

#[test]
fn a_wrong_kind_or_older_answer_through_the_app_is_ignored() {
    let mut app = connected(Some(fake()));
    let id = open_skills(&mut app);
    // The sent id with a `tools` result: not this view's kind.
    app.on_line(session_line(
        "command_accepted",
        serde_json::json!({"command_id": id, "result":
            {"tools": [{"name": "read", "source": "builtin",
                        "state": "full", "bytes": 10}]}}),
    ));
    let frame = app.config_view_screen().expect("the view is open");
    assert!(
        frame
            .below
            .first()
            .is_some_and(|line| line == "Reading the skills…"),
        "{frame:?}"
    );
    // A `skills` result for an older id.
    app.on_line(session_line(
        "command_accepted",
        serde_json::json!({"command_id": "c_other", "result": skills_answer()}),
    ));
    let frame = app.config_view_screen().expect("the view is open");
    assert!(
        frame
            .below
            .first()
            .is_some_and(|line| line == "Reading the skills…"),
        "{frame:?}"
    );
    app.on_line(session_line(
        "command_accepted",
        serde_json::json!({"command_id": id, "result": skills_answer()}),
    ));
    let frame = app.config_view_screen().expect("the view is open");
    assert!(
        frame.rows.iter().any(|row| row[0].0.contains("tdd")),
        "{frame:?}"
    );
}

#[test]
fn a_session_rejection_of_skills_shows_its_message() {
    let mut app = connected(Some(fake()));
    let id = open_skills(&mut app);
    app.on_line(session_line(
        "command_rejected",
        serde_json::json!({"command_id": id, "code": "busy", "message": "Busy."}),
    ));
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.below, vec!["Busy.".to_owned()]);
}

#[test]
fn a_hub_rejection_of_skills_shows_its_message() {
    let mut app = connected(Some(fake()));
    let id = open_skills(&mut app);
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
fn without_a_seam_skills_says_not_available() {
    let mut app = connected(None);
    assert_eq!(slash_skills(&mut app), Effect::None);
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.title, "Skills");
    assert_eq!(
        frame.below,
        vec!["Not available in this terminal.".to_owned()]
    );
}

#[test]
fn opening_skills_closes_the_open_view() {
    let mut app = connected(Some(fake()));
    for ch in "/settings".chars() {
        app.on_key(Key::Char(ch), now());
    }
    app.on_key(Key::Enter, now());
    assert_eq!(
        app.config_view_screen().map(|frame| frame.title),
        Some("Settings".to_owned())
    );
    // Keys land in the open view, so the second open goes straight
    // through: whatever was open closes, its unsaved edit dropped.
    app.open_config_view(super::ConfigView::Skills);
    assert_eq!(
        app.config_view_screen().map(|frame| frame.title),
        Some("Skills".to_owned())
    );
}

#[test]
fn file_closed_rereads_the_lists_and_the_text() {
    let seam = fake();
    let mut app = connected(Some(Arc::clone(&seam)));
    let id = open_skills(&mut app);
    app.on_line(session_line(
        "command_accepted",
        serde_json::json!({"command_id": id, "result": skills_answer()}),
    ));
    app.on_key(Key::Down, now());
    app.on_key(Key::Enter, now());
    assert_eq!(
        app.config_view_screen().map(|frame| frame.title),
        Some("Skill tdd".to_owned())
    );
    if let Ok(mut texts) = seam.texts.lock() {
        *texts = Ok("Edited text.".to_owned());
    }
    app.config_file_closed(Ok(()));
    let frame = app.config_view_screen().expect("the view is open");
    assert!(
        frame.rows.iter().any(|row| row[0].0 == "Edited text."),
        "{frame:?}"
    );
    // Back on the rows, the lists the file holds show.
    if let Ok(mut lists) = seam.skills_off.lock() {
        *lists = Ok(crate::configure::SkillsDisabled {
            project: vec!["tdd".to_owned()],
            everywhere: Vec::new(),
        });
    }
    app.on_key(Key::Esc, now());
    app.config_file_closed(Ok(()));
    let frame = app.config_view_screen().expect("the view is open");
    assert!(
        frame
            .rows
            .iter()
            .any(|row| row.iter().any(|(text, _)| text.contains("off"))),
        "{frame:?}"
    );
}

#[test]
fn left_and_right_reach_the_skills_view_not_the_draft() {
    let seam = fake();
    let mut app = connected(Some(Arc::clone(&seam)));
    let id = open_skills(&mut app);
    app.on_line(session_line(
        "command_accepted",
        serde_json::json!({"command_id": id, "result": skills_answer()}),
    ));
    app.on_key(Key::Down, now());
    app.on_edit(Edit::Right);
    app.on_key(Key::Char(' '), now());
    app.on_edit(Edit::Left);
    app.on_key(Key::Char(' '), now());
    let switched = seam.skill_switched.lock().unwrap().clone();
    assert_eq!(switched.len(), 2);
    assert_eq!(switched[0].2, crate::configure::SwitchScope::Everywhere);
    assert!(!switched[0].3);
    assert_eq!(switched[1].2, crate::configure::SwitchScope::Project);
    assert!(app.input().is_empty());
}
