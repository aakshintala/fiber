//! Tests for the built-in commands: dispatch through the table and
//! the lines Enter and Esc send.

use super::commands_testkit::{
    SESSION, answer_commands, answer_rows, attached, connected, enter, hub_line, image_part, now,
    paste_image, sent, session_line, shown, start_accepted, turn_starts, turn_starts_with,
    type_text,
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
fn reload_rebuilds_renderer_and_installs_an_empty_replacement() {
    use std::sync::atomic::Ordering;
    let mut app = attached();
    let loader = crate::ext::tests::TestLoad::new("replacement");
    app.set_home(crate::home::Launch {
        extensions: Some(loader.clone()),
        ..Default::default()
    });
    enter(&mut app, "/reload");
    assert_eq!(loader.calls.load(Ordering::SeqCst), 1); // One rebuild per /reload.
    assert!(matches!(
        app.handle.get().unwrap().fills_for(
            &crate::ext::SessionCtx {
                session_id: contract::SessionId(SESSION.into()),
                halves: None
            },
            "ledger_row"
        ),
        crate::ext::SlotSelection::Draw { .. }
    ));
    assert!(app.notices().iter().any(|line|matches!(line,crate::app::NoticeLine::Notice{code,text,..} if code=="extension_failed" && text.contains("load fixture notice"))));
    loader.enabled.store(false, Ordering::SeqCst);
    enter(&mut app, "/reload");
    assert_eq!(loader.calls.load(Ordering::SeqCst), 2);
    assert!(
        !app.handle.get().unwrap().fills("ledger_row"),
        "empty result replaces the old renderer"
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

/// The open import report's title and item counts.
fn report_summary(app: &App) -> (String, usize, usize) {
    let report = app
        .import_report()
        .expect("an accepted import opens the report");
    (
        report.title().to_owned(),
        report.copied_count(),
        report.left_count(),
    )
}

#[test]
fn an_accepted_import_opens_one_report_and_adds_no_notices() {
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
    assert_eq!(
        report_summary(&app),
        ("Imported from Codex".to_owned(), 4, 1)
    );
    assert!(app.notices().is_empty());
    // Esc closes the report and leaves no notices behind.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.import_report().is_none());
    assert!(app.notices().is_empty());
}

#[test]
fn a_bare_accepted_import_reports_the_generic_title() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import"));
    let id = lines[0]["id"].clone();
    let accepted = session_line(
        "command_accepted",
        json!({
            "command_id": id,
            "result": {
                "imported": {
                    "mcp_servers": ["alpha", "beta"],
                    "skills": ["lint"],
                    "commands": [],
                    "instructions": [],
                },
                "left_behind": [],
            },
        }),
    );
    assert!(app.on_line(accepted).is_empty());
    // A bare `/import` covers both sources, so the title names neither.
    assert_eq!(report_summary(&app), ("Imported".to_owned(), 3, 0));
    assert!(app.notices().is_empty());
}

#[test]
fn an_empty_accepted_import_still_opens_the_report() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import codex"));
    let id = lines[0]["id"].clone();
    let accepted = session_line(
        "command_accepted",
        json!({
            "command_id": id,
            "result": {
                "imported": {
                    "mcp_servers": [],
                    "skills": [],
                    "commands": [],
                    "instructions": [],
                },
                "left_behind": [],
            },
        }),
    );
    assert!(app.on_line(accepted).is_empty());
    assert_eq!(
        report_summary(&app),
        ("Imported from Codex".to_owned(), 0, 0)
    );
    assert!(app.notices().is_empty());
}

#[test]
fn a_second_accept_for_the_same_import_changes_nothing() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import codex"));
    let id = lines[0]["id"].clone();
    let accepted = || {
        session_line(
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
                    "left_behind": [],
                },
            }),
        )
    };
    assert!(app.on_line(accepted()).is_empty());
    assert_eq!(
        report_summary(&app),
        ("Imported from Codex".to_owned(), 1, 0)
    );
    // The pending entry is consumed by the first answer, so a late or
    // duplicate answer finds nothing: the report stays as it was, even
    // when the late answer carries a different report.
    let late = session_line(
        "command_accepted",
        json!({
            "command_id": id,
            "result": {
                "imported": {
                    "mcp_servers": [],
                    "skills": ["other", "another"],
                    "commands": [],
                    "instructions": [],
                },
                "left_behind": [],
            },
        }),
    );
    assert!(app.on_line(late).is_empty());
    assert_eq!(
        report_summary(&app),
        ("Imported from Codex".to_owned(), 1, 0)
    );
    assert!(app.notices().is_empty());
    // An answer arriving after Esc changes nothing either.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.import_report().is_none());
    assert!(app.on_line(accepted()).is_empty());
    assert!(app.import_report().is_none());
    assert!(app.notices().is_empty());
}

