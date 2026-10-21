//! Tests for the built-in commands: dispatch through the table and
//! the lines Enter and Esc send.

use super::commands_testkit::{
    SESSION, answer_commands, answer_rows, attached, connected, enter, hub_line, image_part, now,
    paste_image, sent, session_line, shown, start_accepted, turn_starts, type_text,
};
use crate::app::{App, Effect};
use crate::keys::Key;
use crate::link::Line;
use serde_json::{Value, json};
use std::path::PathBuf;

#[test]
fn remote_enter_needs_a_workspace_before_a_model_and_keeps_the_prompt() {
    for model in [None, Some("test/model".to_owned())] {
        let mut app = connected();
        type_text(&mut app, "keep this draft");
        app.set_home(crate::home::Launch {
            location: crate::LaunchLocation::Remote {
                hub: "box".to_owned(),
            },
            model,
            ..Default::default()
        });
        // Enter sends only the open picker's home listing, never a start.
        let asks = sent(app.on_enter());
        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0]["command"], "directories");
        assert_eq!(asks[0]["args"]["path"], "~/");
        assert_eq!(app.draft(), "keep this draft");
        assert!(app.home_screen().unwrap().picker.is_some());
        assert!(!app.model_picker_open());
        app.disconnected();
        assert_eq!(app.on_enter(), Effect::None);
        assert_eq!(app.draft(), "keep this draft");
        assert!(app.home_screen().unwrap().picker.is_some());
        app.on_line(Line::Hub(contract::HubLine {
            kind: "hub_hello".to_owned(),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            payload: serde_json::Map::new(),
        }));
        app.on_line(session_line("session_status", json!({
            "name":"chosen", "workspace":"/remote/chosen", "project":"p", "state":"idle", "since":0,
            "started":0, "calls":0, "delegates":0, "jobs":0, "clients":0,
            "model":"test/model", "spend":{"tokens":{"input":0,"output":0,"cache_read":0,"cache_write":{}},"cost":0.0,"subscription_cost":0.0}
        })));
        app.home_click(crate::home::Spot::Pick(0));
        // A model is now present, so the selected workspace can start.
        app.home.as_mut().unwrap().launch.model = Some("test/model".to_owned());
        let lines = sent(app.on_enter());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["command"], "start");
        assert_eq!(lines[0]["args"]["workspace"], "/remote/chosen");
    }
}

#[test]
fn handoff_sends_its_instructions() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/handoff  do x "));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "handoff");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0]["args"], json!({"instructions": "do x"}));
    assert_eq!(app.draft(), "");
    let lines = sent(enter(&mut app, "/handoff"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "handoff");
    assert_eq!(lines[0].get("args"), None);
}

#[test]
fn the_slash_list_shows_the_name_row() {
    let mut app = connected();
    type_text(&mut app, "/nam");
    assert_eq!(
        shown(&app),
        ["/name /rename <text>  Names the session.  command"]
    );
}

#[test]
fn reload_sends_reload_without_args() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/reload"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "reload");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0].get("args"), None);
}

#[test]
fn import_sends_import_without_args_when_bare() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "import");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0].get("args"), None);
}

#[test]
fn import_sends_its_source_when_named() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import codex "));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "import");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0]["args"], json!({"source": "codex"}));
}

#[test]
fn an_accepted_import_reports_every_line_with_a_reload_reminder() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import codex"));
    let id = lines[0]["id"].clone();
    let accepted = session_line(
        "command_accepted",
        json!({
            "command_id": id,
            "result": {
                "imported": {
                    "mcp_servers": ["alpha"],
                    "skills": ["lint"],
                    "commands": ["plan"],
                    "instructions": ["AGENTS.md"],
                },
                "left_behind": [{"item": "auth.json", "reason": "credentials are never imported"}],
            },
        }),
    );
    assert!(app.on_line(accepted).is_empty());
    // At most three notices show: the first three report lines, then the
    // count of the rest.
    assert_eq!(
        notice_texts(&app),
        [
            "imported imported skill lint",
            "imported imported command plan",
            "imported imported instruction AGENTS.md",
            "+3 more",
        ]
    );
    assert_eq!(
        app.notice(),
        Some("Run /reload to apply the imported items.")
    );
}

