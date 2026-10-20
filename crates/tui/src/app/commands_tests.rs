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
    assert_eq!(shown(&app), ["/name <text>  Names the session.  command"]);
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
    for command in ["/handoff x", "/name x", "/reload", "/close"] {
        let mut app = connected();
        assert_eq!(enter(&mut app, command), Effect::None, "{command}");
        assert_eq!(app.notice(), Some("No session on screen."), "{command}");
        assert_eq!(app.draft(), "", "{command}");
    }
}

#[test]
fn a_command_with_the_link_down_keeps_its_draft() {
    for command in ["/handoff x", "/name x", "/reload", "/close"] {
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
fn an_unknown_command_is_sent_as_written() {
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
fn undrawn_built_ins_without_an_extension_go_out_as_typed() {
    for command in ["/credential", "/rewind"] {
        let mut app = attached();
        let lines = sent(enter(&mut app, command));
        assert_eq!(lines.len(), 1, "{command}");
        assert_eq!(lines[0]["command"], "prompt", "{command}");
        assert!(app.keymap().is_none(), "{command}");
    }
}

#[test]
fn every_table_row_dispatches_and_undrawn_built_ins_fall_through() {
    for entry in crate::app::commands::dispatch_table() {
        let mut app = attached();
        type_text(&mut app, &format!("/{}", entry.name));
        assert!(app.built_in().is_some(), "/{}", entry.name);
    }
    for command in ["/credential", "/rewind"] {
        let mut app = attached();
        type_text(&mut app, command);
        assert!(app.built_in().is_none(), "{command}");
    }
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