/// One session envelope from a session other than [`SESSION`]: most
/// answers settle silently, never touching the screen, while an
/// accepted import opens its report.
fn other_session_line(kind: &str, payload: Value) -> Line {
    let mut line = session_line(kind, payload);
    if let Line::Session(envelope) = &mut line {
        envelope.session_id = contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned());
    }
    line
}

/// Results carrying no import report: a missing result, a null
/// A valid import `result`: one copied server, nothing left behind.
fn import_result() -> Value {
    json!({
        "imported": {
            "mcp_servers": ["alpha"],
            "skills": [],
            "commands": [],
            "instructions": [],
        },
        "left_behind": [],
    })
}

/// Results carrying no import report: a missing result, a null
/// one, a valid other command's result, and a shape no command
/// answers with (which fails the typed accept parse).
fn malformed_results() -> [Value; 4] {
    [
        json!({}),
        json!({"result": null}),
        json!({"result": {"tools": []}}),
        json!({"result": {"unrelated": true}}),
    ]
}

#[test]
fn an_accept_for_another_session_opens_the_report() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import codex"));
    let id = lines[0]["id"].clone();
    let report = || {
        json!({
            "command_id": id,
            "result": import_result(),
        })
    };
    // An accept arriving on another session opens the report there:
    // the answer settles the pending entry whatever surface shows.
    assert!(
        app.on_line(other_session_line("command_accepted", report()))
            .is_empty()
    );
    assert_eq!(
        report_summary(&app),
        ("Imported from Codex".to_owned(), 1, 0)
    );
    assert!(app.notices().is_empty());
    // Esc closes the report; a duplicate accept back in the session
    // finds no pending entry, so it opens nothing and adds no notice.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.import_report().is_none());
    assert!(
        app.on_line(session_line("command_accepted", report()))
            .is_empty()
    );
    assert!(app.import_report().is_none());
    assert!(app.notices().is_empty());
}

#[test]
fn a_malformed_accepted_import_adds_one_notice_and_opens_no_report() {
    // Each stub carries no import report (see `malformed_results`).
    let stubs = malformed_results();
    for stub in &stubs {
        let mut app = attached();
        let lines = sent(enter(&mut app, "/import codex"));
        let id = lines[0]["id"].clone();
        let mut payload = stub.clone();
        payload["command_id"] = id.clone();
        if payload["result"] == json!({"unrelated": true}) {
            // The foreign shape fails the typed accept parse, so the
            // settle reads the raw payload.
            assert!(
                serde_json::from_value::<contract::events::CommandAccepted>(payload.clone())
                    .is_err()
            );
        }
        assert!(
            app.on_line(session_line("command_accepted", payload))
                .is_empty()
        );
        // One notice naming the refusal, no report, and the pending
        // entry closes: a later valid accept for the id changes
        // nothing and adds nothing.
        assert_eq!(app.notices().len(), 1);
        assert_eq!(app.notice(), Some("rejected"));
        assert!(app.import_report().is_none());
        assert!(app.outbox.pending.is_empty());
        let late = session_line(
            "command_accepted",
            json!({"command_id": id, "result": import_result()}),
        );
        assert!(app.on_line(late).is_empty());
        assert!(app.import_report().is_none());
        assert_eq!(app.notices().len(), 1);
    }
}

#[test]
fn a_malformed_accept_for_another_session_adds_one_notice() {
    let stubs = malformed_results();
    for stub in &stubs {
        let mut app = attached();
        let lines = sent(enter(&mut app, "/import codex"));
        let id = lines[0]["id"].clone();
        let mut payload = stub.clone();
        payload["command_id"] = id.clone();
        assert!(
            app.on_line(other_session_line("command_accepted", payload))
                .is_empty()
        );
        assert_eq!(app.notices().len(), 1);
        assert_eq!(app.notice(), Some("rejected"));
        assert!(app.import_report().is_none());
        assert!(app.outbox.pending.is_empty());
    }
}

#[test]
fn an_accept_after_going_home_opens_the_report() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import codex"));
    let id = lines[0]["id"].clone();
    app.go_home();
    let accepted = session_line(
        "command_accepted",
        json!({"command_id": id, "result": import_result()}),
    );
    assert!(app.on_line(accepted).is_empty());
    assert_eq!(
        report_summary(&app),
        ("Imported from Codex".to_owned(), 1, 0)
    );
    assert!(app.notices().is_empty());
    assert!(app.outbox.pending.is_empty());
}

#[test]
fn opening_the_report_closes_the_slash_panel() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import codex"));
    let id = lines[0]["id"].clone();
    type_text(&mut app, "/");
    assert!(app.completions().is_some());
    let accepted = session_line(
        "command_accepted",
        json!({"command_id": id, "result": import_result()}),
    );
    assert!(app.on_line(accepted).is_empty());
    assert_eq!(
        report_summary(&app),
        ("Imported from Codex".to_owned(), 1, 0)
    );
    assert!(app.completions().is_none());
    assert!(!app.files_open());
    // Esc closes the report and leaves no panel behind; the draft
    // is untouched.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.import_report().is_none());
    assert!(app.completions().is_none());
    assert!(!app.files_open());
    assert_eq!(app.draft(), "/");
}