#[test]
fn an_accepted_import_without_cuts_shows_every_line() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import"));
    let id = lines[0]["id"].clone();
    let accepted = session_line(
        "command_accepted",
        json!({
            "command_id": id,
            "result": {
                "imported": {
                    "mcp_servers": ["alpha"],
                    "skills": [],
                    "commands": [],
                    "instructions": [],
                },
                "left_behind": [{"item": "auth.json", "reason": "credentials are never imported"}],
            },
        }),
    );
    assert!(app.on_line(accepted).is_empty());
    assert_eq!(
        notice_texts(&app),
        [
            "imported imported server alpha",
            "imported left behind auth.json: credentials are never imported",
            "imported Run /reload to apply the imported items.",
        ]
    );
}

/// Every notice showing, oldest first, each with its code: what the
/// person reads after an accepted import.
fn notice_texts(app: &App) -> Vec<String> {
    app.notices()
        .iter()
        .map(|line| match line {
            crate::app::NoticeLine::Notice { code, text, .. } => format!("{code} {text}"),
            crate::app::NoticeLine::More(n) => format!("+{n} more"),
        })
        .collect()
}

#[test]
fn a_rejected_import_keeps_the_existing_notice_path() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import vim"));
    let id = lines[0]["id"].clone();
    let rejected = session_line(
        "command_rejected",
        json!({"command_id": id, "code": "invalid_arguments", "message": "No."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.draft(), "/import vim");
    assert_eq!(app.notice(), Some("No."));
}

#[test]
fn a_rejected_handoff_returns_its_draft_with_the_notice() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/handoff do x"));
    let id = lines[0]["id"].clone();
    let rejected = session_line(
        "command_rejected",
        json!({"command_id": id, "code": "busy", "message": "Busy."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.draft(), "/handoff do x");
    assert_eq!(app.notice(), Some("Busy."));
}

#[test]
fn a_command_with_no_session_says_so() {
    for command in [
        "/handoff x",
        "/name x",
        "/reload",
        "/import",
        "/import codex",
        "/close",
    ] {
        let mut app = connected();
        assert_eq!(enter(&mut app, command), Effect::None, "{command}");
        assert_eq!(app.notice(), Some("No session on screen."), "{command}");
        assert_eq!(app.draft(), "", "{command}");
    }
}

#[test]
fn a_command_with_the_link_down_keeps_its_draft() {
    for command in [
        "/handoff x",
        "/name x",
        "/reload",
        "/import",
        "/import codex",
        "/close",
    ] {
        let mut app = attached();
        app.disconnected();
        assert_eq!(enter(&mut app, command), Effect::None, "{command}");
        assert_eq!(app.draft(), command, "{command}");
        assert!(app.session().is_some(), "{command}");
    }
}

#[test]
fn close_while_idle_sends_close_and_goes_home() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/close"));
    let commands: Vec<&Value> = lines.iter().map(|line| &line["command"]).collect();
    assert_eq!(commands, [&json!("close")]);
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0]["args"], json!({"now": true}));
    assert_eq!(app.session(), None);
}

#[test]
fn a_rejected_close_gives_only_its_notice_on_the_home_draft() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/close"));
    let rejected = contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": lines[0]["id"], "code": "busy", "message": "Busy."})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    };
    assert!(app.on_line(Line::Hub(rejected)).is_empty());
    assert_eq!(app.draft(), "");
    assert_eq!(app.notice(), Some("Busy."));
}

#[test]
fn close_during_a_turn_sends_only_close_with_now() {
    let mut app = attached();
    turn_starts(&mut app);
    let lines = sent(enter(&mut app, "/close"));
    let commands: Vec<&Value> = lines.iter().map(|line| &line["command"]).collect();
    assert_eq!(commands, [&json!("close")]);
    assert!(lines.iter().all(|line| line["session_id"] == SESSION));
    assert_eq!(lines[0]["args"], json!({"now": true}));
    assert_eq!(app.session(), None);
    assert!(app.lines().is_empty());
}

#[test]
fn quit_quits() {
    let mut app = connected();
    assert_eq!(enter(&mut app, "/quit"), Effect::Quit);
}

#[test]
fn home_and_new_return_to_the_screen_before_a_session() {
    for command in ["/home", "/new"] {
        let mut app = attached();
        answer_commands(&mut app, &["tdd"]);
        turn_starts(&mut app);
        app.set_size(80, 3);
        app.on_key(Key::PageUp, now());
        assert_eq!(enter(&mut app, command), Effect::None, "{command}");
        assert_eq!(app.session(), None, "{command}");
        assert!(app.lines().is_empty(), "{command}");
        assert_eq!(app.top(), None, "{command}");
        assert_eq!(app.draft(), "", "{command}");
        // The old session's skills are gone with it: `/td` finds
        // nothing, so the panel holds only its dim row.
        type_text(&mut app, "/td");
        let completions = app.completions().expect("the dim row shows");
        assert_eq!(completions.lines(), ["no matches"], "{command}");
        assert_eq!(completions.selected, None, "{command}");
        app.on_key(Key::Esc, now());
        app.on_key(Key::CtrlC, now());
        // The next Enter starts a new session.
        let lines = sent(enter(&mut app, "hi"));
        assert_eq!(lines.len(), 1, "{command}");
        assert_eq!(lines[0]["command"], "start", "{command}");
    }
}

