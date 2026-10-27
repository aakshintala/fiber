//! Tests for the slash aliases (#3013): each runs its row's command,
//! rides on that row, and gives way to an extension that takes its name.

use std::path::PathBuf;

use super::commands_testkit::{answer_rows, attached, now, sent, shown, type_text};
use crate::app::{App, Effect};
use crate::keys::Key;
use crate::slash;
use contract::events::CommandInfo;
use serde_json::json;

/// Every row that carries an alias, as `(name, alias)`, in table order.
const ALIASES: &[(&str, &str)] = &[
    ("thinking", "effort"),
    ("usage", "cost"),
    ("settings", "config"),
    ("keys", "keybindings"),
    ("name", "rename"),
    ("feedback", "bug"),
    ("quit", "exit"),
    ("delegates", "agents"),
];

/// Runs `command` through Enter on `app` with the draft set whole, so
/// dispatch reads the name as typed rather than a completion row.
fn run(app: &mut App, command: &str) -> Effect {
    app.draft.set(command);
    app.on_enter()
}

/// An extension's `commands` row named `name`, tagged `acme`.
fn extension(name: &str) -> CommandInfo {
    CommandInfo {
        name: name.to_owned(),
        description: format!("The {name} extension."),
        argument_hint: None,
        tag: "acme".to_owned(),
        extension: true,
    }
}

#[test]
fn each_alias_rides_on_its_commands_row_and_has_no_row_of_its_own() {
    let all = slash::rows(&[]);
    let carried: Vec<(&str, &str)> = all
        .iter()
        .filter_map(|row| row.alias.as_deref().map(|alias| (row.name.as_str(), alias)))
        .collect();
    assert_eq!(carried, ALIASES);
    for (_, alias) in ALIASES {
        assert!(all.iter().all(|row| row.name != *alias), "/{alias}");
    }
    let name = all.iter().find(|row| row.name == "name").unwrap();
    assert_eq!(
        name.line(),
        "/name /rename <text>  Names the session.  command"
    );
}

#[test]
fn typing_an_alias_finds_only_its_commands_row() {
    let all = slash::rows(&[]);
    for (name, alias) in ALIASES {
        let found: Vec<&str> = slash::filter(&all, alias)
            .iter()
            .map(|row| row.name.as_str())
            .collect();
        assert_eq!(found, [*name], "/{alias}");
    }
}

#[test]
fn rename_names_the_session_as_name_does() {
    for command in ["/name fix the parser", "/rename fix the parser"] {
        let mut app = attached();
        let lines = sent(run(&mut app, command));
        assert_eq!(lines.len(), 1, "{command}");
        assert_eq!(lines[0]["command"], "name", "{command}");
        assert_eq!(
            lines[0]["args"],
            json!({"text": "fix the parser"}),
            "{command}"
        );
    }
}

#[test]
fn config_opens_the_configuration_keys_as_settings_does() {
    for command in ["/settings", "/config"] {
        let mut app = attached();
        assert_eq!(run(&mut app, command), Effect::None, "{command}");
        let frame = app.config_view_screen().expect("the view is open");
        assert_eq!(frame.title, "Settings", "{command}");
        assert_eq!(frame.command, "/settings", "{command}");
    }
}

#[test]
fn agents_opens_the_all_delegates_view_as_delegates_does() {
    for command in ["/delegates", "/agents", "/delegates extra words"] {
        let mut app = attached();
        assert_eq!(run(&mut app, command), Effect::None, "{command}");
        let frame = app.session_view_screen(80).expect("the view is open");
        assert_eq!(frame.title, "Delegates", "{command}");
        assert_eq!(frame.command, "/delegates", "{command}");
        assert_eq!(app.draft(), "", "{command}");
    }
}

#[test]
fn delegates_without_a_session_shows_a_notice_and_opens_nothing() {
    for command in ["/delegates", "/agents"] {
        let mut app = App::new(PathBuf::from("/w"));
        app.set_size(80, 24);
        assert_eq!(run(&mut app, command), Effect::None, "{command}");
        assert!(!app.session_view_open(), "{command}");
        assert_eq!(
            app.notices.newest(),
            Some("No session on screen."),
            "{command}"
        );
    }
}

#[test]
fn cost_opens_the_usage_view_as_usage_does() {
    for command in ["/usage", "/cost"] {
        let mut app = attached();
        assert_eq!(run(&mut app, command), Effect::None, "{command}");
        let frame = app.session_view_screen(80).expect("the view is open");
        assert_eq!(frame.title, "Usage", "{command}");
        assert_eq!(app.draft(), "", "{command}");
    }
}

#[test]
fn keybindings_opens_the_rebinding_screen_as_keys_does() {
    for command in ["/keys", "/keybindings"] {
        let mut app = attached();
        run(&mut app, command);
        assert!(app.keys_screen_open(), "{command}");
    }
}

#[test]
fn bug_writes_a_feedback_report_as_feedback_does() {
    for command in ["/feedback the panel flickers", "/bug the panel flickers"] {
        let mut app = attached();
        assert_eq!(
            run(&mut app, command),
            Effect::Feedback {
                description: "the panel flickers".to_owned()
            },
            "{command}"
        );
    }
}

#[test]
fn effort_takes_a_thinking_level_as_thinking_does() {
    for command in ["/thinking turbo", "/effort turbo"] {
        let mut app = attached();
        assert_eq!(run(&mut app, command), Effect::None, "{command}");
        assert_eq!(
            app.notice(),
            Some(
                "Unknown thinking level \"turbo\"; the levels are off, minimal, low, medium, high, xhigh, max."
            ),
            "{command}"
        );
    }
}

#[test]
fn enter_on_a_typed_alias_runs_its_commands_row() {
    let mut app = attached();
    type_text(&mut app, "/cost");
    assert_eq!(
        shown(&app),
        ["/usage /cost  Opens the usage view.  command"]
    );
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let frame = app.session_view_screen(80).expect("the view is open");
    assert_eq!(frame.title, "Usage");
}

#[test]
fn an_extension_named_like_an_alias_takes_that_name() {
    let rows = slash::rows(&[extension("config")]);
    let settings = rows.iter().find(|row| row.name == "settings").unwrap();
    assert_eq!(settings.alias, None);
    assert_eq!(
        settings.line(),
        "/settings  Opens the configuration keys.  command"
    );
    let configs: Vec<&slash::Row> = rows
        .iter()
        .filter(|row| row.name == "config" || row.alias.as_deref() == Some("config"))
        .collect();
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].tag, "acme");
    let mut app = attached();
    answer_rows(
        &mut app,
        json!([{"name": "config", "description": "The config extension.",
            "tag": "acme", "extension": true}]),
    );
    let lines = sent(run(&mut app, "/config"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "command");
    assert_eq!(lines[0]["args"], json!({"name": "config"}));
    assert!(app.config_view_screen().is_none());
}

#[test]
fn a_skill_named_like_an_alias_is_left_out_of_the_list() {
    let skill = CommandInfo {
        extension: false,
        tag: "skill".to_owned(),
        ..extension("bug")
    };
    let rows = slash::rows(&[skill]);
    assert!(rows.iter().all(|row| row.tag != "skill"));
    let feedback = rows.iter().find(|row| row.name == "feedback").unwrap();
    assert_eq!(feedback.alias.as_deref(), Some("bug"));
}