#[test]
fn opening_the_report_closes_the_at_panel() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import codex"));
    let id = lines[0]["id"].clone();
    assert_eq!(app.on_key(Key::Char('@'), now()), Effect::ListFiles);
    app.on_files(app.generation(), Ok(vec!["a.rs".to_owned()]));
    assert!(app.completions().is_some());
    assert!(app.files_open());
    let accepted = session_line(
        "command_accepted",
        json!({"command_id": id, "result": import_result()}),
    );
    assert!(app.on_line(accepted).is_empty());
    assert_eq!(
        report_summary(&app),
        ("Imported from Codex".to_owned(), 1, 0)
    );
    assert!(app.completions().is_none());
    assert!(!app.files_open());
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.import_report().is_none());
    assert!(app.completions().is_none());
    assert!(!app.files_open());
    assert_eq!(app.draft(), "@");
}

#[test]
fn opening_the_report_closes_the_theme_panel() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import codex"));
    let id = lines[0]["id"].clone();
    type_text(&mut app, "/theme ");
    assert!(app.completions().is_some());
    let accepted = session_line(
        "command_accepted",
        json!({"command_id": id, "result": import_result()}),
    );
    assert!(app.on_line(accepted).is_empty());
    assert_eq!(
        report_summary(&app),
        ("Imported from Codex".to_owned(), 1, 0)
    );
    assert!(app.completions().is_none());
    assert!(!app.files_open());
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.import_report().is_none());
    assert!(app.completions().is_none());
    assert!(!app.files_open());
    assert_eq!(app.draft(), "/theme ");
}

#[test]
fn a_rejected_import_adds_one_notice_and_opens_no_report() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import codex"));
    let id = lines[0]["id"].clone();
    let rejected = session_line(
        "command_rejected",
        json!({"command_id": id, "code": "invalid_arguments", "message": "No."}),
    );
    assert!(app.on_line(rejected).is_empty());
    // A failed import is one notice, as any failed command is.
    assert!(app.import_report().is_none());
    assert_eq!(app.notice(), Some("No."));
    // The rejection drops the pending entry, so a late accept for the
    // id changes nothing.
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
                "left_behind": [],
            },
        }),
    );
    assert!(app.on_line(accepted).is_empty());
    assert!(app.import_report().is_none());
}

#[test]
fn a_reject_for_another_session_drops_the_import_source() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import codex"));
    let id = lines[0]["id"].clone();
    // A reject arriving after leaving the session opens no report.
    // Its pending entry goes through the unchanged refused path, so
    // a later accept back in the session opens no stale report.
    let rejected = other_session_line(
        "command_rejected",
        json!({"command_id": id, "code": "invalid_arguments", "message": "No."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert!(app.import_report().is_none());
    assert_eq!(app.notice(), Some("No."));
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
                "left_behind": [],
            },
        }),
    );
    assert!(app.on_line(accepted).is_empty());
    assert!(app.import_report().is_none());
    // The late accept adds nothing to the refusal's one notice.
    assert_eq!(app.notices().len(), 1);
}