#[test]
fn home_while_start_is_pending_only_clears_the_draft() {
    let mut app = connected();
    let lines = sent(enter(&mut app, "hi"));
    assert_eq!(lines[0]["command"], "start");
    assert_eq!(enter(&mut app, "/home"), Effect::None);
    assert_eq!(app.draft(), "");
    // Still waiting on that `start`: Enter sends nothing and keeps the
    // draft, where the screen before a session would send `start`.
    assert_eq!(enter(&mut app, "again"), Effect::None);
    assert_eq!(app.draft(), "again");
}

#[test]
fn an_unknown_command_goes_out_as_written_before_the_answer() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/foo bar"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert_eq!(lines[0]["args"]["content"][0]["text"], "/foo bar");
    turn_starts(&mut app);
    let lines = sent(enter(&mut app, "/foo"));
    assert_eq!(lines[0]["command"], "steer");
    assert_eq!(lines[0]["args"]["content"][0]["text"], "/foo");
}

#[test]
fn start_carries_no_content() {
    let mut app = App::new(PathBuf::from("/w"));
    // Held before `hub_hello`, sent after it.
    assert_eq!(enter(&mut app, "hi"), Effect::None);
    let hello = hub_line("hub_hello", json!({}));
    let lines: Vec<Value> = app
        .on_line(hello)
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    assert_eq!(lines[0]["args"], json!({"workspace": "/w"}));
}

