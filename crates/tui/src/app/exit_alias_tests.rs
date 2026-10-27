//! Tests for the `/exit` alias of `/quit` (see #2832): dispatch, the
//! list row, the filter and the drawn line.

use super::commands_testkit::{
    answer_rows, attached, connected, enter, now, paste_image, sent, shown, type_text,
};
use crate::app::{App, Effect};
use crate::keys::Key;
use crate::slash;
use serde_json::json;
use std::path::PathBuf;

/// An app on home with one streaming session, so quitting asks first.
fn working_home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    app.on_line(crate::link::Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.on_line(crate::link::Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({
            "name": "fix the parser",
            "workspace": "/w",
            "project": "-w",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model",
            "delegates": 0,
            "jobs": 0,
            "clients": 0,
            "state": "streaming",
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
    app
}

/// Runs `command` through Enter on `app` with a fresh draft.
fn run(app: &mut App, command: &str) -> Effect {
    app.draft.set(command);
    app.on_enter()
}

#[test]
fn exit_quits_idle_exactly_like_quit() {
    for command in ["/quit", "/exit"] {
        let mut app = connected();
        assert_eq!(enter(&mut app, command), Effect::Quit, "{command}");
    }
}

#[test]
fn exit_asks_while_working_exactly_like_quit() {
    for command in ["/quit", "/exit"] {
        let mut app = working_home();
        assert_eq!(run(&mut app, command), Effect::None, "{command}");
        assert!(app.quit_open(), "{command}");
        assert_eq!(app.quit_question(), Some((1, 0)), "{command}");
    }
}

#[test]
fn exit_ignores_rest_like_quit() {
    for command in ["/quit now", "/exit now"] {
        let mut app = connected();
        assert_eq!(enter(&mut app, command), Effect::Quit, "{command}");
    }
}

#[test]
fn exit_with_an_image_is_a_prompt() {
    let mut app = attached();
    type_text(&mut app, "/exit");
    // Esc closes the open `/` panel, so the paste reaches the draft.
    app.on_key(Key::Esc, now());
    paste_image(&mut app);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
}

#[test]
fn exit_matches_exact_case_only() {
    // Through dispatch, bypassing the case-folded completion list:
    // an upper-case `/EXIT` is no built-in, so it goes out as typed.
    for command in ["/EXIT", "/Exit"] {
        let mut app = attached();
        app.draft.set(command);
        assert!(app.built_in().is_none(), "{command}");
    }
}

#[test]
fn the_list_carries_one_quit_row_with_the_alias() {
    // The table shows `/exit` after `/quit` (`docs/tui.md:1956,1965`).
    let all = slash::rows(&[]);
    let quits: Vec<&slash::Row> = all.iter().filter(|row| row.name == "quit").collect();
    assert_eq!(quits.len(), 1);
    assert_eq!(quits[0].aliases, ["exit"]);
    assert_eq!(quits[0].line(), "/quit /exit  Quits.  command".to_owned());
    // No second row for the alias: `/exit` rides on the quit row.
    assert!(all.iter().all(|row| row.name != "exit"));
}

#[test]
fn filter_exit_finds_only_the_quit_row_in_its_slot() {
    let all = slash::rows(&[]);
    let at = all.iter().position(|row| row.name == "quit").unwrap();
    let found: Vec<String> = slash::filter(&all, "exit")
        .iter()
        .map(|row| row.name.clone())
        .collect();
    assert_eq!(found, ["quit"]);
    assert_eq!(at, all.iter().position(|row| row.name == "quit").unwrap());
}

#[test]
fn filter_quit_still_finds_only_the_quit_row() {
    let all = slash::rows(&[]);
    let found: Vec<String> = slash::filter(&all, "quit")
        .iter()
        .map(|row| row.name.clone())
        .collect();
    assert_eq!(found, ["quit"]);
}

/// An extension `commands` row named `name` with the `acme` tag.
fn extension_info(name: &str) -> contract::events::CommandInfo {
    contract::events::CommandInfo {
        name: name.to_owned(),
        description: format!("The {name} extension."),
        argument_hint: Some("[args]".to_owned()),
        tag: "acme".to_owned(),
        extension: true,
    }
}

#[test]
fn an_extension_exit_sheds_the_quit_alias_and_keeps_one_exit_row() {
    // The table shows `/exit` after `/quit`; an extension takes that row (`docs/tui.md:1956,1965,1974`).
    let plain = slash::rows(&[]);
    let quit = plain.iter().find(|row| row.name == "quit").unwrap();
    assert_eq!(quit.aliases, ["exit"]);
    let with = slash::rows(&[extension_info("exit")]);
    assert_eq!(with.len(), plain.len() + 1);
    let quit = with.iter().find(|row| row.name == "quit").unwrap();
    assert!(quit.aliases.is_empty());
    assert_eq!(quit.line(), "/quit  Quits.  command".to_owned());
    let exits: Vec<&slash::Row> = with
        .iter()
        .filter(|row| row.name == "exit" || row.aliases.iter().any(|alias| alias == "exit"))
        .collect();
    assert_eq!(exits.len(), 1);
    assert_eq!(
        exits[0].line(),
        "/exit [args]  The exit extension.  acme".to_owned()
    );
}

#[test]
fn an_unrelated_extension_keeps_the_quit_alias() {
    // Only an `exit` extension takes the alias; any other extension
    // leaves it on the quit row (`docs/tui.md:1956,1965,1974`).
    let with = slash::rows(&[extension_info("model")]);
    let quit = with.iter().find(|row| row.name == "quit").unwrap();
    assert_eq!(quit.aliases, ["exit"]);
    assert_eq!(quit.line(), "/quit /exit  Quits.  command".to_owned());
    let exits: Vec<&slash::Row> = with
        .iter()
        .filter(|row| row.name == "exit" || row.aliases.iter().any(|alias| alias == "exit"))
        .collect();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].name, "quit");
}

#[test]
fn an_extension_exit_wins_dispatch_and_filter() {
    let mut app = attached();
    let rows = serde_json::json!([{"name": "exit", "description": "The exit extension.",
        "argument_hint": "[args]", "tag": "acme", "extension": true}]);
    answer_rows(&mut app, rows);
    type_text(&mut app, "/exit");
    let shown = shown(&app);
    assert_eq!(
        shown.iter().find(|line| line.contains("/exit")).cloned(),
        Some("/exit [args]  The exit extension.  acme".to_owned()),
        "{shown:?}"
    );
    assert!(
        shown
            .iter()
            .all(|line| *line != "/quit /exit  Quits.  command"),
        "{shown:?}"
    );
    let found: Vec<String> = slash::filter(&slash::rows(&[extension_info("exit")]), "exit")
        .iter()
        .map(|row| row.name.clone())
        .collect();
    assert_eq!(found, ["exit"]);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "command");
    assert_eq!(lines[0]["args"], json!({"name": "exit"}));
    type_text(&mut app, "/exit now");
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "command");
    assert_eq!(lines[0]["args"], json!({"name": "exit", "text": "now"}));
}