#[test]
fn report_keys_keep_the_held_conversation_position() {
    let mut app = attached();
    app.set_size(80, 24);
    // One durable row for the held anchor to name.
    let held = Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(contract::Seq(0)),
        payload: json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "hi"}]}]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    });
    // The replayed start asks `commands` again; answering with no rows
    // leaves the held row to the anchor below.
    let asked = app.on_line(held);
    // docs/tui.md, "Slash commands": one `commands` request per `turn_started` (docs/testing.md, "Event streams").
    assert_eq!(asked.len(), 1);
    let asked: Value = serde_json::from_str(&asked[0]).unwrap_or_default();
    assert_eq!(asked["command"], "commands");
    let answer = session_line(
        "command_accepted",
        json!({"command_id": asked["id"], "result": {"commands": []}}),
    );
    assert!(app.on_line(answer).is_empty());
    app.screen.pages_mut().hold_anchor(
        &contract::SessionId(SESSION.to_owned()),
        contract::Seq(0),
        0,
    );
    // The open report's keys scroll it without discarding the held
    // position: the anchor reset runs only past the report's keys.
    let lines = sent(enter(&mut app, "/import codex"));
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
                "left_behind": [],
            },
        }),
    );
    assert!(app.on_line(accepted).is_empty());
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert!(
        app.screen.pages_mut().anchored_row().is_some(),
        "report keys keep the held position"
    );
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
    let rows =
        json!([{"name": "deploy", "description": "Deploys.", "tag": "acme", "extension": true}]);
    answer_rows(&mut app, rows.clone());
    // The turn's answer keeps the rows: `/deploy` still dispatches.
    turn_starts_with(&mut app, rows);
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
    let mut app = attached();
    let lines = sent(enter(&mut app, "/rewind"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert!(app.keymap().is_none());
}

#[test]
fn every_table_row_dispatches_and_undrawn_built_ins_fall_through() {
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
    std::sync::Arc::new(move |_: &str, _: Option<&contract::SessionId>| Ok(done.clone()))
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
        crate::feedback::start(app.feedback_writer(), Some(&tx), description, None),
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
        crate::feedback::start(app.feedback_writer(), Some(&tx), description, None),
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
    assert_eq!(app.on_action("session_only"), Effect::None);
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
    assert_eq!(app.on_action("session_only"), Effect::None);
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

#[test]
fn a_rejected_named_import_returns_its_draft_with_the_notice() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import codex"));
    let id = lines[0]["id"].clone();
    let rejected = session_line(
        "command_rejected",
        json!({"command_id": id, "code": "invalid_arguments", "message": "No."}),
    );
    assert!(app.on_line(rejected).is_empty());
    // A failed import is one notice, as any failed command is, and the
    // draft comes back for a retry.
    assert!(app.import_report().is_none());
    assert_eq!(app.draft(), "/import codex");
    assert_eq!(app.notice(), Some("No."));
}

#[test]
fn an_accepted_unknown_source_import_settles_without_a_report() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/import vim"));
    let id = lines[0]["id"].clone();
    assert_eq!(lines[0]["args"], json!({"source": "vim"}));
    // An unknown source stays a plain command: its accept closes the
    // entry with no report and no notice.
    let accepted = session_line(
        "command_accepted",
        json!({"command_id": id, "result": import_result()}),
    );
    assert!(app.on_line(accepted).is_empty());
    assert!(app.import_report().is_none());
    assert!(app.notices().is_empty());
    assert!(app.outbox.pending.is_empty());
}

/// A waiting `session_status` for `session`, held on `kind` with request
/// `r_1`.
fn waiting_status(session: &str, kind: &str) -> Line {
    old_line(
        session,
        "session_status",
        json!({
            "name": "fix the parser", "workspace": "/w", "project": "-w",
            "state": "waiting",
            "waiting": {"request_id": "r_1", "kind": kind, "summary": "Run it"},
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0, "cache_write": {},
                "output": 2}, "cost": 0.0, "subscription_cost": 0.0},
            "model": "fake/m", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    )
}

/// A `permission_requested` approval for `session`.
fn approval(session: &str) -> Line {
    let mut line = old_line(
        session,
        "permission_requested",
        json!({
            "request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "review", "rule": {"subject": "npm test", "prefix": "npm test"},
        }),
    );
    if let Line::Session(envelope) = &mut line {
        envelope.action_id = Some(contract::ActionId("a_1".to_owned()));
    }
    line
}

/// A `session_status` for `session` with `workspace`, `model` and
/// optional `thinking`.
fn clear_status(session: &str, workspace: &str, model: &str, thinking: Option<&str>) -> Line {
    let mut payload = json!({
        "name": "fix the parser", "workspace": workspace, "project": "-w",
        "state": "idle", "started": 0, "calls": 0, "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0, "cache_write": {},
            "output": 2}, "cost": 0.0, "subscription_cost": 0.0},
        "model": model, "delegates": 0, "jobs": 0, "clients": 0,
    });
    if let Some(thinking) = thinking {
        payload["thinking"] = json!(thinking);
    }
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A `preamble_built` for `session` naming `credential` or its absence.
fn clear_preamble(session: &str, model: &str, credential: Option<&str>) -> Line {
    let mut line = session_line(
        "preamble_built",
        json!({
            "reason": "start", "model": model, "context_window": 100,
            "thinking": "high", "tool_choice": "auto", "cache_lifetime": "5m",
            "system_prompt": "", "tools": [], "credential": credential,
        }),
    );
    if let Line::Session(envelope) = &mut line {
        envelope.session_id = contract::SessionId(session.to_owned());
    }
    line
}

/// A credential-changing `model_changed` for `session`.
fn clear_model_changed(session: &str, credential: Option<&str>) -> Line {
    old_line(
        session,
        "model_changed",
        json!({
            "before": {"model": "fake/mod.el", "cache_lifetime": "5m", "credential": "L1"},
            "after": {"model": "fake/mod.el", "cache_lifetime": "5m", "credential": credential},
            "source": "driver",
        }),
    )
}