#[test]
fn an_accepted_start_subscribes_then_prompts() {
    let mut app = connected();
    let lines = start_accepted(&mut app);
    let commands: Vec<&Value> = lines.iter().map(|line| &line["command"]).collect();
    assert_eq!(
        commands,
        [&json!("subscribe"), &json!("commands"), &json!("prompt")]
    );
    assert_eq!(lines[0]["args"]["level"], "full");
    assert_eq!(lines[2]["session_id"], SESSION);
    assert_eq!(
        lines[2]["args"],
        json!({"content": [{"type": "text", "text": "hi"}]})
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

#[test]
fn a_rejected_first_prompt_returns_its_text() {
    let mut app = connected();
    let lines = start_accepted(&mut app);
    let rejected = session_line(
        "command_rejected",
        json!({"command_id": lines[2]["id"], "code": "busy", "message": "Busy."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.notice(), Some("Busy."));
    assert_eq!(app.draft(), "hi");
}

#[test]
fn a_rejected_start_still_returns_its_text() {
    let mut app = connected();
    let lines = sent(enter(&mut app, "hi"));
    let rejected = hub_line(
        "command_rejected",
        json!({"command_id": lines[0]["id"], "code": "invalid_arguments", "message": "No."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.notice(), Some("No."));
    assert_eq!(app.draft(), "hi");
    assert_eq!(app.session(), None);
}

#[test]
fn a_slash_draft_with_an_image_is_a_prompt() {
    // E1: "/name x" would name the session, but an image makes it a prompt.
    let mut app = attached();
    type_text(&mut app, "/name x");
    paste_image(&mut app);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert_eq!(
        lines[0]["args"],
        json!({"content": [{"type": "text", "text": "/name x"}, image_part()]})
    );
}

#[test]
fn a_bang_draft_with_an_image_is_a_prompt() {
    // E2: "!echo hi" would run a shell command, but an image makes it a
    // prompt.
    let mut app = attached();
    type_text(&mut app, "!echo hi");
    paste_image(&mut app);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert_eq!(
        lines[0]["args"],
        json!({"content": [{"type": "text", "text": "!echo hi"}, image_part()]})
    );
}

#[test]
fn an_image_only_draft_sends_one_image_part() {
    // E3.
    let mut app = attached();
    paste_image(&mut app);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert_eq!(lines[0]["args"], json!({"content": [image_part()]}));
}

#[test]
fn a_first_prompt_carries_the_image() {
    // E4: the draft sent before a session starts rides the first prompt.
    let mut app = connected();
    type_text(&mut app, "look");
    paste_image(&mut app);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    let accepted = hub_line(
        "command_accepted",
        json!({"command_id": lines[0]["id"], "result": {"session_id": SESSION}}),
    );
    let out: Vec<Value> = app
        .on_line(accepted)
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert_eq!(out.len(), 3);
    assert_eq!(out[2]["command"], "prompt");
    assert_eq!(
        out[2]["args"],
        json!({"content": [{"type": "text", "text": "look"}, image_part()]})
    );
}

#[test]
fn a_busy_session_gets_the_image_as_steer() {
    // E5.
    let mut app = attached();
    turn_starts(&mut app);
    type_text(&mut app, "look");
    paste_image(&mut app);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "steer");
    assert_eq!(
        lines[0]["args"],
        json!({"content": [{"type": "text", "text": "look"}, image_part()]})
    );
}

#[test]
fn a_rejected_prompt_returns_its_image() {
    // E6: the draft comes back with its image, the cursor at its end.
    let mut app = attached();
    type_text(&mut app, "look");
    paste_image(&mut app);
    let lines = sent(app.on_key(Key::Enter, now()));
    let rejected = session_line(
        "command_rejected",
        json!({"command_id": lines[0]["id"], "code": "busy", "message": "Busy."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.draft(), "look[Image #1]");
    assert_eq!(app.on_key(Key::Char('!'), now()), Effect::None);
    assert_eq!(app.draft(), "look[Image #1]!");
}

#[test]
fn an_unwritten_prompt_returns_its_image() {
    // E7: a prompt never written returns its image too.
    let mut app = attached();
    type_text(&mut app, "look");
    paste_image(&mut app);
    let Effect::Send(lines) = app.on_key(Key::Enter, now()) else {
        panic!("Enter sends the prompt");
    };
    app.write_failed(&lines);
    assert_eq!(app.draft(), "look[Image #1]");
}

#[test]
fn close_with_a_running_job_and_delegate_sends_one_close_with_now() {
    let mut app = attached();
    app.on_line(session_line(
        "job_started",
        json!({"job_id": "j_1", "description": "build", "output_path": "/tmp/out"}),
    ));
    app.on_line(session_line(
        "job_started",
        json!({"job_id": "j_2", "description": "review", "output_path": "/tmp/out"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": "j_2",
            "delegate_session_id": "s_dddddddddddddddd",
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
    // The delegate's view is open: `/close` closes it with the parent.
    let _ = app.open_item(&contract::JobId("j_2".to_owned()));
    assert!(app.item_open());
    let lines = sent(enter(&mut app, "/close"));
    let commands: Vec<&Value> = lines.iter().map(|line| &line["command"]).collect();
    assert_eq!(commands, [&json!("close")]);
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0]["args"], json!({"now": true}));
    assert_eq!(app.session(), None);
    assert!(!app.item_open());
    assert!(app.lines().is_empty());
}

#[test]
fn an_extension_command_sends_command_with_text() {
    let mut app = attached();
    answer_rows(
        &mut app,
        json!([{"name": "deploy", "description": "Deploys.", "argument_hint": "[env]",
            "tag": "acme", "extension": true}]),
    );
    let lines = sent(enter(&mut app, "/deploy now"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "command");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0]["args"], json!({"name": "deploy", "text": "now"}));
    let lines = sent(enter(&mut app, "/deploy"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "command");
    assert_eq!(lines[0]["args"], json!({"name": "deploy"}));
    assert!(lines[0]["args"].get("text").is_none());
}

#[test]
fn an_extension_command_sends_command_during_a_turn() {
    let mut app = attached();
    answer_rows(
        &mut app,
        json!([{"name": "deploy", "description": "Deploys.", "tag": "acme", "extension": true}]),
    );
    turn_starts(&mut app);
    let lines = sent(enter(&mut app, "/deploy now"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "command");
    assert_eq!(lines[0]["args"], json!({"name": "deploy", "text": "now"}));
}

#[test]
fn an_extension_model_takes_the_built_in_slot_and_dispatch() {
    let mut app = attached();
    answer_rows(
        &mut app,
        json!([{"name": "model", "description": "Picks a fast model.", "argument_hint": "[speed]",
            "tag": "acme", "extension": true}]),
    );
    type_text(&mut app, "/model");
    let rows = shown(&app);
    assert_eq!(
        rows.first().cloned(),
        Some("/model [speed]  Picks a fast model.  acme".to_owned()),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .all(|row| *row != "/model  Opens the model picker.  command"),
        "{rows:?}"
    );
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "command");
    assert_eq!(lines[0]["args"], json!({"name": "model"}));
    assert!(app.keymap().is_none());
}

#[test]
fn a_skill_named_like_a_built_in_keeps_the_built_in() {
    let mut app = attached();
    answer_rows(
        &mut app,
        json!([{"name": "model", "description": "The model skill.", "tag": "skill"}]),
    );
    let lines = sent(enter(&mut app, "/model"));
    assert_eq!(lines.len(), 0);
    assert!(app.model_picker_open());
}

#[test]
fn an_undrawn_built_in_goes_out_as_typed_before_the_answer() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/rewind"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert!(app.keymap().is_none());
}

#[test]
fn every_table_row_dispatches_and_undrawn_built_ins_fall_through_before_the_answer() {
    for entry in crate::app::commands::TABLE {
        let mut app = attached();
        type_text(&mut app, &format!("/{}", entry.name));
        assert!(app.built_in().is_some(), "/{}", entry.name);
    }
    let mut app = attached();
    type_text(&mut app, "/rewind");
    assert!(app.built_in().is_none());
}

#[test]
fn a_rejected_extension_command_returns_its_draft_with_the_notice() {
    let mut app = attached();
    answer_rows(
        &mut app,
        json!([{"name": "deploy", "description": "Deploys.", "tag": "acme", "extension": true}]),
    );
    let lines = sent(enter(&mut app, "/deploy now"));
    assert_eq!(lines.len(), 1);
    let rejected = session_line(
        "command_rejected",
        json!({"command_id": lines[0]["id"], "code": "denied", "message": "No."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.draft(), "/deploy now");
    assert_eq!(app.notice(), Some("No."));
}

/// A writer answering `done` for every description.
fn feedback_writer(done: crate::FeedbackDone) -> crate::WriteFeedback {
    std::sync::Arc::new(move |_: &str| Ok(done.clone()))
}

fn feedback_done() -> crate::FeedbackDone {
    crate::FeedbackDone {
        path: PathBuf::from("/w/fiber-feedback-7.md"),
        link: "https://example.test/new?title=x".to_owned(),
    }
}

/// The conversation screen's text at 80 by 24.
fn screen_text(app: &App) -> String {
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

#[test]
fn feedback_without_a_description_asks_for_one() {
    let mut app = attached();
    assert_eq!(enter(&mut app, "/feedback"), Effect::None);
    assert_eq!(app.draft(), "/feedback ");
    assert_eq!(
        app.notice(),
        Some("Describe the feedback after /feedback, then press Enter.")
    );
}

#[test]
fn feedback_without_a_writer_lands_the_notice_through_the_loop() {
    use std::sync::mpsc;
    let mut app = attached();
    let Effect::Feedback { description } = enter(&mut app, "/feedback the sound") else {
        panic!("feedback effect");
    };
    assert_eq!(description, "the sound");
    // No writer, so nothing starts: the loop lands the same notice.
    let (tx, _rx) = mpsc::channel();
    assert_eq!(
        crate::feedback::start(app.feedback_writer(), Some(&tx), description),
        Some(crate::feedback::NO_WRITER.to_owned())
    );
    app.on_feedback(Err(crate::feedback::NO_WRITER.to_owned()));
    assert_eq!(app.draft(), "/feedback the sound");
    assert_eq!(app.notice(), Some(crate::feedback::NO_WRITER));
}

#[test]
fn feedback_sends_the_description_and_lands_path_plus_link() {
    let mut app = attached();
    app.set_feedback(Some(feedback_writer(feedback_done())));
    let effect = enter(&mut app, "/feedback the sound");
    assert_eq!(
        effect,
        Effect::Feedback {
            description: "the sound".to_owned()
        }
    );
    // The draft stays until the worker's answer lands.
    assert_eq!(app.draft(), "/feedback the sound");
    app.on_feedback(Ok(feedback_done()));
    assert!(app.draft().is_empty());
    assert_eq!(
        app.notice(),
        Some("Wrote /w/fiber-feedback-7.md\nhttps://example.test/new?title=x")
    );
}

#[test]
fn feedback_failure_keeps_the_draft_with_the_error() {
    let mut app = attached();
    app.set_feedback(Some(feedback_writer(feedback_done())));
    assert!(matches!(
        enter(&mut app, "/feedback the sound"),
        Effect::Feedback { .. }
    ));
    app.on_feedback(Err("the write failed".to_owned()));
    assert_eq!(app.draft(), "/feedback the sound");
    assert_eq!(app.notice(), Some("the write failed"));
}

#[test]
fn feedback_renders_the_prompt_then_the_written_report() {
    use std::sync::mpsc;
    let mut app = attached();
    app.set_size(80, 24);
    app.set_feedback(Some(feedback_writer(feedback_done())));
    assert_eq!(enter(&mut app, "/feedback"), Effect::None);
    let prompt = screen_text(&app);
    assert!(
        prompt.contains("Describe the feedback after /feedback"),
        "{prompt}"
    );
    assert!(prompt.contains("/feedback"), "{prompt}");
    type_text(&mut app, "the sound");
    let Effect::Feedback { description } = app.on_key(Key::Enter, now()) else {
        panic!("feedback effect");
    };
    let (tx, rx) = mpsc::channel();
    assert_eq!(
        crate::feedback::start(app.feedback_writer(), Some(&tx), description),
        None
    );
    let crate::Input::Feedback(result) = fakes::Deadline::after(std::time::Duration::from_secs(10))
        .recv_or_fail(&rx, "feedback answer")
    else {
        panic!("feedback answer");
    };
    app.on_feedback(result);
    let written = screen_text(&app);
    assert!(written.contains("/w/fiber-feedback-7.md"), "{written}");
    assert!(
        written.contains("https://example.test/new?title=x"),
        "{written}"
    );
}

fn credential_app() -> (App, std::sync::Arc<crate::configure_fake::Fake>) {
    let seam = std::sync::Arc::new(crate::configure_fake::Fake::new(vec![]));
    let mut app = attached();
    app.set_configure(Some(seam.clone()));
    app.set_size(80, 24);
    (app, seam)
}

fn credential_answer(app: &mut App, id: &Value, current: Option<&str>) -> Vec<String> {
    let mut result = json!({"provider":"fake", "credentials":["default","home","work"]});
    if let Some(current) = current {
        result["credential"] = json!(current);
    }
    app.on_line(session_line(
        "command_accepted",
        json!({"command_id":id, "result":result}),
    ))
}

fn credential_open(app: &mut App) {
    let queries = sent(enter(app, "/credential"));
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0]["command"], "credentials");
    assert!(queries[0].get("args").is_none());
    assert!(credential_answer(app, &queries[0]["id"], Some("home")).is_empty());
    assert_eq!(app.key_context(), crate::keyset::Context::Picker);
}

fn credential_accept(app: &mut App, id: &Value, provider: Option<&str>) {
    let mut payload = json!({"command_id":id});
    if let Some(provider) = provider {
        payload["result"] = json!({"provider":provider});
    }
    assert!(
        app.on_line(session_line("command_accepted", payload))
            .is_empty()
    );
}

fn credential_usage(app: &mut App) {
    app.on_line(session_line("usage_recorded", json!({
        "generation_id":"g_1", "model":"fake/m", "tokens":{"input":80000,"cache_read":3000,"cache_write":{"5m":1000},"output":5},
        "input_bytes":1,"cost":null
    })));
}

fn credential_notice(app: &App, code: &str, text: &str) -> bool {
    app.notices().iter().any(|notice| matches!(notice, crate::app::NoticeLine::Notice { code: got, text: message, .. } if got == code && message == text))
}

#[test]
fn credential_picker_switches_saves_on_acceptance_and_names_the_admitted_provider() {
    for provider in ["fake", "other"] {
        let (mut app, seam) = credential_app();
        credential_usage(&mut app);
        credential_open(&mut app);
        app.on_key(Key::Down, now());
        let lines = sent(app.on_key(Key::Enter, now()));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["command"], "credential");
        assert_eq!(lines[0]["args"], json!({"label":"work"}));
        assert!(credential_notice(
            &app,
            "cache_rebuild",
            &format!(
                "switching rebuilds the cache: about {} tokens",
                crate::swapped::about(84_000)
            )
        ));
        assert!(!app.config_view_open());
        assert!(seam.writes().is_empty());
        credential_accept(&mut app, &lines[0]["id"], Some(provider));
        assert_eq!(
            seam.writes(),
            vec![(
                PathBuf::from("/w"),
                crate::configure::Layer::Global,
                format!("providers.\"{provider}\".credential"),
                "work".to_owned()
            )]
        );
    }
}

#[test]
fn credential_current_label_or_missing_usage_skips_rebuild_but_still_saves() {
    for (usage, down) in [(true, false), (false, true)] {
        let (mut app, seam) = credential_app();
        if usage {
            credential_usage(&mut app);
        }
        credential_open(&mut app);
        if down {
            app.on_key(Key::Down, now());
        }
        let lines = sent(app.on_key(Key::Enter, now()));
        assert_eq!(lines.len(), 1);
        assert!(!app.notices().iter().any(
            |n| matches!(n, crate::app::NoticeLine::Notice { code, .. } if code == "cache_rebuild")
        ));
        credential_accept(&mut app, &lines[0]["id"], Some("fake"));
        assert_eq!(seam.writes()[0].3, if down { "work" } else { "home" });
    }
}

#[test]
fn credential_ctrl_s_sends_without_saving() {
    let (mut app, seam) = credential_app();
    credential_open(&mut app);
    app.on_key(Key::Down, now());
    let lines = sent(app.on_press(crate::stroke::Stroke::parse("ctrl+s").unwrap(), now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["args"], json!({"label":"work"}));
    credential_accept(&mut app, &lines[0]["id"], Some("fake"));
    assert!(seam.writes().is_empty());
}

#[test]
fn credential_refusals_clear_the_waiting_save() {
    for code in ["credential_missing", "duplicate_command"] {
        let (mut app, seam) = credential_app();
        credential_open(&mut app);
        let lines = sent(app.on_key(Key::Enter, now()));
        app.on_line(session_line(
            "command_rejected",
            json!({"command_id":lines[0]["id"],"code":code,"message":"refused"}),
        ));
        if code == "credential_missing" {
            assert!(credential_notice(&app, code, "refused"));
        }
        credential_accept(&mut app, &lines[0]["id"], Some("fake"));
        assert!(seam.writes().is_empty());
    }
}

#[test]
fn credential_latest_save_wins_and_a_late_acceptance_after_home_saves_once() {
    let (mut app, seam) = credential_app();
    credential_open(&mut app);
    let first = sent(app.on_key(Key::Enter, now()));
    credential_open(&mut app);
    app.on_key(Key::Down, now());
    let second = sent(app.on_key(Key::Enter, now()));
    app.go_home();
    credential_accept(&mut app, &second[0]["id"], Some("other"));
    credential_accept(&mut app, &first[0]["id"], Some("fake"));
    credential_accept(&mut app, &second[0]["id"], Some("other"));
    assert_eq!(
        seam.writes(),
        vec![(
            PathBuf::from("/w"),
            crate::configure::Layer::Global,
            "providers.\"other\".credential".to_owned(),
            "work".to_owned()
        )]
    );
}

#[test]
fn credential_missing_seam_and_missing_provider_warn_without_saving() {
    for seam_present in [false, true] {
        let (mut app, seam) = credential_app();
        if !seam_present {
            app.set_configure(None);
        }
        credential_open(&mut app);
        let lines = sent(app.on_key(Key::Enter, now()));
        assert_eq!(lines.len(), 1);
        credential_accept(&mut app, &lines[0]["id"], None);
        assert!(credential_notice(
            &app,
            "io_failed",
            "Saving is not available; the switch is for this session only."
        ));
        assert!(seam.writes().is_empty());
    }
}

#[test]
fn credential_disconnected_pick_stays_open_and_shortcut_keeps_draft() {
    let (mut app, seam) = credential_app();
    credential_open(&mut app);
    app.disconnected();
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.config_view_open());
    assert!(credential_notice(
        &app,
        "connection_failed",
        "The hub is not connected; nothing changed."
    ));
    assert!(seam.writes().is_empty());
    app.close_config_view();
    assert_eq!(enter(&mut app, "/credential work"), Effect::None);
    assert_eq!(app.draft(), "/credential work");
    let mut home = connected();
    assert_eq!(enter(&mut home, "/credential"), Effect::None);
    assert!(credential_notice(
        &home,
        "session_not_found",
        "No session on screen."
    ));
    assert!(home.draft().is_empty());
    assert_eq!(enter(&mut home, "/credential work"), Effect::None);
    assert!(home.draft().is_empty());
}

#[test]
fn credential_shortcuts_replace_queries_and_save_the_trimmed_label() {
    let (mut app, seam) = credential_app();
    credential_usage(&mut app);
    let first = sent(enter(&mut app, "/credential work"));
    let second = sent(enter(&mut app, "/credential  home  "));
    assert_eq!(first[0]["command"], "credentials");
    assert!(credential_answer(&mut app, &first[0]["id"], Some("home")).is_empty());
    let lines = credential_answer(&mut app, &second[0]["id"], Some("work"));
    assert_eq!(lines.len(), 1);
    let switch: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(switch["command"], "credential");
    assert_eq!(switch["args"], json!({"label":"home"}));
    assert!(credential_notice(
        &app,
        "cache_rebuild",
        &format!(
            "switching rebuilds the cache: about {} tokens",
            crate::swapped::about(84_000)
        )
    ));
    credential_accept(&mut app, &switch["id"], Some("fake"));
    assert_eq!(seam.writes()[0].3, "home");
}

#[test]
fn credential_bare_replaces_shortcut_and_unattached_answer_sends_nothing() {
    let (mut app, seam) = credential_app();
    let first = sent(enter(&mut app, "/credential work"));
    let second = sent(enter(&mut app, "/credential"));
    assert!(credential_answer(&mut app, &first[0]["id"], Some("home")).is_empty());
    assert_eq!(
        app.config_view_screen().unwrap().below,
        ["Reading the labels…"]
    );
    assert!(credential_answer(&mut app, &second[0]["id"], Some("home")).is_empty());
    app.close_config_view();
    let third = sent(enter(&mut app, "/credential work"));
    app.go_home();
    assert!(credential_answer(&mut app, &third[0]["id"], Some("home")).is_empty());
    assert!(seam.writes().is_empty());
}

#[test]
fn credential_rejected_query_restores_draft_and_picker_rejection_shows_message() {
    let (mut app, seam) = credential_app();
    let query = sent(enter(&mut app, "/credential work"));
    app.on_line(session_line(
        "command_rejected",
        json!({"command_id":query[0]["id"],"code":"closing","message":"ending"}),
    ));
    assert_eq!(app.draft(), "/credential work");
    assert!(credential_notice(&app, "closing", "ending"));
    app.draft.clear();
    let query = sent(enter(&mut app, "/credential"));
    app.on_line(session_line(
        "command_rejected",
        json!({"command_id":query[0]["id"],"code":"closing","message":"ended"}),
    ));
    assert_eq!(app.config_view_screen().unwrap().below, ["ended"]);
    assert!(seam.writes().is_empty());
}

#[test]
fn credential_shortcut_without_seam_and_failed_write_show_notices() {
    for seam_present in [false, true] {
        let (mut app, seam) = credential_app();
        if seam_present {
            *seam.refuse.lock().unwrap() = Some("disk full".to_owned());
        } else {
            app.set_configure(None);
        }
        let query = sent(enter(&mut app, "/credential work"));
        let lines = credential_answer(&mut app, &query[0]["id"], Some("home"));
        assert_eq!(lines.len(), 1);
        let switch: Value = serde_json::from_str(&lines[0]).unwrap();
        credential_accept(&mut app, &switch["id"], Some("fake"));
        let text = if seam_present {
            "Saving providers.\"fake\".credential failed: disk full"
        } else {
            "Saving is not available; the switch is for this session only."
        };
        assert!(credential_notice(&app, "io_failed", text));
    }
}

#[test]
fn credential_overlays_and_rebound_session_only_route_to_the_picker() {
    let (mut app, seam) = credential_app();
    credential_open(&mut app);
    app.open_keymap();
    assert_eq!(app.key_context(), crate::keyset::Context::Overlay);
    assert_eq!(app.credential_session_only(), Effect::None);
    assert_eq!(
        app.on_press(crate::stroke::Stroke::parse("ctrl+s").unwrap(), now()),
        Effect::None
    );
    assert!(app.config_view_open());
    app.close_keymap();
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    app.on_line(session_line("session_status", json!({"name":"working","workspace":"/w","project":"-w","state":"streaming","since":0,"started":0,"calls":0,"delegates":0,"jobs":0,"clients":1,"model":"fake/m","spend":{"tokens":{"input":0,"output":0,"cache_read":0,"cache_write":{}},"cost":0.0,"subscription_cost":0.0}})));
    app.quit();
    assert!(app.quit_open());
    assert_eq!(app.key_context(), crate::keyset::Context::Overlay);
    assert_eq!(app.credential_session_only(), Effect::None);
    assert!(app.config_view_open());
    app.on_key(Key::Esc, now());
    assert!(!app.quit_open());
    assert_eq!(app.key_context(), crate::keyset::Context::Picker);
    let mut user = serde_json::Map::new();
    user.insert("session_only".to_owned(), json!("ctrl+x"));
    app.set_keys(crate::KeysSetup { user });
    let lines = sent(app.on_press(crate::stroke::Stroke::parse("ctrl+x").unwrap(), now()));
    assert_eq!(lines.len(), 1);
    credential_accept(&mut app, &lines[0]["id"], Some("fake"));
    assert!(seam.writes().is_empty());
}

#[test]
fn an_extension_draft_with_an_image_is_a_prompt() {
    let mut app = attached();
    answer_rows(
        &mut app,
        json!([{"name": "deploy", "description": "Deploys.", "tag": "acme", "extension": true}]),
    );
    type_text(&mut app, "/deploy now");
    paste_image(&mut app);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
}