/// A `kind` line from `session`, parsed from the hub's side.
fn old_line(session: &str, kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// An app with home attached to [`SESSION`], so rows fold.
fn homed() -> App {
    let mut app = attached();
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    app
}

/// Accepts hub command `id` with a new `session_id`, returning the lines
/// the acceptance sends, parsed.
fn accept_start(app: &mut App, id: &Value, session: &str) -> Vec<Value> {
    app.on_line(hub_line(
        "command_accepted",
        json!({"command_id": id, "result": {"session_id": session}}),
    ))
    .iter()
    .map(|line| serde_json::from_str(line).unwrap_or_default())
    .collect()
}

/// Folds one assistant reply of `text` into the attached session, so
/// the screen holds a conversation.
fn reply(app: &mut App, text: &str) {
    for (kind, payload) in [
        ("assistant_message_started", json!({})),
        ("text_completed", json!({"text": text})),
        (
            "assistant_message_completed",
            json!({"outcome": "completed"}),
        ),
    ] {
        let mut line = session_line(kind, payload);
        if let Line::Session(envelope) = &mut line {
            envelope.action_id = Some(contract::ActionId("a_1".to_owned()));
        }
        app.on_line(line);
    }
}

const NEW: &str = "s_nnnnnnnnnnnnnnnn";

/// The `/clear` start carries the old session's wire state
/// (`docs/tui.md:1932–1939`, "Clearing").
#[test]
fn clear_sends_a_start_with_the_old_session_s_wire() {
    let mut app = attached();
    app.on_line(clear_status(
        SESSION,
        "/w/work space",
        "fake/mod.el",
        Some("high"),
    ));
    app.on_line(clear_preamble(SESSION, "fake/mod.el", Some("L1")));
    let lines = sent(enter(&mut app, "/clear"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    assert!(lines[0].get("session_id").is_none());
    assert_eq!(lines[0]["args"]["workspace"], "/w/work space");
    assert_eq!(lines[0]["args"]["model"], "fake/mod.el");
    assert_eq!(lines[0]["args"]["worktree"], false);
    assert!(
        lines[0]["args"]["overrides"]
            .as_array()
            .is_some_and(|overrides| {
                overrides.contains(&json!(r#"models."fake/mod.el".thinking=high"#))
                    && overrides.contains(&json!(r#"providers."fake".credential=L1"#))
            })
    );
    assert!(lines[0]["args"].get("continues").is_none());
    assert_eq!(app.draft(), "");
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

/// A build or switch that names no credential overrides an earlier start
/// label (`docs/tui.md:1941–1945`, "Clearing").
#[test]
fn clear_does_not_restore_a_start_credential_after_explicit_absence() {
    for event in [
        clear_preamble(SESSION, "fake/mod.el", None),
        clear_model_changed(SESSION, None),
    ] {
        let mut app = attached();
        app.on_line(clear_status(SESSION, "/w", "fake/mod.el", None));
        app.panel_state
            .note_start(&json!({"overrides": [r#"providers."fake".credential=L1"#]}));
        assert_eq!(app.panel_state.clear_credential(), Some(Some("L1")));
        app.on_line(event);
        assert_eq!(app.panel_state.clear_credential(), Some(None));

        let lines = sent(enter(&mut app, "/clear"));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["command"], "start");
        assert!(lines[0]["args"].get("overrides").is_none());
    }
}

/// One clear start omits unset values as documented
/// (`docs/tui.md:1932–1939`, `docs/events.md:453–454`).
#[test]
fn clear_omits_thinking_credential_and_continues_when_unset() {
    let mut app = attached();
    app.on_line(clear_status(SESSION, "/w", "fake/m", None));
    let lines = sent(enter(&mut app, "/clear"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    assert_eq!(lines[0]["args"]["workspace"], "/w");
    assert_eq!(lines[0]["args"]["model"], "fake/m");
    assert_eq!(lines[0]["args"]["worktree"], false);
    assert!(lines[0]["args"].get("overrides").is_none());
    assert!(lines[0]["args"].get("continues").is_none());
}

/// A clear of a worktree session continues it (`docs/tui.md:1932–1939`).
#[test]
fn clear_of_a_worktree_session_continues_it() {
    let mut app = attached();
    app.on_line(clear_status(SESSION, "/w/tree", "fake/m", None));
    let mut line = session_line(
        "session_started",
        json!({
            "workspace": "/w/tree",
            "variables": {"path": "", "names": [], "source": "inherited"},
            "worktree": {"path": "/w/tree", "branch": "b"},
        }),
    );
    if let Line::Session(envelope) = &mut line {
        envelope.session_id = contract::SessionId(SESSION.to_owned());
    }
    app.on_line(line);
    let lines = sent(enter(&mut app, "/clear"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    assert_eq!(lines[0]["args"]["continues"], SESSION);
    assert_eq!(lines[0]["args"]["worktree"], false);

    accept_start(&mut app, &lines[0]["id"], NEW);
    app.on_line(clear_status(NEW, "/w/tree", "fake/m", None));
    let mut continued = session_line(
        "session_started",
        json!({
            "workspace": "/w/tree",
            "variables": {"path": "", "names": [], "source": "inherited"},
            "worktree": {"path": "/w/tree", "branch": "b"},
        }),
    );
    if let Line::Session(envelope) = &mut continued {
        envelope.session_id = contract::SessionId(NEW.to_owned());
    }
    app.on_line(continued);
    let next = sent(enter(&mut app, "/clear"));
    assert_eq!(next[0]["args"]["continues"], NEW);
}

/// Clearing waits for the `start` answer before moving the screen and
/// closing the old session (`docs/tui.md:1934–1938, 1947–1952`).
#[test]
fn clear_switches_only_on_the_start_answer_then_closes_old() {
    let mut app = attached();
    turn_starts(&mut app);
    reply(&mut app, "hello");
    assert!(!app.lines().is_empty());
    app.on_line(clear_status(SESSION, "/w", "fake/m", Some("high")));
    let lines = sent(enter(&mut app, "/clear"));
    let id = lines[0]["id"].clone();
    // The start goes out alone: no close before the answer, and the
    // screen stays on the old session.
    assert_eq!(lines.len(), 1);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    let after: Vec<Value> = accept_start(&mut app, &id, NEW);
    assert!(after.iter().any(|line| {
        line["command"] == "subscribe"
            && line["session_id"] == NEW
            && line["args"] == json!({"level": "full"})
    }));
    assert!(
        after
            .iter()
            .any(|line| { line["command"] == "commands" && line["session_id"] == NEW })
    );
    assert!(after.iter().any(|line| {
        line["command"] == "close"
            && line["session_id"] == SESSION
            && line["args"] == json!({"now": false})
    }));
    assert!(!after.iter().any(|line| line["command"] == "prompt"));
    assert_eq!(app.session().map(|session| session.0.as_str()), Some(NEW));
    assert_eq!(app.draft(), "");
    assert!(app.lines().is_empty());
}

const NEW2: &str = "s_mmmmmmmmmmmmmmmm";

/// A second `/clear` cannot replace the still-pending `start`
/// (`docs/tui.md:1937–1938`).
#[test]
fn clear_waiting_for_start_answer_does_not_send_a_second_start() {
    let mut app = attached();
    app.on_line(clear_status(SESSION, "/w", "fake/m", None));
    let first = sent(enter(&mut app, "/clear"));
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["command"], "start");

    assert!(matches!(enter(&mut app, "/clear"), Effect::None));
    assert_eq!(app.draft(), "");
    let answered = accept_start(&mut app, &first[0]["id"], NEW);
    assert!(
        answered
            .iter()
            .any(|line| { line["command"] == "close" && line["session_id"] == SESSION })
    );
    assert!(!answered.iter().any(|line| line["command"] == "prompt"));
}

/// Two `/clear`s before any prompt: each `start` uses its session's
/// `session_status` model, thinking only when present, and keeps the
/// credential the first `start` named (`docs/tui.md:1932–1943`).
#[test]
fn clear_twice_before_any_prompt_keeps_the_start_credential() {
    let mut app = connected();
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        model: Some("test/model".to_owned()),
        ..Default::default()
    });
    app.set_launch_overrides(vec![r#"providers."fake".credential=L1"#.to_owned()]);
    let first = sent(enter(&mut app, "hi"));
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["command"], "start");
    assert!(
        first[0]["args"]["overrides"]
            .as_array()
            .is_some_and(|overrides| {
                overrides.contains(&json!(r#"providers."fake".credential=L1"#))
            })
    );
    let started: Vec<Value> = accept_start(&mut app, &first[0]["id"], NEW);
    // The home `start` answers with its first prompt, as today, beside
    // the feed and recent home refills.
    assert!(started.iter().any(|line| line["command"] == "prompt"));
    assert!(started.iter().any(|line| line["command"] == "subscribe"));
    // The new session's own status names another model with no
    // thinking level.
    app.on_line(clear_status(NEW, "/w", "fake/m2", None));
    let second = sent(enter(&mut app, "/clear"));
    assert_eq!(second.len(), 1);
    assert_eq!(second[0]["command"], "start");
    assert_eq!(second[0]["args"]["model"], "fake/m2");
    assert!(
        second[0]["args"]["overrides"]
            .as_array()
            .is_some_and(|overrides| {
                overrides.contains(&json!(r#"providers."fake".credential=L1"#))
            })
    );
    let answered: Vec<Value> = accept_start(&mut app, &second[0]["id"], NEW2);
    assert_eq!(
        answered
            .iter()
            .filter(|line| line["command"] == "prompt")
            .count(),
        0
    );
    // The third `start` uses the newest status again, thinking present.
    app.on_line(clear_status(NEW2, "/w", "fake/m3", Some("low")));
    let third = sent(enter(&mut app, "/clear"));
    assert_eq!(third.len(), 1);
    assert_eq!(third[0]["command"], "start");
    assert_eq!(third[0]["args"]["model"], "fake/m3");
    assert!(
        third[0]["args"]["overrides"]
            .as_array()
            .is_some_and(|overrides| {
                overrides.contains(&json!(r#"models.fake/m3.thinking=low"#))
                    && overrides.contains(&json!(r#"providers."fake".credential=L1"#))
            })
    );
}

/// A refused clear start leaves the old session in place and sends no
/// close (`docs/tui.md:1943–1945`).
#[test]
fn clear_start_refused_keeps_the_old_session_with_one_notice() {
    let mut app = homed();
    app.on_line(clear_status(SESSION, "/w", "fake/m", Some("high")));
    let lines = sent(enter(&mut app, "/clear"));
    let id = lines[0]["id"].clone();
    // Typing after the dispatch stays: the refusal takes nothing.
    type_text(&mut app, "more");
    let rejected = hub_line(
        "command_rejected",
        json!({"command_id": id, "code": "busy", "message": "Busy."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    assert_eq!(app.draft(), "more");
    assert_eq!(app.notice(), Some("Busy."));
    // The intent is gone: clearing again dispatches a fresh `start`.
    app.draft.clear();
    let again = sent(enter(&mut app, "/clear"));
    assert_eq!(again.len(), 1);
    assert_eq!(again[0]["command"], "start");
    assert_ne!(again[0]["id"], lines[0]["id"]);
}

#[test]
fn clear_start_lost_before_write_keeps_the_old_session() {
    let mut app = homed();
    app.on_line(clear_status(SESSION, "/w", "fake/m", None));
    let Effect::Send(out) = enter(&mut app, "/clear") else {
        panic!("the start goes out");
    };
    app.write_failed(&out);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    assert!(app.notice().is_some());
}

#[test]
fn clear_close_refused_stays_on_the_new_session_with_one_notice() {
    let mut app = homed();
    app.on_line(clear_status(SESSION, "/w", "fake/m", Some("high")));
    let lines = sent(enter(&mut app, "/clear"));
    let after = accept_start(&mut app, &lines[0]["id"], NEW);
    let close = after
        .iter()
        .find(|line| line["command"] == "close")
        .unwrap_or_else(|| panic!("the close goes out"));
    let rejected = old_line(
        SESSION,
        "command_rejected",
        json!({"command_id": close["id"], "code": "busy", "message": "Busy."}),
    );
    assert!(app.on_line(rejected).is_empty());
    // The screen stays on the new session; the old one stays a live
    // row, never marked closing.
    assert_eq!(app.session().map(|session| session.0.as_str()), Some(NEW));
    assert_eq!(app.notice(), Some("Busy."));
    let home = app.home.as_ref().unwrap_or_else(|| panic!("home"));
    let old = home
        .sessions
        .row(&contract::SessionId(SESSION.to_owned()))
        .unwrap_or_else(|| panic!("old row"));
    assert!(old.left.is_none());
    assert_eq!(
        old.status.as_ref().map(|status| status.closing),
        Some(false)
    );
}

/// Only the matching accepted `close` answer settles the clear's close
/// intent (`docs/tui.md`, "Clearing").
#[test]
fn clear_close_answer_settles_only_its_matching_command_id() {
    let mut app = homed();
    app.on_line(clear_status(SESSION, "/w", "fake/m", None));
    let start = sent(enter(&mut app, "/clear"));
    let after = accept_start(&mut app, &start[0]["id"], NEW);
    let close = after
        .iter()
        .find(|line| line["command"] == "close")
        .unwrap_or_else(|| panic!("the close goes out"));
    let close_id = close["id"].as_str().unwrap_or_else(|| panic!("close id"));

    app.on_line(hub_line(
        "command_accepted",
        json!({"command_id": "c_unrelated", "result": {}}),
    ));
    assert_eq!(app.clear_close.as_deref(), Some(close_id));
    app.on_line(hub_line(
        "command_accepted",
        json!({"command_id": close_id, "result": {}}),
    ));
    assert_eq!(app.clear_close, None);
}

#[test]
fn clear_close_lost_before_write_names_the_lost_link() {
    let mut app = homed();
    app.on_line(clear_status(SESSION, "/w", "fake/m", None));
    let lines = sent(enter(&mut app, "/clear"));
    let after: Vec<String> = app.on_line(hub_line(
        "command_accepted",
        json!({"command_id": lines[0]["id"], "result": {"session_id": NEW}}),
    ));
    let close = after
        .iter()
        .find(|line| line.contains("\"close\""))
        .unwrap_or_else(|| panic!("the close goes out"))
        .clone();
    app.write_failed(&[close]);
    assert_eq!(app.session().map(|session| session.0.as_str()), Some(NEW));
    assert_eq!(
        app.notice(),
        Some("The link to the old session was lost before its `close` went out.")
    );
    let home = app.home.as_ref().unwrap_or_else(|| panic!("home"));
    let old = home
        .sessions
        .row(&contract::SessionId(SESSION.to_owned()))
        .unwrap_or_else(|| panic!("old row"));
    assert!(old.left.is_none());
}

#[test]
fn clear_with_an_approval_wait_opens_the_overlay_first() {
    let mut app = homed();
    app.on_line(waiting_status(SESSION, "approval"));
    assert_eq!(enter(&mut app, "/clear"), Effect::None);
    assert!(app.clear_open());
    assert_eq!(
        app.clear_question(),
        Some(("fix the parser".to_owned(), "approval".to_owned(), false))
    );
}

#[test]
fn clear_with_a_question_wait_names_the_question() {
    let mut app = homed();
    app.on_line(waiting_status(SESSION, "question"));
    assert_eq!(enter(&mut app, "/clear"), Effect::None);
    assert!(app.clear_open());
    assert_eq!(
        app.clear_question(),
        Some(("fix the parser".to_owned(), "question".to_owned(), false))
    );
}

/// Enter clears a wait only after the clear start answers
/// (`docs/tui.md:1954–1960`, "Clearing").
#[test]
fn clear_overlay_enter_sends_the_start_then_closes_with_now() {
    let mut app = homed();
    app.on_line(waiting_status(SESSION, "approval"));
    assert_eq!(enter(&mut app, "/clear"), Effect::None);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    assert!(!app.clear_open());
    let after = accept_start(&mut app, &lines[0]["id"], NEW);
    let close = after
        .iter()
        .find(|line| line["command"] == "close")
        .unwrap_or_else(|| panic!("the close goes out"));
    assert_eq!(close["args"], json!({"now": true}));
    assert_eq!(app.session().map(|session| session.0.as_str()), Some(NEW));
}

#[test]
fn clear_overlay_esc_stays() {
    let mut app = homed();
    app.on_line(waiting_status(SESSION, "approval"));
    assert_eq!(enter(&mut app, "/clear"), Effect::None);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.clear_open());
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    // The consumed command left the draft with the overlay, so typing
    // `/clear` again dispatches fresh.
    assert_eq!(app.draft(), "");
}

/// A refused overlay start sends no close (`docs/tui.md:1943–1945`).
#[test]
fn clear_overlay_refused_start_closes_with_one_notice() {
    let mut app = homed();
    app.on_line(waiting_status(SESSION, "approval"));
    assert_eq!(enter(&mut app, "/clear"), Effect::None);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    let rejected = hub_line(
        "command_rejected",
        json!({"command_id": lines[0]["id"], "code": "busy", "message": "Busy."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert!(!app.clear_open());
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    assert_eq!(app.notice(), Some("Busy."));
}

/// The clear choice sends a start and stay sends nothing
/// (`docs/tui.md:1954–1959`, "Clearing").
#[test]
fn clear_overlay_clicks_do_what_their_keys_do() {
    use crate::home::{ClearChoice, Spot};
    let mut app = homed();
    app.on_line(waiting_status(SESSION, "approval"));
    assert_eq!(enter(&mut app, "/clear"), Effect::None);
    let lines = sent(app.home_click(Spot::Clear(ClearChoice::Clear)));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    assert!(!app.clear_open());
    // Staying sends nothing and keeps the old session.
    assert_eq!(enter(&mut app, "/clear"), Effect::None);
    assert_eq!(app.home_click(Spot::Clear(ClearChoice::Stay)), Effect::None);
    assert!(!app.clear_open());
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

/// A wait arriving after dispatch keeps the close polite
/// (`docs/tui.md:1959–1960`).
#[test]
fn clear_late_approval_keeps_the_close_polite() {
    let mut app = homed();
    app.on_line(clear_status(SESSION, "/w", "fake/m", None));
    let lines = sent(enter(&mut app, "/clear"));
    assert_eq!(lines.len(), 1);
    // The approval arrives after the dispatch, before its answer.
    app.on_line(approval(SESSION));
    let after = accept_start(&mut app, &lines[0]["id"], NEW);
    let close = after
        .iter()
        .find(|line| line["command"] == "close")
        .unwrap_or_else(|| panic!("the close goes out"));
    assert_eq!(close["args"], json!({"now": false}));
    // The old request waits in the shared queue, shown and answerable.
    assert!(app.queue.open());
}

#[test]
fn a_malformed_accept_leaves_a_non_import_entry_pending() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/reload"));
    let id = lines[0]["id"].clone();
    // An answer the terminal cannot read closes only a pending import:
    // any other kind keeps its silent drop, entry still pending.
    let accepted = session_line(
        "command_accepted",
        json!({"command_id": id, "result": {"unrelated": true}}),
    );
    assert!(app.on_line(accepted).is_empty());
    assert!(app.notices().is_empty());
    assert!(app.import_report().is_none());
    assert!(
        app.outbox
            .pending
            .contains_key(id.as_str().unwrap_or_default())
    );
}
